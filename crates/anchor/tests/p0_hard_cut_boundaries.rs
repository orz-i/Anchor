use std::fs;
use std::path::Path;

fn source(path: &str) -> String {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    fs::read_to_string(root.join(path)).unwrap_or_else(|error| panic!("read {path}: {error}"))
}

#[test]
fn retired_text_search_tool_names_cannot_reenter_dispatch_or_registry() {
    let dispatch = source("src/tools/dispatch.rs");
    let registry = source("src/tools/registry.rs");

    assert!(!dispatch.contains("\"grep\" | \"search_text\""));
    assert!(!dispatch.contains("\"search_text\" =>"));
    assert!(!registry.contains("\"grep\" | \"search_text\" =>"));
    assert!(!registry.contains("\n        \"grep\",\n        \"Grep repository\""));
    assert!(registry.contains("\n        \"search\",\n        \"Search repository\""));
}

#[test]
fn harness_persistence_rejects_retired_history_field_names() {
    let model = source("src/harness/model.rs");

    assert!(!model.contains("serde(alias = \"history_session_key\")"));
    assert!(!model.contains("serde(alias = \"history_session_path\")"));
    assert!(!model.contains("serde(alias = \"history_checkpoint\")"));

    for type_name in [
        "WorkSessionCloseOutbox",
        "StageCommitReceipt",
        "TaskSession",
        "OperationRecord",
    ] {
        let marker = format!("#[serde(deny_unknown_fields)]\npub struct {type_name}");
        assert!(
            model.contains(&marker),
            "{type_name} must fail closed on retired persisted fields"
        );
    }
}

#[test]
fn gateway_control_has_no_impossible_legacy_lifecycle_retry() {
    let protocol = source("src/gateway_control/protocol.rs");
    let ipc = source("src/gateway_control/ipc.rs");

    assert!(!protocol.contains("GATEWAY_LIFECYCLE_PROTOCOL_MIN_VERSION"));
    assert!(!ipc.contains("legacy_lifecycle_retry_protocol"));
    assert!(ipc.contains("let result = request(method).await?;"));
}

#[test]
fn cli_workspace_mutations_delegate_to_management_authority() {
    let cli = source("src/cli/workspace.rs");

    assert!(cli.contains("crate::management::register_workspace(path, name)"));
    assert!(cli.contains("crate::management::delete_workspace_with_timeout("));
    for retired_local_authority in [
        "fn assign_os_available_ports(",
        "fn canonical_workspace_path(",
        "fn same_workspace_path(",
        "drop_tunnel_workspace(",
        "request_daemon_exit_and_wait(",
    ] {
        assert!(
            !cli.contains(retired_local_authority),
            "CLI workspace mutation authority leaked back in: {retired_local_authority}"
        );
    }
}

#[test]
fn cli_gateway_configure_is_only_an_adapter() {
    let cli = source("src/cli/mod.rs");
    let start = cli
        .find("async fn configure_gateway(")
        .expect("configure_gateway");
    let end = cli[start..]
        .find("async fn next_gateway_control_command(")
        .map(|offset| start + offset)
        .expect("next gateway function");
    let configure = &cli[start..end];

    assert!(configure.contains("crate::management::set_mcp_gateway(config).await?;"));
    assert!(!configure.contains("gateway_control::request_apply_config"));
    assert!(!configure.contains("gateway_control::persist_config"));
    assert!(!configure.contains("gateway_daemon::inspect"));
}

#[test]
fn cli_frp_mutations_delegate_to_management_authority() {
    let cli = source("src/cli/frp.rs");

    assert!(cli.contains("crate::management::save_frp_profile_metadata"));
    assert!(cli.contains("crate::management::set_frp_profile_token"));
    assert!(cli.contains("crate::management::clear_frp_profile_token"));
    assert!(cli.contains("crate::management::delete_frp_profile"));
    assert!(!cli.contains("DataStore::update_file"));
    assert!(!cli.contains("fn ensure_profile_not_live("));
    assert!(!cli.contains("fn all_profile_references("));
}

#[test]
fn service_manager_commands_are_pre_dispatched_once() {
    let cli = source("src/cli/mod.rs");

    assert!(!cli.contains("service-run 必须由 OS service manager 入口直接分派"));
    assert!(!cli.contains("service-admin-run 必须由 Windows UAC helper 入口直接分派"));
    assert!(cli.contains(
        "unreachable!(\"service manager commands are dispatched before async CLI execution\")"
    ));
}
