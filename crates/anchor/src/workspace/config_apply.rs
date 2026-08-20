use super::WorkspaceProfile;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceConfigApplyPlan {
    pub mcp_listener_reload: bool,
    pub actions_listener_reload: bool,
    pub mcp_callback_policy_hot_update: bool,
    pub actions_callback_policy_hot_update: bool,
    pub mcp_tunnel_changed: bool,
    pub actions_tunnel_changed: bool,
}

impl WorkspaceConfigApplyPlan {
    pub fn has_changes(self) -> bool {
        self.mcp_listener_reload
            || self.actions_listener_reload
            || self.mcp_callback_policy_hot_update
            || self.actions_callback_policy_hot_update
            || self.mcp_tunnel_changed
            || self.actions_tunnel_changed
    }
}

pub fn plan_workspace_config_apply(
    current: &WorkspaceProfile,
    next: &WorkspaceProfile,
) -> WorkspaceConfigApplyPlan {
    let path_changed = current.path != next.path;
    let mcp_callback_policy_hot_update = current.auth.oauth_redirect_uris
        != next.auth.oauth_redirect_uris
        || current.auth.oauth_redirect_hosts != next.auth.oauth_redirect_hosts;
    let actions_callback_policy_hot_update = current.actions.oauth_redirect_uris
        != next.actions.oauth_redirect_uris
        || current.actions.oauth_redirect_hosts != next.actions.oauth_redirect_hosts;

    WorkspaceConfigApplyPlan {
        mcp_listener_reload: path_changed
            || mcp_runtime_changed(current, next)
            || mcp_auth_listener_changed(current, next),
        actions_listener_reload: path_changed
            || actions_runtime_changed(current, next)
            || actions_auth_listener_changed(current, next),
        mcp_callback_policy_hot_update,
        actions_callback_policy_hot_update,
        mcp_tunnel_changed: mcp_tunnel_changed(current, next),
        actions_tunnel_changed: actions_tunnel_changed(current, next),
    }
}

fn mcp_tunnel_changed(current: &WorkspaceProfile, next: &WorkspaceProfile) -> bool {
    current.tunnel.tunnel_type != next.tunnel.tunnel_type
        || current.tunnel.public_url != next.tunnel.public_url
        || current.tunnel.frp_server != next.tunnel.frp_server
        || current.tunnel.frp_subdomain != next.tunnel.frp_subdomain
        || current.tunnel.frp_profile_id != next.tunnel.frp_profile_id
        || current.tunnel.frp_server_port != next.tunnel.frp_server_port
        || current.tunnel.frp_proxy_type != next.tunnel.frp_proxy_type
        || current.tunnel.frp_cert_path != next.tunnel.frp_cert_path
        || current.tunnel.frp_key_path != next.tunnel.frp_key_path
        || current.tunnel.cloudflare_mode != next.tunnel.cloudflare_mode
        || current.tunnel.use_proxy != next.tunnel.use_proxy
}

fn actions_tunnel_changed(current: &WorkspaceProfile, next: &WorkspaceProfile) -> bool {
    current.actions.public_url != next.actions.public_url
        || current.actions.tunnel_type != next.actions.tunnel_type
        || current.actions.frp_server != next.actions.frp_server
        || current.actions.frp_subdomain != next.actions.frp_subdomain
        || current.actions.frp_profile_id != next.actions.frp_profile_id
        || current.actions.frp_server_port != next.actions.frp_server_port
        || current.actions.frp_proxy_type != next.actions.frp_proxy_type
        || current.actions.frp_cert_path != next.actions.frp_cert_path
        || current.actions.frp_key_path != next.actions.frp_key_path
        || current.actions.cloudflare_mode != next.actions.cloudflare_mode
        || current.actions.use_proxy != next.actions.use_proxy
}

fn mcp_runtime_changed(current: &WorkspaceProfile, next: &WorkspaceProfile) -> bool {
    current.runtime.local_port != next.runtime.local_port
        || current.runtime.tool_profile != next.runtime.tool_profile
        || current.runtime.permission_mode != next.runtime.permission_mode
        || current.runtime.preferred_shell != next.runtime.preferred_shell
        || current.runtime.runtime_command != next.runtime.runtime_command
        || current.runtime.mcp_config != next.runtime.mcp_config
        || current.runtime.allowed_commands != next.runtime.allowed_commands
        || current.runtime.workspace_local_entries != next.runtime.workspace_local_entries
        || current.runtime.workspace_script_extensions != next.runtime.workspace_script_extensions
        || current.runtime.skill_service_enabled != next.runtime.skill_service_enabled
        || current.runtime.skill_roots != next.runtime.skill_roots
        || current.runtime.strict_workspace_reads != next.runtime.strict_workspace_reads
        || current.runtime.external_paid_commands_enabled
            != next.runtime.external_paid_commands_enabled
        || current.runtime.external_paid_max_runs_per_day
            != next.runtime.external_paid_max_runs_per_day
        || current.runtime.external_paid_max_duration_seconds
            != next.runtime.external_paid_max_duration_seconds
}

fn mcp_auth_listener_changed(current: &WorkspaceProfile, next: &WorkspaceProfile) -> bool {
    current.auth.auth_type != next.auth.auth_type
        || current.auth.oauth_client_id != next.auth.oauth_client_id
        || current.auth.use_shared_secrets != next.auth.use_shared_secrets
}

fn actions_runtime_changed(current: &WorkspaceProfile, next: &WorkspaceProfile) -> bool {
    current.actions.local_port != next.actions.local_port
        || current.actions.permission_mode != next.actions.permission_mode
        || current.actions.runtime_command != next.actions.runtime_command
        || current.actions.oauth_scopes != next.actions.oauth_scopes
        || current.actions.allowed_commands != next.actions.allowed_commands
        || current.actions.max_patch_bytes != next.actions.max_patch_bytes
}

fn actions_auth_listener_changed(current: &WorkspaceProfile, next: &WorkspaceProfile) -> bool {
    current.actions.auth_type != next.actions.auth_type
        || current.actions.oauth_client_id != next.actions.oauth_client_id
        || current.actions.use_shared_secrets != next.actions.use_shared_secrets
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile() -> WorkspaceProfile {
        WorkspaceProfile::new("C:/workspace/demo".into(), Some("demo".into()))
    }

    #[test]
    fn metadata_only_changes_do_not_reload_listeners() {
        let current = profile();
        let mut next = current.clone();
        next.name = "renamed".into();

        let plan = plan_workspace_config_apply(&current, &next);
        assert!(!plan.mcp_listener_reload);
        assert!(!plan.actions_listener_reload);
        assert!(!plan.mcp_callback_policy_hot_update);
        assert!(!plan.actions_callback_policy_hot_update);
        assert!(!plan.mcp_tunnel_changed);
        assert!(!plan.actions_tunnel_changed);
    }

    #[test]
    fn workspace_path_change_reloads_both_listeners() {
        let current = profile();
        let mut next = current.clone();
        next.path = "C:/workspace/other".into();

        let plan = plan_workspace_config_apply(&current, &next);
        assert!(plan.mcp_listener_reload);
        assert!(plan.actions_listener_reload);
    }

    #[test]
    fn mcp_policy_change_only_reloads_mcp() {
        let current = profile();
        let mut next = current.clone();
        next.runtime.permission_mode = "read_only".into();

        let plan = plan_workspace_config_apply(&current, &next);
        assert!(plan.mcp_listener_reload);
        assert!(!plan.actions_listener_reload);
    }

    #[test]
    fn actions_policy_change_only_reloads_actions() {
        let current = profile();
        let mut next = current.clone();
        next.actions.max_patch_bytes += 1;

        let plan = plan_workspace_config_apply(&current, &next);
        assert!(!plan.mcp_listener_reload);
        assert!(plan.actions_listener_reload);
    }

    #[test]
    fn callback_policy_changes_are_hot_updates_without_listener_restart() {
        let current = profile();
        let mut next = current.clone();
        next.auth.oauth_redirect_hosts = "chatgpt.com".into();
        next.actions.oauth_redirect_uris = "https://chatgpt.com/callback".into();

        let plan = plan_workspace_config_apply(&current, &next);
        assert!(!plan.mcp_listener_reload);
        assert!(!plan.actions_listener_reload);
        assert!(plan.mcp_callback_policy_hot_update);
        assert!(plan.actions_callback_policy_hot_update);
    }

    #[test]
    fn tunnel_changes_do_not_restart_listeners() {
        let current = profile();
        let mut next = current.clone();
        next.tunnel.frp_server = "frp.example.com".into();
        next.actions.cloudflare_mode = "quick".into();

        let plan = plan_workspace_config_apply(&current, &next);
        assert!(!plan.mcp_listener_reload);
        assert!(!plan.actions_listener_reload);
        assert!(plan.mcp_tunnel_changed);
        assert!(plan.actions_tunnel_changed);
    }

    #[test]
    fn auth_identity_changes_reload_only_the_affected_listener() {
        let current = profile();
        let mut mcp = current.clone();
        mcp.auth.oauth_client_id = "mcp-client".into();
        let mcp_plan = plan_workspace_config_apply(&current, &mcp);
        assert!(mcp_plan.mcp_listener_reload);
        assert!(!mcp_plan.actions_listener_reload);

        let mut actions = current.clone();
        actions.actions.oauth_scopes = "read write".into();
        let actions_plan = plan_workspace_config_apply(&current, &actions);
        assert!(!actions_plan.mcp_listener_reload);
        assert!(actions_plan.actions_listener_reload);
    }
}
