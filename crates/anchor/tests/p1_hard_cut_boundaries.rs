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
fn persisted_configuration_has_explicit_content_schema_versions() {
    let model = source("src/data/model.rs");
    let storage = source("src/data/storage.rs");
    let migration = source("src/data/migration.rs");

    assert!(model.contains("pub(crate) const PROFILES_SCHEMA_VERSION: u32 = 1;"));
    assert!(model.contains("pub(crate) const SECRETS_SCHEMA_VERSION: u32 = 1;"));
    assert!(model.contains("pub schema_version: u32"));
    assert!(storage.contains("migrate_unversioned_profiles"));
    assert!(storage.contains("migrate_unversioned_secrets"));
    assert!(!storage.contains("migrate_legacy_profiles"));
    assert!(migration.contains("const PORTABLE_CONFIG_VERSION: u32 = 2;"));
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
