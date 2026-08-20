mod net;
mod paths;
mod process;

use std::path::{Path, PathBuf};

use crate::error::AppResult;
use crate::platform::Platform;

pub struct WindowsPlatform;

impl Platform for WindowsPlatform {
    fn app_config_dir(&self) -> AppResult<PathBuf> {
        if let Some(path) = crate::platform::app_config_dir_override() {
            return Ok(path);
        }
        paths::roaming_app_data().map(|dir| dir.join(crate::brand::APP_CONFIG_DIR_NAME))
    }

    fn find_pid_listening_on_port(&self, port: u16) -> AppResult<Option<u32>> {
        net::find_pid_listening_on_port(port)
    }

    fn reclaim_listening_port(&self, port: u16) -> AppResult<bool> {
        net::reclaim_listening_port(port)
    }

    fn process_image_path(&self, pid: u32) -> AppResult<Option<String>> {
        process::process_image_path(pid)
    }

    fn process_ids_by_image_path(&self, image_path: &Path) -> AppResult<Vec<u32>> {
        process::process_ids_by_image_path(image_path)
    }

    fn is_process_alive(&self, pid: u32) -> bool {
        process::is_process_alive(pid)
    }

    fn terminate_process_tree(&self, pid: u32) -> AppResult<()> {
        process::terminate_process_tree(pid)
    }

    fn terminate_processes_by_image_path(&self, image_path: &Path) -> AppResult<usize> {
        process::terminate_processes_by_image_path(image_path)
    }

    fn cloudflared_candidates(&self) -> Vec<PathBuf> {
        paths::cloudflared_candidates()
    }

    fn frpc_candidates(&self) -> Vec<PathBuf> {
        paths::frpc_candidates()
    }
}
