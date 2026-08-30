use std::time::{Duration, Instant};
use std::{net::Ipv4Addr, net::TcpListener};

use crate::async_runtime::JoinHandle;

use crate::platform::platform;

pub fn is_own_process(pid: u32) -> bool {
    pid == std::process::id()
}

/// Return whether a fresh listener can actually bind the workspace loopback
/// endpoint right now.
///
/// On Windows, absence from the LISTEN table is not enough: accepted TCP
/// connections can keep the local transport address unavailable after the
/// listening socket has been closed. A short-lived bind probe reflects the
/// same condition the successor daemon will face without relying on privileged
/// `SetTcpEntry` state mutation.
pub fn loopback_port_bindable(port: u16) -> bool {
    match TcpListener::bind((Ipv4Addr::LOCALHOST, port)) {
        Ok(listener) => {
            drop(listener);
            true
        }
        Err(_) => false,
    }
}

pub async fn wait_for_port_free(port: u16, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if loopback_port_bindable(port) {
            return true;
        }
        match platform().find_pid_listening_on_port(port) {
            Ok(Some(pid)) if !is_own_process(pid) => return false,
            Ok(_) => tokio::time::sleep(Duration::from_millis(50)).await,
            Err(_) => return false,
        }
    }
    loopback_port_bindable(port)
}

pub fn wait_for_port_free_blocking(port: u16, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if loopback_port_bindable(port) {
            return true;
        }
        match platform().find_pid_listening_on_port(port) {
            Ok(Some(pid)) if !is_own_process(pid) => return false,
            Ok(_) => std::thread::sleep(Duration::from_millis(50)),
            Err(_) => return false,
        }
    }
    loopback_port_bindable(port)
}

pub async fn await_listener_shutdown(handle: Option<JoinHandle<()>>, port: u16) {
    if let Some(handle) = handle {
        let mut handle = handle;
        tokio::select! {
            _ = &mut handle => {}
            _ = tokio::time::sleep(Duration::from_secs(3)) => {
                handle.abort();
                let _ = handle.await;
            }
        }
    }

    if !wait_for_port_free(port, Duration::from_secs(5)).await {
        eprintln!(
            "listener port {port} is still not bindable after graceful shutdown; successor startup must wait for TCP release"
        );
    }
}

pub fn port_busy_message(port: u16, service_label: &str, pid: u32) -> String {
    let image = platform()
        .process_image_path(pid)
        .ok()
        .flatten()
        .unwrap_or_else(|| format!("pid {pid}"));

    if is_own_process(pid) {
        format!(
            "{service_label}端口 {port} 仍被本应用的上一次服务占用（{image}），请先停止服务或稍后再试"
        )
    } else {
        format!("{service_label}端口 {port} 已被占用：{image}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_bind_probe_tracks_real_bindability() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind fixture");
        let port = listener.local_addr().expect("fixture address").port();

        assert!(!loopback_port_bindable(port));
        drop(listener);
        assert!(loopback_port_bindable(port));
    }
}
