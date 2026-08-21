mod net;
mod paths;
mod process;
mod systemd;

use std::path::PathBuf;

use crate::error::AppResult;
use crate::platform::Platform;

pub(crate) use systemd::run_user_systemctl;

pub struct LinuxPlatform;

impl Platform for LinuxPlatform {
    fn app_config_dir(&self) -> AppResult<PathBuf> {
        if let Some(path) = crate::platform::app_config_dir_override() {
            return Ok(path);
        }
        let base = dirs::config_dir()
            .or_else(dirs::home_dir)
            .ok_or_else(|| crate::error::AppError::Message("config dir not found".into()))?;
        Ok(base.join(crate::brand::APP_CONFIG_DIR_NAME))
    }

    fn find_pid_listening_on_port(&self, port: u16) -> AppResult<Option<u32>> {
        net::find_pid_listening_on_port(port)
    }

    fn process_image_path(&self, pid: u32) -> AppResult<Option<String>> {
        process::process_image_path(pid)
    }

    fn is_process_alive(&self, pid: u32) -> bool {
        process::is_process_alive(pid)
    }

    fn terminate_process_tree(&self, pid: u32) -> AppResult<()> {
        process::terminate_process_tree(pid)
    }

    fn cloudflared_candidates(&self) -> Vec<PathBuf> {
        paths::cloudflared_candidates()
    }

    fn frpc_candidates(&self) -> Vec<PathBuf> {
        paths::frpc_candidates()
    }
}
