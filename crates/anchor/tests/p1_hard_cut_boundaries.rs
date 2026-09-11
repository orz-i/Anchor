mod common;

use std::fs;
use std::path::Path;

use common::{assert_ok, ctx_for, invoke, tiny_js_fixture};
use serde_json::json;

fn source(path: &str) -> String {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    fs::read_to_string(root.join(path)).unwrap_or_else(|error| panic!("read {path}: {error}"))
}

#[test]
fn task_observation_layer_has_one_canonical_module() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let lib = source("src/lib.rs");
    let tasks = source("src/tasks.rs");
    let management = source("src/management.rs");
    let orchestration = source("src/orchestration/mod.rs");

    assert!(root.join("src/tasks.rs").is_file());
    assert!(!root.join("src/canvs.rs").exists());
    assert!(lib.contains("mod tasks;"));
    assert!(!lib.contains("mod canvs;"));

    for active_source in [&tasks, &management, &orchestration] {
        assert!(!active_source.contains("crate::canvs"));
        assert!(!active_source.contains("Canvs"));
    }
    assert!(management.contains("crate::tasks::TaskView"));
    assert!(management.contains("crate::tasks::TaskSnapshot"));
    assert!(orchestration.contains("crate::tasks::list_workspace_tasks"));
}

#[test]
fn task_observation_uses_bounded_recent_harness_reads() {
    let tasks = source("src/tasks.rs");
    let split = source("src/harness/split.rs");
    let store = source("src/harness/store.rs");

    assert!(tasks.contains(".recent_events(&task_id, MAX_RECENT_EVENTS)?"));
    assert!(tasks.contains(".recent_operations_for_task(&task_id, MAX_RECENT_OPERATIONS)?"));
    assert!(!tasks.contains("list_events(&task_id, 0, usize::MAX)"));
    assert!(!tasks.contains("list_operations(0, usize::MAX)"));
    assert!(split.contains("pub fn recent_events("));
    assert!(split.contains("pub fn recent_operations_for_task("));
    assert!(store.contains("fn read_recent_journal<T, F>("));
    assert!(store.contains("journal_segments(dir)?.into_iter().rev()"));
}

#[test]
fn retired_command_arguments_are_rejected_at_the_public_schema() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);

    let sessions = invoke(
        &ctx,
        "list_command_sessions",
        json!({"include_terminal": true}),
    );
    assert_eq!(sessions["ok"], false, "{sessions}");
    assert_eq!(sessions["error"]["code"], "INVALID_TOOL_ARGUMENTS");

    let command = invoke(
        &ctx,
        "exec_command",
        json!({
            "executable": "definitely-not-run",
            "allowed_exit_codes": [0]
        }),
    );
    assert_eq!(command["ok"], false, "{command}");
    assert_eq!(command["error"]["code"], "INVALID_TOOL_ARGUMENTS");

    let canonical = invoke(&ctx, "list_command_sessions", json!({}));
    assert_eq!(assert_ok(&canonical)["scope"], "pending");
}

#[test]
fn data_store_no_longer_runs_retirement_cleanup_on_every_access() {
    let store = source("src/data/store.rs");

    assert!(!store.contains("RETIRED_ACTIONS_SECRET_KEYS"));
    assert!(!store.contains("RETIRED_WORKSPACE_NOTIFICATION_SECRET_KEYS"));
    assert!(!store.contains("strip_retired_actions_secrets"));
    assert!(!store.contains("strip_retired_workspace_notification_secrets"));
    assert!(!store.contains("strip_retired_secrets"));
}

#[test]
fn persisted_configuration_requires_current_content_schema_versions() {
    let model = source("src/data/model.rs");
    let storage = source("src/data/storage.rs");
    let migration = source("src/data/migration.rs");

    assert!(model.contains("pub(crate) const PROFILES_SCHEMA_VERSION: u32 = 1;"));
    assert!(model.contains("pub(crate) const SECRETS_SCHEMA_VERSION: u32 = 1;"));
    assert!(model.contains("pub schema_version: u32"));
    assert!(!storage.contains("migrate_unversioned_profiles"));
    assert!(!storage.contains("migrate_unversioned_secrets"));
    assert!(!storage.contains("migrate_legacy_profiles"));
    assert!(!storage.contains("LEGACY_V0_ACTIONS_SECRET_KEYS"));
    assert!(!storage.contains("LEGACY_V0_WORKSPACE_NOTIFICATION_SECRET_KEYS"));
    assert!(storage.contains("无版本配置已停止支持"));
    assert!(storage.contains("无版本凭据已停止支持"));
    assert!(migration.contains("const PORTABLE_CONFIG_VERSION: u32 = 2;"));
}

#[test]
fn control_planes_have_no_cross_version_retry_bridge() {
    let workspace_protocol = source("src/control/protocol.rs");
    let workspace_ipc = source("src/control/ipc.rs");
    let gateway_protocol = source("src/gateway_control/protocol.rs");
    let gateway_ipc = source("src/gateway_control/ipc.rs");

    assert!(!workspace_protocol.contains("CONTROL_LIFECYCLE_PROTOCOL_MIN_VERSION"));
    assert!(!workspace_protocol.contains("with_protocol_version"));
    assert!(!workspace_ipc.contains("legacy_lifecycle_retry_protocol"));
    assert!(!workspace_ipc.contains("request_with_protocol_version"));
    assert!(!gateway_protocol.contains("with_protocol_version"));
    assert!(!gateway_ipc.contains("request_with_protocol_version"));
    assert!(
        workspace_ipc.contains("let result = request(profile_id, ControlMethod::Version).await?;")
    );
    assert!(gateway_ipc.contains("let result = request(GatewayMethod::Version).await?;"));
}

#[test]
fn unix_control_unavailable_stop_is_a_verified_recovery_not_a_protocol_bridge() {
    let lifecycle = source("src/control/lifecycle.rs");
    let daemon = source("src/daemon.rs");

    assert!(lifecycle.contains("if error.is_unavailable()"));
    assert!(lifecycle.contains("recover_unreachable_daemon_stop"));
    assert!(daemon.contains("pub(crate) async fn recover_unreachable_daemon_stop("));
    assert!(daemon.contains("process_matches_daemon_state(state, &profile.id)"));
    assert!(!daemon.contains("stop_verified_without_control"));
}

#[test]
fn windows_service_has_no_pre_owner_token_frpc_cleanup_lane() {
    let service = source("src/windows_service.rs");
    let platform = source("src/platform/mod.rs");

    assert!(!service.contains("cleanup_wrong_owner_managed_frpc_processes"));
    assert!(!service.contains("managed_frpc_image_candidates"));
    assert!(!service.contains("legacy managed frpc"));
    assert!(!platform.contains("fn process_ids_by_image_path(&self"));
}

#[test]
fn gateway_config_write_does_not_probe_process_local_legacy_runtime() {
    let management = source("src/management.rs");
    let start = management
        .find("pub(crate) async fn set_mcp_gateway(")
        .expect("set_mcp_gateway");
    let end = management[start..]
        .find("pub(crate) async fn reload_mcp_gateway(")
        .map(|offset| start + offset)
        .expect("reload_mcp_gateway");
    let setter = &management[start..end];

    assert!(!setter.contains("crate::mcp::gateway::status"));
    assert!(!setter.contains("旧版 process-local Gateway"));
    assert!(setter.contains("crate::gateway_daemon::inspect()?"));
}
