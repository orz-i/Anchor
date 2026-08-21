use std::fs;
use std::path::PathBuf;

fn source(name: &str) -> String {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    fs::read_to_string(manifest.join("src").join(name)).expect("source file")
}

#[test]
fn persistent_admin_daemon_is_isolated_from_business_runtimes() {
    let daemon = source("admin_daemon.rs");

    for required in [
        "admin.lock",
        "admin.pid",
        "admin.json",
        "crate::admin::serve",
        "try_lock_exclusive",
        "find_pid_listening_on_port",
        "process_image_path",
        "/api/v1/health",
    ] {
        assert!(
            daemon.contains(required),
            "missing Admin daemon guard: {required}"
        );
    }

    for forbidden in [
        "control::reconcile_daemon",
        "gateway_control::",
        "gateway_daemon::spawn",
        "RuntimeSupervisor",
        "TunnelSupervisor",
    ] {
        assert!(
            !daemon.contains(forbidden),
            "Admin daemon escaped its management-plane boundary: {forbidden}"
        );
    }
}

#[test]
fn persistent_admin_recovery_is_identity_checked_and_fail_closed() {
    let daemon = source("admin_daemon.rs");
    let service = source("admin_service.rs");

    assert!(daemon.contains("recover_registered_listener"));
    assert!(daemon.contains("normalize_process_path(executable)"));
    assert!(daemon.contains("normalize_process_path(Path::new(&actual))"));
    assert!(daemon.contains("不会接管其他进程端口"));
    assert!(service.contains("OS autostart 注册缺失"));
    assert!(service.contains("不会静默降级为非托管后台进程"));
    assert!(service.contains("platform_uninstall();"));
}

#[test]
fn admin_service_restart_policy_is_bounded_on_linux_and_windows() {
    let service = source("admin_service.rs");

    for linux_guard in [
        "Restart=on-failure",
        "RestartSec=5",
        "StartLimitIntervalSec=60",
        "StartLimitBurst=3",
        "enable-linger",
        "systemctl\")\n        .arg(\"--user\")",
    ] {
        assert!(
            service.contains(linux_guard),
            "missing Linux Admin service recovery guard: {linux_guard}"
        );
    }
    for windows_guard in [
        "<RestartOnFailure><Interval>PT1M</Interval><Count>3</Count>",
        "<MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>",
        "<LogonType>InteractiveToken</LogonType>",
        "<RunLevel>LeastPrivilege</RunLevel>",
    ] {
        assert!(
            service.contains(windows_guard),
            "missing Windows Admin service recovery guard: {windows_guard}"
        );
    }
}

#[test]
fn admin_service_upgrade_tracks_build_and_current_executable() {
    let service = source("admin_service.rs");

    assert!(service.contains("std::env::current_exe"));
    assert!(service.contains("installed_build: BuildIdentity::current()"));
    assert!(service.contains("same_build(&current_build)"));
    assert!(service.contains("pub async fn upgrade()"));
    assert!(service.contains("install(config.port).await"));
    assert!(service.contains("build_state"));
}

#[test]
fn persistent_admin_lifecycle_is_cli_owned_and_daemon_run_is_internal() {
    let args = source("cli/args.rs");
    let cli = source("cli/mod.rs");
    let service = source("admin_service.rs");

    for public_command in [
        "admin start",
        "admin stop",
        "admin restart",
        "admin status",
        "admin install",
        "admin uninstall",
        "admin enable|disable",
        "admin upgrade",
    ] {
        assert!(
            args.contains(public_command),
            "missing Admin CLI command: {public_command}"
        );
    }
    assert!(args.contains("Some(\"daemon-run\")"));
    assert!(cli.contains("AdminCommand::DaemonRun"));
    assert!(cli.contains("crate::admin_daemon::run"));
    assert!(cli.contains("crate::admin_service::upgrade"));
    assert!(!service.contains("tauri::"));
    assert!(!service.contains("AppState"));
}
