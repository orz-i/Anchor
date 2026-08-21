use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;

use crate::error::{AppError, AppResult};

const USER_BUS_RETRY_ATTEMPTS: usize = 20;
const USER_BUS_RETRY_DELAY: Duration = Duration::from_millis(50);

fn effective_uid() -> u32 {
    unsafe { libc::geteuid() }
}

fn user_runtime_dir(uid: u32) -> PathBuf {
    Path::new("/run/user").join(uid.to_string())
}

fn configure_user_systemctl(command: &mut Command, uid: u32) {
    // Non-interactive SSH sessions may omit XDG_RUNTIME_DIR even though the
    // per-user systemd manager is alive (for example after enabling linger).
    // systemctl --user derives its manager transport from this directory, so
    // make it deterministic for the effective user instead of trusting the
    // login-shell environment.
    command
        .arg("--user")
        .env("XDG_RUNTIME_DIR", user_runtime_dir(uid))
        // A stale session-bus override can otherwise point at a different or
        // already-closed login session. Let sd-bus select the canonical user bus.
        .env_remove("DBUS_SESSION_BUS_ADDRESS");
}

fn user_bus_unavailable(output: &Output) -> bool {
    !output.status.success()
        && String::from_utf8_lossy(&output.stderr).contains("Failed to connect to bus")
}

pub(crate) fn run_user_systemctl(args: &[&str], allow_nonzero: bool) -> AppResult<Output> {
    let uid = effective_uid();
    let runtime_dir = user_runtime_dir(uid);
    for attempt in 0..USER_BUS_RETRY_ATTEMPTS {
        let mut command = Command::new("systemctl");
        configure_user_systemctl(&mut command, uid);
        let output = command
            .args(args)
            .output()
            .map_err(|error| AppError::Message(format!("无法执行 systemctl --user：{error}")))?;
        let bus_unavailable = user_bus_unavailable(&output);
        if output.status.success() || (allow_nonzero && !bus_unavailable) {
            return Ok(output);
        }
        if bus_unavailable && attempt + 1 < USER_BUS_RETRY_ATTEMPTS {
            std::thread::sleep(USER_BUS_RETRY_DELAY);
            continue;
        }
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        if bus_unavailable {
            return Err(AppError::Message(format!(
                "systemctl --user {} 无法连接 user manager bus（uid={uid}, XDG_RUNTIME_DIR={}）：{stderr}；请确认 systemd-logind 与 user@{uid}.service 可用",
                args.join(" "),
                runtime_dir.display()
            )));
        }
        return Err(AppError::Message(format!(
            "systemctl --user {} 失败：{stderr}",
            args.join(" ")
        )));
    }
    unreachable!("systemctl user bus retry loop always returns")
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::ffi::OsStr;

    use super::*;

    #[test]
    fn user_systemctl_pins_runtime_dir_and_ignores_stale_bus_override() {
        let mut command = Command::new("systemctl");
        command.env("DBUS_SESSION_BUS_ADDRESS", "unix:path=/tmp/stale-bus");
        configure_user_systemctl(&mut command, 1234);

        assert_eq!(user_runtime_dir(1234), PathBuf::from("/run/user/1234"));
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            vec![OsStr::new("--user")]
        );
        let env = command.get_envs().collect::<BTreeMap<_, _>>();
        assert_eq!(
            env.get(OsStr::new("XDG_RUNTIME_DIR"))
                .and_then(|value| *value),
            Some(OsStr::new("/run/user/1234"))
        );
        assert_eq!(env.get(OsStr::new("DBUS_SESSION_BUS_ADDRESS")), Some(&None));
    }
}
