use super::WorkspaceProfile;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceConfigApplyPlan {
    pub mcp_listener_reload: bool,
    pub mcp_callback_policy_hot_update: bool,
}

impl WorkspaceConfigApplyPlan {
    pub fn has_changes(self) -> bool {
        self.mcp_listener_reload || self.mcp_callback_policy_hot_update
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
    WorkspaceConfigApplyPlan {
        mcp_listener_reload: path_changed
            || mcp_runtime_changed(current, next)
            || mcp_auth_listener_changed(current, next),
        mcp_callback_policy_hot_update,
    }
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
        assert!(!plan.mcp_callback_policy_hot_update);
    }

    #[test]
    fn workspace_path_change_reloads_listener() {
        let current = profile();
        let mut next = current.clone();
        next.path = "C:/workspace/other".into();

        let plan = plan_workspace_config_apply(&current, &next);
        assert!(plan.mcp_listener_reload);
    }

    #[test]
    fn mcp_policy_change_only_reloads_mcp() {
        let current = profile();
        let mut next = current.clone();
        next.runtime.permission_mode = "read_only".into();

        let plan = plan_workspace_config_apply(&current, &next);
        assert!(plan.mcp_listener_reload);
    }

    #[test]
    fn callback_policy_changes_are_hot_updates_without_listener_restart() {
        let current = profile();
        let mut next = current.clone();
        next.auth.oauth_redirect_hosts = "chatgpt.com".into();

        let plan = plan_workspace_config_apply(&current, &next);
        assert!(!plan.mcp_listener_reload);
        assert!(plan.mcp_callback_policy_hot_update);
    }

    #[test]
    fn auth_identity_changes_reload_listener() {
        let current = profile();
        let mut mcp = current.clone();
        mcp.auth.oauth_client_id = "mcp-client".into();
        let mcp_plan = plan_workspace_config_apply(&current, &mcp);
        assert!(mcp_plan.mcp_listener_reload);
    }
}
