use std::path::Path;

use serde_json::{json, Value};

use crate::tools::context::ToolContext;
use crate::tools::policy::{validate_tool_arguments_for_workspace, PolicyError};
use crate::tools::workspace::{tool_err, tool_err_code, tool_ok, WorkspaceError};
use crate::tools::{exec, file, git, history, image_tool, patch, session, CancellationToken};

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
    let (reason, suggestion) = if dangerous.is_some() {
        (
            "dangerous_mode_required",
            "模型参数不能作为用户批准凭证；请由操作者在受信任控制面将权限模式切换为 dangerous 后重试",
        )
    } else if message.contains("allowlisted") {
        ("command_rejected", "改用允许的命令，或调整工作区命令白名单")
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
        retryable: false,
        details: json!({
            "stage": "policy",
            "reason": reason,
            "recoverable": false,
            "suggestion": suggestion
        }),
    })
}

fn advances_expected_state(name: &str) -> bool {
    matches!(name, "exec_command" | "apply_patch")
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
        return cancelled_tool_result();
    }
    if validate_schema {
        if let Err(error) = crate::tools::schema::validate_tool_input(name, args) {
            return tool_err(error);
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
        return policy_tool_err(e);
    }

    if crate::harness::tools::TOOL_NAMES.contains(&name) {
        return match crate::harness::tools::call(ctx, name, args, cancellation) {
            Ok(value) => value,
            Err(error) => attach_harness_status(ctx, tool_err(error), false),
        };
    }

    let active_task = ctx.harness.current_task().ok().flatten();
    let task_id = if requires_write_baseline(name, &effective_args) {
        if let Some(task) = active_task.as_ref() {
            if let Err(error) = ctx.harness.check_baseline(&task.id) {
                return attach_harness_status(
                    ctx,
                    tool_err_code(error.code(), error.to_string(), "permission"),
                    false,
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
        "read_skill_resource" => crate::skills::read_resource_tool(&ctx.skills, &effective_args),
        "check_exec_environment" => check_exec_environment(ctx),
        "exec_health_check" => exec::exec_health_check(ctx),
        "get_default_cwd" => get_default_cwd_for_session(ctx, session_id),
        "set_default_cwd" => set_default_cwd_for_session(ctx, session_id, &effective_args),
        "read_file" => file::read_file(ws, &effective_args, cancellation),
        "list_dir" => file::list_dir(ws, &effective_args, cancellation),
        "list_files" => file::list_files(ws, &effective_args, cancellation),
        "search_text" | "grep_text" | "grep" => {
            file::search_text(ws, &effective_args, cancellation)
        }
        "patch_check" => patch::patch_check(ctx, &effective_args),
        "apply_patch" => patch::apply_patch(ctx, &effective_args),
        "exec_command" => exec::exec_command_with_cancellation(ctx, &effective_args, cancellation),
        "read_output" => session::read_output(&ctx.sessions, &effective_args),
        "write_stdin" => session::write_stdin(&ctx.sessions, &effective_args),
        "kill_session" => session::kill_session(&ctx.sessions, &effective_args),
        "git_status" => git::git_status(ws, &effective_args),
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
        }
    }
    if output.get("ok").and_then(Value::as_bool) == Some(false) {
        output = attach_harness_status(ctx, output, task_id.is_none());
    }
    if let Some(task_id) = task_id.as_deref() {
        let succeeded = output.get("ok").and_then(Value::as_bool) == Some(true);
        let _ = ctx.harness.record_event(
            task_id,
            "operation_finished",
            Some(name),
            operation_input(args),
            json!({"ok": succeeded, "tool": name}),
        );
        if succeeded && advances_expected_state(name) {
            let operation_id = operation.as_ref().map(|operation| operation.id.as_str());
            let _ = ctx
                .harness
                .refresh_expected_state_for_operation(task_id, operation_id);
        }
    }
    if let Some(operation) = operation {
        let succeeded = output.get("ok").and_then(Value::as_bool) == Some(true);
        let _ = ctx.harness.record_operation(
            Some(&operation.id),
            active_task.as_ref().map(|task| task.id.as_str()),
            name,
            if succeeded { "completed" } else { "failed" },
            operation_input(args),
            json!({
                "ok": succeeded,
                "tool": name,
                "affected_files": output.get("affected_files")
            }),
        );
    }
    output
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
        "read_file" | "search_text" | "grep_text" | "grep" | "git_blame" | "view_image" => {
            if let Some(path) = effective.get("path").and_then(Value::as_str) {
                effective["path"] = Value::String(prefix_relative_path(&base, path));
            }
        }
        "git_diff" => {
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
        "exec_command" | "history_session_checkpoint" => true,
        "apply_patch" => !args
            .get("dry_run")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        _ => false,
    }
}

fn standalone_operation(name: &str) -> bool {
    matches!(name, "patch_check" | "apply_patch" | "exec_command")
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

fn attach_harness_status(ctx: &ToolContext, mut output: Value, standalone: bool) -> Value {
    if let Ok(mut status) = ctx.harness.status() {
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
    let catalog = crate::tools::catalog::build_effective_catalog(ctx)?;
    let tools = catalog
        .tools
        .iter()
        .filter_map(|tool| tool.get("name").and_then(Value::as_str))
        .collect::<Vec<_>>();
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
        "catalog_digest": catalog.digest,
        "catalog_bytes": catalog.total_bytes,
        "catalog_estimated_tokens": catalog.estimated_tokens,
        "local_tool_count": catalog.local_count,
        "proxy_tool_count": catalog.proxy_count,
        "downstream_mcp": ctx.mcp_proxies.status()
    })))
}

pub fn check_exec_environment(ctx: &ToolContext) -> Result<Value, WorkspaceError> {
    let boundary_error = ctx.workspace.ensure_child_process_boundary().err();
    let workspace_exec_available = boundary_error.is_none();
    let mut warnings = vec!["Workspace 子进程尚未启用操作系统级文件系统沙箱".to_string()];
    if let Some(error) = &boundary_error {
        warnings.push(error.message());
    }
    Ok(tool_ok(json!({
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
        // Backward-compatible alias for older MCP clients.
        "allowed_commands": ctx.policy.allowed_commands.iter().cloned().collect::<Vec<_>>(),
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

    use super::{call_tool_with_cancellation, preserve_completed_result};
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
        let ctx = ToolContext::for_test(
            workspace.path().to_path_buf(),
            harness.path().to_path_buf(),
        )
        .expect("context");
        let cancellation = CancellationToken::default();
        cancellation.cancel();

        let result = call_tool_with_cancellation(
            &ctx,
            "list_files",
            &json!({"path": "."}),
            &cancellation,
        );

        assert_eq!(result["ok"], false);
        assert_eq!(result["error"]["code"], "REQUEST_CANCELLED");
    }
}
