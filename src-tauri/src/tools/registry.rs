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
        "Read a UTF-8 text file slice inside the configured workspace.",
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
        "Search UTF-8 workspace files for text or regex matches.",
        true,
        false,
        false,
    ),
    (
        "grep_text",
        "Grep workspace text",
        "Search workspace text with grep-style regex, glob, context, and bounded results.",
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
        "Return a workspace image as MCP image content.",
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
    "grep_text",
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
    "grep_text",
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

pub fn output_schema(name: &str) -> Value {
    if name == "view_image" {
        return json!({
            "type": "object",
            "properties": {
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
            },
            "required": ["ok"],
            "additionalProperties": true
        });
    }
    json!({
        "type": "object",
        "properties": {
            "ok": { "type": "boolean" },
            "error": { "type": "object" }
        },
        "required": ["ok"],
        "additionalProperties": true
    })
}

pub fn canonical_tool_name(name: &str) -> &str {
    match name {
        "grep" => "grep_text",
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
                "session_key": { "type": "string", "minLength": 1 },
                "title": { "type": "string" },
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
                "session_key": { "type": "string", "minLength": 1 },
                "expected_path": { "type": "string", "minLength": 1 },
                "history_dir": { "type": "string", "default": "docs/history-session" },
                "turn_id": { "type": "string", "minLength": 1 },
                "timestamp": { "type": "string" },
                "user_intent": { "type": "string" },
                "findings": { "type": "array", "items": { "type": "string" } },
                "decisions": { "type": "array", "items": { "type": "string" } },
                "files_changed": { "type": "array", "items": { "type": "string" } },
                "tests": { "type": "array", "items": { "type": "string" } },
                "runtime_state": { "type": "array", "items": { "type": "string" } },
                "remaining_issues": { "type": "array", "items": { "type": "string" } },
                "next_actions": { "type": "array", "items": { "type": "string" } },
                "notes": { "type": "string" }
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
    fn core_catalog_exposes_26_chatgpt_compatible_tools() {
        let tools = list_tools_for_profile("core");
        let names: Vec<_> = tools
            .iter()
            .map(|tool| tool["name"].as_str().expect("tool name"))
            .collect();
        let unique: HashSet<_> = names.iter().copied().collect();

        assert_eq!(tools.len(), 26);
        assert_eq!(unique.len(), tools.len());
        assert!(names.contains(&"list_skills"));
        assert!(names.contains(&"load_skill"));
        assert!(names.contains(&"read_skill_resource"));
        assert!(names.contains(&"history_session_bootstrap"));
        assert!(names.contains(&"history_session_checkpoint"));
        assert!(names.contains(&"history_session_validate"));
        assert!(names.contains(&"grep_text"));
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
