use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::{Mutex, Notify};
use tokio::task::JoinSet;
use tokio::time::timeout;

use crate::tools::CancellationToken;
use crate::tunnel::append_profile_log;

const DEFAULT_REQUEST_TIMEOUT_SECONDS: u64 = 30;
const MAX_TOOL_LIST_PAGES: usize = 100;
const MAX_DISCOVERED_PROXY_TOOLS_PER_SERVER: usize = 4_096;
const MAX_PROXY_TOOLS_PER_SERVER: usize = 256;
const MAX_PROXY_TOOLS_TOTAL: usize = 512;
const MAX_PROXY_TOOL_DEFINITION_BYTES: usize = 128 * 1024;

#[derive(Debug, Clone)]
struct SanitizedProxyTool {
    public_name: String,
    downstream_name: String,
    definition: Value,
    input_schema: Value,
    output_schema: Value,
    synthesized_output_schema: bool,
}

fn fallback_proxy_output_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "ok": { "type": "boolean" },
            "result": {
                "type": "object",
                "additionalProperties": true
            }
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
}

#[derive(Debug, Clone)]
pub struct McpProxyServerSpec {
    pub name: String,
    command: String,
    args: Vec<String>,
    env: BTreeMap<String, String>,
    cwd: PathBuf,
    tool_prefix: String,
    include_tools: Option<BTreeSet<String>>,
    exclude_tools: BTreeSet<String>,
    max_tools: Option<usize>,
    request_timeout: Duration,
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
    value
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty() && text.len() <= maximum)
        .map(str::to_string)
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
        let title = bounded_text(object.get("title"), 512)
            .unwrap_or_else(|| format!("{} · {}", spec.name, downstream_name));
        let description = bounded_text(object.get("description"), 8192)
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
    client: Mutex<Option<Arc<McpProxyClient>>>,
    reconnect_scheduled: AtomicBool,
}

#[derive(Debug, Clone, Deserialize)]
struct RawMcpServerConfig {
    #[serde(rename = "type", default)]
    transport_type: String,
    command: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    cwd: Option<String>,
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
    #[serde(rename = "requestTimeoutSeconds", default)]
    request_timeout_seconds: Option<u64>,
}

#[derive(Clone)]
struct ProxyRoute {
    server: Arc<ProxyServer>,
    server_name: String,
    downstream_name: String,
    input_schema: Value,
    output_schema: Value,
    synthesized_output_schema: bool,
}

#[derive(Default)]
struct RegistryState {
    tools: Vec<Value>,
    routes: HashMap<String, ProxyRoute>,
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
    request_timeout: Duration,
    connection: Mutex<ProxyConnection>,
}

struct ProxyConnection {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
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

        while let Some(joined) = tasks.join_next().await {
            let Ok((server_name, workspace_id, result)) = joined else {
                append_profile_log(
                    workspace_id,
                    "stderr.log",
                    "[mcp-proxy] initialization task failed",
                );
                continue;
            };
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
                            },
                        );
                        state.tools.push(tool.definition);
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
                            "[mcp-proxy:{server_name}] connected; discovered={} filtered={} max_tools_truncated={} merged={added}",
                            catalog.discovered_count,
                            catalog.filtered_count,
                            catalog.truncated_count
                        ),
                    );
                }
                Err(error) => append_profile_log(
                    &workspace_id,
                    "stderr.log",
                    &format!("[mcp-proxy:{server_name}] unavailable: {error}"),
                ),
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
        let client = match server.ensure_client().await {
            Ok(client) => client,
            Err(message) => {
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
        if let Err(message) = &result {
            let cancelled = message == "request cancelled";
            server.invalidate_client(&client).await;
            if !cancelled {
                server.clone().schedule_reconnect();
            }
            let log_message = if cancelled {
                format!("[mcp-proxy:{server_name}] request cancelled; next call will reconnect")
            } else {
                format!("[mcp-proxy:{server_name}] connection lost; reconnect scheduled: {message}")
            };
            append_profile_log(&server.workspace_id, "stderr.log", &log_message);
        }

        Some(Ok(match result {
            Ok(result) => normalize_proxy_tool_result(
                &server_name,
                public_name,
                result,
                &route.output_schema,
                route.synthesized_output_schema,
            )
            .unwrap_or_else(|message| {
                proxy_call_error_result(
                    &server_name,
                    public_name,
                    "proxy_result_invalid",
                    message,
                    false,
                    false,
                )
            }),
            Err(message) => {
                let cancelled = message == "request cancelled";
                proxy_call_error_result(
                    &server_name,
                    public_name,
                    if cancelled {
                        "proxy_call_cancelled"
                    } else {
                        "proxy_call_failed"
                    },
                    message,
                    !cancelled,
                    !cancelled,
                )
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
        Ok((
            Arc::new(Self {
                spec,
                workspace_id,
                catalog_digest,
                client: Mutex::new(Some(client)),
                reconnect_scheduled: AtomicBool::new(false),
            }),
            catalog,
        ))
    }

    async fn ensure_client(&self) -> Result<Arc<McpProxyClient>, String> {
        let mut client = self.client.lock().await;
        if let Some(current) = client.as_ref() {
            return Ok(current.clone());
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
        append_profile_log(
            &self.workspace_id,
            "stdout.log",
            &format!("[mcp-proxy:{}] reconnected", self.spec.name),
        );
        Ok(connected)
    }

    async fn invalidate_client(&self, failed: &Arc<McpProxyClient>) {
        let mut client = self.client.lock().await;
        if client
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, failed))
        {
            *client = None;
        }
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

fn normalize_proxy_tool_result(
    server_name: &str,
    public_name: &str,
    result: Value,
    output_schema: &Value,
    synthesized_output_schema: bool,
) -> Result<Value, String> {
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
    let is_error = object.get("isError").and_then(Value::as_bool).unwrap_or(false);
    let mut structured = object.get("structuredContent").cloned();
    if structured.as_ref().is_some_and(|value| !value.is_object()) {
        return Err("downstream structuredContent must be an object".into());
    }
    if synthesized_output_schema {
        structured = Some(json!({
            "ok": !is_error,
            "result": structured.unwrap_or_else(|| json!({}))
        }));
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
            connection: Mutex::new(ProxyConnection {
                child,
                stdin,
                stdout: BufReader::new(stdout),
                next_id: 1,
            }),
        });

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
            .await?;
        client
            .notify("notifications/initialized", json!({}))
            .await?;
        let tools = client.list_tools().await?;
        Ok((client, tools))
    }

    async fn list_tools(&self) -> Result<Vec<Value>, String> {
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

        Err("downstream MCP returned too many tools/list pages".to_string())
    }

    async fn request(&self, method: &str, params: Value) -> Result<Value, String> {
        self.request_with_cancellation(method, params, &CancellationToken::default())
            .await
    }

    async fn request_with_cancellation(
        &self,
        method: &str,
        params: Value,
        cancellation: &CancellationToken,
    ) -> Result<Value, String> {
        let result = timeout(self.request_timeout, async {
            let mut connection = tokio::select! {
                _ = cancellation.cancelled() => return Err("request cancelled".to_string()),
                connection = self.connection.lock() => connection,
            };
            let id = connection.next_id;
            connection.next_id += 1;
            let request = json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": method,
                "params": params
            });
            let cancelled = {
                let response = request_over_stdio(&mut connection, id, &request);
                tokio::pin!(response);
                tokio::select! {
                    result = &mut response => return result,
                    _ = cancellation.cancelled() => true,
                }
            };
            if cancelled {
                let _ = connection.child.kill().await;
                let _ = connection.child.wait().await;
                Err("request cancelled".to_string())
            } else {
                unreachable!()
            }
        })
        .await;

        match result {
            Ok(result) => result,
            Err(_) => {
                if let Ok(mut connection) = self.connection.try_lock() {
                    let _ = connection.child.kill().await;
                    let _ = connection.child.wait().await;
                }
                Err(format!(
                    "request `{method}` timed out after {} seconds including queue wait",
                    self.request_timeout.as_secs()
                ))
            }
        }
    }

    async fn notify(&self, method: &str, params: Value) -> Result<(), String> {
        timeout(self.request_timeout, async {
            let mut connection = self.connection.lock().await;
            let notification = json!({
                "jsonrpc": "2.0",
                "method": method,
                "params": params
            });
            let encoded = serde_json::to_vec(&notification)
                .map_err(|error| format!("failed to encode downstream notification: {error}"))?;
            connection
                .stdin
                .write_all(&encoded)
                .await
                .map_err(|error| format!("failed to write downstream notification: {error}"))?;
            connection
                .stdin
                .write_all(b"\n")
                .await
                .map_err(|error| format!("failed to terminate downstream notification: {error}"))?;
            connection
                .stdin
                .flush()
                .await
                .map_err(|error| format!("failed to flush downstream notification: {error}"))
        })
        .await
        .map_err(|_| format!("notification `{method}` timed out including queue wait"))?
    }
}

async fn request_over_stdio(
    connection: &mut ProxyConnection,
    id: u64,
    request: &Value,
) -> Result<Value, String> {
    let encoded = serde_json::to_vec(request)
        .map_err(|error| format!("failed to encode downstream request: {error}"))?;
    connection
        .stdin
        .write_all(&encoded)
        .await
        .map_err(|error| format!("failed to write downstream request: {error}"))?;
    connection
        .stdin
        .write_all(b"\n")
        .await
        .map_err(|error| format!("failed to terminate downstream request: {error}"))?;
    connection
        .stdin
        .flush()
        .await
        .map_err(|error| format!("failed to flush downstream request: {error}"))?;

    loop {
        let mut line = String::new();
        let bytes = connection
            .stdout
            .read_line(&mut line)
            .await
            .map_err(|error| format!("failed to read downstream response: {error}"))?;
        if bytes == 0 {
            return Err("downstream MCP closed stdout".to_string());
        }
        let Ok(message) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        if message.get("method").and_then(Value::as_str).is_some() {
            if let Some(server_request_id) = message.get("id").cloned() {
                let rejection = json!({
                    "jsonrpc": "2.0",
                    "id": server_request_id,
                    "error": {
                        "code": -32601,
                        "message": "Downstream server requests are not supported by this proxy"
                    }
                });
                let encoded = serde_json::to_vec(&rejection)
                    .map_err(|error| format!("failed to encode downstream rejection: {error}"))?;
                connection
                    .stdin
                    .write_all(&encoded)
                    .await
                    .map_err(|error| format!("failed to reject downstream request: {error}"))?;
                connection.stdin.write_all(b"\n").await.map_err(|error| {
                    format!("failed to terminate downstream rejection: {error}")
                })?;
                connection
                    .stdin
                    .flush()
                    .await
                    .map_err(|error| format!("failed to flush downstream rejection: {error}"))?;
            }
            continue;
        }
        if message.get("id").and_then(Value::as_u64) != Some(id) {
            continue;
        }
        if let Some(error) = message.get("error") {
            return Err(error.to_string());
        }
        return message
            .get("result")
            .cloned()
            .ok_or_else(|| "downstream response is missing result".to_string());
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
        if !config.transport_type.is_empty() && config.transport_type != "stdio" {
            return Err(format!(
                "MCP server `{name}` uses unsupported transport `{}`; only stdio is supported",
                config.transport_type
            ));
        }
        if config.command.trim().is_empty() {
            return Err(format!("MCP server `{name}` is missing command"));
        }

        let workspace_display = workspace_path.display().to_string();
        let command = expand_workspace_placeholders(&config.command, &workspace_display);
        let args = config
            .args
            .into_iter()
            .map(|arg| expand_workspace_placeholders(&arg, &workspace_display))
            .collect();
        let env = config
            .env
            .into_iter()
            .map(|(key, value)| {
                (
                    key,
                    expand_workspace_placeholders(&value, &workspace_display),
                )
            })
            .collect();
        let cwd = config
            .cwd
            .map(|cwd| expand_workspace_placeholders(&cwd, &workspace_display))
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
        if config
            .max_tools
            .is_some_and(|maximum| maximum > MAX_PROXY_TOOLS_PER_SERVER)
        {
            return Err(format!(
                "MCP server `{name}` maxTools must be at most {MAX_PROXY_TOOLS_PER_SERVER}"
            ));
        }

        specs.push(McpProxyServerSpec {
            name,
            command,
            args,
            env,
            cwd,
            tool_prefix,
            include_tools,
            exclude_tools,
            max_tools: config.max_tools,
            request_timeout: Duration::from_secs(
                config
                    .request_timeout_seconds
                    .unwrap_or(DEFAULT_REQUEST_TIMEOUT_SECONDS)
                    .clamp(1, 600),
            ),
        });
    }
    Ok(specs)
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
    use std::time::Duration;

    use serde_json::json;

    use crate::tools::CancellationToken;

    use super::{
        normalize_proxy_tool_result, parse_mcp_proxy_config, proxy_catalog_digest,
        sanitize_proxy_catalog, McpProxyRegistry, McpProxyServerSpec,
    };

    fn test_spec() -> McpProxyServerSpec {
        McpProxyServerSpec {
            name: "test".into(),
            command: "noop".into(),
            args: Vec::new(),
            env: BTreeMap::new(),
            cwd: Path::new(".").to_path_buf(),
            tool_prefix: "test".into(),
            include_tools: None,
            exclude_tools: BTreeSet::new(),
            max_tools: None,
            request_timeout: Duration::from_secs(5),
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
            "test__click",
            json!({
                "content": [{"type": "text", "text": "clicked"}]
            }),
            &tool.output_schema,
            tool.synthesized_output_schema,
        )
        .expect("normalized result");

        assert_eq!(normalized["structuredContent"]["ok"], true);
        assert_eq!(normalized["structuredContent"]["result"], json!({}));
        assert_eq!(normalized["isError"], false);
        assert!(jsonschema::validator_for(&tool.output_schema)
            .expect("fallback output schema")
            .validate(&normalized["structuredContent"])
            .is_ok());
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
                        "maxTools": 2
                    }
                }
            }"#,
            Path::new("/tmp/example"),
        )
        .expect("parse selection controls");

        let spec = &specs[0];
        assert_eq!(spec.max_tools, Some(2));
        assert!(spec
            .include_tools
            .as_ref()
            .is_some_and(|tools| tools.contains("navigate") && tools.contains("click")));
        assert!(spec.exclude_tools.contains("screenshot"));
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
    }

    #[test]
    fn rejects_non_stdio_transports() {
        let error = parse_mcp_proxy_config(
            r#"{"mcpServers":{"remote":{"type":"sse","command":"noop"}}}"#,
            Path::new("/tmp/example"),
        )
        .expect_err("reject unsupported transport");

        assert!(error.contains("only stdio is supported"));
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
                    command: python.display().to_string(),
                    args: vec![script.display().to_string()],
                    env: BTreeMap::new(),
                    cwd: temp.path().to_path_buf(),
                    tool_prefix: "slow".into(),
                    include_tools: None,
                    exclude_tools: BTreeSet::new(),
                    max_tools: None,
                    request_timeout: Duration::from_secs(5),
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
                    command: python.display().to_string(),
                    args: vec![script.display().to_string(), marker.display().to_string()],
                    env: BTreeMap::new(),
                    cwd: temp.path().to_path_buf(),
                    tool_prefix: "unstable".into(),
                    include_tools: None,
                    exclude_tools: BTreeSet::new(),
                    max_tools: None,
                    request_timeout: Duration::from_secs(5),
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
                    command: python.display().to_string(),
                    args: vec![script.display().to_string(), marker.display().to_string()],
                    env: BTreeMap::new(),
                    cwd: temp.path().to_path_buf(),
                    tool_prefix: "drift".into(),
                    include_tools: None,
                    exclude_tools: BTreeSet::new(),
                    max_tools: None,
                    request_timeout: Duration::from_secs(5),
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
