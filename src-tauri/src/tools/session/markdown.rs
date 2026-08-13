use std::sync::OnceLock;

use regex::Regex;
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::model::CheckpointRecord;

const CHECKPOINT_HEADING: &str = "## 本轮检查点";
pub(super) const MAX_TURN_ID_CHARS: usize = 128;
pub(super) const MAX_TIMESTAMP_CHARS: usize = 128;
pub(super) const MAX_USER_INTENT_CHARS: usize = 4_000;
pub(super) const MAX_NOTES_CHARS: usize = 8_000;
pub(super) const MAX_ARRAY_ITEMS: usize = 64;
pub(super) const MAX_ARRAY_ITEM_CHARS: usize = 2_000;
pub(super) const MAX_CHECKPOINT_BYTES: usize = 64 * 1024;
pub(super) const VALID_SESSION_STATUSES: &[&str] = &["active", "paused", "completed"];

pub fn metadata(content: &str, label: &str) -> Option<String> {
    let prefix = format!("**{label}:**");
    content.lines().find_map(|line| {
        line.trim()
            .strip_prefix(&prefix)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    })
}

pub fn validate_checkpoint_record(record: &CheckpointRecord) -> Result<(), String> {
    validate_checkpoint_size(record)
}

pub fn update_document_lifecycle(content: &str, updated_at: &str, status: &str) -> String {
    let mut output = String::with_capacity(content.len() + updated_at.len() + status.len());
    let mut updated_written = false;
    let mut status_written = false;
    for line in content.lines() {
        if line.trim_start().starts_with("**Updated:**") {
            output.push_str(&format!("**Updated:** {updated_at}\n"));
            updated_written = true;
        } else if line.trim_start().starts_with("**Status:**") {
            output.push_str(&format!("**Status:** {status}\n"));
            status_written = true;
        } else {
            output.push_str(line);
            output.push('\n');
        }
    }
    if !updated_written || !status_written {
        return content.to_string();
    }
    output
}

pub fn validate_session_status(value: &str) -> Result<&str, String> {
    let status = value.trim();
    if VALID_SESSION_STATUSES.contains(&status) {
        Ok(status)
    } else {
        Err(format!(
            "session_status must be one of: {}",
            VALID_SESSION_STATUSES.join(", ")
        ))
    }
}

pub fn document_title(content: &str) -> String {
    content
        .lines()
        .find_map(|line| line.trim().strip_prefix("# "))
        .and_then(|line| line.split_once('：').map(|(_, title)| title.trim()))
        .filter(|title| !title.is_empty())
        .unwrap_or("开发会话")
        .to_string()
}

pub fn render_document(
    session_id: &str,
    title: &str,
    host_session_key: Option<&str>,
    created_at: &str,
    updated_at: &str,
    status: &str,
    records: &[CheckpointRecord],
) -> String {
    let title = if title.trim().is_empty() {
        "开发会话"
    } else {
        title.trim()
    };
    let mut output = format!(
        "# Session：{title}\n\n\
**Session id:** {session_id}\n\
**Created:** {created_at}\n\
**Updated:** {updated_at}\n\
**Status:** {status}\n"
    );
    if let Some(host_session_key) = host_session_key.filter(|value| !value.trim().is_empty()) {
        output.push_str(&format!("**Host session key:** {host_session_key}\n"));
    }
    output.push('\n');
    push_section(
        &mut output,
        "用户核心目标",
        records
            .iter()
            .map(|record| record.user_intent.as_str())
            .filter(|value| !value.is_empty()),
    );
    push_section(
        &mut output,
        "已确认事实",
        records
            .iter()
            .flat_map(|record| record.findings.iter().map(String::as_str)),
    );
    push_section(
        &mut output,
        "已完成修改",
        records
            .iter()
            .flat_map(|record| record.files_changed.iter().map(String::as_str)),
    );
    push_section(
        &mut output,
        "关键设计决定",
        records
            .iter()
            .flat_map(|record| record.decisions.iter().map(String::as_str)),
    );
    push_section(
        &mut output,
        "测试结果",
        records
            .iter()
            .flat_map(|record| record.tests.iter().map(String::as_str)),
    );
    push_section(
        &mut output,
        "当前运行状态",
        records
            .iter()
            .flat_map(|record| record.runtime_state.iter().map(String::as_str)),
    );
    push_section(
        &mut output,
        "剩余问题",
        records
            .iter()
            .flat_map(|record| record.remaining_issues.iter().map(String::as_str)),
    );
    push_section(
        &mut output,
        "下一步",
        records
            .iter()
            .flat_map(|record| record.next_actions.iter().map(String::as_str)),
    );
    output.push_str(CHECKPOINT_HEADING);
    output.push_str("\n\n");
    for record in records {
        output.push_str("### ");
        output.push_str(&record.turn_id);
        output.push_str("\n\n```json\n");
        output.push_str(
            &serde_json::to_string_pretty(record).expect("checkpoint record is serializable"),
        );
        output.push_str("\n```\n\n");
    }
    output
}

fn push_section<'a>(output: &mut String, heading: &str, values: impl Iterator<Item = &'a str>) {
    output.push_str("## ");
    output.push_str(heading);
    output.push_str("\n\n");
    let mut seen = Vec::<String>::new();
    for value in values.map(str::trim).filter(|value| !value.is_empty()) {
        if !seen.iter().any(|existing| existing == value) {
            output.push_str("- ");
            output.push_str(value);
            output.push('\n');
            seen.push(value.to_string());
        }
    }
    output.push('\n');
}

pub fn parse_checkpoint_records(content: &str) -> Vec<CheckpointRecord> {
    let Some((_, checkpoint_text)) = content.split_once(CHECKPOINT_HEADING) else {
        return Vec::new();
    };
    let mut records = Vec::new();
    let mut remaining = checkpoint_text;
    while let Some(heading_pos) = remaining.find("\n### ") {
        remaining = &remaining[heading_pos + 1..];
        let Some(fence_start) = remaining.find("```json\n") else {
            break;
        };
        let json_start = fence_start + "```json\n".len();
        let Some(fence_end) = remaining[json_start..].find("\n```") else {
            break;
        };
        let json_text = &remaining[json_start..json_start + fence_end];
        if let Ok(record) = serde_json::from_str::<CheckpointRecord>(json_text) {
            records.push(record);
        }
        remaining = &remaining[json_start + fence_end + "\n```".len()..];
    }
    records
}

pub fn checkpoint_from_args(
    args: &Value,
    _default_timestamp: &str,
) -> Result<CheckpointRecord, String> {
    let explicit_turn_id = bounded_string_field(args, "turn_id", MAX_TURN_ID_CHARS)?
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let explicit_timestamp = bounded_string_field(args, "timestamp", MAX_TIMESTAMP_CHARS)?;
    let record = CheckpointRecord {
        turn_id: explicit_turn_id.unwrap_or_default(),
        timestamp: explicit_timestamp.clone().unwrap_or_default(),
        user_intent: bounded_string_field(args, "user_intent", MAX_USER_INTENT_CHARS)?
            .unwrap_or_default(),
        findings: string_array(args, "findings")?,
        decisions: string_array(args, "decisions")?,
        files_changed: string_array(args, "files_changed")?,
        tests: string_array(args, "tests")?,
        runtime_state: string_array(args, "runtime_state")?,
        remaining_issues: string_array(args, "remaining_issues")?,
        next_actions: string_array(args, "next_actions")?,
        notes: bounded_string_field(args, "notes", MAX_NOTES_CHARS)?.unwrap_or_default(),
    };
    validate_checkpoint_size(&record)?;
    Ok(record)
}

pub fn ensure_turn_id(record: &mut CheckpointRecord) {
    if record.turn_id.is_empty() {
        record.turn_id = automatic_turn_id(record);
    }
}

fn automatic_turn_id(record: &CheckpointRecord) -> String {
    let encoded = serde_json::to_vec(record).expect("checkpoint record is serializable");
    let hash = format!("{:x}", Sha256::digest(encoded));
    format!("auto-{}", &hash[..16])
}

fn bounded_string_field(
    args: &Value,
    name: &str,
    max_chars: usize,
) -> Result<Option<String>, String> {
    let Some(value) = args.get(name) else {
        return Ok(None);
    };
    let value = value
        .as_str()
        .ok_or_else(|| format!("{name} must be a string"))?;
    if value.chars().count() > max_chars {
        return Err(format!("{name} cannot exceed {max_chars} characters"));
    }
    Ok(Some(value.to_string()))
}

fn string_array(args: &Value, name: &str) -> Result<Vec<String>, String> {
    let Some(value) = args.get(name) else {
        return Ok(Vec::new());
    };
    let array = value
        .as_array()
        .ok_or_else(|| format!("{name} must be an array of strings"))?;
    if array.len() > MAX_ARRAY_ITEMS {
        return Err(format!(
            "{name} cannot contain more than {MAX_ARRAY_ITEMS} items"
        ));
    }
    array
        .iter()
        .map(|item| {
            let value = item
                .as_str()
                .ok_or_else(|| format!("{name} must contain only strings"))?;
            if value.chars().count() > MAX_ARRAY_ITEM_CHARS {
                return Err(format!(
                    "{name} items cannot exceed {MAX_ARRAY_ITEM_CHARS} characters"
                ));
            }
            Ok(value.to_string())
        })
        .collect()
}

fn validate_checkpoint_size(record: &CheckpointRecord) -> Result<(), String> {
    let bytes = serde_json::to_vec(record)
        .map_err(|error| format!("checkpoint record cannot be serialized: {error}"))?;
    if bytes.len() > MAX_CHECKPOINT_BYTES {
        return Err(format!(
            "checkpoint content cannot exceed {MAX_CHECKPOINT_BYTES} bytes"
        ));
    }
    Ok(())
}

pub fn redact_record(record: &mut CheckpointRecord) -> bool {
    let mut changed = redact_text(&mut record.timestamp);
    changed |= redact_text(&mut record.user_intent);
    changed |= redact_text(&mut record.notes);
    for values in [
        &mut record.findings,
        &mut record.decisions,
        &mut record.files_changed,
        &mut record.tests,
        &mut record.runtime_state,
        &mut record.remaining_issues,
        &mut record.next_actions,
    ] {
        for value in values {
            changed |= redact_text(value);
        }
    }
    changed
}

fn redact_text(value: &mut String) -> bool {
    static PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();
    let patterns = PATTERNS.get_or_init(|| {
        vec![
            Regex::new(r"(?i)\b(bearer\s+)[A-Za-z0-9._~+/=-]{6,}").expect("bearer regex"),
            Regex::new(r"(?i)\b(api[_ -]?key|token|cookie|password|passwd|pwd)\s*[:=]\s*[^\s,;]+")
                .expect("secret assignment regex"),
            Regex::new(r"(?is)-----BEGIN[^\n]*PRIVATE KEY-----.*?-----END[^\n]*PRIVATE KEY-----")
                .expect("private key regex"),
        ]
    });
    let original = value.clone();
    let mut redacted = value.clone();
    redacted = patterns[0]
        .replace_all(&redacted, "${1}[REDACTED]")
        .into_owned();
    redacted = patterns[1]
        .replace_all(&redacted, |captures: &regex::Captures<'_>| {
            format!("{}=[REDACTED]", &captures[1])
        })
        .into_owned();
    redacted = patterns[2]
        .replace_all(&redacted, "[REDACTED]")
        .into_owned();
    *value = redacted;
    *value != original
}
