mod markdown;
mod model;
mod storage;

use std::fs;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use crate::tools::context::ToolContext;
use crate::tools::workspace::{tool_ok, WorkspaceError, WorkspaceResult};

const MAX_HOST_SESSION_KEY_CHARS: usize = 256;
const MAX_SESSION_ID_CHARS: usize = 64;
const MAX_SESSION_TITLE_CHARS: usize = 200;
const MAX_EXPECTED_PATH_CHARS: usize = 1024;
const DEFAULT_LIST_LIMIT: usize = 20;
const MAX_LIST_LIMIT: usize = 100;
const DEFAULT_GET_BYTES: usize = 64 * 1024;
const MAX_GET_BYTES: usize = 256 * 1024;

pub fn open(ctx: &ToolContext, args: &Value) -> WorkspaceResult<Value> {
    let session_dir = resolve_dir(ctx, args)?;
    storage::ensure_directory(&session_dir)?;
    let _lock = storage::lock_directory(&session_dir)?;
    let mut index = match storage::read_index(&session_dir)? {
        Some(index) => index,
        None => {
            let has_documents = fs::read_dir(&session_dir)
                .map_err(|error| {
                    session_error(
                        "SESSION_READ_FAILED",
                        &error.to_string(),
                        "filesystem",
                        true,
                        json!({}),
                    )
                })?
                .filter_map(Result::ok)
                .any(|entry| {
                    entry.path().extension().and_then(|value| value.to_str()) == Some("md")
                });
            if has_documents {
                return Err(session_error(
                    "SESSION_INDEX_REQUIRED",
                    "Session index is missing while Session documents exist; run session operation=validate with repair=true.",
                    "validation",
                    true,
                    json!({"session_dir": session_dir_display(ctx, &session_dir)}),
                ));
            }
            model::SessionIndex::default()
        }
    };

    let explicit_session_id = args
        .get("session_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if let Some(session_id) = explicit_session_id {
        validate_session_id(session_id)?;
    }
    let host_session_key = resolve_host_session_key(args)?;
    let mapped_session_id = host_session_key
        .as_deref()
        .and_then(|key| index.host_sessions.get(key).map(String::as_str));
    let selected_session_id = explicit_session_id.or(mapped_session_id);
    let create_if_missing = args
        .get("create_if_missing")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let resume_completed = args
        .get("resume_completed")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let timestamp = now_timestamp();

    let (
        session_id,
        session_path,
        created,
        resumed,
        previous_status,
        reactivated,
        checkpoint_count,
        parent_session_id,
    ) = if let Some(session_id) = selected_session_id {
        let entry = index.sessions.get(session_id).cloned().ok_or_else(|| {
            session_error(
                "SESSION_NOT_FOUND",
                "The requested Session is not present in the Session index.",
                "not_found",
                false,
                json!({"session_id": session_id}),
            )
        })?;
        let content =
            fs::read_to_string(ctx.workspace.root().join(&entry.path)).map_err(|error| {
                session_error(
                    "SESSION_READ_FAILED",
                    &error.to_string(),
                    "filesystem",
                    true,
                    json!({"path": entry.path}),
                )
            })?;
        let previous_status = normalized_status(markdown::metadata(&content, "Status").as_deref());
        if previous_status == "completed" && !resume_completed {
            if !create_if_missing {
                return Err(session_error(
                    "SESSION_COMPLETED_IMMUTABLE",
                    "The requested Session is completed and immutable. Start a continuation Session or explicitly set resume_completed=true.",
                    "conflict",
                    false,
                    json!({
                        "session_id": session_id,
                        "session_path": entry.path,
                        "resume_completed": false
                    }),
                ));
            }
            if index.sessions.len() >= storage::MAX_SESSION_DOCUMENTS {
                return Err(session_error(
                    "SESSION_CAPACITY_EXCEEDED",
                    "Session store contains the maximum number of Session documents.",
                    "validation",
                    false,
                    json!({"max_documents": storage::MAX_SESSION_DOCUMENTS}),
                ));
            }
            let parent_session_id = session_id.to_string();
            let child_session_id = storage::new_session_id();
            let title = args
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or(entry.title.as_str());
            validate_bounded_text("title", title, MAX_SESSION_TITLE_CHARS)?;
            let relative_path = format!(
                "{}/{child_session_id}.md",
                session_dir_display(ctx, &session_dir)
            );
            let child_content = markdown::render_document(
                markdown::DocumentMetadata {
                    session_id: &child_session_id,
                    title,
                    host_session_key: host_session_key.as_deref(),
                    parent_session_id: Some(parent_session_id.as_str()),
                    created_at: &timestamp,
                    updated_at: &timestamp,
                    status: "active",
                },
                &[],
            );
            storage::write_markdown(
                &session_dir.join(format!("{child_session_id}.md")),
                &child_content,
            )?;
            index.sessions.insert(
                child_session_id.clone(),
                model::IndexEntry {
                    path: relative_path.clone(),
                    title: title.trim().to_string(),
                    status: "active".into(),
                    created_at: timestamp.clone(),
                    updated_at: timestamp.clone(),
                    parent_session_id: Some(parent_session_id.clone()),
                },
            );
            if let Some(host_session_key) = host_session_key.as_ref() {
                index
                    .host_sessions
                    .insert(host_session_key.clone(), child_session_id.clone());
            }
            storage::write_index(&session_dir, &index)?;
            (
                child_session_id,
                relative_path,
                true,
                false,
                previous_status.to_string(),
                false,
                0,
                Some(parent_session_id),
            )
        } else {
            let reactivated = previous_status != "active";
            if reactivated {
                let updated = markdown::update_document_lifecycle(&content, &timestamp, "active");
                storage::write_markdown(
                    ctx.workspace.root().join(&entry.path).as_path(),
                    &updated,
                )?;
                if let Some(index_entry) = index.sessions.get_mut(session_id) {
                    index_entry.status = "active".into();
                    index_entry.updated_at = timestamp.clone();
                }
                storage::write_index(&session_dir, &index)?;
            }
            (
                session_id.to_string(),
                entry.path,
                false,
                true,
                previous_status.to_string(),
                reactivated,
                markdown::parse_checkpoint_records(&content).len(),
                entry.parent_session_id,
            )
        }
    } else {
        if !create_if_missing {
            return Err(session_error(
                "SESSION_NOT_FOUND",
                "No Session mapping exists for this conversation.",
                "not_found",
                false,
                json!({}),
            ));
        }
        if index.sessions.len() >= storage::MAX_SESSION_DOCUMENTS {
            return Err(session_error(
                "SESSION_CAPACITY_EXCEEDED",
                "Session store contains the maximum number of Session documents.",
                "validation",
                false,
                json!({"max_documents": storage::MAX_SESSION_DOCUMENTS}),
            ));
        }
        let session_id = storage::new_session_id();
        let title = args
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("开发会话");
        validate_bounded_text("title", title, MAX_SESSION_TITLE_CHARS)?;
        let relative_path = format!("{}/{session_id}.md", session_dir_display(ctx, &session_dir));
        let content = markdown::render_document(
            markdown::DocumentMetadata {
                session_id: &session_id,
                title,
                host_session_key: host_session_key.as_deref(),
                parent_session_id: None,
                created_at: &timestamp,
                updated_at: &timestamp,
                status: "active",
            },
            &[],
        );
        storage::write_markdown(&session_dir.join(format!("{session_id}.md")), &content)?;
        index.sessions.insert(
            session_id.clone(),
            model::IndexEntry {
                path: relative_path.clone(),
                title: title.trim().to_string(),
                status: "active".into(),
                created_at: timestamp.clone(),
                updated_at: timestamp.clone(),
                parent_session_id: None,
            },
        );
        if let Some(host_session_key) = host_session_key.as_ref() {
            index
                .host_sessions
                .insert(host_session_key.clone(), session_id.clone());
        }
        storage::write_index(&session_dir, &index)?;
        (
            session_id,
            relative_path,
            true,
            false,
            "active".to_string(),
            false,
            0,
            None,
        )
    };

    Ok(tool_ok(json!({
        "session_id": session_id,
        "session_path": session_path,
        "created": created,
        "resumed": resumed,
        "session_status": "active",
        "previous_status": previous_status,
        "reactivated": reactivated,
        "parent_session_id": parent_session_id.clone(),
        "continuation_created": created && parent_session_id.is_some(),
        "checkpoint_count": checkpoint_count,
        "automatic_history_loading": false,
        "history_injected": false,
        "archive_access": {
            "index_path": format!("{}/index.json", session_dir_display(ctx, &session_dir)),
            "legacy_path": storage::LEGACY_SESSION_DIR,
            "legacy_migration_performed": false,
            "read_method": "Use session operation=list to discover metadata and operation=get to read one explicit Session. Use read_file only for the frozen legacy archive when the user explicitly requests legacy history."
        },
        "checkpoint_policy": {
            "tool": "session",
            "operation": "checkpoint",
            "session_id": session_id,
            "expected_path": session_path,
            "required_before_final_response": true,
            "automatic_milestone_persistence": true
        },
        "persistence": persistence_details(ctx, &session_path),
        "warnings": []
    })))
}

fn auto_checkpoint_identity(args: &Value, output: &Value) -> String {
    if let Some(commit_sha) = output.get("commit_sha").and_then(Value::as_str) {
        return format!("commit:{commit_sha}");
    }
    if let Some(verification_key) = args.get("verification_key").and_then(Value::as_str) {
        return format!("verification:{verification_key}");
    }
    let test_file = args.get("test_file").and_then(Value::as_str);
    let test_name = args.get("test_name").and_then(Value::as_str);
    if test_file.is_some() || test_name.is_some() {
        return format!(
            "verification:{}:{}",
            test_file.unwrap_or_default(),
            test_name.unwrap_or_default()
        );
    }
    if let Some(kind) = args.get("verification_kind").and_then(Value::as_str) {
        return format!("verification-kind:{kind}");
    }
    if let Some(verification) = output.get("verification") {
        for key in ["verification_key", "id", "kind"] {
            if let Some(value) = verification.get(key).and_then(Value::as_str) {
                return format!("verification:{key}:{value}");
            }
        }
    }
    "progress".to_string()
}

pub fn list(ctx: &ToolContext, args: &Value) -> WorkspaceResult<Value> {
    let session_dir = resolve_dir(ctx, args)?;
    let Some(index) = storage::read_index(&session_dir)? else {
        return Ok(tool_ok(json!({
            "sessions": [],
            "cursor": 0,
            "next_cursor": Value::Null,
            "total": 0,
            "legacy_path": storage::LEGACY_SESSION_DIR,
            "legacy_included": false
        })));
    };
    let cursor = args.get("cursor").and_then(Value::as_u64).unwrap_or(0) as usize;
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_LIST_LIMIT as u64)
        .clamp(1, MAX_LIST_LIMIT as u64) as usize;
    let mut entries = index
        .sessions
        .iter()
        .map(|(session_id, entry)| {
            json!({
                "session_id": session_id,
                "path": entry.path,
                "title": entry.title,
                "status": entry.status,
                "created_at": entry.created_at,
                "updated_at": entry.updated_at,
                "parent_session_id": entry.parent_session_id
            })
        })
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| {
        right["updated_at"]
            .as_str()
            .cmp(&left["updated_at"].as_str())
            .then(
                right["session_id"]
                    .as_str()
                    .cmp(&left["session_id"].as_str()),
            )
    });
    let total = entries.len();
    let page = entries
        .into_iter()
        .skip(cursor)
        .take(limit)
        .collect::<Vec<_>>();
    let next_cursor = (cursor + page.len() < total).then_some(cursor + page.len());
    Ok(tool_ok(json!({
        "sessions": page,
        "cursor": cursor,
        "next_cursor": next_cursor,
        "total": total,
        "legacy_path": storage::LEGACY_SESSION_DIR,
        "legacy_included": false
    })))
}

pub fn get(ctx: &ToolContext, args: &Value) -> WorkspaceResult<Value> {
    let session_id = required_session_id(args)?;
    let session_dir = resolve_dir(ctx, args)?;
    let index = storage::read_index(&session_dir)?.ok_or_else(session_not_opened)?;
    let entry = index.sessions.get(&session_id).ok_or_else(|| {
        session_error(
            "SESSION_NOT_FOUND",
            "The requested Session is not present in the Session index.",
            "not_found",
            false,
            json!({"session_id": session_id}),
        )
    })?;
    let content = fs::read_to_string(ctx.workspace.root().join(&entry.path)).map_err(|error| {
        session_error(
            "SESSION_READ_FAILED",
            &error.to_string(),
            "filesystem",
            true,
            json!({"path": entry.path}),
        )
    })?;
    let max_bytes = args
        .get("max_bytes")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_GET_BYTES as u64)
        .clamp(1, MAX_GET_BYTES as u64) as usize;
    let records = markdown::parse_checkpoint_records(&content);
    let latest_checkpoint = records.last().cloned();
    let (content, content_truncated) = truncate_utf8_bytes(&content, max_bytes);
    Ok(tool_ok(json!({
        "session_id": session_id,
        "path": entry.path,
        "title": entry.title,
        "status": entry.status,
        "created_at": entry.created_at,
        "updated_at": entry.updated_at,
        "parent_session_id": entry.parent_session_id,
        "checkpoint_count": records.len(),
        "snapshot": latest_checkpoint,
        "content": content,
        "content_truncated": content_truncated,
        "max_bytes": max_bytes
    })))
}

fn required_checkpoint_argument(args: &Value, name: &str) -> WorkspaceResult<String> {
    let value = args
        .get(name)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            session_error(
                "CHECKPOINT_TARGET_REQUIRED",
                "Pass session_id and expected_path exactly as returned by session operation=open.",
                "validation",
                false,
                json!({"missing_argument": name}),
            )
        })?;
    let max_chars = if name == "session_id" {
        MAX_SESSION_ID_CHARS
    } else {
        MAX_EXPECTED_PATH_CHARS
    };
    validate_bounded_text(name, &value, max_chars)?;
    if name == "session_id" {
        validate_session_id(&value)?;
    }
    Ok(value)
}

pub fn checkpoint(
    ctx: &ToolContext,
    args: &Value,
    mcp_session_id: Option<&str>,
) -> WorkspaceResult<Value> {
    let session_id = required_checkpoint_argument(args, "session_id")?;
    let expected_path = required_checkpoint_argument(args, "expected_path")?;
    let session_dir = resolve_dir(ctx, args)?;
    if !session_dir.exists() {
        return Err(session_not_opened());
    }
    let _lock = storage::lock_directory(&session_dir)?;
    let mut index = storage::read_index(&session_dir)?.ok_or_else(session_not_opened)?;
    let entry = index
        .sessions
        .get(&session_id)
        .cloned()
        .ok_or_else(session_not_opened)?;
    if entry.path != expected_path {
        return Err(session_error(
            "SESSION_TARGET_MISMATCH",
            "The checkpoint target does not match the Session opened by Anchor.",
            "validation",
            false,
            json!({
                "expected_path": expected_path,
                "resolved_path": entry.path,
                "session_id": session_id
            }),
        ));
    }
    let document_path = ctx.workspace.root().join(&entry.path);
    let document_content = fs::read_to_string(&document_path).map_err(|error| {
        session_error(
            "SESSION_READ_FAILED",
            &error.to_string(),
            "filesystem",
            true,
            json!({"path": entry.path}),
        )
    })?;

    let timestamp = now_timestamp();
    let mut record = markdown::checkpoint_from_args(args, &timestamp)
        .map_err(WorkspaceError::invalid_argument)?;
    let redacted = markdown::redact_record(&mut record);
    markdown::ensure_turn_id(&mut record);
    let previous_status =
        normalized_status(markdown::metadata(&document_content, "Status").as_deref()).to_string();
    let session_status = args
        .get("session_status")
        .and_then(Value::as_str)
        .map(markdown::validate_session_status)
        .transpose()
        .map_err(WorkspaceError::invalid_argument)?
        .unwrap_or(previous_status.as_str())
        .to_string();
    if session_status == "completed" {
        let tasks = ctx.harness.list_tasks().map_err(|error| {
            let message = error.to_string();
            session_error(
                "SESSION_TASK_STATE_UNAVAILABLE",
                &message,
                "internal",
                true,
                json!({}),
            )
        })?;
        let active_bound_tasks = tasks
            .into_iter()
            .filter(|task| {
                task.status.is_writable()
                    && task.session_id.as_deref() == Some(session_id.as_str())
                    && task.session_path.as_deref() == Some(expected_path.as_str())
            })
            .map(|task| task.id)
            .collect::<Vec<_>>();
        if !active_bound_tasks.is_empty() {
            return Err(session_error(
                "SESSION_TASK_STILL_ACTIVE",
                "Session 绑定的 Harness Task 尚未关闭；普通 checkpoint 不能代替任务完成。",
                "validation",
                true,
                json!({
                    "task_ids": active_bound_tasks,
                    "suggestion": "使用 close_work_session 完成验证、关闭任务并写入最终 checkpoint"
                }),
            ));
        }
    }
    if let Some(owner_scope) = ctx.command_owner_scope_for_session(mcp_session_id) {
        let (running_sessions, unobserved_terminal_sessions) =
            ctx.sessions.pending_for_owner(&owner_scope, 2_048);
        if !running_sessions.is_empty() || !unobserved_terminal_sessions.is_empty() {
            return Err(session_error(
                "SESSION_COMMAND_RESULTS_PENDING",
                "当前会话仍有运行中或终态尚未消费的 retained command；写入显式 checkpoint 前必须先检查结果。",
                "validation",
                true,
                json!({
                    "running_sessions": running_sessions,
                    "unobserved_terminal_sessions": unobserved_terminal_sessions,
                    "suggestion": "调用 list_command_sessions 定位会话；对每个会话调用 wait_command，或使用 kill_session 终止后消费结果"
                }),
            ));
        }
    }
    let status_changed = session_status != previous_status;
    let mut records = markdown::parse_checkpoint_records(&document_content);
    let mut duplicate_ignored = false;
    let mut updated = false;
    if let Some(existing) = records
        .iter_mut()
        .find(|existing| existing.turn_id == record.turn_id)
    {
        if record.timestamp.is_empty() {
            record.timestamp = existing.timestamp.clone();
        }
        markdown::validate_checkpoint_record(&record).map_err(WorkspaceError::invalid_argument)?;
        if existing == &record {
            duplicate_ignored = !status_changed;
        } else {
            *existing = record.clone();
            updated = true;
        }
    } else {
        if record.timestamp.is_empty() {
            record.timestamp = timestamp.clone();
        }
        markdown::validate_checkpoint_record(&record).map_err(WorkspaceError::invalid_argument)?;
        records.push(record.clone());
        updated = true;
    }
    updated |= status_changed;
    let raw_checkpoint_count = records.len();
    let (records, compaction) = markdown::compact_checkpoint_records(records);
    updated |= compaction.removed > 0;

    let final_content = if !updated {
        document_content.clone()
    } else {
        let created_at =
            markdown::metadata(&document_content, "Created").unwrap_or_else(|| timestamp.clone());
        let title = markdown::document_title(&document_content);
        let host_session_key = markdown::metadata(&document_content, "Host session key");
        markdown::render_document(
            markdown::DocumentMetadata {
                session_id: &session_id,
                title: &title,
                host_session_key: host_session_key.as_deref(),
                parent_session_id: entry.parent_session_id.as_deref(),
                created_at: &created_at,
                updated_at: &timestamp,
                status: &session_status,
            },
            &records,
        )
    };
    if updated {
        let current_store_bytes = indexed_store_bytes(ctx, &index);
        let previous_document_bytes = fs::metadata(&document_path)
            .map(|value| value.len())
            .unwrap_or(0);
        storage::ensure_session_store_capacity(
            current_store_bytes,
            previous_document_bytes,
            final_content.len() as u64,
        )?;
        storage::write_markdown(&document_path, &final_content)?;
        if let Some(entry) = index.sessions.get_mut(&session_id) {
            entry.title = markdown::document_title(&final_content);
            entry.status = session_status.clone();
            entry.updated_at = timestamp.clone();
        }
        storage::write_index(&session_dir, &index)?;
    }
    let mut warnings = Vec::new();
    if redacted {
        warnings.push("检测到疑似敏感信息，归档内容已脱敏。");
    }
    let content_bytes = final_content.len() as u64;
    if content_bytes > storage::MAX_SESSION_FILE_BYTES * 3 / 4 {
        warnings.push("当前 Session 文件已超过容量上限的 75%，建议减少后续 checkpoint 内容或开始新的 Session。");
    }
    let persistence = persistence_details(ctx, &entry.path);
    Ok(tool_ok(json!({
        "session_id": session_id,
        "path": entry.path,
        "expected_path": expected_path,
        "target_preserved": true,
        "turn_id": record.turn_id,
        "session_status": session_status,
        "previous_status": previous_status,
        "status_changed": status_changed,
        "checkpoint_count": records.len(),
        "raw_checkpoint_count": raw_checkpoint_count,
        "compacted_checkpoint_count": compaction.removed,
        "checkpoint_compaction": {
            "superseded_closed_task_auto": compaction.superseded_closed_task_auto,
            "coalesced_active_auto": compaction.coalesced_active_auto
        },
        "content_bytes": content_bytes,
        "max_content_bytes": storage::MAX_SESSION_FILE_BYTES,
        "created": false,
        "updated": updated,
        "duplicate_ignored": duplicate_ignored,
        "content_hash": storage::sha256(final_content.as_bytes()),
        "storage": persistence["storage"],
        "git_tracked": persistence["git_tracked"],
        "git_ignored": persistence["git_ignored"],
        "git_dirty_after_write": persistence["git_dirty_after_write"],
        "persistence_reason": persistence["reason"],
        "warnings": warnings
    })))
}

pub fn auto_checkpoint_after_tool(
    ctx: &ToolContext,
    tool_name: &str,
    args: &Value,
    output: &Value,
    task_id: Option<&str>,
) -> WorkspaceResult<Option<Value>> {
    if !is_auto_checkpoint_tool(tool_name, args, output) {
        return Ok(None);
    }
    let task = task_id
        .and_then(|task_id| ctx.harness.task(task_id).ok())
        .or_else(|| ctx.harness.current_task().ok().flatten());
    let Some(task) = task else {
        return Ok(None);
    };
    let (Some(session_id), Some(expected_path)) =
        (task.session_id.as_deref(), task.session_path.as_deref())
    else {
        return Ok(None);
    };
    let structured = output.get("structuredContent").unwrap_or(output);
    let identity = auto_checkpoint_identity(args, structured);
    let turn_seed = format!("{}:{tool_name}:{identity}", task.id);
    let turn_hash = storage::sha256(turn_seed.as_bytes());
    let turn_id = format!(
        "auto-{}-{}",
        sanitize_checkpoint_segment(tool_name),
        &turn_hash[..16]
    );
    let success = structured.get("ok").and_then(Value::as_bool) == Some(true);
    let mut findings = vec![format!(
        "自动阶段检查点：tool={tool_name}, status={}, success={success}",
        structured
            .get("execution_status")
            .or_else(|| structured.get("status"))
            .and_then(Value::as_str)
            .unwrap_or("completed")
    )];
    if let Some(summary) = structured.get("summary").and_then(Value::as_str) {
        findings.push(format!(
            "summary={}",
            bounded_checkpoint_text(summary, 1_000)
        ));
    }
    if let Some(command) = structured.get("command").and_then(Value::as_str) {
        findings.push(format!(
            "command={}",
            bounded_checkpoint_text(command, 1_000)
        ));
    }
    let mut files_changed = structured
        .get("affected_files")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| {
            item.get("path")
                .and_then(Value::as_str)
                .or_else(|| item.as_str())
        })
        .take(markdown::MAX_ARRAY_ITEMS)
        .map(str::to_string)
        .collect::<Vec<_>>();
    for path in structured
        .get("workspace_artifacts")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| item.get("workspace_path").and_then(Value::as_str))
    {
        if files_changed.len() >= markdown::MAX_ARRAY_ITEMS {
            break;
        }
        if !files_changed.iter().any(|existing| existing == path) {
            files_changed.push(path.to_string());
        }
    }
    let scoped_harness = task.git_worktree.as_ref().and_then(|worktree| {
        ctx.harness
            .with_workspace_root(std::path::PathBuf::from(&worktree.path))
            .ok()
    });
    let harness = scoped_harness
        .as_ref()
        .unwrap_or(&ctx.harness)
        .status_for_task(Some(&task.id))
        .ok();
    let mut runtime_state = vec![
        format!("task_id={}", task.id),
        format!("task_status={:?}", task.status).to_ascii_lowercase(),
        format!("tool={tool_name}"),
    ];
    for key in [
        "session_id",
        "execution_status",
        "exit_code",
        "last_output_at",
        "browser_session_id",
        "connection_status",
        "selected_page",
        "commit_sha",
    ] {
        if let Some(value) = structured.get(key).filter(|value| !value.is_null()) {
            runtime_state.push(format!(
                "{key}={}",
                bounded_checkpoint_text(&value.to_string(), 1_000)
            ));
        }
    }
    if let Some(status) = harness.as_ref() {
        runtime_state.push(format!(
            "branch={}",
            status.branch.as_deref().unwrap_or("unknown")
        ));
        runtime_state.push(format!(
            "head={}",
            status.head.as_deref().unwrap_or("unknown")
        ));
        runtime_state.push(format!("baseline_matches={:?}", status.baseline_matches));
    }
    let verification = structured.get("verification");
    let tests = args
        .get("verification_kind")
        .and_then(Value::as_str)
        .or_else(|| {
            verification
                .and_then(|value| value.get("kind"))
                .and_then(Value::as_str)
        })
        .map(|kind| {
            let verification_succeeded = verification
                .and_then(|value| value.get("status"))
                .and_then(Value::as_str)
                .is_some_and(|status| status == "passed")
                || verification
                    .and_then(|value| value.get("success"))
                    .and_then(Value::as_bool)
                    == Some(true)
                || structured
                    .get("success")
                    .or_else(|| structured.get("command_ok"))
                    .and_then(Value::as_bool)
                    .unwrap_or(success);
            vec![format!(
                "verification_kind={kind}, success={}",
                verification_succeeded
            )]
        })
        .unwrap_or_default();
    let remaining_issues = if success {
        Vec::new()
    } else {
        vec![format!(
            "{}: {}",
            structured
                .get("error")
                .and_then(|error| error.get("code"))
                .and_then(Value::as_str)
                .unwrap_or("TOOL_FAILED"),
            bounded_checkpoint_text(
                structured
                    .get("error")
                    .and_then(|error| error.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or("阶段执行失败"),
                1_000,
            )
        )]
    };
    let checkpoint_args = json!({
        "session_id": session_id,
        "expected_path": expected_path,
        "turn_id": turn_id,
        "timestamp": now_timestamp(),
        "user_intent": task.objective,
        "findings": findings,
        "files_changed": files_changed,
        "tests": tests,
        "runtime_state": runtime_state,
        "remaining_issues": remaining_issues,
        "next_actions": task.pending_steps,
        "session_status": "active",
        "notes": "Anchor 自动保存的结构化阶段检查点；相同阶段身份会幂等更新。"
    });
    checkpoint(ctx, &checkpoint_args, None).map(Some)
}

pub(crate) fn checkpoint_reference(checkpoint: &Value) -> Value {
    json!({
        "saved": true,
        "path": checkpoint.get("path").cloned().unwrap_or(Value::Null),
        "turn_id": checkpoint.get("turn_id").cloned().unwrap_or(Value::Null)
    })
}

fn is_auto_checkpoint_tool(tool_name: &str, args: &Value, output: &Value) -> bool {
    let structured = output.get("structuredContent").unwrap_or(output);
    match tool_name {
        "apply_patch" => tool_succeeded(structured) && output_has_workspace_changes(structured),
        "git_commit" => tool_succeeded(structured),
        "stage_commit" | "wait_stage_commit" => {
            command_stage_is_terminal(structured)
                && tool_succeeded(structured)
                && structured
                    .get("commit_sha")
                    .is_some_and(|value| !value.is_null())
        }
        "exec_command" => {
            command_stage_is_terminal(structured)
                && (output_has_workspace_changes(structured)
                    || blocking_verification_requested(args))
        }
        "wait_command" | "write_stdin" => {
            command_stage_is_terminal(structured)
                && (output_has_workspace_changes(structured)
                    || output_blocking_verification_present(structured))
        }
        _ => false,
    }
}

fn tool_succeeded(output: &Value) -> bool {
    output
        .get("success")
        .or_else(|| output.get("command_ok"))
        .or_else(|| output.get("ok"))
        .and_then(Value::as_bool)
        == Some(true)
}

fn blocking_verification_requested(args: &Value) -> bool {
    let has_verification = args.get("verification_kind").is_some();
    let blocking = matches!(
        args.get("verification_level").and_then(Value::as_str),
        None | Some("blocking") | Some("required")
    );
    has_verification && blocking
}

fn output_blocking_verification_present(output: &Value) -> bool {
    let Some(verification) = output.get("verification") else {
        return false;
    };
    matches!(
        verification.get("level").and_then(Value::as_str),
        None | Some("blocking") | Some("required")
    )
}

fn command_stage_is_terminal(output: &Value) -> bool {
    output.get("execution_status").and_then(Value::as_str) != Some("running")
        && output.get("status").and_then(Value::as_str) != Some("running")
        && output.get("termination_reason").and_then(Value::as_str) != Some("running")
}

fn output_has_workspace_changes(output: &Value) -> bool {
    output.get("mutation_attributed").and_then(Value::as_bool) == Some(true)
        || output
            .get("affected_files")
            .and_then(Value::as_array)
            .is_some_and(|files| !files.is_empty())
}

fn sanitize_checkpoint_segment(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '-'
            }
        })
        .take(48)
        .collect()
}

fn bounded_checkpoint_text(value: &str, maximum: usize) -> String {
    if value.chars().count() <= maximum {
        return value.to_string();
    }
    value.chars().take(maximum).collect::<String>() + "…"
}

fn persistence_details(ctx: &ToolContext, relative_path: &str) -> Value {
    let root = ctx.workspace.root();
    let git_tracked = git_path_command(root, &["ls-files", "--error-unmatch", "--", relative_path])
        .is_some_and(|output| output.status.success());
    let git_ignored = git_path_command(root, &["check-ignore", "-q", "--", relative_path])
        .is_some_and(|output| output.status.success());
    let git_dirty_after_write = git_path_command(
        root,
        &[
            "status",
            "--porcelain",
            "--untracked-files=all",
            "--",
            relative_path,
        ],
    )
    .is_some_and(|output| output.status.success() && !output.stdout.is_empty());
    let reason = if git_tracked && git_dirty_after_write {
        "checkpoint updated a Git-tracked workspace file"
    } else if git_tracked {
        "checkpoint target is Git-tracked and its content is unchanged relative to the index"
    } else if git_ignored {
        "checkpoint is stored as an ignored workspace metadata file outside business Git changes"
    } else if git_dirty_after_write {
        "checkpoint is stored as an untracked workspace file"
    } else {
        "checkpoint is stored in the workspace; Git status did not report a pending change"
    };
    json!({
        "storage": "workspace_file",
        "path": relative_path,
        "git_tracked": git_tracked,
        "git_ignored": git_ignored,
        "git_dirty_after_write": git_dirty_after_write,
        "reason": reason
    })
}

fn git_path_command(root: &std::path::Path, args: &[&str]) -> Option<std::process::Output> {
    let mut command = Command::new("git");
    crate::platform::hide_std_console(&mut command);
    command.arg("-C").arg(root).args(args).output().ok()
}

pub fn validate(ctx: &ToolContext, args: &Value) -> WorkspaceResult<Value> {
    let session_dir = resolve_dir(ctx, args)?;
    let repair = args.get("repair").and_then(Value::as_bool).unwrap_or(false);
    if repair {
        storage::ensure_directory(&session_dir)?;
    }
    let mut index_status = "missing";
    if session_dir.exists() {
        index_status = match storage::read_index(&session_dir) {
            Ok(Some(_)) => "valid",
            Ok(None) => "missing",
            Err(_) => "invalid",
        };
    }
    let report = storage::scan(&ctx.workspace, &session_dir)?;
    let mut warnings = Vec::<String>::new();
    if !report.duplicate_session_ids.is_empty() {
        warnings.push("存在重复 session_id，相关 Session 不会写入索引。".into());
    }
    if !report.duplicate_host_session_keys.is_empty() {
        warnings.push("存在重复 Host session key，相关宿主映射不会写入索引。".into());
    }
    let mut status_counts = serde_json::Map::new();
    for status in ["active", "paused", "completed", "unknown"] {
        let count = report
            .documents
            .iter()
            .filter(|document| normalized_status(document.status.as_deref()) == status)
            .count();
        status_counts.insert(status.into(), json!(count));
    }
    if status_counts
        .get("unknown")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        > 0
    {
        warnings.push("部分 Session 文件包含未知 Status。".into());
    }
    let total_session_bytes = report
        .documents
        .iter()
        .map(|document| document.size_bytes)
        .sum::<u64>();
    let largest_document_bytes = report
        .documents
        .iter()
        .map(|document| document.size_bytes)
        .max()
        .unwrap_or(0);
    let repaired = if repair {
        let _lock = storage::lock_directory(&session_dir)?;
        let locked_report = storage::scan(&ctx.workspace, &session_dir)?;
        storage::write_index(&session_dir, &storage::rebuild_index(&locked_report))?;
        true
    } else {
        false
    };
    Ok(tool_ok(json!({
        "valid": report.sequence_valid(),
        "duplicate_session_ids": report.duplicate_session_ids,
        "duplicate_host_session_keys": report.duplicate_host_session_keys,
        "invalid_files": report.invalid_files,
        "empty_files": report.empty_files,
        "document_count": report.documents.len(),
        "status_counts": status_counts,
        "total_session_bytes": total_session_bytes,
        "largest_document_bytes": largest_document_bytes,
        "max_document_bytes": storage::MAX_SESSION_FILE_BYTES,
        "max_total_session_bytes": storage::MAX_SESSION_TOTAL_BYTES,
        "max_documents": storage::MAX_SESSION_DOCUMENTS,
        "index_status": index_status,
        "repaired": repaired,
        "legacy_path": storage::LEGACY_SESSION_DIR,
        "legacy_scanned": false,
        "legacy_migration_performed": false,
        "warnings": warnings
    })))
}

fn resolve_dir(ctx: &ToolContext, args: &Value) -> WorkspaceResult<std::path::PathBuf> {
    storage::resolve_session_dir(
        &ctx.workspace,
        args.get("workspace_root").and_then(Value::as_str),
        args.get("session_dir").and_then(Value::as_str),
    )
}

fn resolve_host_session_key(args: &Value) -> WorkspaceResult<Option<String>> {
    let value = args
        .get("_host_session_key")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if let Some(value) = value {
        validate_bounded_text("_host_session_key", value, MAX_HOST_SESSION_KEY_CHARS)?;
        return Ok(Some(value.to_string()));
    }
    Ok(None)
}

fn required_session_id(args: &Value) -> WorkspaceResult<String> {
    let session_id = args
        .get("session_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| WorkspaceError::invalid_argument("session_id is required"))?;
    validate_session_id(session_id)?;
    Ok(session_id.to_string())
}

fn validate_session_id(session_id: &str) -> WorkspaceResult<()> {
    validate_bounded_text("session_id", session_id, MAX_SESSION_ID_CHARS)?;
    if !storage::valid_session_id(session_id) {
        return Err(WorkspaceError::invalid_argument(
            "session_id must be an Anchor opaque Session handle",
        ));
    }
    Ok(())
}

fn session_not_opened() -> WorkspaceError {
    session_error(
        "SESSION_NOT_OPENED",
        "The Session has not been opened or is not present in the Session index.",
        "not_found",
        false,
        json!({}),
    )
}

fn session_error(
    code: &'static str,
    message: &str,
    category: &'static str,
    retryable: bool,
    details: Value,
) -> WorkspaceError {
    WorkspaceError::ToolDetails {
        code,
        message: message.into(),
        category,
        retryable,
        details,
    }
}

fn session_dir_display(ctx: &ToolContext, path: &std::path::Path) -> String {
    crate::tools::workspace::relative_display(ctx.workspace.root(), path)
}

fn indexed_store_bytes(ctx: &ToolContext, index: &model::SessionIndex) -> u64 {
    index
        .sessions
        .values()
        .filter_map(|entry| fs::metadata(ctx.workspace.root().join(&entry.path)).ok())
        .map(|metadata| metadata.len())
        .sum()
}

fn truncate_utf8_bytes(value: &str, max_bytes: usize) -> (String, bool) {
    if value.len() <= max_bytes {
        return (value.to_string(), false);
    }
    let mut end = max_bytes.min(value.len());
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    (value[..end].to_string(), true)
}

fn now_timestamp() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format!("unix:{seconds}")
}

fn normalized_status(status: Option<&str>) -> &'static str {
    match status.map(str::trim) {
        Some("active") => "active",
        Some("paused") => "paused",
        Some("completed") => "completed",
        None | Some("") => "active",
        Some(_) => "unknown",
    }
}

fn validate_bounded_text(name: &str, value: &str, max_chars: usize) -> WorkspaceResult<()> {
    if value.chars().count() > max_chars {
        return Err(WorkspaceError::invalid_argument(format!(
            "{name} cannot exceed {max_chars} characters"
        )));
    }
    if value
        .chars()
        .any(|character| matches!(character, '\0' | '\r' | '\n'))
    {
        return Err(WorkspaceError::invalid_argument(format!(
            "{name} cannot contain control line breaks"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod auto_checkpoint_tests {
    use serde_json::json;

    use super::is_auto_checkpoint_tool;

    #[test]
    fn read_only_terminal_commands_do_not_create_automatic_checkpoints() {
        assert!(!is_auto_checkpoint_tool(
            "exec_command",
            &json!({}),
            &json!({
                "status": "exited",
                "execution_status": "succeeded",
                "session_id": "terminal-session",
                "affected_files": [],
                "mutation_attributed": false
            })
        ));
        assert!(!is_auto_checkpoint_tool(
            "exec_command",
            &json!({}),
            &json!({"status": "running", "execution_status": "running"})
        ));
        assert!(is_auto_checkpoint_tool(
            "exec_command",
            &json!({}),
            &json!({
                "status": "exited",
                "execution_status": "succeeded",
                "affected_files": [{"path": "src/main.rs"}],
                "mutation_attributed": true
            })
        ));
    }

    #[test]
    fn retained_command_polling_only_checkpoints_real_milestones() {
        for tool in ["wait_command", "write_stdin"] {
            assert!(!is_auto_checkpoint_tool(
                tool,
                &json!({}),
                &json!({"status": "running", "termination_reason": "running"})
            ));
            assert!(!is_auto_checkpoint_tool(
                tool,
                &json!({}),
                &json!({"status": "exited", "termination_reason": "exited"})
            ));
            assert!(is_auto_checkpoint_tool(
                tool,
                &json!({}),
                &json!({
                    "status": "exited",
                    "termination_reason": "exited",
                    "affected_files": [{"path": "src/main.rs"}],
                    "mutation_attributed": true
                })
            ));
        }
    }

    #[test]
    fn blocking_verification_failure_is_a_checkpoint_milestone() {
        assert!(is_auto_checkpoint_tool(
            "exec_command",
            &json!({"verification_kind": "test", "verification_level": "blocking"}),
            &json!({
                "status": "exited",
                "execution_status": "failed",
                "success": false,
                "affected_files": [],
                "mutation_attributed": false
            })
        ));
        assert!(!is_auto_checkpoint_tool(
            "exec_command",
            &json!({"verification_kind": "diagnostic", "verification_level": "diagnostic"}),
            &json!({
                "status": "exited",
                "execution_status": "failed",
                "success": false,
                "affected_files": [],
                "mutation_attributed": false
            })
        ));
    }
}
