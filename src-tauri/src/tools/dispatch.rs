use std::path::Path;

use serde_json::{json, Value};

use crate::harness::state::{capture_baseline_entries, diff_baseline_entries};
use crate::tools::context::ToolContext;
use crate::tools::policy::{validate_tool_arguments_for_workspace, PolicyError};
use crate::tools::workspace::{tool_err, tool_err_code, tool_ok, WorkspaceError};
use crate::tools::{
    exec, file, git, history, image_tool, patch, recovery, session, CancellationToken,
};

fn policy_tool_err(err: PolicyError) -> Value {
    let dangerous = err
        .0
        .strip_prefix("DANGEROUS_OPERATION_REQUIRES_DANGEROUS_MODE: ");
    let protected = err.0.strip_prefix("PROTECTED_REPOSITORY_ASSET: ");
    let code = if protected.is_some() {
        "PROTECTED_REPOSITORY_ASSET"
    } else if dangerous.is_some() {
        "DANGEROUS_OPERATION_REQUIRES_DANGEROUS_MODE"
    } else {
        "POLICY_REJECTED"
    };
    let message = protected.or(dangerous).unwrap_or(&err.0).to_string();
    let alternatives = policy_alternatives(&message);
    let recoverable = !alternatives.is_empty();
    let (reason, suggestion) = if dangerous.is_some() {
        (
            "dangerous_mode_required",
            "模型参数不能作为用户批准凭证；请由操作者在受信任控制面将权限模式切换为 dangerous 后重试",
        )
    } else if message.contains("allowlisted") {
        (
            "command_rejected",
            if recoverable {
                "使用返回的安全替代工具或命令重试"
            } else {
                "改用允许的命令，或由操作者调整工作区命令白名单"
            },
        )
    } else if message.contains("Shell chaining") {
        (
            "shell_syntax_rejected",
            "移除未加引号的 shell 操作符；引号内的程序参数可以保留",
        )
    } else {
        ("policy_rejected", "根据错误信息修正参数后重试")
    };
    tool_err(WorkspaceError::ToolDetails {
        code,
        message,
        category: "policy",
        retryable: recoverable,
        details: json!({
            "stage": "policy",
            "reason": reason,
            "recoverable": recoverable,
            "suggestion": suggestion,
            "alternatives": alternatives
        }),
    })
}

fn normalize_exec_preflight_result(
    ctx: &ToolContext,
    name: &str,
    args: &Value,
    mut output: Value,
    execution_status: &str,
) -> Value {
    if name != "exec_command" {
        return output;
    }
    if let Some(object) = output.as_object_mut() {
        object.insert(
            "command".into(),
            args.get("cmd")
                .cloned()
                .unwrap_or_else(|| json!("<invalid>")),
        );
        object.insert(
            "resolved_cwd".into(),
            Value::String(ctx.default_cwd_path_for(None).display().to_string()),
        );
        object.insert("status".into(), Value::String(execution_status.into()));
        object.insert(
            "termination_reason".into(),
            Value::String(
                if execution_status == "cancelled" {
                    "cancelled"
                } else {
                    "command_rejected"
                }
                .into(),
            ),
        );
        object.insert("transport_ok".into(), Value::Bool(true));
        object.insert("command_ok".into(), Value::Bool(false));
        object.insert("execution_started".into(), Value::Bool(false));
    }
    session::finalize_execution_result(output)
}

fn policy_alternatives(message: &str) -> Vec<Value> {
    let executable = message
        .strip_prefix("Command is not allowlisted: ")
        .map(str::trim)
        .unwrap_or_default()
        .to_ascii_lowercase();
    match executable.as_str() {
        "rg" | "ripgrep" => vec![
            json!({
                "type": "tool",
                "name": "search_text",
                "reason": "使用 Anchor 的受控文本搜索，不需要额外命令白名单"
            }),
            json!({
                "type": "command",
                "command": "grep",
                "reason": "grep 位于默认诊断命令白名单；调用者需根据原参数重新构造安全参数"
            }),
        ],
        "corepack" => vec![json!({
            "type": "command",
            "command": "pnpm",
            "reason": "pnpm 已在默认白名单中；仅当原命令形如 corepack pnpm ... 时可移除 wrapper"
        })],
        "findstr" => vec![json!({
            "type": "tool",
            "name": "search_text",
            "reason": "使用跨平台工作区文本搜索"
        })],
        _ => Vec::new(),
    }
}

fn advances_expected_state(name: &str, output: &Value) -> bool {
    match name {
        "apply_patch"
        | "history_session_checkpoint"
        | "git_stage"
        | "git_commit"
        | "git_restore"
        | "git_reset"
        | "git_revert"
        | "git_clean"
        | "remove_path" => true,
        "exec_command" => command_output_is_terminal(output),
        _ => false,
    }
}

fn command_output_is_terminal(output: &Value) -> bool {
    output.get("status").and_then(Value::as_str) != Some("running")
        && output.get("termination_reason").and_then(Value::as_str) != Some("running")
}

struct VerificationIdentity<'a> {
    kind: &'a str,
    command: &'a str,
    verification_key: Option<&'a str>,
    test_file: Option<&'a str>,
    test_name: Option<&'a str>,
    level: &'a str,
}

fn record_verification_from_output(
    ctx: &ToolContext,
    task_id: &str,
    identity: VerificationIdentity<'_>,
    supersede_previous_failures: bool,
    output: &mut Value,
) {
    let exit_code = output
        .get("exit_code")
        .and_then(Value::as_i64)
        .and_then(|value| i32::try_from(value).ok());
    if output.get("execution_started").and_then(Value::as_bool) == Some(false) {
        if let Some(object) = output.as_object_mut() {
            object.insert("verification_skipped".into(), Value::Bool(true));
            object.insert(
                "verification_skip_reason".into(),
                Value::String("command_not_executed".into()),
            );
        }
        return;
    }
    let Some(passed) = output.get("command_ok").and_then(Value::as_bool) else {
        if let Some(object) = output.as_object_mut() {
            object.insert("verification_skipped".into(), Value::Bool(true));
            object.insert(
                "verification_skip_reason".into(),
                Value::String("command_not_executed".into()),
            );
        }
        return;
    };
    let duration_ms = output.get("duration_ms").and_then(Value::as_u64);
    if let Ok(verification) = ctx.harness.record_verification(
        task_id,
        identity.kind,
        identity.command,
        identity.verification_key,
        identity.test_file,
        identity.test_name,
        exit_code,
        passed,
        duration_ms,
        None,
        identity.level,
        supersede_previous_failures,
    ) {
        let effective_disposition = verification
            .dispositions
            .last()
            .map(|entry| entry.disposition.as_str())
            .unwrap_or(if verification.passed {
                "passed"
            } else {
                "active_failure"
            });
        if let Some(object) = output.as_object_mut() {
            object.insert(
                "verification_id".into(),
                Value::String(verification.id.clone()),
            );
            object.insert(
                "verification_level".into(),
                Value::String(verification.level.clone()),
            );
            object.insert(
                "supersedes".into(),
                serde_json::to_value(&verification.supersedes).unwrap_or_else(|_| json!([])),
            );
            object.insert(
                "affected_task_status".into(),
                ctx.harness
                    .task(task_id)
                    .ok()
                    .map(|task| serde_json::to_value(task.status).unwrap_or(Value::Null))
                    .unwrap_or(Value::Null),
            );
            object.insert(
                "verification".into(),
                json!({
                    "verification_id": verification.id,
                    "kind": verification.kind,
                    "verification_key": verification.verification_key,
                    "test_file": verification.test_file,
                    "test_name": verification.test_name,
                    "status": verification.status,
                    "level": verification.level,
                    "effective_disposition": effective_disposition,
                    "exit_code": verification.exit_code,
                    "command": verification.command,
                    "duration_ms": verification.duration_ms,
                    "supersedes": verification.supersedes
                }),
            );
        }
    }
}

fn skill_script_permission_error(
    ctx: &ToolContext,
    name: &str,
    args: &Value,
) -> Option<WorkspaceError> {
    if name != "exec_command" {
        return None;
    }
    let command = args.get("cmd").and_then(Value::as_str)?;
    let workdir = args.get("workdir").and_then(Value::as_str).unwrap_or(".");
    let workdir = ctx
        .workspace
        .resolve_existing(workdir)
        .ok()
        .filter(|resolved| resolved.path.is_dir())
        .map(|resolved| resolved.path)
        .unwrap_or_else(|| ctx.workspace.root().to_path_buf());
    let script = ctx.skills.match_script_command(command, &workdir)?;
    if !script.reviewable {
        return Some(WorkspaceError::ToolDetails {
            code: "SKILL_SCRIPT_UNREVIEWABLE",
            message: format!(
                "Skill {} 的脚本 {} 过大或无法生成完整快照摘要，禁止执行",
                script.skill, script.path
            ),
            category: "security",
            retryable: false,
            details: json!({
                "stage": "skill_script_policy",
                "reason": "script_not_reviewable",
                "skill": script.skill,
                "script": script.path,
                "snapshot_digest": script.snapshot_digest,
                "suggestion": "Reduce the script below the Skill resource limit and restart the MCP listener"
            }),
        });
    }
    if script.stale {
        return Some(WorkspaceError::ToolDetails {
            code: "SKILL_SCRIPT_SNAPSHOT_STALE",
            message: format!(
                "Skill {} 的脚本 {} 在目录快照建立后已变化；请重启 MCP listener 后重新审查",
                script.skill, script.path
            ),
            category: "security",
            retryable: false,
            details: json!({
                "stage": "skill_script_policy",
                "reason": "snapshot_digest_mismatch",
                "skill": script.skill,
                "script": script.path,
                "snapshot_digest": script.snapshot_digest,
                "current_digest": script.current_digest,
                "suggestion": "Restart the MCP listener to rebuild the Skill snapshot, then review the script again"
            }),
        });
    }
    if ctx.policy.skip_permission_gates() {
        return None;
    }
    Some(WorkspaceError::ToolDetails {
        code: "SKILL_SCRIPT_REQUIRES_DANGEROUS_MODE",
        message: format!(
            "执行 Skill {} 的脚本 {} 需要操作者在受信任控制面启用 dangerous 权限模式",
            script.skill, script.path
        ),
        category: "permission",
        retryable: false,
        details: json!({
            "stage": "skill_script_policy",
            "reason": "dangerous_mode_required",
            "skill": script.skill,
            "script": script.path,
            "digest": script.snapshot_digest,
            "dedicated_skill_execution": false,
            "suggestion": "Review the script source and digest, then enable dangerous mode through the trusted GUI or CLI control plane"
        }),
    })
}

/// **唯一工具执行入口**。MCP `tools/call` 与 Actions `POST /actions/{tool}` 必须且只能调用此函数。
/// 策略校验、分发、错误格式在此统一，两路传输层不得另做执行前校验（Actions 仅允许额外的暴露层 `validate_actions_exposure`）。
pub fn call_tool(ctx: &ToolContext, name: &str, args: &Value) -> Value {
    call_tool_impl(ctx, name, args, &CancellationToken::default(), true, None)
}

pub fn call_tool_with_cancellation(
    ctx: &ToolContext,
    name: &str,
    args: &Value,
    cancellation: &CancellationToken,
) -> Value {
    call_tool_impl(ctx, name, args, cancellation, true, None)
}

#[doc(hidden)]
pub fn call_tool_for_session(
    ctx: &ToolContext,
    name: &str,
    args: &Value,
    session_id: &str,
) -> Value {
    call_tool_impl(
        ctx,
        name,
        args,
        &CancellationToken::default(),
        true,
        Some(session_id),
    )
}

pub(crate) fn call_tool_prevalidated_with_session_cancellation(
    ctx: &ToolContext,
    name: &str,
    args: &Value,
    cancellation: &CancellationToken,
    session_id: Option<&str>,
) -> Value {
    call_tool_impl(ctx, name, args, cancellation, false, session_id)
}

fn call_tool_impl(
    ctx: &ToolContext,
    name: &str,
    args: &Value,
    cancellation: &CancellationToken,
    validate_schema: bool,
    session_id: Option<&str>,
) -> Value {
    if cancellation.is_cancelled() {
        let output = cancelled_tool_result();
        return normalize_exec_preflight_result(ctx, name, args, output, "cancelled");
    }
    if validate_schema {
        if let Err(error) = crate::tools::schema::validate_tool_input(name, args) {
            let output = tool_err(error);
            return normalize_exec_preflight_result(ctx, name, args, output, "rejected");
        }
    }
    let effective_args = apply_default_cwd(ctx, session_id, name, args);
    if let Some(error) = skill_script_permission_error(ctx, name, &effective_args) {
        return tool_err(error);
    }
    if let Err(e) = validate_tool_arguments_for_workspace(
        name,
        &effective_args,
        &ctx.policy,
        Some(&ctx.workspace),
    ) {
        let output = policy_tool_err(e);
        return normalize_exec_preflight_result(ctx, name, &effective_args, output, "rejected");
    }
    let mut selected_task = if name == "begin_work_session" {
        ctx.bound_task_for_session(session_id)
    } else {
        ctx.task_for_session(session_id)
    };
    if name != "begin_work_session" {
        if let Some(task) = selected_task.as_ref() {
            match ctx
                .harness
                .resume_task_for_activity(&task.id, name, session_id)
            {
                Ok(task) => selected_task = Some(task),
                Err(error) => {
                    return attach_harness_status(
                        ctx,
                        tool_err_code(error.code(), error.to_string(), "internal"),
                        false,
                        session_id,
                    )
                }
            }
        }
    }
    let _workspace_mutation_guard =
        requires_write_baseline(name, &effective_args).then(|| ctx.workspace_mutation_guard());

    if crate::harness::tools::TOOL_NAMES.contains(&name) {
        let active_task = selected_task.clone();
        let requested_task_id = args
            .get("task_id")
            .and_then(Value::as_str)
            .map(str::to_string);
        let operation_task_id = requested_task_id
            .as_deref()
            .or_else(|| active_task.as_ref().map(|task| task.id.as_str()));
        let operation = if name == "operation_log" {
            None
        } else {
            ctx.harness
                .record_operation(
                    None,
                    operation_task_id,
                    session_id,
                    name,
                    "started",
                    operation_input(args),
                    json!({"ok": true}),
                )
                .ok()
        };
        let mut output =
            match crate::harness::tools::call(ctx, name, args, cancellation, session_id) {
                Ok(value) => value,
                Err(error) => attach_harness_status(ctx, tool_err(error), false, session_id),
            };
        let result_task_id = output
            .get("task")
            .and_then(|task| task.get("id"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .or(requested_task_id)
            .or_else(|| active_task.as_ref().map(|task| task.id.clone()));
        if let Some(operation) = operation {
            if let Some(object) = output.as_object_mut() {
                object.insert("operation_id".into(), Value::String(operation.id.clone()));
                object.insert("trace_id".into(), Value::String(operation.id.clone()));
            }
            if output.get("response_bytes").is_some() {
                crate::harness::tools::update_response_bytes(&mut output);
            }
            let succeeded = output.get("ok").and_then(Value::as_bool) == Some(true);
            let _ = ctx.harness.record_operation(
                Some(&operation.id),
                result_task_id.as_deref(),
                session_id,
                name,
                if succeeded { "completed" } else { "failed" },
                operation_input(args),
                operation_result_summary(name, &output),
            );
        }
        attach_auto_checkpoint(ctx, name, args, &mut output, result_task_id.as_deref());
        return output;
    }

    let active_task = selected_task;
    let retained_session = if matches!(name, "write_stdin" | "kill_session" | "wait_command") {
        effective_args
            .get("session_id")
            .and_then(Value::as_str)
            .and_then(|session_id| ctx.sessions.get(session_id).ok())
    } else {
        None
    };
    let task_id = if name == "remove_path" {
        if let Some(task) = active_task.as_ref() {
            let _ = ctx.harness.record_event(
                &task.id,
                "operation_started",
                Some(name),
                operation_input(args),
                json!({"ok": true, "tracking": "recovery_without_baseline_gate"}),
            );
            Some(task.id.clone())
        } else {
            None
        }
    } else if requires_write_baseline(name, &effective_args) {
        if let Some(task) = active_task.as_ref() {
            if let Err(error) = ctx.harness.check_baseline(&task.id) {
                return attach_harness_status(
                    ctx,
                    tool_err_code(error.code(), error.to_string(), "permission"),
                    false,
                    session_id,
                );
            }
            let _ = ctx.harness.record_event(
                &task.id,
                "operation_started",
                Some(name),
                operation_input(args),
                json!({"ok": true, "tracking": "task"}),
            );
            Some(task.id.clone())
        } else {
            None
        }
    } else {
        None
    };

    let operation = if should_log_operation(name) {
        ctx.harness
            .record_operation(
                None,
                active_task.as_ref().map(|task| task.id.as_str()),
                session_id,
                name,
                "started",
                json!({"arguments_present": !args.is_null()}),
                json!({"ok": true}),
            )
            .ok()
    } else {
        None
    };

    let ws = &ctx.workspace;
    let result = match name {
        "history_session_bootstrap" => history::bootstrap(ctx, &effective_args),
        "history_session_checkpoint" => history::checkpoint(ctx, &effective_args),
        "history_session_validate" => history::validate(ctx, &effective_args),
        "server_info" => server_info_for_session(ctx, session_id),
        "list_skills" => crate::skills::list_tool(ctx, &effective_args),
        "load_skill" => crate::skills::load_tool(ctx, &effective_args),
        "list_skill_resources" => crate::skills::list_resources_tool(&ctx.skills, &effective_args),
        "read_skill_resource" => crate::skills::read_resource_tool(&ctx.skills, &effective_args),
        "check_exec_environment" => check_exec_environment(ctx),
        "exec_health_check" => exec::exec_health_check(ctx),
        "command_cost_explain" => exec::command_cost_explain(ctx, &effective_args),
        "get_default_cwd" => get_default_cwd_for_session(ctx, session_id),
        "set_default_cwd" => set_default_cwd_for_session(ctx, session_id, &effective_args),
        "read_file" => file::read_file(ws, &effective_args, cancellation),
        "list_dir" => file::list_dir(ws, &effective_args, cancellation),
        "list_files" => file::list_files(ws, &effective_args, cancellation),
        "search_text" => file::search_text(ws, &effective_args, cancellation),
        "patch_check" => patch::patch_check(ctx, &effective_args),
        "apply_patch" => patch::apply_patch(ctx, &effective_args),
        "remove_path" => recovery::remove_path(ctx, &effective_args),
        "exec_command" => exec::exec_command_with_cancellation(
            ctx,
            &effective_args,
            cancellation,
            active_task.as_ref().map(|task| task.id.as_str()),
        ),
        "read_output" => session::read_output(&ctx.sessions, &effective_args),
        "write_stdin" => session::write_stdin(&ctx.sessions, &effective_args),
        "wait_command" => session::wait_command(&ctx.sessions, &effective_args),
        "list_command_sessions" => session::list_command_sessions(&ctx.sessions, &effective_args),
        "kill_session" => session::kill_session(&ctx.sessions, &effective_args),
        "git_status" => git::git_status(ws, &effective_args),
        "git_stage" => git::git_stage(ws, &effective_args),
        "git_commit" => git::git_commit(ws, &effective_args),
        "git_restore" => git::git_restore(ws, &effective_args),
        "git_reset" => git::git_reset(ws, &effective_args, ctx.policy.skip_permission_gates()),
        "git_revert" => git::git_revert(ws, &effective_args),
        "git_clean" => git::git_clean(ws, &effective_args, ctx.policy.skip_permission_gates()),
        "git_diff" => git::git_diff(ws, &effective_args),
        "git_log" => git::git_log(ws, &effective_args),
        "git_show" => git::git_show(ws, &effective_args),
        "git_blame" => git::git_blame(ws, &effective_args),
        "view_image" => image_tool::view_image(ws, &effective_args),
        _ => {
            return tool_err_code(
                "INVALID_ARGUMENT",
                format!("Unknown tool: {name}"),
                "validation",
            )
        }
    };
    let mut output = match result {
        Ok(v) => v,
        Err(e) => tool_err(e),
    };
    // Cancellation is checked before execution and by cooperative long-running
    // tools. Once a synchronous mutation returns, preserve its committed result
    // instead of reporting a false cancellation that could trigger a retry.
    output = preserve_completed_result(output, cancellation);
    if task_id.is_none()
        && standalone_operation(name)
        && output.get("ok") == Some(&Value::Bool(true))
    {
        attach_standalone_metadata(
            &mut output,
            "当前操作已在 standalone 模式完成；如需继续，直接调用下一个开发工具。",
        );
    }
    if let Some(operation) = operation.as_ref() {
        if let Some(object) = output.as_object_mut() {
            object.insert("operation_id".into(), Value::String(operation.id.clone()));
            object.insert("trace_id".into(), Value::String(operation.id.clone()));
        }
    }
    if output.get("ok").and_then(Value::as_bool) == Some(false) {
        output = attach_harness_status(ctx, output, task_id.is_none(), session_id);
    }
    if let Some(task_id) = task_id.as_deref() {
        let succeeded = output.get("ok").and_then(Value::as_bool) == Some(true);
        let _ = ctx.harness.record_event(
            task_id,
            "operation_finished",
            Some(name),
            operation_input(args),
            operation_result_summary(name, &output),
        );
        let execution_started = output
            .get("execution_started")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let should_advance_expected_state = advances_expected_state(name, &output)
            && (succeeded || (name == "exec_command" && execution_started));
        if should_advance_expected_state {
            let operation_id = operation.as_ref().map(|operation| operation.id.as_str());
            let _ = ctx
                .harness
                .refresh_expected_state_for_operation(task_id, operation_id);
        }
        if name == "exec_command" && command_output_is_terminal(&output) {
            if let (Some(kind), Some(command)) = (
                effective_args
                    .get("verification_kind")
                    .and_then(Value::as_str),
                effective_args.get("cmd").and_then(Value::as_str),
            ) {
                let level = effective_args
                    .get("verification_level")
                    .and_then(Value::as_str)
                    .unwrap_or("blocking");
                let supersede = effective_args
                    .get("supersede_previous_failures")
                    .and_then(Value::as_bool)
                    .unwrap_or(true);
                let verification_key = effective_args
                    .get("verification_key")
                    .and_then(Value::as_str);
                let test_file = effective_args.get("test_file").and_then(Value::as_str);
                let test_name = effective_args.get("test_name").and_then(Value::as_str);
                record_verification_from_output(
                    ctx,
                    task_id,
                    VerificationIdentity {
                        kind,
                        command,
                        verification_key,
                        test_file,
                        test_name,
                        level,
                    },
                    supersede,
                    &mut output,
                );
            }
        } else if name == "exec_command" && effective_args.get("verification_kind").is_some() {
            if let Some(object) = output.as_object_mut() {
                object.insert("verification_pending".into(), Value::Bool(true));
            }
        }
    }
    if let Some(session) = retained_session {
        if command_output_is_terminal(&output) {
            if let Some(object) = output.as_object_mut() {
                object.entry("affected_files").or_insert_with(|| json!([]));
                object
                    .entry("mutation_attributed")
                    .or_insert(Value::Bool(false));
            }
        }
        if command_output_is_terminal(&output) && session.mark_harness_finalized() {
            if let Some(metadata) = session.harness_metadata() {
                let workspace_after = capture_baseline_entries(ctx.workspace.root());
                let affected_files =
                    diff_baseline_entries(&metadata.workspace_before, &workspace_after);
                if let Some(object) = output.as_object_mut() {
                    let mutation_attributed = !affected_files.is_empty();
                    object.insert(
                        "affected_files".into(),
                        serde_json::to_value(affected_files).unwrap_or_else(|_| json!([])),
                    );
                    object.insert(
                        "mutation_attributed".into(),
                        Value::Bool(mutation_attributed),
                    );
                }
                let operation_id = operation.as_ref().map(|operation| operation.id.as_str());
                let _ = ctx
                    .harness
                    .refresh_expected_state_for_operation(&metadata.task_id, operation_id);
                if let Some(kind) = metadata.verification_kind.as_deref() {
                    record_verification_from_output(
                        ctx,
                        &metadata.task_id,
                        VerificationIdentity {
                            kind,
                            command: &metadata.command,
                            verification_key: metadata.verification_key.as_deref(),
                            test_file: metadata.test_file.as_deref(),
                            test_name: metadata.test_name.as_deref(),
                            level: &metadata.verification_level,
                        },
                        metadata.supersede_previous_failures,
                        &mut output,
                    );
                }
                let _ = ctx.harness.record_event(
                    &metadata.task_id,
                    "command_session_finalized",
                    Some(name),
                    json!({"session_id": effective_args.get("session_id")}),
                    json!({
                        "ok": output.get("ok"),
                        "command": metadata.command,
                        "termination_reason": output.get("termination_reason"),
                        "exit_code": output.get("exit_code")
                    }),
                );
            }
        }
    }
    if let Some(operation) = operation {
        let succeeded = output.get("ok").and_then(Value::as_bool) == Some(true);
        let _ = ctx.harness.record_operation(
            Some(&operation.id),
            active_task.as_ref().map(|task| task.id.as_str()),
            session_id,
            name,
            if succeeded { "completed" } else { "failed" },
            operation_input(args),
            operation_result_summary(name, &output),
        );
    } else if output.get("ok").and_then(Value::as_bool) == Some(false) {
        let _ = ctx.harness.record_operation(
            None,
            active_task.as_ref().map(|task| task.id.as_str()),
            session_id,
            name,
            "failed",
            operation_input(args),
            operation_result_summary(name, &output),
        );
    }
    attach_auto_checkpoint(
        ctx,
        name,
        &effective_args,
        &mut output,
        active_task.as_ref().map(|task| task.id.as_str()),
    );
    output
}

fn attach_auto_checkpoint(
    ctx: &ToolContext,
    name: &str,
    args: &Value,
    output: &mut Value,
    task_id: Option<&str>,
) {
    let baseline_was_current = ctx
        .harness
        .status_for_task(task_id)
        .ok()
        .and_then(|status| status.baseline_matches)
        == Some(true);
    match history::auto_checkpoint_after_tool(ctx, name, args, output, task_id) {
        Ok(Some(checkpoint)) => {
            if baseline_was_current {
                if let Some(task_id) = task_id {
                    let _ = ctx
                        .harness
                        .refresh_expected_state_for_operation(task_id, None);
                }
            }
            if let Some(object) = output.as_object_mut() {
                object.insert("auto_checkpoint".into(), checkpoint);
            }
        }
        Ok(None) => {}
        Err(error) => {
            if let Some(object) = output.as_object_mut() {
                object.insert(
                    "auto_checkpoint_error".into(),
                    json!({
                        "code": error.to_error_value()["code"],
                        "message": error.to_string(),
                        "retryable": true
                    }),
                );
                let warnings = object
                    .entry("warnings")
                    .or_insert_with(|| Value::Array(Vec::new()));
                if let Some(warnings) = warnings.as_array_mut() {
                    warnings.push(Value::String(
                        "Automatic History checkpoint failed; the primary tool result remains authoritative."
                            .into(),
                    ));
                }
            }
        }
    }
}

fn operation_result_summary(name: &str, output: &Value) -> Value {
    let error = output.get("error");
    json!({
        "ok": output.get("ok").and_then(Value::as_bool) == Some(true),
        "tool": name,
        "session_id": output.get("session_id"),
        "termination_reason": output.get("termination_reason"),
        "exit_code": output.get("exit_code"),
        "duration_ms": output.get("duration_ms").or_else(|| output.get("elapsed_ms")),
        "error_code": error.and_then(|value| value.get("code")),
        "error_message": error.and_then(|value| value.get("message")),
        "retryable": error.and_then(|value| value.get("retryable")),
        "error_details": error.and_then(|value| value.get("details")),
        "verification_id": output.get("verification_id"),
        "verification_level": output.get("verification_level"),
        "disposition": output
            .get("verification")
            .and_then(|value| value.get("effective_disposition")),
        "supersedes": output.get("supersedes"),
        "affected_task_status": output.get("affected_task_status"),
        "affected_files": output.get("affected_files")
    })
}

fn preserve_completed_result(mut output: Value, cancellation: &CancellationToken) -> Value {
    if cancellation.is_cancelled() && output.get("ok").and_then(Value::as_bool) == Some(true) {
        if let Some(object) = output.as_object_mut() {
            object.insert("cancellation_after_completion".into(), Value::Bool(true));
            let warnings = object
                .entry("warnings")
                .or_insert_with(|| Value::Array(Vec::new()));
            if let Some(warnings) = warnings.as_array_mut() {
                warnings.push(Value::String(
                    "Cancellation arrived after the tool had completed; the committed result is authoritative."
                        .into(),
                ));
            }
        }
    }
    output
}

fn cancelled_tool_result() -> Value {
    tool_err(WorkspaceError::ToolDetails {
        code: "REQUEST_CANCELLED",
        message: "The MCP request was cancelled.".into(),
        category: "runtime",
        retryable: true,
        details: json!({
            "reason": "request_cancelled",
            "termination_reason": "cancelled"
        }),
    })
}

fn apply_default_cwd(
    ctx: &ToolContext,
    session_id: Option<&str>,
    name: &str,
    args: &Value,
) -> Value {
    let base = if ctx.default_cwd_path_for(session_id) == ctx.workspace.root() {
        ".".to_string()
    } else {
        ctx.default_cwd_display_for(session_id)
    };
    if base == "." {
        return args.clone();
    }

    let mut effective = args.clone();
    match name {
        "exec_command" if effective.get("workdir").is_none() && effective.get("cwd").is_none() => {
            effective["workdir"] = Value::String(base.clone());
        }
        "list_dir" | "list_files" | "git_status" | "git_log" => {
            let path = effective.get("path").and_then(Value::as_str).unwrap_or(".");
            effective["path"] = Value::String(prefix_relative_path(&base, path));
        }
        "read_file" | "search_text" | "git_blame" | "view_image" | "remove_path" => {
            if let Some(path) = effective.get("path").and_then(Value::as_str) {
                effective["path"] = Value::String(prefix_relative_path(&base, path));
            }
        }
        "git_diff" | "git_stage" | "git_restore" | "git_clean" => {
            if let Some(path) = effective.get("path").and_then(Value::as_str) {
                effective["path"] = Value::String(prefix_relative_path(&base, path));
            }
            if let Some(paths) = effective.get("paths").and_then(Value::as_array).cloned() {
                effective["paths"] = Value::Array(
                    paths
                        .iter()
                        .map(|path| {
                            path.as_str()
                                .map(|value| Value::String(prefix_relative_path(&base, value)))
                                .unwrap_or_else(|| path.clone())
                        })
                        .collect(),
                );
            }
        }
        "apply_patch" | "patch_check" => {
            if let Some(patch) = effective.get("patch").and_then(Value::as_str) {
                effective["patch"] = Value::String(prefix_patch_paths(&base, patch));
            }
        }
        _ => {}
    }
    effective
}

fn prefix_relative_path(base: &str, path: &str) -> String {
    if path == "." || path.is_empty() {
        return base.to_string();
    }
    if Path::new(path).is_absolute() || path.starts_with("..") {
        return path.to_string();
    }
    format!("{base}/{}", path.trim_start_matches("./"))
}

fn prefix_patch_paths(base: &str, patch: &str) -> String {
    patch
        .lines()
        .map(|line| {
            for marker in ["--- a/", "+++ b/"] {
                if let Some(path) = line.strip_prefix(marker) {
                    return format!("{marker}{base}/{path}");
                }
            }
            line.to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn requires_write_baseline(name: &str, args: &Value) -> bool {
    match name {
        "exec_command"
        | "history_session_checkpoint"
        | "stage_commit"
        | "wait_stage_commit"
        | "remove_path"
        | "git_stage"
        | "git_commit"
        | "git_restore"
        | "git_reset"
        | "git_revert"
        | "git_clean" => true,
        "apply_patch" => !args
            .get("dry_run")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        _ => false,
    }
}

fn standalone_operation(name: &str) -> bool {
    matches!(
        name,
        "patch_check"
            | "apply_patch"
            | "remove_path"
            | "exec_command"
            | "git_stage"
            | "git_commit"
            | "git_restore"
            | "git_reset"
            | "git_revert"
            | "git_clean"
    )
}

fn should_log_operation(name: &str) -> bool {
    standalone_operation(name)
        || matches!(
            name,
            "git_status"
                | "git_diff"
                | "git_log"
                | "git_show"
                | "git_blame"
                | "history_session_checkpoint"
        )
}

fn operation_input(args: &Value) -> Value {
    json!({
        "arguments_present": !args.is_null(),
        "reason": args.get("reason")
    })
}

fn attach_harness_status(
    ctx: &ToolContext,
    mut output: Value,
    standalone: bool,
    session_id: Option<&str>,
) -> Value {
    let selected = ctx.task_for_session(session_id);
    if let Ok(mut status) = ctx
        .harness
        .status_for_task(selected.as_ref().map(|task| task.id.as_str()))
    {
        if standalone && status.task_id.is_none() {
            status.next_actions.clear();
        }
        status.next_actions = filter_exposed_actions(ctx, status.next_actions);
        if let Some(object) = output.as_object_mut() {
            object.insert(
                "harness".into(),
                serde_json::to_value(status).unwrap_or_else(|_| {
                    json!({
                        "status": "unavailable",
                        "reason": "无法序列化 Harness 状态"
                    })
                }),
            );
            if standalone {
                attach_standalone_metadata(
                    &mut output,
                    "命令未成功；请检查 stderr、exit_code 或调整参数后重试。",
                );
            }
        }
    }
    output
}

fn attach_standalone_metadata(output: &mut Value, recovery_hint: &str) {
    if let Some(object) = output.as_object_mut() {
        object.insert("harness_mode".into(), Value::String("standalone".into()));
        object.insert("task_required".into(), Value::Bool(false));
        object.insert("next_actions".into(), json!([]));
        object.insert(
            "recovery_hint".into(),
            Value::String(recovery_hint.to_string()),
        );
    }
}

fn filter_exposed_actions(ctx: &ToolContext, actions: Vec<String>) -> Vec<String> {
    let exposed = crate::tools::registry::exposed_tool_names(&ctx.tool_profile);
    actions
        .into_iter()
        .filter(|action| exposed.contains(&action.as_str()))
        .collect()
}

pub fn server_info(ctx: &ToolContext) -> Result<Value, WorkspaceError> {
    server_info_for_session(ctx, None)
}

fn server_info_for_session(
    ctx: &ToolContext,
    session_id: Option<&str>,
) -> Result<Value, WorkspaceError> {
    let current_catalog = crate::tools::catalog::build_effective_catalog(ctx)?;
    let published_catalog = ctx.published_catalog();
    let running_catalog = published_catalog.as_ref().unwrap_or(&current_catalog);
    let catalog_changed = running_catalog.digest != current_catalog.digest;
    let tools = running_catalog
        .tools
        .iter()
        .filter_map(|tool| tool.get("name").and_then(Value::as_str))
        .collect::<Vec<_>>();
    let tool_groups = tool_group_manifest(&tools);
    let current_tools = current_catalog
        .tools
        .iter()
        .filter_map(|tool| tool.get("name").and_then(Value::as_str))
        .collect::<Vec<_>>();
    let current_tool_groups = tool_group_manifest(&current_tools);
    let command_sessions = ctx.sessions.list_snapshots(true, 0);
    let running_command_sessions = command_sessions
        .iter()
        .filter(|session| session.get("execution_status") == Some(&json!("running")))
        .count();
    let downstream_mcp = ctx.mcp_proxies.status();
    let command_cost_policy = json!({
        "external_paid_commands_enabled": ctx.policy.external_paid_commands_enabled,
        "external_paid_max_runs_per_day": ctx.policy.external_paid_max_runs_per_day,
        "external_paid_max_duration_seconds": ctx.policy.external_paid_max_duration_seconds,
        "workspace_policy_path": ".anchor/command-policy.yml",
        "approval_source": "trusted_runtime_config"
    });
    let connection_layers = json!({
        "plugin": {
            "status": "healthy",
            "server": crate::brand::SERVER_NAME,
            "version": env!("CARGO_PKG_VERSION"),
            "catalog_changed": catalog_changed,
            "reconnect_required": catalog_changed
        },
        "application": {
            "status": "healthy",
            "workspace": ctx.workspace.root_display(),
            "listener_path": "/mcp"
        },
        "authentication": {
            "status": if ctx.auth.auth_enabled() { "configured" } else { "disabled" },
            "type": ctx.auth.auth_type,
            "token_issuer": "workspace_listener",
            "refresh_strategy": if ctx.auth.auth_type == "oauth" {
                "rotating_refresh_token_with_persisted_replay_protection"
            } else {
                "not_applicable"
            }
        },
        "execution": {
            "status": "available",
            "retained_session_count": command_sessions.len(),
            "running_session_count": running_command_sessions,
            "sessions_process_bound": true
        },
        "downstream_mcp": downstream_mcp.clone()
    });
    Ok(tool_ok(json!({
        "server": crate::brand::SERVER_NAME,
        "title": crate::brand::PRODUCT_NAME,
        "version": env!("CARGO_PKG_VERSION"),
        "protocol_version": crate::mcp::protocol::CURRENT_PROTOCOL_VERSION,
        "workspace": ctx.workspace.root_display(),
        "permission_mode": ctx.permission_mode,
        "default_cwd": ctx.default_cwd_display_for(session_id),
        "network_allowed": ctx.policy.network_allowed(),
        "tool_profile": ctx.tool_profile,
        "auth_enabled": ctx.auth.auth_enabled(),
        "auth_type": ctx.auth.auth_type,
        "endpoint_path": "/mcp",
        "tools": tools,
        "tool_count": tools.len(),
        "tool_groups": tool_groups,
        "current_tools": current_tools,
        "current_tool_count": current_tools.len(),
        "current_tool_groups": current_tool_groups,
        "catalog_digest": running_catalog.digest,
        "running_catalog_digest": running_catalog.digest,
        "current_catalog_digest": current_catalog.digest,
        "catalog_published": published_catalog.is_some(),
        "catalog_changed": catalog_changed,
        "reconnect_required": catalog_changed,
        "catalog_version": crate::tools::registry::CATALOG_VERSION,
        "catalog_bytes": running_catalog.total_bytes,
        "catalog_estimated_tokens": running_catalog.estimated_tokens,
        "local_tool_count": running_catalog.local_count,
        "proxy_tool_count": running_catalog.proxy_count,
        "current_catalog_bytes": current_catalog.total_bytes,
        "current_catalog_estimated_tokens": current_catalog.estimated_tokens,
        "current_local_tool_count": current_catalog.local_count,
        "current_proxy_tool_count": current_catalog.proxy_count,
        "command_cost_policy": command_cost_policy,
        "downstream_mcp": downstream_mcp.clone(),
        "connection_layers": connection_layers
    })))
}

fn tool_group_manifest(tools: &[&str]) -> Value {
    let mut workspace = Vec::new();
    let mut git = Vec::new();
    let mut command = Vec::new();
    let mut task = Vec::new();
    let mut skills = Vec::new();
    let mut browser_proxy = Vec::new();
    let mut service = Vec::new();
    for tool in tools {
        let target = if tool.starts_with("git_") {
            &mut git
        } else if matches!(
            *tool,
            "exec_command"
                | "exec_health_check"
                | "wait_command"
                | "list_command_sessions"
                | "write_stdin"
                | "read_output"
                | "kill_session"
                | "check_exec_environment"
        ) {
            &mut command
        } else if matches!(
            *tool,
            "harness_status"
                | "project_state"
                | "start_task"
                | "update_task"
                | "pause_task"
                | "resume_task"
                | "finish_task"
                | "begin_work_session"
                | "close_work_session"
                | "task_context"
                | "list_task_events"
                | "change_summary"
                | "stage_commit"
                | "stage_commit_status"
                | "wait_stage_commit"
                | "update_verification_disposition"
                | "accept_current_baseline"
                | "refresh_baseline"
                | "operation_log"
                | "history_session_bootstrap"
                | "history_session_checkpoint"
                | "history_session_validate"
        ) {
            &mut task
        } else if matches!(*tool, "list_skills" | "load_skill" | "read_skill_resource") {
            &mut skills
        } else if tool.contains("browser")
            || tool.ends_with("__health_check")
            || tool.ends_with("__reconnect")
            || tool.ends_with("__reset_session")
        {
            &mut browser_proxy
        } else if matches!(*tool, "server_info" | "get_default_cwd" | "set_default_cwd") {
            &mut service
        } else {
            &mut workspace
        };
        target.push((*tool).to_string());
    }
    json!({
        "core": tools,
        "workspace": workspace,
        "git": git,
        "command": command,
        "task": task,
        "skills": skills,
        "browser_proxy": browser_proxy,
        "service": service
    })
}

pub fn check_exec_environment(ctx: &ToolContext) -> Result<Value, WorkspaceError> {
    let boundary_error = ctx.workspace.ensure_child_process_boundary().err();
    let workspace_exec_available = boundary_error.is_none();
    let mut warnings = vec!["Workspace 子进程尚未启用操作系统级文件系统沙箱".to_string()];
    if let Some(error) = &boundary_error {
        warnings.push(error.message());
    }
    let development_environment = crate::tools::environment::diagnose(ctx.workspace.root());
    let healthy = workspace_exec_available && development_environment["host_healthy"] == true;
    Ok(tool_ok(json!({
        "healthy": healthy,
        "status": if healthy { "healthy" } else { "degraded" },
        "retryable": !healthy,
        "workspace": ctx.workspace.root_display(),
        "permission_mode": ctx.permission_mode,
        "network_allowed": ctx.policy.network_allowed(),
        "landlock_enabled": false,
        "filesystem_sandbox": {
            "available": false,
            "enforced": false,
            "default_scope": "workspace",
            "host_scope_available": false
        },
        "global_tmp_write": if ctx.permission_mode == "dangerous" { "allowed" } else { "tmp-prefix" },
        "workspace_exec_available": workspace_exec_available,
        "workspace_read_boundary": if ctx.workspace.strict_read_boundary() { "strict" } else { "operator_override" },
        "external_reads_allowed": !ctx.workspace.strict_read_boundary(),
        "workspace_exec_sandbox_enforced": false,
        "workspace_exec_boundary": "policy_only",
        "workspace_link_guard": {
            "safe": workspace_exec_available,
            "scope": "recursive_reparse_points",
            "maximum_entries": 250000,
            "message": boundary_error.as_ref().map(WorkspaceError::message).unwrap_or_default()
        },
        "system_command_allowlist": ctx.policy.allowed_commands.iter().cloned().collect::<Vec<_>>(),
        "workspace_local_entries": {
            "enabled": ctx.policy.workspace_local_entries,
            "script_extensions": ctx.policy.workspace_script_extensions.iter().cloned().collect::<Vec<_>>(),
            "resolution": "workdir_first",
            "allowlist_required": true
        },
        "development_environment": development_environment,
        "warnings": warnings
    })))
}

pub fn get_default_cwd(ctx: &ToolContext) -> Result<Value, WorkspaceError> {
    get_default_cwd_for_session(ctx, None)
}

fn get_default_cwd_for_session(
    ctx: &ToolContext,
    session_id: Option<&str>,
) -> Result<Value, WorkspaceError> {
    Ok(tool_ok(json!({
        "workspace": ctx.workspace.root_display(),
        "default_cwd": ctx.default_cwd_display_for(session_id),
        "resolved_cwd": ctx.default_cwd_path_for(session_id).display().to_string()
    })))
}

pub fn set_default_cwd(ctx: &ToolContext, args: &Value) -> Result<Value, WorkspaceError> {
    set_default_cwd_for_session(ctx, None, args)
}

fn set_default_cwd_for_session(
    ctx: &ToolContext,
    session_id: Option<&str>,
    args: &Value,
) -> Result<Value, WorkspaceError> {
    let path = args.get("path").and_then(Value::as_str).unwrap_or(".");
    let resolved = ctx.workspace.resolve_existing(path)?;
    if !resolved.path.is_dir() {
        return Err(WorkspaceError::not_a_directory(
            "Default cwd must be a directory",
        ));
    }
    ctx.set_default_cwd_for(session_id, resolved.path.clone());
    Ok(tool_ok(json!({
        "workspace": ctx.workspace.root_display(),
        "default_cwd": resolved.display,
        "resolved_cwd": resolved.path.display().to_string()
    })))
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tempfile::tempdir;

    use super::{
        call_tool_with_cancellation, preserve_completed_result, record_verification_from_output,
        VerificationIdentity,
    };
    use crate::tools::{CancellationToken, ToolContext};

    #[test]
    fn late_cancellation_preserves_a_committed_success_result() {
        let cancellation = CancellationToken::default();
        cancellation.cancel();
        let result = preserve_completed_result(
            json!({"ok": true, "status": "success", "affected_files": ["probe.txt"]}),
            &cancellation,
        );
        assert_eq!(result["ok"], true);
        assert_eq!(result["cancellation_after_completion"], true);
        assert_eq!(result["affected_files"], json!(["probe.txt"]));
    }

    #[test]
    fn cancelled_file_scan_stops_before_work() {
        let workspace = tempdir().expect("workspace");
        let harness = tempdir().expect("harness");
        let ctx =
            ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                .expect("context");
        let cancellation = CancellationToken::default();
        cancellation.cancel();

        let result =
            call_tool_with_cancellation(&ctx, "list_files", &json!({"path": "."}), &cancellation);

        assert_eq!(result["ok"], false);
        assert_eq!(result["error"]["code"], "REQUEST_CANCELLED");
    }

    #[test]
    fn command_that_never_started_is_not_recorded_as_a_business_verification_failure() {
        let workspace = tempdir().expect("workspace");
        let harness = tempdir().expect("harness");
        let ctx =
            ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                .expect("context");
        let task = ctx.harness.start_task("policy rejection").expect("task");
        let mut output = json!({
            "ok": true,
            "status": "command_rejected",
            "termination_reason": "command_rejected",
            "execution_started": false,
            "command_ok": false
        });

        record_verification_from_output(
            &ctx,
            &task.id,
            VerificationIdentity {
                kind: "test",
                command: "git add story-live-model.test.ts",
                verification_key: Some("story-local"),
                test_file: Some("tests/story-live-model.test.ts"),
                test_name: Some("Story local test"),
                level: "blocking",
            },
            true,
            &mut output,
        );

        assert_eq!(output["verification_skipped"], true);
        assert_eq!(output["verification_skip_reason"], "command_not_executed");
        assert!(ctx
            .harness
            .list_verifications(&task.id)
            .expect("verifications")
            .is_empty());
    }

    #[test]
    fn terminal_nonzero_command_advances_expected_workspace_state() {
        let workspace = tempdir().expect("workspace");
        let harness = tempdir().expect("harness");
        let ctx =
            ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                .expect("context");
        ctx.harness.start_task("failing mutation").expect("task");

        let result = call_tool_with_cancellation(
            &ctx,
            "exec_command",
            &json!({
                "cmd": "python -c \"from pathlib import Path; Path('failed-output.txt').write_text('changed'); raise SystemExit(3)\"",
                "timeout_ms": 10_000,
                "yield_time_ms": 10_000
            }),
            &CancellationToken::default(),
        );

        assert_eq!(result["ok"], false, "{result}");
        assert_eq!(result["command_ok"], false, "{result}");
        assert_eq!(result["execution_started"], true, "{result}");
        assert!(workspace.path().join("failed-output.txt").is_file());
        assert_eq!(
            ctx.harness.status().expect("status").baseline_matches,
            Some(true)
        );
    }
}
