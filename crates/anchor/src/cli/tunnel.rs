use serde::Serialize;

use crate::data::DataStore;
use crate::error::{AppError, AppResult};
use crate::management;
use crate::secret::SecretStore;
use crate::settings::{AppSettings, FrpProfile};
use crate::tunnel::{validate_workspace_frp_config, TunnelProfile, TunnelServiceKind};

use super::args::{TunnelCommand, TunnelConfigureOptions};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TunnelView {
    id: String,
    name: String,
    workspace_id: String,
    workspace_name: String,
    service: String,
    enabled: bool,
    tunnel_type: String,
    public_url: String,
    effective_public_url: String,
    use_proxy: bool,
    cloudflare_mode: String,
    frp_profile_id: String,
    frp_profile_name: String,
    frp_server: String,
    frp_server_port: u16,
    frp_subdomain: String,
    frp_proxy_type: String,
    frp_cert_path: String,
    frp_key_path: String,
    has_frp_token: bool,
    has_cloudflare_token: bool,
}

pub async fn execute(command: TunnelCommand) -> AppResult<i32> {
    match command {
        TunnelCommand::List => list()?,
        TunnelCommand::Create { workspace, name } => create(&workspace, name.as_deref())?,
        TunnelCommand::Show { tunnel } => show(&tunnel)?,
        TunnelCommand::Configure(options) => configure(*options).await?,
        TunnelCommand::Enable { tunnel } => set_enabled(&tunnel, true).await?,
        TunnelCommand::Disable { tunnel } => set_enabled(&tunnel, false).await?,
        TunnelCommand::Delete { tunnel } => delete(&tunnel).await?,
        TunnelCommand::Status { tunnel } => status(&tunnel).await?,
        TunnelCommand::Start { tunnel } => start(&tunnel).await?,
        TunnelCommand::Stop { tunnel } => stop(&tunnel).await?,
        TunnelCommand::Restart { tunnel } => restart(&tunnel).await?,
        TunnelCommand::Test { tunnel } => test(&tunnel).await?,
        TunnelCommand::SecretSet { tunnel, key, token } => set_secret(&tunnel, &key, token).await?,
        TunnelCommand::SecretClear { tunnel, key } => clear_secret(&tunnel, &key).await?,
    }
    Ok(0)
}

fn list() -> AppResult<()> {
    let store = DataStore::load()?;
    let settings = store.settings();
    let views = store
        .list_tunnels()
        .iter()
        .map(|tunnel| tunnel_view(tunnel, &store, &settings))
        .collect::<AppResult<Vec<_>>>()?;
    super::print_json(&views)
}

fn create(workspace_selector: &str, name: Option<&str>) -> AppResult<()> {
    let store = DataStore::load()?;
    let workspace = super::resolve_workspace(store.list(), workspace_selector)?.clone();
    drop(store);
    let tunnel = management::create_tunnel(&workspace.id, name)?;
    super::print_json(&tunnel)
}

fn show(selector: &str) -> AppResult<()> {
    let store = DataStore::load()?;
    let settings = store.settings();
    let tunnel = resolve_tunnel(store.list_tunnels(), selector)?;
    super::print_json(&tunnel_view(tunnel, &store, &settings)?)
}

async fn configure(options: TunnelConfigureOptions) -> AppResult<()> {
    let store = DataStore::load()?;
    let settings = store.settings();
    let mut tunnel = resolve_tunnel(store.list_tunnels(), &options.tunnel)?.clone();
    let mut workspace = store.get(&tunnel.workspace_id).cloned().ok_or_else(|| {
        AppError::Message(format!("workspace not found: {}", tunnel.workspace_id))
    })?;
    drop(store);

    apply_config_options(&mut tunnel, &options, &settings)?;
    workspace.tunnel = tunnel.config.clone();
    workspace.tunnel_id = tunnel.id.clone();
    if tunnel.config.tunnel_type == "frp" {
        validate_workspace_frp_config(&workspace, TunnelServiceKind::Mcp, &settings)?;
    }

    let saved = management::update_tunnel(tunnel).await?;
    super::print_json(&saved)
}

async fn set_enabled(selector: &str, enabled: bool) -> AppResult<()> {
    let store = DataStore::load()?;
    let mut tunnel = resolve_tunnel(store.list_tunnels(), selector)?.clone();
    drop(store);
    if tunnel.enabled == enabled {
        return super::print_json(&tunnel);
    }
    tunnel.enabled = enabled;
    let tunnel = management::update_tunnel(tunnel).await?;
    super::print_json(&tunnel)
}

async fn delete(selector: &str) -> AppResult<()> {
    let id = {
        let store = DataStore::load()?;
        resolve_tunnel(store.list_tunnels(), selector)?.id.clone()
    };
    management::delete_tunnel(&id).await?;
    super::print_json(&serde_json::json!({"event": "tunnel_deleted", "id": id}))
}

async fn status(selector: &str) -> AppResult<()> {
    let id = resolve_tunnel_id(selector)?;
    super::print_json(&management::get_tunnel_status(&id).await?)
}

async fn start(selector: &str) -> AppResult<()> {
    let id = resolve_tunnel_id(selector)?;
    super::print_json(&management::start_tunnel(&id).await?)
}

async fn stop(selector: &str) -> AppResult<()> {
    let id = resolve_tunnel_id(selector)?;
    super::print_json(&management::stop_tunnel(&id).await?)
}

async fn restart(selector: &str) -> AppResult<()> {
    let id = resolve_tunnel_id(selector)?;
    let _ = management::stop_tunnel(&id).await?;
    super::print_json(&management::start_tunnel(&id).await?)
}

async fn test(selector: &str) -> AppResult<()> {
    let id = resolve_tunnel_id(selector)?;
    super::print_json(&management::test_tunnel(&id).await?)
}

async fn set_secret(selector: &str, key: &str, token: super::args::FrpTokenInput) -> AppResult<()> {
    let id = resolve_tunnel_id(selector)?;
    let token = super::frp::read_token_input(Some(token))?.unwrap_or_default();
    let api_key = secret_api_key(key)?;
    management::set_tunnel_secret(&id, api_key, &token).await?;
    super::print_json(&serde_json::json!({
        "event": "tunnel_secret_updated",
        "id": id,
        "key": key,
        "configured": true
    }))
}

async fn clear_secret(selector: &str, key: &str) -> AppResult<()> {
    let id = resolve_tunnel_id(selector)?;
    management::set_tunnel_secret(&id, secret_api_key(key)?, "").await?;
    super::print_json(&serde_json::json!({
        "event": "tunnel_secret_updated",
        "id": id,
        "key": key,
        "configured": false
    }))
}

fn secret_api_key(key: &str) -> AppResult<&'static str> {
    match key {
        "frp" => Ok("frp_token"),
        "cloudflare" => Ok("cloudflare_token"),
        _ => Err(AppError::Message(format!(
            "unsupported tunnel secret key: {key}"
        ))),
    }
}

fn resolve_tunnel_id(selector: &str) -> AppResult<String> {
    let store = DataStore::load()?;
    Ok(resolve_tunnel(store.list_tunnels(), selector)?.id.clone())
}

pub(super) fn resolve_tunnel<'a>(
    tunnels: &'a [TunnelProfile],
    selector: &str,
) -> AppResult<&'a TunnelProfile> {
    if let Some(tunnel) = tunnels.iter().find(|tunnel| tunnel.id == selector) {
        return Ok(tunnel);
    }
    let matches = tunnels
        .iter()
        .filter(|tunnel| tunnel.name.eq_ignore_ascii_case(selector))
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [tunnel] => Ok(*tunnel),
        [] => Err(AppError::Message(format!("tunnel not found: {selector}"))),
        _ => Err(AppError::Message(format!(
            "tunnel name is ambiguous: {selector}; use tunnel id"
        ))),
    }
}

fn apply_config_options(
    tunnel: &mut TunnelProfile,
    options: &TunnelConfigureOptions,
    settings: &AppSettings,
) -> AppResult<()> {
    let frp_profile_id = options
        .frp_profile
        .as_deref()
        .map(|selector| super::frp::resolve_profile_id(&settings.frp_profiles, selector))
        .transpose()?;
    let has_frp_configuration = frp_profile_id.is_some()
        || options.frp_server.is_some()
        || options.frp_server_port.is_some()
        || options.frp_subdomain.is_some()
        || options.frp_proxy_type.is_some()
        || options.frp_cert_path.is_some()
        || options.frp_key_path.is_some();
    if options.tunnel_type.as_deref() == Some("cloudflare") && has_frp_configuration {
        return Err(AppError::Message(
            "--type cloudflare 不能与 FRP 配置参数同时使用".into(),
        ));
    }
    if options.cloudflare_mode.is_some()
        && (options.tunnel_type.as_deref() == Some("frp") || has_frp_configuration)
    {
        return Err(AppError::Message(
            "--cloudflare-mode 不能与 FRP 配置参数同时使用".into(),
        ));
    }
    if frp_profile_id.is_some() && options.frp_server_port.is_some() {
        return Err(AppError::Message(
            "使用 --frp-profile 时服务器端口来自全局 profile；不要同时传 --frp-port".into(),
        ));
    }

    if let Some(name) = options.name.as_deref() {
        let name = name.trim();
        if name.is_empty() {
            return Err(AppError::Message("Tunnel 名称不能为空".into()));
        }
        tunnel.name = name.to_string();
    }
    let implied_type = if has_frp_configuration {
        Some("frp")
    } else if options.cloudflare_mode.is_some() {
        Some("cloudflare")
    } else {
        None
    };
    if let Some(value) = options.tunnel_type.as_deref().or(implied_type) {
        tunnel.config.tunnel_type = value.to_string();
    }
    if let Some(profile_id) = frp_profile_id {
        tunnel.config.frp_profile_id = profile_id;
    } else if options.clear_frp_profile || options.frp_server.is_some() {
        tunnel.config.frp_profile_id.clear();
    }
    if let Some(value) = options.frp_server.as_deref() {
        tunnel.config.frp_server = value.trim().to_string();
    }
    if let Some(value) = options.frp_server_port {
        tunnel.config.frp_server_port = value;
    }
    if let Some(value) = options.frp_subdomain.as_deref() {
        let value = value.trim();
        if value.is_empty() {
            return Err(AppError::Message("FRP subdomain 不能为空".into()));
        }
        tunnel.config.frp_subdomain = value.to_string();
    }
    if let Some(value) = options.public_url.as_deref() {
        tunnel.config.public_url = value.trim().trim_end_matches('/').to_string();
    }
    if let Some(value) = options.frp_proxy_type.as_deref() {
        tunnel.config.frp_proxy_type = value.to_string();
    }
    if let Some(value) = options.frp_cert_path.as_deref() {
        tunnel.config.frp_cert_path = value.trim().to_string();
    }
    if let Some(value) = options.frp_key_path.as_deref() {
        tunnel.config.frp_key_path = value.trim().to_string();
    }
    if let Some(value) = options.cloudflare_mode.as_deref() {
        tunnel.config.cloudflare_mode = value.to_string();
    }
    if let Some(value) = options.use_proxy {
        tunnel.config.use_proxy = value;
    }
    Ok(())
}

fn tunnel_view(
    tunnel: &TunnelProfile,
    store: &DataStore,
    settings: &AppSettings,
) -> AppResult<TunnelView> {
    let workspace = store.get(&tunnel.workspace_id).ok_or_else(|| {
        AppError::Message(format!("workspace not found: {}", tunnel.workspace_id))
    })?;
    let selected_profile = settings.find_frp_profile(&tunnel.config.frp_profile_id);
    let frp_server = selected_profile
        .map(|profile| profile.server.clone())
        .unwrap_or_else(|| tunnel.config.frp_server.clone());
    let frp_server_port = selected_profile
        .map(|profile| profile.server_port)
        .unwrap_or(tunnel.config.frp_server_port);
    Ok(TunnelView {
        id: tunnel.id.clone(),
        name: tunnel.name.clone(),
        workspace_id: tunnel.workspace_id.clone(),
        workspace_name: workspace.name.clone(),
        service: tunnel.service.clone(),
        enabled: tunnel.enabled,
        tunnel_type: tunnel.config.tunnel_type.clone(),
        public_url: tunnel.config.public_url.clone(),
        effective_public_url: tunnel.effective_public_url(settings),
        use_proxy: tunnel.config.use_proxy,
        cloudflare_mode: tunnel.config.cloudflare_mode.clone(),
        frp_profile_id: tunnel.config.frp_profile_id.clone(),
        frp_profile_name: selected_profile
            .map(|profile| profile.name.clone())
            .unwrap_or_default(),
        frp_server,
        frp_server_port,
        frp_subdomain: tunnel.config.frp_subdomain.clone(),
        frp_proxy_type: tunnel.config.frp_proxy_type.clone(),
        frp_cert_path: tunnel.config.frp_cert_path.clone(),
        frp_key_path: tunnel.config.frp_key_path.clone(),
        has_frp_token: frp_token_configured(tunnel, &settings.frp_profiles)?,
        has_cloudflare_token: SecretStore::get_app("tunnel_cloudflare_token", &tunnel.id)?
            .is_some_and(|value| !value.trim().is_empty()),
    })
}

fn frp_token_configured(tunnel: &TunnelProfile, profiles: &[FrpProfile]) -> AppResult<bool> {
    if !tunnel.config.frp_profile_id.trim().is_empty()
        && SecretStore::get_app("frp_profile_token", &tunnel.config.frp_profile_id)?
            .is_some_and(|value| !value.trim().is_empty())
    {
        return Ok(true);
    }
    if SecretStore::get_app("tunnel_frp_token", &tunnel.id)?
        .is_some_and(|value| !value.trim().is_empty())
    {
        return Ok(true);
    }
    let inline_server = tunnel.config.frp_server.trim();
    if inline_server.is_empty() {
        return Ok(false);
    }
    for profile in profiles {
        if profile.server.trim().eq_ignore_ascii_case(inline_server)
            && SecretStore::get_app("frp_profile_token", &profile.id)?
                .is_some_and(|value| !value.trim().is_empty())
        {
            return Ok(true);
        }
    }
    Ok(false)
}
