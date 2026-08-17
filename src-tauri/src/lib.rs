#![cfg_attr(target_os = "windows", allow(linker_messages))]

mod actions;
#[cfg(feature = "desktop")]
mod app_state;
mod async_runtime;
mod auth;
mod brand;
pub mod build_identity;
mod canvs;
mod canvs_web;
#[cfg(feature = "cli")]
pub mod cli;
#[cfg(feature = "desktop")]
mod commands;
pub mod control;
pub mod daemon;
mod data;
mod error;
pub mod gateway_control;
pub mod gateway_daemon;
pub mod harness;
#[cfg(feature = "desktop")]
mod health;
mod logging;
mod mcp;
mod platform;
pub mod rollout;
mod runtime;
mod secret;
mod settings;
mod skills;
pub mod tools;
mod tunnel;
#[cfg(target_os = "windows")]
pub mod windows_service;
mod workspace;

#[cfg(feature = "desktop")]
use app_state::AppState;
#[cfg(feature = "desktop")]
use commands::{
    create_workspace, delete_frp_profile, delete_workspace, get_actions_runtime_status,
    get_app_settings, get_canvs_snapshot, get_canvs_task_snapshot, get_control_plane_events,
    get_control_plane_status, get_download_config, get_frp_snippet, get_gateway_control_events,
    get_last_workspace_id, get_mcp_gateway, get_mcp_gateway_status, get_proxy, get_runtime_status,
    get_shared_secret, get_windows_service_status, get_workspace_control_events,
    get_workspace_control_status, get_workspace_secret, inspect_workspace_skills, install_software,
    install_windows_service, list_canvs_tasks, list_frp_profiles, list_software, list_workspaces,
    open_workspace_directory, read_gateway_logs, read_workspace_logs, regenerate_shared_secret,
    regenerate_workspace_secret, restart_actions_runtime, restart_runtime, restart_tunnel,
    restart_windows_service, run_health_checks, save_frp_profile, set_download_config,
    set_last_workspace, set_mcp_gateway, set_proxy, set_shared_secret, set_workspace_secret,
    start_actions_runtime, start_runtime, start_tunnel, start_windows_service,
    stop_actions_runtime, stop_runtime, stop_tunnel, stop_windows_service,
    sync_windows_service_plan, test_tunnel, uninstall_software, uninstall_windows_service,
    update_workspace,
};
#[cfg(feature = "desktop")]
use tauri::Manager;

#[cfg(all(feature = "desktop", target_os = "windows"))]
fn acquire_single_instance() -> bool {
    use windows::core::w;
    use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS};
    use windows::Win32::System::Threading::CreateMutexW;

    // HANDLE 保持到进程退出，由 Windows 自动回收。第二个实例必须在
    // cleanup_managed_frpc_instances 之前退出，避免误清理第一个实例的进程。
    let Ok(handle) = (unsafe { CreateMutexW(None, false, w!("Local\\Anchor-SingleInstance")) })
    else {
        eprintln!("创建应用单实例锁失败，为避免误清理其他实例的 frpc，本次启动已取消");
        return false;
    };
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        let _ = unsafe { CloseHandle(handle) };
        return false;
    }
    true
}

#[cfg(all(feature = "desktop", not(target_os = "windows")))]
fn acquire_single_instance() -> bool {
    true
}

#[cfg(all(feature = "desktop", feature = "cli"))]
fn is_internal_daemon_command(args: impl IntoIterator<Item = String>) -> bool {
    let mut args = args.into_iter();
    loop {
        match args.next().as_deref() {
            Some("--config-dir") => {
                if args.next().is_none() {
                    return false;
                }
            }
            Some("--json") => {}
            Some(arg) => {
                return matches!(
                    arg,
                    "daemon-run" | "gateway-daemon-run" | "service-run" | "service-admin-run"
                );
            }
            None => return false,
        }
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
#[cfg(feature = "desktop")]
pub fn run() {
    #[cfg(feature = "cli")]
    if is_internal_daemon_command(std::env::args().skip(1)) {
        std::process::exit(cli::run());
    }
    if !acquire_single_instance() {
        return;
    }
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            app.manage(AppState::new().expect("failed to load app state"));
            crate::runtime::spawn_desktop_maintenance(app.handle().clone());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            list_workspaces,
            create_workspace,
            update_workspace,
            inspect_workspace_skills,
            open_workspace_directory,
            delete_workspace,
            start_runtime,
            stop_runtime,
            get_runtime_status,
            get_control_plane_status,
            get_control_plane_events,
            get_gateway_control_events,
            get_workspace_control_events,
            get_workspace_control_status,
            get_windows_service_status,
            install_windows_service,
            uninstall_windows_service,
            start_windows_service,
            stop_windows_service,
            restart_windows_service,
            sync_windows_service_plan,
            get_canvs_snapshot,
            list_canvs_tasks,
            get_canvs_task_snapshot,
            start_actions_runtime,
            stop_actions_runtime,
            get_actions_runtime_status,
            restart_runtime,
            restart_actions_runtime,
            get_frp_snippet,
            start_tunnel,
            stop_tunnel,
            run_health_checks,
            get_workspace_secret,
            set_workspace_secret,
            regenerate_workspace_secret,
            get_shared_secret,
            set_shared_secret,
            regenerate_shared_secret,
            read_workspace_logs,
            read_gateway_logs,
            list_frp_profiles,
            save_frp_profile,
            delete_frp_profile,
            get_app_settings,
            get_mcp_gateway,
            get_mcp_gateway_status,
            set_mcp_gateway,
            restart_tunnel,
            test_tunnel,
            set_last_workspace,
            get_last_workspace_id,
            list_software,
            install_software,
            uninstall_software,
            get_download_config,
            set_download_config,
            get_proxy,
            set_proxy,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application");

    app.run(|_app_handle, event| {
        if matches!(event, tauri::RunEvent::Exit) {
            crate::async_runtime::block_on(async {
                let _ = crate::mcp::gateway::stop().await;
                crate::tunnel::supervisor()
                    .lock()
                    .await
                    .shutdown_all()
                    .await;
            });
        }
    });
}

#[cfg(all(test, feature = "desktop", feature = "cli"))]
mod tests {
    use super::is_internal_daemon_command;

    #[test]
    fn desktop_binary_dispatches_only_internal_daemon_entrypoints_before_tauri() {
        let args = |values: &[&str]| {
            values
                .iter()
                .map(|value| (*value).to_string())
                .collect::<Vec<_>>()
        };

        assert!(is_internal_daemon_command(args(&["daemon-run"])));
        assert!(is_internal_daemon_command(args(&["gateway-daemon-run"])));
        assert!(is_internal_daemon_command(args(&["service-run"])));
        assert!(is_internal_daemon_command(args(&["service-admin-run"])));
        assert!(is_internal_daemon_command(args(&[
            "--config-dir",
            r"C:\Users\Demo User\AppData\Roaming\anchor",
            "daemon-run",
        ])));
        assert!(is_internal_daemon_command(args(&[
            "--json",
            "--config-dir",
            r"C:\Users\Demo User\AppData\Roaming\anchor",
            "gateway-daemon-run",
        ])));
        assert!(!is_internal_daemon_command(args(&["status"])));
        assert!(!is_internal_daemon_command(args(&[
            "--config-dir",
            r"C:\Users\Demo User\AppData\Roaming\anchor",
            "status",
        ])));
        assert!(!is_internal_daemon_command(args(&["--config-dir"])));
        assert!(!is_internal_daemon_command(std::iter::empty()));
    }
}
