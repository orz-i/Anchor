use serde::Serialize;
use serde_json::Value;

use crate::data::DataStore;
use crate::error::{AppError, AppResult};
use crate::secret::SecretStore;
use crate::settings::{AppSettings, FrpProfile};
use crate::tunnel::{validate_workspace_frp_config, TunnelServiceKind};
use crate::workspace::WorkspaceProfile;

use super::args::{
    ConfigApplyOptions, ConfigAssignment, ConfigMutationOptions, ServiceSelection, TunnelCommand,
    TunnelConfigureOptions, TunnelShowOptions,
};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TunnelShowReport {
    event: &'static str,
    workspace_id: String,
    workspace_name: String,
    services: Vec<TunnelServiceView>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TunnelServiceView {
    service: &'static str,
    tunnel_type: String,
    public_url: String,
    effective_public_url: String,
    use_proxy: bool,
    cloudflare_mode: String,
    frp: FrpBindingView,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct FrpBindingView {
    profile_id: String,
    profile_name: String,
    server: String,
    server_port: u16,
    subdomain: String,
    proxy_type: String,
    cert_path: String,
    key_path: String,
    has_token: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TunnelConfigureReport {
    event: &'static str,
    workspace: String,
    service: String,
    assignments: Vec<ConfigAssignmentView>,
    staged: Option<Value>,
    applied: Option<Value>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ConfigAssignmentView {
    path: String,
    value: String,
}

pub async fn execute(command: TunnelCommand, _as_json: bool) -> AppResult<i32> {
    match command {
        TunnelCommand::Show(options) => show(options)?,
        TunnelCommand::Configure(options) => configure(*options).await?,
    }
    Ok(0)
}

fn show(options: TunnelShowOptions) -> AppResult<()> {
    let store = DataStore::load()?;
    let workspace = super::resolve_workspace(store.list(), &options.workspace)?.clone();
    let settings = store.settings();
    drop(store);

    let services = service_kinds(options.service)
        .into_iter()
        .map(|kind| service_view(&workspace, kind, &settings))
        .collect::<AppResult<Vec<_>>>()?;
    super::print_json(&TunnelShowReport {
        event: "tunnel_show",
        workspace_id: workspace.id,
        workspace_name: workspace.name,
        services,
    })
}

async fn configure(options: TunnelConfigureOptions) -> AppResult<()> {
    let settings = DataStore::load()?.settings();
    let assignments = build_assignments(&options, &settings)?;

    let mut staged = None;
    if !assignments.is_empty() {
        let mutation = ConfigMutationOptions {
            workspace: options.workspace.clone(),
            assignments: assignments.clone(),
        };
        let (_active, candidate) = super::config::preview_config(&mutation)?;
        validate_candidate_tunnel(&candidate, options.service, &settings)?;
        staged = Some(serde_json::to_value(super::config::stage_config(
            mutation,
        )?)?);
    }

    let applied = if options.apply {
        Some(serde_json::to_value(
            super::config::apply_staged_config(ConfigApplyOptions {
                workspace: options.workspace.clone(),
                wait_seconds: options.wait_seconds,
            })
            .await?,
        )?)
    } else {
        None
    };

    super::print_json(&TunnelConfigureReport {
        event: "tunnel_configure",
        workspace: options.workspace,
        service: service_selection_name(options.service).into(),
        assignments: assignments
            .into_iter()
            .map(|assignment| ConfigAssignmentView {
                path: assignment.path,
                value: assignment.value,
            })
            .collect(),
        staged,
        applied,
    })
}

fn build_assignments(
    options: &TunnelConfigureOptions,
    settings: &AppSettings,
) -> AppResult<Vec<ConfigAssignment>> {
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

    let implied_type = if has_frp_configuration {
        Some("frp")
    } else if options.cloudflare_mode.is_some() {
        Some("cloudflare")
    } else {
        None
    };
    let tunnel_type = options.tunnel_type.as_deref().or(implied_type);
    let mut assignments = Vec::new();

    for kind in service_kinds(options.service) {
        let prefix = config_prefix(kind);
        if let Some(value) = tunnel_type {
            assignments.push(assignment(type_path(kind), value));
        }
        if let Some(profile_id) = frp_profile_id.as_deref() {
            assignments.push(assignment(format!("{prefix}.frp_profile_id"), profile_id));
        } else if options.clear_frp_profile || options.frp_server.is_some() {
            assignments.push(assignment(format!("{prefix}.frp_profile_id"), ""));
        }
        if let Some(value) = options.frp_server.as_deref() {
            assignments.push(assignment(format!("{prefix}.frp_server"), value.trim()));
        }
        if let Some(value) = options.frp_server_port {
            assignments.push(assignment(
                format!("{prefix}.frp_server_port"),
                value.to_string(),
            ));
        }
        if let Some(value) = options.frp_subdomain.as_deref() {
            let value = value.trim();
            if value.is_empty() {
                return Err(AppError::Message("FRP subdomain 不能为空".into()));
            }
            assignments.push(assignment(format!("{prefix}.frp_subdomain"), value));
        }
        if let Some(value) = options.public_url.as_deref() {
            assignments.push(assignment(format!("{prefix}.public_url"), value.trim()));
        }
        if let Some(value) = options.frp_proxy_type.as_deref() {
            assignments.push(assignment(format!("{prefix}.frp_proxy_type"), value));
        }
        if let Some(value) = options.frp_cert_path.as_deref() {
            assignments.push(assignment(format!("{prefix}.frp_cert_path"), value.trim()));
        }
        if let Some(value) = options.frp_key_path.as_deref() {
            assignments.push(assignment(format!("{prefix}.frp_key_path"), value.trim()));
        }
        if let Some(value) = options.cloudflare_mode.as_deref() {
            assignments.push(assignment(format!("{prefix}.cloudflare_mode"), value));
        }
        if let Some(value) = options.use_proxy {
            assignments.push(assignment(format!("{prefix}.use_proxy"), value.to_string()));
        }
    }
    Ok(assignments)
}

fn validate_candidate_tunnel(
    candidate: &WorkspaceProfile,
    service: ServiceSelection,
    settings: &AppSettings,
) -> AppResult<()> {
    for kind in service_kinds(service) {
        let tunnel_type = match kind {
            TunnelServiceKind::Mcp => candidate.tunnel.tunnel_type.as_str(),
            TunnelServiceKind::Actions => candidate.actions.tunnel_type.as_str(),
        };
        if tunnel_type == "frp" {
            validate_workspace_frp_config(candidate, kind, settings)?;
        }
    }
    Ok(())
}

fn service_view(
    workspace: &WorkspaceProfile,
    kind: TunnelServiceKind,
    settings: &AppSettings,
) -> AppResult<TunnelServiceView> {
    let (
        service,
        tunnel_type,
        public_url,
        use_proxy,
        cloudflare_mode,
        profile_id,
        inline_server,
        inline_port,
        subdomain,
        proxy_type,
        cert_path,
        key_path,
        secret_key,
    ) = match kind {
        TunnelServiceKind::Mcp => (
            "mcp",
            workspace.tunnel.tunnel_type.as_str(),
            workspace.tunnel.public_url.as_str(),
            workspace.tunnel.use_proxy,
            workspace.tunnel.cloudflare_mode.as_str(),
            workspace.tunnel.frp_profile_id.as_str(),
            workspace.tunnel.frp_server.as_str(),
            workspace.tunnel.frp_server_port,
            workspace.tunnel.frp_subdomain.as_str(),
            workspace.tunnel.frp_proxy_type.as_str(),
            workspace.tunnel.frp_cert_path.as_str(),
            workspace.tunnel.frp_key_path.as_str(),
            "frp_token",
        ),
        TunnelServiceKind::Actions => (
            "actions",
            workspace.actions.tunnel_type.as_str(),
            workspace.actions.public_url.as_str(),
            workspace.actions.use_proxy,
            workspace.actions.cloudflare_mode.as_str(),
            workspace.actions.frp_profile_id.as_str(),
            workspace.actions.frp_server.as_str(),
            workspace.actions.frp_server_port,
            workspace.actions.frp_subdomain.as_str(),
            workspace.actions.frp_proxy_type.as_str(),
            workspace.actions.frp_cert_path.as_str(),
            workspace.actions.frp_key_path.as_str(),
            "actions_frp_token",
        ),
    };
    let selected_profile = settings.find_frp_profile(profile_id);
    let server = selected_profile
        .map(|profile| profile.server.as_str())
        .unwrap_or(inline_server);
    let server_port = selected_profile
        .map(|profile| profile.server_port)
        .unwrap_or(inline_port);
    let has_token = frp_token_configured(
        workspace,
        profile_id,
        inline_server,
        secret_key,
        &settings.frp_profiles,
    )?;
    Ok(TunnelServiceView {
        service,
        tunnel_type: tunnel_type.into(),
        public_url: public_url.into(),
        effective_public_url: match kind {
            TunnelServiceKind::Mcp => workspace.effective_public_url_with(settings),
            TunnelServiceKind::Actions => workspace.actions_effective_public_url_with(settings),
        },
        use_proxy,
        cloudflare_mode: cloudflare_mode.into(),
        frp: FrpBindingView {
            profile_id: profile_id.into(),
            profile_name: selected_profile
                .map(|profile| profile.name.clone())
                .unwrap_or_default(),
            server: server.into(),
            server_port,
            subdomain: subdomain.into(),
            proxy_type: proxy_type.into(),
            cert_path: cert_path.into(),
            key_path: key_path.into(),
            has_token,
        },
    })
}

fn frp_token_configured(
    workspace: &WorkspaceProfile,
    profile_id: &str,
    inline_server: &str,
    workspace_secret_key: &str,
    profiles: &[FrpProfile],
) -> AppResult<bool> {
    if !profile_id.trim().is_empty()
        && SecretStore::get_app("frp_profile_token", profile_id)?
            .is_some_and(|value| !value.trim().is_empty())
    {
        return Ok(true);
    }
    if SecretStore::get(&workspace.id, workspace_secret_key)?
        .is_some_and(|value| !value.trim().is_empty())
    {
        return Ok(true);
    }
    let inline_server = inline_server.trim();
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

fn config_prefix(kind: TunnelServiceKind) -> &'static str {
    match kind {
        TunnelServiceKind::Mcp => "tunnel",
        TunnelServiceKind::Actions => "actions",
    }
}

fn type_path(kind: TunnelServiceKind) -> &'static str {
    match kind {
        TunnelServiceKind::Mcp => "tunnel.type",
        TunnelServiceKind::Actions => "actions.tunnel_type",
    }
}

fn assignment(path: impl Into<String>, value: impl Into<String>) -> ConfigAssignment {
    ConfigAssignment {
        path: path.into(),
        value: value.into(),
    }
}

fn service_kinds(service: ServiceSelection) -> Vec<TunnelServiceKind> {
    match service {
        ServiceSelection::Mcp => vec![TunnelServiceKind::Mcp],
        ServiceSelection::Actions => vec![TunnelServiceKind::Actions],
        ServiceSelection::All => vec![TunnelServiceKind::Mcp, TunnelServiceKind::Actions],
    }
}

fn service_selection_name(service: ServiceSelection) -> &'static str {
    match service {
        ServiceSelection::Mcp => "mcp",
        ServiceSelection::Actions => "actions",
        ServiceSelection::All => "all",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frp_settings() -> AppSettings {
        AppSettings {
            frp_profiles: vec![FrpProfile {
                id: "p1".into(),
                name: "production".into(),
                server: "frp.example.com".into(),
                server_port: 17001,
            }],
            ..AppSettings::default()
        }
    }

    fn options() -> TunnelConfigureOptions {
        TunnelConfigureOptions {
            workspace: "demo".into(),
            service: ServiceSelection::Mcp,
            tunnel_type: None,
            frp_profile: None,
            clear_frp_profile: false,
            frp_server: None,
            frp_server_port: None,
            frp_subdomain: None,
            public_url: None,
            frp_proxy_type: None,
            frp_cert_path: None,
            frp_key_path: None,
            cloudflare_mode: None,
            use_proxy: None,
            apply: false,
            wait_seconds: 30,
        }
    }

    #[test]
    fn global_profile_configuration_resolves_id_and_implies_frp_for_both_services() {
        let mut options = options();
        options.service = ServiceSelection::All;
        options.frp_profile = Some("production".into());
        options.frp_subdomain = Some("anchor".into());
        let assignments = build_assignments(&options, &frp_settings()).expect("assignments");

        for path in ["tunnel.type", "actions.tunnel_type"] {
            assert!(assignments
                .iter()
                .any(|assignment| assignment.path == path && assignment.value == "frp"));
        }
        for path in ["tunnel.frp_profile_id", "actions.frp_profile_id"] {
            assert!(assignments
                .iter()
                .any(|assignment| assignment.path == path && assignment.value == "p1"));
        }
    }

    #[test]
    fn manual_server_clears_global_profile_binding() {
        let mut options = options();
        options.frp_server = Some("43.157.17.95".into());
        options.frp_server_port = Some(17_001);
        options.frp_subdomain = Some("anchor".into());
        options.public_url = Some("https://anchor.taoyan.icu".into());
        let assignments = build_assignments(&options, &frp_settings()).expect("assignments");
        assert!(assignments.iter().any(|assignment| {
            assignment.path == "tunnel.frp_profile_id" && assignment.value.is_empty()
        }));
        assert!(assignments.iter().any(|assignment| {
            assignment.path == "tunnel.frp_server" && assignment.value == "43.157.17.95"
        }));
    }

    #[test]
    fn cloudflare_type_rejects_frp_specific_flags() {
        let mut options = options();
        options.tunnel_type = Some("cloudflare".into());
        options.frp_subdomain = Some("anchor".into());
        assert!(build_assignments(&options, &frp_settings()).is_err());
    }
}
