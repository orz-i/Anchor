/// Windows process creation flag that prevents console applications from
/// allocating a visible console window when launched by the desktop GUI.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Keep long-running supervised children in their own process group so the
/// existing shutdown logic can manage them independently.
#[cfg(windows)]
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

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

    const PROBE_OUTPUT_ENV: &str = "CODING_TOOLS_CONSOLE_PROBE_OUTPUT";

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
        assert_eq!(
            CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP,
            0x0800_0200
        );
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
