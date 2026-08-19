use std::fs;
use std::path::{Path, PathBuf};

use regex::Regex;

fn collect_files(root: &Path, output: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(root).expect("read source directory") {
        let path = entry.expect("source entry").path();
        if path.is_dir() {
            collect_files(&path, output);
        } else {
            output.push(path);
        }
    }
}

#[test]
fn web_admin_supported_manifest_matches_dispatcher() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let admin = fs::read_to_string(manifest.join("src/admin.rs")).expect("admin source");
    let supported = admin
        .split("const WEB_ADMIN_SUPPORTED_COMMANDS")
        .nth(1)
        .expect("supported command manifest")
        .split("];\n")
        .next()
        .expect("supported command manifest body");
    let command = Regex::new(r#"\"([a-z0-9_]+)\""#).expect("command regex");

    for capture in command.captures_iter(supported) {
        let name = capture.get(1).expect("command name").as_str();
        assert!(
            admin.contains(&format!("\"{name}\" =>")),
            "supported Web Admin command has no dispatcher arm: {name}"
        );
    }

    assert!(admin.contains("\"supportedCommands\": WEB_ADMIN_SUPPORTED_COMMANDS"));
    assert!(admin.contains("\"privilegedCommands\": privileged_actions()"));
    assert!(admin.contains("\"privilegedExecutors\": available_privileged_executors()"));
    assert!(admin.contains("\"unavailableCommands\": unavailable_privileged_actions()"));
    assert!(admin.contains("\"mutationCommands\": WEB_ADMIN_MUTATION_COMMANDS"));
}

#[test]
fn frontend_admin_commands_are_supported_or_explicitly_privileged() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo = manifest.parent().expect("repo root");
    let admin = fs::read_to_string(manifest.join("src/admin.rs")).expect("admin source");
    let security =
        fs::read_to_string(manifest.join("src/admin_security.rs")).expect("admin security source");
    let supported = admin
        .split("const WEB_ADMIN_SUPPORTED_COMMANDS")
        .nth(1)
        .expect("supported command manifest")
        .split("];\n")
        .next()
        .expect("supported command manifest body");
    let privileged = security
        .split("const PRIVILEGED_ACTIONS")
        .nth(1)
        .expect("privileged action manifest")
        .split("];\n")
        .next()
        .expect("privileged action manifest body");
    let invocation = Regex::new(
        r#"(?s)invoke(?:Admin|Read|PrivilegedAdmin)(?:<[^>]+>)?\s*\(\s*\"([a-z0-9_]+)\""#,
    )
    .expect("frontend invocation regex");

    let api_root = repo.join("src/lib/api");
    let mut files = Vec::new();
    collect_files(&api_root, &mut files);
    let mut uncovered = Vec::new();
    for path in files {
        if path.extension().and_then(|value| value.to_str()) != Some("ts") {
            continue;
        }
        let source = fs::read_to_string(&path).expect("frontend api source");
        for capture in invocation.captures_iter(&source) {
            let name = capture.get(1).expect("command name").as_str();
            // update_workspace is an explicit Tauri compatibility alias. The
            // browser path for the same public API uses stage/apply instead.
            if name == "update_workspace" {
                continue;
            }
            let quoted = format!("\"{name}\"");
            if !supported.contains(&quoted) && !privileged.contains(&quoted) {
                uncovered.push(format!("{}: {name}", path.display()));
            }
        }
    }

    assert!(
        uncovered.is_empty(),
        "frontend Admin commands are neither supported nor explicitly privileged: {uncovered:?}"
    );
    assert!(supported.contains("\"save_frp_profile_metadata\""));
    assert!(supported.contains("\"set_frp_profile_token\""));
    assert!(!supported.contains("\"save_frp_profile\""));
    assert!(privileged.contains("\"save_frp_profile\""));
}

#[test]
fn privileged_confirmation_exposes_only_reviewed_executors() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let admin = fs::read_to_string(manifest.join("src/admin.rs")).expect("admin source");
    let security =
        fs::read_to_string(manifest.join("src/admin_security.rs")).expect("admin security source");

    assert!(admin.contains("\"prepare_privileged_action\" =>"));
    assert!(admin.contains("\"confirm_privileged_action\" =>"));
    assert!(admin.contains("\"list_admin_audit_events\" =>"));
    assert!(security.contains("pub fn consume_grant"));
    assert!(admin.contains("consume_privileged_grant("));
    assert!(admin.contains("PrivilegedActionBinding::workspace_secret"));
    assert!(admin.contains("PrivilegedActionBinding::shared_secret"));
    assert!(admin.contains("PrivilegedActionBinding::frp_profile"));
    assert!(admin.contains("PrivilegedActionBinding::software_install"));
    assert!(admin.contains("PrivilegedActionBinding::software_uninstall"));
    assert!(admin.contains("PrivilegedActionBinding::windows_service"));
    assert!(admin.contains("canonical_privileged_binding"));
    assert!(admin.contains("windows_service_privileged_binding"));
    assert!(security.contains("binding_fingerprint"));
    assert!(security.contains("record_execution_outcome"));

    let event_shape = security
        .split("pub struct AdminAuditEvent")
        .nth(1)
        .expect("audit event struct")
        .split("}\n\npub fn session_fingerprint")
        .next()
        .expect("audit event fields");
    for forbidden_field in [
        "pub args:",
        "pub payload:",
        "pub secret:",
        "pub token:",
        "pub path:",
        "pub target:",
        "pub owner:",
        "pub revision:",
    ] {
        assert!(
            !event_shape.contains(forbidden_field),
            "audit event exposes sensitive field: {forbidden_field}"
        );
    }
}

#[test]
fn running_gateway_route_changes_use_the_gateway_control_domain() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let management =
        fs::read_to_string(manifest.join("src/management.rs")).expect("management source");
    let protocol = fs::read_to_string(manifest.join("src/gateway_control/protocol.rs"))
        .expect("Gateway protocol source");
    let cli = fs::read_to_string(manifest.join("src/cli/mod.rs")).expect("CLI source");

    assert!(management.contains("gateway_control::request_set_routes"));
    assert!(protocol.contains("SetRoutes"));
    assert!(cli.contains("GatewayControlCommand::SetRoutes"));
}

#[test]
fn frontend_tauri_dependencies_are_confined_to_platform_adapters() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let frontend = manifest.parent().expect("repo root").join("src");
    let mut files = Vec::new();
    collect_files(&frontend, &mut files);

    let mut violations = Vec::new();
    for path in files {
        if !matches!(
            path.extension().and_then(|value| value.to_str()),
            Some("ts" | "svelte")
        ) {
            continue;
        }
        let relative = path
            .strip_prefix(&frontend)
            .expect("frontend relative path");
        let allowed = relative == Path::new("lib/api/invoke.ts")
            || relative.starts_with(Path::new("lib/platform"));
        let source = fs::read_to_string(&path).expect("read frontend source");
        if !allowed && source.contains("@tauri-apps") {
            violations.push(relative.display().to_string());
        }
    }
    assert!(
        violations.is_empty(),
        "Tauri imports escaped the platform adapters: {violations:?}"
    );
}

#[test]
fn desktop_shell_does_not_own_runtime_resources() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let app_state = fs::read_to_string(manifest.join("src/app_state.rs")).expect("app state");
    let desktop = fs::read_to_string(manifest.join("src/lib.rs")).expect("desktop lib");

    assert!(!app_state.contains("RuntimeSupervisor"));
    assert!(!desktop.contains("spawn_desktop_maintenance"));
    assert!(!desktop.contains("gateway::stop().await"));
    assert!(!desktop.contains("shutdown_all()"));
}

#[test]
fn web_admin_writes_delegate_to_shared_management_services() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let admin = fs::read_to_string(manifest.join("src/admin.rs")).expect("admin source");

    for delegated in [
        "management::stage_workspace_config",
        "management::apply_workspace_config",
        "management::start_workspace_service",
        "management::stop_workspace_service",
        "management::restart_workspace_service",
        "management::start_workspace_tunnel",
        "management::restart_workspace_tunnel",
        "management::stop_workspace_tunnel",
        "management::test_workspace_tunnel",
        "management::set_mcp_gateway",
        "management::reload_mcp_gateway",
        "management::set_gateway_workspace_route",
        "management::set_workspace_secret",
        "management::regenerate_workspace_secret",
        "management::set_shared_secret",
        "management::regenerate_shared_secret",
        "management::set_frp_profile_token",
        "management::install_software",
        "management::uninstall_software",
        "management::install_windows_service",
        "management::uninstall_windows_service",
        "management::start_windows_service",
        "management::stop_windows_service",
        "management::restart_windows_service",
        "management::sync_windows_service_plan",
    ] {
        assert!(
            admin.contains(delegated),
            "missing shared delegation: {delegated}"
        );
    }

    for forbidden_direct_write in [
        "control::set_daemon_service(",
        "control::restart_daemon_service(",
        "control::request_tunnel_operation(",
        "gateway_control::request_apply_config(",
        "gateway_control::request_set_routes(",
        "gateway_control::request_exit(",
    ] {
        assert!(
            !admin.contains(forbidden_direct_write),
            "admin HTTP handler owns business control flow: {forbidden_direct_write}"
        );
    }
}

#[test]
fn web_admin_keeps_only_unreviewed_privileged_mutations_outside_dispatcher() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let admin = fs::read_to_string(manifest.join("src/admin.rs")).expect("admin source");

    assert!(
        !admin.contains("\"save_frp_profile\" =>"),
        "unreviewed save_frp_profile Web Admin mutation was exposed unexpectedly"
    );

    for command in [
        "set_workspace_secret",
        "regenerate_workspace_secret",
        "set_shared_secret",
        "regenerate_shared_secret",
        "set_frp_profile_token",
        "install_software",
        "uninstall_software",
        "install_windows_service",
        "uninstall_windows_service",
        "start_windows_service",
        "stop_windows_service",
        "restart_windows_service",
        "sync_windows_service_plan",
    ] {
        assert!(
            admin.contains(&format!("\"{command}\" =>")),
            "reviewed privileged executor is missing: {command}"
        );
    }
}

#[test]
fn windows_service_executor_is_platform_gated_and_server_bound() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let admin = fs::read_to_string(manifest.join("src/admin.rs")).expect("admin source");
    let security =
        fs::read_to_string(manifest.join("src/admin_security.rs")).expect("admin security source");
    let management =
        fs::read_to_string(manifest.join("src/management.rs")).expect("management source");
    let commands = fs::read_to_string(manifest.join("src/commands/windows_service.rs"))
        .expect("Windows Service command source");
    let windows_service = fs::read_to_string(manifest.join("src/windows_service.rs"))
        .expect("Windows Service source");

    for command in [
        "install_windows_service",
        "uninstall_windows_service",
        "start_windows_service",
        "stop_windows_service",
        "restart_windows_service",
        "sync_windows_service_plan",
    ] {
        assert!(
            admin.contains(&format!("\"{command}\" =>")),
            "Windows Service executor missing dispatcher arm: {command}"
        );
        assert!(security.contains(&format!("\"{command}\"")));
    }

    assert!(admin.contains("#[cfg(windows)]\n    \"install_windows_service\""));
    assert!(security.contains("#[cfg(windows)]\n    \"install_windows_service\""));
    assert!(security.contains("#[cfg(not(windows))]\n    \"install_windows_service\""));
    assert!(admin.contains("canonical_privileged_binding(&input.action"));
    assert!(admin.contains("management::windows_service_privileged_target"));
    assert!(management.contains("crate::windows_service::privileged_action_target"));
    assert!(windows_service.contains("pub fn privileged_action_target"));
    assert!(windows_service.contains("fn running_plan_snapshot"));
    assert!(windows_service.contains("current_user_sid()?"));
    assert!(windows_service.contains("\"registeredExecutable\""));
    assert!(windows_service.contains("\"runningSnapshot\""));
    assert!(windows_service.contains("\"currentExecutable\""));

    assert!(commands.contains("management::install_windows_service().await"));
    assert!(commands.contains("management::sync_windows_service_plan()"));
    assert!(!commands.contains("run_elevated_admin_action"));
}
