/// Windows process creation flag that prevents console applications from
/// allocating a visible console window when launched by the desktop GUI.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Keep long-running supervised children in their own process group so the
/// existing shutdown logic can manage them independently.
#[cfg(windows)]
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

/// Permit the durable command supervisor to leave the daemon's kill-on-close
/// Job Object. The daemon Job explicitly opts into BREAKAWAY_OK; ordinary
/// workspace children never receive this flag and remain lifecycle-bound.
#[cfg(windows)]
const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;

/// Keep arbitrary workspace commands below normal priority so they yield CPU
/// to the desktop, daemon control plane, and other interactive applications.
#[cfg(windows)]
const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x0000_4000;

/// Configure a Tokio child process as an internal background process.
///
/// stdout/stderr pipes continue to work; only the Windows console window is
/// suppressed. GUI-subsystem applications are unaffected and may still show
/// their own intended windows.
pub fn hide_tokio_console(command: &mut tokio::process::Command) {
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);

    #[cfg(not(windows))]
    let _ = command;
}

/// Configure the hidden durable command supervisor so its lifetime is not
/// coupled to the MCP daemon process. The supervisor, not the daemon, owns the
/// real workspace child and its output files.
pub fn configure_durable_supervisor_tokio_process(command: &mut tokio::process::Command) {
    #[cfg(windows)]
    command.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW | CREATE_BREAKAWAY_FROM_JOB);

    #[cfg(unix)]
    command.process_group(0);

    #[cfg(not(any(windows, unix)))]
    let _ = command;
}

/// Configure an arbitrary workspace command for host responsiveness.
///
/// This is intentionally separate from supervised daemon children: command
/// execution should yield CPU to interactive processes, while daemon/tunnel
/// lifecycle processes keep their existing scheduling behavior.
pub fn configure_exec_tokio_process(command: &mut tokio::process::Command) {
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW | BELOW_NORMAL_PRIORITY_CLASS);

    #[cfg(unix)]
    command.process_group(0);

    #[cfg(not(any(windows, unix)))]
    let _ = command;
}

/// Signal the complete process tree owned by one workspace command.
///
/// Unix workspace commands are created as process-group leaders, so signaling
/// the negative root PID reaches the root and every descendant that inherited
/// that group. Windows does not provide an equivalent kill-by-process-group
/// primitive; its verified tree terminator repeatedly snapshots descendants
/// before terminating the root.
pub(crate) fn signal_exec_process_tree(pid: u32, signal: &str) -> crate::error::AppResult<()> {
    #[cfg(unix)]
    {
        let pid = i32::try_from(pid).map_err(|_| {
            crate::error::AppError::Message(format!("invalid exec process-group pid: {pid}"))
        })?;
        if pid <= 0 {
            return Err(crate::error::AppError::Message(format!(
                "invalid exec process-group pid: {pid}"
            )));
        }
        let signal = match signal {
            "KILL" => libc::SIGKILL,
            "INT" => libc::SIGINT,
            _ => libc::SIGTERM,
        };
        let result = unsafe { libc::kill(-pid, signal) };
        if result == 0 {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            // A configured workspace command must be its own process-group
            // leader. If the root PID is still alive while its group is
            // missing, fail closed instead of silently falling back to a
            // root-only kill that can leak descendants.
            if unsafe { libc::kill(pid, 0) } == 0
                || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
            {
                return Err(crate::error::AppError::Message(format!(
                    "exec process group {pid} is missing while root process is still alive"
                )));
            }
            return Ok(());
        }
        Err(crate::error::AppError::Message(format!(
            "signal exec process group {pid} failed: {error}"
        )))
    }

    #[cfg(windows)]
    {
        let _ = signal;
        crate::platform::platform().terminate_process_tree(pid)
    }

    #[cfg(not(any(windows, unix)))]
    {
        let _ = (pid, signal);
        Ok(())
    }
}

/// Return whether the process tree for a workspace command can still contain
/// live members after a termination request.
pub(crate) fn exec_process_tree_is_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        let Ok(pid) = i32::try_from(pid) else {
            return false;
        };
        if pid <= 0 {
            return false;
        }
        let result = unsafe { libc::kill(-pid, 0) };
        if result == 0 {
            return true;
        }
        std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }

    #[cfg(windows)]
    {
        // Windows tree termination is synchronous and does not return until
        // descendants have been cleared before the root is terminated. The
        // root liveness check is therefore sufficient after that operation.
        return crate::platform::platform().is_process_alive(pid);
    }

    #[cfg(not(any(windows, unix)))]
    {
        let _ = pid;
        false
    }
}

/// Lower the priority of a newly spawned arbitrary workspace command.
///
/// Unix applies this from the parent immediately after spawn instead of a
/// `pre_exec` hook, avoiding non-async-signal-safe work in the post-fork child.
/// Windows already applies BELOW_NORMAL_PRIORITY_CLASS at process creation.
pub fn lower_exec_child_priority(child: &tokio::process::Child) {
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        // SAFETY: this call runs in the parent process and targets a child PID
        // returned by Tokio. Failure is deliberately non-fatal: the resource
        // governor and child parallelism caps still protect the host.
        unsafe {
            let _ = libc::setpriority(libc::PRIO_PROCESS, pid as libc::id_t, 5);
        }
    }

    #[cfg(not(unix))]
    let _ = child;
}

/// Configure a blocking std child process as an internal background process.
pub fn hide_std_console(command: &mut std::process::Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NO_WINDOW);
    }

    #[cfg(not(windows))]
    let _ = command;
}

/// Configure a long-running child that is supervised by the runtime.
pub fn configure_supervised_tokio_process(command: &mut tokio::process::Command) {
    #[cfg(windows)]
    command.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);

    #[cfg(unix)]
    command.process_group(0);

    #[cfg(not(any(windows, unix)))]
    let _ = command;
}

#[cfg(all(test, windows))]
mod tests {
    use std::ffi::c_void;

    use super::*;

    const PROBE_OUTPUT_ENV: &str = "ANCHOR_CONSOLE_PROBE_OUTPUT";

    #[link(name = "Kernel32")]
    extern "system" {
        fn GetConsoleWindow() -> *mut c_void;
    }

    #[test]
    #[ignore = "launched by the parent tests with CREATE_NO_WINDOW"]
    fn console_probe_child() {
        let Some(output) = std::env::var_os(PROBE_OUTPUT_ENV) else {
            return;
        };
        let has_console = unsafe { !GetConsoleWindow().is_null() };
        std::fs::write(output, if has_console { "visible" } else { "hidden" })
            .expect("write console probe result");
    }

    fn probe_result_path() -> (tempfile::TempDir, std::path::PathBuf) {
        let temp = tempfile::tempdir().expect("console probe tempdir");
        let result = temp.path().join("console-window.txt");
        (temp, result)
    }

    fn configure_probe(command: &mut std::process::Command, result: &std::path::Path) {
        command
            .arg("console_probe_child")
            .arg("--ignored")
            .arg("--nocapture")
            .env(PROBE_OUTPUT_ENV, result);
    }

    fn read_probe_result(path: &std::path::Path) -> String {
        std::fs::read_to_string(path).expect("read console probe result")
    }

    #[test]
    fn supervised_children_keep_console_hidden_and_use_a_process_group() {
        assert_eq!(CREATE_NO_WINDOW, 0x0800_0000);
        assert_eq!(CREATE_NEW_PROCESS_GROUP, 0x0000_0200);
        assert_eq!(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP, 0x0800_0200);
    }

    #[test]
    fn durable_supervisor_uses_job_breakaway_flag() {
        assert_eq!(CREATE_BREAKAWAY_FROM_JOB, 0x0100_0000);
        assert_eq!(
            CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB,
            0x0900_0200
        );
    }

    #[test]
    fn exec_children_use_below_normal_priority_without_a_console() {
        assert_eq!(BELOW_NORMAL_PRIORITY_CLASS, 0x0000_4000);
        assert_eq!(CREATE_NO_WINDOW | BELOW_NORMAL_PRIORITY_CLASS, 0x0800_4000);
    }

    #[test]
    fn std_background_child_has_no_console_window() {
        let (_temp, result) = probe_result_path();
        let mut command = std::process::Command::new(std::env::current_exe().expect("test exe"));
        configure_probe(&mut command, &result);
        hide_std_console(&mut command);

        let status = command.status().expect("run std console probe");

        assert!(status.success());
        assert_eq!(read_probe_result(&result), "hidden");
    }

    #[tokio::test]
    async fn tokio_background_child_has_no_console_window() {
        let (_temp, result) = probe_result_path();
        let mut command = tokio::process::Command::new(std::env::current_exe().expect("test exe"));
        command
            .arg("console_probe_child")
            .arg("--ignored")
            .arg("--nocapture")
            .env(PROBE_OUTPUT_ENV, &result);
        hide_tokio_console(&mut command);

        let status = command.status().await.expect("run Tokio console probe");

        assert!(status.success());
        assert_eq!(read_probe_result(&result), "hidden");
    }

    #[tokio::test]
    async fn supervised_background_child_has_no_console_window() {
        let (_temp, result) = probe_result_path();
        let mut command = tokio::process::Command::new(std::env::current_exe().expect("test exe"));
        command
            .arg("console_probe_child")
            .arg("--ignored")
            .arg("--nocapture")
            .env(PROBE_OUTPUT_ENV, &result);
        configure_supervised_tokio_process(&mut command);

        let status = command
            .status()
            .await
            .expect("run supervised console probe");

        assert!(status.success());
        assert_eq!(read_probe_result(&result), "hidden");
    }
}
