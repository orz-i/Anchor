use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex, RwLock};
use std::time::Duration;

use chrono::Utc;
use futures_util::StreamExt;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, ACCEPT, CONTENT_TYPE};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::{oneshot, Mutex, Notify, Semaphore};
use tokio::task::JoinSet;
use tokio::time::timeout;
use uuid::Uuid;

use crate::tools::CancellationToken;
use crate::tunnel::append_profile_log;

const DEFAULT_REQUEST_TIMEOUT_SECONDS: u64 = 30;
const MAX_TOOL_LIST_PAGES: usize = 100;
const MAX_DISCOVERED_PROXY_TOOLS_PER_SERVER: usize = 4_096;
const MAX_PROXY_TOOLS_PER_SERVER: usize = 256;
const MAX_PROXY_TOOLS_TOTAL: usize = 512;
const MAX_PROXY_TOOL_DEFINITION_BYTES: usize = 128 * 1024;
const MAX_PROXY_TOOL_TITLE_BYTES: usize = 256;
const MAX_PROXY_TOOL_DESCRIPTION_BYTES: usize = 2048;
const MAX_AUTO_PROXY_TOOLS_PER_SERVER: usize = 24;
const DEFAULT_BROWSER_PROXY_TOOLS: &[&str] = &[
    "list_pages",
    "new_page",
    "navigate_page",
    "take_snapshot",
    "take_screenshot",
    "evaluate_script",
    "click",
    "fill",
    "fill_form",
    "wait_for",
    "select_page",
    "close_page",
    "press_key",
    "type_text",
    "hover",
    "handle_dialog",
    "upload_file",
    "resize_page",
];
const DEFAULT_STDIO_MAX_CONCURRENT_REQUESTS: usize = 4;
const DEFAULT_HTTP_MAX_CONCURRENT_REQUESTS: usize = 16;
const MAX_PROXY_CONCURRENT_REQUESTS: usize = 64;
const MAX_HTTP_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
const MCP_SESSION_ID_HEADER: &str = "mcp-session-id";
const MCP_PROTOCOL_VERSION_HEADER: &str = "mcp-protocol-version";
const BROWSER_DEBUGGING_PORT_BASE: u32 = 42_000;
const BROWSER_DEBUGGING_PORT_SPAN: u32 = 20_000;
const BROWSER_DEBUGGING_PORT_STEP: u32 = 7_919;
const BROWSER_DEBUGGING_PORT_ATTEMPTS: u32 = 2_048;
const BROWSER_MANAGEMENT_PAGE_TIMEOUT_SECONDS: u64 = 20;
const PROXY_CONNECTION_REPLACEMENT_TIMEOUT_SECONDS: u64 = 45;
const PROXY_TERMINATION_TIMEOUT_SECONDS: u64 = 5;

#[derive(Debug, Clone)]
struct SanitizedProxyTool {
    public_name: String,
    downstream_name: String,
    definition: Value,
    input_schema: Value,
    output_schema: Value,
    synthesized_output_schema: bool,
}

fn proxy_management_failure(connection: &Value, page_state: &Value) -> Option<String> {
    if let Some(message) = page_state.get("error_message").and_then(Value::as_str) {
        if !message.trim().is_empty() {
            return Some(message.to_string());
        }
    }
    let raw = page_state.get("raw").unwrap_or(&Value::Null);
    if raw.get("isError").and_then(Value::as_bool) == Some(true) {
        return proxy_text_content(raw)
            .filter(|message| !message.trim().is_empty())
            .or_else(|| Some("Downstream browser page-state request failed".into()));
    }
    if connection.get("operational").and_then(Value::as_bool) == Some(false) {
        return connection
            .get("last_error")
            .and_then(Value::as_str)
            .filter(|message| !message.trim().is_empty())
            .map(ToString::to_string)
            .or_else(|| Some("Downstream browser connection is not operational".into()));
    }
    None
}

fn prepare_my_agent_browser_isolation(
    spec: &mut McpProxyServerSpec,
    workspace_id: &str,
) -> Result<Option<BrowserWorkspaceIsolation>, String> {
    if !is_my_agent_browser_spec(spec) {
        return Ok(None);
    }
    if let Some(explicit) = spec.env.get("MY_AGENT_BROWSER_HOME") {
        let home = PathBuf::from(explicit);
        return Ok(Some(BrowserWorkspaceIsolation {
            debugging_port: read_browser_debugging_port(&home.join("config.json"))
                .unwrap_or_default(),
            home,
        }));
    }

    let workspace_segment = sanitize_tool_segment(workspace_id);
    let server_segment = sanitize_tool_segment(&spec.name);
    let home = spec
        .cwd
        .join(".anchor")
        .join("browser")
        .join(workspace_segment)
        .join(server_segment);
    let user_data_dir = home.join("user-data");
    fs::create_dir_all(&user_data_dir).map_err(|error| {
        format!(
            "failed to create workspace browser isolation directory `{}`: {error}",
            home.display()
        )
    })?;
    let config_path = home.join("config.json");
    let debugging_port =
        select_managed_browser_debugging_port(&config_path, workspace_id, &spec.name)?;
    reconcile_managed_browser_config(&config_path, &user_data_dir, debugging_port)?;
    spec.env
        .insert("MY_AGENT_BROWSER_HOME".into(), display_proxy_path(&home));
    Ok(Some(BrowserWorkspaceIsolation {
        home,
        debugging_port: read_browser_debugging_port(&config_path).unwrap_or(debugging_port),
    }))
}

fn reconcile_managed_browser_config(
    config_path: &Path,
    user_data_dir: &Path,
    debugging_port: u16,
) -> Result<(), String> {
    let mut config = if config_path.exists() {
        let content = fs::read_to_string(config_path).map_err(|error| {
            format!(
                "failed to read workspace browser isolation config `{}`: {error}",
                config_path.display()
            )
        })?;
        serde_json::from_str::<Value>(&content).map_err(|error| {
            format!(
                "workspace browser isolation config `{}` is invalid JSON: {error}",
                config_path.display()
            )
        })?
    } else {
        json!({})
    };
    let root = config.as_object_mut().ok_or_else(|| {
        format!(
            "workspace browser isolation config `{}` must contain a JSON object",
            config_path.display()
        )
    })?;
    let browser = root.entry("browser").or_insert_with(|| json!({}));
    let browser = browser.as_object_mut().ok_or_else(|| {
        format!(
            "workspace browser isolation config `{}` must contain a browser object",
            config_path.display()
        )
    })?;

    browser.insert(
        "userDataDir".into(),
        Value::String(display_proxy_path(user_data_dir)),
    );
    browser.insert(
        "debuggingPort".into(),
        Value::Number(u64::from(debugging_port).into()),
    );
    browser.entry("headless").or_insert(Value::Bool(true));
    browser.entry("lazyStart").or_insert(Value::Bool(true));

    let encoded = serde_json::to_string_pretty(&config)
        .map_err(|error| format!("failed to encode browser isolation config: {error}"))?
        + "\n";
    let unchanged = fs::read_to_string(config_path)
        .ok()
        .is_some_and(|current| current == encoded);
    if !unchanged {
        fs::write(config_path, encoded).map_err(|error| {
            format!(
                "failed to write workspace browser isolation config `{}`: {error}",
                config_path.display()
            )
        })?;
    }
    Ok(())
}

fn is_my_agent_browser_spec(spec: &McpProxyServerSpec) -> bool {
    let identity = std::iter::once(spec.command.as_str())
        .chain(spec.args.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();
    identity.contains("my-agent-browser") || identity.contains("start-mcp.js")
}

fn browser_debugging_port(workspace_id: &str, server_name: &str) -> u16 {
    let digest = Sha256::digest(format!("{workspace_id}:{server_name}"));
    let offset =
        u32::from(u16::from_be_bytes([digest[0], digest[1]])) % BROWSER_DEBUGGING_PORT_SPAN;
    u16::try_from(BROWSER_DEBUGGING_PORT_BASE + offset)
        .expect("managed browser debugging port is within the u16 range")
}

fn select_managed_browser_debugging_port(
    config_path: &Path,
    workspace_id: &str,
    server_name: &str,
) -> Result<u16, String> {
    if let Some(existing) = read_browser_debugging_port(config_path) {
        let existing = u32::from(existing);
        let in_managed_range = (BROWSER_DEBUGGING_PORT_BASE
            ..BROWSER_DEBUGGING_PORT_BASE + BROWSER_DEBUGGING_PORT_SPAN)
            .contains(&existing);
        let existing = u16::try_from(existing).expect("existing browser port remains u16");
        if in_managed_range
            && (browser_cdp_port_reachable(existing) || browser_port_is_bindable(existing))
        {
            return Ok(existing);
        }
    }

    let preferred = u32::from(browser_debugging_port(workspace_id, server_name));
    let preferred_offset = preferred.saturating_sub(BROWSER_DEBUGGING_PORT_BASE);
    for attempt in 0..BROWSER_DEBUGGING_PORT_ATTEMPTS {
        let offset = (preferred_offset + attempt.saturating_mul(BROWSER_DEBUGGING_PORT_STEP))
            % BROWSER_DEBUGGING_PORT_SPAN;
        let port = u16::try_from(BROWSER_DEBUGGING_PORT_BASE + offset)
            .expect("managed browser debugging port is within the u16 range");
        if browser_port_is_bindable(port) {
            return Ok(port);
        }
    }

    Err(format!(
        "failed to find an available managed browser debugging port after {BROWSER_DEBUGGING_PORT_ATTEMPTS} deterministic probes"
    ))
}

fn browser_port_is_bindable(port: u16) -> bool {
    TcpListener::bind(("127.0.0.1", port)).is_ok()
}

fn browser_cdp_port_reachable(port: u16) -> bool {
    let address = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let Ok(mut stream) = TcpStream::connect_timeout(&address, Duration::from_millis(100)) else {
        return false;
    };
    let io_timeout = Some(Duration::from_millis(200));
    let _ = stream.set_read_timeout(io_timeout);
    let _ = stream.set_write_timeout(io_timeout);
    if stream
        .write_all(b"GET /json/version HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
        .is_err()
    {
        return false;
    }
    let mut response = [0u8; 8_192];
    let Ok(bytes) = stream.read(&mut response) else {
        return false;
    };
    let response = String::from_utf8_lossy(&response[..bytes]);
    response.starts_with("HTTP/1.1 200") && response.contains("\"Browser\"")
}

fn read_browser_debugging_port(path: &Path) -> Option<u16> {
    let value = serde_json::from_str::<Value>(&fs::read_to_string(path).ok()?).ok()?;
    value
        .get("browser")
        .and_then(|browser| browser.get("debuggingPort"))
        .and_then(Value::as_u64)
        .and_then(|port| u16::try_from(port).ok())
}

fn display_proxy_path(path: &Path) -> String {
    let display = path.to_string_lossy();
    #[cfg(windows)]
    {
        display
            .strip_prefix("\\\\?\\")
            .unwrap_or(&display)
            .to_string()
    }
    #[cfg(not(windows))]
    {
        display.into_owned()
    }
}

fn wrap_proxy_structured_result(structured: Value, output_schema: &Value) -> Value {
    let validation = jsonschema::validator_for(output_schema)
        .and_then(|validator| validator.validate(&structured));
    let structured = match validation {
        Ok(()) => structured,
        Err(error) => json!({
            "ok": false,
            "status": "error",
            "server": "proxy",
            "connection": {},
            "error": {
                "code": "DOWNSTREAM_SCHEMA_MISMATCH",
                "message": format!("Proxy result violates outputSchema: {error}"),
                "retryable": false
            },
            "error_code": "DOWNSTREAM_SCHEMA_MISMATCH",
            "error_message": format!("Proxy result violates outputSchema: {error}"),
            "retryable": false,
            "browser_session_id": "unknown",
            "connection_status": "unknown",
            "transport_connected": false,
            "operational": false,
            "browser_cdp_reachable": null,
            "page_count": 0,
            "pages": [],
            "selected_page": null,
            "page_state": {}
        }),
    };
    let is_error = structured.get("ok").and_then(Value::as_bool) == Some(false);
    json!({
        "content": [{"type": "text", "text": structured.to_string()}],
        "structuredContent": structured,
        "isError": is_error
    })
}

fn proxy_text_content(value: &Value) -> Option<String> {
    value
        .get("content")
        .and_then(Value::as_array)
        .map(|content| {
            content
                .iter()
                .filter(|item| item.get("type") == Some(&json!("text")))
                .filter_map(|item| item.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .filter(|text| !text.trim().is_empty())
}

fn parse_browser_page_label(label: &str) -> (String, String) {
    if let Some(open) = label.rfind(" (") {
        if label.ends_with(')') {
            let candidate = &label[open + 2..label.len() - 1];
            if candidate.starts_with("http://")
                || candidate.starts_with("https://")
                || candidate.starts_with("file://")
                || candidate.starts_with("about:")
                || candidate.starts_with("chrome://")
            {
                return (label[..open].trim().to_string(), candidate.to_string());
            }
        }
    }

    (String::new(), label.to_string())
}

fn browser_proxy_error_code(public_name: &str, detail: &str) -> &'static str {
    let detail = detail.to_ascii_lowercase();
    if detail.contains("structuredcontent")
        || detail.contains("outputschema")
        || detail.contains("schema")
    {
        "DOWNSTREAM_SCHEMA_MISMATCH"
    } else if (detail.contains("uid") || detail.contains("element"))
        && (detail.contains("stale")
            || detail.contains("not found")
            || detail.contains("does not exist")
            || detail.contains("no longer valid"))
    {
        "STALE_ELEMENT_REFERENCE"
    } else if detail.contains("timed out") && public_name.contains("wait_for") {
        "ELEMENT_WAIT_TIMEOUT"
    } else if detail.contains("timed out") && public_name.contains("navigate") {
        "PAGE_LOAD_TIMEOUT"
    } else if detail.contains("timed out") && public_name.contains("evaluate_script") {
        "SCRIPT_TIMEOUT"
    } else if detail.contains("chrome was closed or crashed")
        || detail.contains("browser crashed")
        || detail.contains("chrome crashed")
    {
        "BROWSER_CRASHED"
    } else if detail.contains("target closed")
        || detail.contains("page closed")
        || detail.contains("page has been closed")
    {
        "PAGE_CLOSED"
    } else if detail.contains("no active page")
        || detail.contains("no page selected")
        || detail.contains("no pages")
        || (public_name.contains("snapshot") && detail.contains("page"))
    {
        "NO_ACTIVE_PAGE"
    } else if detail.contains("cdp")
        || detail.contains("devtools")
        || detail.contains("browser disconnected")
        || detail.contains("connection lost")
    {
        "CDP_CONNECTION_LOST"
    } else if detail.contains("not connected")
        || detail.contains("connection refused")
        || detail.contains("failed to connect")
        || detail.contains("failed to start")
        || detail.contains("not reachable")
    {
        "BROWSER_NOT_CONNECTED"
    } else {
        "PROXIED_TOOL_FAILED"
    }
}

fn proxy_result_payload(result: &Value) -> Value {
    let Some(object) = result.as_object() else {
        return json!({"value": result});
    };
    if let Some(structured) = object
        .get("structuredContent")
        .filter(|value| value.is_object())
    {
        return structured.clone();
    }
    let content = object
        .get("content")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let text = content
        .iter()
        .filter(|item| item.get("type") == Some(&json!("text")))
        .filter_map(|item| item.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n");
    if let Ok(parsed) = serde_json::from_str::<Value>(text.trim()) {
        if parsed.is_object() {
            return parsed;
        }
    }
    json!({
        "text": text,
        "content": content.iter().map(proxy_content_summary).collect::<Vec<_>>()
    })
}

fn proxy_content_summary(item: &Value) -> Value {
    let Some(object) = item.as_object() else {
        return json!({"type": "unknown"});
    };
    let content_type = object
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    if matches!(content_type, "image" | "audio") {
        let encoded_bytes = object
            .get("data")
            .and_then(Value::as_str)
            .map(|value| value.len().saturating_mul(3) / 4)
            .unwrap_or_default();
        return json!({
            "type": content_type,
            "mimeType": object.get("mimeType").cloned().unwrap_or(Value::Null),
            "encoded_bytes": encoded_bytes,
            "data_omitted": true
        });
    }
    if content_type == "resource" {
        let resource = object.get("resource").and_then(Value::as_object);
        return json!({
            "type": "resource",
            "uri": resource.and_then(|value| value.get("uri")).cloned().unwrap_or(Value::Null),
            "name": resource.and_then(|value| value.get("name")).cloned().unwrap_or(Value::Null),
            "mimeType": resource.and_then(|value| value.get("mimeType")).cloned().unwrap_or(Value::Null),
            "payload_omitted": resource.is_some_and(|value| value.contains_key("blob") || value.contains_key("text"))
        });
    }
    item.clone()
}

fn proxy_page_state(result: &Value) -> Value {
    let payload = if result.get("structuredContent").is_some() {
        proxy_result_payload(result)
    } else {
        result.clone()
    };
    let payload = payload.get("result").cloned().unwrap_or(payload);
    let mut pages = payload
        .get("pages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if pages.is_empty() {
        let text = payload
            .get("text")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| proxy_text_content(&payload))
            .or_else(|| proxy_text_content(result))
            .unwrap_or_default();
        for line in text.lines() {
            let trimmed = line.trim().trim_start_matches(['-', '*', ' ']);
            let Some((id, rest)) = trimmed.split_once(':') else {
                continue;
            };
            let Ok(page_id) = id.trim().parse::<u64>() else {
                continue;
            };
            let selected = rest.contains("[selected]") || rest.contains("(selected)");
            let label = rest
                .replace("[selected]", "")
                .replace("(selected)", "")
                .trim()
                .to_string();
            let (title, url) = parse_browser_page_label(&label);
            pages.push(json!({
                "page_id": page_id,
                "url": url,
                "title": title,
                "selected": selected
            }));
        }
    }
    let selected_page = payload
        .get("selected_page")
        .or_else(|| payload.get("selectedPage"))
        .cloned()
        .or_else(|| {
            pages
                .iter()
                .find(|page| page.get("selected").and_then(Value::as_bool) == Some(true))
                .cloned()
        })
        .unwrap_or(Value::Null);
    json!({
        "pages": pages,
        "selected_page": selected_page,
        "page_count": pages.len(),
        "raw": payload
    })
}

fn proxy_timestamp() -> String {
    Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn proxy_endpoint_origin(endpoint: &reqwest::Url) -> String {
    let host = endpoint.host_str().unwrap_or("unknown-host");
    match endpoint.port() {
        Some(port) => format!("{}://{host}:{port}", endpoint.scheme()),
        None => format!("{}://{host}", endpoint.scheme()),
    }
}

fn proxy_failure_reason(public_name: &str, error: &ProxyClientError) -> String {
    let detail = error.to_string().to_ascii_lowercase();
    if matches!(error, ProxyClientError::Cancelled) {
        return "proxy_call_cancelled".into();
    }
    if matches!(error, ProxyClientError::Timeout { .. }) {
        if public_name.contains("navigate")
            || public_name.contains("goto")
            || public_name.contains("reload")
            || detail.contains("page load")
            || detail.contains("navigation")
        {
            return "page_load_timeout".into();
        }
        if public_name.contains("wait")
            || public_name.contains("click")
            || public_name.contains("fill")
            || detail.contains("selector")
            || detail.contains("locator")
        {
            return "element_wait_timeout".into();
        }
        return "tool_service_timeout".into();
    }
    if matches!(error, ProxyClientError::Transport(_))
        && (detail.contains("devtools")
            || detail.contains("cdp")
            || detail.contains("target closed")
            || detail.contains("browser disconnected"))
    {
        return "devtools_channel_disconnected".into();
    }
    if matches!(error, ProxyClientError::Transport(_)) {
        return "proxy_transport_disconnected".into();
    }
    if matches!(error, ProxyClientError::Remote(_)) {
        return "proxy_downstream_error".into();
    }
    if matches!(error, ProxyClientError::Protocol(_)) {
        return "proxy_protocol_error".into();
    }
    "proxy_call_failed".into()
}

fn proxy_result_state_summary(result: &Value) -> Value {
    let structured = result.get("structuredContent").unwrap_or(result);
    let keys = [
        "url",
        "currentUrl",
        "pageUrl",
        "title",
        "pageTitle",
        "currentPage",
        "current_page",
        "pages",
        "selected_page",
        "page_count",
        "activeElement",
        "focusPath",
        "openDialogs",
        "openPopovers",
        "openTooltips",
        "dismissableLayerStack",
        "dataState",
        "visibility",
        "boundingBox",
        "inertAncestors",
        "ariaHiddenAncestors",
        "viewport",
    ];
    let mut summary = serde_json::Map::new();
    for key in keys {
        if let Some(value) = find_proxy_state_value(structured, key, 0) {
            summary.insert(key.into(), bounded_proxy_state_value(value, 0));
        }
    }
    let current_url = summary
        .get("currentUrl")
        .or_else(|| summary.get("pageUrl"))
        .or_else(|| summary.get("url"))
        .cloned();
    let current_title = summary
        .get("pageTitle")
        .or_else(|| summary.get("title"))
        .cloned();
    if current_url.is_some() || current_title.is_some() {
        summary.insert(
            "current_page".into(),
            json!({"url": current_url, "title": current_title}),
        );
    }
    Value::Object(summary)
}

fn merge_proxy_success_summary(previous: Option<&Value>, result: &Value) -> Value {
    let mut next = proxy_result_state_summary(result);
    let Some(previous) = previous.and_then(Value::as_object) else {
        return next;
    };
    let Some(next_object) = next.as_object_mut() else {
        return next;
    };
    let has_pages = next_object
        .get("pages")
        .and_then(Value::as_array)
        .is_some_and(|pages| !pages.is_empty());
    if !has_pages {
        for key in ["pages", "page_count"] {
            if let Some(value) = previous.get(key) {
                next_object.insert(key.into(), value.clone());
            }
        }
    }
    let has_selected_page = next_object
        .get("selected_page")
        .is_some_and(|value| !value.is_null());
    if !has_selected_page {
        if let Some(value) = previous
            .get("selected_page")
            .or_else(|| previous.get("current_page"))
        {
            next_object.insert("selected_page".into(), value.clone());
        }
    }
    next
}

fn find_proxy_state_value<'a>(value: &'a Value, key: &str, depth: usize) -> Option<&'a Value> {
    if depth > 3 {
        return None;
    }
    match value {
        Value::Object(object) => {
            if let Some(value) = object.get(key) {
                return Some(value);
            }
            object
                .values()
                .find_map(|value| find_proxy_state_value(value, key, depth + 1))
        }
        Value::Array(values) => values
            .iter()
            .take(20)
            .find_map(|value| find_proxy_state_value(value, key, depth + 1)),
        _ => None,
    }
}

fn bounded_proxy_state_value(value: &Value, depth: usize) -> Value {
    if depth >= 2 {
        return match value {
            Value::String(text) => Value::String(truncate_log_detail(text, 2_048)),
            Value::Bool(_) | Value::Number(_) | Value::Null => value.clone(),
            Value::Array(values) => json!({"item_count": values.len(), "truncated": true}),
            Value::Object(object) => json!({"field_count": object.len(), "truncated": true}),
        };
    }
    match value {
        Value::String(text) => Value::String(truncate_log_detail(text, 2_048)),
        Value::Array(values) => Value::Array(
            values
                .iter()
                .take(20)
                .map(|value| bounded_proxy_state_value(value, depth + 1))
                .collect(),
        ),
        Value::Object(object) => Value::Object(
            object
                .iter()
                .take(20)
                .map(|(key, value)| (key.clone(), bounded_proxy_state_value(value, depth + 1)))
                .collect(),
        ),
        _ => value.clone(),
    }
}

fn proxy_management_tools(
    spec: &McpProxyServerSpec,
) -> Vec<(String, ProxyRouteKind, Value, Value, Value)> {
    if !spec.management_tools {
        return Vec::new();
    }
    let input_schema = json!({
        "type": "object",
        "properties": {},
        "additionalProperties": false
    });
    let output_schema = json!({
        "type": "object",
        "properties": {
            "ok": {"type": "boolean"},
            "status": {"type": "string"},
            "server": {"type": "string"},
            "connection": {"type": "object", "additionalProperties": true},
            "error": {"type": ["object", "null"]},
            "error_code": {"type": ["string", "null"]},
            "error_message": {"type": ["string", "null"]},
            "retryable": {"type": "boolean"},
            "browser_session_id": {"type": "string", "minLength": 1},
            "connection_status": {"type": "string"},
            "transport_connected": {"type": "boolean"},
            "operational": {"type": "boolean"},
            "browser_cdp_reachable": {"type": ["boolean", "null"]},
            "observed_at": {"type": "string", "minLength": 1},
            "recovered_during_probe": {"type": "boolean"},
            "page_count": {"type": "integer", "minimum": 0},
            "pages": {"type": "array", "items": {"type": "object"}},
            "selected_page": {"type": ["object", "null"]},
            "page_state": {"type": "object", "additionalProperties": true}
        },
        "required": [
            "ok", "status", "server", "connection", "retryable",
            "browser_session_id", "connection_status", "transport_connected",
            "operational", "observed_at", "recovered_during_probe", "page_count", "pages"
        ],
        "additionalProperties": true
    });
    [
        ("health_check", ProxyRouteKind::HealthCheck, true),
        ("reconnect", ProxyRouteKind::Reconnect, false),
        ("reset_session", ProxyRouteKind::ResetSession, false),
    ]
    .into_iter()
    .map(|(suffix, kind, read_only)| {
        let public_name = format!("{}__{suffix}", spec.tool_prefix);
        let title = format!("{} {suffix}", spec.name);
        let definition = json!({
            "name": public_name,
            "title": title,
            "description": format!("Manage or inspect the downstream MCP connection for {}.", spec.name),
            "inputSchema": input_schema,
            "outputSchema": output_schema,
            "annotations": {
                "title": title,
                "readOnlyHint": read_only,
                "destructiveHint": !read_only,
                "idempotentHint": true,
                "openWorldHint": true
            }
        });
        (
            public_name,
            kind,
            definition,
            input_schema.clone(),
            output_schema.clone(),
        )
    })
    .collect()
}

fn fallback_proxy_output_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "ok": { "type": "boolean" },
            "result": {
                "type": "object",
                "additionalProperties": true
            },
            "error": { "type": ["object", "null"] },
            "error_code": { "type": ["string", "null"] },
            "error_message": { "type": ["string", "null"] },
            "retryable": { "type": "boolean" },
            "browser_session_id": { "type": ["string", "null"] },
            "connection_status": { "type": "string" },
            "transport_connected": { "type": "boolean" },
            "operational": { "type": "boolean" },
            "browser_cdp_reachable": { "type": ["boolean", "null"] },
            "page_count": { "type": ["integer", "null"], "minimum": 0 },
            "pages": { "type": "array", "items": { "type": "object" } },
            "selected_page": { "type": ["object", "null"] }
        },
        "required": ["ok"],
        "additionalProperties": true
    })
}

#[derive(Debug)]
struct SanitizedProxyCatalog {
    tools: Vec<SanitizedProxyTool>,
    discovered_count: usize,
    filtered_count: usize,
    truncated_count: usize,
    selection_source: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProxyExposureMode {
    Auto,
    Full,
}

impl ProxyExposureMode {
    fn label(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Full => "full",
        }
    }
}

#[derive(Debug, Clone)]
pub struct McpProxyServerSpec {
    pub name: String,
    transport: McpProxyTransportSpec,
    command: String,
    args: Vec<String>,
    env: BTreeMap<String, String>,
    cwd: PathBuf,
    tool_prefix: String,
    include_tools: Option<BTreeSet<String>>,
    exclude_tools: BTreeSet<String>,
    max_tools: Option<usize>,
    exposure_mode: ProxyExposureMode,
    max_concurrent_requests: usize,
    request_timeout: Duration,
    management_tools: bool,
}

#[derive(Debug, Clone)]
enum McpProxyTransportSpec {
    Stdio,
    StreamableHttp {
        url: String,
        headers: BTreeMap<String, String>,
    },
}

impl McpProxyTransportSpec {
    fn label(&self) -> &'static str {
        match self {
            Self::Stdio => "stdio",
            Self::StreamableHttp { .. } => "streamable-http",
        }
    }
}

fn validate_schema_document(schema: &Value, label: &str) -> Result<(), String> {
    let Some(object) = schema.as_object() else {
        return Err(format!("{label} must be a JSON Schema object"));
    };
    if object.get("type").and_then(Value::as_str) != Some("object") {
        return Err(format!("{label} root type must be object"));
    }
    if schema_contains_external_ref(schema) {
        return Err(format!(
            "{label} contains an external JSON Schema reference"
        ));
    }
    jsonschema::meta::validate(schema)
        .map_err(|error| format!("{label} is not a valid JSON Schema: {error}"))
}

fn schema_contains_external_ref(value: &Value) -> bool {
    match value {
        Value::Object(object) => object.iter().any(|(key, value)| {
            if matches!(key.as_str(), "$ref" | "$dynamicRef") {
                return value
                    .as_str()
                    .is_some_and(|reference| !reference.starts_with('#'));
            }
            schema_contains_external_ref(value)
        }),
        Value::Array(items) => items.iter().any(schema_contains_external_ref),
        _ => false,
    }
}

fn valid_public_tool_name(name: &str) -> bool {
    (1..=128).contains(&name.len())
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

fn bounded_text(value: Option<&Value>, maximum: usize) -> Option<String> {
    let text = value
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())?;
    if text.len() <= maximum {
        return Some(text.to_string());
    }
    let mut end = maximum.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    (end > 0).then(|| text[..end].to_string())
}

fn sanitize_proxy_catalog(
    spec: &McpProxyServerSpec,
    catalog: Vec<Value>,
) -> Result<SanitizedProxyCatalog, String> {
    if catalog.len() > MAX_DISCOVERED_PROXY_TOOLS_PER_SERVER {
        return Err(format!(
            "downstream MCP `{}` returned {} tools; discovery maximum is {MAX_DISCOVERED_PROXY_TOOLS_PER_SERVER}",
            spec.name,
            catalog.len()
        ));
    }
    let discovered_count = catalog.len();
    let mut selected = Vec::with_capacity(catalog.len());
    let mut discovered_names = BTreeSet::new();
    for (index, tool) in catalog.into_iter().enumerate() {
        let downstream_name = tool
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty() && name.len() <= 128)
            .ok_or_else(|| format!("downstream tool #{index} has an invalid name"))?
            .to_string();
        discovered_names.insert(downstream_name.clone());
        if spec
            .include_tools
            .as_ref()
            .is_some_and(|included| !included.contains(&downstream_name))
            || spec.exclude_tools.contains(&downstream_name)
        {
            continue;
        }
        selected.push((downstream_name, tool));
    }
    if let Some(included) = spec.include_tools.as_ref() {
        let missing = included
            .difference(&discovered_names)
            .cloned()
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            return Err(format!(
                "downstream MCP `{}` did not advertise includeTools entries: {}",
                spec.name,
                missing.join(", ")
            ));
        }
    }

    let selection_source = if spec.include_tools.is_some() {
        "explicit_include"
    } else if spec.max_tools.is_some() {
        "explicit_max"
    } else if spec.exposure_mode == ProxyExposureMode::Full {
        "explicit_full"
    } else if is_my_agent_browser_spec(spec) {
        selected.retain(|(name, _)| DEFAULT_BROWSER_PROXY_TOOLS.contains(&name.as_str()));
        "browser_workflow"
    } else if selected.len() > MAX_AUTO_PROXY_TOOLS_PER_SERVER {
        return Err(format!(
            "downstream MCP `{}` advertises {} tools without an explicit exposure policy; automatic exposure is limited to {MAX_AUTO_PROXY_TOOLS_PER_SERVER}. Configure includeTools/maxTools, or set exposureMode to `full` after reviewing the catalog",
            spec.name,
            selected.len()
        ));
    } else {
        "auto_all"
    };

    selected.sort_by(|left, right| left.0.cmp(&right.0));
    let filtered_count = discovered_count.saturating_sub(selected.len());
    let truncated_count = spec
        .max_tools
        .map(|maximum| selected.len().saturating_sub(maximum))
        .unwrap_or_default();
    if let Some(maximum) = spec.max_tools {
        selected.truncate(maximum);
    }
    if selected.len() > MAX_PROXY_TOOLS_PER_SERVER {
        return Err(format!(
            "downstream MCP `{}` selected {} tools after includeTools/excludeTools; maximum is {MAX_PROXY_TOOLS_PER_SERVER}. Configure maxTools or narrower filters",
            spec.name,
            selected.len()
        ));
    }

    let mut sanitized = Vec::with_capacity(selected.len());
    let mut names = std::collections::HashSet::new();
    for (index, (downstream_name, tool)) in selected.into_iter().enumerate() {
        let Some(object) = tool.as_object() else {
            return Err(format!("downstream tool #{index} is not an object"));
        };
        if serde_json::to_vec(&tool)
            .map_err(|error| error.to_string())?
            .len()
            > MAX_PROXY_TOOL_DEFINITION_BYTES
        {
            return Err(format!("downstream tool #{index} definition is too large"));
        }
        let public_name = format!(
            "{}__{}",
            sanitize_tool_segment(&spec.tool_prefix),
            sanitize_tool_segment(&downstream_name)
        );
        if !valid_public_tool_name(&public_name) {
            return Err(format!(
                "proxied tool name `{public_name}` is invalid or too long"
            ));
        }
        if !names.insert(public_name.clone()) {
            return Err(format!(
                "downstream tools map to duplicate public name `{public_name}`"
            ));
        }
        let input_schema = object
            .get("inputSchema")
            .ok_or_else(|| format!("downstream tool `{downstream_name}` is missing inputSchema"))?
            .clone();
        validate_schema_document(
            &input_schema,
            &format!("downstream tool `{downstream_name}` inputSchema"),
        )?;
        let (output_schema, synthesized_output_schema) =
            if let Some(schema) = object.get("outputSchema") {
                validate_schema_document(
                    schema,
                    &format!("downstream tool `{downstream_name}` outputSchema"),
                )?;
                (schema.clone(), false)
            } else {
                (fallback_proxy_output_schema(), true)
            };
        let title = bounded_text(object.get("title"), MAX_PROXY_TOOL_TITLE_BYTES)
            .unwrap_or_else(|| format!("{} · {}", spec.name, downstream_name));
        let description = bounded_text(object.get("description"), MAX_PROXY_TOOL_DESCRIPTION_BYTES)
            .map(|description| format!("[{}] {description}", spec.name))
            .unwrap_or_else(|| format!("Proxied from MCP server {}", spec.name));
        let mut definition = json!({
            "name": public_name,
            "title": title,
            "description": description,
            "inputSchema": input_schema,
            // A proxy cannot independently attest downstream side effects.
            // Publish conservative annotations rather than trusting claims.
            "annotations": {
                "title": title,
                "readOnlyHint": false,
                "destructiveHint": true,
                "idempotentHint": false,
                "openWorldHint": true
            }
        });
        definition["outputSchema"] = output_schema.clone();
        sanitized.push(SanitizedProxyTool {
            public_name,
            downstream_name,
            definition,
            input_schema,
            output_schema,
            synthesized_output_schema,
        });
    }
    sanitized.sort_by(|left, right| left.public_name.cmp(&right.public_name));
    Ok(SanitizedProxyCatalog {
        tools: sanitized,
        discovered_count,
        filtered_count,
        truncated_count,
        selection_source,
    })
}

fn proxy_catalog_digest(catalog: &[SanitizedProxyTool]) -> Result<String, String> {
    let contracts = catalog
        .iter()
        .map(|tool| {
            json!({
                "publicName": tool.public_name,
                "downstreamName": tool.downstream_name,
                "definition": tool.definition
            })
        })
        .collect::<Vec<_>>();
    let encoded = serde_json::to_vec(&contracts)
        .map_err(|error| format!("failed to encode proxied tool catalog: {error}"))?;
    Ok(format!("{:x}", Sha256::digest(encoded)))
}

async fn connect_initial_with_retry(
    spec: McpProxyServerSpec,
    workspace_id: String,
) -> Result<(Arc<ProxyServer>, SanitizedProxyCatalog), String> {
    let mut last_error = String::new();
    for attempt in 1u8..=3 {
        let attempt_timeout = spec.request_timeout.min(Duration::from_secs(20));
        match timeout(
            attempt_timeout,
            ProxyServer::connect_initial(spec.clone(), workspace_id.clone()),
        )
        .await
        {
            Ok(Ok(connected)) => return Ok(connected),
            Ok(Err(error)) => {
                last_error = error;
                if attempt < 3 {
                    tokio::time::sleep(proxy_reconnect_delay(attempt)).await;
                }
            }
            Err(_) => {
                last_error = format!(
                    "initial connection attempt timed out after {} seconds",
                    attempt_timeout.as_secs()
                );
                if attempt < 3 {
                    tokio::time::sleep(proxy_reconnect_delay(attempt)).await;
                }
            }
        }
    }
    Err(format!(
        "initial connection failed after 3 attempts: {last_error}"
    ))
}

struct ProxyServer {
    spec: McpProxyServerSpec,
    workspace_id: String,
    catalog_digest: String,
    downstream_tools: BTreeSet<String>,
    discovered_tool_count: usize,
    filtered_tool_count: usize,
    truncated_tool_count: usize,
    selection_source: &'static str,
    client: Mutex<Option<Arc<McpProxyClient>>>,
    session_id: StdMutex<String>,
    concurrency: Semaphore,
    reconnect_scheduled: AtomicBool,
    calls: AtomicU64,
    failures: AtomicU64,
    cancellations: AtomicU64,
    timeouts: AtomicU64,
    queue_timeouts: AtomicU64,
    last_error: StdMutex<Option<String>>,
    last_error_code: StdMutex<Option<String>>,
    last_error_at: StdMutex<Option<String>>,
    last_success_at: StdMutex<Option<String>>,
    last_success_tool: StdMutex<Option<String>>,
    last_success_summary: StdMutex<Option<Value>>,
    reconnect_attempts: AtomicU64,
    last_reconnect_at: StdMutex<Option<String>>,
}

#[derive(Debug, Clone, Deserialize)]
struct RawMcpServerConfig {
    #[serde(rename = "type", default)]
    transport_type: String,
    #[serde(default)]
    command: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    headers: BTreeMap<String, String>,
    #[serde(default)]
    disabled: bool,
    #[serde(rename = "toolPrefix", default)]
    tool_prefix: Option<String>,
    #[serde(rename = "includeTools", default)]
    include_tools: Option<Vec<String>>,
    #[serde(rename = "excludeTools", default)]
    exclude_tools: Vec<String>,
    #[serde(rename = "maxTools", default)]
    max_tools: Option<usize>,
    #[serde(rename = "exposureMode", default)]
    exposure_mode: Option<String>,
    #[serde(rename = "maxConcurrentRequests", default)]
    max_concurrent_requests: Option<usize>,
    #[serde(rename = "requestTimeoutSeconds", default)]
    request_timeout_seconds: Option<u64>,
    #[serde(rename = "managementTools", default)]
    management_tools: Option<bool>,
}

#[derive(Debug, Clone, Copy)]
enum ProxyRouteKind {
    Downstream,
    HealthCheck,
    Reconnect,
    ResetSession,
}

#[derive(Clone)]
struct ProxyRoute {
    server: Arc<ProxyServer>,
    server_name: String,
    downstream_name: String,
    input_schema: Value,
    output_schema: Value,
    synthesized_output_schema: bool,
    kind: ProxyRouteKind,
}

#[derive(Default)]
struct RegistryState {
    tools: Vec<Value>,
    routes: HashMap<String, ProxyRoute>,
    failures: BTreeMap<String, String>,
}

#[derive(Clone)]
pub struct McpProxyRegistry {
    state: Arc<RwLock<RegistryState>>,
    configured: Arc<AtomicBool>,
    configured_notify: Arc<Notify>,
}

impl Default for McpProxyRegistry {
    fn default() -> Self {
        Self {
            state: Arc::new(RwLock::new(RegistryState::default())),
            configured: Arc::new(AtomicBool::new(true)),
            configured_notify: Arc::new(Notify::new()),
        }
    }
}

struct McpProxyClient {
    transport: ProxyClientTransport,
}

enum ProxyClientTransport {
    Stdio(Arc<StdioMcpProxyClient>),
    StreamableHttp(Arc<HttpMcpProxyClient>),
}

struct StdioMcpProxyClient {
    request_timeout: Duration,
    writer: Mutex<ChildStdin>,
    child: Mutex<Option<Child>>,
    pending: StdMutex<HashMap<u64, oneshot::Sender<Result<Value, ProxyClientError>>>>,
    next_id: AtomicU64,
    closed: AtomicBool,
    close_reason: StdMutex<Option<String>>,
    workspace_id: String,
    server_name: String,
    workspace_isolation: Option<BrowserWorkspaceIsolation>,
}

#[derive(Debug, Clone)]
struct BrowserWorkspaceIsolation {
    home: PathBuf,
    debugging_port: u16,
}

struct HttpMcpProxyClient {
    request_timeout: Duration,
    endpoint: reqwest::Url,
    client: reqwest::Client,
    configured_headers: HeaderMap,
    session_id: StdMutex<Option<String>>,
    protocol_version: StdMutex<String>,
    next_id: AtomicU64,
    closed: AtomicBool,
    workspace_id: String,
    server_name: String,
}

#[derive(Debug, Clone)]
enum ProxyClientError {
    Cancelled,
    Timeout { method: String, seconds: u64 },
    Transport(String),
    Remote(String),
    Protocol(String),
}

impl ProxyClientError {
    fn is_cancelled(&self) -> bool {
        matches!(self, Self::Cancelled)
    }

    fn invalidates_connection(&self) -> bool {
        matches!(self, Self::Transport(_))
    }

    fn retryable(&self) -> bool {
        matches!(self, Self::Timeout { .. } | Self::Transport(_))
    }
}

impl std::fmt::Display for ProxyClientError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cancelled => formatter.write_str("request cancelled"),
            Self::Timeout { method, seconds } => write!(
                formatter,
                "request `{method}` timed out after {seconds} seconds"
            ),
            Self::Transport(message) | Self::Remote(message) | Self::Protocol(message) => {
                formatter.write_str(message)
            }
        }
    }
}

impl McpProxyRegistry {
    pub fn begin_configuration(&self) {
        self.configured.store(false, Ordering::Release);
        *self.state.write().expect("mcp proxy registry write") = RegistryState::default();
    }

    pub async fn wait_until_configured(&self, limit: Duration) -> bool {
        if self.configured.load(Ordering::Acquire) {
            return true;
        }
        let notified = self.configured_notify.notified();
        if self.configured.load(Ordering::Acquire) {
            return true;
        }
        timeout(limit, notified).await.is_ok() && self.configured.load(Ordering::Acquire)
    }

    pub fn list_tools(&self) -> Vec<Value> {
        self.state
            .read()
            .expect("mcp proxy registry read")
            .tools
            .clone()
    }

    pub fn contains_tool(&self, public_name: &str) -> bool {
        self.state
            .read()
            .expect("mcp proxy registry read")
            .routes
            .contains_key(public_name)
    }

    pub fn status(&self) -> Value {
        let state = self.state.read().expect("mcp proxy registry read");
        let mut servers = BTreeMap::<String, (Arc<ProxyServer>, usize)>::new();
        for route in state.routes.values() {
            let entry = servers
                .entry(route.server_name.clone())
                .or_insert_with(|| (route.server.clone(), 0));
            entry.1 += 1;
        }
        let servers = servers
            .into_values()
            .map(|(server, tool_count)| server.status(tool_count))
            .collect::<Vec<_>>();
        let unavailable_servers = state
            .failures
            .iter()
            .map(|(name, error)| json!({"name": name, "error": error}))
            .collect::<Vec<_>>();
        json!({
            "configured": self.configured.load(Ordering::Acquire),
            "server_count": servers.len(),
            "servers": servers,
            "unavailable_server_count": unavailable_servers.len(),
            "unavailable_servers": unavailable_servers
        })
    }

    pub async fn configure(&self, specs: Vec<McpProxyServerSpec>, workspace_id: &str) {
        self.begin_configuration();
        let mut tasks = JoinSet::new();
        for spec in specs {
            let server_name = spec.name.clone();
            let workspace_id = workspace_id.to_string();
            tasks.spawn(async move {
                let result = connect_initial_with_retry(spec, workspace_id.clone()).await;
                (server_name, workspace_id, result)
            });
        }

        let mut connected_results = Vec::new();
        while let Some(joined) = tasks.join_next().await {
            let Ok(result) = joined else {
                append_profile_log(
                    workspace_id,
                    "stderr.log",
                    "[mcp-proxy] initialization task failed",
                );
                continue;
            };
            connected_results.push(result);
        }
        connected_results.sort_by(|left, right| left.0.cmp(&right.0));

        for (server_name, workspace_id, result) in connected_results {
            match result {
                Ok((server, catalog)) => {
                    let mut added = 0usize;
                    let mut skipped_duplicates = Vec::new();
                    let mut state = self.state.write().expect("mcp proxy registry write");
                    for tool in catalog.tools {
                        if state.tools.len() >= MAX_PROXY_TOOLS_TOTAL {
                            append_profile_log(
                                &workspace_id,
                                "stderr.log",
                                &format!(
                                    "[mcp-proxy:{server_name}] global proxied tool limit {MAX_PROXY_TOOLS_TOTAL} reached"
                                ),
                            );
                            break;
                        }
                        let public_name = tool.public_name.clone();
                        if state.routes.contains_key(&public_name) {
                            skipped_duplicates.push(public_name);
                            continue;
                        }
                        state.routes.insert(
                            public_name,
                            ProxyRoute {
                                server: server.clone(),
                                server_name: server_name.clone(),
                                downstream_name: tool.downstream_name,
                                input_schema: tool.input_schema,
                                output_schema: tool.output_schema,
                                synthesized_output_schema: tool.synthesized_output_schema,
                                kind: ProxyRouteKind::Downstream,
                            },
                        );
                        state.tools.push(tool.definition);
                        added += 1;
                    }
                    for (public_name, kind, definition, input_schema, output_schema) in
                        proxy_management_tools(&server.spec)
                    {
                        if state.tools.len() >= MAX_PROXY_TOOLS_TOTAL {
                            break;
                        }
                        if state.routes.contains_key(&public_name) {
                            skipped_duplicates.push(public_name);
                            continue;
                        }
                        state.routes.insert(
                            public_name,
                            ProxyRoute {
                                server: server.clone(),
                                server_name: server_name.clone(),
                                downstream_name: String::new(),
                                input_schema,
                                output_schema,
                                synthesized_output_schema: false,
                                kind,
                            },
                        );
                        state.tools.push(definition);
                        added += 1;
                    }
                    drop(state);
                    for public_name in skipped_duplicates {
                        append_profile_log(
                            &workspace_id,
                            "stderr.log",
                            &format!(
                                "[mcp-proxy:{server_name}] skipped duplicate merged tool {public_name}"
                            ),
                        );
                    }
                    append_profile_log(
                        &workspace_id,
                        "stdout.log",
                        &format!(
                            "[mcp-proxy:{server_name}] connected; exposure_mode={} selection_source={} discovered={} filtered={} max_tools_truncated={} merged={added}",
                            server.spec.exposure_mode.label(),
                            catalog.selection_source,
                            catalog.discovered_count,
                            catalog.filtered_count,
                            catalog.truncated_count
                        ),
                    );
                }
                Err(error) => {
                    self.state
                        .write()
                        .expect("mcp proxy registry write")
                        .failures
                        .insert(server_name.clone(), error.clone());
                    append_profile_log(
                        &workspace_id,
                        "stderr.log",
                        &format!("[mcp-proxy:{server_name}] unavailable: {error}"),
                    );
                }
            }
        }
        self.state
            .write()
            .expect("mcp proxy registry write")
            .tools
            .sort_by(|left, right| {
                left.get("name")
                    .and_then(Value::as_str)
                    .cmp(&right.get("name").and_then(Value::as_str))
            });
        self.configured.store(true, Ordering::Release);
        self.configured_notify.notify_waiters();
    }

    pub async fn call_tool(
        &self,
        public_name: &str,
        arguments: &Value,
    ) -> Option<Result<Value, Value>> {
        self.call_tool_with_cancellation(public_name, arguments, &CancellationToken::default())
            .await
    }

    pub async fn call_tool_with_cancellation(
        &self,
        public_name: &str,
        arguments: &Value,
        cancellation: &CancellationToken,
    ) -> Option<Result<Value, Value>> {
        let route = self
            .state
            .read()
            .expect("mcp proxy registry read")
            .routes
            .get(public_name)
            .cloned()?;

        let server = route.server.clone();
        let server_name = route.server_name.clone();
        let input_validator = match jsonschema::validator_for(&route.input_schema) {
            Ok(validator) => validator,
            Err(error) => {
                return Some(Ok(proxy_call_error_result(
                    &server_name,
                    public_name,
                    "proxy_input_schema_invalid",
                    error.to_string(),
                    false,
                    false,
                )))
            }
        };
        if let Err(error) = input_validator.validate(arguments) {
            return Some(Ok(proxy_call_error_result(
                &server_name,
                public_name,
                "proxy_input_invalid",
                error.to_string(),
                false,
                false,
            )));
        }
        if !matches!(route.kind, ProxyRouteKind::Downstream) {
            let structured = server
                .handle_management_call(route.kind, cancellation)
                .await;
            return Some(Ok(wrap_proxy_structured_result(
                structured,
                &route.output_schema,
            )));
        }
        let permit = timeout(server.spec.request_timeout, server.concurrency.acquire());
        tokio::pin!(permit);
        let _permit = tokio::select! {
            _ = cancellation.cancelled() => {
                server.record_queue_cancelled();
                return Some(Ok(proxy_call_error_result(
                    &server_name,
                    public_name,
                    "proxy_call_cancelled",
                    "request cancelled while waiting for downstream capacity".into(),
                    false,
                    false,
                )));
            }
            permit = &mut permit => match permit {
                Ok(Ok(permit)) => permit,
                Ok(Err(_)) => {
                    server.record_failure_message("downstream concurrency queue is closed");
                    return Some(Ok(proxy_call_error_result(
                        &server_name,
                        public_name,
                        "proxy_queue_closed",
                        "downstream concurrency queue is closed".into(),
                        true,
                        false,
                    )));
                }
                Err(_) => {
                    server.record_queue_timeout();
                    return Some(Ok(proxy_call_error_result(
                        &server_name,
                        public_name,
                        "proxy_queue_timeout",
                        format!(
                            "waited {} seconds for downstream capacity",
                            server.spec.request_timeout.as_secs()
                        ),
                        true,
                        false,
                    )));
                }
            }
        };
        server.record_call_started();
        let client = match server.ensure_client().await {
            Ok(client) => client,
            Err(message) => {
                server.record_failure_message(&message);
                server.clone().schedule_reconnect();
                return Some(Ok(proxy_call_error_result(
                    &server_name,
                    public_name,
                    "proxy_reconnect_failed",
                    message,
                    true,
                    true,
                )));
            }
        };
        let result = client
            .request_with_cancellation(
                "tools/call",
                json!({
                    "name": route.downstream_name,
                    "arguments": arguments
                }),
                cancellation,
            )
            .await;
        if let Err(error) = &result {
            server.record_client_error(public_name, error);
            let cancelled = error.is_cancelled();
            let connection_lost = error.invalidates_connection();
            if connection_lost {
                server.invalidate_client(&client).await;
                server.clone().schedule_reconnect();
            }
            let log_message = if cancelled {
                format!("[mcp-proxy:{server_name}] request cancelled without closing the downstream connection")
            } else if connection_lost {
                format!("[mcp-proxy:{server_name}] connection lost; reconnect scheduled: {error}")
            } else {
                format!("[mcp-proxy:{server_name}] request failed without reconnect: {error}")
            };
            append_profile_log(&server.workspace_id, "stderr.log", &log_message);
        }

        Some(Ok(match result {
            Ok(result) => {
                match normalize_proxy_tool_result(
                    &server_name,
                    public_name,
                    result,
                    &route.output_schema,
                    route.synthesized_output_schema,
                ) {
                    Ok(mut normalized) => {
                        if route.synthesized_output_schema {
                            server.decorate_proxy_result(public_name, &mut normalized);
                        }
                        if let Some((code, message)) = normalized_proxy_failure(&normalized) {
                            server.record_failure(&code, &message);
                        } else {
                            server.record_success(public_name, Some(&normalized));
                        }
                        normalized
                    }
                    Err(message) => {
                        server.record_failure("proxy_result_invalid", &message);
                        let mut error_result = proxy_call_error_result(
                            &server_name,
                            public_name,
                            "proxy_result_invalid",
                            message,
                            false,
                            false,
                        );
                        server.decorate_proxy_result(public_name, &mut error_result);
                        error_result
                    }
                }
            }
            Err(error) => {
                let connection_lost = error.invalidates_connection();
                let mut error_result = proxy_call_error_result(
                    &server_name,
                    public_name,
                    &proxy_failure_reason(public_name, &error),
                    error.to_string(),
                    error.retryable(),
                    connection_lost,
                );
                server.decorate_proxy_result(public_name, &mut error_result);
                error_result
            }
        }))
    }
}

impl ProxyServer {
    async fn connect_initial(
        spec: McpProxyServerSpec,
        workspace_id: String,
    ) -> Result<(Arc<Self>, SanitizedProxyCatalog), String> {
        let (client, catalog) = McpProxyClient::connect(spec.clone(), &workspace_id).await?;
        let catalog = sanitize_proxy_catalog(&spec, catalog)?;
        let catalog_digest = proxy_catalog_digest(&catalog.tools)?;
        let downstream_tools = catalog
            .tools
            .iter()
            .map(|tool| tool.downstream_name.clone())
            .collect();
        let discovered_tool_count = catalog.discovered_count;
        let filtered_tool_count = catalog.filtered_count;
        let truncated_tool_count = catalog.truncated_count;
        let selection_source = catalog.selection_source;
        let max_concurrent_requests = spec.max_concurrent_requests;
        Ok((
            Arc::new(Self {
                spec,
                workspace_id,
                catalog_digest,
                downstream_tools,
                discovered_tool_count,
                filtered_tool_count,
                truncated_tool_count,
                selection_source,
                client: Mutex::new(Some(client)),
                session_id: StdMutex::new(Uuid::new_v4().to_string()),
                concurrency: Semaphore::new(max_concurrent_requests),
                reconnect_scheduled: AtomicBool::new(false),
                calls: AtomicU64::new(0),
                failures: AtomicU64::new(0),
                cancellations: AtomicU64::new(0),
                timeouts: AtomicU64::new(0),
                queue_timeouts: AtomicU64::new(0),
                last_error: StdMutex::new(None),
                last_error_code: StdMutex::new(None),
                last_error_at: StdMutex::new(None),
                last_success_at: StdMutex::new(None),
                last_success_tool: StdMutex::new(None),
                last_success_summary: StdMutex::new(None),
                reconnect_attempts: AtomicU64::new(0),
                last_reconnect_at: StdMutex::new(None),
            }),
            catalog,
        ))
    }

    async fn ensure_client(&self) -> Result<Arc<McpProxyClient>, String> {
        let mut client = self.client.lock().await;
        if let Some(current) = client.as_ref() {
            if !current.is_closed() {
                return Ok(current.clone());
            }
            *client = None;
        }
        let (connected, catalog) =
            McpProxyClient::connect(self.spec.clone(), &self.workspace_id).await?;
        let catalog = sanitize_proxy_catalog(&self.spec, catalog)?;
        let reconnected_digest = proxy_catalog_digest(&catalog.tools)?;
        if reconnected_digest != self.catalog_digest {
            return Err(
                "downstream tool catalog contract changed; restart the MCP listener to renegotiate tools/list"
                    .to_string(),
            );
        }
        *client = Some(connected.clone());
        *self.session_id.lock().expect("mcp proxy session id lock") = Uuid::new_v4().to_string();
        append_profile_log(
            &self.workspace_id,
            "stdout.log",
            &format!("[mcp-proxy:{}] reconnected", self.spec.name),
        );
        *self
            .last_reconnect_at
            .lock()
            .expect("mcp proxy reconnect time lock") = Some(proxy_timestamp());
        Ok(connected)
    }

    async fn handle_management_call(
        &self,
        kind: ProxyRouteKind,
        cancellation: &CancellationToken,
    ) -> Value {
        let browser_server = self.is_browser_server();
        let initial_connection = self.status(self.downstream_tools.len());
        let initial_operational = initial_connection
            .get("operational")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let (mut result, page_state) = match kind {
            ProxyRouteKind::HealthCheck if browser_server => (
                Ok("healthy"),
                self.bounded_management_page_state(cancellation).await,
            ),
            ProxyRouteKind::Reconnect if browser_server => {
                let current = self.status(self.downstream_tools.len());
                let operational = current
                    .get("operational")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                if operational {
                    let page_state = self.bounded_management_page_state(cancellation).await;
                    if proxy_management_failure(&current, &page_state).is_none() {
                        (Ok("already_healthy"), page_state)
                    } else {
                        let result = self.replace_connection().await.map(|_| "reconnected");
                        let page_state = if result.is_ok() {
                            self.bounded_management_page_state(cancellation).await
                        } else {
                            self.last_known_page_state()
                        };
                        (result, page_state)
                    }
                } else {
                    let result = self.replace_connection().await.map(|_| "reconnected");
                    let page_state = if result.is_ok() {
                        self.bounded_management_page_state(cancellation).await
                    } else {
                        self.last_known_page_state()
                    };
                    (result, page_state)
                }
            }
            ProxyRouteKind::ResetSession if browser_server => {
                let result = self.replace_connection().await.map(|_| "session_reset");
                let page_state = if result.is_ok() {
                    self.bounded_management_page_state(cancellation).await
                } else {
                    self.last_known_page_state()
                };
                (result, page_state)
            }
            ProxyRouteKind::HealthCheck => {
                let result = self.probe_connection(cancellation).await.map(|_| "healthy");
                let page_state = if result.is_ok() {
                    self.management_page_state(cancellation).await
                } else {
                    self.last_known_page_state()
                };
                (result, page_state)
            }
            ProxyRouteKind::Reconnect => {
                let result = if self.probe_connection(cancellation).await.is_ok() {
                    Ok("already_healthy")
                } else {
                    self.replace_connection().await.map(|_| "reconnected")
                };
                let page_state = if result.is_ok() {
                    self.management_page_state(cancellation).await
                } else {
                    self.last_known_page_state()
                };
                (result, page_state)
            }
            ProxyRouteKind::ResetSession => {
                let result = self.replace_connection().await.map(|_| "session_reset");
                let page_state = if result.is_ok() {
                    self.management_page_state(cancellation).await
                } else {
                    self.last_known_page_state()
                };
                (result, page_state)
            }
            ProxyRouteKind::Downstream => (Ok("healthy"), self.last_known_page_state()),
        };
        let connection = self.status(self.downstream_tools.len());
        if result.is_ok() {
            if let Some(message) = proxy_management_failure(&connection, &page_state) {
                result = Err(message);
            }
        }
        let connection_status = proxy_connection_status(&connection);
        let transport_connected = connection
            .get("transport_connected")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let operational = connection
            .get("operational")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let recovered_during_probe = !initial_operational && operational;
        let observed_at = connection
            .get("observed_at")
            .cloned()
            .unwrap_or_else(|| Value::String(proxy_timestamp()));
        let browser_cdp_reachable = connection
            .get("browser_cdp_reachable")
            .cloned()
            .unwrap_or(Value::Null);
        let session_id = self
            .session_id
            .lock()
            .expect("mcp proxy session id lock")
            .clone();
        let pages = page_state
            .get("pages")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let selected_page = page_state
            .get("selected_page")
            .cloned()
            .unwrap_or(Value::Null);
        match result {
            Ok(status) => json!({
                "ok": true,
                "status": status,
                "server": self.spec.name,
                "connection": connection,
                "error": null,
                "error_code": null,
                "error_message": null,
                "retryable": false,
                "browser_session_id": session_id,
                "connection_status": connection_status,
                "transport_connected": transport_connected,
                "operational": operational,
                "browser_cdp_reachable": browser_cdp_reachable,
                "observed_at": observed_at,
                "recovered_during_probe": recovered_during_probe,
                "page_count": pages.len(),
                "pages": pages,
                "selected_page": selected_page,
                "page_state": page_state
            }),
            Err(message) => {
                let error_code = browser_proxy_error_code("management", &message);
                json!({
                    "ok": false,
                    "status": "unhealthy",
                    "server": self.spec.name,
                    "connection": connection,
                    "error": {
                        "code": error_code,
                        "message": message,
                        "retryable": true
                    },
                    "error_code": error_code,
                    "error_message": message,
                    "retryable": true,
                    "browser_session_id": session_id,
                    "connection_status": connection_status,
                    "transport_connected": transport_connected,
                    "operational": operational,
                    "browser_cdp_reachable": browser_cdp_reachable,
                    "observed_at": observed_at,
                    "recovered_during_probe": recovered_during_probe,
                    "page_count": pages.len(),
                    "pages": pages,
                    "selected_page": selected_page,
                    "page_state": page_state
                })
            }
        }
    }

    async fn management_page_state(&self, cancellation: &CancellationToken) -> Value {
        if !self.is_browser_server() {
            return self.last_known_page_state();
        }
        let Some(tool_name) = self
            .downstream_tools
            .iter()
            .find(|name| matches!(name.as_str(), "list_pages" | "listPages"))
            .cloned()
        else {
            return self.last_known_page_state();
        };
        let client = match self.ensure_client().await {
            Ok(client) => client,
            Err(message) => {
                return json!({
                    "pages": [],
                    "selected_page": null,
                    "page_count": 0,
                    "error_code": "BROWSER_NOT_CONNECTED",
                    "error_message": message
                })
            }
        };
        match client
            .request_with_cancellation(
                "tools/call",
                json!({"name": tool_name, "arguments": {}}),
                cancellation,
            )
            .await
        {
            Ok(result) => proxy_page_state(&result),
            Err(error) => json!({
                "pages": [],
                "selected_page": null,
                "page_count": 0,
                "error_code": browser_proxy_error_code("list_pages", &error.to_string()),
                "error_message": error.to_string()
            }),
        }
    }

    async fn bounded_management_page_state(&self, cancellation: &CancellationToken) -> Value {
        let limit = self
            .spec
            .request_timeout
            .min(Duration::from_secs(BROWSER_MANAGEMENT_PAGE_TIMEOUT_SECONDS));
        match timeout(limit, self.management_page_state(cancellation)).await {
            Ok(page_state) => page_state,
            Err(_) => json!({
                "pages": [],
                "selected_page": null,
                "page_count": 0,
                "error_code": "BROWSER_MANAGEMENT_TIMEOUT",
                "error_message": format!(
                    "browser page-state request timed out after {} seconds",
                    limit.as_secs()
                )
            }),
        }
    }

    fn is_browser_server(&self) -> bool {
        self.spec
            .tool_prefix
            .to_ascii_lowercase()
            .contains("browser")
            || self.spec.name.to_ascii_lowercase().contains("browser")
    }

    fn last_known_page_state(&self) -> Value {
        let summary = self
            .last_success_summary
            .lock()
            .expect("mcp proxy success summary lock")
            .clone()
            .unwrap_or_else(|| json!({}));
        let pages = summary
            .get("pages")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let selected_page = summary
            .get("selected_page")
            .or_else(|| summary.get("current_page"))
            .cloned()
            .unwrap_or(Value::Null);
        json!({
            "pages": pages,
            "selected_page": selected_page,
            "page_count": pages.len(),
            "source": "last_known_state"
        })
    }

    fn decorate_proxy_result(&self, public_name: &str, result: &mut Value) {
        let Some(structured) = result
            .get_mut("structuredContent")
            .and_then(Value::as_object_mut)
        else {
            return;
        };
        let session_id = self
            .session_id
            .lock()
            .expect("mcp proxy session id lock")
            .clone();
        let connection = self.status(self.downstream_tools.len());
        let connection_status = proxy_connection_status(&connection);
        let transport_connected = connection
            .get("transport_connected")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let operational = connection
            .get("operational")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let browser_cdp_reachable = connection
            .get("browser_cdp_reachable")
            .cloned()
            .unwrap_or(Value::Null);
        structured.insert("browser_session_id".into(), Value::String(session_id));
        structured.insert(
            "connection_status".into(),
            Value::String(connection_status.into()),
        );
        structured.insert(
            "transport_connected".into(),
            Value::Bool(transport_connected),
        );
        structured.insert("operational".into(), Value::Bool(operational));
        structured.insert("browser_cdp_reachable".into(), browser_cdp_reachable);
        let mut page_state = proxy_page_state(&Value::Object(structured.clone()));
        if page_state
            .get("pages")
            .and_then(Value::as_array)
            .is_none_or(Vec::is_empty)
        {
            let last_known = self.last_known_page_state();
            if last_known
                .get("pages")
                .and_then(Value::as_array)
                .is_some_and(|pages| !pages.is_empty())
            {
                if let Some(object) = page_state.as_object_mut() {
                    object.insert(
                        "pages".into(),
                        last_known
                            .get("pages")
                            .cloned()
                            .unwrap_or_else(|| json!([])),
                    );
                    object.insert(
                        "selected_page".into(),
                        last_known
                            .get("selected_page")
                            .cloned()
                            .unwrap_or(Value::Null),
                    );
                    object.insert(
                        "page_count".into(),
                        last_known
                            .get("page_count")
                            .cloned()
                            .unwrap_or_else(|| json!(0)),
                    );
                    object.insert("source".into(), Value::String("last_known_state".into()));
                }
            }
        }
        structured.insert(
            "page_count".into(),
            page_state
                .get("page_count")
                .cloned()
                .unwrap_or_else(|| json!(0)),
        );
        structured.insert(
            "pages".into(),
            page_state
                .get("pages")
                .cloned()
                .unwrap_or_else(|| json!([])),
        );
        structured.insert(
            "selected_page".into(),
            page_state
                .get("selected_page")
                .cloned()
                .unwrap_or(Value::Null),
        );
        let failed = structured.get("ok").and_then(Value::as_bool) == Some(false);
        if failed {
            let error = structured.get("error").and_then(Value::as_object);
            let detail = error
                .and_then(|value| value.get("message"))
                .or_else(|| {
                    error
                        .and_then(|value| value.get("details"))
                        .and_then(Value::as_object)
                        .and_then(|details| details.get("detail"))
                })
                .and_then(Value::as_str)
                .unwrap_or("Proxied browser operation failed")
                .to_string();
            let code = browser_proxy_error_code(public_name, &detail);
            let retryable = error
                .and_then(|value| value.get("retryable"))
                .and_then(Value::as_bool)
                .unwrap_or(matches!(
                    code,
                    "BROWSER_NOT_CONNECTED"
                        | "CDP_CONNECTION_LOST"
                        | "BROWSER_CRASHED"
                        | "PAGE_CLOSED"
                        | "STALE_ELEMENT_REFERENCE"
                        | "ELEMENT_WAIT_TIMEOUT"
                        | "PAGE_LOAD_TIMEOUT"
                        | "SCRIPT_TIMEOUT"
                ));
            let recovery_hint = match code {
                "STALE_ELEMENT_REFERENCE" => {
                    "Call browser__take_snapshot and use a fresh element UID."
                }
                "NO_ACTIVE_PAGE" | "PAGE_CLOSED" => {
                    "Call browser__list_pages, browser__select_page, then browser__take_snapshot."
                }
                "BROWSER_CRASHED" => {
                    "Chrome is being relaunched; call browser__health_check, then open or navigate to the target page again."
                }
                "BROWSER_NOT_CONNECTED" | "CDP_CONNECTION_LOST" => {
                    "Call browser__health_check, reconnect only if disconnected, then list pages again."
                }
                "ELEMENT_WAIT_TIMEOUT" | "PAGE_LOAD_TIMEOUT" | "SCRIPT_TIMEOUT" => {
                    "The browser connection was preserved; inspect the current page with browser__take_snapshot before retrying."
                }
                _ => "Inspect error_message and the current browser page state before retrying.",
            };
            structured.insert("error_code".into(), Value::String(code.into()));
            structured.insert("error_message".into(), Value::String(detail));
            structured.insert("retryable".into(), Value::Bool(retryable));
            structured.insert("recovery_hint".into(), Value::String(recovery_hint.into()));
            structured.insert(
                "page_reacquire_required".into(),
                Value::Bool(matches!(
                    code,
                    "NO_ACTIVE_PAGE" | "PAGE_CLOSED" | "BROWSER_CRASHED"
                )),
            );
            structured.insert(
                "element_reacquire_required".into(),
                Value::Bool(code == "STALE_ELEMENT_REFERENCE"),
            );
            structured.insert(
                "connection_reconnect_required".into(),
                Value::Bool(matches!(
                    code,
                    "BROWSER_NOT_CONNECTED" | "CDP_CONNECTION_LOST"
                )),
            );
        } else {
            structured.insert("error_code".into(), Value::Null);
            structured.insert("error_message".into(), Value::Null);
            structured.insert("retryable".into(), Value::Bool(false));
        }
    }

    async fn probe_connection(&self, cancellation: &CancellationToken) -> Result<(), String> {
        let client = self.ensure_client().await?;
        match client
            .request_with_cancellation("ping", json!({}), cancellation)
            .await
        {
            Ok(_) => {
                self.record_success("ping", None);
                Ok(())
            }
            Err(error) => {
                let code = proxy_failure_reason("ping", &error);
                self.record_failure(&code, &error.to_string());
                if error.invalidates_connection() {
                    self.invalidate_client(&client).await;
                }
                Err(error.to_string())
            }
        }
    }

    async fn replace_connection(&self) -> Result<(), String> {
        self.reconnect_attempts.fetch_add(1, Ordering::Relaxed);
        let previous = {
            let mut client = self.client.lock().await;
            client.take()
        };
        if let Some(previous) = previous {
            if timeout(
                Duration::from_secs(PROXY_TERMINATION_TIMEOUT_SECONDS),
                previous.terminate(),
            )
            .await
            .is_err()
            {
                append_profile_log(
                    &self.workspace_id,
                    "stderr.log",
                    &format!(
                        "[mcp-proxy:{}] timed out terminating the previous downstream process",
                        self.spec.name
                    ),
                );
            }
        }
        timeout(
            Duration::from_secs(PROXY_CONNECTION_REPLACEMENT_TIMEOUT_SECONDS),
            self.ensure_client(),
        )
        .await
        .map_err(|_| {
            format!(
                "timed out replacing downstream MCP connection after {PROXY_CONNECTION_REPLACEMENT_TIMEOUT_SECONDS} seconds"
            )
        })?
        .map(|_| ())
    }

    async fn invalidate_client(&self, failed: &Arc<McpProxyClient>) {
        let mut client = self.client.lock().await;
        let removed = client
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, failed));
        if removed {
            *client = None;
        }
        drop(client);
        if removed {
            failed.terminate().await;
        }
    }

    fn record_queue_cancelled(&self) {
        self.cancellations.fetch_add(1, Ordering::Relaxed);
    }

    fn record_queue_timeout(&self) {
        self.timeouts.fetch_add(1, Ordering::Relaxed);
        self.queue_timeouts.fetch_add(1, Ordering::Relaxed);
        self.record_failure_message("downstream concurrency queue timed out");
    }

    fn record_call_started(&self) {
        self.calls.fetch_add(1, Ordering::Relaxed);
    }

    fn record_client_error(&self, public_name: &str, error: &ProxyClientError) {
        if error.is_cancelled() {
            self.cancellations.fetch_add(1, Ordering::Relaxed);
            return;
        }
        if matches!(error, ProxyClientError::Timeout { .. }) {
            self.timeouts.fetch_add(1, Ordering::Relaxed);
        }
        self.record_failure(
            &proxy_failure_reason(public_name, error),
            &error.to_string(),
        );
    }

    fn record_failure_message(&self, message: &str) {
        self.record_failure("proxy_call_failed", message);
    }

    fn record_failure(&self, code: &str, message: &str) {
        self.failures.fetch_add(1, Ordering::Relaxed);
        *self.last_error.lock().expect("mcp proxy last error lock") =
            Some(truncate_log_detail(message, 1_024));
        *self
            .last_error_code
            .lock()
            .expect("mcp proxy error code lock") = Some(code.to_string());
        *self
            .last_error_at
            .lock()
            .expect("mcp proxy error time lock") = Some(proxy_timestamp());
    }

    fn record_success(&self, tool: &str, result: Option<&Value>) {
        *self.last_error.lock().expect("mcp proxy last error lock") = None;
        *self
            .last_error_code
            .lock()
            .expect("mcp proxy error code lock") = None;
        *self
            .last_error_at
            .lock()
            .expect("mcp proxy error time lock") = None;
        *self
            .last_success_at
            .lock()
            .expect("mcp proxy success time lock") = Some(proxy_timestamp());
        *self
            .last_success_tool
            .lock()
            .expect("mcp proxy success tool lock") = Some(tool.to_string());
        if let Some(result) = result {
            let mut summary = self
                .last_success_summary
                .lock()
                .expect("mcp proxy success summary lock");
            let merged = merge_proxy_success_summary(summary.as_ref(), result);
            *summary = Some(merged);
        }
    }

    fn status(&self, tool_count: usize) -> Value {
        let observed_at = proxy_timestamp();
        let (transport_connected, state_busy, mut client_status) = match self.client.try_lock() {
            Ok(client) => {
                let connected = client.as_ref().is_some_and(|client| !client.is_closed());
                let status = client
                    .as_ref()
                    .map(|client| client.status())
                    .unwrap_or_else(|| json!({"state": "disconnected"}));
                (connected, false, status)
            }
            Err(_) => (true, true, json!({"state": "busy"})),
        };
        let browser_cdp_reachable = proxy_browser_cdp_reachable(&client_status);
        if let Some(object) = client_status.as_object_mut() {
            object.insert(
                "cdp_reachable".into(),
                browser_cdp_reachable
                    .map(Value::Bool)
                    .unwrap_or(Value::Null),
            );
            if browser_cdp_reachable == Some(false) {
                object.insert("state".into(), Value::String("degraded".into()));
            }
        }
        let available_slots = self.concurrency.available_permits();
        let last_success_summary = self
            .last_success_summary
            .lock()
            .expect("mcp proxy success summary lock")
            .clone();
        let current_page = last_success_summary
            .as_ref()
            .and_then(|value| value.get("current_page"))
            .cloned();
        let stored_last_error_code = self
            .last_error_code
            .lock()
            .expect("mcp proxy error code lock")
            .clone();
        let stored_last_error = self
            .last_error
            .lock()
            .expect("mcp proxy last error lock")
            .clone();
        let cdp_error = (browser_cdp_reachable == Some(false)).then(|| {
            let port = client_status
                .pointer("/workspace_isolation/debugging_port")
                .and_then(Value::as_u64)
                .unwrap_or_default();
            format!("browser debugging port {port} is not reachable")
        });
        let last_error_code = stored_last_error_code.or_else(|| {
            cdp_error
                .as_ref()
                .map(|_| "BROWSER_NOT_CONNECTED".to_string())
        });
        let last_error = stored_last_error.or(cdp_error);
        let operational = transport_connected && browser_cdp_reachable != Some(false);
        json!({
            "name": self.spec.name,
            "transport": self.spec.transport.label(),
            "connected": operational,
            "transport_connected": transport_connected,
            "operational": operational,
            "browser_cdp_reachable": browser_cdp_reachable,
            "observed_at": observed_at,
            "freshness": {
                "transport": "current_client_snapshot",
                "browser_cdp": if browser_cdp_reachable.is_some() { "live_tcp_probe" } else { "unavailable" }
            },
            "state_busy": state_busy,
            "client": client_status,
            "reconnect_scheduled": self.reconnect_scheduled.load(Ordering::Acquire),
            "tool_count": tool_count,
            "selected_downstream_tool_count": self.downstream_tools.len(),
            "discovered_tool_count": self.discovered_tool_count,
            "filtered_tool_count": self.filtered_tool_count,
            "truncated_tool_count": self.truncated_tool_count,
            "exposure_mode": self.spec.exposure_mode.label(),
            "selection_source": self.selection_source,
            "max_concurrent_requests": self.spec.max_concurrent_requests,
            "in_flight_requests": self.spec.max_concurrent_requests.saturating_sub(available_slots),
            "available_slots": available_slots,
            "calls": self.calls.load(Ordering::Relaxed),
            "failures": self.failures.load(Ordering::Relaxed),
            "cancellations": self.cancellations.load(Ordering::Relaxed),
            "timeouts": self.timeouts.load(Ordering::Relaxed),
            "queue_timeouts": self.queue_timeouts.load(Ordering::Relaxed),
            "management_tools": self.spec.management_tools,
            "reconnect_attempts": self.reconnect_attempts.load(Ordering::Relaxed),
            "last_reconnect_at": self.last_reconnect_at.lock().expect("mcp proxy reconnect time lock").clone(),
            "last_success_at": self.last_success_at.lock().expect("mcp proxy success time lock").clone(),
            "last_success_tool": self.last_success_tool.lock().expect("mcp proxy success tool lock").clone(),
            "last_success_summary": last_success_summary,
            "current_page": current_page,
            "last_error_code": last_error_code,
            "last_error_at": self.last_error_at.lock().expect("mcp proxy error time lock").clone(),
            "last_error": last_error
        })
    }

    fn schedule_reconnect(self: Arc<Self>) {
        if self.reconnect_scheduled.swap(true, Ordering::AcqRel) {
            return;
        }
        crate::async_runtime::spawn(async move {
            for attempt in 1u8..=5 {
                tokio::time::sleep(proxy_reconnect_delay(attempt)).await;
                if self.ensure_client().await.is_ok() {
                    self.reconnect_scheduled.store(false, Ordering::Release);
                    return;
                }
                append_profile_log(
                    &self.workspace_id,
                    "stderr.log",
                    &format!(
                        "[mcp-proxy:{}] reconnect attempt {attempt}/5 failed",
                        self.spec.name
                    ),
                );
            }
            self.reconnect_scheduled.store(false, Ordering::Release);
        });
    }
}

fn proxy_reconnect_delay(attempt: u8) -> Duration {
    Duration::from_millis(250 * (1u64 << attempt.saturating_sub(1).min(4)))
}

fn proxy_call_error_result(
    server_name: &str,
    public_name: &str,
    reason: &str,
    detail: String,
    retryable: bool,
    reconnect_scheduled: bool,
) -> Value {
    crate::tools::wrap_tool_result(json!({
        "ok": false,
        "status": "error",
        "summary": format!("Proxied MCP tool failed: {server_name} / {public_name}"),
        "error": {
            "code": "PROXIED_TOOL_FAILED",
            "message": format!("Proxied MCP tool failed: {server_name} / {public_name}"),
            "category": "runtime",
            "retryable": retryable,
            "details": {
                "reason": reason,
                "server": server_name,
                "tool": public_name,
                "detail": detail,
                "request_replayed": false,
                "reconnect_scheduled": reconnect_scheduled
            }
        }
    }))
}

fn normalized_proxy_failure(result: &Value) -> Option<(String, String)> {
    let structured = result.get("structuredContent").unwrap_or(&Value::Null);
    let failed = result.get("isError").and_then(Value::as_bool) == Some(true)
        || structured.get("ok").and_then(Value::as_bool) == Some(false);
    if !failed {
        return None;
    }
    let code = structured
        .get("error_code")
        .and_then(Value::as_str)
        .or_else(|| structured.pointer("/error/code").and_then(Value::as_str))
        .unwrap_or("PROXIED_TOOL_FAILED")
        .to_string();
    let message = structured
        .get("error_message")
        .and_then(Value::as_str)
        .or_else(|| structured.pointer("/error/message").and_then(Value::as_str))
        .or_else(|| {
            result
                .get("content")
                .and_then(Value::as_array)
                .and_then(|content| {
                    content.iter().find_map(|item| {
                        (item.get("type").and_then(Value::as_str) == Some("text"))
                            .then(|| item.get("text").and_then(Value::as_str))
                            .flatten()
                    })
                })
        })
        .unwrap_or("Downstream MCP tool returned an error")
        .to_string();
    Some((code, message))
}

fn proxy_browser_cdp_reachable(client_status: &Value) -> Option<bool> {
    let port = client_status
        .pointer("/workspace_isolation/debugging_port")
        .and_then(Value::as_u64)
        .and_then(|port| u16::try_from(port).ok())
        .filter(|port| *port != 0)?;
    Some(browser_cdp_port_reachable(port))
}

fn proxy_connection_status(connection: &Value) -> &'static str {
    let fallback_connected = connection
        .get("connected")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let transport_connected = connection
        .get("transport_connected")
        .and_then(Value::as_bool)
        .unwrap_or(fallback_connected);
    let operational = connection
        .get("operational")
        .and_then(Value::as_bool)
        .unwrap_or(fallback_connected);
    if operational {
        "connected"
    } else if transport_connected {
        "degraded"
    } else {
        "disconnected"
    }
}

fn normalize_proxy_tool_result(
    server_name: &str,
    public_name: &str,
    result: Value,
    output_schema: &Value,
    synthesized_output_schema: bool,
) -> Result<Value, String> {
    let payload = proxy_result_payload(&result);
    let Some(object) = result.as_object() else {
        return Err("downstream tools/call result is not an object".into());
    };
    let mut content = object
        .get("content")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| "downstream tools/call result is missing content array".to_string())?;
    if !content.iter().all(|item| {
        item.as_object()
            .and_then(|item| item.get("type"))
            .and_then(Value::as_str)
            .is_some()
    }) {
        return Err("downstream tools/call content contains an invalid item".into());
    }
    let is_error = object
        .get("isError")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mut structured = object.get("structuredContent").cloned();
    if structured.as_ref().is_some_and(|value| !value.is_object()) {
        return Err("downstream structuredContent must be an object".into());
    }
    if synthesized_output_schema {
        let payload = structured.unwrap_or(payload);
        structured = Some(if is_error {
            let message = payload
                .get("text")
                .and_then(Value::as_str)
                .filter(|text| !text.trim().is_empty())
                .unwrap_or("Downstream MCP tool returned an error")
                .to_string();
            let code = browser_proxy_error_code(public_name, &message);
            json!({
                "ok": false,
                "result": payload,
                "error": {
                    "code": code,
                    "message": message,
                    "category": "runtime",
                    "retryable": matches!(
                        code,
                        "BROWSER_NOT_CONNECTED"
                            | "CDP_CONNECTION_LOST"
                            | "BROWSER_CRASHED"
                            | "PAGE_CLOSED"
                    ),
                    "details": {
                        "server": server_name,
                        "tool": public_name,
                        "downstream_is_error": true
                    }
                },
                "error_code": code,
                "error_message": message,
                "retryable": matches!(
                    code,
                    "BROWSER_NOT_CONNECTED"
                        | "CDP_CONNECTION_LOST"
                        | "BROWSER_CRASHED"
                        | "PAGE_CLOSED"
                )
            })
        } else {
            json!({
                "ok": true,
                "result": payload,
                "error": null,
                "error_code": null,
                "error_message": null,
                "retryable": false
            })
        });
    } else {
        let structured = structured.as_ref().ok_or_else(|| {
            "downstream tool declared outputSchema but omitted structuredContent".to_string()
        })?;
        jsonschema::validator_for(output_schema)
            .map_err(|error| format!("failed to compile downstream outputSchema: {error}"))?
            .validate(structured)
            .map_err(|error| {
                format!("downstream structuredContent violates outputSchema: {error}")
            })?;
    }
    let structured = structured.expect("proxy output normalization always produces an object");
    jsonschema::validator_for(output_schema)
        .map_err(|error| format!("failed to compile normalized outputSchema: {error}"))?
        .validate(&structured)
        .map_err(|error| format!("normalized structuredContent violates outputSchema: {error}"))?;
    let has_text = content
        .iter()
        .any(|item| item.get("type") == Some(&json!("text")));
    if !has_text {
        content.push(json!({"type": "text", "text": structured.to_string()}));
    }
    let mut normalized = json!({
        "content": content,
        "isError": is_error,
        "_meta": {
            "proxyServer": server_name,
            "proxyTool": public_name
        }
    });
    normalized["structuredContent"] = structured;
    Ok(normalized)
}

impl McpProxyClient {
    async fn connect(
        spec: McpProxyServerSpec,
        workspace_id: &str,
    ) -> Result<(Arc<Self>, Vec<Value>), String> {
        match spec.transport.clone() {
            McpProxyTransportSpec::Stdio => {
                let (client, tools) = StdioMcpProxyClient::connect(spec, workspace_id).await?;
                Ok((
                    Arc::new(Self {
                        transport: ProxyClientTransport::Stdio(client),
                    }),
                    tools,
                ))
            }
            McpProxyTransportSpec::StreamableHttp { url, headers } => {
                let (client, tools) =
                    HttpMcpProxyClient::connect(&spec, workspace_id, &url, &headers).await?;
                Ok((
                    Arc::new(Self {
                        transport: ProxyClientTransport::StreamableHttp(client),
                    }),
                    tools,
                ))
            }
        }
    }

    fn is_closed(&self) -> bool {
        match &self.transport {
            ProxyClientTransport::Stdio(client) => client.is_closed(),
            ProxyClientTransport::StreamableHttp(client) => client.is_closed(),
        }
    }

    fn status(&self) -> Value {
        match &self.transport {
            ProxyClientTransport::Stdio(client) => {
                let (process_id, process_state) = match client.child.try_lock() {
                    Ok(child) => (child.as_ref().and_then(|child| child.id()), "available"),
                    Err(_) => (None, "busy"),
                };
                json!({
                    "transport": "stdio",
                    "state": if client.is_closed() { "closed" } else { "connected" },
                    "process_id": process_id,
                    "process_state": process_state,
                    "cdp_connection": "downstream_managed",
                    "workspace_isolation": client.workspace_isolation.as_ref().map(|isolation| json!({
                        "mode": "process_and_profile",
                        "home": display_proxy_path(&isolation.home),
                        "debugging_port": isolation.debugging_port
                    })).unwrap_or(Value::Null)
                })
            }
            ProxyClientTransport::StreamableHttp(client) => json!({
                "transport": "streamable-http",
                "state": if client.is_closed() { "closed" } else { "connected" },
                "endpoint_origin": proxy_endpoint_origin(&client.endpoint),
                "mcp_session_id": client.session_id.lock().expect("mcp HTTP session lock").clone(),
                "protocol_version": client.protocol_version.lock().expect("mcp HTTP protocol lock").clone(),
                "cdp_connection": "downstream_managed"
            }),
        }
    }

    async fn terminate(&self) {
        match &self.transport {
            ProxyClientTransport::Stdio(client) => client.terminate().await,
            ProxyClientTransport::StreamableHttp(client) => client.terminate().await,
        }
    }

    async fn request_with_cancellation(
        &self,
        method: &str,
        params: Value,
        cancellation: &CancellationToken,
    ) -> Result<Value, ProxyClientError> {
        match &self.transport {
            ProxyClientTransport::Stdio(client) => {
                client
                    .request_with_cancellation(method, params, cancellation)
                    .await
            }
            ProxyClientTransport::StreamableHttp(client) => {
                client
                    .request_with_cancellation(method, params, cancellation)
                    .await
            }
        }
    }
}

impl HttpMcpProxyClient {
    async fn connect(
        spec: &McpProxyServerSpec,
        workspace_id: &str,
        url: &str,
        headers: &BTreeMap<String, String>,
    ) -> Result<(Arc<Self>, Vec<Value>), String> {
        let endpoint = reqwest::Url::parse(url)
            .map_err(|error| format!("invalid Streamable HTTP endpoint: {error}"))?;
        let mut configured_headers = HeaderMap::new();
        for (name, value) in headers {
            configured_headers.insert(
                HeaderName::from_bytes(name.as_bytes())
                    .map_err(|error| format!("invalid downstream header `{name}`: {error}"))?,
                HeaderValue::from_str(value)
                    .map_err(|error| format!("invalid downstream header `{name}`: {error}"))?,
            );
        }
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(spec.request_timeout.min(Duration::from_secs(30)))
            .build()
            .map_err(|error| format!("failed to build Streamable HTTP client: {error}"))?;
        let client = Arc::new(Self {
            request_timeout: spec.request_timeout,
            endpoint,
            client,
            configured_headers,
            session_id: StdMutex::new(None),
            protocol_version: StdMutex::new(
                crate::mcp::protocol::CURRENT_PROTOCOL_VERSION.to_string(),
            ),
            next_id: AtomicU64::new(1),
            closed: AtomicBool::new(false),
            workspace_id: workspace_id.to_string(),
            server_name: spec.name.clone(),
        });

        let initialized = client
            .request(
                "initialize",
                json!({
                    "protocolVersion": crate::mcp::protocol::CURRENT_PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": {
                        "name": "anchor-proxy",
                        "version": env!("CARGO_PKG_VERSION")
                    }
                }),
            )
            .await
            .map_err(|error| error.to_string())?;
        client.update_protocol_version(&initialized)?;
        client
            .notify("notifications/initialized", json!({}))
            .await
            .map_err(|error| error.to_string())?;
        let tools = client
            .list_tools()
            .await
            .map_err(|error| error.to_string())?;
        Ok((client, tools))
    }

    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    fn update_protocol_version(&self, initialized: &Value) -> Result<(), String> {
        let version = initialized
            .get("protocolVersion")
            .and_then(Value::as_str)
            .filter(|version| !version.is_empty())
            .ok_or_else(|| "downstream initialize result is missing protocolVersion".to_string())?;
        *self
            .protocol_version
            .lock()
            .expect("mcp HTTP protocol version lock") = version.to_string();
        Ok(())
    }

    async fn terminate(&self) {
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        let session_id = self
            .session_id
            .lock()
            .expect("mcp HTTP session lock")
            .clone();
        let Some(session_id) = session_id else {
            return;
        };
        let Ok(headers) = self.request_headers(Some(&session_id), true) else {
            return;
        };
        let _ = timeout(
            Duration::from_secs(5),
            self.client
                .delete(self.endpoint.clone())
                .headers(headers)
                .send(),
        )
        .await;
    }

    async fn list_tools(&self) -> Result<Vec<Value>, ProxyClientError> {
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_TOOL_LIST_PAGES {
            let params = cursor
                .as_ref()
                .map(|value| json!({ "cursor": value }))
                .unwrap_or_else(|| json!({}));
            let result = self.request("tools/list", params).await?;
            if let Some(page) = result.get("tools").and_then(Value::as_array) {
                tools.extend(page.iter().cloned());
            }
            cursor = result
                .get("nextCursor")
                .and_then(Value::as_str)
                .map(ToString::to_string)
                .filter(|value| !value.is_empty());
            if cursor.is_none() {
                return Ok(tools);
            }
        }
        Err(ProxyClientError::Protocol(
            "downstream MCP returned too many tools/list pages".into(),
        ))
    }

    async fn request(&self, method: &str, params: Value) -> Result<Value, ProxyClientError> {
        self.request_with_cancellation(method, params, &CancellationToken::default())
            .await
    }

    async fn request_with_cancellation(
        &self,
        method: &str,
        params: Value,
        cancellation: &CancellationToken,
    ) -> Result<Value, ProxyClientError> {
        if self.is_closed() {
            return Err(ProxyClientError::Transport(
                "downstream Streamable HTTP session is closed".into(),
            ));
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let request = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        });
        let response = timeout(
            self.request_timeout,
            self.post_message(&request, Some(id), method != "initialize"),
        );
        tokio::pin!(response);
        tokio::select! {
            _ = cancellation.cancelled() => {
                self.send_cancel_notification(id, "cancelled by upstream request").await;
                Err(ProxyClientError::Cancelled)
            }
            response = &mut response => match response {
                Ok(result) => result?.ok_or_else(|| {
                    ProxyClientError::Protocol(
                        "downstream HTTP request completed without a JSON-RPC result".into(),
                    )
                }),
                Err(_) => {
                    self.send_cancel_notification(id, "downstream request timed out").await;
                    Err(ProxyClientError::Timeout {
                        method: method.to_string(),
                        seconds: self.request_timeout.as_secs(),
                    })
                }
            }
        }
    }

    async fn notify(&self, method: &str, params: Value) -> Result<(), ProxyClientError> {
        let notification = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params
        });
        timeout(
            self.request_timeout,
            self.post_message(&notification, None, true),
        )
        .await
        .map_err(|_| ProxyClientError::Timeout {
            method: method.to_string(),
            seconds: self.request_timeout.as_secs(),
        })??;
        Ok(())
    }

    async fn send_cancel_notification(&self, id: u64, reason: &str) {
        if self.is_closed() {
            return;
        }
        let notification = json!({
            "jsonrpc": "2.0",
            "method": "notifications/cancelled",
            "params": {
                "requestId": id,
                "reason": reason
            }
        });
        let _ = timeout(
            Duration::from_secs(5),
            self.post_message(&notification, None, true),
        )
        .await;
    }

    async fn post_message(
        &self,
        message: &Value,
        expected_id: Option<u64>,
        include_session: bool,
    ) -> Result<Option<Value>, ProxyClientError> {
        let session_id = include_session
            .then(|| {
                self.session_id
                    .lock()
                    .expect("mcp HTTP session lock")
                    .clone()
            })
            .flatten();
        let headers = self.request_headers(session_id.as_deref(), include_session)?;
        let response = self
            .client
            .post(self.endpoint.clone())
            .headers(headers)
            .json(message)
            .send()
            .await
            .map_err(|error| {
                ProxyClientError::Transport(format!(
                    "downstream Streamable HTTP request failed: {error}"
                ))
            })?;
        self.capture_session_header(response.headers())?;
        let status = response.status();
        if status == reqwest::StatusCode::ACCEPTED {
            return if expected_id.is_none() {
                Ok(None)
            } else {
                Err(ProxyClientError::Protocol(
                    "downstream returned HTTP 202 for a JSON-RPC request".into(),
                ))
            };
        }
        if !status.is_success() {
            let body = collect_http_body(response).await?;
            let detail = String::from_utf8_lossy(&body);
            return Err(ProxyClientError::Remote(format!(
                "downstream Streamable HTTP returned {status}: {}",
                truncate_log_detail(&detail, 4_096)
            )));
        }
        let Some(expected_id) = expected_id else {
            return Ok(None);
        };
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if content_type.starts_with("text/event-stream") {
            return self.read_sse_result(response, expected_id).await.map(Some);
        }
        let body = collect_http_body(response).await?;
        let message = serde_json::from_slice::<Value>(&body).map_err(|error| {
            ProxyClientError::Protocol(format!(
                "downstream HTTP response is not valid JSON: {error}"
            ))
        })?;
        match_http_rpc_message(message, expected_id)?
            .ok_or_else(|| {
                ProxyClientError::Protocol(
                    "downstream HTTP response did not contain the expected request id".into(),
                )
            })
            .map(Some)
    }

    fn request_headers(
        &self,
        session_id: Option<&str>,
        include_protocol: bool,
    ) -> Result<HeaderMap, ProxyClientError> {
        let mut headers = self.configured_headers.clone();
        headers.insert(
            ACCEPT,
            HeaderValue::from_static("application/json, text/event-stream"),
        );
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        if include_protocol {
            let protocol = self
                .protocol_version
                .lock()
                .expect("mcp HTTP protocol version lock")
                .clone();
            headers.insert(
                HeaderName::from_static(MCP_PROTOCOL_VERSION_HEADER),
                HeaderValue::from_str(&protocol).map_err(|error| {
                    ProxyClientError::Protocol(format!(
                        "invalid negotiated MCP protocol version header: {error}"
                    ))
                })?,
            );
        }
        if let Some(session_id) = session_id {
            headers.insert(
                HeaderName::from_static(MCP_SESSION_ID_HEADER),
                HeaderValue::from_str(session_id).map_err(|error| {
                    ProxyClientError::Protocol(format!(
                        "invalid downstream MCP session id: {error}"
                    ))
                })?,
            );
        }
        Ok(headers)
    }

    fn capture_session_header(&self, headers: &HeaderMap) -> Result<(), ProxyClientError> {
        let Some(session_id) = headers
            .get(MCP_SESSION_ID_HEADER)
            .and_then(|value| value.to_str().ok())
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            return Ok(());
        };
        if session_id.len() > 1_024 {
            return Err(ProxyClientError::Protocol(
                "downstream MCP session id is too long".into(),
            ));
        }
        let mut current = self.session_id.lock().expect("mcp HTTP session lock");
        if current
            .as_ref()
            .is_some_and(|current| current != session_id)
        {
            return Err(ProxyClientError::Protocol(
                "downstream MCP changed session id unexpectedly".into(),
            ));
        }
        if current.is_none() {
            *current = Some(session_id.to_string());
            append_profile_log(
                &self.workspace_id,
                "stdout.log",
                &format!(
                    "[mcp-proxy:{}] Streamable HTTP session established",
                    self.server_name
                ),
            );
        }
        Ok(())
    }

    async fn read_sse_result(
        &self,
        response: reqwest::Response,
        expected_id: u64,
    ) -> Result<Value, ProxyClientError> {
        let mut stream = response.bytes_stream();
        let mut buffer = Vec::new();
        let mut received = 0usize;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|error| {
                ProxyClientError::Transport(format!(
                    "failed to read downstream SSE response: {error}"
                ))
            })?;
            received = received.saturating_add(chunk.len());
            if received > MAX_HTTP_RESPONSE_BYTES {
                return Err(ProxyClientError::Protocol(format!(
                    "downstream SSE response exceeds {MAX_HTTP_RESPONSE_BYTES} bytes"
                )));
            }
            buffer.extend_from_slice(&chunk);
            while let Some(event) = take_sse_event(&mut buffer) {
                let Some(message) = parse_sse_message(&event)? else {
                    continue;
                };
                if let Some(result) = match_http_rpc_message(message, expected_id)? {
                    return Ok(result);
                }
            }
        }
        if !buffer.is_empty() {
            if let Some(message) = parse_sse_message(&buffer)? {
                if let Some(result) = match_http_rpc_message(message, expected_id)? {
                    return Ok(result);
                }
            }
        }
        Err(ProxyClientError::Protocol(
            "downstream SSE stream ended before the expected response".into(),
        ))
    }
}

impl Drop for HttpMcpProxyClient {
    fn drop(&mut self) {
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        let session_id = self
            .session_id
            .lock()
            .expect("mcp HTTP session lock")
            .clone();
        let Some(session_id) = session_id else {
            return;
        };
        let Ok(headers) = self.request_headers(Some(&session_id), true) else {
            return;
        };
        let client = self.client.clone();
        let endpoint = self.endpoint.clone();
        crate::async_runtime::spawn(async move {
            let _ = timeout(
                Duration::from_secs(5),
                client.delete(endpoint).headers(headers).send(),
            )
            .await;
        });
    }
}

async fn collect_http_body(response: reqwest::Response) -> Result<Vec<u8>, ProxyClientError> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_HTTP_RESPONSE_BYTES as u64)
    {
        return Err(ProxyClientError::Protocol(format!(
            "downstream HTTP response exceeds {MAX_HTTP_RESPONSE_BYTES} bytes"
        )));
    }
    let mut stream = response.bytes_stream();
    let mut body = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| {
            ProxyClientError::Transport(format!("failed to read downstream HTTP response: {error}"))
        })?;
        if body.len().saturating_add(chunk.len()) > MAX_HTTP_RESPONSE_BYTES {
            return Err(ProxyClientError::Protocol(format!(
                "downstream HTTP response exceeds {MAX_HTTP_RESPONSE_BYTES} bytes"
            )));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn take_sse_event(buffer: &mut Vec<u8>) -> Option<Vec<u8>> {
    let lf = buffer.windows(2).position(|window| window == b"\n\n");
    let crlf = buffer.windows(4).position(|window| window == b"\r\n\r\n");
    let (index, delimiter) = match (lf, crlf) {
        (Some(left), Some(right)) if left <= right => (left, 2),
        (Some(_), Some(right)) => (right, 4),
        (Some(index), None) => (index, 2),
        (None, Some(index)) => (index, 4),
        (None, None) => return None,
    };
    let event = buffer[..index].to_vec();
    buffer.drain(..index + delimiter);
    Some(event)
}

fn parse_sse_message(event: &[u8]) -> Result<Option<Value>, ProxyClientError> {
    let text = std::str::from_utf8(event).map_err(|error| {
        ProxyClientError::Protocol(format!("downstream SSE event is not UTF-8: {error}"))
    })?;
    let data = text
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .map(|line| line.strip_prefix(' ').unwrap_or(line))
        .collect::<Vec<_>>()
        .join("\n");
    if data.is_empty() || data == "[DONE]" {
        return Ok(None);
    }
    serde_json::from_str(&data).map(Some).map_err(|error| {
        ProxyClientError::Protocol(format!("downstream SSE data is not valid JSON: {error}"))
    })
}

fn match_http_rpc_message(
    message: Value,
    expected_id: u64,
) -> Result<Option<Value>, ProxyClientError> {
    if message.get("method").and_then(Value::as_str).is_some() {
        if message.get("id").is_some() {
            return Err(ProxyClientError::Protocol(
                "downstream HTTP server requests are not supported by this proxy".into(),
            ));
        }
        return Ok(None);
    }
    if message.get("id").and_then(Value::as_u64) != Some(expected_id) {
        return Ok(None);
    }
    if let Some(error) = message.get("error") {
        return Err(ProxyClientError::Remote(error.to_string()));
    }
    message
        .get("result")
        .cloned()
        .map(Some)
        .ok_or_else(|| ProxyClientError::Protocol("downstream response is missing result".into()))
}

fn truncate_log_detail(value: &str, maximum: usize) -> String {
    if value.len() <= maximum {
        return value.to_string();
    }
    let mut end = maximum;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &value[..end])
}

impl StdioMcpProxyClient {
    async fn connect(
        mut spec: McpProxyServerSpec,
        workspace_id: &str,
    ) -> Result<(Arc<Self>, Vec<Value>), String> {
        let workspace_isolation = prepare_my_agent_browser_isolation(&mut spec, workspace_id)?;
        let mut command = Command::new(&spec.command);
        crate::platform::hide_tokio_console(&mut command);
        command
            .args(&spec.args)
            .envs(&spec.env)
            .current_dir(&spec.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        let mut child = command.spawn().map_err(|error| {
            format!(
                "failed to start `{}` in `{}`: {error}",
                spec.command,
                spec.cwd.display()
            )
        })?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "downstream MCP stdin is unavailable".to_string())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "downstream MCP stdout is unavailable".to_string())?;

        if let Some(stderr) = child.stderr.take() {
            let server_name = spec.name.clone();
            let workspace_id = workspace_id.to_string();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    append_profile_log(
                        &workspace_id,
                        "stderr.log",
                        &format!("[mcp-proxy:{server_name}] {line}"),
                    );
                }
            });
        }

        let client = Arc::new(Self {
            request_timeout: spec.request_timeout,
            writer: Mutex::new(stdin),
            child: Mutex::new(Some(child)),
            pending: StdMutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            closed: AtomicBool::new(false),
            close_reason: StdMutex::new(None),
            workspace_id: workspace_id.to_string(),
            server_name: spec.name.clone(),
            workspace_isolation,
        });
        Self::spawn_reader(&client, stdout);

        client
            .request(
                "initialize",
                json!({
                    "protocolVersion": crate::mcp::protocol::CURRENT_PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": {
                        "name": "anchor-proxy",
                        "version": env!("CARGO_PKG_VERSION")
                    }
                }),
            )
            .await
            .map_err(|error| error.to_string())?;
        client
            .notify("notifications/initialized", json!({}))
            .await
            .map_err(|error| error.to_string())?;
        let tools = client
            .list_tools()
            .await
            .map_err(|error| error.to_string())?;
        Ok((client, tools))
    }

    fn spawn_reader(client: &Arc<Self>, stdout: ChildStdout) {
        let client = Arc::downgrade(client);
        tokio::spawn(async move {
            let mut stdout = BufReader::new(stdout);
            loop {
                let mut line = String::new();
                let bytes = match stdout.read_line(&mut line).await {
                    Ok(bytes) => bytes,
                    Err(error) => {
                        if let Some(client) = client.upgrade() {
                            client.mark_closed(format!(
                                "failed to read downstream response: {error}"
                            ));
                        }
                        return;
                    }
                };
                if bytes == 0 {
                    if let Some(client) = client.upgrade() {
                        client.mark_closed("downstream MCP closed stdout".into());
                    }
                    return;
                }
                let Ok(message) = serde_json::from_str::<Value>(line.trim()) else {
                    continue;
                };
                let Some(client) = client.upgrade() else {
                    return;
                };
                if let Err(error) = client.handle_incoming_message(message).await {
                    client.mark_closed(error.to_string());
                    return;
                }
            }
        });
    }

    async fn handle_incoming_message(&self, message: Value) -> Result<(), ProxyClientError> {
        if let Some(method) = message.get("method").and_then(Value::as_str) {
            if let Some(server_request_id) = message.get("id").cloned() {
                self.send_message(&json!({
                    "jsonrpc": "2.0",
                    "id": server_request_id,
                    "error": {
                        "code": -32601,
                        "message": "Downstream server requests are not supported by this proxy"
                    }
                }))
                .await?;
            } else if method == "notifications/tools/list_changed" {
                append_profile_log(
                    &self.workspace_id,
                    "stderr.log",
                    &format!(
                        "[mcp-proxy:{}] downstream tools/list changed; restart the MCP listener to renegotiate the public catalog",
                        self.server_name
                    ),
                );
            }
            return Ok(());
        }

        let Some(id) = message.get("id").and_then(Value::as_u64) else {
            return Ok(());
        };
        let sender = self
            .pending
            .lock()
            .expect("mcp proxy pending request lock")
            .remove(&id);
        let Some(sender) = sender else {
            return Ok(());
        };
        let result = if let Some(error) = message.get("error") {
            Err(ProxyClientError::Remote(error.to_string()))
        } else {
            message.get("result").cloned().ok_or_else(|| {
                ProxyClientError::Protocol("downstream response is missing result".to_string())
            })
        };
        let _ = sender.send(result);
        Ok(())
    }

    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    fn closed_error(&self) -> ProxyClientError {
        ProxyClientError::Transport(
            self.close_reason
                .lock()
                .expect("mcp proxy close reason lock")
                .clone()
                .unwrap_or_else(|| "downstream MCP connection is closed".into()),
        )
    }

    fn mark_closed(&self, reason: String) {
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        *self
            .close_reason
            .lock()
            .expect("mcp proxy close reason lock") = Some(reason.clone());
        let pending =
            std::mem::take(&mut *self.pending.lock().expect("mcp proxy pending request lock"));
        for (_, sender) in pending {
            let _ = sender.send(Err(ProxyClientError::Transport(reason.clone())));
        }
    }

    async fn terminate(&self) {
        self.mark_closed("downstream MCP connection terminated".into());
        if let Some(mut child) = self.child.lock().await.take() {
            #[cfg(target_os = "windows")]
            if let Some(pid) = child.id() {
                let _ = tokio::task::spawn_blocking(move || {
                    crate::platform::platform().terminate_process_tree(pid)
                })
                .await;
            }
            let _ = child.start_kill();
            let _ = timeout(
                Duration::from_secs(PROXY_TERMINATION_TIMEOUT_SECONDS),
                child.wait(),
            )
            .await;
        }
    }

    fn remove_pending(&self, id: u64) {
        self.pending
            .lock()
            .expect("mcp proxy pending request lock")
            .remove(&id);
    }

    async fn send_message(&self, message: &Value) -> Result<(), ProxyClientError> {
        if self.is_closed() {
            return Err(self.closed_error());
        }
        let encoded = serde_json::to_vec(message).map_err(|error| {
            ProxyClientError::Protocol(format!("failed to encode downstream message: {error}"))
        })?;
        let result = async {
            let mut writer = self.writer.lock().await;
            writer.write_all(&encoded).await?;
            writer.write_all(b"\n").await?;
            writer.flush().await
        }
        .await;
        if let Err(error) = result {
            let message = format!("failed to write downstream message: {error}");
            self.mark_closed(message.clone());
            return Err(ProxyClientError::Transport(message));
        }
        Ok(())
    }

    async fn send_cancel_notification(&self, id: u64, reason: &str) {
        if self.is_closed() {
            return;
        }
        let _ = self
            .send_message(&json!({
                "jsonrpc": "2.0",
                "method": "notifications/cancelled",
                "params": {
                    "requestId": id,
                    "reason": reason
                }
            }))
            .await;
    }

    async fn list_tools(&self) -> Result<Vec<Value>, ProxyClientError> {
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;

        for _ in 0..MAX_TOOL_LIST_PAGES {
            let params = cursor
                .as_ref()
                .map(|value| json!({ "cursor": value }))
                .unwrap_or_else(|| json!({}));
            let result = self.request("tools/list", params).await?;
            if let Some(page) = result.get("tools").and_then(Value::as_array) {
                tools.extend(page.iter().cloned());
            }
            cursor = result
                .get("nextCursor")
                .and_then(Value::as_str)
                .map(ToString::to_string)
                .filter(|value| !value.is_empty());
            if cursor.is_none() {
                return Ok(tools);
            }
        }

        Err(ProxyClientError::Protocol(
            "downstream MCP returned too many tools/list pages".to_string(),
        ))
    }

    async fn request(&self, method: &str, params: Value) -> Result<Value, ProxyClientError> {
        self.request_with_cancellation(method, params, &CancellationToken::default())
            .await
    }

    async fn request_with_cancellation(
        &self,
        method: &str,
        params: Value,
        cancellation: &CancellationToken,
    ) -> Result<Value, ProxyClientError> {
        if self.is_closed() {
            return Err(self.closed_error());
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = oneshot::channel();
        self.pending
            .lock()
            .expect("mcp proxy pending request lock")
            .insert(id, sender);
        if let Err(error) = self
            .send_message(&json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": method,
                "params": params
            }))
            .await
        {
            self.remove_pending(id);
            return Err(error);
        }

        let response = timeout(self.request_timeout, receiver);
        tokio::pin!(response);
        tokio::select! {
            _ = cancellation.cancelled() => {
                self.remove_pending(id);
                self.send_cancel_notification(id, "cancelled by upstream request").await;
                Err(ProxyClientError::Cancelled)
            }
            response = &mut response => match response {
                Ok(Ok(result)) => result,
                Ok(Err(_)) => Err(self.closed_error()),
                Err(_) => {
                    self.remove_pending(id);
                    self.send_cancel_notification(id, "downstream request timed out").await;
                    Err(ProxyClientError::Timeout {
                        method: method.to_string(),
                        seconds: self.request_timeout.as_secs(),
                    })
                }
            }
        }
    }

    async fn notify(&self, method: &str, params: Value) -> Result<(), ProxyClientError> {
        timeout(
            self.request_timeout,
            self.send_message(&json!({
                "jsonrpc": "2.0",
                "method": method,
                "params": params
            })),
        )
        .await
        .map_err(|_| ProxyClientError::Timeout {
            method: method.to_string(),
            seconds: self.request_timeout.as_secs(),
        })?
    }
}

pub fn parse_mcp_proxy_config(
    raw: &str,
    workspace_path: &Path,
) -> Result<Vec<McpProxyServerSpec>, String> {
    if raw.trim().is_empty() {
        return Ok(Vec::new());
    }

    let value: Value =
        serde_json::from_str(raw).map_err(|error| format!("MCP 聚合配置不是有效 JSON: {error}"))?;
    let servers_value = value
        .get("mcpServers")
        .cloned()
        .unwrap_or_else(|| value.clone());
    let servers: BTreeMap<String, RawMcpServerConfig> = serde_json::from_value(servers_value)
        .map_err(|error| format!("MCP 聚合配置的 mcpServers 无效: {error}"))?;

    let mut specs = Vec::new();
    for (name, config) in servers {
        if config.disabled {
            continue;
        }
        let workspace_display = workspace_path.display().to_string();
        let transport_name = config.transport_type.trim().to_ascii_lowercase();
        let has_url = config
            .url
            .as_deref()
            .is_some_and(|url| !url.trim().is_empty());
        let transport = match transport_name.as_str() {
            "" if has_url => {
                let url = config.url.as_deref().unwrap_or_default();
                McpProxyTransportSpec::StreamableHttp {
                    url: validate_proxy_http_url(
                        &name,
                        &expand_proxy_placeholders(url, &workspace_display)?,
                    )?,
                    headers: validate_proxy_http_headers(
                        &name,
                        config
                            .headers
                            .iter()
                            .map(|(key, value)| {
                                Ok((
                                    key.clone(),
                                    expand_proxy_placeholders(value, &workspace_display)?,
                                ))
                            })
                            .collect::<Result<BTreeMap<_, _>, String>>()?,
                    )?,
                }
            }
            "" | "stdio" => {
                if config.command.trim().is_empty() {
                    return Err(format!("MCP server `{name}` is missing command"));
                }
                if has_url {
                    return Err(format!(
                        "MCP server `{name}` cannot configure both stdio command and url"
                    ));
                }
                McpProxyTransportSpec::Stdio
            }
            "streamable-http" | "http" => {
                if !config.command.trim().is_empty() {
                    return Err(format!(
                        "MCP server `{name}` cannot configure both Streamable HTTP url and command"
                    ));
                }
                let url = config
                    .url
                    .as_deref()
                    .filter(|url| !url.trim().is_empty())
                    .ok_or_else(|| format!("MCP server `{name}` is missing url"))?;
                McpProxyTransportSpec::StreamableHttp {
                    url: validate_proxy_http_url(
                        &name,
                        &expand_proxy_placeholders(url, &workspace_display)?,
                    )?,
                    headers: validate_proxy_http_headers(
                        &name,
                        config
                            .headers
                            .iter()
                            .map(|(key, value)| {
                                Ok((
                                    key.clone(),
                                    expand_proxy_placeholders(value, &workspace_display)?,
                                ))
                            })
                            .collect::<Result<BTreeMap<_, _>, String>>()?,
                    )?,
                }
            }
            "sse" => {
                return Err(format!(
                    "MCP server `{name}` uses legacy SSE transport; configure a Streamable HTTP endpoint instead"
                ));
            }
            other => {
                return Err(format!(
                    "MCP server `{name}` uses unsupported transport `{other}`"
                ));
            }
        };

        let command = expand_proxy_placeholders(&config.command, &workspace_display)?;
        let args = config
            .args
            .into_iter()
            .map(|arg| expand_proxy_placeholders(&arg, &workspace_display))
            .collect::<Result<Vec<_>, _>>()?;
        let env = config
            .env
            .into_iter()
            .map(|(key, value)| Ok((key, expand_proxy_placeholders(&value, &workspace_display)?)))
            .collect::<Result<BTreeMap<_, _>, String>>()?;
        let cwd = config
            .cwd
            .map(|cwd| expand_proxy_placeholders(&cwd, &workspace_display))
            .transpose()?
            .map(PathBuf::from)
            .map(|cwd| {
                if cwd.is_absolute() {
                    cwd
                } else {
                    workspace_path.join(cwd)
                }
            })
            .unwrap_or_else(|| workspace_path.to_path_buf());
        let tool_prefix =
            sanitize_tool_segment(config.tool_prefix.as_deref().unwrap_or(name.as_str()));
        let include_tools = config
            .include_tools
            .map(|tools| parse_configured_tool_names(&name, "includeTools", tools))
            .transpose()?;
        let exclude_tools =
            parse_configured_tool_names(&name, "excludeTools", config.exclude_tools)?;
        let exposure_mode = match config.exposure_mode.as_deref().unwrap_or("auto") {
            "auto" => ProxyExposureMode::Auto,
            "full" => ProxyExposureMode::Full,
            other => {
                return Err(format!(
                    "MCP server `{name}` exposureMode must be `auto` or `full`, got `{other}`"
                ));
            }
        };
        if config
            .max_tools
            .is_some_and(|maximum| maximum > MAX_PROXY_TOOLS_PER_SERVER)
        {
            return Err(format!(
                "MCP server `{name}` maxTools must be at most {MAX_PROXY_TOOLS_PER_SERVER}"
            ));
        }
        let default_concurrency = match &transport {
            McpProxyTransportSpec::Stdio => DEFAULT_STDIO_MAX_CONCURRENT_REQUESTS,
            McpProxyTransportSpec::StreamableHttp { .. } => DEFAULT_HTTP_MAX_CONCURRENT_REQUESTS,
        };
        let max_concurrent_requests = config
            .max_concurrent_requests
            .unwrap_or(default_concurrency);
        if !(1..=MAX_PROXY_CONCURRENT_REQUESTS).contains(&max_concurrent_requests) {
            return Err(format!(
                "MCP server `{name}` maxConcurrentRequests must be between 1 and {MAX_PROXY_CONCURRENT_REQUESTS}"
            ));
        }

        specs.push(McpProxyServerSpec {
            name,
            transport,
            command,
            args,
            env,
            cwd,
            tool_prefix,
            include_tools,
            exclude_tools,
            max_tools: config.max_tools,
            exposure_mode,
            max_concurrent_requests,
            request_timeout: Duration::from_secs(
                config
                    .request_timeout_seconds
                    .unwrap_or(DEFAULT_REQUEST_TIMEOUT_SECONDS)
                    .clamp(1, 600),
            ),
            management_tools: config.management_tools.unwrap_or(true),
        });
    }
    Ok(specs)
}

fn validate_proxy_http_url(server_name: &str, raw: &str) -> Result<String, String> {
    let url = reqwest::Url::parse(raw.trim())
        .map_err(|error| format!("MCP server `{server_name}` has an invalid url: {error}"))?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err(format!(
            "MCP server `{server_name}` url cannot contain user information"
        ));
    }
    if url.fragment().is_some() {
        return Err(format!(
            "MCP server `{server_name}` url cannot contain a fragment"
        ));
    }
    let loopback_http = url.scheme() == "http"
        && url.host_str().is_some_and(|host| {
            host.eq_ignore_ascii_case("localhost")
                || host
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|address| address.is_loopback())
        });
    if url.scheme() != "https" && !loopback_http {
        return Err(format!(
            "MCP server `{server_name}` Streamable HTTP url must use HTTPS or loopback HTTP"
        ));
    }
    Ok(url.to_string())
}

fn validate_proxy_http_headers(
    server_name: &str,
    headers: BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>, String> {
    const MANAGED_HEADERS: &[&str] = &[
        "accept",
        "connection",
        "content-length",
        "content-type",
        "host",
        "mcp-protocol-version",
        "mcp-session-id",
        "transfer-encoding",
    ];
    for (name, value) in &headers {
        let normalized = name.trim().to_ascii_lowercase();
        if MANAGED_HEADERS.contains(&normalized.as_str()) {
            return Err(format!(
                "MCP server `{server_name}` header `{name}` is managed by Anchor"
            ));
        }
        reqwest::header::HeaderName::from_bytes(name.trim().as_bytes()).map_err(|error| {
            format!("MCP server `{server_name}` has invalid header name `{name}`: {error}")
        })?;
        reqwest::header::HeaderValue::from_str(value).map_err(|error| {
            format!("MCP server `{server_name}` has invalid header `{name}` value: {error}")
        })?;
    }
    Ok(headers)
}

fn parse_configured_tool_names(
    server_name: &str,
    field: &str,
    values: Vec<String>,
) -> Result<BTreeSet<String>, String> {
    let mut names = BTreeSet::new();
    for value in values {
        let name = value.trim();
        if name.is_empty() || name.len() > 128 {
            return Err(format!(
                "MCP server `{server_name}` {field} contains an empty or overlong tool name"
            ));
        }
        if !names.insert(name.to_string()) {
            return Err(format!(
                "MCP server `{server_name}` {field} contains duplicate tool `{name}`"
            ));
        }
    }
    Ok(names)
}

fn expand_workspace_placeholders(value: &str, workspace_path: &str) -> String {
    value
        .replace("${workspaceFolder}", workspace_path)
        .replace("${workspaceRoot}", workspace_path)
        .replace("${workspace}", workspace_path)
}

fn expand_proxy_placeholders(value: &str, workspace_path: &str) -> Result<String, String> {
    let expanded = expand_workspace_placeholders(value, workspace_path);
    let mut output = String::with_capacity(expanded.len());
    let mut remaining = expanded.as_str();
    while let Some(start) = remaining.find("${env:") {
        output.push_str(&remaining[..start]);
        let placeholder = &remaining[start + 6..];
        let end = placeholder.find('}').ok_or_else(|| {
            "downstream MCP configuration contains an unterminated ${env:...} placeholder"
                .to_string()
        })?;
        let name = &placeholder[..end];
        if name.is_empty()
            || name.len() > 128
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        {
            return Err(format!(
                "downstream MCP configuration contains invalid environment variable name `{name}`"
            ));
        }
        let resolved = std::env::var(name).map_err(|_| {
            format!("downstream MCP configuration requires missing environment variable `{name}`")
        })?;
        output.push_str(&resolved);
        remaining = &placeholder[end + 1..];
    }
    output.push_str(remaining);
    Ok(output)
}

fn sanitize_tool_segment(value: &str) -> String {
    let normalized: String = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '_' || character == '-' {
                character
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = normalized.trim_matches('_');
    if trimmed.is_empty() {
        "mcp".to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::fs;
    use std::path::Path;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use axum::body::Body;
    use axum::extract::State;
    use axum::http::{HeaderMap as AxumHeaderMap, HeaderValue as AxumHeaderValue, StatusCode};
    use axum::response::{IntoResponse, Response};
    use axum::routing::post;
    use axum::{Json, Router};
    use serde_json::{json, Value};

    use crate::tools::CancellationToken;

    use super::{
        browser_debugging_port, browser_proxy_error_code, expand_proxy_placeholders,
        merge_proxy_success_summary, normalize_proxy_tool_result, normalized_proxy_failure,
        parse_mcp_proxy_config, prepare_my_agent_browser_isolation, proxy_browser_cdp_reachable,
        proxy_catalog_digest, proxy_connection_status, proxy_failure_reason,
        proxy_management_tools, proxy_page_state, proxy_result_state_summary,
        sanitize_proxy_catalog, wrap_proxy_structured_result, McpProxyRegistry, McpProxyServerSpec,
        McpProxyTransportSpec, ProxyClientError, ProxyExposureMode, StdioMcpProxyClient,
    };

    fn test_spec() -> McpProxyServerSpec {
        McpProxyServerSpec {
            name: "test".into(),
            transport: McpProxyTransportSpec::Stdio,
            command: "noop".into(),
            args: Vec::new(),
            env: BTreeMap::new(),
            cwd: Path::new(".").to_path_buf(),
            tool_prefix: "test".into(),
            include_tools: None,
            exclude_tools: BTreeSet::new(),
            max_tools: None,
            exposure_mode: ProxyExposureMode::Full,
            max_concurrent_requests: 4,
            request_timeout: Duration::from_secs(5),
            management_tools: false,
        }
    }

    fn raw_proxy_tool(name: &str) -> serde_json::Value {
        json!({
            "name": name,
            "description": format!("Browser action {name}"),
            "inputSchema": {
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }
        })
    }

    #[derive(Clone, Default)]
    struct HttpProxyFixtureState {
        authorization_seen: Arc<AtomicBool>,
        session_seen: Arc<AtomicBool>,
        protocol_seen: Arc<AtomicBool>,
        cancellations: Arc<AtomicUsize>,
        deletes: Arc<AtomicUsize>,
    }

    fn fixture_json_response(id: u64, result: serde_json::Value) -> Response {
        Json(json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": result
        }))
        .into_response()
    }

    fn fixture_headers_valid(
        state: &HttpProxyFixtureState,
        headers: &AxumHeaderMap,
        require_session: bool,
    ) -> bool {
        if headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            == Some("Bearer fixture-token")
        {
            state.authorization_seen.store(true, Ordering::Release);
        } else {
            return false;
        }
        if !require_session {
            return true;
        }
        if headers
            .get("mcp-session-id")
            .and_then(|value| value.to_str().ok())
            == Some("fixture-session")
        {
            state.session_seen.store(true, Ordering::Release);
        } else {
            return false;
        }
        if headers
            .get("mcp-protocol-version")
            .and_then(|value| value.to_str().ok())
            == Some(crate::mcp::protocol::CURRENT_PROTOCOL_VERSION)
        {
            state.protocol_seen.store(true, Ordering::Release);
        } else {
            return false;
        }
        true
    }

    async fn http_proxy_fixture_post(
        State(state): State<HttpProxyFixtureState>,
        headers: AxumHeaderMap,
        Json(message): Json<serde_json::Value>,
    ) -> Response {
        let method = message
            .get("method")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        if !fixture_headers_valid(&state, &headers, method != "initialize") {
            return (StatusCode::BAD_REQUEST, "missing managed MCP headers").into_response();
        }
        if message.get("id").is_none() {
            if method == "notifications/cancelled" {
                state.cancellations.fetch_add(1, Ordering::AcqRel);
            }
            return StatusCode::ACCEPTED.into_response();
        }
        let id = message["id"].as_u64().unwrap_or_default();
        match method {
            "initialize" => {
                let mut response = fixture_json_response(
                    id,
                    json!({
                        "protocolVersion": crate::mcp::protocol::CURRENT_PROTOCOL_VERSION,
                        "capabilities": {},
                        "serverInfo": {"name": "http-fixture", "version": "1"}
                    }),
                );
                response.headers_mut().insert(
                    "mcp-session-id",
                    AxumHeaderValue::from_static("fixture-session"),
                );
                response
            }
            "tools/list" => fixture_json_response(
                id,
                json!({
                    "tools": [
                        {"name": "json", "description": "JSON", "inputSchema": {"type": "object", "properties": {}}},
                        {"name": "sse", "description": "SSE", "inputSchema": {"type": "object", "properties": {}}},
                        {"name": "slow", "description": "Slow", "inputSchema": {"type": "object", "properties": {}}}
                    ]
                }),
            ),
            "tools/call" => {
                let tool = message["params"]["name"].as_str().unwrap_or_default();
                if tool == "slow" {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
                let result = json!({
                    "content": [{"type": "text", "text": tool}],
                });
                if tool == "sse" {
                    let payload = json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": result
                    });
                    Response::builder()
                        .status(StatusCode::OK)
                        .header("content-type", "text/event-stream")
                        .body(Body::from(format!(
                            "event: message\r\ndata: {payload}\r\n\r\n"
                        )))
                        .expect("SSE response")
                } else {
                    fixture_json_response(id, result)
                }
            }
            _ => fixture_json_response(id, json!({})),
        }
    }

    async fn http_proxy_fixture_delete(
        State(state): State<HttpProxyFixtureState>,
        headers: AxumHeaderMap,
    ) -> Response {
        if !fixture_headers_valid(&state, &headers, true) {
            return (StatusCode::BAD_REQUEST, "missing managed MCP headers").into_response();
        }
        state.deletes.fetch_add(1, Ordering::AcqRel);
        StatusCode::NO_CONTENT.into_response()
    }

    async fn spawn_http_proxy_fixture(
    ) -> (String, HttpProxyFixtureState, tokio::task::JoinHandle<()>) {
        let state = HttpProxyFixtureState::default();
        let app = Router::new()
            .route(
                "/mcp",
                post(http_proxy_fixture_post).delete(http_proxy_fixture_delete),
            )
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("HTTP fixture listener");
        let address = listener.local_addr().expect("fixture address");
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("HTTP fixture server");
        });
        (format!("http://{address}/mcp"), state, server)
    }

    #[test]
    fn proxy_catalog_is_validated_and_published_conservatively() {
        let catalog = sanitize_proxy_catalog(
            &test_spec(),
            vec![json!({
                "name": "read_secret",
                "title": "Untrusted title",
                "description": "Untrusted description",
                "inputSchema": {
                    "type": "object",
                    "properties": {"path": {"type": "string"}},
                    "additionalProperties": false
                },
                "annotations": {
                    "readOnlyHint": true,
                    "destructiveHint": false,
                    "openWorldHint": false
                },
                "_meta": {"untrusted": true}
            })],
        )
        .expect("valid catalog");
        assert_eq!(catalog.tools.len(), 1);
        let definition = &catalog.tools[0].definition;
        assert_eq!(definition["name"], "test__read_secret");
        assert_eq!(definition["annotations"]["readOnlyHint"], false);
        assert_eq!(definition["annotations"]["destructiveHint"], true);
        assert_eq!(definition["annotations"]["openWorldHint"], true);
        assert!(definition.get("execution").is_none());
        assert!(definition.get("_meta").is_none());
        assert_eq!(definition["outputSchema"]["required"], json!(["ok"]));
        assert!(catalog.tools[0].synthesized_output_schema);
        let effective = crate::tools::build_effective_catalog_from_parts(
            "core",
            true,
            vec![definition.clone()],
        )
        .expect("sanitized proxy tool passes the final effective catalog contract");
        assert_eq!(effective.proxy_count, 1);
        let validator = jsonschema::validator_for(&catalog.tools[0].input_schema).unwrap();
        assert!(validator.validate(&json!({"path": "README.md"})).is_ok());
        assert!(validator.validate(&json!({"unknown": true})).is_err());
    }

    #[test]
    fn proxy_catalog_bounds_untrusted_title_and_description_metadata() {
        let catalog = sanitize_proxy_catalog(
            &test_spec(),
            vec![json!({
                "name": "verbose",
                "title": "标".repeat(300),
                "description": "述".repeat(3000),
                "inputSchema": {"type": "object", "properties": {}}
            })],
        )
        .expect("bounded metadata catalog");
        let definition = &catalog.tools[0].definition;
        assert!(definition["title"].as_str().unwrap().len() <= 256);
        let description = definition["description"].as_str().unwrap();
        assert!(description.starts_with("[test] "));
        assert!(description.len() <= 2048 + "[test] ".len());
        assert!(description.is_char_boundary(description.len()));
    }

    #[test]
    fn proxy_catalog_rejects_invalid_or_external_schemas() {
        assert!(sanitize_proxy_catalog(
            &test_spec(),
            vec![json!({
                "name": "bad",
                "inputSchema": {"type": "string"}
            })],
        )
        .is_err());
        assert!(sanitize_proxy_catalog(
            &test_spec(),
            vec![json!({
                "name": "external",
                "inputSchema": {
                    "type": "object",
                    "properties": {"value": {"$ref": "https://example.com/schema.json"}}
                }
            })],
        )
        .is_err());
    }

    #[test]
    fn proxy_catalog_digest_changes_when_schema_changes() {
        let first = sanitize_proxy_catalog(
            &test_spec(),
            vec![json!({
                "name": "ping",
                "inputSchema": {"type": "object", "properties": {}}
            })],
        )
        .expect("first catalog");
        let second = sanitize_proxy_catalog(
            &test_spec(),
            vec![json!({
                "name": "ping",
                "inputSchema": {
                    "type": "object",
                    "properties": {"value": {"type": "string"}},
                    "required": ["value"]
                }
            })],
        )
        .expect("second catalog");
        assert_ne!(
            proxy_catalog_digest(&first.tools).unwrap(),
            proxy_catalog_digest(&second.tools).unwrap()
        );
    }

    #[test]
    fn browser_proxy_failures_are_classified_for_recovery() {
        assert_eq!(
            proxy_failure_reason(
                "browser__navigate",
                &ProxyClientError::Timeout {
                    method: "tools/call".into(),
                    seconds: 90,
                },
            ),
            "page_load_timeout"
        );
        assert_eq!(
            proxy_failure_reason(
                "browser__wait_for",
                &ProxyClientError::Timeout {
                    method: "tools/call".into(),
                    seconds: 30,
                },
            ),
            "element_wait_timeout"
        );
        assert_eq!(
            proxy_failure_reason(
                "browser__click",
                &ProxyClientError::Transport("CDP target closed".into()),
            ),
            "devtools_channel_disconnected"
        );
        assert!(!ProxyClientError::Timeout {
            method: "tools/call".into(),
            seconds: 30,
        }
        .invalidates_connection());
        assert_eq!(
            browser_proxy_error_code("browser__fill", "Element uid=12 is stale and not found"),
            "STALE_ELEMENT_REFERENCE"
        );
        assert_eq!(
            browser_proxy_error_code("browser__wait_for", "request timed out after 30 seconds"),
            "ELEMENT_WAIT_TIMEOUT"
        );
        assert_eq!(
            browser_proxy_error_code(
                "browser__evaluate_script",
                "request timed out after 30 seconds"
            ),
            "SCRIPT_TIMEOUT"
        );
    }

    #[test]
    fn browser_state_summary_keeps_page_focus_and_layer_diagnostics_bounded() {
        let summary = proxy_result_state_summary(&json!({
            "structuredContent": {
                "ok": true,
                "page": {
                    "url": "https://example.test/story",
                    "title": "Story Home",
                    "activeElement": {"role": "button", "name": "Tooltip trigger"},
                    "openTooltips": [{"id": "tooltip-1"}],
                    "dismissableLayerStack": ["tooltip", "sheet"]
                }
            }
        }));
        assert_eq!(summary["current_page"]["url"], "https://example.test/story");
        assert_eq!(summary["current_page"]["title"], "Story Home");
        assert_eq!(summary["activeElement"]["role"], "button");
        assert_eq!(
            summary["dismissableLayerStack"],
            json!(["tooltip", "sheet"])
        );
    }

    #[test]
    fn browser_page_state_parses_normalized_list_pages_text() {
        let state = proxy_page_state(&json!({
            "ok": true,
            "result": {
                "text": "## Pages\n6: Gaoge (http://127.0.0.1:8080/characters/one) [selected]\n7: Blank (about:blank)"
            }
        }));

        assert_eq!(state["page_count"], 2);
        assert_eq!(state["pages"][0]["page_id"], 6);
        assert_eq!(state["pages"][0]["title"], "Gaoge");
        assert_eq!(
            state["pages"][0]["url"],
            "http://127.0.0.1:8080/characters/one"
        );
        assert_eq!(state["selected_page"]["page_id"], 6);
        assert_eq!(state["pages"][1]["url"], "about:blank");
    }

    #[test]
    fn browser_success_summary_preserves_last_known_page_state_for_non_page_tools() {
        let previous = proxy_result_state_summary(&json!({
            "structuredContent": {
                "ok": true,
                "page_count": 1,
                "pages": [{
                    "page_id": 3,
                    "title": "Workspace",
                    "url": "https://example.test/workspace",
                    "selected": true
                }],
                "selected_page": {
                    "page_id": 3,
                    "title": "Workspace",
                    "url": "https://example.test/workspace",
                    "selected": true
                }
            }
        }));
        let merged = merge_proxy_success_summary(
            Some(&previous),
            &json!({
                "structuredContent": {
                    "ok": true,
                    "result": {"value": 42}
                }
            }),
        );

        assert_eq!(merged["page_count"], 1);
        assert_eq!(merged["pages"][0]["page_id"], 3);
        assert_eq!(merged["selected_page"]["page_id"], 3);
    }

    #[test]
    fn my_agent_browser_is_scoped_to_each_workspace_profile() {
        let root = tempfile::tempdir().expect("browser workspace");
        let mut first = test_spec();
        first.command = "node".into();
        first.args = vec!["tools/my-agent-browser/start-mcp.js".into()];
        first.cwd = root.path().to_path_buf();
        first.name = "browser".into();
        let first_isolation = prepare_my_agent_browser_isolation(&mut first, "workspace-one")
            .expect("first isolation")
            .expect("browser isolation");

        let mut second = first.clone();
        second.env.clear();
        let second_isolation = prepare_my_agent_browser_isolation(&mut second, "workspace-two")
            .expect("second isolation")
            .expect("browser isolation");

        assert_ne!(first_isolation.home, second_isolation.home);
        assert_ne!(
            first_isolation.debugging_port,
            second_isolation.debugging_port
        );
        assert_eq!(
            first.env.get("MY_AGENT_BROWSER_HOME"),
            Some(&super::display_proxy_path(&first_isolation.home))
        );
        let first_config: Value = serde_json::from_str(
            &fs::read_to_string(first_isolation.home.join("config.json"))
                .expect("first browser config"),
        )
        .expect("first config json");
        assert_eq!(
            first_config["browser"]["debuggingPort"],
            first_isolation.debugging_port
        );
        assert_eq!(first_config["browser"]["headless"], true);
        assert_eq!(first_config["browser"]["lazyStart"], true);
        assert!(Path::new(
            first_config["browser"]["userDataDir"]
                .as_str()
                .expect("user data dir")
        )
        .ends_with("user-data"));

        fs::write(
            first_isolation.home.join("config.json"),
            serde_json::to_string_pretty(&json!({
                "browser": {
                    "userDataDir": "D:\\wrong-profile",
                    "debuggingPort": 1,
                    "headless": false,
                    "lazyStart": false,
                    "proxy": {"server": "http://127.0.0.1:8080"}
                }
            }))
            .expect("custom config"),
        )
        .expect("write custom config");
        let mut repaired = test_spec();
        repaired.command = "node".into();
        repaired.args = vec!["tools/my-agent-browser/start-mcp.js".into()];
        repaired.cwd = root.path().to_path_buf();
        repaired.name = "browser".into();
        let repaired_isolation = prepare_my_agent_browser_isolation(&mut repaired, "workspace-one")
            .expect("repaired isolation")
            .expect("browser isolation");
        let repaired_config: Value = serde_json::from_str(
            &fs::read_to_string(repaired_isolation.home.join("config.json"))
                .expect("repaired browser config"),
        )
        .expect("repaired config json");
        assert_eq!(
            repaired_config["browser"]["debuggingPort"],
            first_isolation.debugging_port
        );
        assert!(Path::new(
            repaired_config["browser"]["userDataDir"]
                .as_str()
                .expect("repaired user data dir")
        )
        .ends_with("user-data"));
        assert_eq!(repaired_config["browser"]["headless"], false);
        assert_eq!(repaired_config["browser"]["lazyStart"], false);
        assert_eq!(
            repaired_config["browser"]["proxy"]["server"],
            "http://127.0.0.1:8080"
        );

        let explicit_home = root.path().join("explicit-browser-home");
        fs::create_dir_all(&explicit_home).expect("explicit browser home");
        fs::write(
            explicit_home.join("config.json"),
            r#"{"browser":{"debuggingPort":45678}}"#,
        )
        .expect("explicit config");
        let mut explicit = first.clone();
        explicit.env.insert(
            "MY_AGENT_BROWSER_HOME".into(),
            explicit_home.display().to_string(),
        );
        let explicit_isolation =
            prepare_my_agent_browser_isolation(&mut explicit, "workspace-three")
                .expect("explicit isolation")
                .expect("browser isolation");
        assert_eq!(explicit_isolation.home, explicit_home);
        assert_eq!(explicit_isolation.debugging_port, 45_678);
    }

    #[test]
    fn managed_browser_skips_an_unbindable_deterministic_port() {
        let root = tempfile::tempdir().expect("browser workspace");
        let (workspace_id, blocked_port, _reservation) = (0..4_096)
            .find_map(|index| {
                let workspace_id = format!("blocked-workspace-{index}");
                let port = browser_debugging_port(&workspace_id, "browser");
                std::net::TcpListener::bind(("127.0.0.1", port))
                    .ok()
                    .map(|listener| (workspace_id, port, listener))
            })
            .expect("find a bindable deterministic candidate");

        let mut spec = test_spec();
        spec.command = "node".into();
        spec.args = vec!["tools/my-agent-browser/start-mcp.js".into()];
        spec.cwd = root.path().to_path_buf();
        spec.name = "browser".into();
        let isolation = prepare_my_agent_browser_isolation(&mut spec, &workspace_id)
            .expect("browser isolation")
            .expect("managed browser");

        assert_ne!(isolation.debugging_port, blocked_port);
        std::net::TcpListener::bind(("127.0.0.1", isolation.debugging_port))
            .expect("selected replacement port remains bindable");
    }

    #[test]
    fn proxy_catalog_applies_include_exclude_and_max_tools_deterministically() {
        let mut spec = test_spec();
        spec.include_tools = Some(
            ["click", "navigate", "screenshot"]
                .into_iter()
                .map(str::to_string)
                .collect(),
        );
        spec.exclude_tools = ["navigate"].into_iter().map(str::to_string).collect();
        spec.max_tools = Some(1);

        let catalog = sanitize_proxy_catalog(
            &spec,
            vec![
                raw_proxy_tool("screenshot"),
                raw_proxy_tool("evaluate"),
                raw_proxy_tool("navigate"),
                raw_proxy_tool("click"),
                raw_proxy_tool("tabs"),
            ],
        )
        .expect("filtered catalog");

        assert_eq!(catalog.discovered_count, 5);
        assert_eq!(catalog.filtered_count, 3);
        assert_eq!(catalog.truncated_count, 1);
        assert_eq!(catalog.tools.len(), 1);
        assert_eq!(catalog.tools[0].downstream_name, "click");
        assert_eq!(catalog.tools[0].public_name, "test__click");
    }

    #[test]
    fn automatic_generic_exposure_requires_policy_for_high_fanout_catalogs() {
        let mut spec = test_spec();
        spec.exposure_mode = ProxyExposureMode::Auto;
        let tools = (0..25)
            .map(|index| raw_proxy_tool(&format!("tool_{index:02}")))
            .collect::<Vec<_>>();

        let error = sanitize_proxy_catalog(&spec, tools).expect_err("unbounded catalog");
        assert!(error.contains("without an explicit exposure policy"));
        assert!(error.contains("limited to 24"));

        spec.exposure_mode = ProxyExposureMode::Full;
        let tools = (0..25)
            .map(|index| raw_proxy_tool(&format!("tool_{index:02}")))
            .collect::<Vec<_>>();
        let catalog = sanitize_proxy_catalog(&spec, tools).expect("explicit full catalog");
        assert_eq!(catalog.tools.len(), 25);
        assert_eq!(catalog.selection_source, "explicit_full");
    }

    #[test]
    fn automatic_browser_exposure_keeps_the_default_workflow_and_filters_low_frequency_tools() {
        let mut spec = test_spec();
        spec.command = "my-agent-browser".into();
        spec.exposure_mode = ProxyExposureMode::Auto;
        let catalog = sanitize_proxy_catalog(
            &spec,
            vec![
                raw_proxy_tool("list_pages"),
                raw_proxy_tool("navigate_page"),
                raw_proxy_tool("take_snapshot"),
                raw_proxy_tool("click"),
                raw_proxy_tool("lighthouse_audit"),
                raw_proxy_tool("take_heapsnapshot"),
            ],
        )
        .expect("browser workflow catalog");

        let names = catalog
            .tools
            .iter()
            .map(|tool| tool.downstream_name.as_str())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            names,
            ["click", "list_pages", "navigate_page", "take_snapshot"]
                .into_iter()
                .collect()
        );
        assert_eq!(catalog.selection_source, "browser_workflow");
        assert_eq!(catalog.discovered_count, 6);
        assert_eq!(catalog.filtered_count, 2);
    }

    #[test]
    fn proxy_catalog_rejects_missing_include_tools_entries() {
        let mut spec = test_spec();
        spec.include_tools = Some(["missing"].into_iter().map(str::to_string).collect());

        let error = sanitize_proxy_catalog(&spec, vec![raw_proxy_tool("click")])
            .expect_err("missing configured tool");
        assert!(error.contains("did not advertise includeTools entries: missing"));
    }

    #[test]
    fn proxy_results_enforce_output_schema_and_add_text_fallback() {
        let output_schema = json!({
            "type": "object",
            "properties": {"value": {"type": "string"}},
            "required": ["value"],
            "additionalProperties": false
        });
        let valid = normalize_proxy_tool_result(
            "test",
            "test__ping",
            json!({
                "content": [{"type": "image", "data": "AA==", "mimeType": "image/png"}],
                "structuredContent": {"value": "ok"}
            }),
            &output_schema,
            false,
        )
        .expect("valid result");
        assert!(valid["content"]
            .as_array()
            .is_some_and(|content| content.iter().any(|item| item["type"] == "text")));
        assert!(normalize_proxy_tool_result(
            "test",
            "test__ping",
            json!({
                "content": [{"type": "text", "text": "bad"}],
                "structuredContent": {"value": 1}
            }),
            &output_schema,
            false,
        )
        .is_err());
    }

    #[test]
    fn proxy_results_synthesize_structured_output_when_downstream_omits_schema() {
        let catalog = sanitize_proxy_catalog(&test_spec(), vec![raw_proxy_tool("click")])
            .expect("proxy catalog");
        let tool = &catalog.tools[0];

        let normalized = normalize_proxy_tool_result(
            "browser",
            "browser__click",
            json!({
                "content": [{"type": "text", "text": "clicked"}]
            }),
            &tool.output_schema,
            tool.synthesized_output_schema,
        )
        .expect("normalized result");

        assert_eq!(normalized["structuredContent"]["ok"], true);
        assert_eq!(normalized["structuredContent"]["result"]["text"], "clicked");
        assert_eq!(
            normalized["structuredContent"]["result"]["content"][0]["type"],
            "text"
        );
        assert_eq!(normalized["isError"], false);
        assert!(jsonschema::validator_for(&tool.output_schema)
            .expect("fallback output schema")
            .validate(&normalized["structuredContent"])
            .is_ok());

        let screenshot = normalize_proxy_tool_result(
            "browser",
            "test__take_screenshot",
            json!({
                "content": [{
                    "type": "image",
                    "data": "QUJDREVGRw==",
                    "mimeType": "image/png"
                }]
            }),
            &tool.output_schema,
            tool.synthesized_output_schema,
        )
        .expect("screenshot result");
        assert_eq!(screenshot["content"][0]["data"], "QUJDREVGRw==");
        assert_eq!(
            screenshot["structuredContent"]["result"]["content"][0]["data_omitted"],
            true
        );
        assert!(screenshot["structuredContent"]["result"]["content"][0]
            .get("data")
            .is_none());
    }

    #[test]
    fn normalized_proxy_errors_are_not_counted_as_successes() {
        let catalog = sanitize_proxy_catalog(&test_spec(), vec![raw_proxy_tool("click")])
            .expect("proxy catalog");
        let tool = &catalog.tools[0];
        let normalized = normalize_proxy_tool_result(
            "browser",
            "test__click",
            json!({
                "content": [{"type": "text", "text": "Chrome failed to start (port 45678 not reachable)"}],
                "isError": true
            }),
            &tool.output_schema,
            tool.synthesized_output_schema,
        )
        .expect("normalized browser failure");

        let (code, message) = normalized_proxy_failure(&normalized).expect("proxy failure");
        assert_eq!(code, "BROWSER_NOT_CONNECTED");
        assert!(message.contains("port 45678 not reachable"));
        assert_eq!(normalized["structuredContent"]["ok"], false);
    }

    #[test]
    fn browser_cdp_reachability_reflects_the_actual_debugging_port() {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("test listener");
        let port = listener.local_addr().expect("listener address").port();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept CDP probe");
            let mut request = [0u8; 1_024];
            let _ = std::io::Read::read(&mut stream, &mut request);
            std::io::Write::write_all(
                &mut stream,
                b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 20\r\nConnection: close\r\n\r\n{\"Browser\":\"Chrome\"}",
            )
            .expect("write CDP response");
        });
        let status = json!({
            "workspace_isolation": {
                "debugging_port": port
            }
        });
        assert_eq!(proxy_browser_cdp_reachable(&status), Some(true));
        server.join().expect("CDP probe server");

        assert_eq!(proxy_browser_cdp_reachable(&status), Some(false));
        assert_eq!(proxy_browser_cdp_reachable(&json!({})), None);
        assert_eq!(
            proxy_connection_status(&json!({
                "transport_connected": true,
                "operational": true
            })),
            "connected"
        );
        assert_eq!(
            proxy_connection_status(&json!({
                "transport_connected": true,
                "operational": false
            })),
            "degraded"
        );
        assert_eq!(
            proxy_connection_status(&json!({
                "transport_connected": false,
                "operational": false
            })),
            "disconnected"
        );
    }

    #[test]
    fn browser_management_contract_exposes_probe_freshness() {
        let mut spec = test_spec();
        spec.management_tools = true;
        let entries = proxy_management_tools(&spec);
        let health = entries
            .iter()
            .find(|(name, ..)| name.ends_with("__health_check"))
            .expect("health management tool");
        let output_schema = &health.4;
        assert!(output_schema["properties"]["observed_at"].is_object());
        assert!(output_schema["properties"]["recovered_during_probe"].is_object());
        let required = output_schema["required"].as_array().expect("required");
        assert!(required.iter().any(|item| item == "observed_at"));
        assert!(required.iter().any(|item| item == "recovered_during_probe"));
    }

    #[test]
    fn parses_standard_mcp_servers_and_expands_workspace_folder() {
        let specs = parse_mcp_proxy_config(
            r#"{
                "mcpServers": {
                    "code graph": {
                        "type": "stdio",
                        "command": "codegraph",
                        "args": ["serve", "--path", "${workspaceFolder}"]
                    }
                }
            }"#,
            Path::new("/tmp/example"),
        )
        .expect("parse proxy config");

        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].name, "code graph");
        assert_eq!(specs[0].tool_prefix, "code_graph");
        assert_eq!(specs[0].args[2], "/tmp/example");
        assert!(specs[0].management_tools);
    }

    #[test]
    fn parses_proxy_tool_selection_controls() {
        let specs = parse_mcp_proxy_config(
            r#"{
                "mcpServers": {
                    "browser": {
                        "command": "browser-mcp",
                        "includeTools": ["navigate", "click", "screenshot"],
                        "excludeTools": ["screenshot"],
                        "maxTools": 2,
                        "maxConcurrentRequests": 3
                    }
                }
            }"#,
            Path::new("/tmp/example"),
        )
        .expect("parse selection controls");

        let spec = &specs[0];
        assert_eq!(spec.max_tools, Some(2));
        assert_eq!(spec.exposure_mode, ProxyExposureMode::Auto);
        assert_eq!(spec.max_concurrent_requests, 3);
        assert!(spec
            .include_tools
            .as_ref()
            .is_some_and(|tools| tools.contains("navigate") && tools.contains("click")));
        assert!(spec.exclude_tools.contains("screenshot"));
    }

    #[test]
    fn management_tools_can_be_disabled_and_use_stable_prefixed_names() {
        let specs = parse_mcp_proxy_config(
            r#"{"mcpServers":{"browser":{"command":"browser-mcp","managementTools":false}}}"#,
            Path::new("/tmp/example"),
        )
        .expect("parse management flag");
        assert!(!specs[0].management_tools);
        assert!(proxy_management_tools(&specs[0]).is_empty());

        let mut enabled = specs[0].clone();
        enabled.management_tools = true;
        let names = proxy_management_tools(&enabled)
            .into_iter()
            .map(|(name, ..)| name)
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            vec![
                "browser__health_check",
                "browser__reconnect",
                "browser__reset_session"
            ]
        );
    }

    #[test]
    fn management_results_accept_null_error_and_never_publish_null_structured_content() {
        let mut spec = test_spec();
        spec.management_tools = true;
        let tools = proxy_management_tools(&spec);
        let output_schema = tools[0].4.clone();
        let wrapped = wrap_proxy_structured_result(
            json!({
                "ok": true,
                "status": "healthy",
                "server": "browser",
                "connection": {"connected": true},
                "error": null,
                "error_code": null,
                "error_message": null,
                "retryable": false,
                "browser_session_id": "session-1",
                "connection_status": "connected",
                "transport_connected": true,
                "operational": true,
                "browser_cdp_reachable": true,
                "observed_at": "2026-08-11T03:00:00Z",
                "recovered_during_probe": false,
                "page_count": 0,
                "pages": [],
                "selected_page": null,
                "page_state": {}
            }),
            &output_schema,
        );
        assert!(wrapped["structuredContent"].is_object());
        assert_eq!(wrapped["structuredContent"]["error"], Value::Null);
        assert_eq!(wrapped["isError"], false);

        let invalid = wrap_proxy_structured_result(Value::Null, &output_schema);
        assert!(invalid["structuredContent"].is_object());
        assert_eq!(
            invalid["structuredContent"]["error_code"],
            "DOWNSTREAM_SCHEMA_MISMATCH"
        );
        assert_eq!(invalid["isError"], true);
    }

    #[tokio::test]
    async fn browser_health_check_rejects_downstream_page_errors() {
        let Ok(python) = which::which("python") else {
            return;
        };
        let temp = tempfile::tempdir().expect("tempdir");
        let script = temp.path().join("browser_health_error.py");
        fs::write(
            &script,
            r#"import json
import sys

for raw in sys.stdin:
    message = json.loads(raw)
    if "id" not in message:
        continue
    request_id = message["id"]
    method = message.get("method")
    if method == "initialize":
        result = {"protocolVersion": "2025-11-25", "capabilities": {}, "serverInfo": {"name": "browser", "version": "1"}}
    elif method == "tools/list":
        result = {"tools": [{"name": "list_pages", "description": "List pages", "inputSchema": {"type": "object", "properties": {}}}]}
    elif method == "tools/call":
        result = {"content": [{"type": "text", "text": "Chrome failed to start (port unavailable)"}], "isError": True}
    else:
        result = {}
    print(json.dumps({"jsonrpc": "2.0", "id": request_id, "result": result}), flush=True)
"#,
        )
        .expect("write browser fixture");

        let registry = McpProxyRegistry::default();
        registry
            .configure(
                vec![McpProxyServerSpec {
                    name: "browser".into(),
                    transport: McpProxyTransportSpec::Stdio,
                    command: python.display().to_string(),
                    args: vec![script.display().to_string()],
                    env: BTreeMap::new(),
                    cwd: temp.path().to_path_buf(),
                    tool_prefix: "browser".into(),
                    include_tools: None,
                    exclude_tools: BTreeSet::new(),
                    max_tools: None,
                    exposure_mode: ProxyExposureMode::Full,
                    max_concurrent_requests: 2,
                    request_timeout: Duration::from_secs(3),
                    management_tools: true,
                }],
                "browser-health-error-test",
            )
            .await;

        let status = registry.status();
        let server = &status["servers"][0];
        assert_eq!(server["exposure_mode"], "full");
        assert_eq!(server["selection_source"], "explicit_full");
        assert_eq!(server["discovered_tool_count"], 1);
        assert_eq!(server["selected_downstream_tool_count"], 1);

        let health = registry
            .call_tool("browser__health_check", &json!({}))
            .await
            .expect("health route")
            .expect("health result");
        assert_eq!(health["structuredContent"]["ok"], false);
        assert_eq!(health["structuredContent"]["status"], "unhealthy");
        assert_eq!(health["isError"], true);
        assert!(health["structuredContent"]["error_message"]
            .as_str()
            .is_some_and(|message| message.contains("Chrome failed to start")));
    }

    #[cfg(target_os = "windows")]
    #[tokio::test]
    async fn stdio_termination_kills_windows_descendant_processes() {
        let Ok(python) = which::which("python") else {
            return;
        };
        let temp = tempfile::tempdir().expect("tempdir");
        let script = temp.path().join("process_tree_mcp.py");
        let child_pid_path = temp.path().join("child.pid");
        fs::write(
            &script,
            r#"import json
import subprocess
import sys

child = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(60)"])
with open(sys.argv[1], "w", encoding="utf-8") as marker:
    marker.write(str(child.pid))

for raw in sys.stdin:
    message = json.loads(raw)
    if "id" not in message:
        continue
    request_id = message["id"]
    method = message.get("method")
    if method == "initialize":
        result = {"protocolVersion": "2025-11-25", "capabilities": {}, "serverInfo": {"name": "tree", "version": "1"}}
    elif method == "tools/list":
        result = {"tools": []}
    else:
        result = {}
    print(json.dumps({"jsonrpc": "2.0", "id": request_id, "result": result}), flush=True)
"#,
        )
        .expect("write process-tree fixture");

        let mut spec = test_spec();
        spec.command = python.display().to_string();
        spec.args = vec![
            script.display().to_string(),
            child_pid_path.display().to_string(),
        ];
        spec.cwd = temp.path().to_path_buf();
        spec.name = "tree".into();
        let (client, _) = StdioMcpProxyClient::connect(spec, "process-tree-test")
            .await
            .expect("connect process-tree fixture");
        let child_pid = fs::read_to_string(&child_pid_path)
            .expect("read child pid")
            .parse::<u32>()
            .expect("child pid number");
        assert!(crate::platform::platform().is_process_alive(child_pid));

        client.terminate().await;
        for _ in 0..20 {
            if !crate::platform::platform().is_process_alive(child_pid) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(!crate::platform::platform().is_process_alive(child_pid));
    }

    #[test]
    fn parses_streamable_http_servers_and_authorization_headers() {
        let specs = parse_mcp_proxy_config(
            r#"{
                "mcpServers": {
                    "remote": {
                        "type": "streamable-http",
                        "url": "https://mcp.example.com/api",
                        "headers": {
                            "Authorization": "Bearer secret",
                            "X-Workspace": "${workspaceFolder}"
                        }
                    }
                }
            }"#,
            Path::new("/tmp/example"),
        )
        .expect("parse Streamable HTTP config");

        assert_eq!(specs.len(), 1);
        match &specs[0].transport {
            McpProxyTransportSpec::StreamableHttp { url, headers } => {
                assert_eq!(url, "https://mcp.example.com/api");
                assert_eq!(headers["Authorization"], "Bearer secret");
                assert_eq!(headers["X-Workspace"], "/tmp/example");
            }
            McpProxyTransportSpec::Stdio => panic!("expected Streamable HTTP transport"),
        }
    }

    #[test]
    fn expands_environment_placeholders_without_persisting_secret_values() {
        if let Ok(path) = std::env::var("PATH") {
            assert_eq!(
                expand_proxy_placeholders("Bearer ${env:PATH}", "/tmp/example")
                    .expect("expand existing environment variable"),
                format!("Bearer {path}")
            );
        }
        let error = expand_proxy_placeholders(
            "Bearer ${env:ANCHOR_PROXY_TEST_VARIABLE_THAT_MUST_NOT_EXIST}",
            "/tmp/example",
        )
        .expect_err("missing environment variable");
        assert!(error.contains("ANCHOR_PROXY_TEST_VARIABLE_THAT_MUST_NOT_EXIST"));
        assert!(!error.contains("Bearer"));
    }

    #[test]
    fn rejects_invalid_proxy_tool_selection_controls() {
        let duplicate = parse_mcp_proxy_config(
            r#"{"mcpServers":{"browser":{"command":"browser-mcp","includeTools":["click","click"]}}}"#,
            Path::new("/tmp/example"),
        )
        .expect_err("duplicate includeTools");
        assert!(duplicate.contains("includeTools contains duplicate tool `click`"));

        let excessive = parse_mcp_proxy_config(
            r#"{"mcpServers":{"browser":{"command":"browser-mcp","maxTools":257}}}"#,
            Path::new("/tmp/example"),
        )
        .expect_err("excessive maxTools");
        assert!(excessive.contains("maxTools must be at most 256"));

        let invalid_exposure = parse_mcp_proxy_config(
            r#"{"mcpServers":{"browser":{"command":"browser-mcp","exposureMode":"everything"}}}"#,
            Path::new("/tmp/example"),
        )
        .expect_err("invalid exposure mode");
        assert!(invalid_exposure.contains("exposureMode must be `auto` or `full`"));

        let invalid_concurrency = parse_mcp_proxy_config(
            r#"{"mcpServers":{"browser":{"command":"browser-mcp","maxConcurrentRequests":0}}}"#,
            Path::new("/tmp/example"),
        )
        .expect_err("invalid maxConcurrentRequests");
        assert!(invalid_concurrency.contains("must be between 1 and 64"));
    }

    #[test]
    fn rejects_legacy_sse_and_insecure_remote_http_transports() {
        let legacy = parse_mcp_proxy_config(
            r#"{"mcpServers":{"remote":{"type":"sse","command":"noop"}}}"#,
            Path::new("/tmp/example"),
        )
        .expect_err("reject legacy SSE transport");
        assert!(legacy.contains("legacy SSE transport"));

        let insecure = parse_mcp_proxy_config(
            r#"{"mcpServers":{"remote":{"type":"streamable-http","url":"http://example.com/mcp"}}}"#,
            Path::new("/tmp/example"),
        )
        .expect_err("reject insecure remote HTTP");
        assert!(insecure.contains("must use HTTPS or loopback HTTP"));

        let managed_header = parse_mcp_proxy_config(
            r#"{"mcpServers":{"remote":{"url":"https://example.com/mcp","headers":{"MCP-Session-Id":"forged"}}}}"#,
            Path::new("/tmp/example"),
        )
        .expect_err("reject managed headers");
        assert!(managed_header.contains("is managed by Anchor"));
    }

    #[tokio::test]
    async fn readiness_waits_for_initial_configuration() {
        let registry = McpProxyRegistry::default();
        registry.begin_configuration();
        assert!(
            !registry
                .wait_until_configured(Duration::from_millis(10))
                .await
        );

        let configured = registry.clone();
        tokio::spawn(async move {
            configured.configure(Vec::new(), "readiness-test").await;
        });
        assert!(registry.wait_until_configured(Duration::from_secs(1)).await);
    }

    #[test]
    fn proxy_status_surfaces_unavailable_servers_and_governance_failures() {
        let registry = McpProxyRegistry::default();
        registry
            .state
            .write()
            .expect("proxy registry state")
            .failures
            .insert(
                "large-catalog".into(),
                "automatic exposure is limited to 24".into(),
            );

        let status = registry.status();
        assert_eq!(status["server_count"], 0);
        assert_eq!(status["unavailable_server_count"], 1);
        assert_eq!(status["unavailable_servers"][0]["name"], "large-catalog");
        assert!(status["unavailable_servers"][0]["error"]
            .as_str()
            .is_some_and(|error| error.contains("limited to 24")));
    }

    #[tokio::test]
    async fn concurrent_proxy_merge_resolves_duplicate_public_names_by_server_name() {
        let Ok(python) = which::which("python") else {
            return;
        };
        let temp = tempfile::tempdir().expect("tempdir");
        let script = temp.path().join("deterministic_merge_mcp.py");
        fs::write(
            &script,
            r#"import json
import sys
import time

server_name = sys.argv[1]
delay = float(sys.argv[2])
for raw in sys.stdin:
    message = json.loads(raw)
    if "id" not in message:
        continue
    request_id = message["id"]
    method = message.get("method")
    if method == "initialize":
        if delay:
            time.sleep(delay)
        result = {"protocolVersion": "2025-11-25", "capabilities": {}, "serverInfo": {"name": server_name, "version": "1"}}
    elif method == "tools/list":
        result = {"tools": [{"name": "ping", "description": server_name, "inputSchema": {"type": "object", "properties": {}}}]}
    else:
        result = {}
    print(json.dumps({"jsonrpc": "2.0", "id": request_id, "result": result}), flush=True)
"#,
        )
        .expect("write deterministic merge fixture");

        let make_spec = |name: &str, delay: &str| McpProxyServerSpec {
            name: name.into(),
            transport: McpProxyTransportSpec::Stdio,
            command: python.display().to_string(),
            args: vec![script.display().to_string(), name.into(), delay.into()],
            env: BTreeMap::new(),
            cwd: temp.path().to_path_buf(),
            tool_prefix: "shared".into(),
            include_tools: None,
            exclude_tools: BTreeSet::new(),
            max_tools: None,
            exposure_mode: ProxyExposureMode::Full,
            max_concurrent_requests: 2,
            request_timeout: Duration::from_secs(5),
            management_tools: false,
        };

        let registry = McpProxyRegistry::default();
        registry
            .configure(
                vec![make_spec("zeta", "0"), make_spec("alpha", "0.2")],
                "deterministic-merge-test",
            )
            .await;

        let state = registry.state.read().expect("proxy registry state");
        let route = state.routes.get("shared__ping").expect("shared ping route");
        assert_eq!(route.server_name, "alpha");
    }

    #[tokio::test]
    async fn streamable_http_proxy_supports_sessions_json_sse_cancellation_and_delete() {
        let (url, state, server) = spawn_http_proxy_fixture().await;
        let config = json!({
            "mcpServers": {
                "remote": {
                    "type": "streamable-http",
                    "url": url,
                    "headers": {
                        "Authorization": "Bearer fixture-token"
                    },
                    "requestTimeoutSeconds": 10
                }
            }
        })
        .to_string();
        let specs = parse_mcp_proxy_config(&config, Path::new("/tmp/example"))
            .expect("parse HTTP fixture config");
        let registry = McpProxyRegistry::default();
        registry.configure(specs, "proxy-http-test").await;

        let names = registry
            .list_tools()
            .into_iter()
            .filter_map(|tool| tool["name"].as_str().map(str::to_string))
            .collect::<BTreeSet<_>>();
        assert_eq!(
            names,
            [
                "remote__health_check",
                "remote__json",
                "remote__reconnect",
                "remote__reset_session",
                "remote__slow",
                "remote__sse",
            ]
            .into_iter()
            .map(str::to_string)
            .collect()
        );

        let json_result = registry
            .call_tool("remote__json", &json!({}))
            .await
            .expect("known JSON route")
            .expect("JSON result");
        assert_eq!(json_result["structuredContent"]["ok"], true);

        let sse_result = registry
            .call_tool("remote__sse", &json!({}))
            .await
            .expect("known SSE route")
            .expect("SSE result");
        assert_eq!(sse_result["structuredContent"]["ok"], true);
        assert!(sse_result["content"]
            .as_array()
            .is_some_and(|content| content.iter().any(|item| item["text"] == "sse")));

        let cancellation = CancellationToken::default();
        let worker_registry = registry.clone();
        let worker_cancellation = cancellation.clone();
        let slow = tokio::spawn(async move {
            worker_registry
                .call_tool_with_cancellation("remote__slow", &json!({}), &worker_cancellation)
                .await
                .expect("known slow route")
                .expect("cancelled HTTP result")
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        cancellation.cancel();
        let cancelled = slow.await.expect("cancelled worker");
        assert_eq!(
            cancelled["structuredContent"]["error"]["details"]["reason"],
            "proxy_call_cancelled"
        );
        for _ in 0..20 {
            if state.cancellations.load(Ordering::Acquire) > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert_eq!(state.cancellations.load(Ordering::Acquire), 1);
        assert!(state.authorization_seen.load(Ordering::Acquire));
        assert!(state.session_seen.load(Ordering::Acquire));
        assert!(state.protocol_seen.load(Ordering::Acquire));

        let status = registry.status();
        assert_eq!(status["configured"], true);
        assert_eq!(status["server_count"], 1);
        assert_eq!(status["servers"][0]["name"], "remote");
        assert_eq!(status["servers"][0]["transport"], "streamable-http");
        assert_eq!(status["servers"][0]["tool_count"], 6);
        assert_eq!(status["servers"][0]["calls"], 3);
        assert_eq!(status["servers"][0]["cancellations"], 1);
        let encoded_status = status.to_string();
        assert!(!encoded_status.contains("fixture-token"));
        assert!(!encoded_status.contains("/mcp"));

        drop(registry);
        for _ in 0..40 {
            if state.deletes.load(Ordering::Acquire) > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert_eq!(state.deletes.load(Ordering::Acquire), 1);
        server.abort();
    }

    #[tokio::test]
    async fn cancellation_stops_downstream_call_without_replaying() {
        let Ok(python) = which::which("python") else {
            return;
        };
        let temp = tempfile::tempdir().expect("tempdir");
        let script = temp.path().join("slow_mcp.py");
        fs::write(
            &script,
            r#"import json
import sys
import time

for raw in sys.stdin:
    message = json.loads(raw)
    if "id" not in message:
        continue
    request_id = message["id"]
    method = message.get("method")
    if method == "initialize":
        result = {"protocolVersion": "2025-11-25", "capabilities": {}, "serverInfo": {"name": "slow", "version": "1"}}
    elif method == "tools/list":
        result = {"tools": [{"name": "slow", "description": "Slow", "inputSchema": {"type": "object", "properties": {}}}]}
    elif method == "tools/call":
        time.sleep(30)
        result = {"content": [{"type": "text", "text": "too late"}]}
    else:
        result = {}
    print(json.dumps({"jsonrpc": "2.0", "id": request_id, "result": result}), flush=True)
"#,
        )
        .expect("write fixture");

        let registry = McpProxyRegistry::default();
        registry
            .configure(
                vec![McpProxyServerSpec {
                    name: "slow".into(),
                    transport: McpProxyTransportSpec::Stdio,
                    command: python.display().to_string(),
                    args: vec![script.display().to_string()],
                    env: BTreeMap::new(),
                    cwd: temp.path().to_path_buf(),
                    tool_prefix: "slow".into(),
                    include_tools: None,
                    exclude_tools: BTreeSet::new(),
                    max_tools: None,
                    exposure_mode: ProxyExposureMode::Full,
                    max_concurrent_requests: 4,
                    request_timeout: Duration::from_secs(5),
                    management_tools: false,
                }],
                "proxy-cancellation-test",
            )
            .await;

        let token = CancellationToken::default();
        let worker_registry = registry.clone();
        let worker_token = token.clone();
        let worker = tokio::spawn(async move {
            worker_registry
                .call_tool_with_cancellation("slow__slow", &json!({}), &worker_token)
                .await
                .expect("known route")
                .expect("cancelled call is a tool result")
        });
        tokio::time::sleep(Duration::from_millis(150)).await;
        token.cancel();
        let error = worker.await.expect("worker");
        assert_eq!(error["isError"], true);
        assert_eq!(
            error["structuredContent"]["error"]["details"]["reason"],
            "proxy_call_cancelled"
        );
        assert_eq!(
            error["structuredContent"]["error"]["details"]["request_replayed"],
            false
        );
        assert_eq!(
            error["structuredContent"]["error"]["details"]["reconnect_scheduled"],
            false
        );
    }

    #[tokio::test]
    async fn stdio_proxy_multiplexes_concurrent_tool_calls() {
        let Ok(python) = which::which("python") else {
            return;
        };
        let temp = tempfile::tempdir().expect("tempdir");
        let script = temp.path().join("concurrent_mcp.py");
        fs::write(
            &script,
            r#"import json
import sys
import threading
import time

output_lock = threading.Lock()

def emit(message):
    with output_lock:
        print(json.dumps(message), flush=True)

def handle(message):
    request_id = message["id"]
    method = message.get("method")
    if method == "initialize":
        result = {"protocolVersion": "2025-11-25", "capabilities": {}, "serverInfo": {"name": "concurrent", "version": "1"}}
    elif method == "tools/list":
        result = {"tools": [{"name": "wait", "description": "Wait", "inputSchema": {"type": "object", "properties": {}}}]}
    elif method == "tools/call":
        time.sleep(0.6)
        result = {"content": [{"type": "text", "text": "done"}]}
    else:
        result = {}
    emit({"jsonrpc": "2.0", "id": request_id, "result": result})

for raw in sys.stdin:
    message = json.loads(raw)
    if "id" not in message:
        continue
    if message.get("method") == "tools/call":
        threading.Thread(target=handle, args=(message,), daemon=True).start()
    else:
        handle(message)
"#,
        )
        .expect("write fixture");

        let registry = McpProxyRegistry::default();
        registry
            .configure(
                vec![McpProxyServerSpec {
                    name: "concurrent".into(),
                    transport: McpProxyTransportSpec::Stdio,
                    command: python.display().to_string(),
                    args: vec![script.display().to_string()],
                    env: BTreeMap::new(),
                    cwd: temp.path().to_path_buf(),
                    tool_prefix: "concurrent".into(),
                    include_tools: None,
                    exclude_tools: BTreeSet::new(),
                    max_tools: None,
                    exposure_mode: ProxyExposureMode::Full,
                    max_concurrent_requests: 2,
                    request_timeout: Duration::from_secs(5),
                    management_tools: false,
                }],
                "proxy-concurrency-test",
            )
            .await;

        let started = Instant::now();
        let first_args = json!({});
        let second_args = json!({});
        let (first, second) = tokio::join!(
            registry.call_tool("concurrent__wait", &first_args),
            registry.call_tool("concurrent__wait", &second_args)
        );
        assert_eq!(
            first.expect("known first route").expect("first result")["structuredContent"]["ok"],
            true
        );
        assert_eq!(
            second.expect("known second route").expect("second result")["structuredContent"]["ok"],
            true
        );
        assert!(
            started.elapsed() < Duration::from_millis(1_050),
            "concurrent calls were serialized: {:?}",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn cancelled_call_keeps_stdio_connection_available_for_followup() {
        let Ok(python) = which::which("python") else {
            return;
        };
        let temp = tempfile::tempdir().expect("tempdir");
        let script = temp.path().join("cancellable_mcp.py");
        let starts = temp.path().join("starts.txt");
        fs::write(
            &script,
            r#"import json
import sys
import threading
import time

starts = sys.argv[1]
with open(starts, "a", encoding="utf-8") as marker:
    marker.write("start\n")

output_lock = threading.Lock()

def emit(message):
    with output_lock:
        print(json.dumps(message), flush=True)

def handle(message):
    request_id = message["id"]
    method = message.get("method")
    if method == "initialize":
        result = {"protocolVersion": "2025-11-25", "capabilities": {}, "serverInfo": {"name": "cancellable", "version": "1"}}
    elif method == "tools/list":
        tool = {"description": "Tool", "inputSchema": {"type": "object", "properties": {}}}
        result = {"tools": [{"name": "slow", **tool}, {"name": "fast", **tool}]}
    elif method == "tools/call":
        name = message.get("params", {}).get("name")
        if name == "slow":
            time.sleep(5)
        result = {"content": [{"type": "text", "text": name or "done"}]}
    else:
        result = {}
    emit({"jsonrpc": "2.0", "id": request_id, "result": result})

for raw in sys.stdin:
    message = json.loads(raw)
    if "id" not in message:
        continue
    if message.get("method") == "tools/call":
        threading.Thread(target=handle, args=(message,), daemon=True).start()
    else:
        handle(message)
"#,
        )
        .expect("write fixture");

        let registry = McpProxyRegistry::default();
        registry
            .configure(
                vec![McpProxyServerSpec {
                    name: "cancellable".into(),
                    transport: McpProxyTransportSpec::Stdio,
                    command: python.display().to_string(),
                    args: vec![script.display().to_string(), starts.display().to_string()],
                    env: BTreeMap::new(),
                    cwd: temp.path().to_path_buf(),
                    tool_prefix: "cancellable".into(),
                    include_tools: None,
                    exclude_tools: BTreeSet::new(),
                    max_tools: None,
                    exposure_mode: ProxyExposureMode::Full,
                    max_concurrent_requests: 2,
                    request_timeout: Duration::from_secs(10),
                    management_tools: false,
                }],
                "proxy-cancel-followup-test",
            )
            .await;

        let cancellation = CancellationToken::default();
        let worker_registry = registry.clone();
        let worker_cancellation = cancellation.clone();
        let slow = tokio::spawn(async move {
            worker_registry
                .call_tool_with_cancellation("cancellable__slow", &json!({}), &worker_cancellation)
                .await
                .expect("known route")
                .expect("cancelled result")
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        cancellation.cancel();
        let cancelled = slow.await.expect("cancel worker");
        assert_eq!(
            cancelled["structuredContent"]["error"]["details"]["reason"],
            "proxy_call_cancelled"
        );

        let started = Instant::now();
        let fast = registry
            .call_tool("cancellable__fast", &json!({}))
            .await
            .expect("known followup route")
            .expect("followup result");
        assert_eq!(fast["structuredContent"]["ok"], true);
        assert!(started.elapsed() < Duration::from_secs(1));
        assert_eq!(
            fs::read_to_string(&starts)
                .expect("start marker")
                .lines()
                .count(),
            1,
            "cancellation restarted the downstream process"
        );
    }

    #[tokio::test]
    async fn reconnects_after_disconnect_without_replaying_failed_tool_call() {
        let Ok(python) = which::which("python") else {
            return;
        };
        let temp = tempfile::tempdir().expect("tempdir");
        let script = temp.path().join("unstable_mcp.py");
        let marker = temp.path().join("first-call-failed");
        fs::write(
            &script,
            r#"import json
import os
import sys

marker = sys.argv[1]
for raw in sys.stdin:
    message = json.loads(raw)
    if "id" not in message:
        continue
    request_id = message["id"]
    method = message.get("method")
    if method == "initialize":
        result = {"protocolVersion": "2025-06-18", "capabilities": {}, "serverInfo": {"name": "unstable", "version": "1"}}
    elif method == "tools/list":
        result = {"tools": [{"name": "ping", "description": "Ping", "inputSchema": {"type": "object", "properties": {}}}]}
    elif method == "tools/call":
        if not os.path.exists(marker):
            open(marker, "w", encoding="utf-8").write("failed-once")
            sys.exit(0)
        result = {"content": [{"type": "text", "text": "reconnected"}], "structuredContent": {"ok": True}}
    else:
        result = {}
    print(json.dumps({"jsonrpc": "2.0", "id": request_id, "result": result}), flush=True)
"#,
        )
        .expect("write fixture");

        let registry = McpProxyRegistry::default();
        registry
            .configure(
                vec![McpProxyServerSpec {
                    name: "unstable".into(),
                    transport: McpProxyTransportSpec::Stdio,
                    command: python.display().to_string(),
                    args: vec![script.display().to_string(), marker.display().to_string()],
                    env: BTreeMap::new(),
                    cwd: temp.path().to_path_buf(),
                    tool_prefix: "unstable".into(),
                    include_tools: None,
                    exclude_tools: BTreeSet::new(),
                    max_tools: None,
                    exposure_mode: ProxyExposureMode::Full,
                    max_concurrent_requests: 4,
                    request_timeout: Duration::from_secs(5),
                    management_tools: false,
                }],
                "proxy-reconnect-test",
            )
            .await;

        let exposed_tools = registry.list_tools();
        let exposed = exposed_tools
            .iter()
            .find(|tool| tool["name"] == "unstable__ping")
            .expect("proxied tool is exposed");
        assert_eq!(exposed["outputSchema"]["required"], json!(["ok"]));

        let first = registry
            .call_tool("unstable__ping", &json!({}))
            .await
            .expect("known route")
            .expect("first failure is a tool result");
        assert_eq!(first["isError"], true);
        assert_eq!(
            first["structuredContent"]["error"]["details"]["request_replayed"],
            false
        );
        assert_eq!(
            first["structuredContent"]["error"]["details"]["reconnect_scheduled"],
            true
        );

        let second = registry
            .call_tool("unstable__ping", &json!({}))
            .await
            .expect("known route")
            .expect("second call reconnects");
        assert_eq!(second["structuredContent"]["ok"], true);
    }

    #[tokio::test]
    async fn reconnect_rejects_a_changed_tool_catalog() {
        let Ok(python) = which::which("python") else {
            return;
        };
        let temp = tempfile::tempdir().expect("tempdir");
        let script = temp.path().join("catalog_drift_mcp.py");
        let marker = temp.path().join("catalog-drift");
        fs::write(
            &script,
            r#"import json
import os
import sys

marker = sys.argv[1]
for raw in sys.stdin:
    message = json.loads(raw)
    if "id" not in message:
        continue
    request_id = message["id"]
    method = message.get("method")
    if method == "initialize":
        result = {"protocolVersion": "2025-11-25", "capabilities": {}, "serverInfo": {"name": "drift", "version": "1"}}
    elif method == "tools/list":
        schema = {"type": "object", "properties": {}}
        if os.path.exists(marker):
            schema = {"type": "object", "properties": {"value": {"type": "string"}}, "required": ["value"]}
        result = {"tools": [{"name": "ping", "inputSchema": schema}]}
    elif method == "tools/call":
        open(marker, "w", encoding="utf-8").write("drift")
        sys.exit(0)
    else:
        result = {}
    print(json.dumps({"jsonrpc": "2.0", "id": request_id, "result": result}), flush=True)
"#,
        )
        .expect("write fixture");

        let registry = McpProxyRegistry::default();
        registry
            .configure(
                vec![McpProxyServerSpec {
                    name: "drift".into(),
                    transport: McpProxyTransportSpec::Stdio,
                    command: python.display().to_string(),
                    args: vec![script.display().to_string(), marker.display().to_string()],
                    env: BTreeMap::new(),
                    cwd: temp.path().to_path_buf(),
                    tool_prefix: "drift".into(),
                    include_tools: None,
                    exclude_tools: BTreeSet::new(),
                    max_tools: None,
                    exposure_mode: ProxyExposureMode::Full,
                    max_concurrent_requests: 4,
                    request_timeout: Duration::from_secs(5),
                    management_tools: false,
                }],
                "proxy-catalog-drift-test",
            )
            .await;

        let first = registry
            .call_tool("drift__ping", &json!({}))
            .await
            .expect("known route")
            .expect("first failure is a tool result");
        assert_eq!(first["isError"], true);
        assert_eq!(
            first["structuredContent"]["error"]["details"]["request_replayed"],
            false
        );

        let second = registry
            .call_tool("drift__ping", &json!({}))
            .await
            .expect("known route")
            .expect("catalog drift is a tool result");
        assert_eq!(second["isError"], true);
        assert_eq!(
            second["structuredContent"]["error"]["details"]["reason"],
            "proxy_reconnect_failed"
        );
        assert!(second["structuredContent"]["error"]["details"]["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("catalog contract changed")));
    }
}
