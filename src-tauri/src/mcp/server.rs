use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::tools::dispatch::call_tool_prevalidated_with_session_cancellation;
use crate::tools::workspace::{tool_err, tool_ok, WorkspaceError};
use crate::tools::{
    build_effective_catalog, wrap_mcp_tool_result, CancellationToken, EffectiveCatalog,
    SharedToolContext, ToolContext, Workspace,
};
use crate::workspace::AuthConfig;

pub type SharedState = SharedToolContext;

const TOOLS_LIST_PAGE_MAX_TOOLS: usize = 64;
const TOOLS_LIST_PAGE_MAX_BYTES: usize = 192 * 1024;
const TOOLS_LIST_CURSOR_PREFIX: &str = "anchor-v1";
const MAX_BROWSER_ARTIFACTS: usize = 256;
const BROWSER_BUILD_PROBE_JS: &str = r#"async () => {
  const firstString = (...values) => values.find((value) => typeof value === 'string' && value.trim()) || null;
  const meta = (names) => {
    for (const name of names) {
      const node = document.querySelector(`meta[name="${name}"],meta[property="${name}"]`);
      const value = node?.getAttribute('content');
      if (value) return value;
    }
    return null;
  };
  const root = document.documentElement?.dataset || {};
  const globals = globalThis;
  const buildHash = firstString(
    globals.__BUILD_HASH__, globals.__BUILD_ID__, globals.__NEXT_DATA__?.buildId,
    root.buildHash, root.buildId, meta(['build-hash', 'build-id', 'x-build-hash'])
  );
  const gitCommit = firstString(
    globals.__GIT_COMMIT__, globals.__COMMIT_SHA__, globals.__REVISION__,
    root.gitCommit, root.commitSha, meta(['git-commit', 'commit-sha', 'revision'])
  );
  const appVersion = firstString(
    globals.__APP_VERSION__, root.appVersion, root.version,
    meta(['app-version', 'version', 'application-version'])
  );
  const assetUrls = [...document.scripts, ...document.querySelectorAll('link[href]')]
    .map((node) => node.src || node.href)
    .filter(Boolean)
    .slice(0, 200);
  const assetHashes = [...new Set(assetUrls.flatMap((url) => {
    const file = url.split(/[?#]/, 1)[0].split('/').pop() || '';
    return [...file.matchAll(/(?:^|[._-])([a-f0-9]{7,64})(?=[._-]|$)/ig)].map((match) => match[1]);
  }))].slice(0, 64);
  const registrations = 'serviceWorker' in navigator
    ? await navigator.serviceWorker.getRegistrations()
    : [];
  const cacheNames = 'caches' in globalThis ? await caches.keys() : [];
  return {
    href: location.href,
    origin: location.origin,
    title: document.title,
    build_hash: buildHash,
    git_commit: gitCommit,
    app_version: appVersion,
    asset_hashes: assetHashes,
    asset_urls: assetUrls,
    service_workers: registrations.map((registration) => ({
      scope: registration.scope,
      active_script_url: registration.active?.scriptURL || null,
      waiting_script_url: registration.waiting?.scriptURL || null,
      installing_script_url: registration.installing?.scriptURL || null
    })),
    cache_names: cacheNames,
    detected_at: new Date().toISOString()
  };
}"#;

#[derive(Debug, Clone)]
struct BrowserArtifactTarget {
    relative_path: String,
    absolute_path: PathBuf,
    direction: &'static str,
    kind: &'static str,
}

#[cfg(test)]
pub async fn handle_request(state: &SharedState, body: &Value) -> Value {
    if body.get("method").and_then(Value::as_str) == Some("initialize") {
        let requested = match crate::mcp::protocol::requested_protocol_version(body) {
            Ok(version) => version,
            Err(error) => {
                return serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": body.get("id").cloned().unwrap_or(Value::Null),
                    "error": error
                })
            }
        };
        if let Err(error) = crate::mcp::protocol::require_current_protocol_version(requested) {
            return serde_json::json!({
                "jsonrpc": "2.0",
                "id": body.get("id").cloned().unwrap_or(Value::Null),
                "error": error
            });
        }
    }
    handle_request_with_protocol(state, body, crate::mcp::protocol::CURRENT_PROTOCOL_VERSION).await
}

fn proxy_operation_summary(
    name: &str,
    session_id: Option<&str>,
    result: &Result<Value, Value>,
) -> Value {
    match result {
        Ok(value) => {
            let structured = value.get("structuredContent").unwrap_or(&Value::Null);
            let error = structured.get("error");
            serde_json::json!({
                "ok": structured.get("ok").and_then(Value::as_bool).unwrap_or_else(|| {
                    value.get("isError").and_then(Value::as_bool) != Some(true)
                }),
                "tool": name,
                "session_id": session_id,
                "error_code": error.and_then(|value| value.get("code")),
                "error_message": error.and_then(|value| value.get("message")),
                "retryable": error.and_then(|value| value.get("retryable")),
                "error_details": error.and_then(|value| value.get("details")),
                "duration_ms": structured.get("duration_ms"),
                "source": "mcp_proxy"
            })
        }
        Err(error) => serde_json::json!({
            "ok": false,
            "tool": name,
            "session_id": session_id,
            "error_code": error.get("code"),
            "error_message": error.get("message"),
            "error_details": error.get("data"),
            "duration_ms": null,
            "source": "mcp_proxy"
        }),
    }
}

fn tools_list_result(catalog: &EffectiveCatalog, params: &Value) -> Result<Value, Value> {
    let start = tools_list_cursor_offset(params, &catalog.digest, catalog.tools.len())?;
    let mut end = start;
    let mut page_bytes = 0usize;
    while end < catalog.tools.len() && end.saturating_sub(start) < TOOLS_LIST_PAGE_MAX_TOOLS {
        let tool_bytes = serde_json::to_vec(&catalog.tools[end])
            .map_err(|error| {
                serde_json::json!({
                    "code": -32603,
                    "message": "Failed to serialize MCP tool catalog page",
                    "data": {
                        "reason": "catalog_page_serialization_failed",
                        "detail": error.to_string()
                    }
                })
            })?
            .len();
        if end > start && page_bytes.saturating_add(tool_bytes) > TOOLS_LIST_PAGE_MAX_BYTES {
            break;
        }
        page_bytes = page_bytes.saturating_add(tool_bytes);
        end += 1;
    }

    let mut result = serde_json::json!({
        "tools": catalog.tools[start..end].to_vec(),
        "_meta": {
            "anchor/catalog": catalog.metrics_value(),
            "anchor/page": {
                "start": start,
                "end": end,
                "page_tool_count": end.saturating_sub(start),
                "page_bytes": page_bytes,
                "max_page_tools": TOOLS_LIST_PAGE_MAX_TOOLS,
                "max_page_bytes": TOOLS_LIST_PAGE_MAX_BYTES
            }
        }
    });
    if end < catalog.tools.len() {
        result["nextCursor"] = Value::String(format!(
            "{TOOLS_LIST_CURSOR_PREFIX}:{}:{end}",
            catalog.digest
        ));
    }
    Ok(result)
}

fn tools_list_cursor_offset(
    params: &Value,
    expected_digest: &str,
    tool_count: usize,
) -> Result<usize, Value> {
    let Some(cursor) = params.get("cursor") else {
        return Ok(0);
    };
    let Some(cursor) = cursor.as_str().filter(|cursor| !cursor.is_empty()) else {
        return Err(invalid_tools_cursor("cursor must be a non-empty string"));
    };
    let mut parts = cursor.split(':');
    let prefix = parts.next();
    let digest = parts.next();
    let offset = parts.next();
    if prefix != Some(TOOLS_LIST_CURSOR_PREFIX)
        || digest != Some(expected_digest)
        || parts.next().is_some()
    {
        return Err(invalid_tools_cursor(
            "cursor is invalid or belongs to a different tool catalog",
        ));
    }
    let offset = offset
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|offset| *offset <= tool_count)
        .ok_or_else(|| invalid_tools_cursor("cursor offset is invalid"))?;
    Ok(offset)
}

fn invalid_tools_cursor(detail: &str) -> Value {
    serde_json::json!({
        "code": -32602,
        "message": "Invalid tools/list cursor",
        "data": {
            "reason": "invalid_tools_list_cursor",
            "detail": detail
        }
    })
}

fn effective_catalog_error(error: crate::tools::workspace::WorkspaceError) -> Value {
    let data = error.to_error_value();
    let budget_exceeded = data.get("code").and_then(Value::as_str)
        == Some("EFFECTIVE_CATALOG_CHATGPT_BUDGET_EXCEEDED");
    serde_json::json!({
        "code": if budget_exceeded { -32004 } else { -32603 },
        "message": if budget_exceeded {
            "Anchor MCP tool catalog exceeds the ChatGPT compatibility budget. Reduce downstream tools with includeTools, excludeTools, or maxTools, then restart Anchor and refresh or recreate the ChatGPT app."
        } else {
            "Failed to build effective MCP tool catalog"
        },
        "data": data
    })
}

#[cfg(test)]
pub async fn handle_request_with_protocol(
    state: &SharedState,
    body: &Value,
    protocol_version: &str,
) -> Value {
    handle_request_with_protocol_and_cancellation(
        state,
        body,
        protocol_version,
        &CancellationToken::default(),
    )
    .await
}

#[cfg(test)]
pub async fn handle_request_with_protocol_and_cancellation(
    state: &SharedState,
    body: &Value,
    protocol_version: &str,
    cancellation: &CancellationToken,
) -> Value {
    handle_request_with_protocol_session_and_cancellation(
        state,
        body,
        protocol_version,
        cancellation,
        None,
    )
    .await
}

pub async fn handle_request_with_protocol_session_and_cancellation(
    state: &SharedState,
    body: &Value,
    protocol_version: &str,
    cancellation: &CancellationToken,
    session_id: Option<&str>,
) -> Value {
    let method = body.get("method").and_then(Value::as_str).unwrap_or("");
    let id = body.get("id").cloned().unwrap_or(Value::Null);
    let params = body.get("params").cloned().unwrap_or(Value::Null);

    if id.is_null() && method.starts_with("notifications/") {
        return Value::Null;
    }

    let result = match method {
        "initialize" => Ok(initialize_result_for_version(state, protocol_version)),
        "ping" => Ok(serde_json::json!({})),
        "tools/list" => {
            if !state
                .mcp_proxies
                .wait_until_configured(Duration::from_secs(70))
                .await
            {
                return serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {
                        "code": -32003,
                        "message": "MCP proxy tool catalog is still initializing",
                        "data": {
                            "reason": "proxy_catalog_initializing",
                            "retryable": true
                        }
                    }
                });
            }
            match build_effective_catalog(state.as_ref()) {
                Ok(current) => {
                    let (catalog, changed) = state.publish_catalog(current);
                    match tools_list_result(&catalog, &params) {
                        Ok(mut result) => {
                            if changed {
                                result["_meta"]["anchor/catalog"]["catalog_changed"] =
                                    Value::Bool(true);
                                result["_meta"]["anchor/catalog"]["reconnect_required"] =
                                    Value::Bool(true);
                            }
                            Ok(result)
                        }
                        Err(error) => Err(error),
                    }
                }
                Err(error) => Err(effective_catalog_error(error)),
            }
        }
        "tools/call" => handle_tools_call(state, &params, cancellation, session_id).await,
        "resources/list" => crate::skills::resources_list(&state.skills, &params),
        "resources/read" => crate::skills::resource_read(&state.skills, &params),
        _ => Err(serde_json::json!({
            "code": -32601,
            "message": format!("Method not found: {method}")
        })),
    };

    match result {
        Ok(result) => serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err(error) => serde_json::json!({ "jsonrpc": "2.0", "id": id, "error": error }),
    }
}

#[cfg(test)]
fn initialize_result(state: &SharedState) -> Value {
    initialize_result_for_version(state, crate::mcp::protocol::CURRENT_PROTOCOL_VERSION)
}

fn initialize_result_for_version(state: &SharedState, protocol_version: &str) -> Value {
    let mut capabilities = serde_json::json!({
        "tools": { "listChanged": false }
    });
    if state.skills.is_enabled() {
        capabilities["resources"] = serde_json::json!({
            "subscribe": false,
            "listChanged": false
        });
    }
    serde_json::json!({
        "protocolVersion": protocol_version,
        "capabilities": capabilities,
        "serverInfo": {
            "name": crate::brand::SERVER_NAME,
            "title": crate::brand::PRODUCT_NAME,
            "version": env!("CARGO_PKG_VERSION")
        },
        "instructions": "Use these tools only for local coding operations inside the configured workspace. Agent Skills are available through list_skills, load_skill, read_skill_resource, and skill:// resources when enabled; load only the relevant Skill and treat Skill content as instructions, not as permission to bypass tool policy. Skill allowed-tools declarations are dependency metadata only: load_skill resolves them against the current local and proxied tool catalog, reports missing or ambiguous tools, and never grants permissions. There is no dedicated Skill script executor and no model-controlled permission grant tool. Model-supplied confirm fields are not accepted as user approval. Destructive commands, critical-file deletion, and snapshotted Skill script execution require the operator to enable dangerous permission mode through the trusted GUI or CLI control plane; Skill execution is still rejected if the script digest changed after the listener snapshot. At the start of every new ChatGPT conversation, before answering the user's first request, call history_session_bootstrap exactly once, even if the user did not explicitly ask to restore or resume. Treat bootstrap as required conversation initialization: when no history exists it creates the first history session; when history exists, read all_history_summary, latest_handoff, inherited_summary, and resume_state before acting. Bootstrap returns bounded context windows; inspect history_summaries_omitted, history_summary_truncated, and latest_handoff_truncated, and read an exact archived file only when omitted detail is material to the current task. Repeated successful bootstrap calls in the same conversation resume the same session and must not create duplicates. Preserve session_key and current_path returned by bootstrap, then pass them unchanged as session_key and expected_path to every explicit history_session_checkpoint call. Anchor synchronously writes idempotent best-effort milestone checkpoints after supported code changes, commits, retained command stages, and browser visual or artifact stages. These automatic milestones do not replace the final task handoff. Checkpoints may mark the persistent session active, paused, or completed; bootstrapping the same paused/completed session reactivates it without discarding checkpoints. After completing each user-requested task in the conversation, call history_session_checkpoint before the final response. Only state that final progress was saved after checkpoint returns ok=true with the same session_key and path."
    })
}

async fn handle_tools_call(
    state: &SharedState,
    params: &Value,
    cancellation: &CancellationToken,
    session_id: Option<&str>,
) -> Result<Value, Value> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| serde_json::json!({ "code": -32602, "message": "Missing tool name" }))?;
    let raw_args = raw_tool_arguments(params);

    if state.is_published_tool(name) == Some(false) {
        return Err(serde_json::json!({
            "code": -32005,
            "message": format!("Tool {name} was not published for this MCP connection"),
            "data": {
                "reason": "catalog_changed",
                "catalog_changed": true,
                "reconnect_required": true
            }
        }));
    }

    if crate::skills::is_skill_tool(name) && !state.skills.is_enabled() {
        return Err(serde_json::json!({
            "code": -32602,
            "message": "Skill service is disabled for this workspace/profile",
            "data": { "reason": "skill_service_disabled" }
        }));
    }
    if matches!(name, "list_skills" | "load_skill")
        && !state
            .mcp_proxies
            .wait_until_configured(Duration::from_secs(70))
            .await
    {
        return Err(serde_json::json!({
            "code": -32003,
            "message": "MCP proxy tool catalog is still initializing",
            "data": {
                "reason": "proxy_catalog_initializing",
                "retryable": true
            }
        }));
    }

    if state.mcp_proxies.contains_tool(name) {
        if let Err(error) = state
            .harness
            .resume_paused_task_for_activity(name, session_id)
        {
            return Err(serde_json::json!({
                "code": -32603,
                "message": "Failed to resume paused Harness task for proxy activity",
                "data": {
                    "reason": "task_auto_resume_failed",
                    "harness_error": error.code(),
                    "details": error.to_string()
                }
            }));
        }
        let active_task = state.harness.current_task().ok().flatten();
        let (proxy_args, artifact_targets) =
            match prepare_browser_workspace_arguments(&state.workspace, name, &raw_args) {
                Ok(prepared) => prepared,
                Err(error) => return Ok(browser_workspace_path_error(name, error)),
            };
        let operation = state
            .harness
            .record_operation(
                None,
                active_task.as_ref().map(|task| task.id.as_str()),
                session_id,
                name,
                "started",
                serde_json::json!({
                    "arguments": raw_args,
                    "source": "mcp_proxy",
                    "session_id": session_id
                }),
                serde_json::json!({"ok": true}),
            )
            .ok();
        if let Some(mut result) = state
            .mcp_proxies
            .call_tool_with_cancellation(name, &proxy_args, cancellation)
            .await
        {
            if let Ok(value) = &mut result {
                attach_browser_workspace_artifacts(
                    state.workspace.root(),
                    &artifact_targets,
                    value,
                );
                attach_browser_build_info(state.as_ref(), name, value, cancellation).await;
                attach_proxy_auto_checkpoint(state.as_ref(), name, &proxy_args, value);
            }
            if let Some(operation) = operation {
                if let Ok(value) = &mut result {
                    if let Some(structured) = value
                        .get_mut("structuredContent")
                        .and_then(Value::as_object_mut)
                    {
                        structured
                            .insert("operation_id".into(), Value::String(operation.id.clone()));
                        structured.insert("trace_id".into(), Value::String(operation.id.clone()));
                    }
                }
                let summary = proxy_operation_summary(name, session_id, &result);
                let succeeded = summary.get("ok").and_then(Value::as_bool) == Some(true);
                let _ = state.harness.record_operation(
                    Some(&operation.id),
                    active_task.as_ref().map(|task| task.id.as_str()),
                    session_id,
                    name,
                    if succeeded { "completed" } else { "failed" },
                    serde_json::json!({
                        "arguments": raw_args,
                        "source": "mcp_proxy",
                        "session_id": session_id
                    }),
                    summary.clone(),
                );
                if let Some(task) = active_task.as_ref() {
                    let _ = state.harness.record_event(
                        &task.id,
                        "proxy_operation_finished",
                        Some(name),
                        serde_json::json!({"session_id": session_id}),
                        summary,
                    );
                }
            }
            return result;
        }
    }

    if matches!(name, "browser_build_info" | "browser_wait_for_build") {
        if let Err(error) = crate::tools::schema::validate_tool_input(name, &raw_args) {
            return Ok(wrap_mcp_tool_result(name, &raw_args, tool_err(error)));
        }
        let mut structured = if name == "browser_build_info" {
            browser_build_info(state.as_ref(), cancellation).await
        } else {
            browser_wait_for_build(state.as_ref(), &raw_args, cancellation).await
        };
        attach_local_browser_checkpoint(state.as_ref(), name, &raw_args, &mut structured);
        return Ok(wrap_mcp_tool_result(name, &raw_args, structured));
    }

    let known = crate::tools::registry::exposed_tool_names(&state.tool_profile);
    if !known.contains(&name) {
        let catalog_changed = state
            .published_catalog()
            .zip(build_effective_catalog(state.as_ref()).ok())
            .is_some_and(|(published, current)| published.digest != current.digest);
        if catalog_changed {
            return Err(serde_json::json!({
                "code": -32005,
                "message": format!("Tool {name} is no longer available in the current catalog"),
                "data": {
                    "reason": "catalog_changed",
                    "catalog_changed": true,
                    "reconnect_required": true
                }
            }));
        }
        return Err(serde_json::json!({
            "code": -32602,
            "message": format!("Unknown tool: {name}"),
            "data": { "reason": "unknown_tool" }
        }));
    }

    if let Err(error) = crate::tools::schema::validate_tool_input(name, &raw_args) {
        return Ok(wrap_mcp_tool_result(name, &raw_args, tool_err(error)));
    }

    let args = tool_arguments(name, params);

    let state = state.clone();
    let call_name = name.to_string();
    let call_args = args.clone();
    let cancellation = cancellation.clone();
    let session_id = session_id.map(str::to_string);
    let structured = tokio::task::spawn_blocking(move || {
        call_tool_prevalidated_with_session_cancellation(
            state.as_ref(),
            &call_name,
            &call_args,
            &cancellation,
            session_id.as_deref(),
        )
    })
    .await
    .map_err(|error| {
        serde_json::json!({
            "code": -32603,
            "message": "Local MCP tool worker failed",
            "data": {
                "reason": "worker_failed",
                "detail": error.to_string()
            }
        })
    })?;
    Ok(wrap_mcp_tool_result(name, &raw_args, structured))
}

fn raw_tool_arguments(params: &Value) -> Value {
    params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}))
}

fn tool_arguments(name: &str, params: &Value) -> Value {
    let mut args = raw_tool_arguments(params);
    if name.starts_with("history_session_") || name == "begin_work_session" {
        if let Some(session_key) = params
            .get("_meta")
            .and_then(|meta| meta.get("openai/session"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            if !args.is_object() {
                args = serde_json::json!({});
            }
            args["_host_session_key"] = Value::String(session_key.to_string());
        }
    }
    args
}

fn prepare_browser_workspace_arguments(
    workspace: &Workspace,
    tool_name: &str,
    arguments: &Value,
) -> Result<(Value, Vec<BrowserArtifactTarget>), WorkspaceError> {
    if !is_browser_file_tool(tool_name) || !arguments.is_object() {
        return Ok((arguments.clone(), Vec::new()));
    }
    let mut prepared = arguments.clone();
    let mut targets = Vec::new();
    let normalized_name = tool_name.to_ascii_lowercase();

    if let Some(raw) = arguments.get("outputDirPath").and_then(Value::as_str) {
        if !Path::new(raw).is_absolute() {
            let target = resolve_browser_output_directory(workspace, raw)?;
            prepared["outputDirPath"] = Value::String(browser_os_path(&target.absolute_path));
            targets.push(target);
        }
    }

    if let Some(raw) = arguments.get("filePath").and_then(Value::as_str) {
        if !Path::new(raw).is_absolute() {
            let target = if normalized_name.contains("upload") {
                resolve_browser_input_file(workspace, raw)?
            } else {
                resolve_browser_output_file(workspace, raw)?
            };
            prepared["filePath"] = Value::String(browser_os_path(&target.absolute_path));
            targets.push(target);
        }
    }

    if normalized_name.contains("upload") {
        for key in ["filePaths", "paths"] {
            let Some(paths) = arguments.get(key).and_then(Value::as_array) else {
                continue;
            };
            let mut resolved_paths = Vec::with_capacity(paths.len());
            for path in paths {
                let Some(raw) = path.as_str() else {
                    resolved_paths.push(path.clone());
                    continue;
                };
                if Path::new(raw).is_absolute() {
                    resolved_paths.push(Value::String(raw.to_string()));
                    continue;
                }
                let target = resolve_browser_input_file(workspace, raw)?;
                resolved_paths.push(Value::String(browser_os_path(&target.absolute_path)));
                targets.push(target);
            }
            prepared[key] = Value::Array(resolved_paths);
        }
    }

    Ok((prepared, targets))
}

fn is_browser_file_tool(tool_name: &str) -> bool {
    let normalized = tool_name.to_ascii_lowercase();
    normalized.contains("browser")
        || normalized.contains("chrome")
        || [
            "take_screenshot",
            "take_snapshot",
            "lighthouse_audit",
            "take_heapsnapshot",
            "upload_file",
        ]
        .iter()
        .any(|suffix| normalized.ends_with(suffix))
}

fn resolve_browser_output_file(
    workspace: &Workspace,
    raw: &str,
) -> Result<BrowserArtifactTarget, WorkspaceError> {
    workspace.reject_unsafe_text(raw)?;
    workspace.reject_protected_write_path(raw)?;
    workspace.reject_write_symlink(raw)?;
    let resolved = workspace.resolve_for_write(raw)?;
    if let Some(parent) = resolved.path.parent() {
        fs::create_dir_all(parent).map_err(|error| WorkspaceError::ToolDetails {
            code: "BROWSER_ARTIFACT_DIRECTORY_FAILED",
            message: format!("Failed to create browser artifact directory: {error}"),
            category: "runtime",
            retryable: true,
            details: serde_json::json!({
                "path": raw,
                "stage": "browser_workspace_bridge"
            }),
        })?;
    }
    Ok(BrowserArtifactTarget {
        relative_path: raw.replace('\\', "/"),
        absolute_path: resolved.path,
        direction: "output",
        kind: "file",
    })
}

fn resolve_browser_output_directory(
    workspace: &Workspace,
    raw: &str,
) -> Result<BrowserArtifactTarget, WorkspaceError> {
    workspace.reject_unsafe_text(raw)?;
    workspace.reject_protected_write_path(raw)?;
    let relative_path = raw.trim_end_matches(['/', '\\']);
    let probe = if relative_path.is_empty() || relative_path == "." {
        ".anchor-browser-artifact-probe".to_string()
    } else {
        format!("{relative_path}/.anchor-browser-artifact-probe")
    };
    let resolved_probe = workspace.resolve_for_write(&probe)?;
    let directory = resolved_probe
        .path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| workspace.root().to_path_buf());
    fs::create_dir_all(&directory).map_err(|error| WorkspaceError::ToolDetails {
        code: "BROWSER_ARTIFACT_DIRECTORY_FAILED",
        message: format!("Failed to create browser artifact directory: {error}"),
        category: "runtime",
        retryable: true,
        details: serde_json::json!({
            "path": raw,
            "stage": "browser_workspace_bridge"
        }),
    })?;
    Ok(BrowserArtifactTarget {
        relative_path: if relative_path.is_empty() {
            ".".into()
        } else {
            relative_path.replace('\\', "/")
        },
        absolute_path: directory,
        direction: "output",
        kind: "directory",
    })
}

fn resolve_browser_input_file(
    workspace: &Workspace,
    raw: &str,
) -> Result<BrowserArtifactTarget, WorkspaceError> {
    let resolved = workspace.resolve_existing(raw)?;
    if !resolved.path.is_file() {
        return Err(WorkspaceError::Tool {
            code: "BROWSER_UPLOAD_NOT_FILE",
            message: format!("Browser upload path is not a file: {raw}"),
            category: "validation",
            retryable: false,
        });
    }
    Ok(BrowserArtifactTarget {
        relative_path: resolved.display,
        absolute_path: resolved.path,
        direction: "input",
        kind: "file",
    })
}

fn browser_os_path(path: &Path) -> String {
    let display = path.to_string_lossy();
    #[cfg(windows)]
    {
        display
            .strip_prefix("\\\\?\\")
            .unwrap_or(&display)
            .to_string()
    }
    #[cfg(not(windows))]
    display.into_owned()
}

fn browser_workspace_path_error(tool_name: &str, error: WorkspaceError) -> Value {
    let error_value = error.to_error_value();
    let code = error_value
        .get("code")
        .and_then(Value::as_str)
        .unwrap_or("BROWSER_WORKSPACE_PATH_INVALID");
    let message = error_value
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("Browser workspace path is invalid");
    let retryable = error_value
        .get("retryable")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let structured = serde_json::json!({
        "ok": false,
        "status": "error",
        "server": "anchor",
        "connection": {},
        "error": error_value,
        "error_code": code,
        "error_message": message,
        "retryable": retryable,
        "browser_session_id": null,
        "connection_status": "not_called",
        "page_count": 0,
        "pages": [],
        "selected_page": null,
        "workspace_bridge": {
            "tool": tool_name,
            "status": "rejected"
        }
    });
    serde_json::json!({
        "content": [{"type": "text", "text": structured.to_string()}],
        "structuredContent": structured,
        "isError": true
    })
}

fn attach_browser_workspace_artifacts(
    workspace_root: &Path,
    targets: &[BrowserArtifactTarget],
    result: &mut Value,
) {
    if targets.is_empty() {
        return;
    }
    let mut artifacts = Vec::new();
    for target in targets {
        if artifacts.len() >= MAX_BROWSER_ARTIFACTS {
            break;
        }
        artifacts.push(browser_artifact_descriptor(
            &target.relative_path,
            &target.absolute_path,
            target.direction,
            target.kind,
        ));
        if target.kind == "directory" && target.absolute_path.is_dir() {
            for entry in walkdir::WalkDir::new(&target.absolute_path)
                .min_depth(1)
                .max_depth(8)
                .follow_links(false)
                .into_iter()
                .filter_map(Result::ok)
            {
                if artifacts.len() >= MAX_BROWSER_ARTIFACTS || !entry.file_type().is_file() {
                    continue;
                }
                let relative = entry
                    .path()
                    .strip_prefix(workspace_root)
                    .map(|path| path.to_string_lossy().replace('\\', "/"))
                    .unwrap_or_else(|_| target.relative_path.clone());
                artifacts.push(browser_artifact_descriptor(
                    &relative,
                    entry.path(),
                    target.direction,
                    "file",
                ));
            }
        }
    }
    let truncated = artifacts.len() >= MAX_BROWSER_ARTIFACTS;
    let artifact_value = Value::Array(artifacts);
    if let Some(structured) = result
        .get_mut("structuredContent")
        .and_then(Value::as_object_mut)
    {
        structured.insert("workspace_artifacts".into(), artifact_value.clone());
        structured.insert(
            "workspace_artifacts_truncated".into(),
            Value::Bool(truncated),
        );
        structured.insert(
            "workspace_bridge".into(),
            serde_json::json!({
                "status": "ready",
                "path_mode": "workspace_relative_to_absolute",
                "artifact_handle_scheme": "workspace://"
            }),
        );
    }
    if let Some(object) = result.as_object_mut() {
        let metadata = object
            .entry("_meta")
            .or_insert_with(|| serde_json::json!({}));
        if let Some(metadata) = metadata.as_object_mut() {
            metadata.insert("anchor/workspaceArtifacts".into(), artifact_value);
        }
    }
}

fn browser_artifact_descriptor(
    relative_path: &str,
    absolute_path: &Path,
    direction: &str,
    kind: &str,
) -> Value {
    let metadata = fs::metadata(absolute_path).ok();
    let modified_at = metadata
        .as_ref()
        .and_then(|metadata| metadata.modified().ok())
        .map(|timestamp| chrono::DateTime::<chrono::Utc>::from(timestamp).to_rfc3339());
    let normalized = relative_path
        .replace('\\', "/")
        .trim_start_matches("./")
        .to_string();
    serde_json::json!({
        "handle": format!("workspace://{normalized}"),
        "workspace_path": normalized,
        "direction": direction,
        "kind": kind,
        "exists": metadata.is_some(),
        "size_bytes": metadata.as_ref().filter(|metadata| metadata.is_file()).map(|metadata| metadata.len()),
        "modified_at": modified_at
    })
}

fn attach_proxy_auto_checkpoint(
    ctx: &ToolContext,
    tool_name: &str,
    arguments: &Value,
    result: &mut Value,
) {
    let primary_succeeded = result
        .get("structuredContent")
        .and_then(|structured| structured.get("ok"))
        .and_then(Value::as_bool)
        == Some(true);
    match crate::tools::history::auto_checkpoint_after_tool(ctx, tool_name, arguments, result) {
        Ok(Some(checkpoint)) => {
            if primary_succeeded {
                if let Some(task_id) = ctx
                    .harness
                    .current_task()
                    .ok()
                    .flatten()
                    .map(|task| task.id)
                {
                    let _ = ctx
                        .harness
                        .refresh_expected_state_for_operation(&task_id, None);
                }
            }
            if let Some(structured) = result
                .get_mut("structuredContent")
                .and_then(Value::as_object_mut)
            {
                structured.insert("auto_checkpoint".into(), checkpoint);
            }
        }
        Ok(None) => {}
        Err(error) => {
            if let Some(structured) = result
                .get_mut("structuredContent")
                .and_then(Value::as_object_mut)
            {
                structured.insert(
                    "auto_checkpoint_error".into(),
                    serde_json::json!({
                        "error": error.to_error_value(),
                        "retryable": true
                    }),
                );
            }
        }
    }
}

async fn attach_browser_build_info(
    ctx: &ToolContext,
    tool_name: &str,
    result: &mut Value,
    cancellation: &CancellationToken,
) {
    let normalized = tool_name.to_ascii_lowercase();
    let should_probe = [
        "take_snapshot",
        "take_screenshot",
        "lighthouse_audit",
        "performance_stop_trace",
    ]
    .iter()
    .any(|suffix| normalized.ends_with(suffix));
    let primary_succeeded = result
        .get("structuredContent")
        .and_then(|structured| structured.get("ok"))
        .and_then(Value::as_bool)
        == Some(true);
    if !should_probe || !primary_succeeded {
        return;
    }
    match probe_browser_build_info(ctx, cancellation).await {
        Ok((source_tool, build_info)) => {
            if let Some(structured) = result
                .get_mut("structuredContent")
                .and_then(Value::as_object_mut)
            {
                structured.insert("build_info".into(), build_info.clone());
                structured.insert(
                    "current_build".into(),
                    browser_current_build(&build_info)
                        .map(Value::String)
                        .unwrap_or(Value::Null),
                );
                structured.insert("build_info_source_tool".into(), Value::String(source_tool));
            }
        }
        Err(error) => {
            if let Some(structured) = result
                .get_mut("structuredContent")
                .and_then(Value::as_object_mut)
            {
                structured.insert("build_info_error".into(), error.to_error_value());
            }
        }
    }
}

async fn browser_build_info(ctx: &ToolContext, cancellation: &CancellationToken) -> Value {
    match probe_browser_build_info(ctx, cancellation).await {
        Ok((source_tool, build_info)) => tool_ok(serde_json::json!({
            "build_info": build_info,
            "current_build": browser_current_build(&build_info),
            "source_tool": source_tool,
            "warnings": []
        })),
        Err(error) => tool_err(error),
    }
}

async fn browser_wait_for_build(
    ctx: &ToolContext,
    args: &Value,
    cancellation: &CancellationToken,
) -> Value {
    let expected_build = args
        .get("expected_build")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    let timeout_ms = args
        .get("timeout_ms")
        .and_then(Value::as_u64)
        .unwrap_or(60_000)
        .clamp(1_000, 180_000);
    let poll_interval_ms = args
        .get("poll_interval_ms")
        .and_then(Value::as_u64)
        .unwrap_or(1_000)
        .clamp(250, 5_000);
    let clear_service_worker = args
        .get("clear_service_worker")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let clear_cache = args
        .get("clear_cache")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let start = Instant::now();

    let cleanup_script = format!(
        r#"async () => {{
  const clearServiceWorker = {clear_service_worker};
  const clearCache = {clear_cache};
  let serviceWorkersUnregistered = 0;
  let cachesDeleted = 0;
  if (clearServiceWorker && 'serviceWorker' in navigator) {{
    const registrations = await navigator.serviceWorker.getRegistrations();
    const results = await Promise.all(registrations.map((registration) => registration.unregister()));
    serviceWorkersUnregistered = results.filter(Boolean).length;
  }}
  if (clearCache && 'caches' in globalThis) {{
    const names = await caches.keys();
    const results = await Promise.all(names.map((name) => caches.delete(name)));
    cachesDeleted = results.filter(Boolean).length;
  }}
  return {{ service_workers_unregistered: serviceWorkersUnregistered, caches_deleted: cachesDeleted }};
}}"#
    );
    let cleanup = match call_browser_proxy_tool(
        ctx,
        "__evaluate_script",
        &serde_json::json!({"function": cleanup_script}),
        cancellation,
    )
    .await
    {
        Ok(result) => browser_proxy_result_summary(&result),
        Err(error) => return tool_err(error),
    };

    let reload_timeout = timeout_ms.min(30_000);
    let reload = match call_browser_proxy_tool(
        ctx,
        "__navigate_page",
        &serde_json::json!({
            "type": "reload",
            "ignoreCache": true,
            "timeout": reload_timeout
        }),
        cancellation,
    )
    .await
    {
        Ok(result) => browser_proxy_result_summary(&result),
        Err(error) => return tool_err(error),
    };

    let mut attempts = 0u64;
    let mut last_build_info = serde_json::json!({});
    loop {
        if cancellation.is_cancelled() {
            return tool_err(WorkspaceError::ToolDetails {
                code: "REQUEST_CANCELLED",
                message: "Browser build wait was cancelled".into(),
                category: "runtime",
                retryable: true,
                details: serde_json::json!({
                    "expected_build": expected_build,
                    "attempts": attempts
                }),
            });
        }
        attempts = attempts.saturating_add(1);
        let last_error = match probe_browser_build_info(ctx, cancellation).await {
            Ok((_source_tool, build_info)) => {
                let matched = browser_build_matches(&build_info, &expected_build);
                last_build_info = build_info;
                if matched {
                    return tool_ok(serde_json::json!({
                        "expected_build": expected_build,
                        "matched": true,
                        "current_build": browser_current_build(&last_build_info),
                        "build_info": last_build_info,
                        "attempts": attempts,
                        "elapsed_ms": start.elapsed().as_millis(),
                        "cleanup": cleanup,
                        "reload": reload,
                        "warnings": []
                    }));
                }
                None
            }
            Err(error) => Some(error.to_error_value()),
        };

        if start.elapsed() >= Duration::from_millis(timeout_ms) {
            let current_build = browser_current_build(&last_build_info);
            let mut output = tool_err(WorkspaceError::ToolDetails {
                code: "BROWSER_BUILD_TIMEOUT",
                message: format!(
                    "Timed out waiting for browser build `{expected_build}`; current build is {}",
                    current_build.as_deref().unwrap_or("unknown")
                ),
                category: "runtime",
                retryable: true,
                details: serde_json::json!({
                    "expected_build": expected_build,
                    "current_build": current_build,
                    "attempts": attempts,
                    "elapsed_ms": start.elapsed().as_millis(),
                    "last_probe_error": last_error
                }),
            });
            if let Some(object) = output.as_object_mut() {
                object.insert("expected_build".into(), Value::String(expected_build));
                object.insert("matched".into(), Value::Bool(false));
                object.insert(
                    "current_build".into(),
                    current_build.map(Value::String).unwrap_or(Value::Null),
                );
                object.insert("build_info".into(), last_build_info);
                object.insert("attempts".into(), serde_json::json!(attempts));
                object.insert(
                    "elapsed_ms".into(),
                    serde_json::json!(start.elapsed().as_millis()),
                );
                object.insert("cleanup".into(), cleanup);
                object.insert("reload".into(), reload);
                object.insert("warnings".into(), serde_json::json!([]));
            }
            return output;
        }
        tokio::time::sleep(Duration::from_millis(poll_interval_ms)).await;
    }
}

async fn probe_browser_build_info(
    ctx: &ToolContext,
    cancellation: &CancellationToken,
) -> Result<(String, Value), WorkspaceError> {
    let (source_tool, result) = call_browser_proxy_tool_named(
        ctx,
        "__evaluate_script",
        &serde_json::json!({"function": BROWSER_BUILD_PROBE_JS}),
        cancellation,
    )
    .await?;
    let build_info =
        extract_browser_json_payload(&result).ok_or_else(|| WorkspaceError::ToolDetails {
            code: "BROWSER_BUILD_INFO_UNAVAILABLE",
            message: "Browser build probe returned no JSON object".into(),
            category: "runtime",
            retryable: true,
            details: serde_json::json!({
                "source_tool": source_tool,
                "result_summary": browser_proxy_result_summary(&result)
            }),
        })?;
    Ok((source_tool, build_info))
}

async fn call_browser_proxy_tool(
    ctx: &ToolContext,
    suffix: &str,
    arguments: &Value,
    cancellation: &CancellationToken,
) -> Result<Value, WorkspaceError> {
    call_browser_proxy_tool_named(ctx, suffix, arguments, cancellation)
        .await
        .map(|(_, result)| result)
}

async fn call_browser_proxy_tool_named(
    ctx: &ToolContext,
    suffix: &str,
    arguments: &Value,
    cancellation: &CancellationToken,
) -> Result<(String, Value), WorkspaceError> {
    let tool_name =
        find_browser_proxy_tool(ctx, suffix).ok_or_else(|| WorkspaceError::ToolDetails {
            code: "BROWSER_TOOL_UNAVAILABLE",
            message: format!(
                "Required downstream browser tool ending with `{suffix}` is unavailable"
            ),
            category: "runtime",
            retryable: true,
            details: serde_json::json!({
                "suffix": suffix,
                "suggestion": "Reconnect the browser MCP and refresh the tool catalog"
            }),
        })?;
    let result = ctx
        .mcp_proxies
        .call_tool_with_cancellation(&tool_name, arguments, cancellation)
        .await
        .ok_or_else(|| WorkspaceError::Tool {
            code: "BROWSER_TOOL_UNAVAILABLE",
            message: format!("Browser proxy route disappeared: {tool_name}"),
            category: "runtime",
            retryable: true,
        })?;
    let result = result.map_err(|error| WorkspaceError::ToolDetails {
        code: "BROWSER_PROXY_CALL_FAILED",
        message: format!("Browser proxy call failed: {error}"),
        category: "runtime",
        retryable: true,
        details: serde_json::json!({"tool": tool_name, "proxy_error": error}),
    })?;
    let structured = result.get("structuredContent").unwrap_or(&result);
    if structured.get("ok").and_then(Value::as_bool) == Some(false) {
        let error = structured
            .get("error")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({}));
        return Err(WorkspaceError::ToolDetails {
            code: "BROWSER_PROXY_TOOL_FAILED",
            message: structured
                .get("error_message")
                .and_then(Value::as_str)
                .or_else(|| error.get("message").and_then(Value::as_str))
                .unwrap_or("Browser proxy tool failed")
                .to_string(),
            category: "runtime",
            retryable: structured
                .get("retryable")
                .and_then(Value::as_bool)
                .or_else(|| error.get("retryable").and_then(Value::as_bool))
                .unwrap_or(true),
            details: serde_json::json!({
                "tool": tool_name,
                "structured": structured
            }),
        });
    }
    Ok((tool_name, result))
}

fn find_browser_proxy_tool(ctx: &ToolContext, suffix: &str) -> Option<String> {
    let catalog = ctx
        .published_catalog()
        .or_else(|| build_effective_catalog(ctx).ok())?;
    catalog
        .tools
        .iter()
        .filter_map(|tool| tool.get("name").and_then(Value::as_str))
        .find(|name| name.ends_with(suffix) && ctx.mcp_proxies.contains_tool(name))
        .map(str::to_string)
}

fn extract_browser_json_payload(value: &Value) -> Option<Value> {
    extract_browser_json_payload_inner(value, 0)
}

fn extract_browser_json_payload_inner(value: &Value, depth: usize) -> Option<Value> {
    if depth > 10 {
        return None;
    }
    match value {
        Value::Object(object) => {
            if object.keys().any(|key| {
                matches!(
                    key.as_str(),
                    "build_hash"
                        | "git_commit"
                        | "app_version"
                        | "service_workers_unregistered"
                        | "caches_deleted"
                )
            }) {
                return Some(value.clone());
            }
            for key in [
                "structuredContent",
                "result",
                "value",
                "json",
                "text",
                "content",
            ] {
                if let Some(found) = object
                    .get(key)
                    .and_then(|nested| extract_browser_json_payload_inner(nested, depth + 1))
                {
                    return Some(found);
                }
            }
            object
                .values()
                .find_map(|nested| extract_browser_json_payload_inner(nested, depth + 1))
        }
        Value::Array(array) => array
            .iter()
            .find_map(|nested| extract_browser_json_payload_inner(nested, depth + 1)),
        Value::String(text) => parse_json_object_from_text(text),
        _ => None,
    }
}

fn parse_json_object_from_text(text: &str) -> Option<Value> {
    let trimmed = text.trim();
    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        if value.is_object() {
            return Some(value);
        }
    }
    let start = trimmed.find('{')?;
    let end = trimmed.rfind('}')?;
    if end <= start {
        return None;
    }
    serde_json::from_str::<Value>(&trimmed[start..=end])
        .ok()
        .filter(Value::is_object)
}

fn browser_current_build(build_info: &Value) -> Option<String> {
    for key in ["build_hash", "git_commit", "app_version"] {
        if let Some(value) = build_info
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            return Some(value.to_string());
        }
    }
    build_info
        .get("asset_hashes")
        .and_then(Value::as_array)
        .and_then(|hashes| hashes.iter().filter_map(Value::as_str).next())
        .map(str::to_string)
}

fn browser_build_matches(build_info: &Value, expected: &str) -> bool {
    let expected = expected.trim().to_ascii_lowercase();
    if expected.is_empty() {
        return false;
    }
    let mut candidates = ["build_hash", "git_commit", "app_version"]
        .iter()
        .filter_map(|key| build_info.get(key).and_then(Value::as_str))
        .map(str::to_string)
        .collect::<Vec<_>>();
    candidates.extend(
        build_info
            .get("asset_hashes")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_string),
    );
    candidates.into_iter().any(|candidate| {
        let candidate = candidate.trim().to_ascii_lowercase();
        candidate == expected
            || (expected.len().min(candidate.len()) >= 7
                && (candidate.starts_with(&expected) || expected.starts_with(&candidate)))
    })
}

fn browser_proxy_result_summary(result: &Value) -> Value {
    let structured = result.get("structuredContent").unwrap_or(result);
    serde_json::json!({
        "ok": structured.get("ok").and_then(Value::as_bool).unwrap_or(true),
        "error_code": structured.get("error_code"),
        "error_message": structured.get("error_message"),
        "connection_status": structured.get("connection_status"),
        "selected_page": structured.get("selected_page"),
        "payload": extract_browser_json_payload(result)
    })
}

fn attach_local_browser_checkpoint(
    ctx: &ToolContext,
    tool_name: &str,
    arguments: &Value,
    output: &mut Value,
) {
    let succeeded = output.get("ok").and_then(Value::as_bool) == Some(true);
    match crate::tools::history::auto_checkpoint_after_tool(ctx, tool_name, arguments, output) {
        Ok(Some(checkpoint)) => {
            if succeeded {
                if let Some(task_id) = ctx
                    .harness
                    .current_task()
                    .ok()
                    .flatten()
                    .map(|task| task.id)
                {
                    let _ = ctx
                        .harness
                        .refresh_expected_state_for_operation(&task_id, None);
                }
            }
            if let Some(object) = output.as_object_mut() {
                object.insert("auto_checkpoint".into(), checkpoint);
            }
        }
        Ok(None) => {}
        Err(error) => {
            if let Some(object) = output.as_object_mut() {
                object.insert("auto_checkpoint_error".into(), error.to_error_value());
            }
        }
    }
}

pub fn new_state(
    workspace: Workspace,
    auth: AuthConfig,
    policy: crate::tools::policy::PolicySettings,
    tool_profile: String,
    permission_mode: String,
) -> SharedState {
    Arc::new(ToolContext::from_workspace(
        workspace,
        auth,
        policy,
        tool_profile,
        permission_mode,
    ))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::Arc;

    use serde_json::json;

    use crate::tools::{build_effective_catalog_from_parts, CancellationToken, ToolContext};

    use super::{
        attach_browser_workspace_artifacts, browser_build_matches, browser_current_build,
        effective_catalog_error, extract_browser_json_payload, handle_request, handle_tools_call,
        initialize_result, prepare_browser_workspace_arguments, tool_arguments, tools_list_result,
    };

    fn test_state() -> (tempfile::TempDir, tempfile::TempDir, Arc<ToolContext>) {
        let workspace = tempfile::tempdir().expect("workspace tempdir");
        let harness = tempfile::tempdir().expect("harness tempdir");
        let state = Arc::new(
            ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                .expect("tool context"),
        );
        (workspace, harness, state)
    }

    #[test]
    fn browser_relative_output_paths_are_bound_to_the_workspace() {
        let (workspace, _harness, state) = test_state();
        let (prepared, targets) = prepare_browser_workspace_arguments(
            &state.workspace,
            "browser__take_screenshot",
            &json!({"filePath": "docs/artifacts/browser/page.png"}),
        )
        .expect("prepare browser path");
        let prepared_path = prepared["filePath"].as_str().expect("prepared path");
        assert!(std::path::Path::new(prepared_path).is_absolute());
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].relative_path, "docs/artifacts/browser/page.png");
        assert!(targets[0].absolute_path.starts_with(
            workspace
                .path()
                .canonicalize()
                .expect("canonical workspace")
        ));
        assert!(targets[0]
            .absolute_path
            .parent()
            .expect("artifact parent")
            .is_dir());
    }

    #[test]
    fn browser_workspace_artifacts_return_stable_handles() {
        let (_workspace, _harness, state) = test_state();
        let (_prepared, targets) = prepare_browser_workspace_arguments(
            &state.workspace,
            "browser__take_screenshot",
            &json!({"filePath": "docs/artifacts/browser/page.png"}),
        )
        .expect("prepare browser path");
        fs::write(&targets[0].absolute_path, b"image").expect("write artifact");
        let mut result = json!({
            "content": [{"type": "text", "text": "ok"}],
            "structuredContent": {"ok": true},
            "isError": false
        });
        attach_browser_workspace_artifacts(state.workspace.root(), &targets, &mut result);
        assert_eq!(
            result["structuredContent"]["workspace_artifacts"][0]["handle"],
            "workspace://docs/artifacts/browser/page.png"
        );
        assert_eq!(
            result["structuredContent"]["workspace_artifacts"][0]["exists"],
            true
        );
        assert_eq!(
            result["_meta"]["anchor/workspaceArtifacts"][0]["size_bytes"],
            5
        );
    }

    #[test]
    fn browser_build_payload_is_extracted_from_downstream_text() {
        let encoded = json!({
            "build_hash": "abcdef123456",
            "asset_hashes": ["chunk-7890"]
        })
        .to_string();
        let result = json!({
            "content": [{
                "type": "text",
                "text": format!("Evaluation result: {encoded}")
            }]
        });
        let build = extract_browser_json_payload(&result).expect("build payload");
        assert_eq!(build["build_hash"], "abcdef123456");
        assert_eq!(
            browser_current_build(&build).as_deref(),
            Some("abcdef123456")
        );
    }

    #[test]
    fn browser_build_matching_accepts_safe_commit_prefixes_only() {
        let build = json!({
            "git_commit": "abcdef1234567890",
            "asset_hashes": ["chunk-fedcba987654"]
        });
        assert!(browser_build_matches(&build, "abcdef1"));
        assert!(browser_build_matches(&build, "chunk-fedcba987654"));
        assert!(!browser_build_matches(&build, "abc"));
        assert!(!browser_build_matches(&build, "1234567"));
    }

    #[test]
    fn browser_upload_paths_accept_workspace_relative_files() {
        let (workspace, _harness, state) = test_state();
        fs::create_dir_all(workspace.path().join("fixtures")).expect("fixtures");
        fs::write(workspace.path().join("fixtures/upload.txt"), b"upload").expect("upload fixture");
        let (prepared, targets) = prepare_browser_workspace_arguments(
            &state.workspace,
            "browser__upload_file",
            &json!({"filePath": "fixtures/upload.txt"}),
        )
        .expect("prepare upload path");
        assert!(
            std::path::Path::new(prepared["filePath"].as_str().expect("prepared upload path"))
                .is_absolute()
        );
        assert_eq!(targets[0].direction, "input");
        assert_eq!(targets[0].relative_path, "fixtures/upload.txt");
    }

    #[tokio::test]
    async fn unpublished_new_tool_requires_reconnect() {
        let (_workspace, _harness, state) = test_state();
        let core = build_effective_catalog_from_parts("core", true, Vec::new()).expect("core");
        let _ = state.publish_catalog(core);

        let error = handle_tools_call(
            &state,
            &json!({
                "name": "stage_commit_status",
                "arguments": {"task_id": "task", "idempotency_key": "key"}
            }),
            &CancellationToken::default(),
            Some("session"),
        )
        .await
        .expect_err("tool was not published");
        assert_eq!(error["data"]["reason"], "catalog_changed");
        assert_eq!(error["data"]["reconnect_required"], true);
    }

    fn browser_proxy_tools(count: usize) -> Vec<serde_json::Value> {
        (0..count)
            .map(|index| {
                json!({
                    "name": format!("browser__action_{index:02}"),
                    "title": format!("Browser action {index}"),
                    "description": "Representative browser MCP action",
                    "inputSchema": {
                        "type": "object",
                        "properties": {},
                        "additionalProperties": false
                    },
                    "outputSchema": {
                        "type": "object",
                        "properties": {"ok": {"type": "boolean"}},
                        "required": ["ok"],
                        "additionalProperties": true
                    },
                    "annotations": {
                        "readOnlyHint": false,
                        "destructiveHint": true,
                        "idempotentHint": false,
                        "openWorldHint": true
                    }
                })
            })
            .collect()
    }

    #[test]
    fn tools_list_paginates_a_budget_compliant_catalog() {
        let catalog = build_effective_catalog_from_parts("core", true, browser_proxy_tools(48))
            .expect("budget-compliant catalog");
        let first = tools_list_result(&catalog, &json!({})).expect("first page");
        let first_tools = first["tools"].as_array().expect("first tools");
        assert_eq!(first_tools.len(), 64);
        assert_eq!(first["_meta"]["anchor/catalog"]["local_tool_count"], 43);
        assert_eq!(first["_meta"]["anchor/catalog"]["proxy_tool_count"], 48);
        assert!(first["_meta"]["anchor/catalog"]["estimated_tokens"]
            .as_u64()
            .is_some_and(|tokens| tokens > 0));
        let first_names = first_tools
            .iter()
            .filter_map(|tool| tool["name"].as_str())
            .collect::<std::collections::HashSet<_>>();
        for required in [
            "read_file",
            "read_output",
            "wait_command",
            "list_command_sessions",
            "search_text",
            "server_info",
            "browser_build_info",
            "browser_wait_for_build",
        ] {
            assert!(
                first_names.contains(required),
                "{required} missing from first page"
            );
        }
        assert!(first_tools[..catalog.local_count].iter().all(|tool| {
            !tool["name"]
                .as_str()
                .unwrap_or_default()
                .starts_with("browser__action_")
        }));
        let cursor = first["nextCursor"].as_str().expect("next cursor");

        let second = tools_list_result(&catalog, &json!({"cursor": cursor})).expect("second page");
        let second_tools = second["tools"].as_array().expect("second tools");
        assert_eq!(first_tools.len() + second_tools.len(), catalog.tools.len());
        assert!(second.get("nextCursor").is_none());
    }

    #[test]
    fn tools_list_rejects_a_cursor_from_another_catalog() {
        let catalog = build_effective_catalog_from_parts("core", true, browser_proxy_tools(48))
            .expect("catalog");
        let error = tools_list_result(
            &catalog,
            &json!({
                "cursor": "anchor-v1:wrong-digest:64"
            }),
        )
        .expect_err("invalid cursor");

        assert_eq!(error["code"], -32602);
        assert_eq!(error["data"]["reason"], "invalid_tools_list_cursor");
    }

    #[test]
    fn tools_list_maps_over_budget_catalog_to_actionable_server_error() {
        let error = build_effective_catalog_from_parts("advanced", true, browser_proxy_tools(100))
            .expect_err("over-budget catalog");
        let response = effective_catalog_error(error);

        assert_eq!(response["code"], -32004);
        assert!(response["message"]
            .as_str()
            .is_some_and(|message| message.contains("includeTools")));
        assert_eq!(
            response["data"]["details"]["reason"],
            "chatgpt_catalog_budget_exceeded"
        );
    }

    #[test]
    fn initialize_instructions_define_the_history_persistence_workflow() {
        let (_workspace, _harness, state) = test_state();
        let initialized = initialize_result(&state);
        let instructions = initialized["instructions"].as_str().expect("instructions");
        assert!(instructions.contains("list_skills"));
        assert!(instructions.contains("allowed-tools declarations are dependency metadata only"));
        assert!(instructions.contains("never grants permissions"));
        assert!(instructions.contains("There is no dedicated Skill script executor"));
        assert!(instructions.contains("Model-supplied confirm fields are not accepted"));
        assert!(instructions.contains("trusted GUI or CLI control plane"));
        assert!(instructions.contains("script digest changed"));
        assert!(instructions.contains("history_session_bootstrap"));
        assert!(instructions.contains("At the start of every new ChatGPT conversation"));
        assert!(instructions.contains("before answering the user's first request"));
        assert!(instructions.contains("even if the user did not explicitly ask"));
        assert!(instructions.contains("required conversation initialization"));
        assert!(instructions.contains("must not create duplicates"));
        assert!(instructions.contains("history_session_checkpoint"));
        assert!(instructions.contains("session_key and current_path returned by bootstrap"));
        assert!(instructions.contains("session_key and expected_path"));
        assert!(instructions.contains("resume_state"));
        assert!(instructions.contains("best-effort milestone checkpoints"));
        assert!(instructions.contains("automatic milestones do not replace the final task handoff"));
        assert!(instructions.contains("After completing each user-requested task"));
        assert!(instructions.contains("before the final response"));
        assert!(instructions.contains("checkpoint returns ok=true"));
    }

    #[test]
    fn initialize_does_not_claim_tool_catalog_notifications_without_a_stream() {
        let (_workspace, _harness, state) = test_state();
        let initialized = initialize_result(&state);

        assert_eq!(initialized["capabilities"]["tools"]["listChanged"], false);
        assert_eq!(
            initialized["capabilities"]["resources"]["listChanged"],
            false
        );
    }

    #[test]
    fn workspace_prompt_initializes_or_restores_a_chatgpt_session() {
        let component = include_str!("../../../src/lib/components/ChatGptSessionPrompt.svelte");

        assert!(component.contains("ChatGPT 新会话启动提示词"));
        assert!(component.contains("请使用当前工作区的 Anchor MCP 初始化或恢复项目会话"));
        assert!(component.contains("先且仅调用一次 history_session_bootstrap"));
        assert!(component.contains("如果没有历史记录"));
        assert!(component.contains("all_history_summary"));
        assert!(component.contains("latest_handoff"));
        assert!(component.contains("inherited_summary"));
        assert!(component.contains("history_summaries_omitted"));
        assert!(component.contains("history_summary_truncated"));
        assert!(component.contains("latest_handoff_truncated"));
        assert!(component.contains("current_path"));
        assert!(component.contains("expected_path"));
        assert!(component.contains("history_session_checkpoint"));
        assert!(component.contains("发送最终答复前"));
        assert!(component.contains("幂等里程碑检查点"));
        assert!(component.contains("不能替代最终交接"));
        assert!(!component.contains("打开连接器设置"));
    }

    #[test]
    fn chatgpt_session_metadata_is_injected_for_history_and_work_session_tools() {
        let params = json!({
            "arguments": {"session_key": "explicit"},
            "_meta": {"openai/session": "chatgpt-conversation"}
        });
        let history = tool_arguments("history_session_bootstrap", &params);
        assert_eq!(history["session_key"], "explicit");
        assert_eq!(history["_host_session_key"], "chatgpt-conversation");

        let work_session = tool_arguments("begin_work_session", &params);
        assert_eq!(work_session["session_key"], "explicit");
        assert_eq!(work_session["_host_session_key"], "chatgpt-conversation");

        let existing = tool_arguments("read_file", &params);
        assert_eq!(existing["session_key"], "explicit");
        assert!(existing.get("_host_session_key").is_none());
    }

    #[tokio::test]
    async fn explicit_session_key_prevents_changed_chatgpt_metadata_from_redirecting_history() {
        let workspace = tempfile::tempdir().expect("workspace tempdir");
        let harness = tempfile::tempdir().expect("harness tempdir");
        let state = Arc::new(
            ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                .expect("tool context"),
        );
        let response = handle_request(
            &state,
            &json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {
                    "name": "history_session_bootstrap",
                    "arguments": {"session_key": "explicit-session"},
                    "_meta": {"openai/session": "chatgpt-session"}
                }
            }),
        )
        .await;
        let structured = &response["result"]["structuredContent"];
        assert_eq!(structured["ok"], true);
        assert_eq!(structured["session_key_source"], "explicit_session_key");
        assert_eq!(structured["session_key"], "explicit-session");
        assert_eq!(structured["host_session_key_mismatch"], true);
        assert_eq!(structured["host_session_key_mismatch_level"], "debug");
        assert_eq!(structured["target_preserved"], true);
        assert!(!structured["warnings"]
            .as_array()
            .expect("warnings")
            .iter()
            .any(|warning| warning
                .as_str()
                .is_some_and(|value| value.contains("宿主会话"))));
        let content = fs::read_to_string(workspace.path().join("docs/history-session/1.md"))
            .expect("read history file");
        assert!(content.contains("**Session key:** explicit-session"));
        assert!(!content.contains("**Session key:** chatgpt-session"));
    }

    #[tokio::test]
    async fn skills_are_discovered_and_loaded_through_mcp() {
        let workspace = tempfile::tempdir().expect("workspace tempdir");
        let harness = tempfile::tempdir().expect("harness tempdir");
        let skill_dir = workspace.path().join("skills/review");
        fs::create_dir_all(skill_dir.join("references")).expect("create skill");
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: review\ndescription: Review a change.\n---\nRead the diff carefully.\nCheck tests.\nReport findings.\n",
        )
        .expect("write skill");
        fs::write(skill_dir.join("references/RULES.md"), "No regressions.\n")
            .expect("write resource");
        let state = Arc::new(
            ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                .expect("tool context"),
        );

        let tools = handle_request(
            &state,
            &json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
        )
        .await;
        assert!(tools["result"]["tools"]
            .as_array()
            .expect("tools")
            .iter()
            .any(|tool| tool["name"] == "load_skill"));

        let listed = handle_request(
            &state,
            &json!({
                "jsonrpc":"2.0",
                "id":2,
                "method":"tools/call",
                "params":{"name":"list_skills","arguments":{}}
            }),
        )
        .await;
        assert_eq!(
            listed["result"]["structuredContent"]["skills"][0]["name"],
            "review"
        );
        assert_eq!(
            listed["result"]["structuredContent"]["skills"][0]["oversized"],
            false
        );

        let loaded = handle_request(
            &state,
            &json!({
                "jsonrpc":"2.0",
                "id":3,
                "method":"tools/call",
                "params":{
                    "name":"load_skill",
                    "arguments":{"name":"review","start_line":2,"end_line":2}
                }
            }),
        )
        .await;
        assert_eq!(
            loaded["result"]["structuredContent"]["instructions"],
            "Check tests.\n"
        );
        assert_eq!(loaded["result"]["structuredContent"]["startLine"], 2);
        assert_eq!(loaded["result"]["structuredContent"]["endLine"], 2);
        assert_eq!(loaded["result"]["structuredContent"]["totalLines"], 3);
        assert_eq!(loaded["result"]["structuredContent"]["nextStartLine"], 3);
        assert_eq!(loaded["result"]["structuredContent"]["truncated"], true);

        let index = handle_request(
            &state,
            &json!({
                "jsonrpc":"2.0",
                "id":4,
                "method":"resources/read",
                "params":{"uri":"skill://index.json"}
            }),
        )
        .await;
        let index_text = index["result"]["contents"][0]["text"]
            .as_str()
            .expect("index text");
        assert!(index_text.contains("skill://review/SKILL.md"));

        let page = handle_request(
            &state,
            &json!({
                "jsonrpc":"2.0",
                "id":5,
                "method":"resources/read",
                "params":{"uri":"skill://review/SKILL.md?start_line=2&end_line=2"}
            }),
        )
        .await;
        assert!(page["result"]["contents"][0]["text"]
            .as_str()
            .expect("skill page")
            .contains("name: review"));
        assert_eq!(page["result"]["_meta"]["startLine"], 2);
        assert_eq!(page["result"]["_meta"]["endLine"], 2);
    }

    #[tokio::test]
    async fn disabled_skill_service_hides_tools_and_resources_capability() {
        let (_workspace, _harness, state) = test_state();
        state
            .skills
            .configure(crate::skills::SkillSettings::from_text(false, "skills"));

        let initialized = initialize_result(&state);
        assert!(initialized["capabilities"].get("resources").is_none());

        let tools = handle_request(
            &state,
            &json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
        )
        .await;
        assert!(!tools["result"]["tools"]
            .as_array()
            .expect("tools")
            .iter()
            .any(|tool| tool["name"] == "load_skill"));
    }
}
