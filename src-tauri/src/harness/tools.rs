use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::tools::workspace::{tool_ok, WorkspaceError};
use crate::tools::{CancellationToken, ToolContext};

use super::model::{
    HarnessEvent, HarnessSessionStatus, OperationRecord, TaskSession, TaskStatus,
    VerificationRecord, WorkSessionCloseOutbox, WorkSessionClosePhase, SCHEMA_VERSION,
};
use super::store::HarnessError;

const FINISH_RESPONSE_MAX_BYTES: usize = 32 * 1024;
const DEFAULT_SUMMARY_LIMIT: usize = 64;
const DEFAULT_EVENT_LIMIT: usize = 20;

pub const TOOL_NAMES: &[&str] = &[
    "harness_status",
    "operation_log",
    "begin_work_session",
    "close_work_session",
    "update_verification_disposition",
    "project_state",
    "start_task",
    "refresh_baseline",
    "accept_current_baseline",
    "accept_latest_baseline",
    "stage_commit",
    "stage_commit_status",
    "wait_stage_commit",
    "update_task",
    "pause_task",
    "resume_task",
    "switch_task",
    "finish_task",
    "task_context",
    "list_task_events",
    "change_summary",
    "export_work_session",
];

pub fn call(
    ctx: &ToolContext,
    name: &str,
    args: &Value,
    cancellation: &CancellationToken,
    session_id: Option<&str>,
) -> Result<Value, WorkspaceError> {
    let recovered_outboxes = if name == "close_work_session" {
        Vec::new()
    } else {
        recover_close_outboxes(ctx)?
    };
    let value = match name {
        "harness_status" => harness_status(ctx, session_id),
        "operation_log" => operation_log(ctx, args),
        "begin_work_session" => begin_work_session(ctx, args, session_id),
        "close_work_session" => close_work_session(ctx, args),
        "update_verification_disposition" => update_verification_disposition(ctx, args),
        "project_state" => project_state(ctx, args, session_id),
        "start_task" => start_task(ctx, args, session_id),
        "refresh_baseline" => refresh_baseline(ctx, args),
        "accept_current_baseline" => accept_current_baseline(ctx, args),
        "accept_latest_baseline" => accept_latest_baseline(ctx, args),
        "stage_commit" => super::stage_commit::run(ctx, args, cancellation),
        "stage_commit_status" => super::stage_commit::status(ctx, args),
        "wait_stage_commit" => super::stage_commit::wait(ctx, args, cancellation),
        "update_task" => update_task(ctx, args),
        "pause_task" => transition(ctx, args, TaskStatus::Paused),
        "resume_task" => resume_task(ctx, args, session_id),
        "switch_task" => switch_task(ctx, args, session_id),
        "finish_task" => finish_task(ctx, args),
        "task_context" => task_context(ctx, args, session_id),
        "list_task_events" => list_task_events(ctx, args),
        "change_summary" => change_summary(ctx, args, session_id),
        "export_work_session" => export_work_session(ctx, args, session_id),
        _ => return Err(tool_error("INVALID_ARGUMENT", "未知 Harness 工具")),
    }?;
    let mut value = value;
    if !recovered_outboxes.is_empty() {
        if let Some(object) = value.as_object_mut() {
            object.insert("outbox_recovery".into(), json!(recovered_outboxes));
        }
    }
    Ok(tool_ok(value))
}

fn export_work_session(
    ctx: &ToolContext,
    args: &Value,
    session_id: Option<&str>,
) -> Result<Value, WorkspaceError> {
    let task = if let Some(task_id) = args.get("task_id").and_then(Value::as_str) {
        ctx.harness.task(task_id).map_err(map_error)?
    } else {
        ctx.task_for_session(session_id)
            .ok_or_else(|| tool_error("TASK_STATE_REQUIRED", "没有可导出的任务"))?
    };
    let output_path = args
        .get("path")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!(".anchor/handoffs/{}.json", task.id));
    let overwrite = args
        .get("overwrite")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    ctx.workspace.reject_write_symlink(&output_path)?;
    let resolved = ctx.workspace.resolve_for_write(&output_path)?;
    if resolved.existed && !overwrite {
        return Err(WorkspaceError::ToolDetails {
            code: "HANDOFF_EXPORT_EXISTS",
            message: format!("Handoff export already exists: {}", resolved.display),
            category: "conflict",
            retryable: true,
            details: json!({
                "path": resolved.display,
                "suggestion": "Choose a new path or pass overwrite=true"
            }),
        });
    }

    let summary = change_summary(
        ctx,
        &json!({"task_id": task.id, "limit": 1024, "verification_view": "all"}),
        session_id,
    )?;
    let verifications = ctx
        .harness
        .list_verifications(&task.id)
        .map_err(map_error)?;
    let git = crate::tools::git::git_status(
        &ctx.workspace,
        &json!({"path": ".", "include_untracked": true, "refresh_index": true}),
    )?;
    let exported_at_unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .to_string();
    let active_failures = verifications
        .iter()
        .filter(|record| {
            record.status == "failed" && effective_disposition(record) == "active_failure"
        })
        .map(|record| {
            json!({
                "verification_id": record.id,
                "kind": record.kind,
                "command": record.command,
                "level": record.level,
                "status": record.status
            })
        })
        .collect::<Vec<_>>();
    let document = json!({
        "format": "anchor.work-session-handoff",
        "schema_version": 1,
        "plugin": {
            "name": "anchor",
            "version": env!("CARGO_PKG_VERSION"),
            "catalog_version": crate::tools::registry::CATALOG_VERSION
        },
        "exported_at_unix_ms": exported_at_unix_ms,
        "workspace": {
            "path": ctx.workspace.root().display().to_string(),
            "workspace_id": task.workspace_id,
            "git": git
        },
        "history_session": {
            "session_key": task.history_session_key,
            "path": task.history_session_path
        },
        "task": task_view(&task),
        "commits": summary.get("commits").cloned().unwrap_or_else(|| json!([])),
        "change_summary": summary,
        "verifications": verifications,
        "remaining_issues": active_failures,
        "next_actions": task.pending_steps,
        "resume": {
            "strategy": "begin_work_session",
            "objective": task.objective,
            "note": "Import this JSON as source evidence, then create a fresh local History Session and Harness Task instead of injecting private storage files."
        }
    });
    let encoded =
        serde_json::to_vec_pretty(&document).map_err(|error| WorkspaceError::ToolDetails {
            code: "HANDOFF_EXPORT_SERIALIZATION_FAILED",
            message: error.to_string(),
            category: "internal",
            retryable: false,
            details: json!({}),
        })?;
    const MAX_HANDOFF_EXPORT_BYTES: usize = 8 * 1024 * 1024;
    if encoded.len() > MAX_HANDOFF_EXPORT_BYTES {
        return Err(WorkspaceError::ToolDetails {
            code: "HANDOFF_EXPORT_TOO_LARGE",
            message: "The work-session handoff exceeds the 8 MiB export limit.".into(),
            category: "validation",
            retryable: true,
            details: json!({
                "content_bytes": encoded.len(),
                "max_content_bytes": MAX_HANDOFF_EXPORT_BYTES,
                "suggestion": "Dispose or supersede obsolete verification evidence before exporting"
            }),
        });
    }
    if let Some(parent) = resolved.path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| handoff_write_error(&resolved.display, error))?;
    }
    let temp_name = format!(
        ".{}.{}.tmp",
        resolved
            .path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("handoff"),
        exported_at_unix_ms
    );
    let temp_path = resolved.path.with_file_name(temp_name);
    fs::write(&temp_path, &encoded)
        .map_err(|error| handoff_write_error(&resolved.display, error))?;
    if resolved.existed {
        fs::remove_file(&resolved.path)
            .map_err(|error| handoff_write_error(&resolved.display, error))?;
    }
    fs::rename(&temp_path, &resolved.path)
        .map_err(|error| handoff_write_error(&resolved.display, error))?;
    let content_hash = format!("{:x}", Sha256::digest(&encoded));
    Ok(json!({
        "format": "anchor.work-session-handoff",
        "schema_version": 1,
        "path": resolved.display,
        "task_id": task.id,
        "content_bytes": encoded.len(),
        "content_hash": content_hash,
        "git_ignored_recommended": output_path.starts_with(".anchor/handoffs/"),
        "resume_strategy": "begin_work_session",
        "warnings": []
    }))
}

fn handoff_write_error(path: &str, error: std::io::Error) -> WorkspaceError {
    WorkspaceError::ToolDetails {
        code: "HANDOFF_EXPORT_WRITE_FAILED",
        message: format!("Failed to write handoff export {path}: {error}"),
        category: "runtime",
        retryable: true,
        details: json!({"path": path}),
    }
}

fn resume_task(
    ctx: &ToolContext,
    args: &Value,
    session_id: Option<&str>,
) -> Result<Value, WorkspaceError> {
    let target_task_id = task_id(args)?;
    ensure_writer_handoff_available(ctx, Some(target_task_id))?;
    let task = ctx.harness.switch_task(target_task_id).map_err(map_error)?;
    ctx.bind_task_for_session(session_id, &task.id)
        .map_err(|error| tool_error("TASK_BIND_FAILED", error))?;
    Ok(json!({
        "task": task_view(&task),
        "session_task_id": task.id,
        "parallel_tasks_preserved": false,
        "writer_mode": "single_workspace_writer"
    }))
}

fn operation_failure_diagnostics(operations: &[Value]) -> Vec<Value> {
    let mut groups = BTreeMap::<String, FailureDiagnosticGroup>::new();
    for operation in operations {
        if operation.get("status").and_then(Value::as_str) != Some("failed") {
            continue;
        }
        let code = operation
            .get("error_code")
            .and_then(Value::as_str)
            .unwrap_or("UNKNOWN_FAILURE");
        let details = operation
            .get("result_summary")
            .and_then(|value| value.get("error_details"));
        let link_path = details
            .and_then(|value| value.get("link_path"))
            .and_then(Value::as_str);
        let key = if matches!(code, "WORKSPACE_LINK_UNRESOLVED" | "WORKSPACE_LINK_ESCAPE") {
            format!("workspace_link:{}", link_path.unwrap_or("unknown"))
        } else {
            code.to_string()
        };
        let group = groups.entry(key).or_insert_with(|| FailureDiagnosticGroup {
            codes: BTreeSet::new(),
            tools: BTreeSet::new(),
            messages: BTreeSet::new(),
            count: 0,
            link_path: link_path.map(str::to_string),
        });
        group.count += 1;
        group.codes.insert(code.to_string());
        if let Some(tool) = operation.get("tool").and_then(Value::as_str) {
            group.tools.insert(tool.to_string());
        }
        if let Some(message) = operation.get("error_message").and_then(Value::as_str) {
            group.messages.insert(message.to_string());
        }
    }
    groups
        .into_iter()
        .map(|(key, group)| {
            let codes = group.codes.into_iter().collect::<Vec<_>>();
            let tools = group.tools.into_iter().collect::<Vec<_>>();
            let sample_messages = group.messages.into_iter().take(3).collect::<Vec<_>>();
            let (root_cause, recommended_actions) = diagnostic_recommendation(
                codes
                    .first()
                    .map(String::as_str)
                    .unwrap_or("UNKNOWN_FAILURE"),
                group.link_path.as_deref(),
            );
            json!({
                "key": key,
                "root_cause": root_cause,
                "count": group.count,
                "codes": codes,
                "affected_tools": tools,
                "sample_messages": sample_messages,
                "link_path": group.link_path,
                "recommended_actions": recommended_actions
            })
        })
        .collect()
}

struct FailureDiagnosticGroup {
    codes: BTreeSet<String>,
    tools: BTreeSet<String>,
    messages: BTreeSet<String>,
    count: usize,
    link_path: Option<String>,
}

fn diagnostic_recommendation(code: &str, link_path: Option<&str>) -> (String, Value) {
    match code {
        "WORKSPACE_LINK_UNRESOLVED" | "WORKSPACE_LINK_ESCAPE" => (
            "工作区包含失效或越界的 symlink/junction，多个文件、Git 与命令工具可能因此出现级联失败。"
                .into(),
            json!([{
                "tool": "remove_path",
                "args": {"path": link_path.unwrap_or("")},
                "description": "删除链接本体并保留目标目录。"
            }]),
        ),
        "BASELINE_OBSERVATION_STALE" | "BASELINE_REFRESH_CAS_FAILED" | "BASELINE_UNSTABLE" => (
            "工作区在基线读取与接受期间持续变化。".into(),
            json!([{
                "tool": "accept_latest_baseline",
                "description": "在一次调用内重试捕获并接受稳定的最新状态。"
            }]),
        ),
        "SKILL_RESOURCE_INVALID" => (
            "请求的 Skill 资源不在当前受控清单中。".into(),
            json!([{
                "tool": "list_skill_resources",
                "description": "先枚举该 Skill 当前可读取的精确资源路径。"
            }]),
        ),
        "NOT_FOUND" => (
            "请求路径不在当前受控文件或资源清单中。".into(),
            json!([{
                "tool": "list_dir",
                "description": "重新读取当前目录后再执行。"
            }]),
        ),
        "GIT_ERROR" | "GIT_REVERT_CONFLICT" => (
            "Git 索引、工作树或回退操作存在冲突。".into(),
            json!([{
                "tool": "git_status",
                "description": "检查结构化 Git 状态和冲突路径。"
            }]),
        ),
        _ => (
            format!("重复失败由错误代码 {code} 触发。"),
            json!([]),
        ),
    }
}

fn switch_task(
    ctx: &ToolContext,
    args: &Value,
    session_id: Option<&str>,
) -> Result<Value, WorkspaceError> {
    let target_task_id = task_id(args)?;
    ensure_writer_handoff_available(ctx, Some(target_task_id))?;
    let task = ctx.harness.switch_task(target_task_id).map_err(map_error)?;
    ctx.bind_task_for_session(session_id, &task.id)
        .map_err(|error| tool_error("TASK_BIND_FAILED", error))?;
    Ok(json!({
        "task": task_view(&task),
        "session_task_id": task.id,
        "parallel_tasks_preserved": false,
        "writer_mode": "single_workspace_writer",
        "harness": ctx.harness.status_for_task(Some(&task.id)).map_err(map_error)?
    }))
}

fn ensure_writer_handoff_available(
    ctx: &ToolContext,
    target_task_id: Option<&str>,
) -> Result<(), WorkspaceError> {
    let blocking_task_ids = ctx
        .sessions
        .running_task_ids()
        .into_iter()
        .filter(|task_id| Some(task_id.as_str()) != target_task_id)
        .collect::<Vec<_>>();
    if blocking_task_ids.is_empty() {
        return Ok(());
    }
    Err(WorkspaceError::ToolDetails {
        code: "WORKSPACE_WRITER_BUSY",
        message: "Another task still owns a running command in this workspace.".into(),
        category: "conflict",
        retryable: true,
        details: json!({
            "blocking_task_ids": blocking_task_ids,
            "suggestion": "Wait for or stop the running command before transferring the workspace writer lease"
        }),
    })
}

fn accept_latest_baseline(ctx: &ToolContext, args: &Value) -> Result<Value, WorkspaceError> {
    let task_id = task_id(args)?;
    let reason = args
        .get("reason")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| tool_error("INVALID_ARGUMENT", "reason 是必填项"))?;
    let max_attempts = args
        .get("max_attempts")
        .and_then(Value::as_u64)
        .unwrap_or(3)
        .clamp(1, 10) as u8;
    let (task, attempts, baseline) = ctx
        .harness
        .accept_latest_baseline(task_id, reason, max_attempts)
        .map_err(map_error)?;
    Ok(json!({
        "task": task_view(&task),
        "harness": ctx.harness.status().map_err(map_error)?,
        "accepted": true,
        "attempts": attempts,
        "accepted_state": {
            "branch": baseline.branch,
            "head": baseline.head,
            "worktree_fingerprint": baseline.worktree_fingerprint
        }
    }))
}

fn verification_identity_key(record: &VerificationRecord) -> String {
    if let Some(key) = record.verification_key.as_deref() {
        return format!("{}:key:{key}", record.kind);
    }
    if record.test_file.is_some() || record.test_name.is_some() {
        return format!(
            "{}:test:{}:{}",
            record.kind,
            record.test_file.as_deref().unwrap_or_default(),
            record.test_name.as_deref().unwrap_or_default()
        );
    }
    format!("{}:command:{}", record.kind, record.command.trim())
}

fn blocking_verification_views(records: &[VerificationRecord]) -> Vec<Value> {
    effective_verifications(records)
        .into_iter()
        .filter(|record| effective_disposition(record) == "active_failure")
        .map(|record| {
            json!({
                "verification_id": record.id,
                "verification_kind": record.kind,
                "verification_key": record.verification_key,
                "test_file": record.test_file,
                "test_name": record.test_name,
                "command": bounded_text(&record.command, 4_000),
                "failure_disposition": "active_failure",
                "level": record.level,
                "suggested_actions": [
                    "rerun_with_same_verification_identity",
                    "update_verification_disposition"
                ]
            })
        })
        .collect()
}

fn accept_current_baseline(ctx: &ToolContext, args: &Value) -> Result<Value, WorkspaceError> {
    let task_id = task_id(args)?;
    let observation_token = args
        .get("observation_token")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| tool_error("INVALID_ARGUMENT", "observation_token 是必填项"))?;
    let reason = args
        .get("reason")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| tool_error("INVALID_ARGUMENT", "reason 是必填项"))?;
    let task = ctx
        .harness
        .accept_current_baseline(task_id, observation_token, reason)
        .map_err(map_error)?;
    Ok(json!({
        "task": task_view(&task),
        "harness": ctx.harness.status().map_err(map_error)?,
        "accepted": true
    }))
}

fn update_verification_disposition(
    ctx: &ToolContext,
    args: &Value,
) -> Result<Value, WorkspaceError> {
    let task_id = task_id(args)?;
    let verification_id = args
        .get("verification_id")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| tool_error("INVALID_ARGUMENT", "verification_id 是必填项"))?;
    let disposition = args
        .get("disposition")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| tool_error("INVALID_ARGUMENT", "disposition 是必填项"))?;
    let reason = args
        .get("reason")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| tool_error("INVALID_ARGUMENT", "reason 是必填项"))?;
    if disposition == "waived" && !ctx.policy.skip_permission_gates() {
        return Err(tool_error(
            "DANGEROUS_OPERATION_REQUIRES_DANGEROUS_MODE",
            "waived 会接受未通过的验证债务，必须由操作者在受信任控制面启用 dangerous 模式",
        ));
    }
    let source = if disposition == "waived" {
        "dangerous_operator_waiver"
    } else {
        "audited_disposition"
    };
    let verification = ctx
        .harness
        .update_verification_disposition(task_id, verification_id, disposition, reason, source)
        .map_err(map_error)?;
    let records = ctx.harness.list_verifications(task_id).map_err(map_error)?;
    Ok(json!({
        "verification": verification_view(&verification),
        "verification_status": verification_status(&records),
        "effective_disposition": effective_disposition(&verification),
        "task_id": task_id
    }))
}

fn begin_work_session(
    ctx: &ToolContext,
    args: &Value,
    mcp_session_id: Option<&str>,
) -> Result<Value, WorkspaceError> {
    let objective = args
        .get("objective")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| tool_error("INVALID_ARGUMENT", "objective 是必填项"))?;
    let history = crate::tools::history::bootstrap(ctx, args)?;
    let session_key = history
        .get("session_key")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            tool_error(
                "HISTORY_SESSION_INVALID",
                "History Session 缺少 session_key",
            )
        })?;
    let current_path = history
        .get("current_path")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            tool_error(
                "HISTORY_SESSION_INVALID",
                "History Session 缺少 current_path",
            )
        })?;

    let explicit_task = ctx.bound_task_for_session(mcp_session_id);
    let history_task = ctx
        .harness
        .list_tasks()
        .map_err(map_error)?
        .into_iter()
        .find(|task| {
            task.status.is_writable()
                && task.objective == objective
                && task.history_session_key.as_deref() == Some(session_key)
                && task.history_session_path.as_deref() == Some(current_path)
        });
    let fallback_task = if mcp_session_id.is_none() {
        ctx.harness.current_task().map_err(map_error)?
    } else {
        None
    };
    let selected_task = history_task.or(explicit_task).or(fallback_task);
    let (task, task_created, previous_task_id) = match selected_task {
        Some(task) => {
            if task.objective != objective {
                let previous_task_id = task.id.clone();
                ensure_writer_handoff_available(ctx, None)?;
                let next = ctx
                    .harness
                    .start_task_with_handoff(objective, true)
                    .map_err(map_error)?;
                (next, true, Some(previous_task_id))
            } else {
                ensure_writer_handoff_available(ctx, Some(&task.id))?;
                let task = ctx.harness.switch_task(&task.id).map_err(map_error)?;
                (task, false, None)
            }
        }
        None => (
            {
                ensure_writer_handoff_available(ctx, None)?;
                ctx.harness
                    .start_task_with_handoff(objective, true)
                    .map_err(map_error)?
            },
            true,
            None,
        ),
    };
    let task = ctx
        .harness
        .bind_history_session(&task.id, session_key, current_path)
        .map_err(map_error)?;
    ctx.bind_task_for_session(mcp_session_id, &task.id)
        .map_err(|error| tool_error("TASK_BIND_FAILED", error))?;
    let harness = ctx
        .harness
        .status_for_task(Some(&task.id))
        .map_err(map_error)?;
    Ok(json!({
        "work_session": {
            "status": "active",
            "history_session_key": session_key,
            "history_session_path": current_path,
            "task_id": task.id,
            "task_created": task_created,
            "previous_task_id": previous_task_id,
            "parallel": false,
            "writer_mode": "single_workspace_writer",
            "baseline": baseline_view(&task),
            "expected_state": task.expected_state
        },
        "history": compact_history_view(&history),
        "task": task_view(&task),
        "harness": harness,
        "reconnect_required": false
    }))
}

fn compact_history_view(history: &Value) -> Value {
    json!({
        "session_key": history.get("session_key").cloned().unwrap_or(Value::Null),
        "current_path": history.get("current_path").cloned().unwrap_or(Value::Null),
        "current_number": history.get("current_number").cloned().unwrap_or(Value::Null),
        "created": history.get("created").cloned().unwrap_or(Value::Bool(false)),
        "resumed": history.get("resumed").cloned().unwrap_or(Value::Bool(false)),
        "session_status": history.get("session_status").cloned().unwrap_or(Value::Null),
        "checkpoint_count": history.get("checkpoint_count").cloned().unwrap_or(Value::Null),
        "persistence": history.get("persistence").cloned().unwrap_or(Value::Null),
        "warnings": history.get("warnings").cloned().unwrap_or_else(|| json!([]))
    })
}

fn close_work_session(ctx: &ToolContext, args: &Value) -> Result<Value, WorkspaceError> {
    let task_id = task_id(args)?;
    if let Some(outbox) = ctx.harness.load_close_outbox(task_id).map_err(map_error)? {
        return resume_close_outbox(ctx, outbox, true);
    }
    let task_before = ctx.harness.task(task_id).map_err(map_error)?;
    let session_key = task_before
        .history_session_key
        .clone()
        .ok_or_else(|| tool_error("WORK_SESSION_NOT_BOUND", "任务未绑定 History Session"))?;
    let expected_path = task_before
        .history_session_path
        .clone()
        .ok_or_else(|| tool_error("WORK_SESSION_NOT_BOUND", "任务未绑定 History Session 路径"))?;
    let session_status = parse_session_status(args)?;
    let mut checkpoint = args
        .get("checkpoint")
        .cloned()
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}));
    checkpoint["session_key"] = Value::String(session_key.clone());
    checkpoint["expected_path"] = Value::String(expected_path.clone());
    checkpoint["turn_id"] = Value::String(format!("close-work-session-{task_id}"));
    if checkpoint.get("user_intent").is_none() {
        checkpoint["user_intent"] = Value::String(task_before.objective.clone());
    }
    if checkpoint.get("notes").is_none() {
        checkpoint["notes"] = Value::String(
            args.get("summary")
                .and_then(Value::as_str)
                .unwrap_or("Work session closed through close_work_session.")
                .to_string(),
        );
    }
    checkpoint["session_status"] =
        Value::String(harness_session_status_text(session_status).to_string());
    let mut finish_args = args.clone();
    finish_args["task_id"] = Value::String(task_id.to_string());
    let now = harness_timestamp();
    let outbox = WorkSessionCloseOutbox {
        schema_version: SCHEMA_VERSION,
        task_id: task_id.to_string(),
        history_session_key: session_key,
        history_session_path: expected_path,
        session_status,
        finish_args,
        checkpoint_args: checkpoint,
        phase: WorkSessionClosePhase::Prepared,
        attempts: 0,
        last_error: None,
        created_at: now.clone(),
        updated_at: now,
    };
    ctx.harness.save_close_outbox(&outbox).map_err(map_error)?;
    resume_close_outbox(ctx, outbox, true)
}

pub(crate) fn recover_close_outboxes(ctx: &ToolContext) -> Result<Vec<Value>, WorkspaceError> {
    let outboxes = ctx.harness.list_close_outboxes().map_err(map_error)?;
    let mut recovered = Vec::new();
    for outbox in outboxes {
        if outbox.phase == WorkSessionClosePhase::Completed {
            continue;
        }
        match resume_close_outbox(ctx, outbox, false) {
            Ok(result) => recovered.push(json!({
                "ok": result.get("ok").cloned().unwrap_or(Value::Bool(false)),
                "task_id": result.pointer("/work_session/task_id").cloned(),
                "phase": result.pointer("/outbox/phase").cloned(),
                "closed": result.pointer("/work_session/closed").cloned()
            })),
            Err(error) => recovered.push(json!({
                "ok": false,
                "error": error.to_error_value()
            })),
        }
    }
    Ok(recovered)
}

fn resume_close_outbox(
    ctx: &ToolContext,
    mut outbox: WorkSessionCloseOutbox,
    propagate_checkpoint_error: bool,
) -> Result<Value, WorkspaceError> {
    outbox.attempts = outbox.attempts.saturating_add(1);
    outbox.updated_at = harness_timestamp();
    ctx.harness.save_close_outbox(&outbox).map_err(map_error)?;

    let mut finish = None;
    let mut checkpoint = None;
    if outbox.phase == WorkSessionClosePhase::Completed {
        let task = ctx.harness.task(&outbox.task_id).map_err(map_error)?;
        finish = Some(json!({
            "ok": true,
            "task_status": task.status,
            "closed": true,
            "session_status": harness_session_status_text(outbox.session_status),
            "next_stage_started": false,
            "task": task_view(&task),
            "idempotent_retry": true
        }));
        checkpoint = Some(json!({
            "ok": true,
            "session_key": outbox.history_session_key,
            "path": outbox.history_session_path,
            "idempotent_retry": true,
            "outbox_completed": true
        }));
    }
    if outbox.phase == WorkSessionClosePhase::Prepared {
        let task_before = ctx.harness.task(&outbox.task_id).map_err(map_error)?;
        let result = if task_before.status.is_writable() {
            finish_task(ctx, &outbox.finish_args)?
        } else {
            json!({
                "ok": true,
                "task_status": task_before.status,
                "closed": true,
                "session_status": harness_session_status_text(outbox.session_status),
                "next_stage_started": false,
                "task": task_view(&task_before),
                "idempotent_retry": true
            })
        };
        if result.get("ok").and_then(Value::as_bool) != Some(true) {
            let blocking = result
                .get("blocking_verifications")
                .cloned()
                .unwrap_or_else(|| json!([]));
            outbox.last_error = Some(json!({
                "phase": "finish_task",
                "result": result
            }));
            outbox.updated_at = harness_timestamp();
            ctx.harness.save_close_outbox(&outbox).map_err(map_error)?;
            return Ok(json!({
                "ok": false,
                "closed": false,
                "phase": "finish_task",
                "finish": result,
                "checkpoint": null,
                "retryable": true,
                "error": {
                    "code": "WORK_SESSION_VERIFICATION_BLOCKED",
                    "message": "Harness task could not be closed because verification or working-tree requirements are not satisfied.",
                    "category": "validation",
                    "retryable": true,
                    "details": {
                        "blocking_verifications": blocking,
                        "suggestion": "Run the suggested verification action, or use update_verification_disposition for an audited false positive/expected failure."
                    }
                },
                "outbox": close_outbox_view(&outbox)
            }));
        }
        finish = Some(result);
        outbox.phase = WorkSessionClosePhase::TaskClosed;
        outbox.last_error = None;
        outbox.updated_at = harness_timestamp();
        ctx.harness.save_close_outbox(&outbox).map_err(map_error)?;
    }

    if matches!(
        outbox.phase,
        WorkSessionClosePhase::TaskClosed | WorkSessionClosePhase::CheckpointPending
    ) {
        match crate::tools::history::checkpoint(ctx, &outbox.checkpoint_args) {
            Ok(result) => {
                checkpoint = Some(result);
                outbox.phase = WorkSessionClosePhase::Completed;
                outbox.last_error = None;
                outbox.updated_at = harness_timestamp();
                ctx.harness.save_close_outbox(&outbox).map_err(map_error)?;
            }
            Err(error) => {
                outbox.phase = WorkSessionClosePhase::CheckpointPending;
                outbox.last_error = Some(json!({
                    "phase": "history_checkpoint",
                    "cause": error.to_error_value()
                }));
                outbox.updated_at = harness_timestamp();
                ctx.harness.save_close_outbox(&outbox).map_err(map_error)?;
                if propagate_checkpoint_error {
                    return Err(WorkspaceError::ToolDetails {
                        code: "WORK_SESSION_CHECKPOINT_PENDING",
                        message: error.message(),
                        category: "runtime",
                        retryable: true,
                        details: json!({
                            "phase": "history_checkpoint",
                            "task_closed": true,
                            "task_id": outbox.task_id,
                            "session_key": outbox.history_session_key,
                            "expected_path": outbox.history_session_path,
                            "outbox": close_outbox_view(&outbox),
                            "suggestion": "Checkpoint intent is durable and will be retried automatically on the next Harness call.",
                            "cause": error.to_error_value()
                        }),
                    });
                }
            }
        }
    }

    let task = ctx.harness.task(&outbox.task_id).map_err(map_error)?;
    let completed = outbox.phase == WorkSessionClosePhase::Completed;
    Ok(json!({
        "ok": completed,
        "work_session": {
            "status": harness_session_status_text(outbox.session_status),
            "history_session_key": outbox.history_session_key,
            "history_session_path": outbox.history_session_path,
            "task_id": outbox.task_id,
            "task_status": task.status,
            "closed": completed,
            "next_stage_started": false
        },
        "finish": finish,
        "checkpoint": checkpoint,
        "outbox": close_outbox_view(&outbox),
        "task": task_view(&task),
        "harness": ctx.harness.status().map_err(map_error)?
    }))
}

fn close_outbox_view(outbox: &WorkSessionCloseOutbox) -> Value {
    json!({
        "schema_version": outbox.schema_version,
        "task_id": outbox.task_id,
        "phase": outbox.phase,
        "attempts": outbox.attempts,
        "last_error": outbox.last_error,
        "created_at": outbox.created_at,
        "updated_at": outbox.updated_at
    })
}

fn harness_session_status_text(status: HarnessSessionStatus) -> &'static str {
    match status {
        HarnessSessionStatus::Active => "active",
        HarnessSessionStatus::Paused => "paused",
        HarnessSessionStatus::Completed => "completed",
    }
}

fn harness_timestamp() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis().to_string())
        .unwrap_or_else(|_| "0".into())
}

fn bounded_text(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let bounded = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        format!("{bounded}…")
    } else {
        bounded
    }
}

fn bounded_strings(values: &[String], max_items: usize, max_chars: usize) -> Vec<String> {
    values
        .iter()
        .take(max_items)
        .map(|value| bounded_text(value, max_chars))
        .collect()
}

fn refresh_baseline(ctx: &ToolContext, args: &Value) -> Result<Value, WorkspaceError> {
    let task_id = task_id(args)?;
    let observed_head = args.get("observed_head").and_then(Value::as_str);
    let observed_fingerprint = args
        .get("observed_fingerprint")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| tool_error("INVALID_ARGUMENT", "observed_fingerprint 是必填项"))?;
    let reason = args
        .get("reason")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| tool_error("INVALID_ARGUMENT", "reason 是必填项"))?;
    let task = ctx
        .harness
        .refresh_baseline(task_id, observed_head, observed_fingerprint, reason)
        .map_err(map_error)?;
    Ok(json!({
        "task": task_view(&task),
        "harness": ctx.harness.status().map_err(map_error)?
    }))
}

fn harness_status(ctx: &ToolContext, session_id: Option<&str>) -> Result<Value, WorkspaceError> {
    let selected = ctx.task_for_session(session_id);
    serde_json::to_value(
        ctx.harness
            .status_for_task(selected.as_ref().map(|task| task.id.as_str()))
            .map_err(map_error)?,
    )
    .map_err(|e| tool_error("SERIALIZE_FAILED", e.to_string()))
}

fn operation_log(ctx: &ToolContext, args: &Value) -> Result<Value, WorkspaceError> {
    let offset = args.get("cursor").and_then(Value::as_u64).unwrap_or(0) as usize;
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(50)
        .clamp(1, 200) as usize;
    let collapse = args
        .get("collapse")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let task_filter = optional_text(args, "task_id");
    let history_filter = optional_text(args, "history_session_key");
    let mcp_filter = optional_text(args, "mcp_session_id");
    let tool_filter = optional_text(args, "tool");
    let status_filter = optional_text(args, "status");
    let failures_only = args
        .get("failures_only")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let started_after = optional_text(args, "started_after")
        .map(parse_time_filter)
        .transpose()?;
    let started_before = optional_text(args, "started_before")
        .map(parse_time_filter)
        .transpose()?;
    let raw = ctx.harness.all_operations(20_000).map_err(map_error)?;
    let mut operations = if collapse {
        collapse_operations(raw)
    } else {
        raw.into_iter().map(operation_value).collect()
    };
    operations.retain(|operation| {
        matches_optional(operation, "task_id", task_filter.as_deref())
            && matches_optional(operation, "history_session_key", history_filter.as_deref())
            && matches_optional(operation, "mcp_session_id", mcp_filter.as_deref())
            && matches_optional(operation, "tool", tool_filter.as_deref())
            && matches_optional(operation, "status", status_filter.as_deref())
            && (!failures_only || operation.get("status").and_then(Value::as_str) == Some("failed"))
            && within_time_range(operation, started_after, started_before)
    });
    operations.sort_by(|left, right| {
        operation_timestamp(left)
            .cmp(&operation_timestamp(right))
            .then_with(|| left["id"].as_str().cmp(&right["id"].as_str()))
    });
    let total_matches = operations.len();
    let diagnostics = operation_failure_diagnostics(&operations);
    let page = operations
        .into_iter()
        .skip(offset)
        .take(limit)
        .collect::<Vec<_>>();
    let summary = operation_summary(&page, total_matches);
    Ok(json!({
        "operations": page,
        "summary": summary,
        "diagnostics": diagnostics,
        "total_matches": total_matches,
        "next_cursor": if offset + page.len() < total_matches { Some(offset + page.len()) } else { None },
        "filters": {
            "task_id": task_filter,
            "history_session_key": history_filter,
            "mcp_session_id": mcp_filter,
            "tool": tool_filter,
            "status": status_filter,
            "failures_only": failures_only,
            "started_after": args.get("started_after"),
            "started_before": args.get("started_before"),
            "collapse": collapse
        }
    }))
}

fn optional_text(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn parse_time_filter(value: String) -> Result<i64, WorkspaceError> {
    if let Ok(epoch) = value.parse::<i64>() {
        return Ok(epoch);
    }
    chrono::DateTime::parse_from_rfc3339(&value)
        .map(|time| time.timestamp_millis())
        .map_err(|_| {
            tool_error(
                "INVALID_ARGUMENT",
                "started_after/started_before 必须是 epoch 毫秒或 RFC3339 时间",
            )
        })
}

fn operation_timestamp(operation: &Value) -> i64 {
    operation
        .get("started_at")
        .or_else(|| operation.get("created_at"))
        .and_then(Value::as_str)
        .and_then(|value| value.parse().ok())
        .unwrap_or_default()
}

fn within_time_range(operation: &Value, after: Option<i64>, before: Option<i64>) -> bool {
    let timestamp = operation_timestamp(operation);
    after.is_none_or(|value| timestamp >= value) && before.is_none_or(|value| timestamp <= value)
}

fn matches_optional(operation: &Value, key: &str, expected: Option<&str>) -> bool {
    expected.is_none_or(|expected| operation.get(key).and_then(Value::as_str) == Some(expected))
}

fn operation_value(operation: OperationRecord) -> Value {
    let created_at_iso = operation_iso(&operation);
    let result = &operation.result_summary;
    let session_id = result
        .get("session_id")
        .cloned()
        .or_else(|| operation.mcp_session_id.clone().map(Value::String));
    let error_code = result.get("error_code").cloned().unwrap_or(Value::Null);
    let error_message = result.get("error_message").cloned().unwrap_or(Value::Null);
    let duration_ms = result.get("duration_ms").cloned().unwrap_or(Value::Null);
    let verification_id = result
        .get("verification_id")
        .cloned()
        .unwrap_or(Value::Null);
    let disposition = result.get("disposition").cloned().unwrap_or(Value::Null);
    let superseded_by = result.get("superseded_by").cloned().unwrap_or(Value::Null);
    let supersedes = result.get("supersedes").cloned().unwrap_or(Value::Null);
    json!({
        "id": operation.id,
        "trace_id": operation.id,
        "workspace_id": operation.workspace_id,
        "task_id": operation.task_id,
        "history_session_key": operation.history_session_key,
        "mcp_session_id": operation.mcp_session_id,
        "session_id": session_id,
        "tool": operation.tool,
        "status": operation.kind,
        "input_summary": operation.input_summary,
        "result_summary": operation.result_summary,
        "error_code": error_code,
        "error_message": error_message,
        "duration_ms": duration_ms,
        "verification_id": verification_id,
        "disposition": disposition,
        "superseded_by": superseded_by,
        "supersedes": supersedes,
        "reason": operation.reason,
        "affected_files": operation.affected_files,
        "created_at": operation.created_at,
        "created_at_iso": created_at_iso
    })
}

fn collapse_operations(operations: Vec<OperationRecord>) -> Vec<Value> {
    let mut order = Vec::<String>::new();
    let mut grouped = HashMap::<String, Vec<OperationRecord>>::new();
    for operation in operations {
        if !grouped.contains_key(&operation.id) {
            order.push(operation.id.clone());
        }
        grouped
            .entry(operation.id.clone())
            .or_default()
            .push(operation);
    }
    order
        .into_iter()
        .filter_map(|id| grouped.remove(&id))
        .filter_map(collapse_operation)
        .collect()
}

fn collapse_operation(records: Vec<OperationRecord>) -> Option<Value> {
    let started = records.first()?;
    let final_record = records
        .iter()
        .rev()
        .find(|record| record.kind != "started")
        .unwrap_or(started);
    let status = if final_record.kind == "started" {
        "running"
    } else {
        final_record.kind.as_str()
    };
    let affected_files = records
        .iter()
        .rev()
        .find(|record| !record.affected_files.is_empty())
        .map(|record| record.affected_files.clone())
        .unwrap_or_default();
    let started_at_iso = operation_iso(started);
    let completed_at_iso = operation_iso(final_record);
    let computed_duration_ms = if status == "running" {
        None
    } else {
        final_record
            .created_at
            .parse::<i64>()
            .ok()
            .zip(started.created_at.parse::<i64>().ok())
            .map(|(completed, started)| completed.saturating_sub(started).max(0) as u64)
    };
    let result = &final_record.result_summary;
    let session_id = result
        .get("session_id")
        .cloned()
        .or_else(|| final_record.mcp_session_id.clone().map(Value::String));
    let error_code = result.get("error_code").cloned().unwrap_or(Value::Null);
    let error_message = result.get("error_message").cloned().unwrap_or(Value::Null);
    let duration_ms = result
        .get("duration_ms")
        .cloned()
        .unwrap_or_else(|| computed_duration_ms.map(Value::from).unwrap_or(Value::Null));
    let verification_id = result
        .get("verification_id")
        .cloned()
        .unwrap_or(Value::Null);
    let disposition = result.get("disposition").cloned().unwrap_or(Value::Null);
    let superseded_by = result.get("superseded_by").cloned().unwrap_or(Value::Null);
    let supersedes = result.get("supersedes").cloned().unwrap_or(Value::Null);
    Some(json!({
        "id": started.id,
        "trace_id": started.id,
        "workspace_id": started.workspace_id,
        "task_id": final_record.task_id.as_ref().or(started.task_id.as_ref()),
        "history_session_key": final_record.history_session_key.as_ref().or(started.history_session_key.as_ref()),
        "mcp_session_id": final_record.mcp_session_id.as_ref().or(started.mcp_session_id.as_ref()),
        "session_id": session_id,
        "tool": final_record.tool,
        "status": status,
        "input_summary": final_record.input_summary,
        "result_summary": final_record.result_summary,
        "error_code": error_code,
        "error_message": error_message,
        "duration_ms": duration_ms,
        "verification_id": verification_id,
        "disposition": disposition,
        "superseded_by": superseded_by,
        "supersedes": supersedes,
        "reason": final_record.reason.as_ref().or(started.reason.as_ref()),
        "affected_files": affected_files,
        "started_at": started.created_at,
        "started_at_iso": started_at_iso,
        "completed_at": if status == "running" { Value::Null } else { Value::String(final_record.created_at.clone()) },
        "completed_at_iso": if status == "running" { Value::Null } else { Value::String(completed_at_iso) },
        "event_count": records.len()
    }))
}

fn operation_iso(operation: &OperationRecord) -> String {
    if !operation.created_at_iso.is_empty() {
        return operation.created_at_iso.clone();
    }
    operation
        .created_at
        .parse::<i64>()
        .ok()
        .and_then(chrono::DateTime::from_timestamp_millis)
        .map(|time| time.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
        .unwrap_or_default()
}

fn operation_summary(operations: &[Value], total_matches: usize) -> Value {
    let mut tool_counts = BTreeMap::<String, usize>::new();
    let mut affected_files = Vec::<String>::new();
    let mut failed = 0usize;
    let mut running = 0usize;
    let mut duration_ms = 0u64;
    for operation in operations {
        if let Some(tool) = operation.get("tool").and_then(Value::as_str) {
            *tool_counts.entry(tool.to_string()).or_default() += 1;
        }
        match operation.get("status").and_then(Value::as_str) {
            Some("failed") => failed += 1,
            Some("running") | Some("started") => running += 1,
            _ => {}
        }
        duration_ms = duration_ms.saturating_add(
            operation
                .get("duration_ms")
                .and_then(Value::as_u64)
                .unwrap_or_default(),
        );
        if let Some(files) = operation.get("affected_files").and_then(Value::as_array) {
            affected_files.extend(
                files.iter().filter_map(|file| {
                    file.get("path").and_then(Value::as_str).map(str::to_string)
                }),
            );
        }
    }
    affected_files.sort();
    affected_files.dedup();
    json!({
        "total_matches": total_matches,
        "returned_operations": operations.len(),
        "failed_operations": failed,
        "running_operations": running,
        "tool_counts": tool_counts,
        "affected_files": affected_files,
        "duration_ms": duration_ms
    })
}

fn project_state(
    ctx: &ToolContext,
    args: &Value,
    session_id: Option<&str>,
) -> Result<Value, WorkspaceError> {
    let max_files = args.get("max_files").and_then(Value::as_u64).unwrap_or(200) as usize;
    let selected_task = ctx.task_for_session(session_id);
    let state = ctx
        .harness
        .project_state_for_task(
            max_files,
            selected_task.as_ref().map(|task| task.id.as_str()),
        )
        .map_err(map_error)?;
    let all_tasks = ctx.harness.list_tasks().map_err(map_error)?;
    let task_count = all_tasks.len();
    let tasks = all_tasks
        .into_iter()
        .take(100)
        .map(|task| task_view(&task))
        .collect::<Vec<_>>();
    Ok(json!({
        "schema_version": state.schema_version,
        "workspace_id": state.workspace_id,
        "branch": state.branch,
        "head": state.head,
        "clean": state.clean,
        "files": state.files,
        "total_files": state.total_files,
        "truncated": state.truncated,
        "active_task_id": state.active_task_id,
        "active_task_ids": state.active_task_ids,
        "selected_task_id": selected_task.as_ref().map(|task| task.id.clone()),
        "task": state.task.as_ref().map(task_view),
        "tasks": tasks,
        "task_count": task_count,
        "tasks_truncated": task_count > 100,
        "recent_events": state.recent_events
    }))
}

fn start_task(
    ctx: &ToolContext,
    args: &Value,
    session_id: Option<&str>,
) -> Result<Value, WorkspaceError> {
    let objective = args
        .get("objective")
        .and_then(Value::as_str)
        .ok_or_else(|| tool_error("INVALID_ARGUMENT", "objective 是必填项"))?;
    ensure_writer_handoff_available(ctx, None)?;
    let task = ctx
        .harness
        .start_task_with_handoff(objective, true)
        .map_err(map_error)?;
    ctx.bind_task_for_session(session_id, &task.id)
        .map_err(|error| tool_error("TASK_BIND_FAILED", error))?;
    Ok(json!({
        "task": task_view(&task),
        "session_task_id": task.id,
        "parallel": false,
        "writer_mode": "single_workspace_writer",
        "next": ["project_state", "task_context"]
    }))
}

fn update_task(ctx: &ToolContext, args: &Value) -> Result<Value, WorkspaceError> {
    let task_id = task_id(args)?;
    let completed_steps = string_list(args.get("completed_steps"))?;
    let pending_steps = string_list(args.get("pending_steps"))?;
    let task = ctx
        .harness
        .update_steps(task_id, completed_steps, pending_steps)
        .map_err(map_error)?;
    Ok(json!({"task": task_view(&task)}))
}

fn transition(
    ctx: &ToolContext,
    args: &Value,
    status: TaskStatus,
) -> Result<Value, WorkspaceError> {
    let task = ctx
        .harness
        .transition(task_id(args)?, status)
        .map_err(map_error)?;
    Ok(json!({"task": task_view(&task)}))
}

fn finish_task(ctx: &ToolContext, args: &Value) -> Result<Value, WorkspaceError> {
    let task_id = task_id(args)?;
    ctx.harness.check_baseline(task_id).map_err(map_error)?;
    let allow_unverified = args
        .get("allow_unverified")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let verifications = ctx.harness.list_verifications(task_id).map_err(map_error)?;
    let verification_status = verification_status(&verifications);
    let mut working_tree_files = git_working_tree_files(ctx.workspace.root());
    working_tree_files.retain(|path| !is_runtime_artifact(path));
    let (task_working_tree_files, peer_working_tree_files, unattributed_working_tree_files) =
        classify_working_tree_ownership(ctx, task_id, &working_tree_files);
    let mut blocking_working_tree_files = task_working_tree_files.clone();
    blocking_working_tree_files.extend(unattributed_working_tree_files.clone());
    blocking_working_tree_files.sort();
    blocking_working_tree_files.dedup();
    if !blocking_working_tree_files.is_empty() {
        let task = ctx.harness.mark_verifying(task_id).map_err(map_error)?;
        return Ok(json!({
            "ok": false,
            "task_status": "verifying",
            "verification_status": verification_status,
            "closed": false,
            "session_status": "active",
            "next_stage_started": false,
            "reason": "当前任务仍拥有未提交或无法归属的业务文件；提交或还原这些改动后才能关闭任务。",
            "error": {
                "code": "TASK_WORKTREE_NOT_CLEAN",
                "message": "Workspace contains uncommitted business files.",
                "category": "validation",
                "retryable": true,
                "details": {
                    "working_tree_files": blocking_working_tree_files,
                    "task_working_tree_files": task_working_tree_files,
                    "unattributed_working_tree_files": unattributed_working_tree_files,
                    "peer_working_tree_files": peer_working_tree_files
                }
            },
            "working_tree_files": blocking_working_tree_files,
            "task_working_tree_files": task_working_tree_files,
            "unattributed_working_tree_files": unattributed_working_tree_files,
            "peer_working_tree_files": peer_working_tree_files,
            "next_actions": ["git_status", "stage_commit", "finish_task"],
            "task": task_view(&task),
            "verification": verification_views(&verifications, "effective"),
            "verification_summary": verification_presentation_summary(&verifications)
        }));
    }
    if !allow_unverified && !verification_status_is_accepted(verification_status) {
        let task = ctx.harness.mark_verifying(task_id).map_err(map_error)?;
        let reason = if verification_status == "missing" {
            "任务缺少结构化验证证据；请使用 exec_command.verification_kind 或 stage_commit.required_checks 运行验证。"
        } else {
            "至少一项结构化验证失败；修复后重新运行验证并再次调用 finish_task。"
        };
        return Ok(json!({
            "ok": false,
            "task_status": "verifying",
            "verification_status": verification_status,
            "closed": false,
            "session_status": "active",
            "next_stage_started": false,
            "reason": reason,
            "error": {
                "code": if verification_status == "missing" { "TASK_VERIFICATION_MISSING" } else { "TASK_VERIFICATION_FAILED" },
                "message": reason,
                "category": "validation",
                "retryable": true,
                "details": {
                    "blocking_verifications": blocking_verification_views(&verifications),
                    "suggestion": "Run a later successful verification with verification_key/test_file/test_name, or update the blocking record disposition with an audited reason."
                }
            },
            "blocking_verifications": blocking_verification_views(&verifications),
            "next_actions": ["exec_command", "change_summary", "finish_task"],
            "task": task_view(&task),
            "verification": verification_views(&verifications, "effective"),
            "verification_summary": verification_presentation_summary(&verifications)
        }));
    }
    let session_status = parse_session_status(args)?;
    let verified = verification_status_is_accepted(verification_status);
    let change_summary = change_summary(
        ctx,
        &json!({
            "task_id": task_id,
            "limit": DEFAULT_SUMMARY_LIMIT
        }),
        None,
    )?;
    let task = ctx
        .harness
        .complete_task(task_id, verified, session_status)
        .map_err(map_error)?;
    let workspace_session_status = ctx.harness.status().map_err(map_error)?.session_status;
    let mut response = json!({
        "ok": true,
        "task_status": if verified { "completed" } else { "completed_unverified" },
        "verification_status": if verified { verification_status } else { "unverified" },
        "closed": true,
        "session_status": workspace_session_status,
        "requested_session_status": session_status,
        "next_stage_started": false,
        "task": task_view(&task),
        "change_summary": change_summary,
        "truncated": false,
        "details_tool": {
            "name": "change_summary",
            "arguments": {"task_id": task_id}
        },
        "max_response_bytes": FINISH_RESPONSE_MAX_BYTES
    });
    bound_finish_response(&mut response);
    Ok(response)
}

fn task_context(
    ctx: &ToolContext,
    args: &Value,
    session_id: Option<&str>,
) -> Result<Value, WorkspaceError> {
    let task = if let Some(task_id) = args.get("task_id").and_then(Value::as_str) {
        Some(ctx.harness.task(task_id).map_err(map_error)?)
    } else {
        ctx.task_for_session(session_id)
    };
    let Some(task) = task else {
        return Ok(json!({"task": null, "message": "当前没有活动任务"}));
    };
    let max_bytes = args
        .get("max_bytes")
        .and_then(Value::as_u64)
        .unwrap_or(32_768)
        .clamp(8_192, 131_072) as usize;
    let events = ctx
        .harness
        .list_events(&task.id, 0, 200)
        .map_err(map_error)?;
    let mut bounded_events = Vec::new();
    for event in events.iter() {
        let candidate = compact_event(event);
        let mut probe = bounded_events.clone();
        probe.push(candidate.clone());
        let probe_value = json!({"task": task_view(&task), "events": probe});
        if serde_json::to_vec(&probe_value)
            .map(|bytes| bytes.len() > max_bytes)
            .unwrap_or(true)
        {
            break;
        }
        bounded_events.push(candidate);
    }
    let truncated = bounded_events.len() < events.len();
    Ok(json!({
        "task": task_view(&task),
        "events": bounded_events,
        "truncated": truncated,
        "next_cursor": if truncated { Some(bounded_events.len()) } else { None },
        "max_bytes": max_bytes
    }))
}

fn list_task_events(ctx: &ToolContext, args: &Value) -> Result<Value, WorkspaceError> {
    let task_id = task_id(args)?;
    let offset = args.get("cursor").and_then(Value::as_u64).unwrap_or(0) as usize;
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(50)
        .clamp(1, 200) as usize;
    let events = ctx
        .harness
        .list_events(task_id, offset, limit)
        .map_err(map_error)?;
    Ok(json!({"events": events, "next_cursor": offset + events.len()}))
}

fn change_summary(
    ctx: &ToolContext,
    args: &Value,
    session_id: Option<&str>,
) -> Result<Value, WorkspaceError> {
    let task = if let Some(task_id) = args.get("task_id").and_then(Value::as_str) {
        ctx.harness.task(task_id).map_err(map_error)?
    } else {
        ctx.task_for_session(session_id)
            .ok_or_else(|| tool_error("TASK_STATE_REQUIRED", "没有可总结的活动任务"))?
    };
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_SUMMARY_LIMIT as u64)
        .clamp(1, 500) as usize;
    let cursor = args.get("cursor").and_then(Value::as_u64).unwrap_or(0) as usize;
    let section = args.get("section").and_then(Value::as_str);
    let verification_history_mode = args
        .get("verification_history_mode")
        .and_then(Value::as_str)
        .unwrap_or("effective");
    let requested_change_id = args
        .get("change_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let all_changes = ctx.harness.list_change_sets(&task.id).map_err(map_error)?;
    let selected_changes = if let Some(change_id) = requested_change_id {
        ctx.harness
            .load_change_set(change_id)
            .map_err(map_error)?
            .into_iter()
            .collect::<Vec<_>>()
    } else {
        all_changes
    };
    let commits = selected_changes
        .iter()
        .map(|change| {
            json!({
                "change_id": change.id,
                "commit_sha": change.commit_sha,
                "created_at": change.created_at,
                "committed_files": change.committed_files,
                "verification_ids": change.verification_ids
            })
        })
        .collect::<Vec<_>>();
    let commit_sha = selected_changes
        .last()
        .and_then(|change| change.commit_sha.clone())
        .or_else(|| requested_change_id.map(str::to_string))
        .or_else(|| {
            task.expected_state
                .head
                .clone()
                .filter(|head| task.baseline.head.as_ref() != Some(head))
        });
    let mut committed_file_set = BTreeSet::new();
    for change in &selected_changes {
        committed_file_set.extend(change.committed_files.iter().cloned());
    }
    if committed_file_set.is_empty() {
        if let Some(commit) = commit_sha.as_deref() {
            committed_file_set.extend(git_paths(
                ctx.workspace.root(),
                &[
                    "diff-tree",
                    "--no-commit-id",
                    "--name-only",
                    "-r",
                    "-z",
                    commit,
                ],
            ));
        }
    }
    let committed_files = committed_file_set.into_iter().collect::<Vec<_>>();
    let files_by_commit = selected_changes
        .iter()
        .map(|change| {
            json!({
                "commit_sha": change.commit_sha,
                "change_id": change.id,
                "files": change.committed_files
            })
        })
        .collect::<Vec<_>>();
    let first_commit = selected_changes
        .first()
        .and_then(|change| change.commit_sha.clone());
    let last_commit = selected_changes
        .last()
        .and_then(|change| change.commit_sha.clone());
    let end_head = task
        .expected_state
        .head
        .clone()
        .or_else(|| last_commit.clone());
    let net_changed_files = match (task.baseline.head.as_deref(), end_head.as_deref()) {
        (Some(start), Some(end)) if start != end => {
            let range = format!("{start}..{end}");
            git_paths(
                ctx.workspace.root(),
                &["diff", "--name-only", "-z", range.as_str()],
            )
        }
        _ => Vec::new(),
    };
    let mut working_tree_files = git_working_tree_files(ctx.workspace.root());
    let mut runtime_artifacts = known_runtime_artifacts(ctx.workspace.root());
    let mut ignored_files = known_ignored_paths(ctx.workspace.root());
    working_tree_files.retain(|path| !is_runtime_artifact(path));
    let (task_working_tree_files, peer_working_tree_files, unattributed_working_tree_files) =
        classify_working_tree_ownership(ctx, &task.id, &working_tree_files);
    working_tree_files = task_working_tree_files.clone();
    working_tree_files.extend(unattributed_working_tree_files.clone());
    working_tree_files.sort();
    working_tree_files.dedup();
    for change in &selected_changes {
        runtime_artifacts.extend(change.runtime_artifacts.clone());
        ignored_files.extend(change.ignored_files.clone());
    }
    runtime_artifacts.sort();
    runtime_artifacts.dedup();
    ignored_files.sort();
    ignored_files.dedup();
    let verifications = ctx
        .harness
        .list_verifications(&task.id)
        .map_err(map_error)?;
    let verification = verification_views(&verifications, verification_history_mode);
    let verification_summary = verification_presentation_summary(&verifications);
    let events = ctx
        .harness
        .list_events(&task.id, 0, 100)
        .map_err(map_error)?;
    let evidence = events
        .iter()
        .rev()
        .take(DEFAULT_EVENT_LIMIT)
        .map(compact_event)
        .collect::<Vec<_>>();
    let counts = json!({
        "commits": commits.len(),
        "committed_files": committed_files.len(),
        "net_changed_files": net_changed_files.len(),
        "working_tree_files": working_tree_files.len(),
        "task_working_tree_files": task_working_tree_files.len(),
        "peer_working_tree_files": peer_working_tree_files.len(),
        "unattributed_working_tree_files": unattributed_working_tree_files.len(),
        "runtime_artifacts": runtime_artifacts.len(),
        "ignored_files": ignored_files.len(),
        "verification": verification.len(),
        "verification_total": verifications.len(),
        "evidence": events.len()
    });
    let bounded_objective = bounded_text(&task.objective, 4_000);
    let mut summary = json!({
        "task_id": task.id,
        "objective": bounded_objective,
        "why": {"text": bounded_objective, "source": "task_objective"},
        "commit_sha": commit_sha,
        "commit_count": commits.len(),
        "first_commit": first_commit,
        "last_commit": last_commit,
        "commits": commits,
        "files_by_commit": files_by_commit,
        "committed_files": committed_files,
        "net_changed_files": net_changed_files,
        "working_tree_files": working_tree_files,
        "task_working_tree_files": task_working_tree_files,
        "peer_working_tree_files": peer_working_tree_files,
        "unattributed_working_tree_files": unattributed_working_tree_files,
        "runtime_artifacts": runtime_artifacts,
        "ignored_files": ignored_files,
        "evidence": evidence,
        "verification": verification,
        "verification_history_mode": verification_history_mode,
        "verification_summary": verification_summary,
        "verification_status": verification_status(&verifications),
        "risks": [],
        "rollback_capability": if selected_changes.is_empty() { "not_available" } else { "git_commit_range" },
        "baseline": baseline_view(&task),
        "counts": counts,
        "truncated": false,
        "next_cursor": null
    });
    paginate_summary_section(&mut summary, section, cursor, limit);
    Ok(summary)
}

fn parse_session_status(args: &Value) -> Result<HarnessSessionStatus, WorkspaceError> {
    match args
        .get("session_status")
        .and_then(Value::as_str)
        .unwrap_or("paused")
    {
        "active" => Ok(HarnessSessionStatus::Active),
        "paused" => Ok(HarnessSessionStatus::Paused),
        "completed" => Ok(HarnessSessionStatus::Completed),
        _ => Err(tool_error(
            "INVALID_ARGUMENT",
            "session_status 必须是 active、paused 或 completed",
        )),
    }
}

fn baseline_view(task: &TaskSession) -> Value {
    json!({
        "file_count": task.baseline.file_count,
        "baseline_hash": task.baseline.worktree_fingerprint,
        "baseline_object_id": task.baseline.object_id,
        "head": task.baseline.head,
        "branch": task.baseline.branch,
        "captured_at": task.baseline.captured_at
    })
}

fn task_view(task: &TaskSession) -> Value {
    json!({
        "id": task.id,
        "workspace_id": task.workspace_id,
        "objective": bounded_text(&task.objective, 4_000),
        "status": task.status,
        "baseline": baseline_view(task),
        "expected_state": task.expected_state,
        "completed_steps": bounded_strings(&task.completed_steps, 64, 1_000),
        "pending_steps": bounded_strings(&task.pending_steps, 64, 1_000),
        "latest_change_id": task.latest_change_id,
        "latest_verification_id": task.latest_verification_id,
        "history_session_key": task.history_session_key,
        "history_session_path": task.history_session_path,
        "created_at": task.created_at,
        "updated_at": task.updated_at
    })
}

fn verification_status(records: &[VerificationRecord]) -> &'static str {
    let effective = effective_verifications(records);
    if effective.is_empty() {
        "missing"
    } else if effective
        .iter()
        .any(|record| effective_disposition(record) == "active_failure")
    {
        "failed"
    } else if effective
        .iter()
        .any(|record| matches!(effective_disposition(record), "expected_failure" | "waived"))
    {
        "verified_with_exceptions"
    } else {
        "verified"
    }
}

fn verification_status_is_accepted(status: &str) -> bool {
    matches!(status, "verified" | "verified_with_exceptions")
}

fn effective_verifications(records: &[VerificationRecord]) -> Vec<&VerificationRecord> {
    let mut latest = BTreeMap::<String, &VerificationRecord>::new();
    for record in records {
        latest.insert(verification_identity_key(record), record);
    }
    latest
        .into_values()
        .filter(|record| {
            !matches!(
                effective_disposition(record),
                "diagnostic_only" | "superseded"
            )
        })
        .collect()
}

fn verification_views(records: &[VerificationRecord], history_mode: &str) -> Vec<Value> {
    if history_mode == "all" {
        return records.iter().map(verification_view).collect();
    }
    effective_verifications(records)
        .into_iter()
        .map(verification_view)
        .collect()
}

fn verification_presentation_summary(records: &[VerificationRecord]) -> Value {
    let effective = effective_verifications(records);
    let effective_ids = effective
        .iter()
        .map(|record| record.id.as_str())
        .collect::<HashSet<_>>();
    let collapsed_records = records.len().saturating_sub(effective.len());
    let historical_failures_collapsed = records
        .iter()
        .filter(|record| !record.passed && !effective_ids.contains(record.id.as_str()))
        .count();
    let active_failures = effective
        .iter()
        .filter(|record| effective_disposition(record) == "active_failure")
        .count();
    let exceptions = effective
        .iter()
        .filter(|record| matches!(effective_disposition(record), "expected_failure" | "waived"))
        .count();
    let passed = effective
        .iter()
        .filter(|record| effective_disposition(record) == "passed")
        .count();
    json!({
        "total_records": records.len(),
        "effective_records": effective.len(),
        "collapsed_records": collapsed_records,
        "historical_failures_collapsed": historical_failures_collapsed,
        "active_failures": active_failures,
        "exceptions": exceptions,
        "passed": passed,
        "expand_with": {
            "tool": "change_summary",
            "arguments": {"verification_history_mode": "all", "section": "verification"}
        }
    })
}

fn verification_view(record: &VerificationRecord) -> Value {
    json!({
        "verification_id": record.id,
        "kind": record.kind,
        "verification_key": record.verification_key,
        "test_file": record.test_file,
        "test_name": record.test_name,
        "status": record.status,
        "level": record.level,
        "passed": record.passed,
        "effective_disposition": effective_disposition(record),
        "disposition_history": record.dispositions,
        "exit_code": record.exit_code,
        "command": bounded_text(&record.command, 4_000),
        "duration_ms": record.duration_ms,
        "change_id": record.change_id,
        "supersedes": record.supersedes,
        "created_at": record.created_at
    })
}

fn effective_disposition(record: &VerificationRecord) -> &str {
    record
        .dispositions
        .last()
        .map(|entry| entry.disposition.as_str())
        .unwrap_or(if record.passed {
            "passed"
        } else {
            "active_failure"
        })
}

fn compact_event(event: &HarnessEvent) -> Value {
    json!({
        "id": event.id,
        "operation_id": event.operation_id,
        "kind": event.kind,
        "tool_name": event.tool_name,
        "created_at": event.created_at,
        "result": {
            "ok": event.result_summary.get("ok"),
            "status": event.result_summary.get("status"),
            "exit_code": event.result_summary.get("exit_code")
        }
    })
}

fn bound_finish_response(response: &mut Value) {
    let initial_bytes = serde_json::to_vec(response)
        .map(|bytes| bytes.len())
        .unwrap_or(usize::MAX);
    if initial_bytes > FINISH_RESPONSE_MAX_BYTES {
        if let Some(summary) = response
            .get_mut("change_summary")
            .and_then(Value::as_object_mut)
        {
            for key in [
                "committed_files",
                "working_tree_files",
                "runtime_artifacts",
                "ignored_files",
                "verification",
            ] {
                if let Some(array) = summary.get_mut(key).and_then(Value::as_array_mut) {
                    array.truncate(16);
                }
            }
            if let Some(array) = summary.get_mut("evidence").and_then(Value::as_array_mut) {
                array.clear();
            }
            summary.insert("truncated".into(), Value::Bool(true));
        }
        if let Some(object) = response.as_object_mut() {
            object.insert("truncated".into(), Value::Bool(true));
        }
    }
    let after_array_trim = serde_json::to_vec(response)
        .map(|bytes| bytes.len())
        .unwrap_or(FINISH_RESPONSE_MAX_BYTES);
    if after_array_trim > FINISH_RESPONSE_MAX_BYTES {
        if let Some(summary) = response
            .get_mut("change_summary")
            .and_then(Value::as_object_mut)
        {
            summary.remove("evidence");
            summary.remove("verification");
            summary.remove("why");
            summary.insert("truncated".into(), Value::Bool(true));
        }
        if let Some(task) = response.get_mut("task").and_then(Value::as_object_mut) {
            task.insert("completed_steps".into(), json!([]));
            task.insert("pending_steps".into(), json!([]));
            if let Some(objective) = task.get("objective").and_then(Value::as_str) {
                task.insert("objective".into(), json!(bounded_text(objective, 1_000)));
            }
        }
    }
    if let Some(object) = response.as_object_mut() {
        object.insert("response_bytes".into(), json!(0));
    }
    let mut response_bytes = update_response_bytes(response);
    if response_bytes > FINISH_RESPONSE_MAX_BYTES {
        if let Some(summary) = response
            .get_mut("change_summary")
            .and_then(Value::as_object_mut)
        {
            for key in [
                "committed_files",
                "working_tree_files",
                "runtime_artifacts",
                "ignored_files",
                "verification",
                "evidence",
            ] {
                summary.insert(key.into(), json!([]));
            }
            summary.insert("objective".into(), json!(""));
            summary.remove("why");
            summary.insert("truncated".into(), Value::Bool(true));
        }
        if let Some(object) = response.as_object_mut() {
            object.insert("truncated".into(), Value::Bool(true));
        }
        response_bytes = update_response_bytes(response);
    }
    debug_assert_eq!(response_bytes, update_response_bytes(response));
}

pub(crate) fn update_response_bytes(response: &mut Value) -> usize {
    let mut measured = 0usize;
    for _ in 0..4 {
        measured = serde_json::to_vec(response)
            .map(|bytes| bytes.len())
            .unwrap_or(FINISH_RESPONSE_MAX_BYTES);
        let current = response.get("response_bytes").and_then(Value::as_u64);
        if current == Some(measured as u64) {
            return measured;
        }
        if let Some(object) = response.as_object_mut() {
            object.insert("response_bytes".into(), json!(measured));
        }
    }
    serde_json::to_vec(response)
        .map(|bytes| bytes.len())
        .unwrap_or(measured)
}

fn paginate_summary_section(
    summary: &mut Value,
    section: Option<&str>,
    cursor: usize,
    limit: usize,
) {
    let keys = [
        "commits",
        "files_by_commit",
        "committed_files",
        "net_changed_files",
        "working_tree_files",
        "runtime_artifacts",
        "ignored_files",
        "verification",
        "evidence",
    ];
    let mut next_cursors = serde_json::Map::new();
    let mut truncated = false;
    for key in keys {
        let selected = section.is_none() || section == Some(key);
        let start = if section == Some(key) { cursor } else { 0 };
        let page_limit = if key == "evidence" {
            limit.min(DEFAULT_EVENT_LIMIT)
        } else {
            limit
        };
        let Some(array) = summary.get_mut(key).and_then(Value::as_array_mut) else {
            continue;
        };
        if !selected {
            array.clear();
            continue;
        }
        let total = array.len();
        let page = array
            .iter()
            .skip(start)
            .take(page_limit)
            .cloned()
            .collect::<Vec<_>>();
        let next = start + page.len();
        if next < total {
            truncated = true;
            next_cursors.insert(key.into(), json!(next));
        }
        *array = page;
    }
    if let Some(object) = summary.as_object_mut() {
        object.insert("truncated".into(), Value::Bool(truncated));
        object.insert(
            "next_cursor".into(),
            if next_cursors.is_empty() {
                Value::Null
            } else {
                Value::Object(next_cursors)
            },
        );
        if let Some(section) = section {
            object.insert("section".into(), Value::String(section.to_string()));
        }
    }
}

fn git_paths(root: &Path, args: &[&str]) -> Vec<String> {
    let mut command = Command::new("git");
    crate::platform::hide_std_console(&mut command);
    let Ok(output) = command.arg("-C").arg(root).args(args).output() else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .filter_map(|path| std::str::from_utf8(path).ok())
        .map(|path| path.replace('\\', "/"))
        .collect()
}

fn git_working_tree_files(root: &Path) -> Vec<String> {
    let mut files = git_paths(root, &["diff", "HEAD", "--name-only", "-z"]);
    files.extend(git_paths(
        root,
        &["ls-files", "--others", "--exclude-standard", "-z"],
    ));
    files.sort();
    files.dedup();
    files
}

fn classify_working_tree_ownership(
    ctx: &ToolContext,
    task_id: &str,
    working_tree_files: &[String],
) -> (Vec<String>, Vec<String>, Vec<String>) {
    let mut owners = HashMap::<String, String>::new();
    if let Ok(operations) = ctx.harness.all_operations(20_000) {
        for operation in operations {
            let Some(owner) = operation.task_id else {
                continue;
            };
            for file in operation.affected_files {
                owners.insert(file.path.replace('\\', "/"), owner.clone());
            }
        }
    }
    let mut owned = Vec::new();
    let mut peer = Vec::new();
    let mut unattributed = Vec::new();
    for path in working_tree_files {
        match owners.get(path) {
            Some(owner) if owner == task_id => owned.push(path.clone()),
            Some(_) => peer.push(path.clone()),
            None => unattributed.push(path.clone()),
        }
    }
    (owned, peer, unattributed)
}

fn known_runtime_artifacts(root: &Path) -> Vec<String> {
    [".codegraph", ".gitnexus"]
        .into_iter()
        .filter(|path| root.join(path).exists() && is_git_ignored(root, path))
        .map(|path| format!("{path}/"))
        .collect()
}

fn known_ignored_paths(root: &Path) -> Vec<String> {
    [".codegraph", ".gitnexus", "docs/history-session"]
        .into_iter()
        .filter(|path| root.join(path).exists() && is_git_ignored(root, path))
        .map(|path| format!("{path}/"))
        .collect()
}

fn is_git_ignored(root: &Path, path: &str) -> bool {
    let mut command = Command::new("git");
    crate::platform::hide_std_console(&mut command);
    command
        .arg("-C")
        .arg(root)
        .args(["check-ignore", "-q", "--", path])
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn is_runtime_artifact(path: &str) -> bool {
    path.starts_with(".codegraph/")
        || path.starts_with(".gitnexus/")
        || path.ends_with(".db-wal")
        || path.ends_with(".db-shm")
        || path.ends_with("daemon.log")
        || path.ends_with("daemon.pid")
}

fn task_id(args: &Value) -> Result<&str, WorkspaceError> {
    args.get("task_id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| tool_error("INVALID_ARGUMENT", "task_id 是必填项"))
}

fn string_list(value: Option<&Value>) -> Result<Option<Vec<String>>, WorkspaceError> {
    let Some(value) = value else { return Ok(None) };
    let list = value
        .as_array()
        .ok_or_else(|| tool_error("INVALID_ARGUMENT", "步骤必须是字符串数组"))?
        .iter()
        .map(|item| {
            item.as_str()
                .map(str::to_string)
                .ok_or_else(|| tool_error("INVALID_ARGUMENT", "步骤必须是字符串数组"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Some(list))
}

fn map_error(error: HarnessError) -> WorkspaceError {
    tool_error(error.code(), error.to_string())
}

fn tool_error(code: &'static str, message: impl Into<String>) -> WorkspaceError {
    WorkspaceError::Tool {
        code,
        message: message.into(),
        category: "permission",
        retryable: matches!(
            code,
            "TASK_ALREADY_ACTIVE"
                | "FILE_CHANGED_EXTERNALLY"
                | "BASELINE_STALE"
                | "BASELINE_OBSERVATION_STALE"
        ),
    }
}
