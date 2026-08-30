use std::path::{Path, PathBuf};

use crate::error::AppResult;

/// Cross-platform OS primitives used by the desktop runtime.
///
/// Windows uses `windows-rs`. macOS and Linux live in dedicated modules.
pub trait Platform: Send + Sync {
    fn app_config_dir(&self) -> AppResult<PathBuf>;

    fn find_pid_listening_on_port(&self, port: u16) -> AppResult<Option<u32>>;

    fn process_image_path(&self, pid: u32) -> AppResult<Option<String>>;

    /// Return processes whose executable image exactly matches `image_path`.
    #[cfg(windows)]
    fn process_ids_by_image_path(&self, image_path: &Path) -> AppResult<Vec<u32>>;

    fn is_process_alive(&self, pid: u32) -> bool;

    fn terminate_process_tree(&self, pid: u32) -> AppResult<()>;

    /// 清理由应用管理的同一路径进程；默认平台不做处理。
    fn terminate_processes_by_image_path(&self, _image_path: &Path) -> AppResult<usize> {
        Ok(0)
    }

    fn cloudflared_candidates(&self) -> Vec<PathBuf>;

    fn frpc_candidates(&self) -> Vec<PathBuf>;
}

#[cfg(any(windows, test))]
fn filter_live_pid<F>(pid: Option<u32>, is_alive: F) -> Option<u32>
where
    F: Fn(u32) -> bool,
{
    pid.filter(|pid| is_alive(*pid))
}

pub(crate) fn app_config_dir_override() -> Option<PathBuf> {
    std::env::var_os(crate::brand::CONFIG_DIR_ENV)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

mod child_process;
mod open;
mod paths;

pub(crate) use child_process::{
    configure_durable_supervisor_tokio_process, configure_exec_tokio_process,
    configure_supervised_tokio_process, hide_std_console, hide_tokio_console,
    lower_exec_child_priority,
};
pub use open::open_path_in_file_manager;

#[cfg(target_os = "linux")]
pub(crate) use linux::run_user_systemctl;
#[cfg(target_os = "linux")]
pub use linux::LinuxPlatform;
#[cfg(target_os = "macos")]
pub use macos::MacPlatform;
#[cfg(target_os = "windows")]
pub use windows::WindowsPlatform;

#[cfg(target_os = "windows")]
pub(crate) fn install_windows_kill_on_close_job() -> AppResult<()> {
    windows::install_kill_on_close_job()
}

static PLATFORM: std::sync::OnceLock<Box<dyn Platform>> = std::sync::OnceLock::new();

pub fn platform() -> &'static dyn Platform {
    PLATFORM.get_or_init(|| create_platform()).as_ref()
}

fn create_platform() -> Box<dyn Platform> {
    #[cfg(target_os = "windows")]
    {
        Box::new(WindowsPlatform)
    }
    #[cfg(target_os = "macos")]
    {
        Box::new(MacPlatform)
    }
    #[cfg(target_os = "linux")]
    {
        Box::new(LinuxPlatform)
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    {
        struct Unsupported;
        impl Platform for Unsupported {
            fn app_config_dir(&self) -> AppResult<PathBuf> {
                Err(crate::error::AppError::Message(
                    "unsupported operating system".into(),
                ))
            }
            fn find_pid_listening_on_port(&self, _port: u16) -> AppResult<Option<u32>> {
                Ok(None)
            }
            fn process_image_path(&self, _pid: u32) -> AppResult<Option<String>> {
                Ok(None)
            }
            fn is_process_alive(&self, _pid: u32) -> bool {
                false
            }
            fn terminate_process_tree(&self, _pid: u32) -> AppResult<()> {
                Ok(())
            }
            fn cloudflared_candidates(&self) -> Vec<PathBuf> {
                paths::resolve_from_path("cloudflared")
                    .into_iter()
                    .collect()
            }
            fn frpc_candidates(&self) -> Vec<PathBuf> {
                paths::resolve_from_path("frpc").into_iter().collect()
            }
        }
        Box::new(Unsupported)
    }
}

#[cfg(test)]
mod tests {
    use super::filter_live_pid;

    #[test]
    fn stale_listener_owner_is_discarded() {
        assert_eq!(filter_live_pid(Some(1956), |_| false), None);
        assert_eq!(filter_live_pid(Some(1956), |_| true), Some(1956));
        assert_eq!(
            filter_live_pid(None, |_| panic!("no PID must not be probed")),
            None
        );
    }
}
