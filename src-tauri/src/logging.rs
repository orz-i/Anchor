use chrono::{SecondsFormat, Utc};

use crate::workspace::WorkspaceProfile;

#[cfg(windows)]
pub(crate) fn redirect_stdio_to_file(path: &std::path::Path) -> crate::error::AppResult<()> {
    use std::fs::OpenOptions;
    use std::os::windows::io::IntoRawHandle;

    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::Console::{SetStdHandle, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE};

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let stdout = OpenOptions::new().create(true).append(true).open(path)?;
    let stderr = stdout.try_clone()?;
    let stdout_handle = HANDLE(stdout.into_raw_handle());
    let stderr_handle = HANDLE(stderr.into_raw_handle());
    unsafe {
        SetStdHandle(STD_OUTPUT_HANDLE, stdout_handle).map_err(|error| {
            crate::error::AppError::Message(format!(
                "无法将 daemon stdout 重定向到 {}：{error}",
                path.display()
            ))
        })?;
        SetStdHandle(STD_ERROR_HANDLE, stderr_handle).map_err(|error| {
            crate::error::AppError::Message(format!(
                "无法将 daemon stderr 重定向到 {}：{error}",
                path.display()
            ))
        })?;
    }
    // SetStdHandle does not duplicate/own the supplied handles. into_raw_handle
    // intentionally keeps both files open for the lifetime of this daemon.
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProfileLogService {
    Mcp,
    Actions,
}

pub(crate) type ProfileLogFile = (&'static str, &'static str);

const MCP_LOG_FILES: &[ProfileLogFile] = &[
    ("mcp-oauth", "mcp-oauth.log"),
    ("mcp-requests", "mcp-requests.log"),
    ("mcp-stderr", "stderr.log"),
    ("mcp-stdout", "stdout.log"),
];

const ACTIONS_LOG_FILES: &[ProfileLogFile] = &[
    ("actions-oauth", "actions-oauth.log"),
    ("actions-stderr", "actions-stderr.log"),
    ("actions-stdout", "actions-stdout.log"),
];

pub(crate) fn profile_log_files(
    profile: &WorkspaceProfile,
    service: ProfileLogService,
) -> Vec<ProfileLogFile> {
    let (tunnel_type, tunnel_file, base_files) = match service {
        ProfileLogService::Mcp => (
            profile.tunnel.tunnel_type.as_str(),
            (
                "mcp-cloudflare",
                "mcp-frp",
                "cloudflared.log",
                "frpc-mcp.log",
            ),
            MCP_LOG_FILES,
        ),
        ProfileLogService::Actions => (
            profile.actions.tunnel_type.as_str(),
            (
                "actions-cloudflare",
                "actions-frp",
                "actions-cloudflared.log",
                "frpc-actions.log",
            ),
            ACTIONS_LOG_FILES,
        ),
    };
    let mut files = Vec::with_capacity(base_files.len() + 1);
    match tunnel_type {
        "cloudflare" => files.push((tunnel_file.0, tunnel_file.2)),
        "frp" => files.push((tunnel_file.1, tunnel_file.3)),
        _ => {}
    }
    files.extend_from_slice(base_files);
    files
}

pub(crate) fn timestamped_line(line: &str) -> String {
    let timestamp = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
    format!("[{timestamp}] {line}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_line_uses_parseable_utc_rfc3339_timestamp() {
        let line = timestamped_line("[mcp] started");
        let (timestamp, message) = line
            .strip_prefix('[')
            .and_then(|value| value.split_once("] "))
            .expect("timestamp prefix");

        assert!(timestamp.ends_with('Z'));
        assert!(chrono::DateTime::parse_from_rfc3339(timestamp).is_ok());
        assert_eq!(message, "[mcp] started");
    }

    #[test]
    fn gui_and_cli_share_complete_profile_log_catalogs() {
        let mut profile = WorkspaceProfile::new(".".into(), Some("logs".into()));
        profile.tunnel.tunnel_type = "cloudflare".into();
        profile.actions.tunnel_type = "frp".into();

        let mcp = profile_log_files(&profile, ProfileLogService::Mcp);
        let actions = profile_log_files(&profile, ProfileLogService::Actions);

        assert!(mcp.iter().any(|file| file.1 == "cloudflared.log"));
        assert!(mcp.iter().any(|file| file.1 == "mcp-oauth.log"));
        assert!(mcp.iter().any(|file| file.1 == "mcp-requests.log"));
        assert!(actions.iter().any(|file| file.1 == "frpc-actions.log"));
        assert!(actions.iter().any(|file| file.1 == "actions-oauth.log"));
    }
}
