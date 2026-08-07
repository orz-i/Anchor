use std::path::{Path, PathBuf};
use std::time::Duration;

use reqwest::header::{ALLOW, AUTHORIZATION, WWW_AUTHENTICATE};
use serde::Serialize;
use serde_json::{json, Value};

use crate::auth::builtin_redirect_hosts;
use crate::data::DataStore;
use crate::error::{AppError, AppResult};
use crate::platform::platform;
use crate::tunnel::drop_workspace as drop_tunnel_workspace;
use crate::workspace::resources::assign_free_workspace_ports_with_reserved;
use crate::workspace::WorkspaceProfile;

use super::args::{
    EndpointSelection, GptConfigOptions, UnregisterOptions, WorkspaceCommand, WorkspaceTestOptions,
};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkspaceMutationResult {
    event: &'static str,
    workspace: WorkspaceIdentity,
    mcp_port: u16,
    actions_port: u16,
    project_files_deleted: bool,
    warnings: Vec<String>,
}

fn assign_os_available_ports(
    profiles: &[WorkspaceProfile],
    profile: &mut WorkspaceProfile,
) -> AppResult<()> {
    let mut reserved = profiles
        .iter()
        .flat_map(|item| [item.runtime.local_port, item.actions.local_port])
        .collect::<std::collections::HashSet<_>>();
    profile.runtime.local_port = next_available_port(profile.runtime.local_port, &reserved)?;
    reserved.insert(profile.runtime.local_port);
    profile.actions.local_port = next_available_port(profile.actions.local_port, &reserved)?;
    Ok(())
}

fn next_available_port(
    preferred: u16,
    reserved: &std::collections::HashSet<u16>,
) -> AppResult<u16> {
    for port in preferred.max(1)..=u16::MAX {
        if reserved.contains(&port) {
            continue;
        }
        if platform().find_pid_listening_on_port(port)?.is_none() {
            return Ok(port);
        }
    }
    Err(AppError::Message(format!(
        "无法从端口 {preferred} 起找到可用端口"
    )))
}

fn workspace_path_string(path: &Path) -> String {
    let value = path.to_string_lossy();
    #[cfg(windows)]
    {
        if let Some(rest) = value.strip_prefix(r"\\?\UNC\") {
            return format!(r"\\{rest}");
        }
        if let Some(rest) = value.strip_prefix(r"\\?\") {
            return rest.to_string();
        }
    }
    value.into_owned()
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkspaceIdentity {
    id: String,
    name: String,
    path: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ConnectionTestReport {
    workspace: WorkspaceIdentity,
    endpoint_mode: String,
    ok: bool,
    checks: Vec<ConnectionCheck>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ConnectionCheck {
    service: &'static str,
    name: String,
    ok: bool,
    detail: String,
    hint: String,
}

pub async fn execute(command: WorkspaceCommand, as_json: bool) -> AppResult<i32> {
    match command {
        WorkspaceCommand::List => super::list_workspaces(as_json).map(|_| 0),
        WorkspaceCommand::Register(options) => {
            register_workspace(&options.path, options.name, as_json).map(|_| 0)
        }
        WorkspaceCommand::Unregister(options) => unregister_workspace(options, as_json).await,
        WorkspaceCommand::Show { workspace } => {
            super::show_workspace(&workspace, as_json).map(|_| 0)
        }
        WorkspaceCommand::Start(options) => super::start_daemon(options, as_json).await.map(|_| 0),
        WorkspaceCommand::Stop(options) => super::stop_daemon(options, as_json).await.map(|_| 0),
        WorkspaceCommand::GptConfig(options) => show_gpt_config(options, as_json).map(|_| 0),
        WorkspaceCommand::Test(options) => test_workspace(options, as_json).await,
    }
}

fn register_workspace(path: &str, name: Option<String>, as_json: bool) -> AppResult<()> {
    let canonical = canonical_workspace_path(path)?;
    let canonical_text = workspace_path_string(&canonical);
    let mut store = DataStore::load()?;

    if let Some(existing) = store
        .list()
        .iter()
        .find(|profile| same_workspace_path(&profile.path, &canonical))
        .cloned()
    {
        return print_mutation("already_registered", &existing, false, Vec::new(), as_json);
    }

    let requested_name = name
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let mut profile = WorkspaceProfile::new(canonical_text, requested_name);
    if store
        .list()
        .iter()
        .any(|existing| existing.name.eq_ignore_ascii_case(&profile.name))
    {
        return Err(AppError::Message(format!(
            "workspace 名称已存在：{}；请使用 --name 指定唯一名称",
            profile.name
        )));
    }
    let gateway = store.settings().mcp_gateway;
    let reserved = if gateway.enabled {
        std::collections::HashSet::from([gateway.local_port])
    } else {
        std::collections::HashSet::new()
    };
    assign_free_workspace_ports_with_reserved(store.list(), &mut profile, &reserved)?;
    assign_os_available_ports(store.list(), &mut profile)?;
    store.register_workspace(profile.clone())?;
    print_mutation("registered", &profile, false, Vec::new(), as_json)
}

async fn unregister_workspace(options: UnregisterOptions, as_json: bool) -> AppResult<i32> {
    if !options.force {
        return Err(AppError::Message(
            "注销会删除 WorkspaceProfile 和关联密钥；请显式添加 --force。项目目录不会被删除。"
                .into(),
        ));
    }
    let store = DataStore::load()?;
    let profile = super::resolve_workspace(store.list(), &options.workspace)?.clone();
    crate::mcp::gateway::ensure_workspace_is_not_owner(&store.settings().mcp_gateway, &profile.id)?;
    drop(store);

    let gateway_inspection = crate::gateway_daemon::inspect()?;
    if gateway_inspection.ambiguous {
        return Err(AppError::Message(gateway_inspection.detail));
    }
    if gateway_inspection.running
        && gateway_inspection
            .state
            .as_ref()
            .is_some_and(|gateway_state| gateway_state.workspace_ids.contains(&profile.id))
    {
        return Err(AppError::Message(
            "该 Workspace 正由 Gateway daemon 提供路由。请先执行 `anchor gateway stop`，再注销 Workspace。"
                .into(),
        ));
    }

    let inspection = super::daemon::inspect(&profile)?;
    if inspection.running {
        crate::control::request_daemon_exit_and_wait(
            &profile,
            crate::control::ControlOperation::Shutdown,
            Duration::from_secs(options.timeout_seconds),
            true,
        )
        .await?;
    } else if inspection.ambiguous {
        return Err(AppError::Message(inspection.detail));
    }

    let mut warnings = Vec::new();
    for (label, port) in [
        ("MCP", profile.runtime.local_port),
        ("Actions", profile.actions.local_port),
    ] {
        if let Some(pid) = platform().find_pid_listening_on_port(port)? {
            warnings.push(format!(
                "{label} 端口 {port} 当前由外部 PID {pid} 监听；注销不会停止该进程"
            ));
        }
    }

    drop_tunnel_workspace(&profile.id).await?;
    super::daemon::cleanup(&profile)?;
    let mut store = DataStore::load()?;
    let removed = store
        .remove(&profile.id)?
        .ok_or_else(|| AppError::Message(format!("workspace 已不存在：{}", profile.id)))?;
    print_mutation("unregistered", &removed, false, warnings, as_json)?;
    Ok(0)
}

fn show_gpt_config(options: GptConfigOptions, as_json: bool) -> AppResult<()> {
    let store = DataStore::load()?;
    let profile = super::resolve_workspace(store.list(), &options.workspace)?;
    let mut root = serde_json::Map::new();
    root.insert("workspace".into(), serde_json::to_value(identity(profile))?);
    root.insert(
        "endpointMode".into(),
        Value::String(options.endpoint.as_str().into()),
    );
    root.insert("secretsIncluded".into(), Value::Bool(options.show_secrets));

    if options.service.includes_mcp() {
        root.insert(
            "mcp".into(),
            mcp_gpt_config(&store, profile, options.endpoint, options.show_secrets)?,
        );
    }
    if options.service.includes_actions() {
        root.insert(
            "actions".into(),
            actions_gpt_config(&store, profile, options.endpoint, options.show_secrets)?,
        );
    }
    let value = Value::Object(root);

    if as_json {
        super::print_json(&value)?;
    } else {
        if options.show_secrets {
            eprintln!("警告：输出包含连接密钥，请勿粘贴到日志、工单或公共聊天。");
        }
        print_human_gpt_config(&value);
    }
    Ok(())
}

fn mcp_gpt_config(
    store: &DataStore,
    profile: &WorkspaceProfile,
    mode: EndpointSelection,
    show_secrets: bool,
) -> AppResult<Value> {
    let (endpoint, source) = select_mcp_endpoint(profile, mode)?;
    let base = endpoint.trim_end_matches("/mcp").trim_end_matches('/');
    let shared = profile.auth.use_shared_secrets;
    let auth_type = profile.auth.auth_type.as_str();
    let auth = match auth_type {
        "oauth" => {
            let client_id = if shared {
                store
                    .get_shared_secret("oauth_client_id")
                    .unwrap_or_else(|| profile.auth.oauth_client_id.clone())
            } else {
                profile.auth.oauth_client_id.clone()
            };
            let client_secret = secret(store, profile, "oauth_client_secret", shared)?;
            let password = secret(store, profile, "oauth_password", shared)?;
            let redirect_uris = redirect_uri_list(&profile.auth.oauth_redirect_uris);
            let redirect_hosts = redirect_uri_list(&profile.auth.oauth_redirect_hosts);
            json!({
                "type": "oauth",
                "clientId": client_id,
                "clientSecret": visible_secret(client_secret, show_secrets),
                "authorizationPassword": visible_secret(password, show_secrets),
                "authorizationUrl": format!("{base}/oauth/authorize"),
                "tokenUrl": format!("{base}/oauth/token"),
                "authorizationServerMetadata": format!("{base}/.well-known/oauth-authorization-server"),
                "protectedResourceMetadata": format!("{base}/.well-known/oauth-protected-resource"),
                "scope": "mcp",
                "usesSharedSecrets": shared,
                "registeredRedirectUris": redirect_uris,
                "builtInCallbackHosts": builtin_redirect_hosts(),
                "callbackEnrollmentHosts": redirect_hosts,
                "callbackRegistrationRequired": false,
                "callbackRegistrationNote": "Official ChatGPT callbacks are accepted automatically with no GUI configuration or authorization-page confirmation. Additional configured callback hosts also auto-enroll the exact redirect URI without restarting the listener or tunnel."
            })
        }
        "bearer" => {
            let token = secret(store, profile, "bearer_token", shared)?;
            json!({
                "type": "bearer",
                "bearerToken": visible_secret(token, show_secrets),
                "usesSharedSecrets": shared
            })
        }
        other => json!({ "type": other }),
    };
    Ok(json!({
        "connectorUrl": endpoint,
        "endpointSource": source,
        "auth": auth,
        "setupPath": "ChatGPT → Settings → Connectors / MCP"
    }))
}

fn actions_gpt_config(
    store: &DataStore,
    profile: &WorkspaceProfile,
    mode: EndpointSelection,
    show_secrets: bool,
) -> AppResult<Value> {
    let (base, source) = select_actions_base(profile, mode)?;
    let shared = profile.actions.use_shared_secrets;
    let auth = match profile.actions.auth_type.as_str() {
        "api_key" => {
            let key = secret(store, profile, "actions_api_key", shared)?;
            json!({
                "type": "api_key",
                "header": "Authorization",
                "scheme": "Bearer",
                "apiKey": visible_secret(key, show_secrets),
                "usesSharedSecrets": shared
            })
        }
        "oauth" => {
            let secret = secret(store, profile, "actions_oauth_client_secret", shared)?;
            let redirect_uris = redirect_uri_list(&profile.actions.oauth_redirect_uris);
            let redirect_hosts = redirect_uri_list(&profile.actions.oauth_redirect_hosts);
            json!({
                "type": "oauth",
                "clientId": profile.actions.oauth_client_id,
                "clientSecret": visible_secret(secret, show_secrets),
                "authorizationUrl": format!("{base}/oauth/authorize"),
                "tokenUrl": format!("{base}/oauth/token"),
                "scope": profile.actions.oauth_scopes,
                "usesSharedSecrets": shared,
                "registeredRedirectUris": redirect_uris,
                "builtInCallbackHosts": builtin_redirect_hosts(),
                "callbackEnrollmentHosts": redirect_hosts,
                "callbackRegistrationRequired": false,
                "callbackRegistrationNote": "Official ChatGPT callbacks are accepted automatically. Additional configured callback hosts auto-enroll the exact redirect URI without GUI interaction or service restart."
            })
        }
        other => json!({ "type": other }),
    };
    Ok(json!({
        "openApiSchemaUrl": format!("{base}/openapi.json"),
        "privacyPolicyUrl": format!("{base}/privacy"),
        "endpointSource": source,
        "auth": auth,
        "setupPath": "GPT Editor → Actions → Import from URL"
    }))
}

async fn test_workspace(options: WorkspaceTestOptions, as_json: bool) -> AppResult<i32> {
    let store = DataStore::load()?;
    let profile = super::resolve_workspace(store.list(), &options.workspace)?.clone();
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(options.timeout_seconds))
        .build()
        .map_err(|error| AppError::Message(format!("创建 HTTP 测试客户端失败：{error}")))?;
    let mut checks = Vec::new();
    if options.service.includes_mcp() {
        checks.extend(test_mcp(&client, &store, &profile, options.endpoint).await);
    }
    if options.service.includes_actions() {
        checks.extend(test_actions(&client, &store, &profile, options.endpoint).await);
    }
    let ok = checks.iter().all(|check| check.ok);
    let report = ConnectionTestReport {
        workspace: identity(&profile),
        endpoint_mode: options.endpoint.as_str().into(),
        ok,
        checks,
    };
    if as_json {
        super::print_json(&report)?;
    } else {
        println!("{} ({})", report.workspace.name, report.workspace.id);
        for check in &report.checks {
            println!(
                "{}\t{}\t{}\t{}",
                if check.ok { "PASS" } else { "FAIL" },
                check.service,
                check.name,
                check.detail
            );
            if !check.ok && !check.hint.is_empty() {
                println!("  建议：{}", check.hint);
            }
        }
    }
    Ok(if ok { 0 } else { 1 })
}

async fn test_mcp(
    client: &reqwest::Client,
    store: &DataStore,
    profile: &WorkspaceProfile,
    mode: EndpointSelection,
) -> Vec<ConnectionCheck> {
    let endpoint = match select_mcp_endpoint(profile, mode) {
        Ok((endpoint, _)) => endpoint,
        Err(error) => {
            return vec![failed_check(
                "mcp",
                "Endpoint",
                error.to_string(),
                "配置公网 URL 或改用 --local",
            )]
        }
    };
    let base = endpoint.trim_end_matches("/mcp").trim_end_matches('/');
    let mut checks = Vec::new();
    match client.get(&endpoint).send().await {
        Ok(response) => {
            let status = response.status().as_u16();
            let allow = response
                .headers()
                .get(ALLOW)
                .and_then(|value| value.to_str().ok())
                .unwrap_or("");
            let ok = status == 405
                && allow
                    .split(',')
                    .any(|method| method.trim().eq_ignore_ascii_case("POST"));
            checks.push(check(
                "mcp",
                "Streamable HTTP 入口",
                ok,
                format!("GET {endpoint} → HTTP {status}; Allow={allow}"),
                "确认服务已启动，且代理保留 405 与 Allow: POST",
            ));
        }
        Err(error) => checks.push(failed_check(
            "mcp",
            "Streamable HTTP 入口",
            error.to_string(),
            "确认服务、隧道、DNS 和防火墙状态",
        )),
    }

    match profile.auth.auth_type.as_str() {
        "oauth" => {
            checks.push(
                test_json_url(
                    client,
                    "mcp",
                    "OAuth Authorization Server Metadata",
                    &format!("{base}/.well-known/oauth-authorization-server"),
                    "token_endpoint_auth_methods_supported",
                )
                .await,
            );
            checks.push(
                test_json_url(
                    client,
                    "mcp",
                    "OAuth Protected Resource Metadata",
                    &format!("{base}/.well-known/oauth-protected-resource"),
                    "authorization_servers",
                )
                .await,
            );
            let response = client
                .post(&endpoint)
                .header("Accept", "application/json, text/event-stream")
                .json(&initialize_request())
                .send()
                .await;
            match response {
                Ok(response) => {
                    let status = response.status().as_u16();
                    let challenge = response
                        .headers()
                        .get(WWW_AUTHENTICATE)
                        .and_then(|value| value.to_str().ok())
                        .unwrap_or("");
                    checks.push(check(
                        "mcp",
                        "OAuth Challenge",
                        status == 401 && challenge.contains("resource_metadata"),
                        format!("POST initialize → HTTP {status}; WWW-Authenticate={challenge}"),
                        "确认公网代理保留 WWW-Authenticate 响应头",
                    ));
                }
                Err(error) => checks.push(failed_check(
                    "mcp",
                    "OAuth Challenge",
                    error.to_string(),
                    "确认服务和公网入口可达",
                )),
            }
        }
        "bearer" => {
            let token = secret(
                store,
                profile,
                "bearer_token",
                profile.auth.use_shared_secrets,
            )
            .ok()
            .flatten();
            checks.push(
                test_mcp_initialize(client, &endpoint, token.as_deref().map(bearer_value)).await,
            );
        }
        _ => checks.push(test_mcp_initialize(client, &endpoint, None).await),
    }
    checks
}

async fn test_actions(
    client: &reqwest::Client,
    store: &DataStore,
    profile: &WorkspaceProfile,
    mode: EndpointSelection,
) -> Vec<ConnectionCheck> {
    let base = match select_actions_base(profile, mode) {
        Ok((base, _)) => base,
        Err(error) => {
            return vec![failed_check(
                "actions",
                "Endpoint",
                error.to_string(),
                "配置公网 URL 或改用 --local",
            )]
        }
    };
    let mut checks = vec![
        test_status_url(client, "actions", "Health", &format!("{base}/health"), 200).await,
        test_json_url(
            client,
            "actions",
            "OpenAPI Schema",
            &format!("{base}/openapi.json"),
            "openapi",
        )
        .await,
        test_status_url(
            client,
            "actions",
            "Privacy Policy",
            &format!("{base}/privacy"),
            200,
        )
        .await,
    ];
    match profile.actions.auth_type.as_str() {
        "oauth" => {
            checks.push(
                test_json_url(
                    client,
                    "actions",
                    "OAuth Metadata",
                    &format!("{base}/.well-known/oauth-authorization-server"),
                    "token_endpoint_auth_methods_supported",
                )
                .await,
            );
            match client
                .post(format!("{base}/actions/server_info"))
                .json(&json!({}))
                .send()
                .await
            {
                Ok(response) => {
                    let status = response.status().as_u16();
                    let challenge = response
                        .headers()
                        .get(WWW_AUTHENTICATE)
                        .and_then(|value| value.to_str().ok())
                        .unwrap_or("");
                    checks.push(check(
                        "actions",
                        "OAuth Challenge",
                        status == 401 && challenge.contains("resource_metadata"),
                        format!("POST server_info → HTTP {status}; WWW-Authenticate={challenge}"),
                        "确认反向代理保留 WWW-Authenticate 响应头",
                    ));
                }
                Err(error) => checks.push(failed_check(
                    "actions",
                    "OAuth Challenge",
                    error.to_string(),
                    "确认 Actions 服务和公网入口可达",
                )),
            }
        }
        "api_key" => {
            let api_key = secret(
                store,
                profile,
                "actions_api_key",
                profile.actions.use_shared_secrets,
            )
            .ok()
            .flatten();
            checks.push(
                test_actions_call(
                    client,
                    &base,
                    api_key.as_deref().map(bearer_value),
                    "API Key 调用",
                )
                .await,
            );
        }
        _ => checks.push(test_actions_call(client, &base, None, "无认证调用").await),
    }
    checks
}

async fn test_actions_call(
    client: &reqwest::Client,
    base: &str,
    authorization: Option<String>,
    name: &str,
) -> ConnectionCheck {
    let mut request = client
        .post(format!("{base}/actions/server_info"))
        .json(&json!({}));
    if let Some(authorization) = authorization {
        request = request.header(AUTHORIZATION, authorization);
    }
    match request.send().await {
        Ok(response) => {
            let status = response.status().as_u16();
            let text = response.text().await.unwrap_or_default();
            check(
                "actions",
                name,
                status == 200 && text.contains("\"ok\":true"),
                format!("POST server_info → HTTP {status}"),
                "确认 Actions 认证密钥、工具暴露和服务状态",
            )
        }
        Err(error) => failed_check(
            "actions",
            name,
            error.to_string(),
            "确认 Actions 服务可达并检查认证配置",
        ),
    }
}

async fn test_mcp_initialize(
    client: &reqwest::Client,
    endpoint: &str,
    authorization: Option<String>,
) -> ConnectionCheck {
    let mut request = client
        .post(endpoint)
        .header("Accept", "application/json, text/event-stream")
        .json(&initialize_request());
    if let Some(authorization) = authorization {
        request = request.header(AUTHORIZATION, authorization);
    }
    match request.send().await {
        Ok(response) => {
            let status = response.status().as_u16();
            let session_id = response
                .headers()
                .get("mcp-session-id")
                .and_then(|value| value.to_str().ok())
                .unwrap_or("")
                .to_string();
            let protocol_version = response
                .headers()
                .get("mcp-protocol-version")
                .and_then(|value| value.to_str().ok())
                .unwrap_or("")
                .to_string();
            let text = response.text().await.unwrap_or_default();
            let ok = status == 200
                && text.contains("serverInfo")
                && !session_id.is_empty()
                && protocol_version == crate::mcp::protocol::CURRENT_PROTOCOL_VERSION;
            check(
                "mcp",
                "Initialize",
                ok,
                format!(
                    "POST initialize → HTTP {status}; session={}; protocol={protocol_version}",
                    if session_id.is_empty() {
                        "missing"
                    } else {
                        "present"
                    }
                ),
                "确认认证密钥正确，且服务返回 JSON-RPC initialize、MCP-Session-Id 与协议版本",
            )
        }
        Err(error) => failed_check(
            "mcp",
            "Initialize",
            error.to_string(),
            "确认服务可达并检查认证配置",
        ),
    }
}

async fn test_json_url(
    client: &reqwest::Client,
    service: &'static str,
    name: &str,
    url: &str,
    field: &str,
) -> ConnectionCheck {
    match client.get(url).send().await {
        Ok(response) => {
            let status = response.status().as_u16();
            let value = response.json::<Value>().await.ok();
            let ok = status == 200
                && value
                    .as_ref()
                    .is_some_and(|value| value.get(field).is_some());
            check(
                service,
                name,
                ok,
                format!("GET {url} → HTTP {status}; field={field}"),
                "确认服务配置和反向代理没有改写或缓存元数据",
            )
        }
        Err(error) => failed_check(service, name, error.to_string(), "确认 URL、DNS 和服务状态"),
    }
}

async fn test_status_url(
    client: &reqwest::Client,
    service: &'static str,
    name: &str,
    url: &str,
    expected: u16,
) -> ConnectionCheck {
    match client.get(url).send().await {
        Ok(response) => {
            let status = response.status().as_u16();
            check(
                service,
                name,
                status == expected,
                format!("GET {url} → HTTP {status}"),
                "确认 Actions 服务、隧道和反向代理状态",
            )
        }
        Err(error) => failed_check(service, name, error.to_string(), "确认 URL、DNS 和服务状态"),
    }
}

fn select_mcp_endpoint(
    profile: &WorkspaceProfile,
    mode: EndpointSelection,
) -> AppResult<(String, &'static str)> {
    let local = profile.local_endpoint();
    let public = profile.public_endpoint()?;
    select_endpoint(local, public, mode, "MCP")
}

fn select_actions_base(
    profile: &WorkspaceProfile,
    mode: EndpointSelection,
) -> AppResult<(String, &'static str)> {
    let local = profile.actions_local_base_url();
    let public = profile.actions_effective_public_url()?;
    select_endpoint(local, public, mode, "Actions")
}

fn select_endpoint(
    local: String,
    public: String,
    mode: EndpointSelection,
    label: &str,
) -> AppResult<(String, &'static str)> {
    match mode {
        EndpointSelection::Local => Ok((local, "local")),
        EndpointSelection::Public if public.is_empty() => {
            Err(AppError::Message(format!("{label} 未配置公网入口")))
        }
        EndpointSelection::Public => Ok((public, "public")),
        EndpointSelection::Auto if public.is_empty() => Ok((local, "local")),
        EndpointSelection::Auto => Ok((public, "public")),
    }
}

fn canonical_workspace_path(raw: &str) -> AppResult<PathBuf> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(AppError::Message("workspace path 不能为空".into()));
    }
    let path = PathBuf::from(trimmed);
    let canonical = path.canonicalize().map_err(|error| {
        AppError::Message(format!(
            "workspace 目录不存在或无法访问：{trimmed}（{error}）"
        ))
    })?;
    if !canonical.is_dir() {
        return Err(AppError::Message(format!(
            "workspace path 不是目录：{}",
            canonical.display()
        )));
    }
    Ok(canonical)
}

fn same_workspace_path(existing: &str, canonical: &Path) -> bool {
    Path::new(existing)
        .canonicalize()
        .map(|path| path == canonical)
        .unwrap_or_else(|_| {
            super::normalize_path(existing) == super::normalize_path(&canonical.to_string_lossy())
        })
}

fn secret(
    store: &DataStore,
    profile: &WorkspaceProfile,
    key: &str,
    shared: bool,
) -> AppResult<Option<String>> {
    if shared {
        Ok(store.get_shared_secret(key))
    } else {
        store.get_workspace_secret(&profile.id, key)
    }
}

fn visible_secret(value: Option<String>, show: bool) -> Value {
    match (value, show) {
        (Some(value), true) => json!({ "available": true, "value": value }),
        (Some(_), false) => json!({ "available": true, "value": null }),
        (None, _) => json!({ "available": false, "value": null }),
    }
}

fn bearer_value(value: &str) -> String {
    format!("Bearer {value}")
}

fn initialize_request() -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": crate::mcp::protocol::CURRENT_PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": { "name": "anchor-cli-test", "version": env!("CARGO_PKG_VERSION") }
        }
    })
}

fn identity(profile: &WorkspaceProfile) -> WorkspaceIdentity {
    WorkspaceIdentity {
        id: profile.id.clone(),
        name: profile.name.clone(),
        path: profile.path.clone(),
    }
}

fn print_mutation(
    event: &'static str,
    profile: &WorkspaceProfile,
    project_files_deleted: bool,
    warnings: Vec<String>,
    as_json: bool,
) -> AppResult<()> {
    let result = WorkspaceMutationResult {
        event,
        workspace: identity(profile),
        mcp_port: profile.runtime.local_port,
        actions_port: profile.actions.local_port,
        project_files_deleted,
        warnings,
    };
    if as_json {
        super::print_json(&result)?;
    } else {
        println!(
            "{}\t{}\t{}\tMCP:{}\tActions:{}",
            event, result.workspace.id, result.workspace.path, result.mcp_port, result.actions_port
        );
        if event == "unregistered" {
            println!("项目文件未删除。");
        }
        for warning in &result.warnings {
            eprintln!("警告：{warning}");
        }
    }
    Ok(())
}

fn print_human_gpt_config(value: &Value) {
    let workspace = &value["workspace"];
    println!(
        "{} ({})",
        workspace["name"].as_str().unwrap_or("workspace"),
        workspace["id"].as_str().unwrap_or("")
    );
    if let Some(mcp) = value.get("mcp") {
        println!("\n[MCP / ChatGPT Connector]");
        println!("URL: {}", mcp["connectorUrl"].as_str().unwrap_or(""));
        println!("Auth: {}", mcp["auth"]["type"].as_str().unwrap_or(""));
        print_optional("Client ID", &mcp["auth"]["clientId"]);
        print_secret("Client Secret", &mcp["auth"]["clientSecret"]);
        print_secret(
            "Authorization Password",
            &mcp["auth"]["authorizationPassword"],
        );
        print_secret("Bearer Token", &mcp["auth"]["bearerToken"]);
        print_redirect_uris(&mcp["auth"]);
    }
    if let Some(actions) = value.get("actions") {
        println!("\n[GPT Actions]");
        println!(
            "OpenAPI: {}",
            actions["openApiSchemaUrl"].as_str().unwrap_or("")
        );
        println!(
            "Privacy: {}",
            actions["privacyPolicyUrl"].as_str().unwrap_or("")
        );
        println!("Auth: {}", actions["auth"]["type"].as_str().unwrap_or(""));
        print_optional("Client ID", &actions["auth"]["clientId"]);
        print_secret("Client Secret", &actions["auth"]["clientSecret"]);
        print_secret("API Key", &actions["auth"]["apiKey"]);
        print_redirect_uris(&actions["auth"]);
    }
}

fn redirect_uri_list(raw: &str) -> Vec<String> {
    raw.lines()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect()
}

fn print_redirect_uris(auth: &Value) {
    let built_in_hosts = auth
        .get("builtInCallbackHosts")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let uris = auth
        .get("registeredRedirectUris")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let hosts = auth
        .get("callbackEnrollmentHosts")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for host in built_in_hosts.iter().filter_map(Value::as_str) {
        println!("Built-in Callback Host: {host} (automatic)");
    }
    for uri in uris.iter().filter_map(Value::as_str) {
        println!("Registered Callback: {uri}");
    }
    for host in hosts.iter().filter_map(Value::as_str) {
        println!("Additional Callback Host: {host} (automatic)");
    }
}

fn print_optional(label: &str, value: &Value) {
    if let Some(value) = value.as_str().filter(|value| !value.is_empty()) {
        println!("{label}: {value}");
    }
}

fn print_secret(label: &str, value: &Value) {
    let Some(value) = value.as_object() else {
        return;
    };
    let available = value
        .get("available")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let visible = value.get("value").and_then(Value::as_str);
    if let Some(visible) = visible {
        println!("{label}: {visible}");
    } else if available {
        println!("{label}: <hidden; use --show-secrets>");
    } else {
        println!("{label}: <not configured>");
    }
}

fn check(
    service: &'static str,
    name: &str,
    ok: bool,
    detail: String,
    hint: &str,
) -> ConnectionCheck {
    ConnectionCheck {
        service,
        name: name.into(),
        ok,
        detail,
        hint: if ok { String::new() } else { hint.into() },
    }
}

fn failed_check(service: &'static str, name: &str, detail: String, hint: &str) -> ConnectionCheck {
    check(service, name, false, detail, hint)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_auto_prefers_public_and_falls_back_to_local() {
        assert_eq!(
            select_endpoint(
                "local".into(),
                "public".into(),
                EndpointSelection::Auto,
                "MCP"
            )
            .expect("public"),
            ("public".into(), "public")
        );
        assert_eq!(
            select_endpoint(
                "local".into(),
                String::new(),
                EndpointSelection::Auto,
                "MCP"
            )
            .expect("local"),
            ("local".into(), "local")
        );
    }

    #[test]
    fn public_endpoint_requires_configuration() {
        let error = select_endpoint(
            "local".into(),
            String::new(),
            EndpointSelection::Public,
            "MCP",
        )
        .expect_err("missing public");
        assert!(error.to_string().contains("未配置公网入口"));
    }

    #[test]
    fn secrets_are_redacted_by_default() {
        assert_eq!(
            visible_secret(Some("secret".into()), false),
            json!({"available": true, "value": null})
        );
        assert_eq!(
            visible_secret(Some("secret".into()), true),
            json!({"available": true, "value": "secret"})
        );
    }

    #[test]
    fn workspace_path_string_is_stable_for_normal_paths() {
        let path = Path::new("/srv/workspace");
        assert_eq!(workspace_path_string(path), "/srv/workspace");
    }
}
