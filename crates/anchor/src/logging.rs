use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use chrono::{SecondsFormat, Utc};
use tokio::io::{
    AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncSeekExt, AsyncWriteExt, BufReader,
};
use tokio::sync::mpsc;

use crate::workspace::WorkspaceProfile;

pub(crate) const PROCESS_LOG_CHANNEL_CAPACITY: usize = 128;
pub(crate) const PROCESS_LOG_LINE_MAX_BYTES: usize = 16 * 1024;
const PROFILE_LOG_MAX_BYTES: u64 = 8 * 1024 * 1024;
const PROFILE_LOG_RETAIN_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Debug, Clone, Copy)]
struct LogBounds {
    max_bytes: u64,
    retain_bytes: u64,
}

const PROFILE_LOG_BOUNDS: LogBounds = LogBounds {
    max_bytes: PROFILE_LOG_MAX_BYTES,
    retain_bytes: PROFILE_LOG_RETAIN_BYTES,
};

static PROFILE_LOG_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

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
}

pub(crate) type ProfileLogFile = (&'static str, &'static str);

const MCP_LOG_FILES: &[ProfileLogFile] = &[
    ("mcp-oauth", "mcp-oauth.log"),
    ("mcp-requests", "mcp-requests.log"),
    ("mcp-stderr", "stderr.log"),
    ("mcp-stdout", "stdout.log"),
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

fn truncate_log_line(line: &str, max_bytes: usize) -> String {
    if line.len() <= max_bytes {
        return line.to_string();
    }
    const MARKER: &str = " …[truncated]";
    let budget = max_bytes.saturating_sub(MARKER.len());
    let mut end = budget.min(line.len());
    while end > 0 && !line.is_char_boundary(end) {
        end -= 1;
    }
    let mut output = line[..end].to_string();
    output.push_str(MARKER);
    output
}

fn bounded_log_payload(line: &str) -> Vec<u8> {
    let line = truncate_log_line(line, PROCESS_LOG_LINE_MAX_BYTES);
    let mut bytes = timestamped_line(&line).into_bytes();
    bytes.push(b'\n');
    bytes
}

fn align_retained_tail(mut tail: Vec<u8>, started_mid_file: bool) -> Vec<u8> {
    if started_mid_file {
        if let Some(newline) = tail.iter().position(|byte| *byte == b'\n') {
            tail.drain(..=newline);
        }
    }
    tail
}

fn trim_sync_tail(file: &mut File, len: u64, retain_bytes: u64) -> std::io::Result<u64> {
    let retain = retain_bytes.min(len);
    file.flush()?;
    file.seek(SeekFrom::Start(len.saturating_sub(retain)))?;
    let mut tail = Vec::with_capacity(retain as usize);
    file.read_to_end(&mut tail)?;
    let tail = align_retained_tail(tail, len > retain);
    file.set_len(0)?;
    file.seek(SeekFrom::Start(0))?;
    file.write_all(&tail)?;
    file.seek(SeekFrom::End(0))?;
    Ok(tail.len() as u64)
}

fn append_bounded_profile_log_with_bounds(
    path: &Path,
    line: &str,
    bounds: LogBounds,
) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let payload = bounded_log_payload(line);
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .append(true)
        .open(path)?;
    let mut len = file.metadata()?.len();
    if len > bounds.max_bytes {
        len = trim_sync_tail(&mut file, len, bounds.retain_bytes)?;
    } else {
        file.seek(SeekFrom::End(0))?;
    }
    if len.saturating_add(payload.len() as u64) > bounds.max_bytes {
        let retain = bounds
            .retain_bytes
            .min(bounds.max_bytes.saturating_sub(payload.len() as u64));
        let _ = trim_sync_tail(&mut file, len, retain)?;
    }
    file.write_all(&payload)
}

pub(crate) fn append_bounded_profile_log(path: &Path, line: &str) -> std::io::Result<()> {
    let _guard = PROFILE_LOG_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    append_bounded_profile_log_with_bounds(path, line, PROFILE_LOG_BOUNDS)
}

pub(crate) struct BoundedAsyncLogWriter {
    file: tokio::fs::File,
    len: u64,
    bounds: LogBounds,
}

impl BoundedAsyncLogWriter {
    pub(crate) async fn open(path: &Path) -> std::io::Result<Self> {
        Self::open_with_bounds(path, PROFILE_LOG_BOUNDS).await
    }

    async fn open_with_bounds(path: &Path, bounds: LogBounds) -> std::io::Result<Self> {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let file = tokio::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .append(true)
            .open(path)
            .await?;
        let len = file.metadata().await?.len();
        let mut writer = Self { file, len, bounds };
        if writer.len > bounds.max_bytes {
            writer.trim_tail(bounds.retain_bytes).await?;
        } else {
            writer.file.seek(SeekFrom::End(0)).await?;
        }
        Ok(writer)
    }

    pub(crate) async fn write_line(&mut self, line: &str) -> std::io::Result<()> {
        let payload = bounded_log_payload(line);
        if self.len.saturating_add(payload.len() as u64) > self.bounds.max_bytes {
            let retain = self
                .bounds
                .retain_bytes
                .min(self.bounds.max_bytes.saturating_sub(payload.len() as u64));
            self.trim_tail(retain).await?;
        }
        self.file.write_all(&payload).await?;
        self.len = self.len.saturating_add(payload.len() as u64);
        Ok(())
    }

    async fn trim_tail(&mut self, retain_bytes: u64) -> std::io::Result<()> {
        self.file.flush().await?;
        let len = self.file.metadata().await?.len();
        let retain = retain_bytes.min(len);
        self.file
            .seek(SeekFrom::Start(len.saturating_sub(retain)))
            .await?;
        let mut tail = Vec::with_capacity(retain as usize);
        self.file.read_to_end(&mut tail).await?;
        let tail = align_retained_tail(tail, len > retain);
        self.file.set_len(0).await?;
        self.file.seek(SeekFrom::Start(0)).await?;
        self.file.write_all(&tail).await?;
        self.file.flush().await?;
        self.len = tail.len() as u64;
        Ok(())
    }

    pub(crate) async fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush().await
    }
}

async fn read_bounded_line<R>(reader: &mut R, max_bytes: usize) -> std::io::Result<Option<String>>
where
    R: AsyncBufRead + Unpin,
{
    let mut bytes = Vec::with_capacity(max_bytes.min(1024));
    let mut saw_input = false;
    let mut truncated = false;
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            if !saw_input {
                return Ok(None);
            }
            break;
        }
        saw_input = true;
        let newline = available.iter().position(|byte| *byte == b'\n');
        let consume = newline.map(|index| index + 1).unwrap_or(available.len());
        let content = newline.unwrap_or(consume);
        if bytes.len() < max_bytes {
            let copy = content.min(max_bytes - bytes.len());
            bytes.extend_from_slice(&available[..copy]);
            truncated |= copy < content;
        } else {
            truncated |= content > 0;
        }
        reader.consume(consume);
        if newline.is_some() {
            break;
        }
    }
    if bytes.last() == Some(&b'\r') {
        bytes.pop();
    }
    let mut line = String::from_utf8_lossy(&bytes).into_owned();
    if truncated {
        line.push_str(" …[truncated]");
    }
    Ok(Some(line))
}

pub(crate) async fn forward_bounded_lines<R>(reader: R, tx: mpsc::Sender<String>)
where
    R: AsyncRead + Unpin,
{
    let mut reader = BufReader::new(reader);
    while let Ok(Some(line)) = read_bounded_line(&mut reader, PROCESS_LOG_LINE_MAX_BYTES).await {
        if tx.send(line).await.is_err() {
            break;
        }
    }
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

        let mcp = profile_log_files(&profile, ProfileLogService::Mcp);

        assert!(mcp.iter().any(|file| file.1 == "cloudflared.log"));
        assert!(mcp.iter().any(|file| file.1 == "mcp-oauth.log"));
        assert!(mcp.iter().any(|file| file.1 == "mcp-requests.log"));
    }

    #[test]
    fn bounded_profile_log_rotates_before_exceeding_limit() {
        let temp = tempfile::tempdir().expect("log root");
        let path = temp.path().join("mcp-requests.log");
        let bounds = LogBounds {
            max_bytes: 512,
            retain_bytes: 192,
        };

        for index in 0..24 {
            append_bounded_profile_log_with_bounds(
                &path,
                &format!("event-{index:02} {}", "x".repeat(48)),
                bounds,
            )
            .expect("append bounded profile log");
            assert!(std::fs::metadata(&path).expect("metadata").len() <= bounds.max_bytes);
        }

        let content = std::fs::read_to_string(&path).expect("bounded log");
        assert!(content.contains("event-23"));
        assert!(!content.contains("event-00"));
        assert!(content.lines().all(|line| line.starts_with('[')));
    }

    #[tokio::test]
    async fn bounded_async_log_writer_retains_recent_complete_lines() {
        let temp = tempfile::tempdir().expect("log root");
        let path = temp.path().join("cloudflared.log");
        let bounds = LogBounds {
            max_bytes: 640,
            retain_bytes: 256,
        };
        let mut writer = BoundedAsyncLogWriter::open_with_bounds(&path, bounds)
            .await
            .expect("open bounded writer");

        for index in 0..24 {
            writer
                .write_line(&format!("cloudflare-{index:02} {}", "y".repeat(48)))
                .await
                .expect("write bounded tunnel log");
        }
        writer.flush().await.expect("flush bounded writer");

        assert!(tokio::fs::metadata(&path).await.expect("metadata").len() <= bounds.max_bytes);
        let content = tokio::fs::read_to_string(&path).await.expect("bounded log");
        assert!(content.contains("cloudflare-23"));
        assert!(!content.contains("cloudflare-00"));
        assert!(content.lines().all(|line| line.starts_with('[')));
    }

    #[tokio::test]
    async fn bounded_line_reader_discards_oversized_tail_and_keeps_next_line() {
        let input = format!("{}\r\nnext-line\n", "z".repeat(4096));
        let mut reader = BufReader::new(input.as_bytes());

        let first = read_bounded_line(&mut reader, 64)
            .await
            .expect("read oversized line")
            .expect("first line");
        assert!(first.starts_with(&"z".repeat(64)));
        assert!(first.ends_with("…[truncated]"));

        let second = read_bounded_line(&mut reader, 64)
            .await
            .expect("read next line")
            .expect("second line");
        assert_eq!(second, "next-line");
        assert!(read_bounded_line(&mut reader, 64)
            .await
            .expect("read eof")
            .is_none());
    }
}
