use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use crate::error::AppResult;
use crate::gateway_daemon;

use super::protocol::{GatewayLogChunk, GatewayLogCursor};

const MAX_LOG_SCAN_BYTES: u64 = 1_048_576;
const MAX_LOG_CONTENT_BYTES: usize = 8 * 1024;
const MAX_TAIL_LINES: usize = 5_000;

pub fn read_gateway_log(
    tail_lines: u32,
    cursor: Option<&GatewayLogCursor>,
) -> AppResult<GatewayLogChunk> {
    let path = gateway_daemon::daemon_log_path()?;
    match cursor {
        Some(cursor) => read_since(&path, cursor.offset, MAX_LOG_CONTENT_BYTES),
        None => read_tail(
            &path,
            usize::try_from(tail_lines)
                .unwrap_or(MAX_TAIL_LINES)
                .min(MAX_TAIL_LINES),
            MAX_LOG_CONTENT_BYTES,
        ),
    }
}

fn read_tail(path: &Path, lines: usize, limit: usize) -> AppResult<GatewayLogChunk> {
    let Ok(mut file) = File::open(path) else {
        return Ok(missing_chunk(path));
    };
    let size = file.seek(SeekFrom::End(0))?;
    let scan_start = size.saturating_sub(MAX_LOG_SCAN_BYTES);
    file.seek(SeekFrom::Start(scan_start))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    let text = String::from_utf8_lossy(&bytes);
    let all_lines = text.lines().collect::<Vec<_>>();
    let selected_start = all_lines.len().saturating_sub(lines);
    let mut content = all_lines[selected_start..].join("\n");
    if !content.is_empty() {
        content.push('\n');
    }
    let content_was_trimmed = trim_to_last_bytes(&mut content, limit);
    Ok(GatewayLogChunk {
        name: "gateway-daemon".into(),
        path: path.display().to_string(),
        content,
        next_offset: size,
        exists: true,
        truncated: scan_start > 0 || selected_start > 0 || content_was_trimmed,
    })
}

fn read_since(path: &Path, offset: u64, limit: usize) -> AppResult<GatewayLogChunk> {
    let Ok(mut file) = File::open(path) else {
        return Ok(missing_chunk(path));
    };
    let size = file.seek(SeekFrom::End(0))?;
    let start = if offset > size { 0 } else { offset };
    file.seek(SeekFrom::Start(start))?;
    let available = size.saturating_sub(start);
    let read_len = available.min(u64::try_from(limit).unwrap_or(u64::MAX));
    let mut bytes = vec![0; usize::try_from(read_len).unwrap_or(0)];
    file.read_exact(&mut bytes)?;
    let next_offset = start.saturating_add(read_len);
    Ok(GatewayLogChunk {
        name: "gateway-daemon".into(),
        path: path.display().to_string(),
        content: String::from_utf8_lossy(&bytes).into_owned(),
        next_offset,
        exists: true,
        truncated: next_offset < size,
    })
}

fn missing_chunk(path: &Path) -> GatewayLogChunk {
    GatewayLogChunk {
        name: "gateway-daemon".into(),
        path: path.display().to_string(),
        content: String::new(),
        next_offset: 0,
        exists: false,
        truncated: false,
    }
}

fn trim_to_last_bytes(content: &mut String, limit: usize) -> bool {
    if content.len() <= limit {
        return false;
    }
    if limit == 0 {
        content.clear();
        return true;
    }
    let mut start = content.len().saturating_sub(limit);
    while start < content.len() && !content.is_char_boundary(start) {
        start += 1;
    }
    *content = content[start..].to_string();
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway_control::protocol::{
        GatewayResponse, GatewayResult, MAX_GATEWAY_CONTROL_FRAME_BYTES,
    };

    #[test]
    fn tail_and_cursor_reads_are_bounded_and_rotation_safe() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("gateway.log");
        std::fs::write(&path, "one\ntwo\nthree\n").expect("write log");

        let tail = read_tail(&path, 2, 1024).expect("tail");
        assert_eq!(tail.content, "two\nthree\n");
        assert_eq!(tail.next_offset, 14);

        let append = read_since(&path, 4, 1024).expect("cursor");
        assert_eq!(append.content, "two\nthree\n");

        std::fs::write(&path, "new\n").expect("rotate log");
        let rotated = read_since(&path, 100, 1024).expect("rotated cursor");
        assert_eq!(rotated.content, "new\n");
        assert_eq!(rotated.next_offset, 4);
    }

    #[test]
    fn hostile_log_content_still_fits_one_gateway_control_frame() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("gateway.log");
        std::fs::write(&path, vec![0u8; 32 * 1024]).expect("write hostile log");
        let chunk = read_since(&path, 0, MAX_LOG_CONTENT_BYTES).expect("bounded read");
        let response = GatewayResponse::success("request".into(), GatewayResult::Logs { chunk });
        let encoded = serde_json::to_vec(&response).expect("serialize response");
        assert!(encoded.len() < MAX_GATEWAY_CONTROL_FRAME_BYTES);
    }

    #[test]
    fn log_budget_preserves_utf8_boundaries() {
        let mut content = "甲乙丙丁".to_string();
        assert!(trim_to_last_bytes(&mut content, 7));
        assert!(content.is_char_boundary(0));
        assert!(content.len() <= 7);
    }
}
