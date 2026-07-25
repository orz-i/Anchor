use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use serde::Deserialize;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::{Mutex, Notify};
use tokio::task::JoinSet;
use tokio::time::timeout;

use crate::tools::CancellationToken;
use crate::tunnel::append_profile_log;

const DEFAULT_REQUEST_TIMEOUT_SECONDS: u64 = 30;
const MAX_TOOL_LIST_PAGES: usize = 100;

#[derive(Debug, Clone)]
pub struct McpProxyServerSpec {
    pub name: String,
    command: String,
    args: Vec<String>,
    env: BTreeMap<String, String>,
    cwd: PathBuf,
    tool_prefix: String,
    request_timeout: Duration,
}

fn catalog_tool_names(catalog: &[Value]) -> Vec<String> {
    let mut names = catalog
        .iter()
        .filter_map(|tool| tool.get("name").and_then(Value::as_str))
        .map(str::to_string)
        .collect::<Vec<_>>();
    names.sort();
    names.dedup();
    names
}

async fn connect_initial_with_retry(
    spec: McpProxyServerSpec,
    workspace_id: String,
) -> Result<(Arc<ProxyServer>, Vec<Value>), String> {
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
    catalog_names: Vec<String>,
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
    #[serde(rename = "requestTimeoutSeconds", default)]
    request_timeout_seconds: Option<u64>,
}

#[derive(Clone)]
struct ProxyRoute {
    server: Arc<ProxyServer>,
    server_name: String,
    downstream_name: String,
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
                    for tool in catalog {
                        if !tool.is_object() {
                            continue;
                        }
                        let Some(downstream_name) = tool
                            .get("name")
                            .and_then(Value::as_str)
                            .map(ToString::to_string)
                        else {
                            continue;
                        };
                        let public_name = format!(
                            "{}__{}",
                            sanitize_tool_segment(&server.spec.tool_prefix),
                            sanitize_tool_segment(&downstream_name)
                        );
                        if state.routes.contains_key(&public_name) {
                            skipped_duplicates.push(public_name);
                            continue;
                        }

                        let mut merged = tool;
                        merged["name"] = Value::String(public_name.clone());
                        let description = merged
                            .get("description")
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        merged["description"] = Value::String(if description.is_empty() {
                            format!("Proxied from MCP server {server_name}")
                        } else {
                            format!("[{server_name}] {description}")
                        });
                        if merged.get("title").is_none() {
                            merged["title"] =
                                Value::String(format!("{} · {}", server_name, downstream_name));
                        }

                        state.routes.insert(
                            public_name,
                            ProxyRoute {
                                server: server.clone(),
                                server_name: server_name.clone(),
                                downstream_name,
                            },
                        );
                        state.tools.push(merged);
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
                        &format!("[mcp-proxy:{server_name}] connected; merged {added} tools"),
                    );
                }
                Err(error) => append_profile_log(
                    &workspace_id,
                    "stderr.log",
                    &format!("[mcp-proxy:{server_name}] unavailable: {error}"),
                ),
            }
        }
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
        let client = match server.ensure_client().await {
            Ok(client) => client,
            Err(message) => {
                server.clone().schedule_reconnect();
                return Some(Err(proxy_call_error(
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

        Some(result.map_err(|message| {
            let cancelled = message == "request cancelled";
            proxy_call_error(
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
        }))
    }
}

impl ProxyServer {
    async fn connect_initial(
        spec: McpProxyServerSpec,
        workspace_id: String,
    ) -> Result<(Arc<Self>, Vec<Value>), String> {
        let (client, catalog) = McpProxyClient::connect(spec.clone(), &workspace_id).await?;
        let catalog_names = catalog_tool_names(&catalog);
        Ok((
            Arc::new(Self {
                spec,
                workspace_id,
                catalog_names,
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
        let reconnected_names = catalog_tool_names(&catalog);
        if reconnected_names != self.catalog_names {
            return Err(format!(
                "downstream tool catalog changed from {:?} to {:?}; restart the MCP listener to renegotiate tools/list",
                self.catalog_names, reconnected_names
            ));
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

fn proxy_call_error(
    server_name: &str,
    public_name: &str,
    reason: &str,
    detail: String,
    retryable: bool,
    reconnect_scheduled: bool,
) -> Value {
    json!({
        "code": -32603,
        "message": format!("Proxied MCP tool failed: {server_name} / {public_name}"),
        "data": {
            "reason": reason,
            "server": server_name,
            "tool": public_name,
            "detail": detail,
            "retryable": retryable,
            "request_replayed": false,
            "reconnect_scheduled": reconnect_scheduled
        }
    })
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
                        "name": "coding-tools-mcp-proxy",
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

        specs.push(McpProxyServerSpec {
            name,
            command,
            args,
            env,
            cwd,
            tool_prefix,
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
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::Path;
    use std::time::Duration;

    use serde_json::json;

    use crate::tools::CancellationToken;

    use super::{parse_mcp_proxy_config, McpProxyRegistry, McpProxyServerSpec};

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
                .expect_err("cancelled call")
        });
        tokio::time::sleep(Duration::from_millis(150)).await;
        token.cancel();
        let error = worker.await.expect("worker");
        assert_eq!(error["data"]["reason"], "proxy_call_cancelled");
        assert_eq!(error["data"]["request_replayed"], false);
        assert_eq!(error["data"]["reconnect_scheduled"], false);
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
                    request_timeout: Duration::from_secs(5),
                }],
                "proxy-reconnect-test",
            )
            .await;

        assert!(registry
            .list_tools()
            .iter()
            .any(|tool| tool["name"] == "unstable__ping"));

        let first = registry
            .call_tool("unstable__ping", &json!({}))
            .await
            .expect("known route")
            .expect_err("first call disconnects");
        assert_eq!(first["data"]["request_replayed"], false);
        assert_eq!(first["data"]["reconnect_scheduled"], true);

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
        tools = [{"name": "ping", "inputSchema": {"type": "object", "properties": {}}}]
        if os.path.exists(marker):
            tools.append({"name": "new_tool", "inputSchema": {"type": "object", "properties": {}}})
        result = {"tools": tools}
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
                    request_timeout: Duration::from_secs(5),
                }],
                "proxy-catalog-drift-test",
            )
            .await;

        let first = registry
            .call_tool("drift__ping", &json!({}))
            .await
            .expect("known route")
            .expect_err("first call disconnects");
        assert_eq!(first["data"]["request_replayed"], false);

        let second = registry
            .call_tool("drift__ping", &json!({}))
            .await
            .expect("known route")
            .expect_err("catalog drift rejects reconnect");
        assert_eq!(second["data"]["reason"], "proxy_reconnect_failed");
        assert!(second["data"]["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("tool catalog changed")));
    }
}
