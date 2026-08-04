mod markdown;
mod model;
mod storage;

use std::collections::HashSet;
use std::fs;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::tools::context::ToolContext;
use crate::tools::workspace::{tool_ok, WorkspaceError, WorkspaceResult};

const MAX_SESSION_KEY_CHARS: usize = 256;
const MAX_SESSION_TITLE_CHARS: usize = 200;
const MAX_EXPECTED_PATH_CHARS: usize = 1024;
const MAX_BOOTSTRAP_SUMMARIES: usize = 64;
const MAX_BOOTSTRAP_SUMMARY_CHARS: usize = 48_000;
const MAX_SINGLE_SUMMARY_CHARS: usize = 3_000;
const MAX_HISTORY_NUMBERS: usize = 256;
const MAX_LATEST_HANDOFF_CHARS: usize = 64_000;

pub fn bootstrap(ctx: &ToolContext, args: &Value) -> WorkspaceResult<Value> {
    let (session_key, source) = resolve_session_key(args)?;
    let history_dir = resolve_dir(ctx, args)?;
    storage::ensure_directory(&history_dir)?;
    let _lock = storage::lock_directory(&history_dir)?;
    let report = storage::scan(&ctx.workspace, &history_dir)?;
    reject_ambiguous_history(&report)?;
    let mut current_archive_bytes = report
        .documents
        .iter()
        .map(|document| document.size_bytes)
        .sum::<u64>();
    if !report.missing_numbers.is_empty() {
        return Err(history_error(
            "HISTORY_SEQUENCE_CONFLICT",
            "History numbering contains gaps; run history_session_validate before creating a session.",
            "validation",
            true,
            json!({"missing_numbers": report.missing_numbers}),
        ));
    }

    let mut warnings = Vec::<String>::new();
    match storage::read_index(&history_dir) {
        Ok(Some(_)) => {}
        Ok(None) => warnings.push("历史索引缺失，已根据 Markdown 重建。".into()),
        Err(_) => warnings.push("历史索引损坏，已根据 Markdown 重建。".into()),
    }
    let readme = history_dir.join("README.md");
    if readme.exists() {
        fs::read_to_string(&readme).map_err(|error| {
            history_error(
                "HISTORY_READ_FAILED",
                &error.to_string(),
                "filesystem",
                true,
                json!({"path": "docs/history-session/README.md"}),
            )
        })?;
    } else {
        warnings.push("docs/history-session/README.md 不存在。".into());
    }

    let existing = report
        .documents
        .iter()
        .find(|document| document.session_key.as_deref() == Some(session_key.as_str()));
    let (current_number, current_path, created, resumed, previous_status, reactivated) =
        if let Some(document) = existing {
            let previous_status = normalized_status(document.status.as_deref());
            let reactivated = previous_status != "active";
            if reactivated {
                let timestamp = now_timestamp();
                let content =
                    markdown::update_document_lifecycle(&document.content, &timestamp, "active");
                storage::ensure_history_archive_capacity(
                    current_archive_bytes,
                    document.size_bytes,
                    content.len() as u64,
                )?;
                storage::write_markdown(
                    &history_dir.join(format!("{}.md", document.number)),
                    &content,
                )?;
            }
            (
                document.number,
                document.path.clone(),
                false,
                true,
                previous_status.to_string(),
                reactivated,
            )
        } else {
            if !args
                .get("create_if_missing")
                .and_then(Value::as_bool)
                .unwrap_or(true)
            {
                return Err(history_error(
                    "SESSION_NOT_BOOTSTRAPPED",
                    "No history mapping exists for this session_key.",
                    "not_found",
                    false,
                    json!({"session_key_source": source}),
                ));
            }
            if report.documents.len() >= storage::MAX_HISTORY_DOCUMENTS {
                return Err(history_error(
                    "HISTORY_CAPACITY_EXCEEDED",
                    "History archive contains the maximum number of session documents.",
                    "validation",
                    false,
                    json!({"max_documents": storage::MAX_HISTORY_DOCUMENTS}),
                ));
            }
            let number = report.latest_number().unwrap_or(0) + 1;
            let relative_path = format!("{}/{number}.md", history_dir_display(ctx, &history_dir));
            let timestamp = now_timestamp();
            let title = args
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or("开发会话");
            validate_bounded_text("title", title, MAX_SESSION_TITLE_CHARS)?;
            let inherited_summary = build_inherited_summary(&report.documents);
            let content = markdown::attach_inherited_summary(
                markdown::render_document(
                    number,
                    title,
                    &session_key,
                    &timestamp,
                    &timestamp,
                    "active",
                    &[],
                ),
                &inherited_summary,
            );
            storage::ensure_history_archive_capacity(
                current_archive_bytes,
                0,
                content.len() as u64,
            )?;
            storage::write_markdown(&history_dir.join(format!("{number}.md")), &content)?;
            (
                number,
                relative_path,
                true,
                false,
                "active".to_string(),
                false,
            )
        };

    let parallel_history_session_keys = ctx
        .harness
        .active_tasks()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|task| task.history_session_key)
        .collect::<HashSet<_>>();
    let paused_previous_sessions = pause_other_active_sessions(
        &history_dir,
        &report.documents,
        current_number,
        &mut current_archive_bytes,
        &parallel_history_session_keys,
    )?;
    if !paused_previous_sessions.is_empty() {
        warnings.push(format!(
            "已自动暂停其他 active History Session：{}",
            paused_previous_sessions
                .iter()
                .map(u64::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    let refreshed = storage::scan(&ctx.workspace, &history_dir)?;
    reject_ambiguous_history(&refreshed)?;
    storage::write_index(&history_dir, &storage::rebuild_index(&refreshed))?;

    let prior = refreshed
        .documents
        .iter()
        .filter(|document| document.number != current_number)
        .collect::<Vec<_>>();
    let history_numbers_total = prior.len();
    let history_numbers_start = prior.len().saturating_sub(MAX_HISTORY_NUMBERS);
    let history_numbers = prior[history_numbers_start..]
        .iter()
        .map(|document| document.number)
        .collect::<Vec<_>>();
    let history_numbers_truncated = history_numbers.len() < history_numbers_total;
    let (session_summaries, history_summaries_omitted, summary_content_truncated) =
        build_bootstrap_summaries(&prior);
    let history_summary_truncated = history_summaries_omitted > 0 || summary_content_truncated;
    if history_summaries_omitted > 0 {
        warnings.push(format!(
            "历史摘要响应仅返回最近 {} 个会话，省略了 {} 个较早会话。",
            session_summaries.len(),
            history_summaries_omitted
        ));
    }
    if history_numbers_truncated {
        warnings.push(format!(
            "history_numbers 仅返回最近 {MAX_HISTORY_NUMBERS} 个编号。"
        ));
    }
    let all_history_summary = session_summaries
        .iter()
        .map(|summary| {
            format!(
                "会话 {}（{}）：{}",
                summary["number"].as_u64().unwrap_or_default(),
                summary["path"].as_str().unwrap_or_default(),
                summary["summary"].as_str().unwrap_or_default()
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let history_summaries_returned = session_summaries.len();
    let latest = prior.iter().max_by_key(|document| document.number).copied();
    let latest_completed = prior
        .iter()
        .rev()
        .find(|document| normalized_status(document.status.as_deref()) == "completed")
        .copied();
    let current_document = refreshed
        .documents
        .iter()
        .find(|document| document.number == current_number);
    let current_has_checkpoints = current_document
        .is_some_and(|document| !markdown::parse_checkpoint_records(&document.content).is_empty());
    let (handoff_document, latest_handoff_source) = if resumed && current_has_checkpoints {
        (current_document, "current_session")
    } else if latest.is_some() {
        (latest, "latest_prior")
    } else {
        (None, "none")
    };
    let (latest_handoff, latest_handoff_truncated) = handoff_document
        .map(|document| bounded_handoff(&document.content, MAX_LATEST_HANDOFF_CHARS))
        .map_or((None, false), |(handoff, truncated)| {
            (Some(handoff), truncated)
        });
    if latest_handoff_truncated {
        warnings.push(format!(
            "latest_handoff 已按 {MAX_LATEST_HANDOFF_CHARS} 字符预算保留头尾内容。"
        ));
    }
    let checkpoint_count = current_document
        .map(|document| markdown::parse_checkpoint_records(&document.content).len())
        .unwrap_or_default();
    let current_status = current_document
        .map(|document| normalized_status(document.status.as_deref()))
        .unwrap_or("active");
    let mut digest = Sha256::new();
    let mut total_bytes = 0_u64;
    for document in &prior {
        digest.update(document.number.to_le_bytes());
        digest.update(document.content.as_bytes());
        total_bytes += document.size_bytes;
    }

    let mut payload = json!({
        "is_new_session": created,
        "session_key": session_key.clone(),
        "session_key_source": source,
        "target_preserved": true,
        "history_numbers": history_numbers,
        "history_count": prior.len(),
        "latest_completed_number": latest_completed.map(|document| document.number),
        "latest_completed_path": latest_completed.map(|document| document.path.clone()),
        "current_number": current_number,
        "current_path": current_path.clone(),
        "created": created,
        "resumed": resumed,
        "sequence_valid": report.sequence_valid(),
        "all_history_summary": all_history_summary,
        "inherited_summary": markdown::inherited_summary(
            refreshed
                .documents
                .iter()
                .find(|document| document.number == current_number)
                .map(|document| document.content.as_str())
                .unwrap_or_default()
        ),
        "session_summaries": session_summaries,
        "latest_handoff": latest_handoff,
        "latest_handoff_source": latest_handoff_source,
        "latest_handoff_session_number": handoff_document.map(|document| document.number),
        "latest_handoff_session_path": handoff_document.map(|document| document.path.clone()),
        "history_read_mode": "bounded_recent_summaries_plus_latest_handoff",
        "total_history_bytes": total_bytes,
        "full_history_included": false,
        "history_digest": format!("{:x}", digest.finalize()),
        "persistence_mode": "hybrid_explicit_and_automatic_milestones",
        "assistant_instructions": "Read all_history_summary, latest_handoff, inherited_summary, and resume_state before continuing the project. These fields are bounded context windows: inspect history_summaries_omitted, history_summary_truncated, and latest_handoff_truncated, and use read_file on the exact archived path only when the current task requires omitted detail. Preserve the session_key and current_path returned by bootstrap, then pass them unchanged as session_key and expected_path to every history_session_checkpoint call. Anchor synchronously writes idempotent automatic checkpoints only for meaningful code changes, successful commits, and blocking verification failures. After completing each user-requested task, still call history_session_checkpoint before the final response. Only state that progress was saved after checkpoint returns ok=true with the same session_key and path.",
        "required_next_actions": [
            "read_all_history_summary",
            "read_latest_handoff",
            "verify_workspace_state",
            "execute_user_task",
            "checkpoint_after_each_completed_task"
        ],
        "checkpoint_policy": {
            "tool": "history_session_checkpoint",
            "session_key": session_key,
            "expected_path": current_path,
            "stable_target_required": true,
            "required_before_final_response": true,
            "applies_after_bootstrap": true,
            "automatic_background_persistence": false,
            "automatic_milestone_persistence": true,
            "automatic_milestones": [
                "code_change",
                "commit",
                "blocking_verification_failure"
            ]
        },
        "persistence": persistence_details(ctx, &current_path),
        "resume_state": build_resume_state(ctx),
        "warnings": warnings
    });
    let object = payload
        .as_object_mut()
        .expect("history bootstrap payload is an object");
    object.insert("history_numbers_total".into(), json!(history_numbers_total));
    object.insert(
        "history_numbers_truncated".into(),
        json!(history_numbers_truncated),
    );
    object.insert(
        "history_summaries_returned".into(),
        json!(history_summaries_returned),
    );
    object.insert(
        "history_summaries_omitted".into(),
        json!(history_summaries_omitted),
    );
    object.insert(
        "history_summary_truncated".into(),
        json!(history_summary_truncated),
    );
    object.insert(
        "latest_prior_number".into(),
        json!(latest.map(|document| document.number)),
    );
    object.insert(
        "latest_prior_path".into(),
        json!(latest.map(|document| document.path.clone())),
    );
    object.insert(
        "latest_prior_status".into(),
        json!(latest.map(|document| normalized_status(document.status.as_deref()))),
    );
    object.insert("session_status".into(), json!(current_status));
    object.insert("previous_status".into(), json!(previous_status));
    object.insert("reactivated".into(), json!(reactivated));
    object.insert(
        "paused_previous_sessions".into(),
        json!(paused_previous_sessions),
    );
    object.insert("checkpoint_count".into(), json!(checkpoint_count));
    object.insert(
        "latest_handoff_truncated".into(),
        json!(latest_handoff_truncated),
    );
    object.insert(
        "latest_handoff_total_bytes".into(),
        json!(handoff_document
            .map(|document| document.size_bytes)
            .unwrap_or(0)),
    );
    Ok(tool_ok(payload))
}

fn pause_other_active_sessions(
    history_dir: &std::path::Path,
    documents: &[model::HistoryDocument],
    current_number: u64,
    archive_bytes: &mut u64,
    preserved_session_keys: &HashSet<String>,
) -> WorkspaceResult<Vec<u64>> {
    let timestamp = now_timestamp();
    let mut paused = Vec::new();
    for document in documents {
        if document.number == current_number
            || normalized_status(document.status.as_deref()) != "active"
            || document
                .session_key
                .as_ref()
                .is_some_and(|session_key| preserved_session_keys.contains(session_key))
        {
            continue;
        }
        let content = markdown::update_document_lifecycle(&document.content, &timestamp, "paused");
        storage::ensure_history_archive_capacity(
            *archive_bytes,
            document.size_bytes,
            content.len() as u64,
        )?;
        storage::write_markdown(
            &history_dir.join(format!("{}.md", document.number)),
            &content,
        )?;
        *archive_bytes = archive_bytes
            .saturating_sub(document.size_bytes)
            .saturating_add(content.len() as u64);
        paused.push(document.number);
    }
    Ok(paused)
}

fn required_checkpoint_argument(args: &Value, name: &str) -> WorkspaceResult<String> {
    let value = args
        .get(name)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            history_error(
                "CHECKPOINT_TARGET_REQUIRED",
                "Pass session_key and expected_path exactly as returned by history_session_bootstrap.",
                "validation",
                false,
                json!({"missing_argument": name}),
            )
        })?;
    let max_chars = if name == "session_key" {
        MAX_SESSION_KEY_CHARS
    } else {
        MAX_EXPECTED_PATH_CHARS
    };
    validate_bounded_text(name, &value, max_chars)?;
    Ok(value)
}

pub fn checkpoint(ctx: &ToolContext, args: &Value) -> WorkspaceResult<Value> {
    let session_key = required_checkpoint_argument(args, "session_key")?;
    let expected_path = required_checkpoint_argument(args, "expected_path")?;
    let history_dir = resolve_dir(ctx, args)?;
    if !history_dir.exists() {
        return Err(session_not_bootstrapped());
    }
    let _lock = storage::lock_directory(&history_dir)?;
    let report = storage::scan(&ctx.workspace, &history_dir)?;
    reject_ambiguous_history(&report)?;
    let document = report
        .documents
        .iter()
        .find(|document| document.session_key.as_deref() == Some(session_key.as_str()))
        .ok_or_else(session_not_bootstrapped)?;
    if document.path != expected_path {
        return Err(history_error(
            "SESSION_TARGET_MISMATCH",
            "The checkpoint target does not match the session initialized by bootstrap.",
            "validation",
            false,
            json!({
                "expected_path": expected_path,
                "resolved_path": document.path,
                "session_key": session_key
            }),
        ));
    }

    let timestamp = now_timestamp();
    let mut record = markdown::checkpoint_from_args(args, &timestamp)
        .map_err(WorkspaceError::invalid_argument)?;
    let redacted = markdown::redact_record(&mut record);
    markdown::ensure_turn_id(&mut record);
    let previous_status = normalized_status(document.status.as_deref()).to_string();
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
            history_error(
                "HISTORY_TASK_STATE_UNAVAILABLE",
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
                    && task.history_session_key.as_deref() == Some(session_key.as_str())
                    && task.history_session_path.as_deref() == Some(expected_path.as_str())
            })
            .map(|task| task.id)
            .collect::<Vec<_>>();
        if !active_bound_tasks.is_empty() {
            return Err(history_error(
                "HISTORY_TASK_STILL_ACTIVE",
                "History Session 绑定的 Harness Task 尚未关闭；普通 checkpoint 不能代替任务完成。",
                "validation",
                true,
                json!({
                    "task_ids": active_bound_tasks,
                    "suggestion": "使用 close_work_session 完成验证、关闭任务并写入最终 checkpoint"
                }),
            ));
        }
    }
    let status_changed = session_status != previous_status;
    let mut records = markdown::parse_checkpoint_records(&document.content);
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

    let final_content = if duplicate_ignored && !status_changed {
        document.content.clone()
    } else {
        let created_at = document
            .created_at
            .clone()
            .unwrap_or_else(|| timestamp.clone());
        let inherited_summary = markdown::inherited_summary(&document.content);
        markdown::attach_inherited_summary(
            markdown::render_document(
                document.number,
                &markdown::document_title(&document.content, document.number),
                &session_key,
                &created_at,
                &timestamp,
                &session_status,
                &records,
            ),
            inherited_summary.as_deref().unwrap_or_default(),
        )
    };
    if !duplicate_ignored || status_changed {
        let current_archive_bytes = report
            .documents
            .iter()
            .map(|document| document.size_bytes)
            .sum::<u64>();
        storage::ensure_history_archive_capacity(
            current_archive_bytes,
            document.size_bytes,
            final_content.len() as u64,
        )?;
        storage::write_markdown(
            &history_dir.join(format!("{}.md", document.number)),
            &final_content,
        )?;
        let refreshed = storage::scan(&ctx.workspace, &history_dir)?;
        storage::write_index(&history_dir, &storage::rebuild_index(&refreshed))?;
    }
    let mut warnings = Vec::new();
    if redacted {
        warnings.push("检测到疑似敏感信息，归档内容已脱敏。");
    }
    let content_bytes = final_content.len() as u64;
    if content_bytes > storage::MAX_HISTORY_FILE_BYTES * 3 / 4 {
        warnings.push("当前历史会话文件已超过容量上限的 75%，建议减少后续 checkpoint 内容或切换新的显式 session_key。");
    }
    let persistence = persistence_details(ctx, &document.path);
    Ok(tool_ok(json!({
        "session_number": document.number,
        "path": document.path,
        "session_key": session_key,
        "expected_path": expected_path,
        "target_preserved": true,
        "turn_id": record.turn_id,
        "session_status": session_status,
        "previous_status": previous_status,
        "status_changed": status_changed,
        "checkpoint_count": records.len(),
        "content_bytes": content_bytes,
        "max_content_bytes": storage::MAX_HISTORY_FILE_BYTES,
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
    let (Some(session_key), Some(expected_path)) = (
        task.history_session_key.as_deref(),
        task.history_session_path.as_deref(),
    ) else {
        return Ok(None);
    };
    let structured = output.get("structuredContent").unwrap_or(output);
    let identity = structured
        .get("session_id")
        .or_else(|| structured.get("commit_sha"))
        .or_else(|| structured.get("operation_id"))
        .or_else(|| structured.get("trace_id"))
        .and_then(Value::as_str)
        .unwrap_or("stage");
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
    let harness = ctx.harness.status_for_task(Some(&task.id)).ok();
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
    let tests = args
        .get("verification_kind")
        .and_then(Value::as_str)
        .map(|kind| {
            vec![format!(
                "verification_kind={kind}, success={}",
                structured
                    .get("success")
                    .or_else(|| structured.get("command_ok"))
                    .and_then(Value::as_bool)
                    .unwrap_or(success)
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
        "session_key": session_key,
        "expected_path": expected_path,
        "turn_id": turn_id,
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
    checkpoint(ctx, &checkpoint_args).map(Some)
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
                    || blocking_verification_failed(args, structured))
        }
        "wait_command" | "write_stdin" => {
            command_stage_is_terminal(structured)
                && (output_has_workspace_changes(structured)
                    || output_blocking_verification_failed(structured))
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

fn blocking_verification_failed(args: &Value, output: &Value) -> bool {
    let has_verification = args.get("verification_kind").is_some();
    let blocking = matches!(
        args.get("verification_level").and_then(Value::as_str),
        None | Some("blocking") | Some("required")
    );
    has_verification && blocking && !tool_succeeded(output)
}

fn output_blocking_verification_failed(output: &Value) -> bool {
    let Some(verification) = output.get("verification") else {
        return false;
    };
    let blocking = matches!(
        verification.get("level").and_then(Value::as_str),
        None | Some("blocking") | Some("required")
    );
    let passed = verification
        .get("status")
        .and_then(Value::as_str)
        .is_some_and(|status| status == "passed")
        || verification.get("success").and_then(Value::as_bool) == Some(true);
    blocking && !passed
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

fn build_resume_state(ctx: &ToolContext) -> Value {
    let harness_status = ctx
        .harness
        .status()
        .ok()
        .and_then(|status| serde_json::to_value(status).ok())
        .unwrap_or_else(|| json!({"available": false}));
    let task = ctx.harness.current_task().ok().flatten().map(|task| {
        json!({
            "task_id": task.id,
            "workspace_id": task.workspace_id,
            "objective": task.objective,
            "status": task.status,
            "baseline": {
                "branch": task.baseline.branch,
                "head": task.baseline.head,
                "fingerprint": task.baseline.worktree_fingerprint,
                "object_id": task.baseline.object_id,
                "file_count": task.baseline.file_count,
                "captured_at": task.baseline.captured_at
            },
            "expected_state": task.expected_state,
            "completed_steps": task.completed_steps,
            "pending_steps": task.pending_steps,
            "latest_change_id": task.latest_change_id,
            "latest_verification_id": task.latest_verification_id,
            "updated_at": task.updated_at
        })
    });
    let git_status =
        crate::tools::git::git_status(&ctx.workspace, &json!({})).unwrap_or_else(|error| {
            json!({
                "ok": false,
                "error": error.to_error_value()
            })
        });
    let command_sessions = ctx.sessions.list_snapshots(true, 4_096);
    let running_command_sessions = command_sessions
        .iter()
        .filter(|session| session.get("execution_status") == Some(&json!("running")))
        .count();
    let next_actions = task
        .as_ref()
        .and_then(|task| task.get("pending_steps"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(|| {
            harness_status
                .get("next_actions")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
        });
    json!({
        "workspace": ctx.workspace.root_display(),
        "task": task,
        "harness": harness_status,
        "git": git_status,
        "command_sessions": command_sessions,
        "running_command_session_count": running_command_sessions,
        "downstream_mcp": ctx.mcp_proxies.status(),
        "next_actions": next_actions,
        "captured_at": now_timestamp(),
        "command_sessions_process_bound": true
    })
}

fn git_path_command(root: &std::path::Path, args: &[&str]) -> Option<std::process::Output> {
    let mut command = Command::new("git");
    crate::platform::hide_std_console(&mut command);
    command.arg("-C").arg(root).args(args).output().ok()
}

pub fn validate(ctx: &ToolContext, args: &Value) -> WorkspaceResult<Value> {
    let history_dir = resolve_dir(ctx, args)?;
    let repair = args.get("repair").and_then(Value::as_bool).unwrap_or(false);
    if repair {
        storage::ensure_directory(&history_dir)?;
    }
    let mut index_status = "missing";
    if history_dir.exists() {
        index_status = match storage::read_index(&history_dir) {
            Ok(Some(_)) => "valid",
            Ok(None) => "missing",
            Err(_) => "invalid",
        };
    }
    let report = storage::scan(&ctx.workspace, &history_dir)?;
    let mut warnings = Vec::<String>::new();
    if !report.duplicate_session_keys.is_empty() {
        warnings.push("存在重复 session_key，相关映射未写入索引。".into());
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
        warnings.push(
            "部分历史文件包含未知 Status；下次 bootstrap 会将对应会话重新激活为 active。".into(),
        );
    }
    let total_history_bytes = report
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
        let _lock = storage::lock_directory(&history_dir)?;
        let locked_report = storage::scan(&ctx.workspace, &history_dir)?;
        storage::write_index(&history_dir, &storage::rebuild_index(&locked_report))?;
        true
    } else {
        false
    };
    let latest_number = report.latest_number();
    let latest_path = latest_number.and_then(|number| {
        report
            .documents
            .iter()
            .find(|document| document.number == number)
            .map(|document| document.path.clone())
    });
    Ok(tool_ok(json!({
        "sequence_valid": report.sequence_valid(),
        "numbers": report.numbers,
        "missing_numbers": report.missing_numbers,
        "duplicate_session_keys": report.duplicate_session_keys,
        "invalid_files": report.invalid_files,
        "empty_files": report.empty_files,
        "latest_number": latest_number,
        "latest_path": latest_path,
        "document_count": report.documents.len(),
        "status_counts": status_counts,
        "total_history_bytes": total_history_bytes,
        "largest_document_bytes": largest_document_bytes,
        "max_document_bytes": storage::MAX_HISTORY_FILE_BYTES,
        "max_total_history_bytes": storage::MAX_HISTORY_TOTAL_BYTES,
        "max_documents": storage::MAX_HISTORY_DOCUMENTS,
        "index_status": index_status,
        "repaired": repaired,
        "warnings": warnings
    })))
}

fn resolve_dir(ctx: &ToolContext, args: &Value) -> WorkspaceResult<std::path::PathBuf> {
    storage::resolve_history_dir(
        &ctx.workspace,
        args.get("workspace_root").and_then(Value::as_str),
        args.get("history_dir").and_then(Value::as_str),
    )
}

fn resolve_session_key(args: &Value) -> WorkspaceResult<(String, &'static str)> {
    if let Some(value) = args
        .get("session_key")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        validate_bounded_text("session_key", value, MAX_SESSION_KEY_CHARS)?;
        return Ok((value.to_string(), "explicit_session_key"));
    }
    if let Some(value) = args
        .get("_host_session_key")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        validate_bounded_text("_host_session_key", value, MAX_SESSION_KEY_CHARS)?;
        return Ok((value.to_string(), "platform_conversation_id"));
    }
    Err(history_error(
        "SESSION_ID_UNAVAILABLE",
        "A stable ChatGPT session identifier is required.",
        "validation",
        false,
        json!({}),
    ))
}

fn reject_ambiguous_history(report: &model::ScanReport) -> WorkspaceResult<()> {
    if report.duplicate_session_keys.is_empty() {
        return Ok(());
    }
    Err(history_error(
        "HISTORY_INDEX_CONFLICT",
        "Multiple history files declare the same session_key.",
        "validation",
        false,
        json!({"duplicate_session_keys": report.duplicate_session_keys}),
    ))
}

fn session_not_bootstrapped() -> WorkspaceError {
    history_error(
        "SESSION_NOT_BOOTSTRAPPED",
        "The session_key has not been bootstrapped.",
        "not_found",
        false,
        json!({}),
    )
}

fn history_error(
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

fn history_dir_display(ctx: &ToolContext, path: &std::path::Path) -> String {
    crate::tools::workspace::relative_display(ctx.workspace.root(), path)
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

fn build_bootstrap_summaries(documents: &[&model::HistoryDocument]) -> (Vec<Value>, usize, bool) {
    let mut summaries = Vec::new();
    let mut used_chars = 0_usize;
    let mut content_truncated = false;
    for document in documents.iter().rev() {
        if summaries.len() >= MAX_BOOTSTRAP_SUMMARIES {
            break;
        }
        let raw_summary = markdown::summary(&document.content);
        let (summary, truncated) = truncate_chars_with_flag(&raw_summary, MAX_SINGLE_SUMMARY_CHARS);
        let entry_chars = summary.chars().count() + document.path.chars().count() + 96;
        if !summaries.is_empty() && used_chars + entry_chars > MAX_BOOTSTRAP_SUMMARY_CHARS {
            break;
        }
        used_chars += entry_chars;
        content_truncated |= truncated;
        summaries.push(json!({
            "number": document.number,
            "path": document.path,
            "status": normalized_status(document.status.as_deref()),
            "updated_at": document.updated_at,
            "size_bytes": document.size_bytes,
            "summary": summary,
            "summary_truncated": truncated
        }));
    }
    summaries.reverse();
    let omitted = documents.len().saturating_sub(summaries.len());
    (summaries, omitted, content_truncated)
}

fn bounded_handoff(content: &str, max_chars: usize) -> (String, bool) {
    let total_chars = content.chars().count();
    if total_chars <= max_chars {
        return (content.to_string(), false);
    }
    const MARKER: &str = "\n\n…（handoff 中部内容已按响应预算省略）…\n\n";
    let marker_chars = MARKER.chars().count();
    let content_budget = max_chars.saturating_sub(marker_chars);
    let head_chars = content_budget / 2;
    let tail_chars = content_budget.saturating_sub(head_chars);
    let head = content.chars().take(head_chars).collect::<String>();
    let tail = content
        .chars()
        .rev()
        .take(tail_chars)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<String>();
    (format!("{head}{MARKER}{tail}"), true)
}

fn build_inherited_summary(documents: &[model::HistoryDocument]) -> String {
    const MAX_TOTAL_CHARS: usize = 16_000;
    const MAX_SESSION_CHARS: usize = 3_000;

    let mut entries = Vec::new();
    let mut used = 0_usize;
    let mut omitted = 0_usize;
    for document in documents.iter().rev() {
        let compact = truncate_chars(&markdown::summary(&document.content), MAX_SESSION_CHARS);
        let entry = format!(
            "### 会话 {}（{}）\n\n{}",
            document.number, document.path, compact
        );
        let entry_len = entry.chars().count();
        if used + entry_len > MAX_TOTAL_CHARS {
            omitted += 1;
            continue;
        }
        used += entry_len;
        entries.push(entry);
    }
    entries.reverse();
    if omitted > 0 {
        entries.insert(
            0,
            format!("> 另有 {omitted} 个较早会话未展开，可通过 all_history_summary 读取。"),
        );
    }
    entries.join("\n\n")
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    truncate_chars_with_flag(value, max_chars).0
}

fn truncate_chars_with_flag(value: &str, max_chars: usize) -> (String, bool) {
    if value.chars().count() <= max_chars {
        return (value.to_string(), false);
    }
    let suffix = "…（摘要已截断）";
    let keep = max_chars.saturating_sub(suffix.chars().count());
    let mut truncated = value.chars().take(keep).collect::<String>();
    truncated.push_str(suffix);
    (truncated, true)
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
