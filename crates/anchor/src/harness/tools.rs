use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::tools::workspace::{tool_ok, Workspace, WorkspaceError};
use crate::tools::{CancellationToken, ToolContext};

use super::model::{
    HarnessEvent, HarnessSessionStatus, OperationRecord, TaskContract, TaskPhase,
    TaskRecoveryStatus, TaskSession, TaskSlice, TaskSliceStatus, TaskStatus, TaskWorkingSet,
    VerificationRecord, VerificationRequirement, WorkSessionCloseOutbox, WorkSessionClosePhase,
    SCHEMA_VERSION,
};
use super::store::HarnessError;

const FINISH_RESPONSE_MAX_BYTES: usize = 32 * 1024;
const DEFAULT_SUMMARY_LIMIT: usize = 64;
const DEFAULT_EVENT_LIMIT: usize = 20;

pub const TOOL_NAMES: &[&str] = &[
    "harness_status",
    "operation_log",
    "resolve_recovery",
    "begin_work_session",
    "close_work_session",
    "complete_work_session",
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
    "task_gate_status",
    "start_slice",
    "update_slice",
    "complete_slice",
    "pause_task",
    "abort_task",
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
    if matches!(
        name,
        "harness_status" | "begin_work_session" | "project_state" | "start_task" | "switch_task"
    ) {
        auto_pause_stale_tasks(ctx)?;
    }
    let recovered_outboxes = if matches!(name, "close_work_session" | "complete_work_session")
        || !ctx.is_primary_workspace()
    {
        Vec::new()
    } else {
        recover_close_outboxes(ctx)?
    };
    let value = match name {
        "harness_status" => harness_status(ctx, session_id),
        "operation_log" => operation_log(ctx, args),
        "resolve_recovery" => resolve_recovery(ctx, args),
        "begin_work_session" => begin_work_session(ctx, args, session_id),
        "close_work_session" => close_work_session(ctx, args),
        "complete_work_session" => complete_work_session(ctx, args),
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
        "task_gate_status" => task_gate_status(ctx, args, session_id),
        "start_slice" => start_slice(ctx, args),
        "update_slice" => update_slice(ctx, args),
        "complete_slice" => complete_slice(ctx, args),
        "pause_task" => transition(ctx, args, TaskStatus::Paused),
        "abort_task" => abort_task(ctx, args),
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
    let visible_outboxes = visible_outbox_recovery(name, args, &value, recovered_outboxes);
    if !visible_outboxes.is_empty() {
        if let Some(object) = value.as_object_mut() {
            object.insert("outbox_recovery".into(), json!(visible_outboxes));
        }
    }

    Ok(tool_ok(value))
}

fn ensure_session_reclaim_safe(
    ctx: &ToolContext,
    task: &TaskSession,
    args: &Value,
) -> Result<(), WorkspaceError> {
    let expected_head = args
        .get("expected_head")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| WorkspaceError::ToolDetails {
            code: "WORK_SESSION_RECLAIM_HEAD_REQUIRED",
            message: "expected_head is required when reclaim_session=true".into(),
            category: "validation",
            retryable: true,
            details: json!({
                "cause_scope": "task_lease",
                "workspace_mutated": false,
                "task_id": task.id,
                "recommended_retry": {
                    "tool": "begin_work_session",
                    "arguments": {
                        "task_id": task.id,
                        "objective": task.objective,
                        "create_if_missing": false,
                        "reclaim_session": true,
                        "expected_head": task.expected_state.head
                    }
                }
            }),
        })?;
    let durable_head = task.expected_state.head.as_deref().unwrap_or_default();
    if durable_head != expected_head {
        return Err(WorkspaceError::ToolDetails {
            code: "WORK_SESSION_RECLAIM_HEAD_MISMATCH",
            message: "The observed HEAD does not match the durable Task expected HEAD.".into(),
            category: "conflict",
            retryable: true,
            details: json!({
                "task_id": task.id,
                "expected_head": durable_head,
                "observed_head": expected_head,
                "cause_scope": "task_lease",
                "workspace_mutated": false,
                "recommended_retry": {
                    "tool": "begin_work_session",
                    "arguments": {
                        "task_id": task.id,
                        "objective": task.objective,
                        "create_if_missing": false,
                        "reclaim_session": true,
                        "expected_head": durable_head
                    }
                }
            }),
        });
    }
    let (running, unobserved_terminal) = ctx.sessions.pending_for_task(&task.id, 4096);
    if !running.is_empty() || !unobserved_terminal.is_empty() {
        return Err(WorkspaceError::ToolDetails {
            code: "WORK_SESSION_RECLAIM_BUSY",
            message: "The prior Task lease still owns a running command or an unconsumed terminal result.".into(),
            category: "conflict",
            retryable: true,
            details: json!({
                "task_id": task.id,
                "running_sessions": running,
                "unobserved_terminal_sessions": unobserved_terminal,
                "required_action": "Wait for/consume or terminate the retained command session before reclaiming the Task.",
                "cause_scope": "task_lease",
                "workspace_mutated": false,
                "recommended_retry": {
                    "strategy": "consume_or_terminate_retained_commands_then_retry",
                    "tools": ["wait_command", "kill_session", "begin_work_session"]
                }
            }),
        });
    }
    ensure_writer_handoff_available(ctx, Some(&task.id), None)
}

fn auto_pause_stale_tasks(ctx: &ToolContext) -> Result<Vec<String>, WorkspaceError> {
    let protected = ctx
        .sessions
        .task_ids_requiring_followup()
        .into_iter()
        .collect::<std::collections::HashSet<_>>();
    ctx.harness
        .pause_stale_active_tasks(&protected)
        .map_err(map_error)
}

fn resolve_recovery(ctx: &ToolContext, args: &Value) -> Result<Value, WorkspaceError> {
    let task_id = task_id(args)?;
    let recovery_id = args
        .get("recovery_id")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| tool_error("INVALID_ARGUMENT", "recovery_id 是必填项"))?;
    let reason = args
        .get("reason")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| tool_error("INVALID_ARGUMENT", "reason 是必填项"))?;
    let evidence = string_list(args.get("evidence"))?.unwrap_or_default();
    let recovery = ctx
        .harness
        .resolve_recovery(task_id, recovery_id, reason, &evidence)
        .map_err(map_error)?;
    let task = ctx.harness.task(task_id).map_err(map_error)?;
    Ok(json!({
        "task_id": task_id,
        "recovery": recovery,
        "task": task_view(&task)
    }))
}

fn visible_outbox_recovery(
    name: &str,
    args: &Value,
    value: &Value,
    recovered: Vec<Value>,
) -> Vec<Value> {
    if name == "project_state" {
        return recovered;
    }
    let selected_task_id = args
        .get("task_id")
        .and_then(Value::as_str)
        .or_else(|| value.get("task_id").and_then(Value::as_str))
        .or_else(|| value.pointer("/task/id").and_then(Value::as_str))
        .or_else(|| {
            value
                .pointer("/work_session/task_id")
                .and_then(Value::as_str)
        });
    let Some(selected_task_id) = selected_task_id else {
        return Vec::new();
    };
    recovered
        .into_iter()
        .filter(|entry| entry.get("task_id").and_then(Value::as_str) == Some(selected_task_id))
        .collect()
}

fn git_lines(root: &Path, args: &[&str]) -> Vec<String> {
    let mut command = Command::new("git");
    crate::platform::hide_std_console(&mut command);
    let Ok(output) = command.arg("-C").arg(root).args(args).output() else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect()
}

fn cleanup_closed_task_worktree(
    ctx: &ToolContext,
    task_id: &str,
    source_tool: &str,
) -> Result<Value, WorkspaceError> {
    let task = ctx.harness.task(task_id).map_err(map_error)?;
    let Some(worktree) = task.git_worktree.as_ref() else {
        return Ok(json!({"requested": false, "removed": false}));
    };
    let worktree_exists = Path::new(&worktree.path).exists();
    if !worktree.remove_on_close {
        return Ok(json!({
            "requested": false,
            "removed": !worktree_exists,
            "already_absent": !worktree_exists,
            "preserved": worktree_exists,
            "path": worktree.path,
            "branch": worktree.branch
        }));
    }
    if !worktree_exists {
        return Ok(json!({
            "requested": true,
            "removed": true,
            "idempotent": true,
            "already_absent": true,
            "path": worktree.path,
            "branch": worktree.branch
        }));
    }
    let primary_workspace = if ctx.is_primary_workspace() {
        None
    } else {
        Some(
            Workspace::new(ctx.primary_workspace_root().to_path_buf())?
                .with_strict_read_boundary(ctx.workspace.strict_read_boundary()),
        )
    };
    let workspace = primary_workspace.as_ref().unwrap_or(&ctx.workspace);
    crate::tools::git::remove_managed_task_worktree(workspace, worktree)?;
    let _ = ctx.harness.record_event(
        task_id,
        "git_worktree_removed",
        Some(source_tool),
        json!({"path": worktree.path, "branch": worktree.branch}),
        json!({"ok": true}),
    );
    Ok(json!({
        "requested": true,
        "removed": true,
        "idempotent": false,
        "path": worktree.path,
        "branch": worktree.branch
    }))
}

fn finish_task_worktree_cleanup(ctx: &ToolContext, task_id: &str) -> Value {
    match cleanup_closed_task_worktree(ctx, task_id, "finish_task") {
        Ok(result) => result,
        Err(error) => {
            let error_value = error.to_error_value();
            let _ = ctx.harness.record_event(
                task_id,
                "git_worktree_cleanup_pending",
                Some("finish_task"),
                json!({}),
                json!({
                    "ok": false,
                    "error": error_value.clone(),
                    "next_actions": ["git_worktree_list", "git_worktree_remove"]
                }),
            );
            json!({
                "requested": true,
                "removed": false,
                "pending": true,
                "error": error_value,
                "next_actions": ["git_worktree_list", "git_worktree_remove"]
            })
        }
    }
}

#[cfg(test)]
mod response_shape_tests {
    use super::*;

    #[test]
    fn peer_outbox_recovery_is_hidden_from_task_scoped_responses() {
        let recovered = vec![
            json!({"status": "checkpoint_pending", "task_id": "task-a"}),
            json!({"status": "prepared", "task_id": "task-b"}),
            json!({"status": "prepared", "task_id": null}),
        ];
        let visible = visible_outbox_recovery(
            "operation_log",
            &json!({"task_id": "task-a"}),
            &json!({"task_id": "task-a"}),
            recovered.clone(),
        );
        assert_eq!(visible, vec![recovered[0].clone()]);
        assert_eq!(
            visible_outbox_recovery("project_state", &json!({}), &json!({}), recovered.clone()),
            recovered
        );
        assert!(visible_outbox_recovery(
            "operation_log",
            &json!({}),
            &json!({}),
            vec![json!({"status": "prepared", "task_id": null})]
        )
        .is_empty());
    }
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

    let scoped = ctx
        .scoped_for_task(&task, session_id)
        .map_err(|message| tool_error("TASK_WORKTREE_UNAVAILABLE", message))?;
    let task_context = scoped.as_ref().unwrap_or(ctx);
    let summary = change_summary(
        task_context,
        &json!({"task_id": task.id, "limit": 1024, "verification_view": "all"}),
        session_id,
    )?;
    let verifications = ctx
        .harness
        .list_verifications(&task.id)
        .map_err(map_error)?;
    let git = crate::tools::git::git_status(
        &task_context.workspace,
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
            "execution_path": task_context.workspace.root().display().to_string(),
            "mode": task_workspace_mode(&task),
            "workspace_id": task.workspace_id,
            "git": git
        },
        "session": {
            "session_id": task.session_id,
            "path": task.session_path
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
            "note": "Import this JSON as source evidence, then create a fresh isolated local Session and Harness Task instead of injecting private storage files."
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
    ensure_writer_handoff_available(ctx, Some(target_task_id), None)?;
    let task = ctx.harness.switch_task(target_task_id).map_err(map_error)?;
    ctx.bind_task_for_session(session_id, &task.id)
        .map_err(|error| tool_error("TASK_BIND_FAILED", error))?;
    let parallel = task.git_worktree.is_some();
    Ok(json!({
        "task": task_view(&task),
        "session_task_id": task.id,
        "parallel_tasks_preserved": true,
        "workspace_mode": task_workspace_mode(&task),
        "writer_mode": if parallel { "isolated_worktree" } else { "single_shared_writer" },
        "git_worktree": task.git_worktree
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

fn verification_at_or_after(record: &VerificationRecord, not_before: Option<&str>) -> bool {
    let Some(not_before) = not_before else {
        return true;
    };
    match (
        record.created_at.parse::<u128>(),
        not_before.parse::<u128>(),
    ) {
        (Ok(record_time), Ok(minimum_time)) => record_time >= minimum_time,
        _ => false,
    }
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
                "tool": "skill",
                "args": {"operation": "get"},
                "description": "通过公开 skill facade 重新读取该 Skill 的受控资源清单。"
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
    ensure_writer_handoff_available(ctx, Some(target_task_id), None)?;
    let task = ctx.harness.switch_task(target_task_id).map_err(map_error)?;
    ctx.bind_task_for_session(session_id, &task.id)
        .map_err(|error| tool_error("TASK_BIND_FAILED", error))?;
    let scoped = ctx
        .scoped_for_task(&task, session_id)
        .map_err(|message| tool_error("TASK_WORKTREE_UNAVAILABLE", message))?;
    let status_context = scoped.as_ref().unwrap_or(ctx);
    let parallel = task.git_worktree.is_some();
    Ok(json!({
        "task": task_view(&task),
        "session_task_id": task.id,
        "parallel_tasks_preserved": true,
        "workspace_mode": task_workspace_mode(&task),
        "writer_mode": if parallel { "isolated_worktree" } else { "single_shared_writer" },
        "git_worktree": task.git_worktree,
        "harness": status_context.harness.status_for_task(Some(&task.id)).map_err(map_error)?
    }))
}

fn ensure_writer_handoff_available(
    ctx: &ToolContext,
    target_task_id: Option<&str>,
    requested_worktree_path: Option<&str>,
) -> Result<(), WorkspaceError> {
    let target_domain = if let Some(task_id) = target_task_id {
        let task = ctx.harness.task(task_id).map_err(map_error)?;
        task.git_worktree
            .map(|worktree| worktree.path)
            .unwrap_or_else(|| "shared".to_string())
    } else {
        requested_worktree_path
            .map(str::to_string)
            .unwrap_or_else(|| "shared".to_string())
    };
    let blocking_task_ids = ctx
        .sessions
        .running_task_ids()
        .into_iter()
        .filter(|task_id| Some(task_id.as_str()) != target_task_id)
        .filter(|task_id| {
            ctx.harness
                .task(task_id)
                .map(|task| {
                    task.git_worktree
                        .map(|worktree| worktree.path)
                        .unwrap_or_else(|| "shared".to_string())
                        == target_domain
                })
                .unwrap_or(true)
        })
        .collect::<Vec<_>>();
    if blocking_task_ids.is_empty() {
        return Ok(());
    }
    Err(WorkspaceError::ToolDetails {
        code: "WORKSPACE_WRITER_BUSY",
        message: "Another task still owns a running command in this write domain.".into(),
        category: "conflict",
        retryable: true,
        details: json!({
            "blocking_task_ids": blocking_task_ids,
            "write_domain": target_domain,
            "suggestion": "Wait for or stop the running command before transferring this writer lease"
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
    let task_recovery = if disposition != "active_failure" {
        let task = ctx.harness.task(task_id).map_err(map_error)?;
        let matching_step = task.recovery.as_ref().and_then(|recovery| {
            (recovery.status == TaskRecoveryStatus::Open
                && recovery.related_verification_id.as_deref() == Some(verification_id))
            .then(|| {
                (
                    recovery.failed_step.clone(),
                    recovery.step_fingerprint.clone(),
                )
            })
        });
        if let Some((step, fingerprint)) = matching_step {
            ctx.harness
                .resolve_recovery_for_step(task_id, &step, fingerprint.as_deref())
                .map_err(map_error)?
        } else {
            None
        }
    } else {
        None
    };
    let records = ctx.harness.list_verifications(task_id).map_err(map_error)?;
    Ok(json!({
        "verification": verification_view(&verification),
        "verification_status": verification_status(&records),
        "effective_disposition": effective_disposition(&verification),
        "task_recovery": task_recovery,
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
    let configuration = parse_task_configuration(args)?;
    let completed_steps = string_list(args.get("completed_steps"))?;
    let pending_steps = string_list(args.get("pending_steps"))?;
    let create_if_missing = args
        .get("create_if_missing")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let objective_revision_requested = args
        .get("objective_revision")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let session_state = crate::tools::session::open(ctx, args)?;
    let session_id = session_state
        .get("session_id")
        .and_then(Value::as_str)
        .ok_or_else(|| tool_error("SESSION_INVALID", "Session 缺少 session_id"))?;
    let session_path = session_state
        .get("session_path")
        .and_then(Value::as_str)
        .ok_or_else(|| tool_error("SESSION_INVALID", "Session 缺少 session_path"))?;

    let tasks = ctx.harness.list_tasks().map_err(map_error)?;
    let requested_task_id = args
        .get("task_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let requested_task = requested_task_id
        .map(|task_id| {
            tasks
                .iter()
                .find(|task| task.id == task_id && task.status.is_writable())
                .cloned()
                .ok_or_else(|| WorkspaceError::ToolDetails {
                    code: "WORK_SESSION_TASK_NOT_FOUND",
                    message:
                        "The requested durable Harness Task does not exist or is not writable."
                            .into(),
                    category: "not_found",
                    retryable: false,
                    details: json!({"task_id": task_id}),
                })
        })
        .transpose()?;
    let explicit_task = ctx.bound_task_for_session(mcp_session_id).filter(|task| {
        task.session_id.as_deref() == Some(session_id)
            && task.session_path.as_deref() == Some(session_path)
    });
    let writable_session_tasks = tasks
        .iter()
        .filter(|task| {
            task.status.is_writable()
                && task.session_id.as_deref() == Some(session_id)
                && task.session_path.as_deref() == Some(session_path)
        })
        .cloned()
        .collect::<Vec<_>>();
    let session_task = writable_session_tasks
        .iter()
        .find(|task| task.objective == objective)
        .cloned();
    let unique_session_task = (writable_session_tasks.len() == 1)
        .then(|| writable_session_tasks.first().cloned())
        .flatten();
    let existing_session_tasks = tasks
        .iter()
        .filter(|task| {
            task.session_id.as_deref() == Some(session_id)
                && task.session_path.as_deref() == Some(session_path)
        })
        .map(|task| {
            json!({
                "task_id": task.id,
                "objective": task.objective,
                "status": task.status,
                "phase": task.phase,
                "workspace_mode": task_workspace_mode(task),
                "git_worktree": task.git_worktree.as_ref().map(|worktree| &worktree.path)
            })
        })
        .collect::<Vec<_>>();
    let selected_task = requested_task
        .or(session_task)
        .or(explicit_task)
        .or(unique_session_task);
    let mut objective_revised_from = None::<String>;
    let (task, task_created, previous_task_id) = match selected_task {
        Some(task) => {
            validate_requested_workspace_mode(ctx, args, &task)?;
            if task.objective != objective {
                if objective_revision_requested {
                    ensure_writer_handoff_available(ctx, Some(&task.id), None)?;
                    let previous_objective = task.objective.clone();
                    ctx.harness.switch_task(&task.id).map_err(map_error)?;
                    let task = ctx
                        .harness
                        .revise_objective(&task.id, objective)
                        .map_err(map_error)?;
                    objective_revised_from = Some(previous_objective);
                    (task, false, None)
                } else if requested_task_id.is_some() {
                    return Err(WorkspaceError::ToolDetails {
                        code: "WORK_SESSION_TASK_CONFLICT",
                        message: "The requested durable Harness Task has a different objective."
                            .into(),
                        category: "conflict",
                        retryable: true,
                        details: json!({
                            "task_id": task.id,
                            "requested_objective": objective,
                            "existing_objective": task.objective,
                            "cause_scope": "task_objective",
                            "workspace_mutated": false,
                            "recommended_retry": {
                                "tool": "begin_work_session",
                                "arguments": {
                                    "task_id": task.id,
                                    "objective": objective,
                                    "objective_revision": true,
                                    "create_if_missing": false
                                }
                            }
                        }),
                    });
                } else if !create_if_missing {
                    return Err(WorkspaceError::ToolDetails {
                        code: "WORK_SESSION_TASK_CONFLICT",
                        message: "The existing Session is bound to a different writable Harness Task, and create_if_missing=false forbids creating a replacement Task/worktree.".into(),
                        category: "conflict",
                        retryable: true,
                        details: json!({
                            "session_id": session_id,
                            "session_path": session_path,
                            "requested_objective": objective,
                            "existing_task_id": task.id,
                            "existing_objective": task.objective,
                            "existing_status": task.status,
                            "existing_phase": task.phase,
                            "existing_workspace_mode": task_workspace_mode(&task),
                            "create_if_missing": false,
                            "cause_scope": "task_objective",
                            "workspace_mutated": false,
                            "recommended_retry": {
                                "tool": "begin_work_session",
                                "arguments": {
                                    "task_id": task.id,
                                    "objective": objective,
                                    "objective_revision": true,
                                    "create_if_missing": false
                                }
                            }
                        }),
                    });
                } else {
                    let previous_task_id = task.id.clone();
                    let next = start_task_for_workspace_mode(ctx, objective, args)?;
                    (next, true, Some(previous_task_id))
                }
            } else {
                ensure_writer_handoff_available(ctx, Some(&task.id), None)?;
                let task = ctx.harness.switch_task(&task.id).map_err(map_error)?;
                (task, false, None)
            }
        }
        None => {
            if !create_if_missing {
                return Err(WorkspaceError::ToolDetails {
                    code: "WORK_SESSION_TASK_NOT_FOUND",
                    message: "The Session exists, but no matching writable Harness Task is available and create_if_missing=false forbids creating a new Task/worktree.".into(),
                    category: "not_found",
                    retryable: false,
                    details: json!({
                        "session_id": session_id,
                        "session_path": session_path,
                        "requested_objective": objective,
                        "create_if_missing": false,
                        "existing_session_tasks": existing_session_tasks,
                        "suggestion": "Inspect the existing terminal task directly, or set create_if_missing=true only when a genuinely new Task/worktree is intended."
                    }),
                });
            }
            (
                start_task_for_workspace_mode(ctx, objective, args)?,
                true,
                None,
            )
        }
    };
    let session_changed = task
        .session_id
        .as_deref()
        .is_some_and(|value| value != session_id)
        || task
            .session_path
            .as_deref()
            .is_some_and(|value| value != session_path);
    let mut task = if session_changed {
        let reclaim = args
            .get("reclaim_session")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if !reclaim {
            return Err(WorkspaceError::ToolDetails {
                code: "WORK_SESSION_RECLAIM_REQUIRED",
                message: "The durable Harness Task is leased to another Session. Explicit reclaim is required before rebinding it.".into(),
                category: "conflict",
                retryable: true,
                details: json!({
                    "task_id": task.id,
                    "current_session_id": task.session_id,
                    "requested_session_id": session_id,
                    "required_action": "Retry begin_work_session with task_id, reclaim_session=true, and expected_head after confirming the prior client no longer owns a running command.",
                    "cause_scope": "task_lease",
                    "workspace_mutated": false,
                    "recommended_retry": {
                        "tool": "begin_work_session",
                        "arguments": {
                            "task_id": task.id,
                            "objective": objective,
                            "session_id": session_id,
                            "create_if_missing": false,
                            "reclaim_session": true,
                            "expected_head": task.expected_state.head
                        }
                    }
                }),
            });
        }
        ensure_session_reclaim_safe(ctx, &task, args)?;
        ctx.harness
            .reclaim_session(&task.id, session_id, session_path)
            .map_err(map_error)?
    } else {
        ctx.harness
            .bind_session(&task.id, session_id, session_path)
            .map_err(map_error)?
    };
    if task_created && !configuration.is_empty() {
        task = ctx
            .harness
            .configure_task(
                &task.id,
                configuration.phase,
                configuration.contract,
                configuration.slices,
                configuration.working_set,
            )
            .map_err(map_error)?;
    }
    if task_created && (completed_steps.is_some() || pending_steps.is_some()) {
        task = ctx
            .harness
            .update_steps(&task.id, completed_steps, pending_steps)
            .map_err(map_error)?;
    }
    let auto_paused_previous_task_ids = if task_created && task.git_worktree.is_none() {
        let running_task_ids = ctx.sessions.running_task_ids();
        ctx.harness
            .list_tasks()
            .map_err(map_error)?
            .into_iter()
            .filter(|previous| previous.id != task.id)
            .filter(|previous| previous.git_worktree.is_none())
            .filter(|previous| {
                previous.session_id.as_deref() == Some(session_id)
                    && previous.session_path.as_deref() == Some(session_path)
            })
            .filter(|previous| {
                matches!(previous.status, TaskStatus::Active | TaskStatus::Verifying)
                    && !running_task_ids
                        .iter()
                        .any(|task_id| task_id == &previous.id)
            })
            .filter_map(|previous| {
                ctx.harness
                    .transition(&previous.id, TaskStatus::Paused)
                    .ok()
                    .map(|_| previous.id)
            })
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    ctx.bind_task_for_session(mcp_session_id, &task.id)
        .map_err(|error| tool_error("TASK_BIND_FAILED", error))?;
    let scoped = ctx
        .scoped_for_task(&task, mcp_session_id)
        .map_err(|message| tool_error("TASK_WORKTREE_UNAVAILABLE", message))?;
    let status_context = scoped.as_ref().unwrap_or(ctx);
    let harness = status_context
        .harness
        .status_for_task(Some(&task.id))
        .map_err(map_error)?;
    let previous_session_status = session_state
        .get("previous_status")
        .cloned()
        .unwrap_or_else(|| json!("active"));
    let current_session_status = session_state
        .get("session_status")
        .cloned()
        .unwrap_or_else(|| json!("active"));
    let session_reactivated = session_state
        .get("reactivated")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    Ok(json!({
        "work_session": {
            "status": "active",
            "session_id": session_id,
            "session_path": session_path,
            "task_id": task.id,
            "task_created": task_created,
            "previous_task_id": previous_task_id,
            "objective_revised": objective_revised_from.is_some(),
            "previous_objective": objective_revised_from,
            "auto_paused_previous_task_ids": auto_paused_previous_task_ids,
            "parallel": task.git_worktree.is_some(),
            "workspace_mode": task_workspace_mode(&task),
            "worktree_reused": task_created && args.get("worktree_path").is_some(),
            "writer_mode": if task.git_worktree.is_some() { "isolated_worktree" } else { "single_shared_writer" },
            "git_worktree": task.git_worktree,
            "baseline": baseline_view(&task),
            "expected_state": task.expected_state
        },
        "session": compact_session_view(&session_state),
        "session_state_transition": {
            "from": previous_session_status,
            "to": current_session_status,
            "changed": session_reactivated,
            "reason": if session_reactivated { "begin_work_session" } else { "already_active" }
        },
        "state_scopes": {
            "session_lease": {
                "status": session_state.get("session_status").cloned().unwrap_or(Value::Null),
                "reactivated": session_reactivated
            },
            "harness_task": {
                "status": task.status,
                "phase": task.phase
            },
            "checkpoint": {
                "count": session_state.get("checkpoint_count").cloned().unwrap_or(Value::Null),
                "session_lifecycle_status": session_state.get("session_status").cloned().unwrap_or(Value::Null)
            }
        },
        "task": task_view(&task),
        "harness": harness,
        "reconnect_required": false
    }))
}

fn compact_session_view(session: &Value) -> Value {
    json!({
        "session_id": session.get("session_id").cloned().unwrap_or(Value::Null),
        "session_path": session.get("session_path").cloned().unwrap_or(Value::Null),
        "created": session.get("created").cloned().unwrap_or(Value::Bool(false)),
        "resumed": session.get("resumed").cloned().unwrap_or(Value::Bool(false)),
        "previous_status": session.get("previous_status").cloned().unwrap_or(Value::Null),
        "reactivated": session.get("reactivated").cloned().unwrap_or(Value::Bool(false)),
        "session_status": session.get("session_status").cloned().unwrap_or(Value::Null),
        "checkpoint_count": session.get("checkpoint_count").cloned().unwrap_or(Value::Null),
        "history_injected": session.get("history_injected").cloned().unwrap_or(Value::Bool(false)),
        "persistence": session.get("persistence").cloned().unwrap_or(Value::Null),
        "warnings": session.get("warnings").cloned().unwrap_or_else(|| json!([]))
    })
}

fn complete_work_session(ctx: &ToolContext, args: &Value) -> Result<Value, WorkspaceError> {
    let task_id = task_id(args)?;
    let detail = args
        .get("detail")
        .and_then(Value::as_str)
        .unwrap_or("compact");
    if let Some(mut outbox) = ctx.harness.load_close_outbox(task_id).map_err(map_error)? {
        if parse_close_outcome(&outbox.finish_args)? == "incomplete" {
            return Err(tool_error(
                "WORK_SESSION_ALREADY_ABORTING",
                "该 Work Session 已开始按 incomplete outcome 终止，不能改写为 completed",
            ));
        }
        if outbox.phase == WorkSessionClosePhase::Prepared {
            let mut strict = args.clone();
            strict["task_id"] = Value::String(task_id.to_string());
            strict["allow_unverified"] = Value::Bool(false);
            strict["session_status"] = Value::String("completed".into());
            strict["outcome"] = Value::String("completed".into());
            strict["_completion_via_work_session"] = Value::Bool(true);
            outbox.finish_args = strict;
            outbox.session_status = HarnessSessionStatus::Completed;
            if let Some(checkpoint) = args.get("checkpoint").and_then(Value::as_object) {
                if let Some(target) = outbox.checkpoint_args.as_object_mut() {
                    for (key, value) in checkpoint {
                        target.insert(key.clone(), value.clone());
                    }
                }
            }
            if let Some(summary) = args.get("summary").and_then(Value::as_str) {
                outbox.checkpoint_args["notes"] = Value::String(summary.to_string());
            }
            outbox.checkpoint_args["session_status"] = Value::String("completed".into());
            outbox.last_error = None;
            outbox.updated_at = harness_timestamp();
            ctx.harness.save_close_outbox(&outbox).map_err(map_error)?;
        }
        return resume_close_outbox(ctx, outbox, true)
            .map(|value| present_complete_work_session(value, detail));
    }
    let mut strict = args.clone();
    strict["allow_unverified"] = Value::Bool(false);
    strict["session_status"] = Value::String("completed".into());
    strict["outcome"] = Value::String("completed".into());
    strict["_completion_via_work_session"] = Value::Bool(true);
    close_work_session(ctx, &strict).map(|value| present_complete_work_session(value, detail))
}

fn present_complete_work_session(value: Value, detail: &str) -> Value {
    if detail == "full" || value.get("ok").and_then(Value::as_bool) != Some(true) {
        return value;
    }
    json!({
        "ok": true,
        "detail": "compact",
        "work_session": value.get("work_session").cloned().unwrap_or(Value::Null),
        "finish": value.get("finish").map(compact_finish_value).unwrap_or(Value::Null),
        "checkpoint": value.get("checkpoint").cloned().unwrap_or(Value::Null),
        "worktree_cleanup": value.get("worktree_cleanup").cloned().unwrap_or(Value::Null),
        "outbox": value.get("outbox").cloned().unwrap_or(Value::Null),
        "task": value.get("task").map(compact_task_value).unwrap_or(Value::Null),
        "harness": value.get("harness").map(compact_harness_value).unwrap_or(Value::Null)
    })
}

fn compact_finish_value(value: &Value) -> Value {
    let completion_gate = value.get("completion_gate").map(|gate| {
        json!({
            "ready": gate.get("ready").cloned().unwrap_or(Value::Null),
            "missing_count": gate.get("missing").and_then(Value::as_array).map(Vec::len).unwrap_or(0),
            "next_actions": gate.get("next_actions").cloned().unwrap_or_else(|| json!([]))
        })
    });
    json!({
        "ok": value.get("ok").cloned().unwrap_or(Value::Bool(true)),
        "closed": value.get("closed").cloned().unwrap_or(Value::Bool(false)),
        "task_status": value.get("task_status").cloned().unwrap_or(Value::Null),
        "verification_status": value.get("verification_status").cloned().unwrap_or(Value::Null),
        "session_status": value.get("session_status").cloned().unwrap_or(Value::Null),
        "requested_session_status": value.get("requested_session_status").cloned().unwrap_or(Value::Null),
        "reconciled_phases": value.get("reconciled_phases").cloned().unwrap_or_else(|| json!([])),
        "completion_gate": completion_gate.unwrap_or(Value::Null),
        "worktree_cleanup": value.get("worktree_cleanup").cloned().unwrap_or(Value::Null),
        "truncated": value.get("truncated").cloned().unwrap_or(Value::Bool(false)),
        "details_tool": value.get("details_tool").cloned().unwrap_or_else(|| json!({"name": "change_summary"}))
    })
}

fn compact_task_value(value: &Value) -> Value {
    json!({
        "id": value.get("id").cloned().unwrap_or(Value::Null),
        "status": value.get("status").cloned().unwrap_or(Value::Null),
        "phase": value.get("phase").cloned().unwrap_or(Value::Null),
        "session_id": value.get("session_id").cloned().unwrap_or(Value::Null),
        "workspace_mode": value.get("workspace_mode").cloned().unwrap_or(Value::Null),
        "current_slice_id": value.get("current_slice_id").cloned().unwrap_or(Value::Null),
        "latest_change_id": value.get("latest_change_id").cloned().unwrap_or(Value::Null),
        "latest_verification_id": value.get("latest_verification_id").cloned().unwrap_or(Value::Null),
        "pending_step_count": value.get("pending_steps").and_then(Value::as_array).map(Vec::len).unwrap_or(0),
        "slice_count": value.get("slices").and_then(Value::as_array).map(Vec::len).unwrap_or(0),
        "recovery_status": value.pointer("/recovery/status").cloned().unwrap_or(Value::Null)
    })
}

fn compact_harness_value(value: &Value) -> Value {
    json!({
        "workspace_id": value.get("workspace_id").cloned().unwrap_or(Value::Null),
        "task_id": value.get("task_id").cloned().unwrap_or(Value::Null),
        "task_state": value.get("task_state").cloned().unwrap_or(Value::Null),
        "session_status": value.get("session_status").cloned().unwrap_or(Value::Null),
        "active_task_count": value.get("active_task_count").cloned().unwrap_or(Value::Null),
        "branch": value.get("branch").cloned().unwrap_or(Value::Null),
        "head": value.get("head").cloned().unwrap_or(Value::Null),
        "next_actions": value.get("next_actions").cloned().unwrap_or_else(|| json!([])),
        "reason": value.get("reason").cloned().unwrap_or(Value::Null),
        "recoverable": value.get("recoverable").cloned().unwrap_or(Value::Null)
    })
}

fn close_work_session(ctx: &ToolContext, args: &Value) -> Result<Value, WorkspaceError> {
    let task_id = task_id(args)?;
    if let Some(outbox) = ctx.harness.load_close_outbox(task_id).map_err(map_error)? {
        return resume_close_outbox(ctx, outbox, true);
    }
    let task_before = ctx.harness.task(task_id).map_err(map_error)?;
    let outcome = parse_close_outcome(args)?;
    if outcome == "completed" && task_before.status == TaskStatus::Incomplete {
        return Err(tool_error(
            "WORK_SESSION_TASK_INCOMPLETE",
            "Task 已以 incomplete 终态关闭；请使用 close_work_session outcome=incomplete 完成 Session checkpoint，不能改写为 completed",
        ));
    }
    if outcome == "incomplete" {
        args.get("reason")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                tool_error(
                    "INVALID_ARGUMENT",
                    "close_work_session outcome=incomplete 必须提供 reason",
                )
            })?;
        if !task_before.status.is_writable() && task_before.status != TaskStatus::Incomplete {
            return Err(tool_error(
                "WORK_SESSION_TASK_ALREADY_CLOSED",
                "Task 已以其他终态关闭，不能改写为 incomplete",
            ));
        }
    }
    let session_id = task_before
        .session_id
        .clone()
        .ok_or_else(|| tool_error("WORK_SESSION_NOT_BOUND", "任务未绑定 Session"))?;
    let expected_path = task_before
        .session_path
        .clone()
        .ok_or_else(|| tool_error("WORK_SESSION_NOT_BOUND", "任务未绑定 Session 路径"))?;
    let session_status = parse_session_status(args)?;
    if outcome == "incomplete" && session_status == HarnessSessionStatus::Completed {
        return Err(tool_error(
            "INVALID_ARGUMENT",
            "outcome=incomplete 时 session_status 只能是 active 或 paused",
        ));
    }
    let mut checkpoint = args
        .get("checkpoint")
        .cloned()
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}));
    checkpoint["session_id"] = Value::String(session_id.clone());
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
    finish_args["outcome"] = Value::String(outcome.to_string());
    let now = harness_timestamp();
    let outbox = WorkSessionCloseOutbox {
        schema_version: SCHEMA_VERSION,
        task_id: task_id.to_string(),
        session_id,
        session_path: expected_path,
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
    let outcome = parse_close_outcome(&outbox.finish_args)?;
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
            "session_id": outbox.session_id,
            "path": outbox.session_path,
            "idempotent_retry": true,
            "outbox_completed": true
        }));
    }
    if outbox.phase == WorkSessionClosePhase::Prepared {
        let task_before = ctx.harness.task(&outbox.task_id).map_err(map_error)?;
        let scoped = ctx
            .scoped_for_task(&task_before, None)
            .map_err(|message| tool_error("TASK_WORKTREE_UNAVAILABLE", message))?;
        let finish_context = scoped.as_ref().unwrap_or(ctx);
        let result = if task_before.status.is_writable() {
            if outcome == "incomplete" {
                abort_task(finish_context, &outbox.finish_args)?
            } else {
                finish_task(finish_context, &outbox.finish_args)?
            }
        } else if outcome == "incomplete" && task_before.status == TaskStatus::Incomplete {
            json!({
                "ok": true,
                "task_status": "incomplete",
                "outcome": "incomplete",
                "closed": true,
                "session_status": harness_session_status_text(outbox.session_status),
                "requested_session_status": harness_session_status_text(outbox.session_status),
                "reason": task_before.termination.as_ref().map(|termination| termination.reason.clone()),
                "task": task_view(&task_before),
                "idempotent_retry": true
            })
        } else if outcome == "completed"
            && matches!(
                task_before.status,
                TaskStatus::Completed | TaskStatus::CompletedUnverified
            )
        {
            json!({
                "ok": true,
                "task_status": task_before.status,
                "outcome": "completed",
                "closed": true,
                "session_status": harness_session_status_text(outbox.session_status),
                "next_stage_started": false,
                "task": task_view(&task_before),
                "idempotent_retry": true
            })
        } else {
            return Err(tool_error(
                "WORK_SESSION_OUTCOME_CONFLICT",
                "Task 终态与 durable close outcome 不一致，拒绝改写历史结果",
            ));
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
            let (error_code, error_message) = if outcome == "incomplete" {
                (
                    "WORK_SESSION_ABORT_BLOCKED",
                    "Harness task could not be aborted because retained command results are still pending.",
                )
            } else {
                (
                    "WORK_SESSION_VERIFICATION_BLOCKED",
                    "Harness task could not be closed because verification or working-tree requirements are not satisfied.",
                )
            };
            let suggestion = if outcome == "incomplete" {
                "Terminate any running retained command, consume every terminal result, then retry the same incomplete close."
            } else {
                "Run the suggested verification action, or use update_verification_disposition for an audited false positive/expected failure."
            };
            return Ok(json!({
                "ok": false,
                "closed": false,
                "phase": "finish_task",
                "finish": result,
                "checkpoint": null,
                "retryable": true,
                "error": {
                    "code": error_code,
                    "message": error_message,
                    "category": "validation",
                    "retryable": true,
                    "details": {
                        "blocking_verifications": blocking,
                        "suggestion": suggestion
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
        match crate::tools::session::checkpoint(ctx, &outbox.checkpoint_args, None) {
            Ok(result) => {
                checkpoint = Some(result);
                outbox.phase = WorkSessionClosePhase::Completed;
                outbox.last_error = None;
                outbox.updated_at = harness_timestamp();
                ctx.harness.save_close_outbox(&outbox).map_err(map_error)?;
            }
            Err(error) => {
                outbox.phase = WorkSessionClosePhase::CheckpointPending;
                let cause = error.to_error_value();
                outbox.last_error = Some(json!({
                    "phase": "session_checkpoint",
                    "cause": cause.clone()
                }));
                outbox.updated_at = harness_timestamp();
                ctx.harness.save_close_outbox(&outbox).map_err(map_error)?;
                if propagate_checkpoint_error {
                    return Ok(json!({
                        "ok": false,
                        "closed": false,
                        "phase": "session_checkpoint",
                        "finish": finish,
                        "checkpoint": null,
                        "retryable": true,
                        "error": {
                            "code": "WORK_SESSION_CHECKPOINT_PENDING",
                            "message": error.message(),
                            "category": "runtime",
                            "retryable": true,
                            "details": {
                                "phase": "session_checkpoint",
                                "task_closed": true,
                                "task_id": outbox.task_id,
                                "session_id": outbox.session_id,
                                "expected_path": outbox.session_path,
                                "outbox": close_outbox_view(&outbox),
                                "suggestion": "Checkpoint intent is durable and will be retried automatically on the next Harness call.",
                                "cause": cause
                            }
                        },
                        "outbox": close_outbox_view(&outbox)
                    }));
                }
            }
        }
    }

    let completed = outbox.phase == WorkSessionClosePhase::Completed;
    let worktree_cleanup = if completed && outcome == "completed" {
        cleanup_closed_task_worktree(ctx, &outbox.task_id, "close_work_session")?
    } else if completed {
        json!({
            "requested": false,
            "removed": false,
            "preserved": true,
            "reason": "incomplete_task"
        })
    } else {
        Value::Null
    };
    let task = ctx.harness.task(&outbox.task_id).map_err(map_error)?;
    Ok(json!({
        "ok": completed,
        "work_session": {
            "status": harness_session_status_text(outbox.session_status),
            "session_id": outbox.session_id,
            "session_path": outbox.session_path,
            "task_id": outbox.task_id,
            "task_status": task.status,
            "outcome": outcome,
            "closed": completed,
            "next_stage_started": false
        },
        "finish": finish,
        "checkpoint": checkpoint,
        "worktree_cleanup": worktree_cleanup,
        "outbox": close_outbox_view(&outbox),
        "task": task_view(&task),
        "harness": ctx.harness.status().map_err(map_error)?
    }))
}

fn close_outbox_view(outbox: &WorkSessionCloseOutbox) -> Value {
    json!({
        "schema_version": outbox.schema_version,
        "task_id": outbox.task_id,
        "outcome": outbox.finish_args.get("outcome").and_then(Value::as_str).unwrap_or("completed"),
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
    let selected_task_id = selected.as_ref().map(|task| task.id.clone());
    let mut value = serde_json::to_value(
        ctx.harness
            .status_for_task(selected.as_ref().map(|task| task.id.as_str()))
            .map_err(map_error)?,
    )
    .map_err(|e| tool_error("SERIALIZE_FAILED", e.to_string()))?;
    if let Some(object) = value.as_object_mut() {
        let (running, pending_terminal) = selected_task_id
            .as_deref()
            .map(|task_id| ctx.sessions.pending_for_task(task_id, 512))
            .unwrap_or_default();
        let current_operation = running.first().map(compact_command_heartbeat);
        object.insert(
            "current_operation".into(),
            current_operation.unwrap_or(Value::Null),
        );
        object.insert("running_command_count".into(), json!(running.len()));
        object.insert(
            "pending_terminal_command_count".into(),
            json!(pending_terminal.len()),
        );
    }
    Ok(value)
}

fn compact_command_heartbeat(snapshot: &Value) -> Value {
    json!({
        "kind": "command",
        "session_id": snapshot.get("session_id").cloned().unwrap_or(Value::Null),
        "command": snapshot.get("command").cloned().unwrap_or(Value::Null),
        "execution_status": snapshot.get("execution_status").cloned().unwrap_or_else(|| json!("running")),
        "started_at": snapshot.get("started_at").cloned().unwrap_or(Value::Null),
        "elapsed_ms": snapshot.get("execution_duration_ms").or_else(|| snapshot.get("elapsed_ms")).cloned().unwrap_or(Value::Null),
        "last_output_at": snapshot.get("last_output_at").cloned().unwrap_or(Value::Null),
        "waiting_reason": "command_running",
        "next_milestone": "terminal_command_result"
    })
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
    let history_filter = optional_text(args, "session_id");
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
            && matches_optional(operation, "session_id", history_filter.as_deref())
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
            "session_id": history_filter,
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
    let command_session_id = result
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
        "session_id": operation.session_id,
        "mcp_session_id": operation.mcp_session_id,
        "command_session_id": command_session_id,
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
    let command_session_id = result
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
        "session_id": final_record.session_id.as_ref().or(started.session_id.as_ref()),
        "mcp_session_id": final_record.mcp_session_id.as_ref().or(started.mcp_session_id.as_ref()),
        "command_session_id": command_session_id,
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
    let configuration = parse_task_configuration(args)?;
    let completed_steps = string_list(args.get("completed_steps"))?;
    let pending_steps = string_list(args.get("pending_steps"))?;
    let mut task = start_task_for_workspace_mode(ctx, objective, args)?;
    if !configuration.is_empty() {
        task = ctx
            .harness
            .configure_task(
                &task.id,
                configuration.phase,
                configuration.contract,
                configuration.slices,
                configuration.working_set,
            )
            .map_err(map_error)?;
    }
    if completed_steps.is_some() || pending_steps.is_some() {
        task = ctx
            .harness
            .update_steps(&task.id, completed_steps, pending_steps)
            .map_err(map_error)?;
    }
    ctx.bind_task_for_session(session_id, &task.id)
        .map_err(|error| tool_error("TASK_BIND_FAILED", error))?;
    Ok(json!({
        "task": task_view(&task),
        "session_task_id": task.id,
        "parallel": task.git_worktree.is_some(),
        "workspace_mode": task_workspace_mode(&task),
        "worktree_reused": task.git_worktree.is_some() && args.get("worktree_path").is_some(),
        "writer_mode": if task.git_worktree.is_some() { "isolated_worktree" } else { "single_shared_writer" },
        "git_worktree": task.git_worktree,
        "next": ["project_state", "task_context"]
    }))
}

fn start_task_for_workspace_mode(
    ctx: &ToolContext,
    objective: &str,
    args: &Value,
) -> Result<TaskSession, WorkspaceError> {
    let mode = args
        .get("workspace_mode")
        .and_then(Value::as_str)
        .unwrap_or("shared");
    match mode {
        "shared" => {
            if args.get("worktree_path").is_some() {
                return Err(tool_error(
                    "INVALID_ARGUMENT",
                    "worktree_path is valid only when workspace_mode=worktree",
                ));
            }
            ctx.harness.start_task(objective).map_err(map_error)
        }
        "worktree" => {
            let task_id = Uuid::new_v4().simple().to_string();
            let existing_path = args
                .get("worktree_path")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|path| !path.is_empty());
            let branch = args.get("worktree_branch").and_then(Value::as_str);
            let remove_on_close = args
                .get("worktree_remove_on_close")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let (worktree, created_now) = if let Some(path) = existing_path {
                if branch.is_some() || args.get("worktree_base_ref").is_some() {
                    return Err(tool_error(
                        "INVALID_ARGUMENT",
                        "worktree_path cannot be combined with worktree_branch or worktree_base_ref",
                    ));
                }
                let worktree = crate::tools::git::resolve_existing_managed_worktree(
                    &ctx.workspace,
                    path,
                    remove_on_close,
                )?;
                ensure_existing_managed_worktree_attachable(ctx, &worktree.path)?;
                (worktree, false)
            } else {
                let base_ref = args
                    .get("worktree_base_ref")
                    .and_then(Value::as_str)
                    .unwrap_or("HEAD");
                let worktree = crate::tools::git::create_managed_worktree(
                    &ctx.workspace,
                    &task_id,
                    branch,
                    base_ref,
                    remove_on_close,
                )?;
                (worktree, true)
            };
            if let Err(error) = ensure_writer_handoff_available(ctx, None, Some(&worktree.path)) {
                if created_now {
                    let _ =
                        crate::tools::git::remove_managed_task_worktree(&ctx.workspace, &worktree);
                }
                return Err(error);
            }
            match ctx
                .harness
                .start_task_in_git_worktree(objective, task_id, worktree.clone())
            {
                Ok(task) => Ok(task),
                Err(error) => {
                    if created_now {
                        let _ = crate::tools::git::remove_managed_task_worktree(
                            &ctx.workspace,
                            &worktree,
                        );
                    }
                    Err(map_error(error))
                }
            }
        }
        _ => Err(tool_error(
            "INVALID_ARGUMENT",
            "workspace_mode must be shared or worktree",
        )),
    }
}

fn ensure_existing_managed_worktree_attachable(
    ctx: &ToolContext,
    raw_path: &str,
) -> Result<(), WorkspaceError> {
    let target = crate::tools::git::managed_worktree_path(&ctx.workspace, raw_path)?;
    let blocking_tasks = ctx
        .harness
        .list_tasks()
        .map_err(map_error)?
        .into_iter()
        .filter(|task| task.status.is_writable())
        .filter(|task| {
            task.git_worktree
                .as_ref()
                .filter(|worktree| worktree.managed)
                .map(|worktree| {
                    let candidate = std::path::PathBuf::from(&worktree.path);
                    candidate.canonicalize().unwrap_or(candidate) == target
                })
                .unwrap_or(false)
        })
        .map(|task| {
            json!({
                "task_id": task.id,
                "status": task.status,
                "phase": task.phase,
                "objective": task.objective,
                "session_id": task.session_id
            })
        })
        .collect::<Vec<_>>();
    if blocking_tasks.is_empty() {
        return Ok(());
    }
    let blocking_task_ids = blocking_tasks
        .iter()
        .filter_map(|task| task.get("task_id").and_then(Value::as_str))
        .collect::<Vec<_>>();
    Err(WorkspaceError::ToolDetails {
        code: "GIT_WORKTREE_IN_USE",
        message: "The managed worktree is still attached to a writable Harness Task.".into(),
        category: "conflict",
        retryable: true,
        details: json!({
            "path": raw_path,
            "blocking_task_ids": blocking_task_ids,
            "blocking_tasks": blocking_tasks,
            "required_action": "Complete or abort the existing writable task before binding this managed worktree to a new task."
        }),
    })
}

fn validate_requested_workspace_mode(
    ctx: &ToolContext,
    args: &Value,
    task: &TaskSession,
) -> Result<(), WorkspaceError> {
    let Some(requested) = args.get("workspace_mode").and_then(Value::as_str) else {
        return Ok(());
    };
    if requested != task_workspace_mode(task) {
        return Err(WorkspaceError::ToolDetails {
            code: "TASK_WORKSPACE_MODE_CONFLICT",
            message: "The existing task uses a different workspace mode.".into(),
            category: "conflict",
            retryable: false,
            details: json!({
                "requested_workspace_mode": requested,
                "task_workspace_mode": task_workspace_mode(task),
                "task_id": task.id
            }),
        });
    }
    if let Some(raw_path) = args
        .get("worktree_path")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|path| !path.is_empty())
    {
        let requested_path = crate::tools::git::managed_worktree_path(&ctx.workspace, raw_path)?;
        let Some(worktree) = task.git_worktree.as_ref() else {
            return Err(WorkspaceError::ToolDetails {
                code: "TASK_WORKTREE_PATH_CONFLICT",
                message: "The existing task is not attached to a Git worktree.".into(),
                category: "conflict",
                retryable: false,
                details: json!({
                    "requested_worktree_path": raw_path,
                    "task_id": task.id,
                    "task_workspace_mode": task_workspace_mode(task)
                }),
            });
        };
        let current = std::path::PathBuf::from(&worktree.path);
        let current = current.canonicalize().unwrap_or(current);
        if requested_path != current {
            return Err(WorkspaceError::ToolDetails {
                code: "TASK_WORKTREE_PATH_CONFLICT",
                message: "The existing task is attached to a different managed worktree.".into(),
                category: "conflict",
                retryable: false,
                details: json!({
                    "requested_worktree_path": requested_path,
                    "task_worktree_path": current,
                    "task_id": task.id
                }),
            });
        }
    }
    Ok(())
}

fn task_workspace_mode(task: &TaskSession) -> &'static str {
    if task.git_worktree.is_some() {
        "worktree"
    } else {
        "shared"
    }
}

fn update_task(ctx: &ToolContext, args: &Value) -> Result<Value, WorkspaceError> {
    let task_id = task_id(args)?;
    let objective = args
        .get("objective")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let completed_steps = string_list(args.get("completed_steps"))?;
    let pending_steps = string_list(args.get("pending_steps"))?;
    let configuration = parse_task_configuration(args)?;
    let mut task = if let Some(objective) = objective {
        ctx.harness
            .revise_objective(task_id, objective)
            .map_err(map_error)?
    } else {
        ctx.harness.task(task_id).map_err(map_error)?
    };
    if completed_steps.is_some() || pending_steps.is_some() {
        task = ctx
            .harness
            .update_steps(task_id, completed_steps, pending_steps)
            .map_err(map_error)?;
    }
    if !configuration.is_empty() {
        task = ctx
            .harness
            .configure_task(
                task_id,
                configuration.phase,
                configuration.contract,
                configuration.slices,
                configuration.working_set,
            )
            .map_err(map_error)?;
    }
    Ok(json!({"task": task_view(&task)}))
}

fn task_gate_status(
    ctx: &ToolContext,
    args: &Value,
    session_id: Option<&str>,
) -> Result<Value, WorkspaceError> {
    let task = if let Some(task_id) = args.get("task_id").and_then(Value::as_str) {
        ctx.harness.task(task_id).map_err(map_error)?
    } else {
        ctx.task_for_session(session_id)
            .ok_or_else(|| tool_error("TASK_STATE_REQUIRED", "当前会话未绑定任务"))?
    };
    let verifications = ctx
        .harness
        .list_verifications(&task.id)
        .map_err(map_error)?;
    let completion_gate = completion_gate_value(ctx, &task, &verifications, false, false);
    let detail = args
        .get("detail")
        .and_then(Value::as_str)
        .unwrap_or("compact");
    let verification_summary = verification_presentation_summary(&verifications);
    if detail == "full" {
        return Ok(json!({
            "task_id": task.id,
            "ready": completion_gate["ready"],
            "detail": "full",
            "completion_gate": completion_gate,
            "task": task_view(&task),
            "verification": verification_views(&verifications, "effective"),
            "verification_summary": verification_summary
        }));
    }
    let missing = completion_gate["missing"]
        .as_array()
        .into_iter()
        .flatten()
        .map(compact_gate_missing_item)
        .collect::<Vec<_>>();
    let blocking_failures = blocking_verification_views(&verifications)
        .into_iter()
        .map(|verification| {
            json!({
                "verification_id": verification.get("verification_id").cloned().unwrap_or(Value::Null),
                "verification_kind": verification.get("verification_kind").cloned().unwrap_or(Value::Null),
                "verification_key": verification.get("verification_key").cloned().unwrap_or(Value::Null),
                "test_file": verification.get("test_file").cloned().unwrap_or(Value::Null),
                "test_name": verification.get("test_name").cloned().unwrap_or(Value::Null)
            })
        })
        .collect::<Vec<_>>();
    let current_slice = task.current_slice_id.as_deref().and_then(|slice_id| {
        task.slices
            .iter()
            .find(|slice| slice.id == slice_id)
            .map(|slice| json!({"id": slice.id, "title": slice.title, "status": slice.status}))
    });
    let blocking_recovery = task
        .recovery
        .as_ref()
        .filter(|recovery| recovery.blocks_completion())
        .map(|recovery| {
            json!({
                "id": recovery.id,
                "failed_step": recovery.failed_step,
                "error_code": recovery.error_code,
                "recovery_key": recovery.id
            })
        });
    Ok(json!({
        "task_id": task.id,
        "ready": completion_gate["ready"],
        "detail": "compact",
        "completion_gate": {
            "ready": completion_gate["ready"],
            "task_id": task.id,
            "phase": task.phase,
            "missing": missing,
            "next_actions": completion_gate["next_actions"],
            "current_slice": current_slice,
            "blocking_failures": blocking_failures,
            "running_session_count": completion_gate["running_sessions"].as_array().map_or(0, Vec::len),
            "unobserved_terminal_session_count": completion_gate["unobserved_terminal_sessions"].as_array().map_or(0, Vec::len),
            "recovery": blocking_recovery
        },
        "current_slice": current_slice,
        "blocking_failures": blocking_failures,
        "verification_summary": verification_summary
    }))
}

fn compact_gate_missing_item(item: &Value) -> Value {
    let mut compact = serde_json::Map::new();
    compact.insert(
        "code".into(),
        item.get("code").cloned().unwrap_or(Value::Null),
    );
    for key in ["verification_status", "slice_id", "phase"] {
        if let Some(value) = item.get(key) {
            compact.insert(key.to_string(), value.clone());
        }
    }
    if let Some(requirement) = item.get("requirement") {
        compact.insert(
            "requirement_id".into(),
            requirement.get("id").cloned().unwrap_or(Value::Null),
        );
    }
    Value::Object(compact)
}

fn start_slice(ctx: &ToolContext, args: &Value) -> Result<Value, WorkspaceError> {
    let task_id = task_id(args)?;
    let now = harness_timestamp();
    let slice = parse_task_slice(
        &json!({
            "id": args.get("slice_id").cloned().unwrap_or(Value::Null),
            "title": args.get("title").cloned().unwrap_or(Value::Null),
            "status": "in_progress",
            "files": args.get("files").cloned().unwrap_or_else(|| json!([])),
            "acceptance_checks": args
                .get("acceptance_checks")
                .cloned()
                .unwrap_or_else(|| json!([]))
        }),
        TaskSliceStatus::InProgress,
        &now,
    )?;
    let slice_id = slice.id.clone();
    let task = ctx.harness.start_slice(task_id, slice).map_err(map_error)?;
    let slice = task
        .slices
        .iter()
        .find(|slice| slice.id == slice_id)
        .cloned();
    Ok(json!({
        "task_id": task_id,
        "slice": slice,
        "task": task_view(&task),
        "progress_event": {
            "slice": slice_id,
            "from": "planned",
            "to": "in_progress"
        }
    }))
}

fn update_slice(ctx: &ToolContext, args: &Value) -> Result<Value, WorkspaceError> {
    let task_id = task_id(args)?;
    let slice_id = required_bounded_string(args.get("slice_id"), "slice_id", 128)?;
    let status = args
        .get("status")
        .map(parse_slice_status_value)
        .transpose()?;
    let title = args
        .get("title")
        .map(|value| required_bounded_string(Some(value), "title", 500))
        .transpose()?;
    let files = parse_string_array(args.get("files"), 256, 2_000, "files")?;
    let acceptance_checks = parse_requirements_value(args.get("acceptance_checks"))?;
    let commit_sha = args
        .get("commit_sha")
        .map(|value| optional_bounded_string(Some(value), "commit_sha", 128))
        .transpose()?;
    let blocker = args
        .get("blocker")
        .map(|value| optional_bounded_string(Some(value), "blocker", 2_000))
        .transpose()?;
    let task = ctx
        .harness
        .update_slice(
            task_id,
            &slice_id,
            status,
            title,
            files,
            acceptance_checks,
            commit_sha,
            blocker,
        )
        .map_err(map_error)?;
    let slice = task
        .slices
        .iter()
        .find(|slice| slice.id == slice_id)
        .cloned();
    Ok(json!({
        "task_id": task_id,
        "slice": slice,
        "task": task_view(&task),
        "progress_event": {
            "slice": slice_id,
            "to": status
        }
    }))
}

fn complete_slice(ctx: &ToolContext, args: &Value) -> Result<Value, WorkspaceError> {
    let task_id = task_id(args)?;
    let slice_id = required_bounded_string(args.get("slice_id"), "slice_id", 128)?;
    let commit_sha = optional_bounded_string(args.get("commit_sha"), "commit_sha", 128)?;
    let task_before = ctx.harness.task(task_id).map_err(map_error)?;
    let slice = task_before
        .slices
        .iter()
        .find(|slice| slice.id == slice_id)
        .ok_or_else(|| tool_error("SLICE_NOT_FOUND", format!("Slice not found: {slice_id}")))?;
    let verifications = ctx.harness.list_verifications(task_id).map_err(map_error)?;
    let effective_commit = commit_sha.clone().or_else(|| slice.commit_sha.clone());
    let mut missing = Vec::new();
    if slice.status != TaskSliceStatus::Verifying {
        missing.push(json!({
            "code": "slice_not_verifying",
            "slice_id": slice_id,
            "status": slice.status
        }));
    }
    if let Some(blocker) = slice
        .blocker
        .as_ref()
        .filter(|value| !value.trim().is_empty())
    {
        missing.push(json!({
            "code": "slice_blocked",
            "slice_id": slice_id,
            "blocker": blocker
        }));
    }
    let acceptance = requirement_outcomes(
        &slice.acceptance_checks,
        &verifications,
        Some(&slice.created_at),
    );
    for outcome in &acceptance {
        if outcome.get("satisfied") != Some(&Value::Bool(true)) {
            missing.push(json!({
                "code": "slice_acceptance_missing",
                "slice_id": slice_id,
                "requirement": outcome
            }));
        }
    }
    if task_before.contract.completion_policy.require_slice_commits && effective_commit.is_none() {
        missing.push(json!({
            "code": "slice_commit_missing",
            "slice_id": slice_id
        }));
    }
    if !missing.is_empty() {
        return Ok(json!({
            "ok": false,
            "completed": false,
            "task_id": task_id,
            "slice_id": slice_id,
            "missing": missing,
            "acceptance": acceptance,
            "next_actions": ["exec_command", "update_slice", "complete_slice"],
            "error": {
                "code": "SLICE_COMPLETION_GATE_FAILED",
                "message": "Slice acceptance gate is not satisfied.",
                "category": "validation",
                "retryable": true,
                "details": {"slice_id": slice_id}
            },
            "task": task_view(&task_before)
        }));
    }
    let task = ctx
        .harness
        .complete_slice(task_id, &slice_id, effective_commit)
        .map_err(map_error)?;
    let completed_slice = task
        .slices
        .iter()
        .find(|slice| slice.id == slice_id)
        .cloned();
    Ok(json!({
        "completed": true,
        "task_id": task_id,
        "slice_id": slice_id,
        "slice": completed_slice,
        "acceptance": acceptance,
        "task": task_view(&task),
        "progress_event": {
            "slice": slice_id,
            "from": "verifying",
            "to": "completed",
            "evidence": {"commit": completed_slice.as_ref().and_then(|slice| slice.commit_sha.clone())}
        }
    }))
}

fn completion_gate_value(
    ctx: &ToolContext,
    task: &TaskSession,
    verifications: &[VerificationRecord],
    allow_unverified: bool,
    completion_via_work_session: bool,
) -> Value {
    let (running_sessions, unobserved_terminal_sessions) =
        ctx.sessions.pending_for_task(&task.id, 2_048);
    let mut working_tree_files = git_working_tree_files(ctx.workspace.root());
    working_tree_files.retain(|path| !is_runtime_artifact(path));
    let (task_working_tree_files, peer_working_tree_files, unattributed_working_tree_files) =
        classify_working_tree_ownership(ctx, &task.id, &working_tree_files);
    let mut blocking_working_tree_files = task_working_tree_files.clone();
    blocking_working_tree_files.extend(unattributed_working_tree_files.clone());
    blocking_working_tree_files.sort();
    blocking_working_tree_files.dedup();

    let mut missing = Vec::<Value>::new();
    let mut next_actions = BTreeSet::<String>::new();
    if !running_sessions.is_empty() || !unobserved_terminal_sessions.is_empty() {
        missing.push(json!({
            "code": "command_results_pending",
            "running_sessions": running_sessions,
            "unobserved_terminal_sessions": unobserved_terminal_sessions
        }));
        next_actions.extend(
            ["list_command_sessions", "wait_command", "kill_session"]
                .into_iter()
                .map(str::to_string),
        );
    }
    if !blocking_working_tree_files.is_empty() {
        missing.push(json!({
            "code": "worktree_not_clean",
            "files": blocking_working_tree_files
        }));
        next_actions.extend(
            ["git_status", "stage_commit"]
                .into_iter()
                .map(str::to_string),
        );
    }

    let verification_status = verification_status(verifications);
    let policy = &task.contract.completion_policy;
    let generic_verification_required = !allow_unverified || policy.disallow_unverified_completion;
    if generic_verification_required && !verification_status_is_accepted(verification_status) {
        missing.push(json!({
            "code": if verification_status == "missing" { "verification_missing" } else { "verification_failed" },
            "verification_status": verification_status,
            "blocking_verifications": blocking_verification_views(verifications)
        }));
        next_actions.insert("exec_command".into());
    }

    if policy.require_pending_steps_empty && !task.pending_steps.is_empty() {
        missing.push(json!({
            "code": "pending_steps_remaining",
            "pending_steps": bounded_strings(&task.pending_steps, 64, 1_000)
        }));
        next_actions.insert("update_task".into());
    }
    let incomplete_slices = task
        .slices
        .iter()
        .filter(|slice| slice.status != TaskSliceStatus::Completed)
        .map(|slice| json!({"id": slice.id, "title": slice.title, "status": slice.status}))
        .collect::<Vec<_>>();
    if policy.require_all_slices_completed && !incomplete_slices.is_empty() {
        missing.push(json!({
            "code": "slices_incomplete",
            "slices": incomplete_slices
        }));
        next_actions.extend(
            ["start_slice", "update_slice", "complete_slice"]
                .into_iter()
                .map(str::to_string),
        );
    }
    let missing_slice_commits = task
        .slices
        .iter()
        .filter(|slice| {
            slice.status == TaskSliceStatus::Completed && slice.commit_sha.as_deref().is_none()
        })
        .map(|slice| slice.id.clone())
        .collect::<Vec<_>>();
    if policy.require_slice_commits && !missing_slice_commits.is_empty() {
        missing.push(json!({
            "code": "slice_commits_missing",
            "slice_ids": missing_slice_commits
        }));
        next_actions.insert("update_slice".into());
    }
    if policy.require_no_open_recovery
        && task
            .recovery
            .as_ref()
            .is_some_and(|recovery| recovery.blocks_completion())
    {
        missing.push(json!({
            "code": "recovery_open",
            "recovery": task.recovery
        }));
        if let Some(step) = task
            .recovery
            .as_ref()
            .map(|recovery| recovery.failed_step.clone())
        {
            next_actions.insert(step);
        }
    }
    let task_already_completed = task.status == TaskStatus::Completed;
    if policy.require_ready_to_close
        && task.phase != TaskPhase::ReadyToClose
        && !task_already_completed
    {
        missing.push(json!({
            "code": "ready_to_close_phase_missing",
            "phase": task.phase
        }));
        next_actions.insert("update_task".into());
    }
    if policy.require_complete_work_session
        && !completion_via_work_session
        && !task_already_completed
    {
        missing.push(json!({
            "code": "complete_work_session_required"
        }));
        next_actions.insert("complete_work_session".into());
    }

    let required_verifications = requirement_outcomes(
        &task.contract.required_verifications,
        verifications,
        Some(&task.created_at),
    );
    for outcome in &required_verifications {
        if outcome.get("satisfied") != Some(&Value::Bool(true)) {
            missing.push(json!({
                "code": "required_verification_missing",
                "requirement": outcome
            }));
            next_actions.insert("exec_command".into());
        }
    }
    let mut slice_acceptance = Vec::new();
    for slice in &task.slices {
        let outcomes = requirement_outcomes(
            &slice.acceptance_checks,
            verifications,
            Some(&slice.created_at),
        );
        for outcome in &outcomes {
            if outcome.get("satisfied") != Some(&Value::Bool(true)) {
                missing.push(json!({
                    "code": "slice_acceptance_missing",
                    "slice_id": slice.id,
                    "requirement": outcome
                }));
                next_actions.insert("exec_command".into());
            }
        }
        slice_acceptance.push(json!({
            "slice_id": slice.id,
            "checks": outcomes
        }));
    }

    json!({
        "ready": missing.is_empty(),
        "task_id": task.id,
        "phase": task.phase,
        "no_early_stop": task.contract.no_early_stop,
        "completion_policy": task.contract.completion_policy,
        "missing": missing,
        "next_actions": next_actions.into_iter().collect::<Vec<_>>(),
        "verification_status": verification_status,
        "required_verifications": required_verifications,
        "slice_acceptance": slice_acceptance,
        "pending_steps": bounded_strings(&task.pending_steps, 64, 1_000),
        "running_sessions": running_sessions,
        "unobserved_terminal_sessions": unobserved_terminal_sessions,
        "working_tree_files": blocking_working_tree_files,
        "task_working_tree_files": task_working_tree_files,
        "unattributed_working_tree_files": unattributed_working_tree_files,
        "peer_working_tree_files": peer_working_tree_files,
        "recovery": task.recovery
    })
}

fn requirement_outcomes(
    requirements: &[VerificationRequirement],
    verifications: &[VerificationRecord],
    not_before: Option<&str>,
) -> Vec<Value> {
    let effective = effective_verifications(verifications);
    requirements
        .iter()
        .map(|requirement| {
            let matched = effective.iter().copied().find(|record| {
                verification_at_or_after(record, not_before)
                    && requirement
                        .kind
                        .as_deref()
                        .is_none_or(|value| record.kind == value)
                    && requirement
                        .verification_key
                        .as_deref()
                        .is_none_or(|value| record.verification_key.as_deref() == Some(value))
                    && requirement
                        .test_file
                        .as_deref()
                        .is_none_or(|value| record.test_file.as_deref() == Some(value))
                    && requirement
                        .test_name
                        .as_deref()
                        .is_none_or(|value| record.test_name.as_deref() == Some(value))
                    && effective_disposition(record) != "active_failure"
            });
            json!({
                "id": requirement.id,
                "description": requirement.description,
                "kind": requirement.kind,
                "verification_key": requirement.verification_key,
                "test_file": requirement.test_file,
                "test_name": requirement.test_name,
                "satisfied": matched.is_some(),
                "matched_verification_id": matched.map(|record| record.id.clone()),
                "matched_disposition": matched.map(effective_disposition)
            })
        })
        .collect()
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

fn abort_task(ctx: &ToolContext, args: &Value) -> Result<Value, WorkspaceError> {
    let task_id = task_id(args)?;
    let reason = args
        .get("reason")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| tool_error("INVALID_ARGUMENT", "abort_task 必须提供 reason"))?;
    let requested_session_status = parse_session_status(args)?;
    if requested_session_status == HarnessSessionStatus::Completed {
        return Err(tool_error(
            "INVALID_ARGUMENT",
            "未完成任务终止后 session_status 只能是 active 或 paused",
        ));
    }
    let task_before = ctx.harness.task(task_id).map_err(map_error)?;
    let verifications = ctx.harness.list_verifications(task_id).map_err(map_error)?;
    let completion_gate = completion_gate_value(ctx, &task_before, &verifications, false, false);
    let running_sessions = completion_gate["running_sessions"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let unobserved_terminal_sessions = completion_gate["unobserved_terminal_sessions"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if !running_sessions.is_empty() || !unobserved_terminal_sessions.is_empty() {
        return Ok(json!({
            "ok": false,
            "task_status": task_before.status,
            "outcome": "incomplete",
            "closed": false,
            "session_status": ctx.harness.status().map_err(map_error)?.session_status,
            "requested_session_status": requested_session_status,
            "reason": "任务仍有运行中或尚未消费终态的 retained command；请先终止/读取结果，再执行 abort。",
            "running_sessions": running_sessions,
            "unobserved_terminal_sessions": unobserved_terminal_sessions,
            "completion_gate": completion_gate,
            "task": task_view(&task_before),
            "error": {
                "code": "TASK_ABORT_COMMAND_RESULTS_PENDING",
                "message": "Retained commands must be terminated or consumed before aborting the task.",
                "category": "validation",
                "retryable": true,
                "details": {
                    "running_sessions": running_sessions,
                    "unobserved_terminal_sessions": unobserved_terminal_sessions
                }
            }
        }));
    }
    let task = ctx
        .harness
        .abort_task(task_id, reason, requested_session_status)
        .map_err(map_error)?;
    let workspace_session_status = ctx.harness.status().map_err(map_error)?.session_status;
    let stored_reason = task
        .termination
        .as_ref()
        .map(|termination| termination.reason.as_str())
        .unwrap_or(reason);
    Ok(json!({
        "ok": true,
        "task_status": "incomplete",
        "outcome": "incomplete",
        "closed": true,
        "session_status": workspace_session_status,
        "requested_session_status": requested_session_status,
        "reason": stored_reason,
        "completion_gate": completion_gate,
        "task": task_view(&task),
        "worktree_cleanup": {
            "requested": false,
            "removed": false,
            "preserved": true,
            "reason": "incomplete_task"
        }
    }))
}

fn finish_task(ctx: &ToolContext, args: &Value) -> Result<Value, WorkspaceError> {
    let task_id = task_id(args)?;
    let allow_unverified = args
        .get("allow_unverified")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let completion_via_work_session = args
        .get("_completion_via_work_session")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let verifications = ctx.harness.list_verifications(task_id).map_err(map_error)?;
    let task_before = ctx.harness.task(task_id).map_err(map_error)?;
    let (task_before, reconciled_phases) = reconcile_completion_phase(
        ctx,
        task_before,
        &verifications,
        allow_unverified,
        completion_via_work_session,
    )?;
    let verification_status = verification_status(&verifications);
    let completion_gate = completion_gate_value(
        ctx,
        &task_before,
        &verifications,
        allow_unverified,
        completion_via_work_session,
    );
    let command_results_pending = completion_gate["missing"]
        .as_array()
        .is_some_and(|missing| {
            missing
                .iter()
                .any(|item| item["code"] == "command_results_pending")
        });
    if !command_results_pending {
        ctx.harness.check_baseline(task_id).map_err(map_error)?;
    }
    if completion_gate["ready"] != Value::Bool(true) {
        let missing_codes = completion_gate["missing"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|item| item.get("code").and_then(Value::as_str))
            .collect::<Vec<_>>();
        let closure_protocol_only = !missing_codes.is_empty()
            && missing_codes
                .iter()
                .all(|code| *code == "complete_work_session_required");
        let task = if closure_protocol_only {
            task_before.clone()
        } else {
            ctx.harness.mark_verifying(task_id).map_err(map_error)?
        };
        let (code, reason, message) = if missing_codes.contains(&"command_results_pending") {
            (
                "TASK_COMMAND_RESULTS_PENDING",
                "当前任务仍有运行中或终态尚未消费的 retained command；必须先读取最终结果或显式终止后才能关闭任务。",
                "Retained command results must be consumed before task completion.",
            )
        } else if missing_codes.contains(&"worktree_not_clean") {
            (
                "TASK_WORKTREE_NOT_CLEAN",
                "当前任务仍拥有未提交或无法归属的业务文件；提交或还原这些改动后才能关闭任务。",
                "Workspace contains uncommitted business files.",
            )
        } else if missing_codes.contains(&"verification_missing") {
            (
                "TASK_VERIFICATION_MISSING",
                "任务缺少结构化验证证据；请使用 exec_command.verification_kind 或 stage_commit.required_checks 运行验证。",
                "Structured verification evidence is missing.",
            )
        } else if missing_codes.contains(&"verification_failed") {
            (
                "TASK_VERIFICATION_FAILED",
                "至少一项结构化验证失败；修复后重新运行验证并再次调用 finish_task。",
                "At least one structured verification is still failing.",
            )
        } else if missing_codes.contains(&"complete_work_session_required") {
            (
                "TASK_COMPLETE_WORK_SESSION_REQUIRED",
                "任务契约要求通过 complete_work_session 完成最终检查点和会话关闭。",
                "The task contract requires complete_work_session.",
            )
        } else {
            (
                "TASK_COMPLETION_GATE_FAILED",
                "任务完成门禁未通过；必须处理所有缺失验收项后才能声明完成。",
                "The task completion gate is not satisfied.",
            )
        };
        return Ok(json!({
            "ok": false,
            "task_status": task.status,
            "verification_status": verification_status,
            "closed": false,
            "session_status": "active",
            "next_stage_started": false,
            "reconciled_phases": reconciled_phases,
            "reason": reason,
            "error": {
                "code": code,
                "message": message,
                "category": "validation",
                "retryable": true,
                "details": {
                    "blocking_verifications": blocking_verification_views(&verifications),
                    "completion_gate": completion_gate
                }
            },
            "blocking_verifications": blocking_verification_views(&verifications),
            "working_tree_files": completion_gate["working_tree_files"],
            "task_working_tree_files": completion_gate["task_working_tree_files"],
            "unattributed_working_tree_files": completion_gate["unattributed_working_tree_files"],
            "peer_working_tree_files": completion_gate["peer_working_tree_files"],
            "running_sessions": completion_gate["running_sessions"],
            "unobserved_terminal_sessions": completion_gate["unobserved_terminal_sessions"],
            "next_actions": completion_gate["next_actions"],
            "completion_gate": completion_gate,
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
    let worktree_cleanup = finish_task_worktree_cleanup(ctx, task_id);
    let workspace_session_status = ctx.harness.status().map_err(map_error)?.session_status;
    let mut response = json!({
        "ok": true,
        "task_status": if verified { "completed" } else { "completed_unverified" },
        "verification_status": if verified { verification_status } else { "unverified" },
        "closed": true,
        "session_status": workspace_session_status,
        "requested_session_status": session_status,
        "next_stage_started": false,
        "reconciled_phases": reconciled_phases,
        "completion_gate": completion_gate,
        "task": task_view(&task),
        "worktree_cleanup": worktree_cleanup,
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

fn reconcile_completion_phase(
    ctx: &ToolContext,
    mut task: TaskSession,
    verifications: &[VerificationRecord],
    allow_unverified: bool,
    completion_via_work_session: bool,
) -> Result<(TaskSession, Vec<String>), WorkspaceError> {
    if !completion_via_work_session
        || !task.contract.completion_policy.require_ready_to_close
        || task.phase == TaskPhase::ReadyToClose
        || !task.status.is_writable()
    {
        return Ok((task, Vec::new()));
    }
    let gate = completion_gate_value(
        ctx,
        &task,
        verifications,
        allow_unverified,
        completion_via_work_session,
    );
    let only_phase_missing = gate["missing"].as_array().is_some_and(|missing| {
        !missing.is_empty()
            && missing.iter().all(|item| {
                item.get("code").and_then(Value::as_str) == Some("ready_to_close_phase_missing")
            })
    });
    if !only_phase_missing {
        return Ok((task, Vec::new()));
    }
    let phases = match task.phase {
        TaskPhase::Unspecified | TaskPhase::Planning | TaskPhase::Implementing => vec![
            TaskPhase::Verifying,
            TaskPhase::Cleanup,
            TaskPhase::ReadyToClose,
        ],
        TaskPhase::Deploying => vec![
            TaskPhase::Verifying,
            TaskPhase::Cleanup,
            TaskPhase::ReadyToClose,
        ],
        TaskPhase::BrowserReview => vec![TaskPhase::Cleanup, TaskPhase::ReadyToClose],
        TaskPhase::Verifying => vec![TaskPhase::Cleanup, TaskPhase::ReadyToClose],
        TaskPhase::Cleanup => vec![TaskPhase::ReadyToClose],
        TaskPhase::ReadyToClose
        | TaskPhase::Completed
        | TaskPhase::Aborted
        | TaskPhase::Blocked
        | TaskPhase::Paused => Vec::new(),
    };
    let mut reconciled = Vec::new();
    for phase in phases {
        task = ctx
            .harness
            .configure_task(&task.id, Some(phase), None, None, None)
            .map_err(map_error)?;
        if let Ok(Value::String(label)) = serde_json::to_value(phase) {
            reconciled.push(label);
        }
    }
    Ok((task, reconciled))
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
    let verifications = ctx
        .harness
        .list_verifications(&task.id)
        .map_err(map_error)?;
    let completion_gate = completion_gate_value(ctx, &task, &verifications, false, false);
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
        "completion_gate": completion_gate,
        "verification": verification_views(&verifications, "effective"),
        "verification_summary": verification_presentation_summary(&verifications),
        "events": bounded_events,
        "truncated": truncated,
        "next_cursor": if truncated { Some(bounded_events.len()) } else { None },
        "max_bytes": max_bytes
    }))
}

fn list_task_events(ctx: &ToolContext, args: &Value) -> Result<Value, WorkspaceError> {
    let task_id = task_id(args)?;
    let detail = args
        .get("detail")
        .and_then(Value::as_str)
        .unwrap_or("compact");
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
    let event_count = events.len();
    let events = if detail == "full" {
        events
            .iter()
            .map(|event| serde_json::to_value(event).unwrap_or(Value::Null))
            .collect::<Vec<_>>()
    } else {
        events.iter().map(compact_event).collect::<Vec<_>>()
    };
    Ok(json!({
        "events": events,
        "detail": detail,
        "next_cursor": offset + event_count
    }))
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
    let fallback_commit_shas = if selected_changes.is_empty() {
        if let Some(change_id) = requested_change_id {
            git_lines(
                ctx.workspace.root(),
                &[
                    "rev-parse",
                    "--verify",
                    format!("{change_id}^{{commit}}").as_str(),
                ],
            )
        } else {
            match (
                task.baseline.head.as_deref(),
                task.expected_state.head.as_deref(),
            ) {
                (Some(start), Some(end)) if start != end => {
                    let range = format!("{start}..{end}");
                    git_lines(
                        ctx.workspace.root(),
                        &["rev-list", "--reverse", range.as_str()],
                    )
                }
                _ => Vec::new(),
            }
        }
    } else {
        Vec::new()
    };
    let commits = if selected_changes.is_empty() {
        fallback_commit_shas
            .iter()
            .map(|commit| {
                let files = git_paths(
                    ctx.workspace.root(),
                    &[
                        "diff-tree",
                        "--no-commit-id",
                        "--name-only",
                        "-r",
                        "-z",
                        commit,
                    ],
                );
                json!({
                    "change_id": commit,
                    "commit_sha": commit,
                    "created_at": null,
                    "committed_files": files,
                    "verification_ids": [],
                    "source": "git_commit_range_fallback"
                })
            })
            .collect::<Vec<_>>()
    } else {
        selected_changes
            .iter()
            .map(|change| {
                json!({
                    "change_id": change.id,
                    "commit_sha": change.commit_sha,
                    "created_at": change.created_at,
                    "committed_files": change.committed_files,
                    "verification_ids": change.verification_ids,
                    "source": "harness_change_set"
                })
            })
            .collect::<Vec<_>>()
    };
    let commit_sha = commits
        .last()
        .and_then(|commit| commit.get("commit_sha"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| requested_change_id.map(str::to_string));
    let mut committed_file_set = BTreeSet::new();
    for commit in &commits {
        if let Some(files) = commit.get("committed_files").and_then(Value::as_array) {
            committed_file_set.extend(files.iter().filter_map(Value::as_str).map(str::to_string));
        }
    }
    let committed_files = committed_file_set.into_iter().collect::<Vec<_>>();
    let files_by_commit = commits
        .iter()
        .map(|commit| {
            json!({
                "commit_sha": commit.get("commit_sha"),
                "change_id": commit.get("change_id"),
                "files": commit.get("committed_files")
            })
        })
        .collect::<Vec<_>>();
    let first_commit = commits
        .first()
        .and_then(|commit| commit.get("commit_sha"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let last_commit = commits
        .last()
        .and_then(|commit| commit.get("commit_sha"))
        .and_then(Value::as_str)
        .map(str::to_string);
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
        "rollback_capability": if commits.is_empty() { "not_available" } else { "git_commit_range" },
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

fn parse_close_outcome(args: &Value) -> Result<&str, WorkspaceError> {
    match args
        .get("outcome")
        .and_then(Value::as_str)
        .unwrap_or("completed")
    {
        "completed" => Ok("completed"),
        "incomplete" => Ok("incomplete"),
        _ => Err(tool_error(
            "INVALID_ARGUMENT",
            "outcome 仅支持 completed 或 incomplete",
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
        "phase": task.phase,
        "contract": task.contract,
        "slices": task.slices,
        "current_slice_id": task.current_slice_id,
        "working_set": task.working_set,
        "recovery": task.recovery,
        "termination": task.termination,
        "baseline": baseline_view(task),
        "expected_state": task.expected_state,
        "completed_steps": bounded_strings(&task.completed_steps, 64, 1_000),
        "pending_steps": bounded_strings(&task.pending_steps, 64, 1_000),
        "latest_change_id": task.latest_change_id,
        "latest_verification_id": task.latest_verification_id,
        "session_id": task.session_id,
        "session_path": task.session_path,
        "workspace_mode": task_workspace_mode(task),
        "git_worktree": task.git_worktree,
        "created_at": task.created_at,
        "updated_at": task.updated_at,
        "last_activity_at": task.last_activity_at
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
        "terminal_at": record.terminal_at,
        "output_refs": record.output_refs,
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
        "affected_file_count": event.affected_files.len(),
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
    [
        ".codegraph",
        ".gitnexus",
        "docs/session",
        // Frozen pre-Catalog-37 archive; retained locally but never used by the new Session store.
        "docs/history-session",
    ]
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
        .output()
        .map(|output| output.status.success())
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

#[derive(Default)]
struct TaskConfiguration {
    phase: Option<TaskPhase>,
    contract: Option<TaskContract>,
    slices: Option<Vec<TaskSlice>>,
    working_set: Option<TaskWorkingSet>,
}

impl TaskConfiguration {
    fn is_empty(&self) -> bool {
        self.phase.is_none()
            && self.contract.is_none()
            && self.slices.is_none()
            && self.working_set.is_none()
    }
}

fn parse_task_configuration(args: &Value) -> Result<TaskConfiguration, WorkspaceError> {
    Ok(TaskConfiguration {
        phase: parse_task_phase(args.get("phase"))?,
        contract: parse_task_contract(args.get("contract"))?,
        slices: parse_task_slices(args.get("slices"))?,
        working_set: parse_working_set(args.get("working_set"))?,
    })
}

fn parse_task_phase(value: Option<&Value>) -> Result<Option<TaskPhase>, WorkspaceError> {
    let Some(value) = value else { return Ok(None) };
    let raw = value
        .as_str()
        .ok_or_else(|| tool_error("INVALID_ARGUMENT", "phase 必须是字符串"))?;
    let phase = match raw {
        "unspecified" => TaskPhase::Unspecified,
        "planning" => TaskPhase::Planning,
        "implementing" => TaskPhase::Implementing,
        "verifying" => TaskPhase::Verifying,
        "deploying" => TaskPhase::Deploying,
        "browser_review" => TaskPhase::BrowserReview,
        "cleanup" => TaskPhase::Cleanup,
        "ready_to_close" => TaskPhase::ReadyToClose,
        "completed" => TaskPhase::Completed,
        "blocked" => TaskPhase::Blocked,
        "paused" => TaskPhase::Paused,
        _ => return Err(tool_error("INVALID_ARGUMENT", "unsupported task phase")),
    };
    Ok(Some(phase))
}

fn parse_task_contract(value: Option<&Value>) -> Result<Option<TaskContract>, WorkspaceError> {
    let Some(value) = value else { return Ok(None) };
    let mut contract: TaskContract = serde_json::from_value(value.clone())
        .map_err(|error| tool_error("INVALID_ARGUMENT", format!("invalid contract: {error}")))?;
    if contract.no_early_stop {
        contract.completion_policy.require_pending_steps_empty = true;
        contract.completion_policy.require_all_slices_completed = true;
        contract.completion_policy.require_no_open_recovery = true;
        contract.completion_policy.require_ready_to_close = true;
        contract.completion_policy.require_complete_work_session = true;
        contract.completion_policy.disallow_unverified_completion = true;
    }
    validate_string_values(&contract.constraints, 64, 2_000, "contract constraints")?;
    validate_requirements(
        &contract.required_verifications,
        "contract required_verifications",
    )?;
    Ok(Some(contract))
}

fn parse_working_set(value: Option<&Value>) -> Result<Option<TaskWorkingSet>, WorkspaceError> {
    let Some(value) = value else { return Ok(None) };
    let working_set: TaskWorkingSet = serde_json::from_value(value.clone())
        .map_err(|error| tool_error("INVALID_ARGUMENT", format!("invalid working_set: {error}")))?;
    validate_string_values(&working_set.primary, 256, 2_000, "working_set.primary")?;
    validate_string_values(&working_set.tests, 256, 2_000, "working_set.tests")?;
    validate_string_values(&working_set.locales, 256, 2_000, "working_set.locales")?;
    validate_string_values(
        &working_set.reference_only,
        256,
        2_000,
        "working_set.reference_only",
    )?;
    Ok(Some(working_set))
}

fn parse_task_slices(value: Option<&Value>) -> Result<Option<Vec<TaskSlice>>, WorkspaceError> {
    let Some(value) = value else { return Ok(None) };
    let values = value
        .as_array()
        .ok_or_else(|| tool_error("INVALID_ARGUMENT", "slices 必须是数组"))?;
    if values.len() > 64 {
        return Err(tool_error("INVALID_ARGUMENT", "slices 最多 64 项"));
    }
    let now = harness_timestamp();
    let mut slices = Vec::with_capacity(values.len());
    let mut ids = HashSet::new();
    for value in values {
        let slice = parse_task_slice(value, TaskSliceStatus::Planned, &now)?;
        if slice.status == TaskSliceStatus::Completed {
            return Err(tool_error(
                "SLICE_COMPLETION_TOOL_REQUIRED",
                "A configured Slice cannot start as completed",
            ));
        }
        if !ids.insert(slice.id.clone()) {
            return Err(tool_error("INVALID_ARGUMENT", "slice id 必须唯一"));
        }
        slices.push(slice);
    }
    Ok(Some(slices))
}

fn parse_task_slice(
    value: &Value,
    default_status: TaskSliceStatus,
    now: &str,
) -> Result<TaskSlice, WorkspaceError> {
    let object = value
        .as_object()
        .ok_or_else(|| tool_error("INVALID_ARGUMENT", "slice 必须是对象"))?;
    let id = required_bounded_string(object.get("id"), "slice.id", 128)?;
    let title = required_bounded_string(object.get("title"), "slice.title", 500)?;
    let status = object
        .get("status")
        .map(parse_slice_status_value)
        .transpose()?
        .unwrap_or(default_status);
    let files =
        parse_string_array(object.get("files"), 256, 2_000, "slice.files")?.unwrap_or_default();
    let acceptance_checks =
        parse_requirements_value(object.get("acceptance_checks"))?.unwrap_or_default();
    let commit_sha = optional_bounded_string(object.get("commit_sha"), "slice.commit_sha", 128)?;
    let blocker = optional_bounded_string(object.get("blocker"), "slice.blocker", 2_000)?;
    Ok(TaskSlice {
        id,
        title,
        status,
        files,
        acceptance_checks,
        commit_sha,
        blocker,
        created_at: now.to_string(),
        updated_at: now.to_string(),
    })
}

fn parse_slice_status_value(value: &Value) -> Result<TaskSliceStatus, WorkspaceError> {
    match value.as_str() {
        Some("planned") => Ok(TaskSliceStatus::Planned),
        Some("in_progress") => Ok(TaskSliceStatus::InProgress),
        Some("verifying") => Ok(TaskSliceStatus::Verifying),
        Some("blocked") => Ok(TaskSliceStatus::Blocked),
        Some("paused") => Ok(TaskSliceStatus::Paused),
        Some("completed") => Ok(TaskSliceStatus::Completed),
        _ => Err(tool_error("INVALID_ARGUMENT", "unsupported slice status")),
    }
}

fn parse_requirements_value(
    value: Option<&Value>,
) -> Result<Option<Vec<VerificationRequirement>>, WorkspaceError> {
    let Some(value) = value else { return Ok(None) };
    let requirements: Vec<VerificationRequirement> = serde_json::from_value(value.clone())
        .map_err(|error| {
            tool_error(
                "INVALID_ARGUMENT",
                format!("invalid verification requirements: {error}"),
            )
        })?;
    validate_requirements(&requirements, "verification requirements")?;
    Ok(Some(requirements))
}

fn validate_requirements(
    requirements: &[VerificationRequirement],
    subject: &str,
) -> Result<(), WorkspaceError> {
    if requirements.len() > 64 {
        return Err(tool_error(
            "INVALID_ARGUMENT",
            format!("{subject} 最多 64 项"),
        ));
    }
    let mut ids = HashSet::new();
    for requirement in requirements {
        if requirement.id.trim().is_empty() || requirement.id.len() > 128 {
            return Err(tool_error(
                "INVALID_ARGUMENT",
                format!("{subject} 的 id 必须为 1-128 字符"),
            ));
        }
        if !ids.insert(requirement.id.clone()) {
            return Err(tool_error(
                "INVALID_ARGUMENT",
                format!("{subject} 的 id 必须唯一"),
            ));
        }
        if requirement.kind.is_none()
            && requirement.verification_key.is_none()
            && requirement.test_file.is_none()
            && requirement.test_name.is_none()
        {
            return Err(tool_error(
                "INVALID_ARGUMENT",
                format!("{subject} 每项至少提供 kind、verification_key、test_file 或 test_name"),
            ));
        }
        for (name, value, max) in [
            ("description", requirement.description.as_str(), 2_000usize),
            ("kind", requirement.kind.as_deref().unwrap_or(""), 128),
            (
                "verification_key",
                requirement.verification_key.as_deref().unwrap_or(""),
                256,
            ),
            (
                "test_file",
                requirement.test_file.as_deref().unwrap_or(""),
                2_000,
            ),
            (
                "test_name",
                requirement.test_name.as_deref().unwrap_or(""),
                1_000,
            ),
        ] {
            if value.len() > max {
                return Err(tool_error(
                    "INVALID_ARGUMENT",
                    format!("{subject}.{name} 超过长度上限"),
                ));
            }
        }
    }
    Ok(())
}

fn parse_string_array(
    value: Option<&Value>,
    max_items: usize,
    max_len: usize,
    subject: &str,
) -> Result<Option<Vec<String>>, WorkspaceError> {
    let Some(value) = value else { return Ok(None) };
    let values = value
        .as_array()
        .ok_or_else(|| tool_error("INVALID_ARGUMENT", format!("{subject} 必须是字符串数组")))?
        .iter()
        .map(|value| {
            value.as_str().map(str::to_string).ok_or_else(|| {
                tool_error("INVALID_ARGUMENT", format!("{subject} 必须是字符串数组"))
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    validate_string_values(&values, max_items, max_len, subject)?;
    Ok(Some(values))
}

fn validate_string_values(
    values: &[String],
    max_items: usize,
    max_len: usize,
    subject: &str,
) -> Result<(), WorkspaceError> {
    if values.len() > max_items {
        return Err(tool_error(
            "INVALID_ARGUMENT",
            format!("{subject} 最多 {max_items} 项"),
        ));
    }
    if values
        .iter()
        .any(|value| value.trim().is_empty() || value.len() > max_len)
    {
        return Err(tool_error(
            "INVALID_ARGUMENT",
            format!("{subject} 包含空值或超长值"),
        ));
    }
    Ok(())
}

fn required_bounded_string(
    value: Option<&Value>,
    subject: &str,
    max_len: usize,
) -> Result<String, WorkspaceError> {
    let value = value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| tool_error("INVALID_ARGUMENT", format!("{subject} 是必填项")))?;
    if value.len() > max_len {
        return Err(tool_error(
            "INVALID_ARGUMENT",
            format!("{subject} 超过长度上限"),
        ));
    }
    Ok(value.to_string())
}

fn optional_bounded_string(
    value: Option<&Value>,
    subject: &str,
    max_len: usize,
) -> Result<Option<String>, WorkspaceError> {
    let Some(value) = value else { return Ok(None) };
    if value.is_null() {
        return Ok(None);
    }
    let value = value
        .as_str()
        .ok_or_else(|| tool_error("INVALID_ARGUMENT", format!("{subject} 必须是字符串或 null")))?;
    if value.len() > max_len {
        return Err(tool_error(
            "INVALID_ARGUMENT",
            format!("{subject} 超过长度上限"),
        ));
    }
    Ok(Some(value.to_string()))
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
