use serde_json::{json, Value};

pub const CATALOG_VERSION: u32 = 26;

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
        "accept_current_baseline",
        "Accept current baseline",
        "Accept the currently observed workspace HEAD and fingerprint using a task-bound observation token.",
        false,
        false,
        false,
    ),
    (
        "accept_latest_baseline",
        "Accept latest baseline",
        "Capture and accept a stable latest workspace baseline in one call, retrying bounded concurrent changes without a caller-managed observation token.",
        false,
        false,
        false,
    ),
    (
        "update_verification_disposition",
        "Update verification disposition",
        "Append an audited disposition to immutable verification evidence, such as expected_failure, diagnostic_only, superseded, or waived.",
        false,
        false,
        false,
    ),
    (
        "begin_work_session",
        "Begin work session",
        "Create or resume a History Session and bind the calling MCP session to a shared-checkout task by default or an explicitly requested isolated Git worktree task.",
        false,
        false,
        false,
    ),
    (
        "close_work_session",
        "Close work session",
        "Validate and close the bound Harness Task, then persist the matching History Session checkpoint as a recoverable workflow.",
        false,
        false,
        false,
    ),
    (
        "wait_command",
        "Wait for command",
        "Wait for a retained command session and return explicit terminal state plus incremental stdout/stderr.",
        true,
        false,
        false,
    ),
    (
        "list_command_sessions",
        "List command sessions",
        "Return all retained command sessions with command identity, execution state, output references, and recent activity timestamps for reliable resume.",
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
        "browser_build_info",
        "Browser build info",
        "Read the selected page build hash, Git commit, app version, loaded asset hashes, Service Worker registrations, and cache names.",
        true,
        false,
        true,
    ),
    (
        "browser_wait_for_build",
        "Wait for browser build",
        "Clear selected-page Service Workers and Cache Storage, reload without cache, and wait until the expected frontend build hash or Git commit is loaded.",
        false,
        true,
        true,
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
        "list_skill_resources",
        "List Skill resources",
        "List the exact bounded resource and script manifest that read_skill_resource is allowed to read for one discovered Skill.",
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
        "Return the current project plus the selected task and all active parallel task ids.",
        true,
        false,
        false,
    ),
    (
        "start_task",
        "Start task",
        "Start a durable coding task, capture the shared workspace baseline, and bind it to the calling MCP session without pausing other tasks unless pause_current=true.",
        false,
        false,
        false,
    ),
    (
        "refresh_baseline",
        "Refresh Harness baseline",
        "Accept an explicitly observed workspace HEAD and fingerprint as the active task's new expected state without changing the immutable task-start baseline.",
        false,
        false,
        false,
    ),
    (
        "stage_commit",
        "Validate and commit a stage",
        "Run or start a durable staged-commit workflow. Deferred mode returns quickly and is advanced with wait_stage_commit without replaying completed checks.",
        false,
        true,
        false,
    ),
    (
        "stage_commit_status",
        "Stage commit status",
        "Read a durable stage_commit workflow without advancing or replaying it.",
        true,
        false,
        false,
    ),
    (
        "wait_stage_commit",
        "Wait for stage commit",
        "Wait for and advance a deferred stage_commit workflow for up to sixty seconds.",
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
        "Pause one explicit coding task without changing other active tasks.",
        false,
        false,
        false,
    ),
    (
        "resume_task",
        "Resume task",
        "Resume a paused or failed coding task and bind it to the calling MCP session without pausing peers.",
        false,
        false,
        false,
    ),
    (
        "switch_task",
        "Switch task",
        "Bind the calling MCP session to an existing task and transfer the writer lease only within that task's shared checkout or linked-worktree write domain.",
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
        "Return bounded durable context for an explicit task or the task bound to the calling MCP session.",
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
        "export_work_session",
        "Export work session",
        "Write a versioned, portable JSON handoff containing task, History binding, commits, verifications, remaining issues, and Git state without copying private Harness storage.",
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
        "Apply a patch envelope transactionally with cooperative cancellation, bounded processing time, and atomic rollback inside the workspace.",
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
        "remove_path",
        "Remove path",
        "Remove one exact workspace-local file, empty directory, symlink, or junction without following the final link or invoking a child process.",
        false,
        true,
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
        "command_cost_explain",
        "Explain command cost",
        "Classify a command without executing it or reserving cost budget, including the executable, declarations, and matched evidence.",
        true,
        false,
        false,
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
        "git_worktree_list",
        "Git worktree list",
        "List linked worktrees with branch, HEAD, lock, prune, main-worktree, and Anchor-managed status.",
        true,
        false,
        false,
    ),
    (
        "git_worktree_create",
        "Git worktree create",
        "Create an Anchor-managed linked worktree under .anchor/worktrees from a base ref and optional branch.",
        false,
        false,
        false,
    ),
    (
        "git_worktree_remove",
        "Git worktree remove",
        "Remove an Anchor-managed linked worktree; dirty worktrees require operator-approved force removal.",
        false,
        true,
        false,
    ),
    (
        "git_worktree_prune",
        "Git worktree prune",
        "Prune stale linked-worktree administrative records and report the remaining worktree count.",
        false,
        false,
        false,
    ),
    (
        "git_stage",
        "Git stage",
        "Stage explicit workspace-relative paths without invoking a shell.",
        false,
        false,
        false,
    ),
    (
        "git_commit",
        "Git commit",
        "Commit the currently staged changes with an explicit message and return the exact committed files.",
        false,
        false,
        false,
    ),
    (
        "git_restore",
        "Git restore",
        "Restore explicit workspace-relative paths in the worktree and/or staging area without invoking a shell.",
        false,
        false,
        false,
    ),
    (
        "git_reset",
        "Git reset",
        "Move HEAD to an explicit commit using soft, mixed, or operator-approved hard reset.",
        false,
        true,
        false,
    ),
    (
        "git_revert",
        "Git revert",
        "Apply the inverse of one commit to the index and worktree without committing, or abort an in-progress revert.",
        false,
        true,
        false,
    ),
    (
        "git_clean",
        "Git clean",
        "Preview or remove untracked workspace paths with explicit scope and dangerous-mode gates for repository-wide or ignored-file deletion.",
        false,
        true,
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
    "browser_build_info",
    "browser_wait_for_build",
    "begin_work_session",
    "close_work_session",
    "update_verification_disposition",
    "accept_latest_baseline",
    "list_skills",
    "load_skill",
    "list_skill_resources",
    "read_skill_resource",
    "switch_task",
    "history_session_bootstrap",
    "history_session_checkpoint",
    "history_session_validate",
    "check_exec_environment",
    "command_cost_explain",
    "get_default_cwd",
    "set_default_cwd",
    "read_file",
    "list_dir",
    "list_files",
    "search_text",
    "apply_patch",
    "remove_path",
    "exec_command",
    "write_stdin",
    "wait_command",
    "list_command_sessions",
    "kill_session",
    "read_output",
    "git_status",
    "git_worktree_list",
    "git_stage",
    "git_commit",
    "git_restore",
    "git_reset",
    "git_revert",
    "git_clean",
    "git_diff",
    "git_log",
    "git_show",
    "git_blame",
    "view_image",
];

pub const CORE_READ_ONLY_TOOLS: &[&str] = &[
    "server_info",
    "browser_build_info",
    "list_skills",
    "load_skill",
    "list_skill_resources",
    "read_skill_resource",
    "check_exec_environment",
    "command_cost_explain",
    "get_default_cwd",
    "read_file",
    "list_dir",
    "list_files",
    "search_text",
    "read_output",
    "wait_command",
    "list_command_sessions",
    "git_status",
    "git_worktree_list",
    "git_diff",
    "git_log",
    "git_show",
    "git_blame",
    "view_image",
];

pub const ALLOWED_TOOLS: &[&str] = &[
    "harness_status",
    "operation_log",
    "begin_work_session",
    "close_work_session",
    "update_verification_disposition",
    "accept_current_baseline",
    "accept_latest_baseline",
    "server_info",
    "browser_build_info",
    "browser_wait_for_build",
    "list_skills",
    "load_skill",
    "list_skill_resources",
    "read_skill_resource",
    "history_session_bootstrap",
    "history_session_checkpoint",
    "history_session_validate",
    "check_exec_environment",
    "exec_health_check",
    "command_cost_explain",
    "get_default_cwd",
    "set_default_cwd",
    "read_file",
    "list_dir",
    "list_files",
    "search_text",
    "apply_patch",
    "patch_check",
    "remove_path",
    "exec_command",
    "write_stdin",
    "wait_command",
    "list_command_sessions",
    "kill_session",
    "read_output",
    "git_status",
    "git_worktree_list",
    "git_worktree_create",
    "git_worktree_remove",
    "git_worktree_prune",
    "git_stage",
    "git_commit",
    "git_restore",
    "git_reset",
    "git_revert",
    "git_clean",
    "git_diff",
    "git_log",
    "git_show",
    "git_blame",
    "project_state",
    "start_task",
    "refresh_baseline",
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
    "view_image",
];

pub const MUTATING_TOOLS: &[&str] = &[
    "begin_work_session",
    "browser_wait_for_build",
    "close_work_session",
    "update_verification_disposition",
    "accept_current_baseline",
    "accept_latest_baseline",
    "history_session_bootstrap",
    "history_session_checkpoint",
    "history_session_validate",
    "apply_patch",
    "remove_path",
    "exec_command",
    "git_worktree_create",
    "git_worktree_remove",
    "git_worktree_prune",
    "git_stage",
    "git_commit",
    "git_restore",
    "git_reset",
    "git_revert",
    "git_clean",
    "write_stdin",
    "kill_session",
    "set_default_cwd",
    "start_task",
    "refresh_baseline",
    "stage_commit",
    "wait_stage_commit",
    "update_task",
    "pause_task",
    "resume_task",
    "switch_task",
    "finish_task",
    "export_work_session",
];

pub const READ_ONLY_TOOLS: &[&str] = &[
    "harness_status",
    "operation_log",
    "stage_commit_status",
    "server_info",
    "browser_build_info",
    "list_skills",
    "load_skill",
    "read_skill_resource",
    "check_exec_environment",
    "exec_health_check",
    "command_cost_explain",
    "get_default_cwd",
    "read_file",
    "list_dir",
    "list_files",
    "search_text",
    "read_output",
    "wait_command",
    "list_command_sessions",
    "git_status",
    "git_worktree_list",
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
            "target_preserved": { "type": "boolean", "const": true },
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
            "latest_handoff_source": { "type": "string", "enum": ["current_session", "latest_prior", "none"] },
            "latest_handoff_session_number": nullable_integer_property(),
            "latest_handoff_session_path": { "type": ["string", "null"] },
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
            "resume_state": { "type": "object" },
            "persistence": {
                "type": "object",
                "properties": {
                    "storage": { "type": "string", "const": "workspace_file" },
                    "path": { "type": "string", "minLength": 1 },
                    "git_tracked": { "type": "boolean" },
                    "git_ignored": { "type": "boolean" },
                    "git_dirty_after_write": { "type": "boolean" },
                    "reason": { "type": "string", "minLength": 1 }
                },
                "required": ["storage", "path", "git_tracked", "git_ignored", "git_dirty_after_write", "reason"],
                "additionalProperties": false
            },
            "warnings": warnings_property()
        }),
    ])
}

fn session_snapshot_output_schema() -> Value {
    success_output_schema(
        json!({
            "session_id": { "type": "string", "minLength": 1 },
            "command": { "type": "string" },
            "resolved_cwd": { "type": "string" },
            "interactive": { "type": "boolean" },
            "stdin_open": { "type": "boolean" },
            "status": { "type": "string", "minLength": 1 },
            "termination_reason": { "type": "string", "minLength": 1 },
            "recoverable": { "type": "boolean" },
            "suggestion": { "type": "string" },
            "exit_code": nullable_integer_property(),
            "transport_ok": { "type": "boolean" },
            "transport_status": { "type": "string", "enum": ["ok", "error"] },
            "execution_status": { "type": "string", "enum": ["running", "succeeded", "failed", "cancelled", "timed_out", "killed", "spawn_failed", "rejected", "interrupted"] },
            "success": { "type": ["boolean", "null"] },
            "retryable": { "type": "boolean" },
            "command_ok": { "type": ["boolean", "null"] },
            "stdout": { "type": "string" },
            "stderr": { "type": "string" },
            "stdout_truncated": { "type": "boolean" },
            "stderr_truncated": { "type": "boolean" },
            "stdout_complete": { "type": "boolean" },
            "stderr_complete": { "type": "boolean" },
            "elapsed_ms": { "type": "integer", "minimum": 0 },
            "affected_files": { "type": "array", "items": { "type": "object" } },
            "mutation_attributed": { "type": "boolean" },
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
            "command",
            "resolved_cwd",
            "interactive",
            "stdin_open",
            "status",
            "termination_reason",
            "recoverable",
            "suggestion",
            "exit_code",
            "transport_ok",
            "transport_status",
            "execution_status",
            "success",
            "retryable",
            "command_ok",
            "stdout",
            "stderr",
            "stdout_truncated",
            "stderr_truncated",
            "stdout_complete",
            "stderr_complete",
            "elapsed_ms",
            "output_refs",
        ],
    )
}

pub fn output_schema(name: &str) -> Value {
    match name {
        "server_info" => success_output_schema(
            merge_schema_properties(vec![
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
                    "tool_groups": { "type": "object", "additionalProperties": { "type": "array", "items": { "type": "string" } } },
                    "current_tools": { "type": "array", "items": { "type": "string" } },
                    "current_tool_count": { "type": "integer", "minimum": 0 },
                    "current_tool_groups": { "type": "object", "additionalProperties": { "type": "array", "items": { "type": "string" } } },
                    "catalog_digest": { "type": "string", "minLength": 64, "maxLength": 64 },
                    "running_catalog_digest": { "type": "string", "minLength": 64, "maxLength": 64 },
                    "current_catalog_digest": { "type": "string", "minLength": 64, "maxLength": 64 },
                    "catalog_published": { "type": "boolean" },
                    "catalog_changed": { "type": "boolean" },
                    "reconnect_required": { "type": "boolean" },
                    "catalog_version": { "type": "integer", "minimum": 1 },
                    "build_identity": {
                        "type": "object",
                        "properties": {
                            "git_sha": { "type": "string", "minLength": 1 },
                            "git_dirty": { "type": "boolean" },
                            "build_workspace": { "type": "string", "minLength": 1 },
                            "catalog_version": { "type": "integer", "minimum": 1 },
                            "package_version": { "type": "string", "minLength": 1 }
                        },
                        "required": ["git_sha", "git_dirty", "build_workspace", "catalog_version", "package_version"],
                        "additionalProperties": false
                    },
                    "catalog_bytes": { "type": "integer", "minimum": 0 },
                    "catalog_estimated_tokens": { "type": "integer", "minimum": 0 },
                    "local_tool_count": { "type": "integer", "minimum": 0 },
                    "proxy_tool_count": { "type": "integer", "minimum": 0 },
                    "current_catalog_bytes": { "type": "integer", "minimum": 0 },
                    "current_catalog_estimated_tokens": { "type": "integer", "minimum": 0 },
                    "current_local_tool_count": { "type": "integer", "minimum": 0 },
                    "current_proxy_tool_count": { "type": "integer", "minimum": 0 }
                }),
                json!({
                    "command_cost_policy": { "type": "object" },
                    "downstream_mcp": {
                        "type": "object",
                        "properties": {
                            "configured": { "type": "boolean" },
                            "server_count": { "type": "integer", "minimum": 0 },
                            "servers": {
                                "type": "array",
                                "items": {
                                    "type": "object",
                                    "additionalProperties": true
                                }
                            }
                        },
                        "required": ["configured", "server_count", "servers"],
                        "additionalProperties": false
                    },
                    "connection_layers": { "type": "object" }
                }),
            ]),
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
                "tool_groups",
                "current_tools",
                "current_tool_count",
                "current_tool_groups",
                "catalog_digest",
                "running_catalog_digest",
                "current_catalog_digest",
                "catalog_published",
                "catalog_changed",
                "reconnect_required",
                "catalog_version",
                "build_identity",
                "catalog_bytes",
                "catalog_estimated_tokens",
                "local_tool_count",
                "proxy_tool_count",
                "current_catalog_bytes",
                "current_catalog_estimated_tokens",
                "current_local_tool_count",
                "current_proxy_tool_count",
                "command_cost_policy",
                "downstream_mcp",
                "connection_layers",
            ],
        ),
        "export_work_session" => success_output_schema(
            json!({
                "format": { "type": "string", "const": "anchor.work-session-handoff" },
                "schema_version": { "type": "integer", "const": 1 },
                "path": { "type": "string", "minLength": 1 },
                "task_id": { "type": "string", "minLength": 1 },
                "content_bytes": { "type": "integer", "minimum": 1, "maximum": 8388608 },
                "content_hash": { "type": "string", "minLength": 64, "maxLength": 64 },
                "git_ignored_recommended": { "type": "boolean" },
                "resume_strategy": { "type": "string", "const": "begin_work_session" },
                "warnings": warnings_property()
            }),
            &[
                "format",
                "schema_version",
                "path",
                "task_id",
                "content_bytes",
                "content_hash",
                "git_ignored_recommended",
                "resume_strategy",
                "warnings",
            ],
        ),
        "browser_build_info" => success_output_schema(
            json!({
                "build_info": { "type": "object" },
                "current_build": { "type": ["string", "null"] },
                "source_tool": { "type": "string", "minLength": 1 },
                "warnings": warnings_property()
            }),
            &["build_info", "current_build", "source_tool", "warnings"],
        ),
        "browser_wait_for_build" => success_output_schema(
            json!({
                "expected_build": { "type": "string", "minLength": 1 },
                "matched": { "type": "boolean", "const": true },
                "build_info": { "type": "object" },
                "current_build": { "type": ["string", "null"] },
                "attempts": { "type": "integer", "minimum": 1 },
                "elapsed_ms": { "type": "integer", "minimum": 0 },
                "cleanup": { "type": "object" },
                "reload": { "type": "object" },
                "warnings": warnings_property()
            }),
            &[
                "expected_build",
                "matched",
                "build_info",
                "current_build",
                "attempts",
                "elapsed_ms",
                "cleanup",
                "reload",
                "warnings",
            ],
        ),
        "list_command_sessions" => success_output_schema(
            json!({
                "sessions": { "type": "array", "items": { "type": "object" } },
                "session_count": { "type": "integer", "minimum": 0 },
                "running_count": { "type": "integer", "minimum": 0 },
                "terminal_count": { "type": "integer", "minimum": 0 },
                "process_bound": { "type": "boolean", "const": true },
                "warnings": warnings_property()
            }),
            &[
                "sessions",
                "session_count",
                "running_count",
                "terminal_count",
                "process_bound",
                "warnings",
            ],
        ),
        "git_reset" => success_output_schema(
            json!({
                "before_head": { "type": "string" },
                "target_head": { "type": "string", "minLength": 40 },
                "after_head": { "type": "string", "minLength": 40 },
                "mode": { "type": "string", "enum": ["soft", "mixed", "hard"] },
                "mutation_attributed": { "type": "boolean", "const": true },
                "warnings": warnings_property()
            }),
            &[
                "before_head",
                "target_head",
                "after_head",
                "mode",
                "mutation_attributed",
                "warnings",
            ],
        ),
        "git_revert" => success_output_schema(
            json!({
                "aborted": { "type": "boolean" },
                "reverted_commit": { "type": ["string", "null"] },
                "no_commit": { "type": "boolean", "const": true },
                "staged_files": { "type": "array", "items": { "type": "string" } },
                "mutation_attributed": { "type": "boolean", "const": true },
                "warnings": warnings_property()
            }),
            &[
                "aborted",
                "reverted_commit",
                "no_commit",
                "staged_files",
                "mutation_attributed",
                "warnings",
            ],
        ),
        "git_clean" => success_output_schema(
            json!({
                "dry_run": { "type": "boolean" },
                "directories": { "type": "boolean" },
                "include_ignored": { "type": "boolean" },
                "paths": { "type": "array", "items": { "type": "string" } },
                "candidates": { "type": "array", "items": { "type": "string" } },
                "removed_paths": { "type": "array", "items": { "type": "string" } },
                "mutation_attributed": { "type": "boolean" },
                "warnings": warnings_property()
            }),
            &[
                "dry_run",
                "directories",
                "include_ignored",
                "paths",
                "candidates",
                "removed_paths",
                "mutation_attributed",
                "warnings",
            ],
        ),
        "remove_path" => success_output_schema(
            json!({
                "path": { "type": "string", "minLength": 1 },
                "kind": { "type": "string", "enum": ["file", "directory", "symlink_or_junction"] },
                "recursive": { "type": "boolean" },
                "link_like": { "type": "boolean" },
                "target_preserved": { "type": "boolean" },
                "affected_files": { "type": "array", "items": { "type": "object" } },
                "mutation_attributed": { "type": "boolean", "const": true },
                "warnings": warnings_property()
            }),
            &[
                "path",
                "kind",
                "recursive",
                "link_like",
                "target_preserved",
                "affected_files",
                "mutation_attributed",
                "warnings",
            ],
        ),
        "accept_current_baseline" => success_output_schema(
            json!({
                "accepted": { "type": "boolean", "const": true },
                "task": { "type": "object" },
                "harness": { "type": "object" }
            }),
            &["accepted", "task", "harness"],
        ),
        "accept_latest_baseline" => success_output_schema(
            json!({
                "accepted": { "type": "boolean", "const": true },
                "attempts": { "type": "integer", "minimum": 1, "maximum": 10 },
                "accepted_state": { "type": "object" },
                "task": { "type": "object" },
                "harness": { "type": "object" }
            }),
            &["accepted", "attempts", "accepted_state", "task", "harness"],
        ),
        "switch_task" => success_output_schema(
            json!({
                "task": { "type": "object" },
                "harness": { "type": "object" }
            }),
            &["task", "harness"],
        ),
        "list_skill_resources" => success_output_schema(
            json!({
                "skill": { "type": "string", "minLength": 1 },
                "resources": { "type": "array", "items": { "type": "object" } },
                "totalResources": { "type": "integer", "minimum": 0 },
                "nextCursor": nullable_integer_property(),
                "resourceTruncated": { "type": "boolean" },
                "snapshotMode": { "type": "string" },
                "catalogDigest": { "type": "string" },
                "warnings": warnings_property()
            }),
            &[
                "skill",
                "resources",
                "totalResources",
                "nextCursor",
                "resourceTruncated",
                "snapshotMode",
                "catalogDigest",
                "warnings",
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
        "search_text" => success_output_schema(
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
                        "post_validation": { "type": "array", "items": { "type": "object" } },
                        "hunk_matches": { "type": "array", "items": { "type": "object" } },
                        "transaction": { "type": "object" },
                        "recovery": { "type": "string", "minLength": 1 },
                        "duration_ms": { "type": "integer", "minimum": 0 },
                        "timeout_ms": { "type": "integer", "minimum": 1000, "maximum": 60000 },
                        "terminal_status": { "type": "string", "enum": ["completed", "dry_run_completed"] },
                        "warnings": warnings_property()
                    }),
                    &[
                        "dry_run",
                        "clean",
                        "summary",
                        "affected_files",
                        "post_validation",
                        "duration_ms",
                        "timeout_ms",
                        "terminal_status",
                        "warnings",
                    ],
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
                "post_validation": { "type": "array", "items": { "type": "object" } },
                "duration_ms": { "type": "integer", "minimum": 0 },
                "timeout_ms": { "type": "integer", "minimum": 1000, "maximum": 60000 },
                "terminal_status": { "type": "string", "const": "dry_run_completed" },
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
                "post_validation",
                "duration_ms",
                "timeout_ms",
                "terminal_status",
                "warnings",
            ],
        ),
        "command_cost_explain" => success_output_schema(
            json!({
                "command": { "type": "string", "minLength": 1 },
                "classification": { "type": "object" },
                "would_require_operator_approval": { "type": "boolean" },
                "executed": { "type": "boolean", "const": false },
                "run_budget_reserved": { "type": "boolean", "const": false }
            }),
            &[
                "command",
                "classification",
                "would_require_operator_approval",
                "executed",
                "run_budget_reserved",
            ],
        ),
        "exec_command" => success_output_schema(
            json!({
                "session_id": { "type": ["string", "null"] },
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
                "stdout_complete": { "type": "boolean" },
                "stderr_complete": { "type": "boolean" },
                "duration_ms": { "type": "integer", "minimum": 0 },
                "elapsed_ms": { "type": "integer", "minimum": 0 },
                "execution_mode": { "type": "string", "minLength": 1 },
                "filesystem_scope": { "type": "string", "const": "workspace" },
                "sandbox_enforced": { "type": "boolean" },
                "execution_boundary": { "type": "string", "minLength": 1 },
                "child_process": { "type": "boolean" },
                "execution_started": { "type": "boolean" },
                "transport_ok": { "type": "boolean" },
                "transport_status": { "type": "string", "enum": ["ok", "error"] },
                "execution_status": { "type": "string", "enum": ["running", "succeeded", "failed", "cancelled", "timed_out", "killed", "spawn_failed", "rejected", "interrupted"] },
                "success": { "type": ["boolean", "null"] },
                "retryable": { "type": "boolean" },
                "command_ok": { "type": ["boolean", "null"] },
                "verification_pending": { "type": "boolean" },
                "verification_id": { "type": "string", "minLength": 1 },
                "verification_level": { "type": "string", "enum": ["diagnostic", "informational", "required", "blocking"] },
                "supersedes": { "type": "array", "items": { "type": "string" } },
                "affected_task_status": { "type": ["string", "null"] },
                "verification": { "type": "object", "additionalProperties": true },
                "cost_policy": { "type": "object", "additionalProperties": true },
                "affected_files": { "type": "array", "items": { "type": "object" } },
                "mutation_attributed": { "type": "boolean" },
                "verification_skipped": { "type": "boolean" },
                "verification_skip_reason": { "type": "string" },
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
                "stdout_complete",
                "stderr_complete",
                "duration_ms",
                "elapsed_ms",
                "execution_mode",
                "child_process",
                "execution_started",
                "transport_ok",
                "transport_status",
                "execution_status",
                "success",
                "retryable",
                "command_ok",
                "warnings",
            ],
        ),
        "write_stdin" => session_snapshot_output_schema(),
        "wait_command" => success_output_schema(
            json!({
                "session_id": { "type": "string", "minLength": 1 },
                "state": { "type": "string", "enum": ["running", "completed", "failed", "cancelled"] },
                "status": { "type": "string", "minLength": 1 },
                "termination_reason": { "type": "string", "minLength": 1 },
                "exit_code": nullable_integer_property(),
                "command_ok": { "type": ["boolean", "null"] },
                "transport_status": { "type": "string", "enum": ["ok", "error"] },
                "execution_status": { "type": "string", "enum": ["running", "succeeded", "failed", "cancelled", "timed_out", "killed", "spawn_failed", "rejected", "interrupted"] },
                "success": { "type": ["boolean", "null"] },
                "retryable": { "type": "boolean" },
                "started_at": { "type": "string", "minLength": 1 },
                "elapsed_ms": { "type": "integer", "minimum": 0 },
                "last_output_at": { "type": "string", "minLength": 1 },
                "stdin_open": { "type": "boolean" },
                "stdout": { "type": "object" },
                "stderr": { "type": "object" },
                "stdout_complete": { "type": "boolean" },
                "stderr_complete": { "type": "boolean" },
                "output_refs": { "type": "object" },
                "stop_pattern_matched": { "type": ["string", "null"] },
                "wait_timeout_ms": { "type": "integer", "minimum": 0 },
                "affected_files": { "type": "array", "items": { "type": "object" } },
                "mutation_attributed": { "type": "boolean" },
                "warnings": warnings_property()
            }),
            &[
                "session_id",
                "state",
                "status",
                "termination_reason",
                "exit_code",
                "command_ok",
                "transport_status",
                "execution_status",
                "success",
                "retryable",
                "started_at",
                "elapsed_ms",
                "last_output_at",
                "stdin_open",
                "stdout",
                "stderr",
                "stdout_complete",
                "stderr_complete",
                "output_refs",
                "wait_timeout_ms",
                "warnings",
            ],
        ),
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
        "git_worktree_list" => success_output_schema(
            json!({
                "worktrees": { "type": "array", "items": { "type": "object" } },
                "count": { "type": "integer", "minimum": 1 },
                "managed_root": { "type": "string", "const": ".anchor/worktrees" },
                "warnings": warnings_property()
            }),
            &["worktrees", "count", "managed_root", "warnings"],
        ),
        "git_worktree_create" => success_output_schema(
            json!({
                "path": { "type": "string", "minLength": 1 },
                "absolute_path": { "type": "string", "minLength": 1 },
                "branch": { "type": "string", "minLength": 1 },
                "base_ref": { "type": "string", "minLength": 1 },
                "managed": { "type": "boolean", "const": true },
                "remove_on_close": { "type": "boolean" },
                "created_at": { "type": "string", "minLength": 1 },
                "mutation_attributed": { "type": "boolean", "const": false },
                "warnings": warnings_property()
            }),
            &[
                "path",
                "absolute_path",
                "branch",
                "base_ref",
                "managed",
                "remove_on_close",
                "created_at",
                "mutation_attributed",
                "warnings",
            ],
        ),
        "git_worktree_remove" => success_output_schema(
            json!({
                "path": { "type": "string", "minLength": 1 },
                "removed": { "type": "boolean", "const": true },
                "force": { "type": "boolean" },
                "mutation_attributed": { "type": "boolean", "const": false },
                "warnings": warnings_property()
            }),
            &[
                "path",
                "removed",
                "force",
                "mutation_attributed",
                "warnings",
            ],
        ),
        "git_worktree_prune" => success_output_schema(
            json!({
                "pruned_count": { "type": "integer", "minimum": 0 },
                "remaining_count": { "type": "integer", "minimum": 1 },
                "details": { "type": "array", "items": { "type": "string" } },
                "mutation_attributed": { "type": "boolean", "const": false },
                "warnings": warnings_property()
            }),
            &[
                "pruned_count",
                "remaining_count",
                "details",
                "mutation_attributed",
                "warnings",
            ],
        ),
        "git_stage" => success_output_schema(
            json!({
                "staged_paths": { "type": "array", "items": { "type": "string" } },
                "staged_files": { "type": "array", "items": { "type": "string" } },
                "mutation_attributed": { "type": "boolean", "const": true },
                "warnings": { "type": "array", "items": { "type": "string" } }
            }),
            &[
                "staged_paths",
                "staged_files",
                "mutation_attributed",
                "warnings",
            ],
        ),
        "git_commit" => success_output_schema(
            json!({
                "commit_sha": { "type": "string", "minLength": 1 },
                "message": { "type": "string", "minLength": 1 },
                "committed_files": { "type": "array", "items": { "type": "string" } },
                "previously_staged_files": { "type": "array", "items": { "type": "string" } },
                "mutation_attributed": { "type": "boolean", "const": true },
                "warnings": { "type": "array", "items": { "type": "string" } }
            }),
            &[
                "commit_sha",
                "message",
                "committed_files",
                "previously_staged_files",
                "mutation_attributed",
                "warnings",
            ],
        ),
        "git_restore" => success_output_schema(
            json!({
                "restored_paths": { "type": "array", "items": { "type": "string" } },
                "staged": { "type": "boolean" },
                "worktree": { "type": "boolean" },
                "mutation_attributed": { "type": "boolean", "const": true },
                "warnings": { "type": "array", "items": { "type": "string" } }
            }),
            &[
                "restored_paths",
                "staged",
                "worktree",
                "mutation_attributed",
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
                "raw_clean": { "type": "boolean" },
                "entries": { "type": "array", "items": { "type": "object" } },
                "metadata_only_entries": { "type": "array", "items": { "type": "object" } },
                "metadata_only_count": { "type": "integer", "minimum": 0 },
                "content_changed_count": { "type": "integer", "minimum": 0 },
                "index_refresh_performed": { "type": "boolean" },
                "index_refresh_failed_count": { "type": "integer", "minimum": 0 },
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
                "target_preserved",
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
                "latest_handoff_source",
                "latest_handoff_session_number",
                "latest_handoff_session_path",
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
                "resume_state",
                "persistence",
                "warnings",
            ],
        ),
        "history_session_checkpoint" => success_output_schema(
            json!({
                "session_number": { "type": "integer", "minimum": 1 },
                "path": { "type": "string", "minLength": 1 },
                "session_key": { "type": "string", "minLength": 1 },
                "expected_path": { "type": "string", "minLength": 1 },
                "target_preserved": { "type": "boolean", "const": true },
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
                "storage": { "type": "string", "const": "workspace_file" },
                "git_tracked": { "type": "boolean" },
                "git_ignored": { "type": "boolean" },
                "git_dirty_after_write": { "type": "boolean" },
                "persistence_reason": { "type": "string", "minLength": 1 },
                "warnings": warnings_property()
            }),
            &[
                "session_number",
                "path",
                "session_key",
                "expected_path",
                "target_preserved",
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
                "storage",
                "git_tracked",
                "git_ignored",
                "git_dirty_after_write",
                "persistence_reason",
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
        "harness_status" => success_output_schema(
            json!({
                "schema_version": { "type": "integer", "minimum": 1 },
                "workspace_id": { "type": "string", "minLength": 1 },
                "default_task_id": { "type": ["string", "null"] },
                "active_task_ids": { "type": "array", "items": { "type": "string", "minLength": 1 } },
                "active_task_count": { "type": "integer", "minimum": 0 },
                "task_id": { "type": ["string", "null"] },
                "task_state": { "type": ["string", "null"] },
                "session_status": { "type": "string", "enum": ["active", "paused", "completed"] },
                "next_stage_started": { "type": "boolean", "const": false },
                "writable": { "type": "boolean" },
                "baseline_matches": { "type": ["boolean", "null"] },
                "branch": { "type": ["string", "null"] },
                "head": { "type": ["string", "null"] },
                "worktree_fingerprint": { "type": "string", "minLength": 64, "maxLength": 64 },
                "expected_branch": { "type": ["string", "null"] },
                "expected_head": { "type": ["string", "null"] },
                "expected_fingerprint": { "type": ["string", "null"] },
                "observation_token": { "type": ["string", "null"] },
                "capabilities": { "type": "object" },
                "journal_health": { "type": "object" },
                "next_actions": { "type": "array", "items": { "type": "string" } },
                "reason": { "type": "string" },
                "recoverable": { "type": "boolean" }
            }),
            &[
                "schema_version",
                "workspace_id",
                "default_task_id",
                "active_task_ids",
                "active_task_count",
                "session_status",
                "next_stage_started",
                "writable",
                "worktree_fingerprint",
                "capabilities",
                "journal_health",
                "next_actions",
                "reason",
                "recoverable",
            ],
        ),
        "operation_log" => success_output_schema(
            json!({
                "operations": { "type": "array", "items": { "type": "object" } },
                "summary": { "type": "object" },
                "diagnostics": { "type": "array", "items": { "type": "object" } },
                "total_matches": { "type": "integer", "minimum": 0 },
                "next_cursor": nullable_integer_property(),
                "filters": { "type": "object" }
            }),
            &[
                "operations",
                "summary",
                "diagnostics",
                "total_matches",
                "next_cursor",
                "filters",
            ],
        ),
        "begin_work_session" => success_output_schema(
            json!({
                "work_session": { "type": "object" },
                "history": { "type": "object" },
                "task": { "type": "object" },
                "harness": { "type": "object" },
                "reconnect_required": { "type": "boolean", "const": false }
            }),
            &[
                "work_session",
                "history",
                "task",
                "harness",
                "reconnect_required",
            ],
        ),
        "close_work_session" => json!({
            "type": "object",
            "properties": {
                "ok": { "type": "boolean" },
                "closed": { "type": "boolean" },
                "phase": { "type": "string" },
                "retryable": { "type": "boolean" },
                "work_session": { "type": "object" },
                "finish": { "type": ["object", "null"] },
                "checkpoint": { "type": ["object", "null"] },
                "outbox": { "type": "object" },
                "task": { "type": "object" },
                "harness": { "type": "object" },
                "error": error_output_schema()
            },
            "required": ["ok"],
            "allOf": [
                {
                    "if": { "properties": { "ok": { "const": true } }, "required": ["ok"] },
                    "then": { "required": ["work_session", "finish", "checkpoint", "outbox", "task", "harness"] }
                },
                {
                    "if": { "properties": { "ok": { "const": false } }, "required": ["ok"] },
                    "then": { "required": ["closed", "phase", "finish", "checkpoint", "retryable", "error"] }
                }
            ],
            "additionalProperties": true
        }),
        "update_verification_disposition" => success_output_schema(
            json!({
                "task_id": { "type": "string", "minLength": 1 },
                "verification": { "type": "object" },
                "verification_status": { "type": "string", "enum": ["missing", "failed", "verified", "verified_with_exceptions"] },
                "effective_disposition": { "type": "string", "enum": ["active_failure", "expected_failure", "diagnostic_only", "superseded", "waived", "passed"] }
            }),
            &[
                "task_id",
                "verification",
                "verification_status",
                "effective_disposition",
            ],
        ),
        "stage_commit" | "stage_commit_status" | "wait_stage_commit" => success_output_schema(
            json!({
                "workflow_id": { "type": "string", "minLength": 1 },
                "idempotency_key": { "type": "string", "minLength": 1 },
                "workflow_status": { "type": "string", "minLength": 1 },
                "state": { "type": "string", "enum": ["running", "checkpoint_pending", "completed", "failed"] },
                "complete": { "type": "boolean" },
                "retryable": { "type": "boolean" },
                "task_id": { "type": "string", "minLength": 1 },
                "paths": { "type": "array", "items": { "type": "string" } },
                "checks": { "type": "array", "items": { "type": "object" } },
                "required_check_count": { "type": "integer", "minimum": 0 },
                "current_check_index": { "type": "integer", "minimum": 0 },
                "current_check": { "type": ["string", "null"] },
                "current_session_id": { "type": ["string", "null"] },
                "verification_ids": { "type": "array", "items": { "type": "string" } },
                "commit_sha": { "type": ["string", "null"] },
                "committed_files": { "type": "array", "items": { "type": "string" } },
                "working_tree_files": { "type": "array", "items": { "type": "string" } },
                "runtime_artifacts": { "type": "array", "items": { "type": "string" } },
                "ignored_files": { "type": "array", "items": { "type": "string" } },
                "baseline_refreshed": { "type": "boolean" },
                "checkpoint_hash": { "type": ["string", "null"] },
                "checkpoint_count": nullable_integer_property(),
                "next_actions": { "type": "array", "items": { "type": "string" } },
                "error": { "type": ["object", "null"] }
            }),
            &[
                "workflow_id",
                "idempotency_key",
                "workflow_status",
                "state",
                "complete",
                "retryable",
                "task_id",
                "paths",
                "checks",
                "required_check_count",
                "current_check_index",
                "verification_ids",
                "committed_files",
                "working_tree_files",
                "runtime_artifacts",
                "ignored_files",
                "baseline_refreshed",
                "next_actions",
            ],
        ),
        "finish_task" => json!({
            "type": "object",
            "properties": {
                "ok": { "type": "boolean" },
                "task_status": { "type": "string", "enum": ["verifying", "completed", "completed_unverified"] },
                "verification_status": { "type": "string", "enum": ["missing", "failed", "verified", "verified_with_exceptions", "unverified"] },
                "closed": { "type": "boolean" },
                "session_status": { "type": "string", "enum": ["active", "paused", "completed"] },
                "requested_session_status": { "type": "string", "enum": ["active", "paused", "completed"] },
                "next_stage_started": { "type": "boolean", "const": false },
                "reason": { "type": "string" },
                "next_actions": { "type": "array", "items": { "type": "string" } },
                "error": error_output_schema(),
                "blocking_verifications": { "type": "array", "items": { "type": "object" } },
                "working_tree_files": { "type": "array", "items": { "type": "string" } },
                "task": { "type": "object" },
                "verification": { "type": "array", "items": { "type": "object" } },
                "verification_summary": { "type": "object" },
                "change_summary": { "type": "object" },
                "worktree_cleanup": { "type": "object" },
                "truncated": { "type": "boolean" },
                "details_tool": { "type": "object" },
                "max_response_bytes": { "type": "integer", "minimum": 1 },
                "response_bytes": { "type": "integer", "minimum": 1 }
            },
            "required": ["ok", "task_status", "verification_status", "closed", "session_status", "next_stage_started", "task"],
            "allOf": [
                {
                    "if": { "properties": { "ok": { "const": true } }, "required": ["ok"] },
                    "then": {
                        "properties": { "closed": { "const": true } },
                        "required": ["change_summary", "worktree_cleanup", "truncated", "details_tool", "max_response_bytes", "response_bytes"]
                    }
                },
                {
                    "if": { "properties": { "ok": { "const": false } }, "required": ["ok"] },
                    "then": {
                        "properties": { "task_status": { "const": "verifying" }, "closed": { "const": false } },
                        "required": ["reason", "next_actions", "verification", "error"]
                    }
                }
            ],
            "additionalProperties": true
        }),
        "change_summary" => success_output_schema(
            json!({
                "task_id": { "type": "string", "minLength": 1 },
                "objective": { "type": "string" },
                "commit_sha": { "type": ["string", "null"] },
                "commit_count": { "type": "integer", "minimum": 0 },
                "first_commit": { "type": ["string", "null"] },
                "last_commit": { "type": ["string", "null"] },
                "commits": { "type": "array", "items": { "type": "object" } },
                "files_by_commit": { "type": "array", "items": { "type": "object" } },
                "committed_files": { "type": "array", "items": { "type": "string" } },
                "net_changed_files": { "type": "array", "items": { "type": "string" } },
                "working_tree_files": { "type": "array", "items": { "type": "string" } },
                "task_working_tree_files": { "type": "array", "items": { "type": "string" } },
                "peer_working_tree_files": { "type": "array", "items": { "type": "string" } },
                "unattributed_working_tree_files": { "type": "array", "items": { "type": "string" } },
                "runtime_artifacts": { "type": "array", "items": { "type": "string" } },
                "ignored_files": { "type": "array", "items": { "type": "string" } },
                "verification": { "type": "array", "items": { "type": "object" } },
                "verification_history_mode": { "type": "string", "enum": ["effective", "all"] },
                "verification_summary": { "type": "object" },
                "verification_status": { "type": "string", "enum": ["missing", "failed", "verified", "verified_with_exceptions"] },
                "evidence": { "type": "array", "items": { "type": "object" } },
                "counts": { "type": "object" },
                "baseline": { "type": "object" },
                "truncated": { "type": "boolean" },
                "next_cursor": { "type": ["object", "null"] },
                "section": { "type": "string" }
            }),
            &[
                "task_id",
                "objective",
                "commit_count",
                "commits",
                "files_by_commit",
                "committed_files",
                "net_changed_files",
                "working_tree_files",
                "runtime_artifacts",
                "ignored_files",
                "verification",
                "verification_history_mode",
                "verification_summary",
                "verification_status",
                "evidence",
                "counts",
                "baseline",
                "truncated",
                "next_cursor",
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

pub fn require_tool_profile(profile: &str) -> Result<&'static str, String> {
    match profile {
        "advanced" => Ok("advanced"),
        "core" => Ok("core"),
        "read-only" => Ok("read-only"),
        _ => Err(format!(
            "unsupported tool profile `{profile}`; expected core, advanced, or read-only"
        )),
    }
}

pub fn exposed_tool_names(tool_profile: &str) -> Vec<&'static str> {
    match require_tool_profile(tool_profile).expect("tool profile must be validated") {
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
        "browser_build_info" => json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        }),
        "export_work_session" => json!({
            "type": "object",
            "properties": {
                "task_id": { "type": "string", "minLength": 1 },
                "path": { "type": "string", "minLength": 1, "default": ".anchor/handoffs/<task_id>.json" },
                "overwrite": { "type": "boolean", "default": false }
            },
            "additionalProperties": false
        }),
        "browser_wait_for_build" => json!({
            "type": "object",
            "properties": {
                "expected_build": { "type": "string", "minLength": 1, "maxLength": 256 },
                "timeout_ms": { "type": "integer", "minimum": 1000, "maximum": 180000, "default": 60000 },
                "poll_interval_ms": { "type": "integer", "minimum": 250, "maximum": 5000, "default": 1000 },
                "clear_service_worker": { "type": "boolean", "default": true },
                "clear_cache": { "type": "boolean", "default": true }
            },
            "required": ["expected_build"],
            "additionalProperties": false
        }),
        "list_skills" => json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "Optional case-insensitive name/description filter" },
                "max_results": { "type": "integer", "minimum": 1, "maximum": 200, "default": 100 }
            },
            "additionalProperties": false
        }),
        "accept_latest_baseline" => json!({
            "type": "object",
            "properties": {
                "task_id": { "type": "string", "minLength": 1 },
                "reason": { "type": "string", "minLength": 1, "maxLength": 2000 },
                "max_attempts": { "type": "integer", "minimum": 1, "maximum": 10, "default": 3 }
            },
            "required": ["task_id", "reason"],
            "additionalProperties": false
        }),
        "stage_commit_status" => json!({
            "type": "object",
            "properties": {
                "task_id": { "type": "string", "minLength": 1 },
                "idempotency_key": { "type": "string", "minLength": 1, "maxLength": 128 }
            },
            "required": ["task_id", "idempotency_key"],
            "additionalProperties": false
        }),
        "wait_stage_commit" => json!({
            "type": "object",
            "properties": {
                "task_id": { "type": "string", "minLength": 1 },
                "idempotency_key": { "type": "string", "minLength": 1, "maxLength": 128 },
                "wait_timeout_ms": { "type": "integer", "minimum": 0, "maximum": 60000, "default": 30000 },
                "restart_lost_check": { "type": "boolean", "default": false },
                "history_checkpoint": {
                    "type": "object",
                    "required": ["session_key", "expected_path"],
                    "properties": {
                        "session_key": { "type": "string", "minLength": 1, "maxLength": 256 },
                        "expected_path": { "type": "string", "minLength": 1, "maxLength": 1024 }
                    },
                    "additionalProperties": true
                }
            },
            "required": ["task_id", "idempotency_key"],
            "additionalProperties": false
        }),
        "accept_current_baseline" => json!({
            "type": "object",
            "properties": {
                "task_id": { "type": "string", "minLength": 1 },
                "observation_token": { "type": "string", "minLength": 32 },
                "reason": { "type": "string", "minLength": 1, "maxLength": 2000 }
            },
            "required": ["task_id", "observation_token", "reason"],
            "additionalProperties": false
        }),
        "stage_commit" => json!({
            "type": "object",
            "properties": {
                "task_id": { "type": "string", "minLength": 1 },
                "expected_head": { "type": "string", "minLength": 40, "maxLength": 64 },
                "expected_fingerprint": { "type": "string", "minLength": 64, "maxLength": 64 },
                "paths": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": 256,
                    "items": { "type": "string", "minLength": 1 }
                },
                "message": { "type": "string", "minLength": 1, "maxLength": 2000 },
                "required_checks": {
                    "type": "array",
                    "maxItems": 16,
                    "default": [],
                    "items": { "type": "string", "minLength": 1, "maxLength": 4000 }
                },
                "check_timeout_ms": { "type": "integer", "minimum": 1000, "maximum": 600000, "default": 600000 },
                "execution_mode": { "type": "string", "enum": ["blocking", "deferred"], "default": "blocking" },
                "wait_timeout_ms": { "type": "integer", "minimum": 0, "maximum": 60000, "default": 0 },
                "history_checkpoint": {
                    "type": "object",
                    "required": ["session_key", "expected_path"],
                    "properties": {
                        "session_key": { "type": "string", "minLength": 1, "maxLength": 256 },
                        "expected_path": { "type": "string", "minLength": 1, "maxLength": 1024 }
                    },
                    "additionalProperties": true
                },
                "idempotency_key": { "type": "string", "minLength": 1, "maxLength": 128 },
                "reason": { "type": "string", "maxLength": 2000, "default": "" }
            },
            "required": [
                "task_id",
                "expected_head",
                "expected_fingerprint",
                "paths",
                "message",
                "idempotency_key"
            ],
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
        "list_skill_resources" => json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "minLength": 1 },
                "cursor": { "type": "integer", "minimum": 0, "default": 0 },
                "limit": { "type": "integer", "minimum": 1, "maximum": 500, "default": 100 }
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
                "limit": { "type": "integer", "minimum": 1, "maximum": 200, "default": 50 },
                "task_id": { "type": "string", "minLength": 1 },
                "history_session_key": { "type": "string", "minLength": 1 },
                "mcp_session_id": { "type": "string", "minLength": 1 },
                "started_after": { "type": "string", "minLength": 1, "description": "Epoch milliseconds or RFC3339" },
                "started_before": { "type": "string", "minLength": 1, "description": "Epoch milliseconds or RFC3339" },
                "tool": { "type": "string", "minLength": 1 },
                "status": { "type": "string", "enum": ["started", "running", "completed", "failed", "incomplete"] },
                "failures_only": { "type": "boolean", "default": false },
                "collapse": { "type": "boolean", "default": true }
            },
            "additionalProperties": false
        }),
        "begin_work_session" => json!({
            "type": "object",
            "properties": {
                "objective": { "type": "string", "minLength": 1, "maxLength": 4000 },
                "session_key": { "type": "string", "minLength": 1, "maxLength": 256 },
                "title": { "type": "string", "maxLength": 200 },
                "create_if_missing": { "type": "boolean", "default": true },
                "pause_current_and_start": { "type": "boolean", "default": true, "description": "Deprecated compatibility flag. Anchor always enforces a single writable task per workspace." },
                "workspace_mode": { "type": "string", "enum": ["shared", "worktree"], "default": "shared", "description": "Use the configured workspace by default, or create an isolated managed Git worktree for a new task." },
                "worktree_branch": { "type": "string", "minLength": 1, "maxLength": 255 },
                "worktree_base_ref": { "type": "string", "minLength": 1, "maxLength": 255, "default": "HEAD" },
                "worktree_remove_on_close": { "type": "boolean", "default": false },
                "history_dir": { "type": "string", "default": "docs/history-session" },
                "workspace_root": { "type": "string", "minLength": 1 }
            },
            "required": ["objective"],
            "additionalProperties": false
        }),
        "close_work_session" => json!({
            "type": "object",
            "properties": {
                "task_id": { "type": "string", "minLength": 1 },
                "summary": { "type": "string", "maxLength": 8000 },
                "allow_unverified": { "type": "boolean", "default": false },
                "session_status": { "type": "string", "enum": ["active", "paused", "completed"], "default": "paused" },
                "checkpoint": { "type": "object", "additionalProperties": true }
            },
            "required": ["task_id"],
            "additionalProperties": false
        }),
        "update_verification_disposition" => json!({
            "type": "object",
            "properties": {
                "task_id": { "type": "string", "minLength": 1 },
                "verification_id": { "type": "string", "minLength": 1 },
                "disposition": {
                    "type": "string",
                    "enum": ["active_failure", "expected_failure", "diagnostic_only", "superseded", "waived", "passed"]
                },
                "reason": { "type": "string", "minLength": 1, "maxLength": 2000 }
            },
            "required": ["task_id", "verification_id", "disposition", "reason"],
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
                "objective": { "type": "string", "minLength": 1 },
                "pause_current": { "type": "boolean", "default": true, "description": "Deprecated compatibility flag. Shared-checkout tasks transfer the shared writer lease; worktree tasks use independent write domains." },
                "workspace_mode": { "type": "string", "enum": ["shared", "worktree"], "default": "shared" },
                "worktree_branch": { "type": "string", "minLength": 1, "maxLength": 255 },
                "worktree_base_ref": { "type": "string", "minLength": 1, "maxLength": 255, "default": "HEAD" },
                "worktree_remove_on_close": { "type": "boolean", "default": false }
            },
            "required": ["objective"],
            "additionalProperties": false
        }),
        "refresh_baseline" => json!({
            "type": "object",
            "properties": {
                "task_id": { "type": "string", "minLength": 1 },
                "observed_head": { "type": "string", "minLength": 1 },
                "observed_fingerprint": { "type": "string", "minLength": 64, "maxLength": 64 },
                "reason": { "type": "string", "minLength": 1, "maxLength": 2000 }
            },
            "required": ["task_id", "observed_fingerprint", "reason"],
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
        "pause_task" | "resume_task" | "switch_task" => json!({
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
                "allow_unverified": { "type": "boolean", "default": false },
                "session_status": {
                    "type": "string",
                    "enum": ["active", "paused", "completed"],
                    "default": "paused",
                    "description": "Requested workspace session state after this task closes. If peer tasks remain active, the actual returned session_status stays active."
                }
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
            "properties": {
                "task_id": { "type": "string" },
                "change_id": { "type": "string" },
                "section": {
                    "type": "string",
                    "enum": ["commits", "files_by_commit", "committed_files", "net_changed_files", "working_tree_files", "runtime_artifacts", "ignored_files", "verification", "evidence"]
                },
                "verification_history_mode": { "type": "string", "enum": ["effective", "all"], "default": "effective" },
                "cursor": { "type": "integer", "minimum": 0, "default": 0 },
                "limit": { "type": "integer", "minimum": 1, "maximum": 500, "default": 64 }
            },
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
                "exclude_patterns": { "type": "array", "items": { "type": "string" } },
                "include_hidden": { "type": "boolean", "default": false },
                "include_ignored": { "type": "boolean", "default": false },
                "max_results": { "type": "integer", "minimum": 1, "maximum": 50000, "default": 5000 }
            },
            "additionalProperties": false
        }),
        "search_text" => json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "minLength": 1 },
                "path": { "type": "string", "default": "." },
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
                "mode": { "type": "string", "enum": ["exact", "fuzzy"], "default": "exact" },
                "validation_mode": { "type": "string", "enum": ["none", "syntax"], "default": "syntax" },
                "timeout_ms": { "type": "integer", "minimum": 1000, "maximum": 60000, "default": 20000 },
                "reason": { "type": "string", "default": "" }
            },
            "required": ["patch"],
            "additionalProperties": false
        }),
        "remove_path" => json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "minLength": 1 },
                "recursive": { "type": "boolean", "default": false }
            },
            "required": ["path"],
            "additionalProperties": false
        }),
        "patch_check" => json!({
            "type": "object",
            "properties": {
                "patch": { "type": "string", "minLength": 1 },
                "mode": { "type": "string", "enum": ["exact", "fuzzy"], "default": "exact" },
                "validation_mode": { "type": "string", "enum": ["none", "syntax"], "default": "syntax" },
                "timeout_ms": { "type": "integer", "minimum": 1000, "maximum": 60000, "default": 20000 }
            },
            "required": ["patch"],
            "additionalProperties": false
        }),
        "command_cost_explain" => json!({
            "type": "object",
            "properties": {
                "cmd": { "type": "string", "minLength": 1 },
                "timeout_ms": { "type": "integer", "minimum": 1, "maximum": 3600000, "default": 30000 },
                "cost_intent": { "type": "string", "enum": ["auto", "local_only", "external_paid"], "default": "auto" },
                "network_mode": { "type": "string", "enum": ["auto", "disabled", "enabled"], "default": "auto" }
            },
            "required": ["cmd"],
            "additionalProperties": false
        }),
        "exec_command" => json!({
            "type": "object",
            "minProperties": 1,
            "properties": {
                "cmd": { "type": "string", "minLength": 1 },
                "executable": { "type": "string", "minLength": 1, "description": "Executable to run directly without shell parsing." },
                "args": { "type": "array", "maxItems": 256, "items": { "type": "string", "maxLength": 32768 }, "description": "Exact argument vector for executable or shell mode." },
                "env": { "type": "object", "maxProperties": 128, "additionalProperties": { "type": "string", "maxLength": 131072 }, "description": "Environment variables applied only to this command." },
                "shell": { "type": "string", "enum": ["direct", "pwsh", "powershell", "cmd"], "description": "Optional explicit Windows shell. Use direct with executable for shell-free execution." },
                "workdir": { "type": "string", "default": "." },
                "timeout_ms": { "type": "integer", "minimum": 1, "maximum": 3600000, "default": 30000 },
                "max_output_bytes": { "type": "integer", "minimum": 1024, "maximum": 1048576, "default": 32768 },
                "yield_time_ms": { "type": "integer", "minimum": 0, "maximum": 30000, "default": 1000 },
                "tty": { "type": "boolean", "default": false },
                "stdin": { "type": "string", "default": "" },
                "cost_intent": {
                    "type": "string",
                    "enum": ["auto", "local_only", "external_paid"],
                    "default": "auto",
                    "description": "Declares whether this execution is local-only or intentionally uses an external paid service."
                },
                "network_mode": {
                    "type": "string",
                    "enum": ["auto", "disabled", "enabled"],
                    "default": "auto",
                    "description": "Declares whether the command is expected to use network access. Explicit paid evidence that conflicts with disabled/local-only declarations is rejected."
                },
                "verification_kind": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": 128,
                    "description": "Optional verification category such as lint, test, build, check, or diff_check. Terminal results are persisted as Harness verification evidence when a task is active."
                },
                "verification_key": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": 256,
                    "description": "Stable caller-provided identity used to supersede earlier results for the same logical verification."
                },
                "test_file": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": 2000,
                    "description": "Optional test file identity for verification supersession and diagnostics."
                },
                "test_name": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": 1000,
                    "description": "Optional test name identity for verification supersession and diagnostics."
                },
                "verification_level": {
                    "type": "string",
                    "enum": ["diagnostic", "informational", "required", "blocking"],
                    "default": "blocking",
                    "description": "Controls whether a failed verification blocks task completion. Diagnostic and informational failures are recorded as non-blocking evidence."
                },
                "supersede_previous_failures": {
                    "type": "boolean",
                    "default": true,
                    "description": "When a verification passes, supersede active failures matching verification_key, test_file/test_name, or the same verification kind for legacy callers."
                },
                "filesystem_scope": { "type": "string", "enum": ["workspace"], "default": "workspace" },
                "include_diagnostics": { "type": "boolean", "default": false, "description": "Include cost policy, execution boundary, and sandbox diagnostics in the response." },
                "reason": { "type": "string", "default": "" }
            },
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
        "wait_command" => json!({
            "type": "object",
            "properties": {
                "session_id": { "type": "string", "minLength": 1 },
                "timeout_ms": { "type": "integer", "minimum": 0, "maximum": 60000, "default": 30000 },
                "stdout_offset": { "type": "integer", "minimum": 0, "description": "Optional explicit cursor. Omit it to continue from the caller session's last returned stdout offset." },
                "stderr_offset": { "type": "integer", "minimum": 0, "description": "Optional explicit cursor. Omit it to continue from the caller session's last returned stderr offset." },
                "limit": { "type": "integer", "minimum": 1, "maximum": 1048576, "default": 65536 },
                "return_incremental_output": { "type": "boolean", "default": true },
                "stop_on_patterns": { "type": "array", "maxItems": 16, "items": { "type": "string", "minLength": 1 } }
            },
            "required": ["session_id"],
            "additionalProperties": false
        }),
        "list_command_sessions" => json!({
            "type": "object",
            "properties": {
                "include_terminal": { "type": "boolean", "default": true },
                "max_output_bytes": { "type": "integer", "minimum": 0, "maximum": 65536, "default": 4096 }
            },
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
        "git_worktree_list" | "git_worktree_prune" => json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        }),
        "git_worktree_create" => json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "minLength": 1, "maxLength": 128 },
                "branch": { "type": "string", "minLength": 1, "maxLength": 255 },
                "base_ref": { "type": "string", "minLength": 1, "maxLength": 255, "default": "HEAD" },
                "remove_on_close": { "type": "boolean", "default": false }
            },
            "required": ["name"],
            "additionalProperties": false
        }),
        "git_worktree_remove" => json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "minLength": 1 },
                "force": { "type": "boolean", "default": false }
            },
            "required": ["path"],
            "additionalProperties": false
        }),
        "git_stage" => json!({
            "type": "object",
            "properties": {
                "paths": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": 256,
                    "items": { "type": "string", "minLength": 1 }
                }
            },
            "required": ["paths"],
            "additionalProperties": false
        }),
        "git_reset" => json!({
            "type": "object",
            "properties": {
                "revision": { "type": "string", "default": "HEAD" },
                "mode": { "type": "string", "enum": ["soft", "mixed", "hard"], "default": "mixed" }
            },
            "additionalProperties": false
        }),
        "git_revert" => json!({
            "type": "object",
            "properties": {
                "revision": { "type": "string" },
                "abort": { "type": "boolean", "default": false }
            },
            "additionalProperties": false
        }),
        "git_clean" => json!({
            "type": "object",
            "properties": {
                "dry_run": { "type": "boolean", "default": true },
                "directories": { "type": "boolean", "default": false },
                "include_ignored": { "type": "boolean", "default": false },
                "paths": {
                    "type": "array",
                    "maxItems": 256,
                    "default": [],
                    "items": { "type": "string", "minLength": 1 }
                }
            },
            "additionalProperties": false
        }),
        "git_commit" => json!({
            "type": "object",
            "properties": {
                "message": { "type": "string", "minLength": 1, "maxLength": 4000 }
            },
            "required": ["message"],
            "additionalProperties": false
        }),
        "git_restore" => json!({
            "type": "object",
            "properties": {
                "paths": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": 256,
                    "items": { "type": "string", "minLength": 1 }
                },
                "staged": { "type": "boolean", "default": false },
                "worktree": { "type": "boolean", "default": true }
            },
            "required": ["paths"],
            "additionalProperties": false
        }),
        "git_status" => json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "default": "." },
                "include_untracked": { "type": "boolean", "default": true },
                "max_entries": { "type": "integer", "minimum": 1, "maximum": 10000, "default": 1000 },
                "diagnose_metadata_only": { "type": "boolean", "default": true },
                "refresh_index": { "type": "boolean", "default": false, "description": "Safely refresh index stat metadata only for paths whose filtered worktree blob already matches the index blob." }
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

    use super::{input_schema, list_tools_for_profile, output_schema, require_tool_profile};

    #[test]
    fn core_catalog_exposes_44_chatgpt_compatible_tools() {
        let tools = list_tools_for_profile("core");
        let names: Vec<_> = tools
            .iter()
            .map(|tool| tool["name"].as_str().expect("tool name"))
            .collect();
        let unique: HashSet<_> = names.iter().copied().collect();

        assert_eq!(tools.len(), 44);
        assert_eq!(unique.len(), tools.len());
        assert!(names.contains(&"list_skills"));
        assert!(names.contains(&"git_worktree_list"));
        assert!(names.contains(&"load_skill"));
        assert!(names.contains(&"list_skill_resources"));
        assert!(names.contains(&"read_skill_resource"));
        assert!(names.contains(&"history_session_bootstrap"));
        assert!(names.contains(&"history_session_checkpoint"));
        assert!(names.contains(&"history_session_validate"));
        assert!(names.contains(&"begin_work_session"));
        assert!(names.contains(&"close_work_session"));
        assert!(names.contains(&"wait_command"));
        assert!(names.contains(&"list_command_sessions"));
        assert!(names.contains(&"browser_build_info"));
        assert!(names.contains(&"browser_wait_for_build"));
        assert!(names.contains(&"search_text"));
        assert!(names.contains(&"command_cost_explain"));
        assert!(names.contains(&"git_stage"));
        assert!(names.contains(&"git_commit"));
        assert!(names.contains(&"git_restore"));
        assert!(names.contains(&"remove_path"));
        assert!(names.contains(&"git_reset"));
        assert!(names.contains(&"git_revert"));
        assert!(names.contains(&"git_clean"));
        assert!(names.contains(&"accept_latest_baseline"));
        assert!(names.contains(&"switch_task"));
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
    fn only_current_profile_names_are_accepted() {
        assert_eq!(require_tool_profile("core").unwrap(), "core");
        assert_eq!(require_tool_profile("advanced").unwrap(), "advanced");
        assert_eq!(require_tool_profile("read-only").unwrap(), "read-only");
        assert!(require_tool_profile("unknown").is_err());
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
}
