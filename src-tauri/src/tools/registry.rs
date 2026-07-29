use serde_json::{json, Value};

pub const P0_TOOLS: &[(&str, &str, &str, bool, bool, bool)] = &[
    (
        "harness_status",
        "Harness status",
        "Return durable task, workspace, capability, and recovery status.",
        true,
        false,
        false,
    ),
    (
        "operation_log",
        "Operation log",
        "Return Workspace-level operation history independent of Task state.",
        true,
        false,
        false,
    ),
    (
        "server_info",
        "Server info",
        "Return server, workspace, auth, profile, and exposed-tool metadata.",
        true,
        false,
        false,
    ),
    (
        "list_skills",
        "List Agent Skills",
        "Discover Agent Skills exposed by this workspace/profile. Returns only bounded metadata for progressive disclosure.",
        true,
        false,
        false,
    ),
    (
        "load_skill",
        "Load Agent Skill",
        "Load bounded instructions for one discovered skill and resolve its declared tool dependencies against the current MCP catalog. Declarations never grant permissions.",
        true,
        false,
        false,
    ),
    (
        "read_skill_resource",
        "Read Skill resource",
        "Read a supporting resource or script source inside a discovered Skill directory without executing it.",
        true,
        false,
        false,
    ),
    (
        "history_session_bootstrap",
        "Initialize or restore development session",
        "At the start of every new ChatGPT conversation, call this exactly once before the first response, even when the user did not ask to restore. It creates the first history session when none exists, or returns ordered summaries plus the latest full handoff and resumes the current ChatGPT session without duplicates.",
        false,
        false,
        false,
    ),
    (
        "history_session_checkpoint",
        "Save development checkpoint",
        "Save or update one idempotent, redacted development handoff. Pass session_key and expected_path exactly as returned by history_session_bootstrap so changing host metadata cannot redirect the checkpoint. The turn_id is optional and generated deterministically when omitted.",
        false,
        false,
        false,
    ),
    (
        "history_session_validate",
        "Validate session archive",
        "Validate history numbering, files, session mappings, and optionally rebuild the derived index without deleting history.",
        false,
        false,
        false,
    ),
    (
        "project_state",
        "Project state",
        "Return the current project, task, change, and verification state.",
        true,
        false,
        false,
    ),
    (
        "start_task",
        "Start task",
        "Start a durable coding task and capture the workspace baseline.",
        false,
        false,
        false,
    ),
    (
        "update_task",
        "Update task",
        "Update task steps and durable progress.",
        false,
        false,
        false,
    ),
    (
        "pause_task",
        "Pause task",
        "Pause the active coding task.",
        false,
        false,
        false,
    ),
    (
        "resume_task",
        "Resume task",
        "Resume a paused or failed coding task.",
        false,
        false,
        false,
    ),
    (
        "finish_task",
        "Finish task",
        "Finish a task with verification status and change summary.",
        false,
        false,
        false,
    ),
    (
        "task_context",
        "Task context",
        "Return a bounded durable task context for a new conversation.",
        true,
        false,
        false,
    ),
    (
        "list_task_events",
        "List task events",
        "Read task event history with pagination.",
        true,
        false,
        false,
    ),
    (
        "change_summary",
        "Change summary",
        "Explain what changed, why, and what evidence exists.",
        true,
        false,
        false,
    ),
    (
        "check_exec_environment",
        "Check exec environment",
        "Return lightweight exec_command sandbox and environment status known to the server.",
        true,
        false,
        false,
    ),
    (
        "exec_health_check",
        "Exec health check",
        "Verify the exec worker, session creation, command execution, and stdout/stderr capture.",
        true,
        false,
        false,
    ),
    (
        "get_default_cwd",
        "Get default cwd",
        "Return the current default cwd inside the workspace.",
        true,
        false,
        false,
    ),
    (
        "set_default_cwd",
        "Set default cwd",
        "Set the default cwd for relative tool paths inside the workspace.",
        false,
        false,
        false,
    ),
    (
        "read_file",
        "Read file",
        "Read a UTF-8 or BOM-marked UTF-16 text file slice strictly inside the configured workspace.",
        true,
        false,
        false,
    ),
    (
        "list_dir",
        "List directory",
        "List directory entries inside the configured workspace.",
        true,
        false,
        false,
    ),
    (
        "list_files",
        "List files",
        "List workspace files using glob filters.",
        true,
        false,
        false,
    ),
    (
        "search_text",
        "Search text",
        "Search UTF-8 or BOM-marked UTF-16 workspace files for text or regex matches.",
        true,
        false,
        false,
    ),
    (
        "apply_patch",
        "Apply patch",
        "Apply a patch envelope transactionally inside the workspace.",
        false,
        true,
        false,
    ),
    (
        "patch_check",
        "Check patch",
        "Validate a patch without changing the workspace.",
        true,
        false,
        false,
    ),
    (
        "exec_command",
        "Execute command",
        "Run a bounded command in the workspace under runtime policy.",
        false,
        true,
        true,
    ),
    (
        "write_stdin",
        "Write stdin",
        "Write characters to a server-managed running command session.",
        false,
        true,
        false,
    ),
    (
        "kill_session",
        "Kill session",
        "Terminate a server-managed running command session.",
        false,
        true,
        false,
    ),
    (
        "read_output",
        "Read output",
        "Read retained stdout or stderr by output_ref with per-stream byte offset pagination.",
        true,
        false,
        false,
    ),
    (
        "git_status",
        "Git status",
        "Return git working tree status for the workspace.",
        true,
        false,
        false,
    ),
    (
        "git_diff",
        "Git diff",
        "Return unified git diff for workspace changes.",
        true,
        false,
        false,
    ),
    (
        "git_log",
        "Git log",
        "Return recent git commits with bounded structured metadata.",
        true,
        false,
        false,
    ),
    (
        "git_show",
        "Git show",
        "Return bounded git show output for a revision.",
        true,
        false,
        false,
    ),
    (
        "git_blame",
        "Git blame",
        "Return bounded git blame metadata for a workspace file.",
        true,
        false,
        false,
    ),
    (
        "view_image",
        "View image",
        "Return an image strictly inside the configured workspace as MCP image content.",
        true,
        false,
        false,
    ),
];

/// old Python 版本默认提供的核心工具集。默认 MCP 只暴露这一组，保持 Agent 的工具面稳定。
pub const CORE_TOOLS: &[&str] = &[
    "server_info",
    "list_skills",
    "load_skill",
    "read_skill_resource",
    "history_session_bootstrap",
    "history_session_checkpoint",
    "history_session_validate",
    "check_exec_environment",
    "get_default_cwd",
    "set_default_cwd",
    "read_file",
    "list_dir",
    "list_files",
    "search_text",
    "apply_patch",
    "exec_command",
    "write_stdin",
    "kill_session",
    "read_output",
    "git_status",
    "git_diff",
    "git_log",
    "git_show",
    "git_blame",
    "view_image",
];

pub const CORE_READ_ONLY_TOOLS: &[&str] = &[
    "server_info",
    "list_skills",
    "load_skill",
    "read_skill_resource",
    "check_exec_environment",
    "get_default_cwd",
    "read_file",
    "list_dir",
    "list_files",
    "search_text",
    "read_output",
    "git_status",
    "git_diff",
    "git_log",
    "git_show",
    "git_blame",
    "view_image",
];

pub const ALLOWED_TOOLS: &[&str] = &[
    "harness_status",
    "operation_log",
    "server_info",
    "list_skills",
    "load_skill",
    "read_skill_resource",
    "history_session_bootstrap",
    "history_session_checkpoint",
    "history_session_validate",
    "check_exec_environment",
    "exec_health_check",
    "get_default_cwd",
    "set_default_cwd",
    "read_file",
    "list_dir",
    "list_files",
    "search_text",
    "grep_text",
    "grep",
    "apply_patch",
    "patch_check",
    "exec_command",
    "write_stdin",
    "kill_session",
    "read_output",
    "git_status",
    "git_diff",
    "git_log",
    "git_show",
    "git_blame",
    "project_state",
    "start_task",
    "update_task",
    "pause_task",
    "resume_task",
    "finish_task",
    "task_context",
    "list_task_events",
    "change_summary",
    "view_image",
];

pub const MUTATING_TOOLS: &[&str] = &[
    "history_session_bootstrap",
    "history_session_checkpoint",
    "history_session_validate",
    "apply_patch",
    "exec_command",
    "write_stdin",
    "kill_session",
    "set_default_cwd",
    "start_task",
    "update_task",
    "pause_task",
    "resume_task",
    "finish_task",
];

pub const READ_ONLY_TOOLS: &[&str] = &[
    "harness_status",
    "operation_log",
    "server_info",
    "list_skills",
    "load_skill",
    "read_skill_resource",
    "check_exec_environment",
    "exec_health_check",
    "get_default_cwd",
    "read_file",
    "list_dir",
    "list_files",
    "search_text",
    "grep_text",
    "grep",
    "read_output",
    "git_status",
    "git_diff",
    "git_log",
    "git_show",
    "git_blame",
    "view_image",
    "patch_check",
    "project_state",
    "task_context",
    "list_task_events",
    "change_summary",
];

pub fn is_allowed_tool(name: &str) -> bool {
    ALLOWED_TOOLS.contains(&name)
}

fn error_output_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "code": { "type": "string", "minLength": 1 },
            "message": { "type": "string", "minLength": 1 },
            "category": { "type": "string", "minLength": 1 },
            "retryable": { "type": "boolean" },
            "details": { "type": "object" }
        },
        "required": ["code", "message", "category", "retryable"],
        "additionalProperties": true
    })
}

fn success_output_schema(properties: Value, success_required: &[&str]) -> Value {
    let mut properties = properties.as_object().cloned().unwrap_or_default();
    properties.insert("ok".into(), json!({ "type": "boolean" }));
    properties.insert("error".into(), error_output_schema());
    let mut required = vec!["ok"];
    required.extend_from_slice(success_required);
    json!({
        "type": "object",
        "properties": properties,
        "required": ["ok"],
        "allOf": [
            {
                "if": {
                    "properties": { "ok": { "const": true } },
                    "required": ["ok"]
                },
                "then": { "required": required }
            },
            {
                "if": {
                    "properties": { "ok": { "const": false } },
                    "required": ["ok"]
                },
                "then": { "required": ["error"] }
            }
        ],
        "additionalProperties": true
    })
}

fn append_output_condition(mut schema: Value, condition: Value) -> Value {
    schema["allOf"]
        .as_array_mut()
        .expect("output schema allOf")
        .push(condition);
    schema
}

fn warnings_property() -> Value {
    json!({ "type": "array", "items": { "type": "string" } })
}

fn nullable_integer_property() -> Value {
    json!({ "type": ["integer", "null"] })
}

fn merge_schema_properties(chunks: Vec<Value>) -> Value {
    let mut properties = serde_json::Map::new();
    for chunk in chunks {
        properties.extend(
            chunk
                .as_object()
                .cloned()
                .expect("schema property chunk is an object"),
        );
    }
    Value::Object(properties)
}

fn history_bootstrap_output_properties() -> Value {
    merge_schema_properties(vec![
        json!({
            "is_new_session": { "type": "boolean" },
            "session_key": { "type": "string", "minLength": 1 },
            "session_key_source": { "type": "string", "minLength": 1 },
            "host_session_key_mismatch": { "type": "boolean" },
            "history_numbers": { "type": "array", "maxItems": 256, "items": { "type": "integer", "minimum": 1 } },
            "history_numbers_total": { "type": "integer", "minimum": 0 },
            "history_numbers_truncated": { "type": "boolean" },
            "history_count": { "type": "integer", "minimum": 0 },
            "history_summaries_returned": { "type": "integer", "minimum": 0 },
            "history_summaries_omitted": { "type": "integer", "minimum": 0 },
            "history_summary_truncated": { "type": "boolean" },
            "latest_prior_number": nullable_integer_property(),
            "latest_prior_path": { "type": ["string", "null"] },
            "latest_prior_status": { "type": ["string", "null"], "enum": ["active", "paused", "completed", "unknown", null] },
            "latest_completed_number": nullable_integer_property(),
            "latest_completed_path": { "type": ["string", "null"] }
        }),
        json!({
            "current_number": { "type": "integer", "minimum": 1 },
            "current_path": { "type": "string", "minLength": 1 },
            "session_status": { "type": "string", "enum": ["active", "paused", "completed", "unknown"] },
            "previous_status": { "type": "string", "enum": ["active", "paused", "completed", "unknown"] },
            "reactivated": { "type": "boolean" },
            "checkpoint_count": { "type": "integer", "minimum": 0 },
            "created": { "type": "boolean" },
            "resumed": { "type": "boolean" },
            "sequence_valid": { "type": "boolean" },
            "all_history_summary": { "type": "string", "maxLength": 60000 },
            "inherited_summary": { "type": ["string", "null"] },
            "latest_handoff": { "type": ["string", "null"], "maxLength": 64000 },
            "latest_handoff_truncated": { "type": "boolean" },
            "latest_handoff_total_bytes": { "type": "integer", "minimum": 0 }
        }),
        json!({
            "session_summaries": {
                "type": "array",
                "maxItems": 64,
                "items": {
                    "type": "object",
                    "properties": {
                        "number": { "type": "integer", "minimum": 1 },
                        "path": { "type": "string", "minLength": 1 },
                        "status": { "type": "string", "enum": ["active", "paused", "completed", "unknown"] },
                        "updated_at": { "type": ["string", "null"] },
                        "size_bytes": { "type": "integer", "minimum": 0 },
                        "summary": { "type": "string", "maxLength": 3000 },
                        "summary_truncated": { "type": "boolean" }
                    },
                    "required": ["number", "path", "status", "updated_at", "size_bytes", "summary", "summary_truncated"],
                    "additionalProperties": false
                }
            },
            "history_read_mode": { "type": "string", "minLength": 1 },
            "total_history_bytes": { "type": "integer", "minimum": 0 },
            "full_history_included": { "type": "boolean", "const": false },
            "history_digest": { "type": "string", "minLength": 64, "maxLength": 64 },
            "persistence_mode": { "type": "string", "minLength": 1 },
            "assistant_instructions": { "type": "string", "minLength": 1 },
            "required_next_actions": { "type": "array", "items": { "type": "string" } },
            "checkpoint_policy": { "type": "object" },
            "warnings": warnings_property()
        }),
    ])
}

fn session_snapshot_output_schema() -> Value {
    success_output_schema(
        json!({
            "session_id": { "type": "string", "minLength": 1 },
            "interactive": { "type": "boolean" },
            "stdin_open": { "type": "boolean" },
            "status": { "type": "string", "minLength": 1 },
            "termination_reason": { "type": "string", "minLength": 1 },
            "recoverable": { "type": "boolean" },
            "suggestion": { "type": "string" },
            "exit_code": nullable_integer_property(),
            "transport_ok": { "type": "boolean" },
            "command_ok": { "type": ["boolean", "null"] },
            "stdout": { "type": "string" },
            "stderr": { "type": "string" },
            "stdout_truncated": { "type": "boolean" },
            "stderr_truncated": { "type": "boolean" },
            "elapsed_ms": { "type": "integer", "minimum": 0 },
            "output_refs": {
                "type": "object",
                "properties": {
                    "stdout": { "type": "string", "minLength": 1 },
                    "stderr": { "type": "string", "minLength": 1 }
                },
                "required": ["stdout", "stderr"],
                "additionalProperties": false
            },
            "warnings": warnings_property()
        }),
        &[
            "session_id",
            "interactive",
            "stdin_open",
            "status",
            "termination_reason",
            "recoverable",
            "suggestion",
            "exit_code",
            "transport_ok",
            "command_ok",
            "stdout",
            "stderr",
            "stdout_truncated",
            "stderr_truncated",
            "elapsed_ms",
            "output_refs",
        ],
    )
}

pub fn output_schema(name: &str) -> Value {
    match canonical_tool_name(name) {
        "server_info" => success_output_schema(
            json!({
                "server": { "type": "string", "const": crate::brand::SERVER_NAME },
                "title": { "type": "string", "minLength": 1 },
                "version": { "type": "string", "minLength": 1 },
                "protocol_version": { "type": "string", "minLength": 1 },
                "workspace": { "type": "string", "minLength": 1 },
                "permission_mode": { "type": "string", "minLength": 1 },
                "default_cwd": { "type": "string", "minLength": 1 },
                "network_allowed": { "type": "boolean" },
                "tool_profile": { "type": "string", "enum": ["core", "read-only", "advanced"] },
                "auth_enabled": { "type": "boolean" },
                "auth_type": { "type": "string", "minLength": 1 },
                "endpoint_path": { "type": "string", "const": "/mcp" },
                "tools": { "type": "array", "items": { "type": "string" } },
                "tool_count": { "type": "integer", "minimum": 0 },
                "catalog_digest": { "type": "string", "minLength": 64, "maxLength": 64 },
                "catalog_bytes": { "type": "integer", "minimum": 0 },
                "catalog_estimated_tokens": { "type": "integer", "minimum": 0 },
                "local_tool_count": { "type": "integer", "minimum": 0 },
                "proxy_tool_count": { "type": "integer", "minimum": 0 }
            }),
            &[
                "server",
                "title",
                "version",
                "protocol_version",
                "workspace",
                "permission_mode",
                "default_cwd",
                "network_allowed",
                "tool_profile",
                "auth_enabled",
                "auth_type",
                "endpoint_path",
                "tools",
                "tool_count",
                "catalog_digest",
                "catalog_bytes",
                "catalog_estimated_tokens",
                "local_tool_count",
                "proxy_tool_count",
            ],
        ),
        "read_file" => success_output_schema(
            json!({
                "path": { "type": "string" },
                "content": { "type": "string" },
                "encoding": { "type": "string", "enum": ["utf-8", "utf-16le", "utf-16be"] },
                "start_line": { "type": "integer", "minimum": 1 },
                "end_line": { "type": "integer", "minimum": 0 },
                "total_lines": { "type": "integer", "minimum": 0 },
                "total_bytes": { "type": "integer", "minimum": 0 },
                "bytes_read": { "type": "integer", "minimum": 0 },
                "truncated": { "type": "boolean" },
                "truncated_by": { "type": ["string", "null"] },
                "warnings": warnings_property()
            }),
            &[
                "path",
                "content",
                "encoding",
                "start_line",
                "end_line",
                "total_lines",
                "total_bytes",
                "bytes_read",
                "truncated",
                "truncated_by",
                "warnings",
            ],
        ),
        "search_text" | "grep_text" => success_output_schema(
            json!({
                "query": { "type": "string" },
                "matches": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "path": { "type": "string" },
                            "line": { "type": "integer", "minimum": 1 },
                            "column": { "type": "integer", "minimum": 1 },
                            "preview": { "type": "string" },
                            "before": { "type": "array", "items": { "type": "string" } },
                            "after": { "type": "array", "items": { "type": "string" } }
                        },
                        "required": ["path", "line", "column", "preview"],
                        "additionalProperties": true
                    }
                },
                "total_matches": { "type": "integer", "minimum": 0 },
                "truncated": { "type": "boolean" },
                "warnings": warnings_property()
            }),
            &["query", "matches", "total_matches", "truncated", "warnings"],
        ),
        "apply_patch" => append_output_condition(
            append_output_condition(
                success_output_schema(
                    json!({
                        "dry_run": { "type": "boolean" },
                        "preflight": { "type": "boolean" },
                        "clean": { "type": "boolean" },
                        "change_id": { "type": "string", "minLength": 1 },
                        "summary": { "type": "string" },
                        "affected_files": { "type": "array", "items": { "type": "object" } },
                        "files_created": { "type": "array", "items": { "type": "string" } },
                        "files_modified": { "type": "array", "items": { "type": "string" } },
                        "files_deleted": { "type": "array", "items": { "type": "string" } },
                        "would_create": { "type": "array", "items": { "type": "string" } },
                        "would_modify": { "type": "array", "items": { "type": "string" } },
                        "would_delete": { "type": "array", "items": { "type": "string" } },
                        "recovery": { "type": "string", "minLength": 1 },
                        "warnings": warnings_property()
                    }),
                    &["dry_run", "clean", "summary", "affected_files", "warnings"],
                ),
                json!({
                    "if": {
                        "properties": {
                            "ok": { "const": true },
                            "dry_run": { "const": true }
                        },
                        "required": ["ok", "dry_run"]
                    },
                    "then": {
                        "required": ["preflight", "would_create", "would_modify", "would_delete"]
                    }
                }),
            ),
            json!({
                "if": {
                    "properties": {
                        "ok": { "const": true },
                        "dry_run": { "const": false }
                    },
                    "required": ["ok", "dry_run"]
                },
                "then": {
                    "required": [
                        "change_id", "files_created", "files_modified", "files_deleted", "recovery"
                    ]
                }
            }),
        ),
        "patch_check" => success_output_schema(
            json!({
                "dry_run": { "type": "boolean", "const": true },
                "preflight": { "type": "boolean", "const": true },
                "clean": { "type": "boolean" },
                "summary": { "type": "string" },
                "affected_files": { "type": "array", "items": { "type": "object" } },
                "would_create": { "type": "array", "items": { "type": "string" } },
                "would_modify": { "type": "array", "items": { "type": "string" } },
                "would_delete": { "type": "array", "items": { "type": "string" } },
                "warnings": warnings_property()
            }),
            &[
                "dry_run",
                "preflight",
                "clean",
                "summary",
                "affected_files",
                "would_create",
                "would_modify",
                "would_delete",
                "warnings",
            ],
        ),
        "exec_command" => success_output_schema(
            json!({
                "command": { "type": "string", "minLength": 1 },
                "resolved_cwd": { "type": "string", "minLength": 1 },
                "status": { "type": "string", "minLength": 1 },
                "termination_reason": { "type": "string", "minLength": 1 },
                "recoverable": { "type": "boolean" },
                "suggestion": { "type": "string" },
                "exit_code": nullable_integer_property(),
                "stdout": { "type": "string" },
                "stderr": { "type": "string" },
                "stdout_truncated": { "type": "boolean" },
                "stderr_truncated": { "type": "boolean" },
                "duration_ms": { "type": "integer", "minimum": 0 },
                "elapsed_ms": { "type": "integer", "minimum": 0 },
                "execution_mode": { "type": "string", "minLength": 1 },
                "filesystem_scope": { "type": "string", "const": "workspace" },
                "sandbox_enforced": { "type": "boolean" },
                "execution_boundary": { "type": "string", "minLength": 1 },
                "child_process": { "type": "boolean" },
                "transport_ok": { "type": "boolean" },
                "command_ok": { "type": ["boolean", "null"] },
                "warnings": warnings_property()
            }),
            &[
                "command",
                "resolved_cwd",
                "status",
                "termination_reason",
                "recoverable",
                "suggestion",
                "exit_code",
                "stdout",
                "stderr",
                "stdout_truncated",
                "stderr_truncated",
                "duration_ms",
                "elapsed_ms",
                "execution_mode",
                "filesystem_scope",
                "sandbox_enforced",
                "execution_boundary",
                "child_process",
                "transport_ok",
                "command_ok",
                "warnings",
            ],
        ),
        "write_stdin" => session_snapshot_output_schema(),
        "kill_session" => {
            let schema = session_snapshot_output_schema();
            append_output_condition(
                schema,
                json!({
                    "if": {
                        "properties": { "ok": { "const": true } },
                        "required": ["ok"]
                    },
                    "then": {
                        "properties": {
                            "killed": { "type": "boolean" },
                            "evicted": { "type": "boolean" }
                        },
                        "required": ["killed", "evicted"]
                    }
                }),
            )
        }
        "read_output" => success_output_schema(
            json!({
                "output_ref": { "type": "string", "minLength": 1 },
                "stream_output_ref": { "type": "string", "minLength": 1 },
                "stream": { "type": "string", "enum": ["stdout", "stderr"] },
                "offset": { "type": "integer", "minimum": 0 },
                "requested_offset": { "type": "integer", "minimum": 0 },
                "retained_start_offset": { "type": "integer", "minimum": 0 },
                "limit": { "type": "integer", "minimum": 1 },
                "content": { "type": "string" },
                "next_offset": nullable_integer_property(),
                "total_retained_bytes": { "type": "integer", "minimum": 0 },
                "total_stream_bytes": { "type": "integer", "minimum": 0 },
                "truncated": { "type": "boolean" },
                "warnings": warnings_property()
            }),
            &[
                "output_ref",
                "stream_output_ref",
                "stream",
                "offset",
                "requested_offset",
                "retained_start_offset",
                "limit",
                "content",
                "next_offset",
                "total_retained_bytes",
                "total_stream_bytes",
                "truncated",
                "warnings",
            ],
        ),
        "git_status" => success_output_schema(
            json!({
                "is_repo": { "type": "boolean" },
                "branch": { "type": "string" },
                "head": { "type": "string" },
                "upstream": { "type": "string" },
                "ahead": { "type": "integer", "minimum": 0 },
                "behind": { "type": "integer", "minimum": 0 },
                "clean": { "type": "boolean" },
                "entries": { "type": "array", "items": { "type": "object" } },
                "truncated": { "type": "boolean" },
                "warnings": warnings_property()
            }),
            &["is_repo", "clean", "entries", "warnings"],
        ),
        "history_session_bootstrap" => success_output_schema(
            history_bootstrap_output_properties(),
            &[
                "is_new_session",
                "session_key",
                "session_key_source",
                "host_session_key_mismatch",
                "history_numbers",
                "history_numbers_total",
                "history_numbers_truncated",
                "history_count",
                "history_summaries_returned",
                "history_summaries_omitted",
                "history_summary_truncated",
                "latest_prior_number",
                "latest_prior_path",
                "latest_prior_status",
                "latest_completed_number",
                "latest_completed_path",
                "current_number",
                "current_path",
                "session_status",
                "previous_status",
                "reactivated",
                "checkpoint_count",
                "created",
                "resumed",
                "sequence_valid",
                "all_history_summary",
                "inherited_summary",
                "session_summaries",
                "latest_handoff",
                "latest_handoff_truncated",
                "latest_handoff_total_bytes",
                "history_read_mode",
                "total_history_bytes",
                "full_history_included",
                "history_digest",
                "persistence_mode",
                "assistant_instructions",
                "required_next_actions",
                "checkpoint_policy",
                "warnings",
            ],
        ),
        "history_session_checkpoint" => success_output_schema(
            json!({
                "session_number": { "type": "integer", "minimum": 1 },
                "path": { "type": "string", "minLength": 1 },
                "session_key": { "type": "string", "minLength": 1 },
                "expected_path": { "type": "string", "minLength": 1 },
                "host_session_key_mismatch": { "type": "boolean" },
                "turn_id": { "type": "string", "minLength": 1 },
                "session_status": { "type": "string", "enum": ["active", "paused", "completed"] },
                "previous_status": { "type": "string", "enum": ["active", "paused", "completed", "unknown"] },
                "status_changed": { "type": "boolean" },
                "checkpoint_count": { "type": "integer", "minimum": 0 },
                "content_bytes": { "type": "integer", "minimum": 0 },
                "max_content_bytes": { "type": "integer", "minimum": 1 },
                "created": { "type": "boolean" },
                "updated": { "type": "boolean" },
                "duplicate_ignored": { "type": "boolean" },
                "content_hash": { "type": "string", "minLength": 64, "maxLength": 64 },
                "warnings": warnings_property()
            }),
            &[
                "session_number",
                "path",
                "session_key",
                "expected_path",
                "host_session_key_mismatch",
                "turn_id",
                "session_status",
                "previous_status",
                "status_changed",
                "checkpoint_count",
                "content_bytes",
                "max_content_bytes",
                "created",
                "updated",
                "duplicate_ignored",
                "content_hash",
                "warnings",
            ],
        ),
        "history_session_validate" => success_output_schema(
            json!({
                "sequence_valid": { "type": "boolean" },
                "numbers": { "type": "array", "items": { "type": "integer", "minimum": 1 } },
                "missing_numbers": { "type": "array", "items": { "type": "integer", "minimum": 1 } },
                "duplicate_session_keys": { "type": "array", "items": { "type": "string" } },
                "invalid_files": { "type": "array", "items": { "type": "string" } },
                "empty_files": { "type": "array", "items": { "type": "string" } },
                "latest_number": nullable_integer_property(),
                "latest_path": { "type": ["string", "null"] },
                "document_count": { "type": "integer", "minimum": 0 },
                "status_counts": {
                    "type": "object",
                    "properties": {
                        "active": { "type": "integer", "minimum": 0 },
                        "paused": { "type": "integer", "minimum": 0 },
                        "completed": { "type": "integer", "minimum": 0 },
                        "unknown": { "type": "integer", "minimum": 0 }
                    },
                    "required": ["active", "paused", "completed", "unknown"],
                    "additionalProperties": false
                },
                "total_history_bytes": { "type": "integer", "minimum": 0 },
                "largest_document_bytes": { "type": "integer", "minimum": 0 },
                "max_document_bytes": { "type": "integer", "minimum": 1 },
                "max_total_history_bytes": { "type": "integer", "minimum": 1 },
                "max_documents": { "type": "integer", "minimum": 1 },
                "index_status": { "type": "string", "minLength": 1 },
                "repaired": { "type": "boolean" },
                "warnings": warnings_property()
            }),
            &[
                "sequence_valid",
                "numbers",
                "missing_numbers",
                "duplicate_session_keys",
                "invalid_files",
                "empty_files",
                "latest_number",
                "latest_path",
                "document_count",
                "status_counts",
                "total_history_bytes",
                "largest_document_bytes",
                "max_document_bytes",
                "max_total_history_bytes",
                "max_documents",
                "index_status",
                "repaired",
                "warnings",
            ],
        ),
        "view_image" => success_output_schema(
            json!({
                "ok": { "type": "boolean" },
                "path": { "type": "string" },
                "mime_type": { "type": "string" },
                "bytes": { "type": "integer", "minimum": 0 },
                "width": { "type": "integer", "minimum": 1 },
                "height": { "type": "integer", "minimum": 1 },
                "resized": { "type": "boolean" },
                "original": { "type": "object" },
                "data_url": { "type": "string" },
                "warnings": { "type": "array", "items": { "type": "string" } },
                "error": { "type": "object" }
            }),
            &[
                "path",
                "mime_type",
                "bytes",
                "width",
                "height",
                "resized",
                "original",
                "warnings",
            ],
        ),
        _ => success_output_schema(json!({}), &[]),
    }
}

pub fn canonical_tool_name(name: &str) -> &str {
    match name {
        "grep" | "grep_text" => "search_text",
        _ => name,
    }
}

pub fn normalize_tool_profile(profile: &str) -> &'static str {
    match profile {
        "advanced" | "full" => "advanced",
        "core" => "core",
        "read-only" => "read-only",
        // Legacy compatibility profile used to expose mutating tools with false
        // read-only annotations. Preserve the stored value as a safe alias.
        "compat-readonly-all" => "read-only",
        _ => "core",
    }
}

pub fn exposed_tool_names(tool_profile: &str) -> Vec<&'static str> {
    match normalize_tool_profile(tool_profile) {
        "read-only" => CORE_READ_ONLY_TOOLS.to_vec(),
        "advanced" => P0_TOOLS.iter().map(|(name, ..)| *name).collect(),
        _ => CORE_TOOLS.to_vec(),
    }
}

pub fn list_tools() -> Vec<Value> {
    list_tools_for_profile("advanced")
}

pub fn list_tools_for_profile(tool_profile: &str) -> Vec<Value> {
    exposed_tool_names(tool_profile)
        .into_iter()
        .filter_map(|name| {
            P0_TOOLS.iter().find(|(n, ..)| *n == name).map(|entry| {
                let (name, title, description, read_only, destructive, open_world) = *entry;
                json!({
                    "name": name,
                    "title": title,
                    "description": description,
                    "inputSchema": input_schema(name),
                    "outputSchema": output_schema(name),
                    "annotations": {
                        "title": title,
                        "readOnlyHint": read_only,
                        "destructiveHint": destructive,
                        "idempotentHint": read_only,
                        "openWorldHint": open_world
                    }
                })
            })
        })
        .collect()
}

pub fn input_schema(name: &str) -> Value {
    match name {
        "list_skills" => json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "Optional case-insensitive name/description filter" },
                "max_results": { "type": "integer", "minimum": 1, "maximum": 200, "default": 100 }
            },
            "additionalProperties": false
        }),
        "load_skill" => json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "minLength": 1 },
                "start_line": { "type": "integer", "minimum": 1, "default": 1 },
                "end_line": { "type": "integer", "minimum": 1 },
                "max_bytes": { "type": "integer", "minimum": 1, "maximum": 131072, "default": 65536 }
            },
            "required": ["name"],
            "additionalProperties": false
        }),
        "read_skill_resource" => json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "minLength": 1 },
                "path": { "type": "string", "minLength": 1 },
                "start_line": { "type": "integer", "minimum": 1 },
                "end_line": { "type": "integer", "minimum": 1 },
                "max_bytes": { "type": "integer", "minimum": 1, "maximum": 1048576, "default": 262144 }
            },
            "required": ["name", "path"],
            "additionalProperties": false
        }),
        "history_session_bootstrap" => json!({
            "type": "object",
            "properties": {
                "workspace_root": { "type": "string", "minLength": 1 },
                "session_key": { "type": "string", "minLength": 1, "maxLength": 256 },
                "title": { "type": "string", "maxLength": 200 },
                "history_dir": { "type": "string", "default": "docs/history-session" },
                "create_if_missing": { "type": "boolean", "default": true }
            },
            "additionalProperties": false
        }),
        "history_session_checkpoint" => json!({
            "type": "object",
            "required": ["session_key", "expected_path"],
            "properties": {
                "workspace_root": { "type": "string", "minLength": 1 },
                "session_key": { "type": "string", "minLength": 1, "maxLength": 256 },
                "expected_path": { "type": "string", "minLength": 1, "maxLength": 1024 },
                "history_dir": { "type": "string", "default": "docs/history-session" },
                "turn_id": { "type": "string", "minLength": 1, "maxLength": 128 },
                "timestamp": { "type": "string", "maxLength": 128 },
                "session_status": { "type": "string", "enum": ["active", "paused", "completed"] },
                "user_intent": { "type": "string", "maxLength": 4000 },
                "findings": { "type": "array", "maxItems": 64, "items": { "type": "string", "maxLength": 2000 } },
                "decisions": { "type": "array", "maxItems": 64, "items": { "type": "string", "maxLength": 2000 } },
                "files_changed": { "type": "array", "maxItems": 64, "items": { "type": "string", "maxLength": 2000 } },
                "tests": { "type": "array", "maxItems": 64, "items": { "type": "string", "maxLength": 2000 } },
                "runtime_state": { "type": "array", "maxItems": 64, "items": { "type": "string", "maxLength": 2000 } },
                "remaining_issues": { "type": "array", "maxItems": 64, "items": { "type": "string", "maxLength": 2000 } },
                "next_actions": { "type": "array", "maxItems": 64, "items": { "type": "string", "maxLength": 2000 } },
                "notes": { "type": "string", "maxLength": 8000 }
            },
            "additionalProperties": false
        }),
        "history_session_validate" => json!({
            "type": "object",
            "properties": {
                "workspace_root": { "type": "string", "minLength": 1 },
                "history_dir": { "type": "string", "default": "docs/history-session" },
                "repair": { "type": "boolean", "default": false }
            },
            "additionalProperties": false
        }),
        "harness_status" => json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        }),
        "exec_health_check" => json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        }),
        "operation_log" => json!({
            "type": "object",
            "properties": {
                "cursor": { "type": "integer", "minimum": 0, "default": 0 },
                "limit": { "type": "integer", "minimum": 1, "maximum": 200, "default": 50 }
            },
            "additionalProperties": false
        }),
        "project_state" => json!({
            "type": "object",
            "properties": {
                "max_files": { "type": "integer", "minimum": 1, "maximum": 10000, "default": 200 }
            },
            "additionalProperties": false
        }),
        "start_task" => json!({
            "type": "object",
            "properties": {
                "objective": { "type": "string", "minLength": 1 }
            },
            "required": ["objective"],
            "additionalProperties": false
        }),
        "update_task" => json!({
            "type": "object",
            "properties": {
                "task_id": { "type": "string", "minLength": 1 },
                "completed_steps": { "type": "array", "items": { "type": "string" } },
                "pending_steps": { "type": "array", "items": { "type": "string" } }
            },
            "required": ["task_id"],
            "additionalProperties": false
        }),
        "pause_task" | "resume_task" => json!({
            "type": "object",
            "properties": { "task_id": { "type": "string", "minLength": 1 } },
            "required": ["task_id"],
            "additionalProperties": false
        }),
        "finish_task" => json!({
            "type": "object",
            "properties": {
                "task_id": { "type": "string", "minLength": 1 },
                "summary": { "type": "string" },
                "allow_unverified": { "type": "boolean", "default": false }
            },
            "required": ["task_id"],
            "additionalProperties": false
        }),
        "task_context" => json!({
            "type": "object",
            "properties": {
                "task_id": { "type": "string" },
                "max_bytes": { "type": "integer", "minimum": 8192, "maximum": 131072, "default": 32768 }
            },
            "additionalProperties": false
        }),
        "list_task_events" => json!({
            "type": "object",
            "properties": {
                "task_id": { "type": "string", "minLength": 1 },
                "cursor": { "type": "integer", "minimum": 0, "default": 0 },
                "limit": { "type": "integer", "minimum": 1, "maximum": 200, "default": 50 }
            },
            "required": ["task_id"],
            "additionalProperties": false
        }),
        "change_summary" => json!({
            "type": "object",
            "properties": { "task_id": { "type": "string" }, "change_id": { "type": "string" } },
            "additionalProperties": false
        }),
        "read_file" => json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "minLength": 1 },
                "start_line": { "type": "integer", "minimum": 1, "default": 1 },
                "end_line": { "type": "integer", "minimum": 1 },
                "max_bytes": { "type": "integer", "minimum": 1, "maximum": 1048576, "default": 131072 }
            },
            "required": ["path"],
            "additionalProperties": false
        }),
        "list_dir" => json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "default": "." },
                "recursive": { "type": "boolean", "default": false },
                "max_depth": { "type": "integer", "minimum": 1, "maximum": 20, "default": 1 },
                "max_entries": { "type": "integer", "minimum": 1, "maximum": 10000, "default": 1000 },
                "include_hidden": { "type": "boolean", "default": false },
                "include_ignored": { "type": "boolean", "default": false }
            },
            "additionalProperties": false
        }),
        "list_files" => json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "default": "." },
                "patterns": { "type": "array", "items": { "type": "string" } },
                "glob": { "type": "string", "description": "Alias for a single patterns entry" },
                "exclude_patterns": { "type": "array", "items": { "type": "string" } },
                "include_hidden": { "type": "boolean", "default": false },
                "include_ignored": { "type": "boolean", "default": false },
                "max_results": { "type": "integer", "minimum": 1, "maximum": 50000, "default": 5000 }
            },
            "additionalProperties": false
        }),
        "search_text" | "grep_text" | "grep" => json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "minLength": 1 },
                "path": { "type": "string", "default": "." },
                "glob": { "type": "string", "description": "Alias appended to include_globs" },
                "include_globs": { "type": "array", "items": { "type": "string" } },
                "exclude_globs": { "type": "array", "items": { "type": "string" } },
                "regex": { "type": "boolean", "default": false },
                "case_sensitive": { "type": "boolean", "default": false },
                "context_lines": { "type": "integer", "minimum": 0, "maximum": 20, "default": 0 },
                "max_preview_bytes": { "type": "integer", "minimum": 64, "maximum": 4096, "default": 512 },
                "max_results": { "type": "integer", "minimum": 1, "maximum": 10000, "default": 1000 }
            },
            "required": ["query"],
            "additionalProperties": false
        }),
        "apply_patch" => json!({
            "type": "object",
            "properties": {
                "patch": { "type": "string", "minLength": 1 },
                "dry_run": { "type": "boolean", "default": false },
                "reason": { "type": "string", "default": "" }
            },
            "required": ["patch"],
            "additionalProperties": false
        }),
        "patch_check" => json!({
            "type": "object",
            "properties": {
                "patch": { "type": "string", "minLength": 1 }
            },
            "required": ["patch"],
            "additionalProperties": false
        }),
        "exec_command" => json!({
            "type": "object",
            "properties": {
                "cmd": { "type": "string", "minLength": 1 },
                "workdir": { "type": "string", "default": "." },
                "timeout_ms": { "type": "integer", "minimum": 1, "maximum": 600000, "default": 30000 },
                "max_output_bytes": { "type": "integer", "minimum": 1024, "maximum": 1048576, "default": 65536 },
                "yield_time_ms": { "type": "integer", "minimum": 0, "maximum": 30000, "default": 1000 },
                "tty": { "type": "boolean", "default": false },
                "stdin": { "type": "string", "default": "" },
                "filesystem_scope": { "type": "string", "enum": ["workspace"], "default": "workspace" },
                "reason": { "type": "string", "default": "" }
            },
            "required": ["cmd"],
            "additionalProperties": false
        }),
        "write_stdin" => json!({
            "type": "object",
            "properties": {
                "session_id": { "type": "string", "minLength": 1 },
                "chars": { "type": "string", "default": "" },
                "yield_time_ms": { "type": "integer", "minimum": 0, "maximum": 30000, "default": 1000 },
                "max_output_bytes": { "type": "integer", "minimum": 1, "maximum": 1048576, "default": 65536 }
            },
            "required": ["session_id"],
            "additionalProperties": false
        }),
        "kill_session" => json!({
            "type": "object",
            "properties": {
                "session_id": { "type": "string", "minLength": 1 },
                "signal": { "type": "string", "enum": ["TERM", "KILL", "INT"], "default": "TERM" },
                "wait_ms": { "type": "integer", "minimum": 0, "maximum": 30000, "default": 5000 },
                "max_output_bytes": { "type": "integer", "minimum": 1, "maximum": 1048576, "default": 65536 }
            },
            "required": ["session_id"],
            "additionalProperties": false
        }),
        "read_output" => json!({
            "type": "object",
            "properties": {
                "output_ref": { "type": "string", "minLength": 1 },
                "stream": { "type": "string", "enum": ["stdout", "stderr"] },
                "offset": { "type": "integer", "minimum": 0, "default": 0 },
                "limit": { "type": "integer", "minimum": 1, "maximum": 1048576, "default": 4096 }
            },
            "required": ["output_ref"],
            "additionalProperties": false
        }),
        "git_status" => json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "default": "." },
                "include_untracked": { "type": "boolean", "default": true },
                "max_entries": { "type": "integer", "minimum": 1, "maximum": 10000, "default": 1000 }
            },
            "additionalProperties": false
        }),
        "git_diff" => json!({
            "type": "object",
            "properties": {
                "paths": { "type": "array", "items": { "type": "string" }, "default": [] },
                "staged": { "type": "boolean", "default": false },
                "unstaged": { "type": "boolean", "default": true },
                "context_lines": { "type": "integer", "minimum": 0, "maximum": 20, "default": 3 },
                "max_bytes": { "type": "integer", "minimum": 1024, "maximum": 1048576, "default": 262144 }
            },
            "additionalProperties": false
        }),
        "git_log" => json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "default": "." },
                "ref": { "type": "string", "default": "HEAD" },
                "max_count": { "type": "integer", "minimum": 1, "maximum": 100, "default": 20 },
                "skip": { "type": "integer", "minimum": 0, "maximum": 10000, "default": 0 }
            },
            "additionalProperties": false
        }),
        "git_show" => json!({
            "type": "object",
            "properties": {
                "rev": { "type": "string", "default": "HEAD" },
                "path": { "type": "string" },
                "paths": { "type": "array", "items": { "type": "string" } },
                "include_diff": { "type": "boolean", "default": true },
                "context_lines": { "type": "integer", "minimum": 0, "maximum": 20, "default": 3 },
                "max_bytes": { "type": "integer", "minimum": 1, "maximum": 1048576, "default": 262144 }
            },
            "additionalProperties": false
        }),
        "git_blame" => json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "minLength": 1 },
                "rev": { "type": "string" },
                "start_line": { "type": "integer", "minimum": 1, "default": 1 },
                "end_line": { "type": "integer", "minimum": 1 },
                "max_lines": { "type": "integer", "minimum": 1, "maximum": 1000, "default": 200 }
            },
            "required": ["path"],
            "additionalProperties": false
        }),
        "set_default_cwd" => json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "default": "." }
            },
            "additionalProperties": false
        }),
        "view_image" => json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "minLength": 1 },
                "max_bytes": { "type": "integer", "minimum": 1024, "maximum": 10485760, "default": 5242880 },
                "max_width": { "type": "integer", "minimum": 1, "maximum": 10000, "default": 2000 },
                "max_height": { "type": "integer", "minimum": 1, "maximum": 10000, "default": 2000 },
                "auto_resize": { "type": "boolean", "default": true },
                "output": { "type": "string", "enum": ["mcp_image", "data_url"], "default": "mcp_image" }
            },
            "required": ["path"],
            "additionalProperties": false
        }),
        _ => json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        }),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use serde_json::json;

    use super::{input_schema, list_tools_for_profile, normalize_tool_profile, output_schema};

    #[test]
    fn core_catalog_exposes_25_chatgpt_compatible_tools() {
        let tools = list_tools_for_profile("core");
        let names: Vec<_> = tools
            .iter()
            .map(|tool| tool["name"].as_str().expect("tool name"))
            .collect();
        let unique: HashSet<_> = names.iter().copied().collect();

        assert_eq!(tools.len(), 25);
        assert_eq!(unique.len(), tools.len());
        assert!(names.contains(&"list_skills"));
        assert!(names.contains(&"load_skill"));
        assert!(names.contains(&"read_skill_resource"));
        assert!(names.contains(&"history_session_bootstrap"));
        assert!(names.contains(&"history_session_checkpoint"));
        assert!(names.contains(&"history_session_validate"));
        assert!(names.contains(&"search_text"));
        assert!(!names.contains(&"grep_text"));
        assert!(!names.contains(&"grep"));
        assert!(!names.contains(&"request_permissions"));

        for name in names {
            let schema = input_schema(name);
            assert_eq!(schema["type"], "object", "{name} schema type");
            assert!(schema["properties"].is_object(), "{name} properties");
            assert!(schema.get("oneOf").is_none(), "{name} oneOf");
            assert!(schema.get("anyOf").is_none(), "{name} anyOf");
            assert!(schema.get("$ref").is_none(), "{name} ref");
            let output = output_schema(name);
            assert_eq!(output["type"], "object", "{name} output type");
            assert!(output["properties"].is_object(), "{name} output properties");
            assert_eq!(output["required"], json!(["ok"]));
        }
    }

    #[test]
    fn profile_aliases_map_to_canonical_profiles() {
        assert_eq!(normalize_tool_profile("core"), "core");
        assert_eq!(normalize_tool_profile("advanced"), "advanced");
        assert_eq!(normalize_tool_profile("full"), "advanced");
        assert_eq!(normalize_tool_profile("read-only"), "read-only");
        assert_eq!(normalize_tool_profile("compat-readonly-all"), "read-only");
    }

    #[test]
    fn every_advanced_tool_has_a_valid_output_schema() {
        for tool in list_tools_for_profile("advanced") {
            let name = tool["name"].as_str().expect("tool name");
            let schema = tool.get("outputSchema").expect("output schema");
            assert_eq!(schema["type"], "object", "{name} output root");
            jsonschema::meta::validate(schema)
                .unwrap_or_else(|error| panic!("{name} output schema: {error}"));
        }
    }

    #[test]
    fn read_only_catalog_excludes_state_mutation_tools() {
        let tools = list_tools_for_profile("read-only");
        let names: Vec<_> = tools
            .iter()
            .map(|tool| tool["name"].as_str().expect("tool name"))
            .collect();

        assert!(!names.contains(&"set_default_cwd"));
        assert!(!names.contains(&"apply_patch"));
        assert!(!names.contains(&"exec_command"));
        assert!(names.contains(&"get_default_cwd"));
        assert!(names.contains(&"read_file"));
    }

    #[test]
    fn legacy_compat_profile_is_a_truthful_read_only_alias() {
        let read_only = list_tools_for_profile("read-only");
        let compat = list_tools_for_profile("compat-readonly-all");
        assert_eq!(compat, read_only);
        assert!(compat.iter().all(|tool| {
            tool["annotations"]["readOnlyHint"] == true
                && tool["annotations"]["destructiveHint"] == false
        }));
        assert!(!compat.iter().any(|tool| tool["name"] == "apply_patch"));
        assert!(!compat.iter().any(|tool| tool["name"] == "exec_command"));
    }
}
