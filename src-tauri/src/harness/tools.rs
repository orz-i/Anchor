use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use serde_json::{json, Value};

use crate::tools::workspace::{tool_ok, WorkspaceError};
use crate::tools::{CancellationToken, ToolContext};

use super::model::{
    HarnessEvent, HarnessSessionStatus, TaskSession, TaskStatus, VerificationRecord,
};
use super::store::HarnessError;

const FINISH_RESPONSE_MAX_BYTES: usize = 32 * 1024;
const DEFAULT_SUMMARY_LIMIT: usize = 64;
const DEFAULT_EVENT_LIMIT: usize = 20;

pub const TOOL_NAMES: &[&str] = &[
    "harness_status",
    "operation_log",
    "project_state",
    "start_task",
    "refresh_baseline",
    "stage_commit",
    "update_task",
    "pause_task",
    "resume_task",
    "finish_task",
    "task_context",
    "list_task_events",
    "change_summary",
];

pub fn call(
    ctx: &ToolContext,
    name: &str,
    args: &Value,
    cancellation: &CancellationToken,
) -> Result<Value, WorkspaceError> {
    let value = match name {
        "harness_status" => harness_status(ctx),
        "operation_log" => operation_log(ctx, args),
        "project_state" => project_state(ctx, args),
        "start_task" => start_task(ctx, args),
        "refresh_baseline" => refresh_baseline(ctx, args),
        "stage_commit" => super::stage_commit::run(ctx, args, cancellation),
        "update_task" => update_task(ctx, args),
        "pause_task" => transition(ctx, args, TaskStatus::Paused),
        "resume_task" => transition(ctx, args, TaskStatus::Active),
        "finish_task" => finish_task(ctx, args),
        "task_context" => task_context(ctx, args),
        "list_task_events" => list_task_events(ctx, args),
        "change_summary" => change_summary(ctx, args),
        _ => return Err(tool_error("INVALID_ARGUMENT", "未知 Harness 工具")),
    }?;
    Ok(tool_ok(value))
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

fn harness_status(ctx: &ToolContext) -> Result<Value, WorkspaceError> {
    serde_json::to_value(ctx.harness.status().map_err(map_error)?)
        .map_err(|e| tool_error("SERIALIZE_FAILED", e.to_string()))
}

fn operation_log(ctx: &ToolContext, args: &Value) -> Result<Value, WorkspaceError> {
    let offset = args.get("cursor").and_then(Value::as_u64).unwrap_or(0) as usize;
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(50)
        .clamp(1, 200) as usize;
    let operations = ctx
        .harness
        .list_operations(offset, limit)
        .map_err(map_error)?;
    Ok(json!({
        "operations": operations,
        "next_cursor": offset + operations.len()
    }))
}

fn project_state(ctx: &ToolContext, args: &Value) -> Result<Value, WorkspaceError> {
    let max_files = args.get("max_files").and_then(Value::as_u64).unwrap_or(200) as usize;
    let state = ctx.harness.project_state(max_files).map_err(map_error)?;
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
        "task": state.task.as_ref().map(task_view),
        "recent_events": state.recent_events
    }))
}

fn start_task(ctx: &ToolContext, args: &Value) -> Result<Value, WorkspaceError> {
    let objective = args
        .get("objective")
        .and_then(Value::as_str)
        .ok_or_else(|| tool_error("INVALID_ARGUMENT", "objective 是必填项"))?;
    let task = ctx.harness.start_task(objective).map_err(map_error)?;
    Ok(json!({"task": task_view(&task), "next": ["project_state", "task_context"]}))
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
    if !working_tree_files.is_empty() {
        let task = ctx.harness.mark_verifying(task_id).map_err(map_error)?;
        return Ok(json!({
            "ok": false,
            "task_status": "verifying",
            "verification_status": verification_status,
            "closed": false,
            "session_status": "active",
            "next_stage_started": false,
            "reason": "工作区仍存在未提交的业务文件；提交或还原这些改动后才能关闭任务。",
            "working_tree_files": working_tree_files,
            "next_actions": ["git_status", "stage_commit", "finish_task"],
            "task": task_view(&task),
            "verification": verification_views(&verifications)
        }));
    }
    if !allow_unverified && verification_status != "verified" {
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
            "next_actions": ["exec_command", "change_summary", "finish_task"],
            "task": task_view(&task),
            "verification": verification_views(&verifications)
        }));
    }
    let session_status = parse_session_status(args)?;
    let verified = verification_status == "verified";
    let change_summary = change_summary(
        ctx,
        &json!({
            "task_id": task_id,
            "limit": DEFAULT_SUMMARY_LIMIT
        }),
    )?;
    let task = ctx
        .harness
        .complete_task(task_id, verified, session_status)
        .map_err(map_error)?;
    let mut response = json!({
        "ok": true,
        "task_status": if verified { "completed" } else { "completed_unverified" },
        "verification_status": if verified { "verified" } else { "unverified" },
        "closed": true,
        "session_status": session_status,
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

fn task_context(ctx: &ToolContext, args: &Value) -> Result<Value, WorkspaceError> {
    let task = if let Some(task_id) = args.get("task_id").and_then(Value::as_str) {
        Some(ctx.harness.task(task_id).map_err(map_error)?)
    } else {
        ctx.harness.current_task().map_err(map_error)?
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

fn change_summary(ctx: &ToolContext, args: &Value) -> Result<Value, WorkspaceError> {
    let task = if let Some(task_id) = args.get("task_id").and_then(Value::as_str) {
        ctx.harness.task(task_id).map_err(map_error)?
    } else {
        ctx.harness
            .current_task()
            .map_err(map_error)?
            .ok_or_else(|| tool_error("TASK_STATE_REQUIRED", "没有可总结的活动任务"))?
    };
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_SUMMARY_LIMIT as u64)
        .clamp(1, 500) as usize;
    let cursor = args.get("cursor").and_then(Value::as_u64).unwrap_or(0) as usize;
    let section = args.get("section").and_then(Value::as_str);
    let requested_change_id = args
        .get("change_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let selected_change_id = requested_change_id.or(task.latest_change_id.as_deref());
    let change = selected_change_id
        .and_then(|change_id| ctx.harness.load_change_set(change_id).ok().flatten());
    let commit_sha = change
        .as_ref()
        .and_then(|change| change.commit_sha.clone())
        .or_else(|| selected_change_id.map(str::to_string))
        .or_else(|| {
            task.expected_state
                .as_ref()
                .and_then(|expected| expected.head.clone())
                .filter(|head| task.baseline.head.as_ref() != Some(head))
        });
    let committed_files = change
        .as_ref()
        .map(|change| change.committed_files.clone())
        .filter(|files| !files.is_empty())
        .unwrap_or_else(|| {
            commit_sha
                .as_deref()
                .map(|commit| {
                    git_paths(
                        ctx.workspace.root(),
                        &[
                            "diff-tree",
                            "--no-commit-id",
                            "--name-only",
                            "-r",
                            "-z",
                            commit,
                        ],
                    )
                })
                .unwrap_or_default()
        });
    let mut working_tree_files = git_working_tree_files(ctx.workspace.root());
    let mut runtime_artifacts = known_runtime_artifacts(ctx.workspace.root());
    let mut ignored_files = known_ignored_paths(ctx.workspace.root());
    working_tree_files.retain(|path| !is_runtime_artifact(path));
    if let Some(change) = change.as_ref() {
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
        "committed_files": committed_files.len(),
        "working_tree_files": working_tree_files.len(),
        "runtime_artifacts": runtime_artifacts.len(),
        "ignored_files": ignored_files.len(),
        "verification": verifications.len(),
        "evidence": events.len()
    });
    let bounded_objective = bounded_text(&task.objective, 4_000);
    let mut summary = json!({
        "task_id": task.id,
        "objective": bounded_objective,
        "why": {"text": bounded_objective, "source": "task_objective"},
        "commit_sha": commit_sha,
        "committed_files": committed_files,
        "working_tree_files": working_tree_files,
        "runtime_artifacts": runtime_artifacts,
        "ignored_files": ignored_files,
        "evidence": evidence,
        "verification": verification_views(&verifications),
        "verification_status": verification_status(&verifications),
        "risks": [],
        "rollback_capability": if task.latest_change_id.is_some() { "git_commit" } else { "not_available" },
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
        "file_count": task.baseline.entries.len(),
        "baseline_hash": task.baseline.worktree_fingerprint,
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
        "created_at": task.created_at,
        "updated_at": task.updated_at
    })
}

fn verification_status(records: &[VerificationRecord]) -> &'static str {
    let effective = effective_verifications(records);
    if effective.is_empty() {
        "missing"
    } else if effective.iter().all(|record| record.passed) {
        "verified"
    } else {
        "failed"
    }
}

fn effective_verifications(records: &[VerificationRecord]) -> Vec<&VerificationRecord> {
    let mut latest = BTreeMap::<(&str, &str), &VerificationRecord>::new();
    for record in records {
        latest.insert((record.kind.as_str(), record.command.as_str()), record);
    }
    latest.into_values().collect()
}

fn verification_views(records: &[VerificationRecord]) -> Vec<Value> {
    records
        .iter()
        .map(|record| {
            json!({
                "verification_id": record.id,
                "kind": record.kind,
                "status": record.status,
                "passed": record.passed,
                "exit_code": record.exit_code,
                "command": bounded_text(&record.command, 4_000),
                "duration_ms": record.duration_ms,
                "change_id": record.change_id,
                "created_at": record.created_at
            })
        })
        .collect()
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

fn update_response_bytes(response: &mut Value) -> usize {
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
        "committed_files",
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
            "TASK_ALREADY_ACTIVE" | "FILE_CHANGED_EXTERNALLY" | "BASELINE_STALE"
        ),
    }
}
