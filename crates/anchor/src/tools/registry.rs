use std::collections::BTreeMap;

use serde_json::{json, Value};

pub const CATALOG_VERSION: u32 = 45;

const FACADE_NAMES: &[&str] = &[
    "session",
    "skill",
    "git",
    "task",
    "slice",
    "commit_stage",
    "environment",
    "cwd",
];

pub const SESSION_OPERATIONS: &[(&str, &str)] = &[
    ("open", "session_open"),
    ("checkpoint", "session_checkpoint"),
    ("list", "session_list"),
    ("get", "session_get"),
    ("validate", "session_validate"),
];

pub const ENVIRONMENT_OPERATIONS: &[(&str, &str)] = &[
    ("check", "check_exec_environment"),
    ("health", "exec_health_check"),
    ("cost", "command_cost_explain"),
];

pub const CWD_OPERATIONS: &[(&str, &str)] =
    &[("get", "get_default_cwd"), ("set", "set_default_cwd")];

pub const SKILL_OPERATIONS: &[(&str, &str)] = &[
    ("list", "list_skills"),
    ("get", "load_skill"),
    ("read_resource", "read_skill_resource"),
];

pub const GIT_OPERATIONS: &[(&str, &str)] = &[
    ("status", "git_status"),
    ("worktree_list", "git_worktree_list"),
    ("worktree_create", "git_worktree_create"),
    ("worktree_remove", "git_worktree_remove"),
    ("worktree_prune", "git_worktree_prune"),
    ("merge", "git_merge"),
    ("branch_create", "git_branch_create"),
    ("branch_delete", "git_branch_delete"),
    ("is_ancestor", "git_is_ancestor"),
    ("switch", "git_switch"),
    ("stage", "git_stage"),
    ("commit", "git_commit"),
    ("restore", "git_restore"),
    ("reset", "git_reset"),
    ("revert", "git_revert"),
    ("clean", "git_clean"),
    ("diff", "git_diff"),
    ("log", "git_log"),
    ("show", "git_show"),
    ("blame", "git_blame"),
];

pub const TASK_OPERATIONS: &[(&str, &str)] = &[
    ("status", "harness_status"),
    ("operation_log", "operation_log"),
    ("resolve_recovery", "resolve_recovery"),
    (
        "verification_disposition",
        "update_verification_disposition",
    ),
    ("project_state", "project_state"),
    ("start", "start_task"),
    ("refresh_baseline", "refresh_baseline"),
    ("accept_current_baseline", "accept_current_baseline"),
    ("accept_latest_baseline", "accept_latest_baseline"),
    ("update", "update_task"),
    ("gate_status", "task_gate_status"),
    ("pause", "pause_task"),
    ("abort", "abort_task"),
    ("resume", "resume_task"),
    ("switch", "switch_task"),
    ("finish", "finish_task"),
    ("context", "task_context"),
    ("events", "list_task_events"),
    ("change_summary", "change_summary"),
    ("export", "export_work_session"),
];

pub const SLICE_OPERATIONS: &[(&str, &str)] = &[
    ("start", "start_slice"),
    ("update", "update_slice"),
    ("complete", "complete_slice"),
];

pub const COMMIT_STAGE_OPERATIONS: &[(&str, &str)] = &[
    ("run", "stage_commit"),
    ("status", "stage_commit_status"),
    ("wait", "wait_stage_commit"),
];

fn facade_operations(facade: &str) -> Option<&'static [(&'static str, &'static str)]> {
    match facade {
        "session" => Some(SESSION_OPERATIONS),
        "skill" => Some(SKILL_OPERATIONS),
        "git" => Some(GIT_OPERATIONS),
        "task" => Some(TASK_OPERATIONS),
        "slice" => Some(SLICE_OPERATIONS),
        "commit_stage" => Some(COMMIT_STAGE_OPERATIONS),
        "environment" => Some(ENVIRONMENT_OPERATIONS),
        "cwd" => Some(CWD_OPERATIONS),
        _ => None,
    }
}

fn merge_output_properties(parts: impl IntoIterator<Item = Value>) -> Value {
    let mut merged = serde_json::Map::new();
    for part in parts {
        if let Value::Object(properties) = part {
            merged.extend(properties);
        }
    }
    Value::Object(merged)
}

pub fn is_facade_tool(name: &str) -> bool {
    facade_operations(name).is_some()
}

pub fn facade_for_operation_tool(name: &str) -> Option<&'static str> {
    FACADE_NAMES.iter().copied().find(|facade| {
        facade_operations(facade)
            .into_iter()
            .flatten()
            .any(|(_, tool)| *tool == name)
    })
}

pub fn is_facade_operation_tool(name: &str) -> bool {
    facade_for_operation_tool(name).is_some()
}

pub fn facade_tool_for_operation(facade: &str, operation: &str) -> Option<&'static str> {
    facade_operations(facade)?
        .iter()
        .find_map(|(candidate, tool)| (*candidate == operation).then_some(*tool))
}

pub fn facade_operations_for_profile(facade: &str, tool_profile: &str) -> Vec<&'static str> {
    let available = profile_tool_names(tool_profile);
    facade_operations(facade)
        .into_iter()
        .flatten()
        .filter_map(|(operation, tool)| available.contains(tool).then_some(*operation))
        .collect()
}

pub fn facade_operation_argument_contract(
    facade: &str,
    operation: &str,
) -> Option<(Vec<String>, Vec<String>)> {
    let tool = facade_tool_for_operation(facade, operation)?;
    let schema = input_schema(tool);
    let mut allowed = schema
        .get("properties")
        .and_then(Value::as_object)
        .into_iter()
        .flat_map(|properties| properties.keys().cloned())
        .collect::<Vec<_>>();
    let mut required = schema
        .get("required")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect::<Vec<_>>();
    allowed.sort();
    required.sort();
    Some((allowed, required))
}

pub fn git_tool_for_operation(operation: &str) -> Option<&'static str> {
    facade_tool_for_operation("git", operation)
}

pub fn git_operations_for_profile(tool_profile: &str) -> Vec<&'static str> {
    facade_operations_for_profile("git", tool_profile)
}

pub const P0_TOOLS: &[(&str, &str, &str, bool, bool, bool)] = &[
    (
        "session",
        "Session",
        "[anchor-core] Manage the current development Session through one domain tool. Use open once at conversation start; open never injects other Session history. Use list/get only when prior Session history is explicitly needed, checkpoint to persist current progress, and validate for explicit store integrity checks.",
        false,
        false,
        false,
    ),
    (
        "git_merge",
        "Git fast-forward merge",
        "Fast-forward the current branch to an explicit Git ref without creating a merge commit.",
        false,
        false,
        false,
    ),
    (
        "git_branch_create",
        "Git branch create",
        "Create one local branch at an explicit start point without switching the worktree.",
        false,
        false,
        false,
    ),
    (
        "git_branch_delete",
        "Git branch delete",
        "Delete one local branch; force deletion requires operator-enabled dangerous mode.",
        false,
        true,
        false,
    ),
    (
        "git_is_ancestor",
        "Git ancestry check",
        "Return whether one explicit commit-ish is an ancestor of another.",
        true,
        false,
        false,
    ),
    (
        "git_switch",
        "Git switch",
        "Switch a clean worktree to an existing local branch without invoking a shell.",
        false,
        false,
        false,
    ),
    (
        "skill",
        "Agent Skill",
        "[anchor-core anchor-skill] Discover and load workspace Agent Skills through one read-only MCP tool for hosts such as ChatGPT Developer Mode that reliably discover tools but may not surface native Skill UI. Use operation=list to find a relevant Skill, operation=get before following its instructions, and operation=read_resource only for supporting files needed by that Skill.",
        true,
        false,
        false,
    ),
    (
        "environment",
        "Environment",
        "[anchor-command] Inspect the execution environment through one read-only domain tool. Set operation to check, health, or cost; operation-specific arguments are validated against the existing command contracts.",
        true,
        false,
        false,
    ),
    (
        "cwd",
        "Working directory",
        "[anchor-core] Read or update the session-scoped default working directory through one domain tool. Set operation to get or set; profile permissions remain enforced per operation.",
        false,
        false,
        false,
    ),
    (
        "task",
        "Task",
        "[anchor-core anchor-task] Manage and inspect Harness tasks through one domain tool. Set operation to a profile-available task action; operation-specific arguments are validated against the existing Harness contracts.",
        false,
        true,
        false,
    ),
    (
        "slice",
        "Task slice",
        "[anchor-task] Manage first-class task Slices through one domain tool. Set operation to start, update, or complete; arguments are validated against the existing Slice contracts.",
        false,
        true,
        false,
    ),
    (
        "commit_stage",
        "Commit stage workflow",
        "[anchor-task] Run, inspect, or wait for the durable staged-commit workflow through one domain tool. Set operation to run, status, or wait.",
        false,
        true,
        false,
    ),
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
        "resolve_recovery",
        "Resolve task recovery",
        "Resolve the current Task Recovery using explicit post-failure evidence when the logical work was completed by a corrected or later step.",
        false,
        false,
        false,
    ),
    (
        "begin_work_session",
        "Begin work session",
        "[anchor-core] Open or resume only the current isolated Session and bind it to a shared-checkout task by default or an explicitly requested isolated Git worktree task. With create_if_missing=false this is recovery-only: Anchor must not create a Session, Harness Task, or Git worktree.",
        false,
        false,
        false,
    ),
    (
        "close_work_session",
        "Close work session",
        "[anchor-core] Close the bound Harness Task as completed (validated by completion gates) or explicitly incomplete after a user/operator abort, then persist the matching Session checkpoint as a recoverable workflow. Closure is rejected while retained commands are running or their terminal results are unconsumed.",
        false,
        false,
        false,
    ),
    (
        "complete_work_session",
        "Complete work session",
        "[anchor-core] Enforce the full persisted task contract, require verified evidence, close the Harness Task, and save the final History checkpoint. This strict completion path cannot bypass verification.",
        false,
        false,
        false,
    ),
    (
        "wait_command",
        "Wait for command",
        "[anchor-core anchor-command] Wait for a retained command session and return explicit terminal state plus incremental stdout/stderr; a terminal response consumes the retained result for completion gates.",
        true,
        false,
        false,
    ),
    (
        "list_command_sessions",
        "List command sessions",
        "[anchor-core anchor-command] Return retained command sessions, stable execution duration, separate session age/retention time, and running or terminal-unconsumed result counts for reliable resume and final-response checks.",
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
        "[anchor-core] Return server, workspace, auth, profile, exposed-tool metadata, and lazy-schema discovery guidance.",
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
        "read_skill_resource",
        "Read Skill resource",
        "Read a supporting resource or script source inside a discovered Skill directory without executing it.",
        true,
        false,
        false,
    ),
    (
        "session_open",
        "Open Session",
        "Create or resume only the current development Session. This operation never returns or injects other Session content.",
        false,
        false,
        false,
    ),
    (
        "session_checkpoint",
        "Checkpoint Session",
        "Persist one idempotent, redacted checkpoint for an explicit session_id and expected_path.",
        false,
        false,
        false,
    ),
    (
        "session_list",
        "List Sessions",
        "List bounded Session metadata from the Session index without reading Session Markdown bodies.",
        true,
        false,
        false,
    ),
    (
        "session_get",
        "Get Session",
        "Read one explicit Session document by opaque session_id with a bounded byte budget.",
        true,
        false,
        false,
    ),
    (
        "session_validate",
        "Validate Session store",
        "Validate the new docs/session store and optionally rebuild its metadata index. The frozen docs/history-session legacy archive is never scanned or migrated.",
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
        "Start a durable coding task, capture the shared workspace baseline, and bind it to the calling MCP session while preserving peer task lifecycle states.",
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
        "Update durable task steps, phase, contract, Slice plan, and working set.",
        false,
        false,
        false,
    ),
    (
        "task_gate_status",
        "Task completion gate",
        "Return every currently missing completion condition, including commands, Git state, required verifications, pending steps, Slices, recovery, and strict close policy.",
        true,
        false,
        false,
    ),
    (
        "start_slice",
        "Start task Slice",
        "Start a first-class task Slice with declared files and acceptance checks.",
        false,
        false,
        false,
    ),
    (
        "update_slice",
        "Update task Slice",
        "Update a Slice without bypassing its completion gate.",
        false,
        false,
        false,
    ),
    (
        "complete_slice",
        "Complete task Slice",
        "Complete a Slice only after its acceptance checks and optional commit requirement pass.",
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
        "abort_task",
        "Abort incomplete task",
        "Terminate one explicit task as incomplete after an explicit user/operator stop decision. This never satisfies completion gates and never rewrites the task as completed.",
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
        "[anchor-core anchor-files] Read a UTF-8 or BOM-marked UTF-16 text file slice strictly inside the configured workspace.",
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
        "[anchor-core anchor-files] List workspace files using glob filters.",
        true,
        false,
        false,
    ),
    (
        "grep",
        "Grep repository",
        "[anchor-core anchor-files] Search workspace text with structured results; uses ripgrep acceleration when available and falls back to Anchor's built-in scanner.",
        true,
        false,
        false,
    ),
    (
        "search",
        "Search repository",
        "[anchor-core anchor-files] Search repository text and code structure through one deterministic interface; engine selection is managed internally by Anchor.",
        true,
        false,
        false,
    ),
    (
        "replace_text",
        "Replace text",
        "[anchor-core anchor-files] Replace one literal string across an explicit bounded file set with dry-run, SHA/match preconditions, encoding/mode preservation, and pre-commit CAS.",
        false,
        true,
        false,
    ),
    (
        "apply_patch",
        "Apply patch",
        "[anchor-core anchor-files] Apply a patch envelope transactionally with cooperative cancellation, bounded processing time, and atomic rollback inside the workspace.",
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
        "[anchor-core anchor-command] Run a bounded command in the workspace under runtime policy.",
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
        "[anchor-core anchor-command] Write characters to a server-managed running command session.",
        false,
        true,
        false,
    ),
    (
        "kill_session",
        "Kill session",
        "[anchor-core anchor-command] Terminate a server-managed running command session and return its consumed terminal result.",
        false,
        true,
        false,
    ),
    (
        "read_output",
        "Read output",
        "[anchor-core anchor-command] Read retained stdout or stderr by output_ref with per-stream byte offset pagination.",
        true,
        false,
        false,
    ),
    (
        "git",
        "Git",
        "[anchor-core anchor-git] Inspect or mutate Git through one domain tool. Supports status, structured branch/switch/fast-forward merge/ancestry checks, diff/log/show/blame, stage/commit/restore/reset/revert/clean, and managed worktree operations.",
        false,
        true,
        false,
    ),
    (
        "git_status",
        "Git status",
        "[anchor-core anchor-git] Return git working tree status for the workspace.",
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
        "[anchor-core anchor-git] Stage explicit workspace-relative paths without invoking a shell.",
        false,
        false,
        false,
    ),
    (
        "git_commit",
        "Git commit",
        "[anchor-core anchor-git] Commit the currently staged changes with an explicit message and return the exact committed files.",
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
        "[anchor-core anchor-git] Return unified git diff for workspace changes.",
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

/// Core profile capability set. It includes internal facade operation handlers;
/// `exposed_tool_names` removes those implementation details before publication.
pub const CORE_TOOLS: &[&str] = &[
    "server_info",
    "browser_build_info",
    "browser_wait_for_build",
    "begin_work_session",
    "close_work_session",
    "session",
    "session_open",
    "session_checkpoint",
    "session_list",
    "session_get",
    "session_validate",
    "skill",
    "task",
    "update_verification_disposition",
    "resolve_recovery",
    "accept_latest_baseline",
    "list_skills",
    "load_skill",
    "read_skill_resource",
    "switch_task",
    "environment",
    "cwd",
    "check_exec_environment",
    "command_cost_explain",
    "get_default_cwd",
    "set_default_cwd",
    "read_file",
    "list_dir",
    "list_files",
    "search",
    "replace_text",
    "apply_patch",
    "remove_path",
    "exec_command",
    "write_stdin",
    "wait_command",
    "list_command_sessions",
    "kill_session",
    "read_output",
    "git",
    "git_status",
    "git_worktree_list",
    "git_merge",
    "git_branch_create",
    "git_branch_delete",
    "git_is_ancestor",
    "git_switch",
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

/// Advanced is an explicit additive profile, not an alias for every registered
/// local tool. This prevents newly introduced diagnostics/admin helpers from
/// silently expanding the published ChatGPT catalog.
pub const ADVANCED_EXTRA_TOOLS: &[&str] = &[
    "slice",
    "commit_stage",
    "complete_work_session",
    "patch_check",
    "harness_status",
    "operation_log",
    "project_state",
    "refresh_baseline",
    "accept_current_baseline",
    "start_task",
    "update_task",
    "task_gate_status",
    "pause_task",
    "abort_task",
    "resume_task",
    "finish_task",
    "task_context",
    "list_task_events",
    "change_summary",
    "export_work_session",
    "start_slice",
    "update_slice",
    "complete_slice",
    "stage_commit",
    "stage_commit_status",
    "wait_stage_commit",
    "exec_health_check",
    "git_worktree_create",
    "git_worktree_remove",
    "git_worktree_prune",
];

pub const CORE_READ_ONLY_TOOLS: &[&str] = &[
    "server_info",
    "browser_build_info",
    "session",
    "session_list",
    "session_get",
    "skill",
    "list_skills",
    "load_skill",
    "read_skill_resource",
    "environment",
    "cwd",
    "check_exec_environment",
    "command_cost_explain",
    "get_default_cwd",
    "read_file",
    "list_dir",
    "list_files",
    "search",
    "read_output",
    "wait_command",
    "list_command_sessions",
    "git",
    "git_status",
    "git_worktree_list",
    "git_is_ancestor",
    "git_diff",
    "git_log",
    "git_show",
    "git_blame",
    "view_image",
];

pub const MUTATING_TOOLS: &[&str] = &[
    "session",
    "session_open",
    "session_checkpoint",
    "session_validate",
    "task",
    "slice",
    "commit_stage",
    "begin_work_session",
    "complete_work_session",
    "browser_wait_for_build",
    "close_work_session",
    "cwd",
    "replace_text",
    "apply_patch",
    "remove_path",
    "exec_command",
    "git",
    "write_stdin",
    "kill_session",
    "set_default_cwd",
];

pub fn is_allowed_tool(name: &str) -> bool {
    exposed_tool_names("advanced").contains(&name)
}

fn error_output_schema() -> Value {
    // This schema primitive is also reused by nested/internal intermediate results
    // (for example staged-commit child command receipts) before they cross the
    // public tool boundary. Machine-actionable fields are therefore declared here
    // but are enforced at the public boundary by the shared error normalizer and
    // public contract tests rather than required on every intermediate object.
    json!({
        "type": "object",
        "properties": {
            "code": { "type": "string", "minLength": 1 },
            "error_code": { "type": "string", "minLength": 1 },
            "message": { "type": "string", "minLength": 1 },
            "category": { "type": "string", "minLength": 1 },
            "retryable": { "type": "boolean" },
            "cause_scope": { "type": "string", "minLength": 1 },
            "workspace_mutated": { "type": ["boolean", "null"] },
            "recommended_retry": {},
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

fn verification_requirement_input_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "id": { "type": "string", "minLength": 1, "maxLength": 128 },
            "description": { "type": "string", "maxLength": 2000, "default": "" },
            "kind": { "type": "string", "minLength": 1, "maxLength": 128 },
            "verification_key": { "type": "string", "minLength": 1, "maxLength": 256 },
            "test_file": { "type": "string", "minLength": 1, "maxLength": 2000 },
            "test_name": { "type": "string", "minLength": 1, "maxLength": 1000 }
        },
        "anyOf": [
            { "required": ["id", "kind"] },
            { "required": ["id", "verification_key"] },
            { "required": ["id", "test_file"] },
            { "required": ["id", "test_name"] }
        ],
        "additionalProperties": false
    })
}

fn task_completion_policy_input_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "require_pending_steps_empty": { "type": "boolean", "default": false },
            "require_all_slices_completed": { "type": "boolean", "default": true },
            "require_slice_commits": { "type": "boolean", "default": false },
            "require_no_open_recovery": { "type": "boolean", "default": true },
            "require_ready_to_close": { "type": "boolean", "default": false },
            "require_complete_work_session": { "type": "boolean", "default": false },
            "disallow_unverified_completion": { "type": "boolean", "default": false }
        },
        "additionalProperties": false
    })
}

fn task_contract_input_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "no_early_stop": { "type": "boolean", "default": false },
            "constraints": {
                "type": "array",
                "maxItems": 64,
                "items": { "type": "string", "minLength": 1, "maxLength": 2000 }
            },
            "required_verifications": {
                "type": "array",
                "maxItems": 64,
                "items": verification_requirement_input_schema()
            },
            "completion_policy": task_completion_policy_input_schema()
        },
        "additionalProperties": false
    })
}

fn task_slice_input_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "id": { "type": "string", "minLength": 1, "maxLength": 128 },
            "title": { "type": "string", "minLength": 1, "maxLength": 500 },
            "status": {
                "type": "string",
                "enum": ["planned", "in_progress", "verifying", "blocked", "paused"],
                "default": "planned"
            },
            "files": {
                "type": "array",
                "maxItems": 256,
                "items": { "type": "string", "minLength": 1, "maxLength": 2000 }
            },
            "acceptance_checks": {
                "type": "array",
                "maxItems": 64,
                "items": verification_requirement_input_schema()
            },
            "commit_sha": { "type": ["string", "null"], "maxLength": 128 },
            "blocker": { "type": ["string", "null"], "maxLength": 2000 }
        },
        "required": ["id", "title"],
        "additionalProperties": false
    })
}

fn task_working_set_input_schema() -> Value {
    let paths = json!({
        "type": "array",
        "maxItems": 256,
        "items": { "type": "string", "minLength": 1, "maxLength": 2000 }
    });
    json!({
        "type": "object",
        "properties": {
            "primary": paths.clone(),
            "tests": paths.clone(),
            "locales": paths.clone(),
            "reference_only": paths
        },
        "additionalProperties": false
    })
}

fn task_configuration_input_properties() -> Value {
    json!({
        "phase": {
            "type": "string",
            "enum": [
                "unspecified", "planning", "implementing", "verifying", "deploying",
                "browser_review", "cleanup", "ready_to_close", "blocked", "paused"
            ]
        },
        "contract": task_contract_input_schema(),
        "slices": {
            "type": "array",
            "maxItems": 64,
            "items": task_slice_input_schema()
        },
        "working_set": task_working_set_input_schema()
    })
}

fn session_open_output_properties() -> Value {
    json!({
        "session_id": { "type": "string", "pattern": "^ses_[0-9a-fA-F]{32}$" },
        "session_path": { "type": "string", "minLength": 1 },
        "created": { "type": "boolean" },
        "resumed": { "type": "boolean" },
        "session_status": { "type": "string", "enum": ["active", "paused", "completed"] },
        "previous_status": { "type": "string", "enum": ["active", "paused", "completed", "unknown"] },
        "reactivated": { "type": "boolean" },
        "parent_session_id": { "type": ["string", "null"] },
        "continuation_created": { "type": "boolean" },
        "checkpoint_count": { "type": "integer", "minimum": 0 },
        "automatic_history_loading": { "type": "boolean", "const": false },
        "history_injected": { "type": "boolean", "const": false },
        "archive_access": { "type": "object" },
        "checkpoint_policy": { "type": "object" },
        "persistence": { "type": "object" },
        "warnings": warnings_property()
    })
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
            "execution_duration_ms": { "type": "integer", "minimum": 0 },
            "session_age_ms": { "type": "integer", "minimum": 0 },
            "retained_ms": { "type": "integer", "minimum": 0 },
            "finished_at": { "type": ["string", "null"] },
            "result_observed": { "type": "boolean" },
            "durable": { "type": "boolean" },
            "process_bound": { "type": "boolean" },
            "execution_resources": { "type": ["object", "null"], "additionalProperties": true },
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
            "execution_duration_ms",
            "session_age_ms",
            "retained_ms",
            "finished_at",
            "result_observed",
            "durable",
            "process_bound",
            "output_refs",
        ],
    )
}

pub fn output_schema(name: &str) -> Value {
    match name {
        facade if is_facade_tool(facade) => success_output_schema(
            json!({
                "operation": {
                    "type": "string",
                    "enum": facade_operations(facade)
                        .into_iter()
                        .flatten()
                        .map(|(operation, _)| *operation)
                        .collect::<Vec<_>>()
                },
                "facade": { "type": "string", "const": facade },
                "status": { "type": "string" },
                "summary": { "type": ["string", "object", "null"] }
            }),
            &["operation", "facade"],
        ),
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
                    "detail": { "type": "string", "enum": ["compact", "full"] },
                    "full_detail_available": { "type": "boolean", "const": true },
                    "schema_telemetry": {
                        "type": "object",
                        "properties": {
                            "definition_bytes": { "type": "integer", "minimum": 0 },
                            "estimated_tokens": { "type": "integer", "minimum": 0 },
                            "largest_tools": { "type": "array", "maxItems": 8, "items": { "type": "object" } },
                            "largest_tools_limit": { "type": "integer", "const": 8 }
                        },
                        "required": ["definition_bytes", "estimated_tokens", "largest_tools", "largest_tools_limit"],
                        "additionalProperties": false
                    },
                    "response_bytes": { "type": "integer", "minimum": 1 }
                }),
                json!({
                    "preferred_shell": { "type": "string", "enum": ["auto", "pwsh", "powershell", "cmd"] },
                    "catalog_profile_guidance": {
                        "type": "object",
                        "properties": {
                            "recommended_profile": { "type": "string", "enum": ["core", "read-only", "advanced"] },
                            "reason": { "type": "string", "minLength": 1 },
                            "current_profile": { "type": "string", "enum": ["core", "read-only", "advanced"] }
                        },
                        "required": ["recommended_profile", "reason", "current_profile"],
                        "additionalProperties": false
                    }
                }),
                json!({
                    "schema_discovery": {
                        "type": "object",
                        "properties": {
                            "recommended_query": { "type": "string", "const": "anchor-core" },
                            "group_queries": { "type": "array", "items": { "type": "string" }, "minItems": 1 },
                            "strategy": { "type": "string", "minLength": 1 },
                            "host_followup_notice": { "type": "string", "minLength": 1 },
                            "catalog_publication": { "type": "object", "additionalProperties": true }
                        },
                        "required": ["recommended_query", "group_queries", "strategy", "host_followup_notice", "catalog_publication"],
                        "additionalProperties": false
                    },
                    "command_cost_policy": { "type": "object" },
                    "downstream_mcp": {
                        "type": "object",
                        "properties": {
                            "configured": { "type": "boolean" },
                            "server_count": { "type": "integer", "minimum": 0 },
                            "unavailable_server_count": { "type": "integer", "minimum": 0 },
                            "unavailable_servers": {
                                "type": "array",
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "name": { "type": "string", "minLength": 1 },
                                        "error": { "type": "string", "minLength": 1 }
                                    },
                                    "required": ["name", "error"],
                                    "additionalProperties": false
                                }
                            },
                            "servers": {
                                "type": "array",
                                "items": {
                                    "type": "object",
                                    "additionalProperties": true
                                }
                            }
                        },
                        "required": ["configured", "server_count", "servers", "unavailable_server_count", "unavailable_servers"],
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
                "preferred_shell",
                "default_cwd",
                "network_allowed",
                "tool_profile",
                "auth_enabled",
                "auth_type",
                "endpoint_path",
                "tool_count",
                "current_tool_count",
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
                "detail",
                "full_detail_available",
                "schema_telemetry",
                "response_bytes",
                "catalog_profile_guidance",
                "schema_discovery",
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
                "scope": { "type": "string", "enum": ["pending", "running", "history"] },
                "history_included": { "type": "boolean" },
                "retained_total_count": { "type": "integer", "minimum": 0 },
                "session_count": { "type": "integer", "minimum": 0 },
                "running_count": { "type": "integer", "minimum": 0 },
                "terminal_count": { "type": "integer", "minimum": 0 },
                "unobserved_terminal_count": { "type": "integer", "minimum": 0 },
                "pending_result_count": { "type": "integer", "minimum": 0 },
                "running_session_ids": { "type": "array", "items": { "type": "string" } },
                "unobserved_terminal_session_ids": { "type": "array", "items": { "type": "string" } },
                "requires_followup": { "type": "boolean" },
                "next_actions": { "type": "array", "items": { "type": "string" } },
                "process_bound": { "type": "boolean" },
                "process_bound_session_count": { "type": "integer", "minimum": 0 },
                "durable_session_count": { "type": "integer", "minimum": 0 },
                "durable_supervisor_available": { "type": "boolean" },
                "warnings": warnings_property()
            }),
            &[
                "sessions",
                "scope",
                "history_included",
                "retained_total_count",
                "session_count",
                "running_count",
                "terminal_count",
                "unobserved_terminal_count",
                "pending_result_count",
                "running_session_ids",
                "unobserved_terminal_session_ids",
                "requires_followup",
                "next_actions",
                "process_bound",
                "process_bound_session_count",
                "durable_session_count",
                "durable_supervisor_available",
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
        "list_files" => success_output_schema(
            json!({
                "path": { "type": "string" },
                "files": { "type": "array", "items": { "type": "object" } },
                "cursor": { "type": "integer", "minimum": 0 },
                "next_cursor": nullable_integer_property(),
                "total_files": { "type": "integer", "minimum": 0 },
                "truncated": { "type": "boolean" },
                "scan": { "type": "object" },
                "warnings": warnings_property()
            }),
            &[
                "path",
                "files",
                "cursor",
                "next_cursor",
                "total_files",
                "truncated",
                "scan",
                "warnings",
            ],
        ),
        "read_file" => success_output_schema(
            json!({
                "mode": { "type": "string", "enum": ["single", "batch"] },
                "path": { "type": "string" },
                "content": { "type": "string" },
                "encoding": { "type": "string", "enum": ["utf-8", "utf-16le", "utf-16be"] },
                "start_line": { "type": "integer", "minimum": 1 },
                "start_byte": { "type": "integer", "minimum": 0 },
                "end_line": { "type": "integer", "minimum": 0 },
                "total_lines": { "type": "integer", "minimum": 0 },
                "total_bytes": { "type": "integer", "minimum": 0 },
                "bytes_read": { "type": "integer", "minimum": 0 },
                "requested_files": { "type": "integer", "minimum": 0 },
                "failed_files": { "type": "integer", "minimum": 0 },
                "files": { "type": "array", "items": { "type": "object" } },
                "truncated": { "type": "boolean" },
                "truncated_by": { "type": ["string", "null"] },
                "next": { "type": ["object", "null"] },
                "warnings": warnings_property()
            }),
            &["mode", "bytes_read", "truncated", "next", "warnings"],
        ),
        "search" => success_output_schema(
            json!({
                "query": { "type": "string" },
                "requested_mode": { "type": "string", "enum": ["auto", "text", "symbol", "callers", "callees", "impact", "explore"] },
                "mode": { "type": "string", "enum": ["text", "symbol", "callers", "callees", "impact", "explore"] },
                "engine": { "type": "string" },
                "degraded": { "type": "boolean" },
                "degraded_reason": { "type": ["string", "null"] },
                "data": { "type": ["object", "array", "string", "number", "boolean", "null"] },
                "warnings": warnings_property()
            }),
            &[
                "query",
                "requested_mode",
                "mode",
                "engine",
                "degraded",
                "degraded_reason",
                "data",
                "warnings",
            ],
        ),
        "grep" | "search_text" => success_output_schema(
            json!({
                "query": { "type": "string" },
                "output_mode": { "type": "string", "enum": ["matches", "files", "count", "summary"] },
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
                "files": { "type": "array", "items": { "type": "string" } },
                "summary": { "type": "array", "items": { "type": "object" } },
                "total_matches": { "type": "integer", "minimum": 0 },
                "matched_files": { "type": "integer", "minimum": 0 },
                "cursor": { "type": "integer", "minimum": 0 },
                "next_cursor": nullable_integer_property(),
                "truncated": { "type": "boolean" },
                "scan": { "type": "object" },
                "warnings": warnings_property()
            }),
            &[
                "query",
                "output_mode",
                "matches",
                "files",
                "summary",
                "total_matches",
                "matched_files",
                "cursor",
                "next_cursor",
                "truncated",
                "scan",
                "warnings",
            ],
        ),
        "replace_text" => success_output_schema(
            json!({
                "dry_run": { "type": "boolean" },
                "mode": { "type": "string", "const": "literal" },
                "change_id": { "type": "string", "minLength": 1 },
                "files": { "type": "array", "items": { "type": "object" } },
                "files_modified": { "type": "array", "items": { "type": "string" } },
                "total_matches": { "type": "integer", "minimum": 1 },
                "bytes_processed": { "type": "integer", "minimum": 0 },
                "transaction": {
                    "type": "object",
                    "properties": {
                        "committed": { "type": "boolean" },
                        "atomic": { "type": "boolean", "const": true },
                        "cas_verified": { "type": "boolean" }
                    },
                    "required": ["committed", "atomic", "cas_verified"],
                    "additionalProperties": false
                },
                "recovery": { "type": "string", "minLength": 1 },
                "warnings": warnings_property()
            }),
            &[
                "dry_run",
                "mode",
                "files",
                "files_modified",
                "total_matches",
                "bytes_processed",
                "transaction",
                "warnings",
            ],
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
            merge_output_properties([
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
                "stderr_classification": { "type": "string", "enum": ["empty", "non_blocking_warning", "diagnostic", "error"] },
                "non_blocking_warnings": warnings_property(),
                "duration_ms": { "type": "integer", "minimum": 0 },
                "elapsed_ms": { "type": "integer", "minimum": 0 },
                "execution_mode": { "type": "string", "minLength": 1 },
                "resolved_executable": { "type": "string", "minLength": 1 },
                "argv": { "type": "array", "items": { "type": "string" } },
                "failure_stage": { "type": "string", "enum": ["pre_spawn", "spawn", "process", "test"] },
                "failure_classification": { "type": "string", "enum": ["infrastructure", "command", "test_failure"] },
                "test_outcome": { "type": "string", "enum": ["passed", "test_failure", "test_infrastructure_failure", "zero_tests_started", "device_unavailable"] },
                "toolchain": { "type": "object", "additionalProperties": true },
                "named_toolchains": { "type": "object", "additionalProperties": true },
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
                "command_ok": { "type": ["boolean", "null"] }
                }),
                json!({
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
                "verification_inconclusive": { "type": "boolean" },
                "verification_test_count": { "type": "integer", "minimum": 0 },
                "execution_resources": { "type": ["object", "null"], "additionalProperties": true },
                "warnings": warnings_property()
                }),
            ]),
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
                "command": { "type": "string", "minLength": 1 },
                "resolved_cwd": { "type": "string", "minLength": 1 },
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
                "execution_duration_ms": { "type": "integer", "minimum": 0 },
                "session_age_ms": { "type": "integer", "minimum": 0 },
                "retained_ms": { "type": "integer", "minimum": 0 },
                "finished_at": { "type": ["string", "null"] },
                "result_observed": { "type": "boolean" },
                "durable": { "type": "boolean" },
                "process_bound": { "type": "boolean" },
                "last_output_at": { "type": "string", "minLength": 1 },
                "stdin_open": { "type": "boolean" },
                "stdout": { "type": "object" },
                "stderr": { "type": "object" },
                "stdout_complete": { "type": "boolean" },
                "stderr_complete": { "type": "boolean" },
                "output_refs": { "type": "object" },
                "execution_resources": { "type": ["object", "null"], "additionalProperties": true },
                "stop_pattern_matched": { "type": ["string", "null"] },
                "wait_timeout_ms": { "type": "integer", "minimum": 0 },
                "affected_files": { "type": "array", "items": { "type": "object" } },
                "mutation_attributed": { "type": "boolean" },
                "warnings": warnings_property()
            }),
            &[
                "session_id",
                "command",
                "resolved_cwd",
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
                "execution_duration_ms",
                "session_age_ms",
                "retained_ms",
                "finished_at",
                "result_observed",
                "durable",
                "process_bound",
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
                "mutation_attributed": { "type": "boolean" },
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
        "git_merge" => success_output_schema(
            json!({
                "ref": { "type": "string" },
                "before_head": { "type": "string" },
                "target_head": { "type": "string" },
                "after_head": { "type": "string" },
                "fast_forwarded": { "type": "boolean" },
                "mutation_attributed": { "type": "boolean", "const": true },
                "warnings": warnings_property()
            }),
            &[
                "ref",
                "before_head",
                "target_head",
                "after_head",
                "fast_forwarded",
                "mutation_attributed",
                "warnings",
            ],
        ),
        "git_branch_create" => success_output_schema(
            json!({
                "branch": { "type": "string" },
                "start_point": { "type": "string" },
                "head": { "type": "string" },
                "mutation_attributed": { "type": "boolean", "const": true },
                "warnings": warnings_property()
            }),
            &[
                "branch",
                "start_point",
                "head",
                "mutation_attributed",
                "warnings",
            ],
        ),
        "git_branch_delete" => success_output_schema(
            json!({
                "branch": { "type": "string" },
                "deleted_head": { "type": "string" },
                "force": { "type": "boolean" },
                "mutation_attributed": { "type": "boolean", "const": true },
                "warnings": warnings_property()
            }),
            &[
                "branch",
                "deleted_head",
                "force",
                "mutation_attributed",
                "warnings",
            ],
        ),
        "git_is_ancestor" => success_output_schema(
            json!({
                "ancestor": { "type": "string" },
                "descendant": { "type": "string" },
                "ancestor_head": { "type": "string" },
                "descendant_head": { "type": "string" },
                "is_ancestor": { "type": "boolean" },
                "warnings": warnings_property()
            }),
            &[
                "ancestor",
                "descendant",
                "ancestor_head",
                "descendant_head",
                "is_ancestor",
                "warnings",
            ],
        ),
        "git_switch" => success_output_schema(
            json!({
                "target": { "type": "string" },
                "before_branch": { "type": "string" },
                "before_head": { "type": "string" },
                "target_head": { "type": "string" },
                "after_branch": { "type": "string" },
                "after_head": { "type": "string" },
                "mutation_attributed": { "type": "boolean", "const": true },
                "warnings": warnings_property()
            }),
            &[
                "target",
                "before_branch",
                "before_head",
                "target_head",
                "after_branch",
                "after_head",
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
        "session_open" => success_output_schema(
            session_open_output_properties(),
            &[
                "session_id",
                "session_path",
                "created",
                "resumed",
                "session_status",
                "previous_status",
                "reactivated",
                "parent_session_id",
                "continuation_created",
                "checkpoint_count",
                "automatic_history_loading",
                "history_injected",
                "archive_access",
                "checkpoint_policy",
                "persistence",
                "warnings",
            ],
        ),
        "session_checkpoint" => success_output_schema(
            json!({
                "session_id": { "type": "string", "pattern": "^ses_[0-9a-fA-F]{32}$" },
                "path": { "type": "string", "minLength": 1 },
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
                "session_id",
                "path",
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
        "session_list" => success_output_schema(
            json!({
                "sessions": { "type": "array", "items": { "type": "object" } },
                "cursor": { "type": "integer", "minimum": 0 },
                "next_cursor": { "type": ["integer", "null"], "minimum": 0 },
                "total": { "type": "integer", "minimum": 0 },
                "legacy_path": { "type": "string", "const": "docs/history-session" },
                "legacy_included": { "type": "boolean", "const": false }
            }),
            &[
                "sessions",
                "cursor",
                "next_cursor",
                "total",
                "legacy_path",
                "legacy_included",
            ],
        ),
        "session_get" => success_output_schema(
            json!({
                "session_id": { "type": "string", "pattern": "^ses_[0-9a-fA-F]{32}$" },
                "path": { "type": "string", "minLength": 1 },
                "title": { "type": "string" },
                "status": { "type": "string", "enum": ["active", "paused", "completed", "unknown"] },
                "created_at": { "type": "string" },
                "updated_at": { "type": "string" },
                "parent_session_id": { "type": ["string", "null"] },
                "checkpoint_count": { "type": "integer", "minimum": 0 },
                "snapshot": { "type": ["object", "null"] },
                "content": { "type": "string" },
                "content_truncated": { "type": "boolean" },
                "max_bytes": { "type": "integer", "minimum": 1 }
            }),
            &[
                "session_id",
                "path",
                "title",
                "status",
                "created_at",
                "updated_at",
                "parent_session_id",
                "checkpoint_count",
                "snapshot",
                "content",
                "content_truncated",
                "max_bytes",
            ],
        ),
        "session_validate" => success_output_schema(
            json!({
                "valid": { "type": "boolean" },
                "duplicate_session_ids": { "type": "array", "items": { "type": "string" } },
                "duplicate_host_session_keys": { "type": "array", "items": { "type": "string" } },
                "invalid_files": { "type": "array", "items": { "type": "string" } },
                "empty_files": { "type": "array", "items": { "type": "string" } },
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
                "total_session_bytes": { "type": "integer", "minimum": 0 },
                "largest_document_bytes": { "type": "integer", "minimum": 0 },
                "max_document_bytes": { "type": "integer", "minimum": 1 },
                "max_total_session_bytes": { "type": "integer", "minimum": 1 },
                "max_documents": { "type": "integer", "minimum": 1 },
                "index_status": { "type": "string", "minLength": 1 },
                "repaired": { "type": "boolean" },
                "legacy_path": { "type": "string", "const": "docs/history-session" },
                "legacy_scanned": { "type": "boolean", "const": false },
                "legacy_migration_performed": { "type": "boolean", "const": false },
                "warnings": warnings_property()
            }),
            &[
                "valid",
                "duplicate_session_ids",
                "duplicate_host_session_keys",
                "invalid_files",
                "empty_files",
                "document_count",
                "status_counts",
                "total_session_bytes",
                "largest_document_bytes",
                "max_document_bytes",
                "max_total_session_bytes",
                "max_documents",
                "index_status",
                "repaired",
                "legacy_path",
                "legacy_scanned",
                "legacy_migration_performed",
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
                "stale_active_task_ids": { "type": "array", "items": { "type": "string", "minLength": 1 } },
                "warnings": { "type": "array", "items": { "type": "string" } },
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
                "current_operation": { "type": ["object", "null"] },
                "running_command_count": { "type": "integer", "minimum": 0 },
                "pending_terminal_command_count": { "type": "integer", "minimum": 0 },
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
                "stale_active_task_ids",
                "warnings",
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
                "session": { "type": "object" },
                "session_state_transition": { "type": "object" },
                "state_scopes": { "type": "object" },
                "task": { "type": "object" },
                "harness": { "type": "object" },
                "reconnect_required": { "type": "boolean", "const": false }
            }),
            &[
                "work_session",
                "session",
                "session_state_transition",
                "state_scopes",
                "task",
                "harness",
                "reconnect_required",
            ],
        ),
        "close_work_session" | "complete_work_session" => json!({
            "type": "object",
            "properties": {
                "ok": { "type": "boolean" },
                "detail": { "type": "string", "enum": ["compact", "full"] },
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
                    "then": {
                        "required": ["error"],
                        "description": "Workflow failures may additionally expose closed/phase/finish/checkpoint/retryable. Direct validation or state-conflict failures use the standard structured tool error shape."
                    }
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
        "resolve_recovery" => success_output_schema(
            json!({
                "task_id": { "type": "string", "minLength": 1 },
                "recovery": { "type": "object" },
                "task": { "type": "object" }
            }),
            &["task_id", "recovery", "task"],
        ),
        "task_gate_status" => success_output_schema(
            json!({
                "task_id": { "type": "string", "minLength": 1 },
                "ready": { "type": "boolean" },
                "detail": { "type": "string", "enum": ["compact", "full"] },
                "completion_gate": { "type": "object" },
                "current_slice": { "type": ["object", "null"] },
                "blocking_failures": { "type": "array", "items": { "type": "object" } },
                "task": { "type": "object" },
                "verification": { "type": "array", "items": { "type": "object" } },
                "verification_summary": { "type": "object" }
            }),
            &[
                "task_id",
                "ready",
                "detail",
                "completion_gate",
                "verification_summary",
            ],
        ),
        "start_slice" | "update_slice" => success_output_schema(
            json!({
                "task_id": { "type": "string", "minLength": 1 },
                "slice": { "type": ["object", "null"] },
                "task": { "type": "object" },
                "progress_event": { "type": "object" }
            }),
            &["task_id", "slice", "task", "progress_event"],
        ),
        "complete_slice" => json!({
            "type": "object",
            "properties": {
                "ok": { "type": "boolean" },
                "completed": { "type": "boolean" },
                "task_id": { "type": "string", "minLength": 1 },
                "slice_id": { "type": "string", "minLength": 1 },
                "slice": { "type": ["object", "null"] },
                "acceptance": { "type": "array", "items": { "type": "object" } },
                "missing": { "type": "array", "items": { "type": "object" } },
                "next_actions": { "type": "array", "items": { "type": "string" } },
                "task": { "type": "object" },
                "progress_event": { "type": "object" },
                "error": error_output_schema()
            },
            "required": ["ok", "completed", "task_id", "slice_id", "task"],
            "allOf": [
                {
                    "if": { "properties": { "ok": { "const": true } }, "required": ["ok"] },
                    "then": { "properties": { "completed": { "const": true } }, "required": ["slice", "acceptance", "progress_event"] }
                },
                {
                    "if": { "properties": { "ok": { "const": false } }, "required": ["ok"] },
                    "then": { "properties": { "completed": { "const": false } }, "required": ["missing", "acceptance", "next_actions", "error"] }
                }
            ],
            "additionalProperties": true
        }),
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
        "abort_task" => json!({
            "type": "object",
            "properties": {
                "ok": { "type": "boolean" },
                "task_status": { "type": "string", "enum": ["active", "paused", "verifying", "failed", "incomplete"] },
                "outcome": { "type": "string", "const": "incomplete" },
                "closed": { "type": "boolean" },
                "session_status": { "type": "string", "enum": ["active", "paused"] },
                "requested_session_status": { "type": "string", "enum": ["active", "paused"] },
                "reason": { "type": "string" },
                "running_sessions": { "type": "array", "items": { "type": "object" } },
                "unobserved_terminal_sessions": { "type": "array", "items": { "type": "object" } },
                "completion_gate": { "type": "object" },
                "task": { "type": "object" },
                "worktree_cleanup": { "type": "object" },
                "error": error_output_schema()
            },
            "required": ["ok", "task_status", "outcome", "closed", "session_status", "requested_session_status", "reason", "completion_gate", "task"],
            "allOf": [
                {
                    "if": { "properties": { "ok": { "const": true } }, "required": ["ok"] },
                    "then": {
                        "properties": { "task_status": { "const": "incomplete" }, "closed": { "const": true } },
                        "required": ["worktree_cleanup"]
                    }
                },
                {
                    "if": { "properties": { "ok": { "const": false } }, "required": ["ok"] },
                    "then": {
                        "properties": { "closed": { "const": false } },
                        "required": ["running_sessions", "unobserved_terminal_sessions", "error"]
                    }
                }
            ],
            "additionalProperties": true
        }),
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
                "completion_gate": { "type": "object" },
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

fn profile_tool_names(tool_profile: &str) -> Vec<&'static str> {
    match require_tool_profile(tool_profile).expect("tool profile must be validated") {
        "read-only" => CORE_READ_ONLY_TOOLS.to_vec(),
        "advanced" => {
            let mut tools = CORE_TOOLS.to_vec();
            for tool in ADVANCED_EXTRA_TOOLS {
                if !tools.contains(tool) {
                    tools.push(tool);
                }
            }
            tools
        }
        _ => CORE_TOOLS.to_vec(),
    }
}

pub fn exposed_tool_names(tool_profile: &str) -> Vec<&'static str> {
    profile_tool_names(tool_profile)
        .into_iter()
        .filter(|name| !is_facade_operation_tool(name))
        .collect()
}

pub fn list_tools() -> Vec<Value> {
    list_tools_for_profile("advanced")
}

pub fn list_tools_for_profile(tool_profile: &str) -> Vec<Value> {
    exposed_tool_names(tool_profile)
        .into_iter()
        .filter_map(|name| {
            P0_TOOLS.iter().find(|(n, ..)| *n == name).map(|entry| {
                let (name, title, description, mut read_only, mut destructive, mut open_world) =
                    *entry;
                let input_schema = if is_facade_tool(name) {
                    let operations = facade_operations_for_profile(name, tool_profile);
                    let annotations = facade_annotations_for_profile(name, tool_profile);
                    read_only = annotations.0;
                    destructive = annotations.1;
                    open_world = annotations.2;
                    facade_input_schema(name, &operations)
                } else {
                    input_schema(name)
                };
                let mut definition = json!({
                    "name": name,
                    "title": title,
                    "description": description,
                    "inputSchema": input_schema,
                    "annotations": {
                        "title": title,
                        "readOnlyHint": read_only,
                        "destructiveHint": destructive,
                        "idempotentHint": read_only,
                        "openWorldHint": open_world
                    }
                });
                if name == "view_image" {
                    definition["_meta"] =
                        crate::mcp::ui::image_viewer_tool_meta("Loading image…", "Image ready");
                }
                definition
            })
        })
        .collect()
}

fn facade_annotations_for_profile(facade: &str, tool_profile: &str) -> (bool, bool, bool) {
    let operations = facade_operations_for_profile(facade, tool_profile);
    let entries = operations
        .iter()
        .filter_map(|operation| facade_tool_for_operation(facade, operation))
        .filter_map(|tool| P0_TOOLS.iter().find(|(name, ..)| *name == tool))
        .collect::<Vec<_>>();
    let read_only = !entries.is_empty() && entries.iter().all(|(_, _, _, value, _, _)| *value);
    let destructive = entries.iter().any(|(_, _, _, _, value, _)| *value);
    let open_world = entries.iter().any(|(_, _, _, _, _, value)| *value);
    (read_only, destructive, open_world)
}

fn normalized_facade_property_schema(schema: &Value) -> Value {
    let mut normalized = schema.clone();
    if let Some(object) = normalized.as_object_mut() {
        object.remove("default");
        object.remove("description");
    }
    normalized
}

fn published_facade_operation_schema(facade: &str, operation: &str, tool: &str) -> Value {
    let mut schema = input_schema(tool);
    if facade == "task" && operation == "update" {
        if let Some(properties) = schema.get_mut("properties").and_then(Value::as_object_mut) {
            // Task plan shape is established by begin_work_session. Runtime
            // updates stay focused on progress/phase/working-set changes; Slice
            // mutations use the dedicated slice facade.
            properties.remove("contract");
            properties.remove("slices");
        }
    }
    schema
}

fn facade_input_schema(facade: &str, operations: &[&str]) -> Value {
    let mut properties = serde_json::Map::new();
    let mut property_operations = BTreeMap::<String, Vec<String>>::new();
    let mut contracts = Vec::new();
    for operation in operations {
        let Some(tool) = facade_tool_for_operation(facade, operation) else {
            continue;
        };
        let schema = published_facade_operation_schema(facade, operation, tool);
        let required = schema
            .get("required")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>();
        if !required.is_empty() {
            contracts.push(format!("{operation}({})", required.join(",")));
        }
        if let Some(operation_properties) = schema.get("properties").and_then(Value::as_object) {
            for (property, property_schema) in operation_properties {
                if property == "operation" {
                    continue;
                }
                property_operations
                    .entry(property.clone())
                    .or_default()
                    .push((*operation).to_string());
                let candidate = normalized_facade_property_schema(property_schema);
                match properties.get(property) {
                    None => {
                        properties.insert(property.clone(), candidate);
                    }
                    Some(existing) if existing == &candidate => {}
                    Some(_) => {
                        // A shared property name can have operation-specific bounds or shapes.
                        // Keep the facade permissive here; the delegated canonical schema below
                        // remains authoritative and rejects invalid operation-specific arguments.
                        properties.insert(property.clone(), json!({}));
                    }
                }
            }
        }
    }

    for (property, valid_operations) in property_operations {
        let Some(property_schema) = properties.get_mut(&property).and_then(Value::as_object_mut)
        else {
            continue;
        };
        property_schema.insert(
            "description".into(),
            Value::String(format!("Only for: {}", valid_operations.join(","))),
        );
    }

    properties.insert(
        "operation".into(),
        json!({
            "type": "string",
            "enum": operations,
            "description": if contracts.is_empty() {
                format!("Select {facade} operation.")
            } else {
                format!("Select {facade} operation. Required: {}", contracts.join(";"))
            }
        }),
    );
    json!({
        "type": "object",
        "properties": properties,
        "required": ["operation"],
        "additionalProperties": false
    })
}

pub fn input_schema(name: &str) -> Value {
    match name {
        facade if is_facade_tool(facade) => {
            let operations = facade_operations(facade)
                .into_iter()
                .flatten()
                .map(|(operation, _)| *operation)
                .collect::<Vec<_>>();
            facade_input_schema(facade, &operations)
        }
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
                "session_checkpoint": {
                    "type": "object",
                    "required": ["session_id", "expected_path"],
                    "properties": {
                        "session_id": { "type": "string", "pattern": "^ses_[0-9a-fA-F]{32}$" },
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
                "session_checkpoint": {
                    "type": "object",
                    "required": ["session_id", "expected_path"],
                    "properties": {
                        "session_id": { "type": "string", "pattern": "^ses_[0-9a-fA-F]{32}$" },
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
        "session_open" => json!({
            "type": "object",
            "properties": {
                "workspace_root": { "type": "string", "minLength": 1 },
                "session_id": { "type": "string", "pattern": "^ses_[0-9a-fA-F]{32}$" },
                "title": { "type": "string", "maxLength": 200 },
                "session_dir": { "type": "string", "default": "docs/session" },
                "create_if_missing": { "type": "boolean", "default": true },
                "resume_completed": { "type": "boolean", "default": false, "description": "Explicitly reactivate the selected completed Session instead of creating a continuation Session." }
            },
            "additionalProperties": false
        }),
        "session_checkpoint" => json!({
            "type": "object",
            "required": ["session_id", "expected_path"],
            "properties": {
                "workspace_root": { "type": "string", "minLength": 1 },
                "session_id": { "type": "string", "pattern": "^ses_[0-9a-fA-F]{32}$" },
                "expected_path": { "type": "string", "minLength": 1, "maxLength": 1024 },
                "session_dir": { "type": "string", "default": "docs/session" },
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
        "session_list" => json!({
            "type": "object",
            "properties": {
                "workspace_root": { "type": "string", "minLength": 1 },
                "session_dir": { "type": "string", "default": "docs/session" },
                "cursor": { "type": "integer", "minimum": 0, "default": 0 },
                "limit": { "type": "integer", "minimum": 1, "maximum": 100, "default": 20 }
            },
            "additionalProperties": false
        }),
        "session_get" => json!({
            "type": "object",
            "required": ["session_id"],
            "properties": {
                "workspace_root": { "type": "string", "minLength": 1 },
                "session_dir": { "type": "string", "default": "docs/session" },
                "session_id": { "type": "string", "pattern": "^ses_[0-9a-fA-F]{32}$" },
                "max_bytes": { "type": "integer", "minimum": 1, "maximum": 262144, "default": 65536 }
            },
            "additionalProperties": false
        }),
        "session_validate" => json!({
            "type": "object",
            "properties": {
                "workspace_root": { "type": "string", "minLength": 1 },
                "session_dir": { "type": "string", "default": "docs/session" },
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
                "session_id": { "type": "string", "minLength": 1 },
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
            "properties": merge_schema_properties(vec![json!({
                "objective": { "type": "string", "minLength": 1, "maxLength": 4000 },
                "completed_steps": { "type": "array", "maxItems": 256, "items": { "type": "string", "maxLength": 2000 } },
                "pending_steps": { "type": "array", "maxItems": 256, "items": { "type": "string", "maxLength": 2000 } },
                "session_id": { "type": "string", "pattern": "^ses_[0-9a-fA-F]{32}$" },
                "title": { "type": "string", "maxLength": 200 },
                "create_if_missing": { "type": "boolean", "default": true, "description": "When false, recover an already-existing Session + writable Harness Task only. Never create a replacement Task or Git worktree; return a structured not-found/conflict error instead." },
                "resume_completed": { "type": "boolean", "default": false, "description": "Explicitly reactivate the selected completed Session instead of creating a continuation Session." },
                "task_id": { "type": "string", "minLength": 1, "description": "Explicit durable Harness Task to attach to instead of selecting by the current Session binding." },
                "objective_revision": { "type": "boolean", "default": false, "description": "When reusing a writable durable Task with a different objective, explicitly revise that Task objective in place and preserve the prior objective in Harness event history instead of creating/requiring a replacement Task." },
                "reclaim_session": { "type": "boolean", "default": false, "description": "Explicitly transfer a durable Task from its previous Session lease to the opened Session. Reclaim is rejected while the Task owns running or unconsumed command sessions." },
                "expected_head": { "type": "string", "minLength": 1, "maxLength": 128, "description": "Caller-observed Git HEAD required when reclaim_session=true; must match the durable Task expected HEAD." },
                "workspace_mode": { "type": "string", "enum": ["shared", "worktree"], "default": "shared", "description": "Use the configured workspace by default, or use an isolated Anchor-managed Git worktree for a new task." },
                "worktree_path": { "type": "string", "minLength": 1, "maxLength": 2000, "description": "Bind the new task to an existing registered Anchor-managed worktree under .anchor/worktrees. Valid only with workspace_mode=worktree and mutually exclusive with worktree_branch/worktree_base_ref." },
                "worktree_branch": { "type": "string", "minLength": 1, "maxLength": 255 },
                "worktree_base_ref": { "type": "string", "minLength": 1, "maxLength": 255, "default": "HEAD" },
                "worktree_remove_on_close": { "type": "boolean", "default": false },
                "session_dir": { "type": "string", "default": "docs/session" },
                "workspace_root": { "type": "string", "minLength": 1 }
            }), task_configuration_input_properties()]),
            "required": ["objective"],
            "additionalProperties": false
        }),
        "close_work_session" => json!({
            "type": "object",
            "properties": {
                "task_id": { "type": "string", "minLength": 1 },
                "summary": { "type": "string", "maxLength": 8000 },
                "outcome": {
                    "type": "string",
                    "enum": ["completed", "incomplete"],
                    "default": "completed",
                    "description": "Use incomplete only after an explicit user/operator stop decision. It closes the task without satisfying completion gates."
                },
                "reason": { "type": "string", "minLength": 1, "maxLength": 2000 },
                "allow_unverified": { "type": "boolean", "default": false },
                "session_status": { "type": "string", "enum": ["active", "paused", "completed"], "default": "paused" },
                "checkpoint": { "type": "object", "additionalProperties": true }
            },
            "required": ["task_id"],
            "allOf": [
                {
                    "if": {
                        "properties": { "outcome": { "const": "incomplete" } },
                        "required": ["outcome"]
                    },
                    "then": {
                        "properties": {
                            "session_status": { "type": "string", "enum": ["active", "paused"] }
                        },
                        "required": ["reason"]
                    }
                }
            ],
            "additionalProperties": false
        }),
        "complete_work_session" => json!({
            "type": "object",
            "properties": {
                "task_id": { "type": "string", "minLength": 1 },
                "summary": { "type": "string", "maxLength": 8000 },
                "checkpoint": { "type": "object", "additionalProperties": true },
                "detail": { "type": "string", "enum": ["compact", "full"], "default": "compact" }
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
        "resolve_recovery" => json!({
            "type": "object",
            "properties": {
                "task_id": { "type": "string", "minLength": 1 },
                "recovery_id": { "type": "string", "minLength": 1 },
                "reason": { "type": "string", "minLength": 1, "maxLength": 2000 },
                "evidence": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": 32,
                    "items": { "type": "string", "minLength": 1, "maxLength": 2000 }
                }
            },
            "required": ["task_id", "recovery_id", "reason", "evidence"],
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
            "properties": merge_schema_properties(vec![json!({
                "objective": { "type": "string", "minLength": 1 },
                "completed_steps": { "type": "array", "maxItems": 256, "items": { "type": "string", "maxLength": 2000 } },
                "pending_steps": { "type": "array", "maxItems": 256, "items": { "type": "string", "maxLength": 2000 } },
                "workspace_mode": { "type": "string", "enum": ["shared", "worktree"], "default": "shared" },
                "worktree_path": { "type": "string", "minLength": 1, "maxLength": 2000, "description": "Bind the new task to an existing registered Anchor-managed worktree under .anchor/worktrees. Valid only with workspace_mode=worktree and mutually exclusive with worktree_branch/worktree_base_ref." },
                "worktree_branch": { "type": "string", "minLength": 1, "maxLength": 255 },
                "worktree_base_ref": { "type": "string", "minLength": 1, "maxLength": 255, "default": "HEAD" },
                "worktree_remove_on_close": { "type": "boolean", "default": false }
            }), task_configuration_input_properties()]),
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
            "properties": merge_schema_properties(vec![json!({
                "task_id": { "type": "string", "minLength": 1 },
                "objective": { "type": "string", "minLength": 1, "maxLength": 4000, "description": "Explicitly revise the current objective in place. The previous objective is preserved in Harness event history." },
                "completed_steps": { "type": "array", "items": { "type": "string" } },
                "pending_steps": { "type": "array", "items": { "type": "string" } }
            }), task_configuration_input_properties()]),
            "required": ["task_id"],
            "additionalProperties": false
        }),
        "task_gate_status" => json!({
            "type": "object",
            "properties": {
                "task_id": { "type": "string", "minLength": 1 },
                "detail": { "type": "string", "enum": ["compact", "full"], "default": "compact" }
            },
            "additionalProperties": false
        }),
        "start_slice" => json!({
            "type": "object",
            "properties": {
                "task_id": { "type": "string", "minLength": 1 },
                "slice_id": { "type": "string", "minLength": 1, "maxLength": 128 },
                "title": { "type": "string", "minLength": 1, "maxLength": 500 },
                "files": { "type": "array", "maxItems": 256, "items": { "type": "string", "minLength": 1, "maxLength": 2000 } },
                "acceptance_checks": { "type": "array", "maxItems": 64, "items": verification_requirement_input_schema() }
            },
            "required": ["task_id", "slice_id", "title"],
            "additionalProperties": false
        }),
        "update_slice" => json!({
            "type": "object",
            "properties": {
                "task_id": { "type": "string", "minLength": 1 },
                "slice_id": { "type": "string", "minLength": 1, "maxLength": 128 },
                "status": { "type": "string", "enum": ["planned", "in_progress", "verifying", "blocked", "paused"] },
                "title": { "type": "string", "minLength": 1, "maxLength": 500 },
                "files": { "type": "array", "maxItems": 256, "items": { "type": "string", "minLength": 1, "maxLength": 2000 } },
                "acceptance_checks": { "type": "array", "maxItems": 64, "items": verification_requirement_input_schema() },
                "commit_sha": { "type": ["string", "null"], "maxLength": 128 },
                "blocker": { "type": ["string", "null"], "maxLength": 2000 }
            },
            "required": ["task_id", "slice_id"],
            "additionalProperties": false
        }),
        "complete_slice" => json!({
            "type": "object",
            "properties": {
                "task_id": { "type": "string", "minLength": 1 },
                "slice_id": { "type": "string", "minLength": 1, "maxLength": 128 },
                "commit_sha": { "type": "string", "minLength": 1, "maxLength": 128 }
            },
            "required": ["task_id", "slice_id"],
            "additionalProperties": false
        }),
        "pause_task" | "resume_task" | "switch_task" => json!({
            "type": "object",
            "properties": { "task_id": { "type": "string", "minLength": 1 } },
            "required": ["task_id"],
            "additionalProperties": false
        }),
        "abort_task" => json!({
            "type": "object",
            "properties": {
                "task_id": { "type": "string", "minLength": 1 },
                "reason": { "type": "string", "minLength": 1, "maxLength": 2000 },
                "session_status": {
                    "type": "string",
                    "enum": ["active", "paused"],
                    "default": "paused",
                    "description": "Requested workspace session state after the incomplete task is terminated. Active peer tasks can keep the actual workspace session active."
                }
            },
            "required": ["task_id", "reason"],
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
                "limit": { "type": "integer", "minimum": 1, "maximum": 200, "default": 50 },
                "detail": { "type": "string", "enum": ["compact", "full"], "default": "compact" }
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
                "start_byte": { "type": "integer", "minimum": 0, "default": 0, "description": "Decoded UTF-8 byte offset within start_line. Normally use only the value returned by next for exact continuation." },
                "files": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": 32,
                    "description": "Batch read requests sharing max_bytes. Mutually exclusive with path.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "path": { "type": "string", "minLength": 1 },
                            "start_line": { "type": "integer", "minimum": 1, "default": 1 },
                            "end_line": { "type": "integer", "minimum": 1 },
                            "start_byte": { "type": "integer", "minimum": 0, "default": 0 }
                        },
                        "required": ["path"],
                        "additionalProperties": false
                    }
                },
                "max_bytes": { "type": "integer", "minimum": 1, "maximum": 1048576, "default": 131072 }
            },
            "allOf": [
                {
                    "if": { "required": ["path"] },
                    "then": { "not": { "required": ["files"] } },
                    "else": { "required": ["files"] }
                }
            ],
            "additionalProperties": false
        }),
        "list_dir" => json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "default": ".", "description": "Text-search scope. In auto mode, a non-default path deterministically selects the text backend." },
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
                "cursor": { "type": "integer", "minimum": 0, "default": 0, "description": "Stable result offset returned as next_cursor." },
                "max_scan_files": { "type": "integer", "minimum": 1, "maximum": 250000, "default": 100000, "description": "Hard candidate-file budget. The scan fails atomically instead of returning an unstable partial page when exceeded." },
                "scan_timeout_ms": { "type": "integer", "minimum": 1000, "maximum": 120000, "default": 30000, "description": "Cooperative repository traversal deadline." },
                "max_results": { "type": "integer", "minimum": 1, "maximum": 50000, "default": 5000 }
            },
            "additionalProperties": false
        }),
        "search" => json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "minLength": 1 },
                "mode": {
                    "type": "string",
                    "enum": ["auto", "text", "symbol", "callers", "callees", "impact", "explore"],
                    "default": "auto",
                    "description": "Deterministic search intent. auto uses text controls, whitespace, and identifier shape; it does not invoke an LLM classifier."
                },
                "path": { "type": "string", "default": "." },
                "include_globs": { "type": "array", "items": { "type": "string" } },
                "exclude_globs": { "type": "array", "items": { "type": "string" } },
                "regex": { "type": "boolean", "default": false },
                "case_sensitive": { "type": "boolean", "default": false },
                "context_lines": { "type": "integer", "minimum": 0, "maximum": 20, "default": 0 },
                "output_mode": { "type": "string", "enum": ["matches", "files", "count", "summary"], "default": "matches", "description": "Text-backend output shape." },
                "cursor": { "type": "integer", "minimum": 0, "default": 0 },
                "max_scan_files": { "type": "integer", "minimum": 1, "maximum": 250000, "default": 100000 },
                "max_scan_bytes": { "type": "integer", "minimum": 1, "maximum": 1073741824, "default": 134217728 },
                "scan_timeout_ms": { "type": "integer", "minimum": 1000, "maximum": 120000, "default": 30000 },
                "max_preview_bytes": { "type": "integer", "minimum": 64, "maximum": 4096, "default": 512 },
                "max_results": { "type": "integer", "minimum": 1, "maximum": 10000, "default": 1000 },
                "graph_depth": { "type": "integer", "minimum": 1, "maximum": 10, "default": 3 },
                "graph_timeout_ms": { "type": "integer", "minimum": 1000, "maximum": 120000, "default": 60000 }
            },
            "required": ["query"],
            "additionalProperties": false
        }),
        "grep" | "search_text" => json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "minLength": 1 },
                "path": { "type": "string", "default": "." },
                "include_globs": { "type": "array", "items": { "type": "string" } },
                "exclude_globs": { "type": "array", "items": { "type": "string" } },
                "regex": { "type": "boolean", "default": false },
                "case_sensitive": { "type": "boolean", "default": false },
                "context_lines": { "type": "integer", "minimum": 0, "maximum": 20, "default": 0 },
                "output_mode": { "type": "string", "enum": ["matches", "files", "count", "summary"], "default": "matches", "description": "Choose detailed matches or compact file/count/summary output." },
                "cursor": { "type": "integer", "minimum": 0, "default": 0, "description": "Stable offset in the selected output mode; continue with next_cursor." },
                "max_scan_files": { "type": "integer", "minimum": 1, "maximum": 250000, "default": 100000, "description": "Hard candidate-file budget. Exceeding it returns a retryable budget error rather than unstable partial results." },
                "max_scan_bytes": { "type": "integer", "minimum": 1, "maximum": 1073741824, "default": 134217728, "description": "Hard aggregate source-byte budget for this search." },
                "scan_timeout_ms": { "type": "integer", "minimum": 1000, "maximum": 120000, "default": 30000, "description": "Cooperative repository traversal and search deadline." },
                "max_preview_bytes": { "type": "integer", "minimum": 64, "maximum": 4096, "default": 512 },
                "max_results": { "type": "integer", "minimum": 1, "maximum": 10000, "default": 1000 }
            },
            "required": ["query"],
            "additionalProperties": false
        }),
        "replace_text" => json!({
            "type": "object",
            "properties": {
                "files": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": 64,
                    "items": {
                        "type": "object",
                        "properties": {
                            "path": { "type": "string", "minLength": 1 },
                            "expected_sha256": { "type": "string", "pattern": "^[0-9A-Fa-f]{64}$" },
                            "expected_matches": { "type": "integer", "minimum": 0, "maximum": 100000 }
                        },
                        "required": ["path"],
                        "additionalProperties": false
                    }
                },
                "find": { "type": "string", "minLength": 1 },
                "replace": { "type": "string" },
                "dry_run": { "type": "boolean", "default": false },
                "max_matches": { "type": "integer", "minimum": 1, "maximum": 100000, "default": 10000 },
                "max_total_bytes": { "type": "integer", "minimum": 1, "maximum": 268435456, "default": 67108864 }
            },
            "required": ["files", "find", "replace"],
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
                "recovery_key": { "type": "string", "minLength": 1, "maxLength": 256, "description": "Stable identity for retrying the same logical Patch after correcting its content." },
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
        "check_exec_environment" => json!({
            "type": "object",
            "properties": {
                "detail": {
                    "type": "string",
                    "enum": ["compact", "full"],
                    "default": "compact",
                    "description": "Compact returns the actionable environment summary; full includes complete probes and dependency diagnostics."
                }
            },
            "additionalProperties": false
        }),
        "server_info" => json!({
            "type": "object",
            "properties": {
                "detail": {
                    "type": "string",
                    "enum": ["compact", "full"],
                    "default": "compact",
                    "description": "Compact omits repeated tool-name/group manifests while retaining counts, digests, connection state, and schema telemetry; full returns the complete manifest."
                }
            },
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
                "toolchain_paths": {
                    "type": "array",
                    "maxItems": 16,
                    "items": { "type": "string", "minLength": 1, "maxLength": 1024 },
                    "description": "Workspace-relative toolchain directories to prepend to this child process PATH and use when resolving the requested executable. Parent traversal and paths resolving outside the workspace are rejected."
                },
                "toolchains": {
                    "type": "object",
                    "maxProperties": 4,
                    "properties": {
                        "java": { "type": "string", "minLength": 1, "maxLength": 64, "description": "Trusted JDK selector. Use default or a version prefix such as 17 or 21. External paths are never accepted." },
                        "node": { "type": "string", "minLength": 1, "maxLength": 64, "description": "Trusted Node.js selector. Use default or a version prefix such as 20 or 24. External paths are never accepted." },
                        "flutter": { "type": "string", "minLength": 1, "maxLength": 64, "description": "Trusted Flutter SDK selector. Use default or a discovered version prefix. External paths are never accepted." },
                        "android_sdk": { "type": "string", "enum": ["default"], "description": "Select the trusted active/default Android SDK without passing ANDROID_HOME, ANDROID_SDK_ROOT, or external paths." }
                    },
                    "additionalProperties": false,
                    "description": "Named trusted host runtimes discovered by Anchor. These selectors do not bypass command allowlists or workspace policy and cannot contain filesystem paths."
                },
                "shell": { "type": "string", "enum": ["direct", "pwsh", "powershell", "cmd"], "description": "Optional explicit Windows shell. Use direct with executable for shell-free execution. Do not launch the same shell family again inside args; redundant PowerShell/cmd nesting is rejected before spawn." },
                "workdir": { "type": "string", "default": ".", "description": "Working directory relative to the current session cwd. '.' means the session cwd. Paths already prefixed by the current cwd are de-duplicated instead of being joined twice." },
                "timeout_ms": { "type": "integer", "minimum": 1, "maximum": 3600000, "default": 30000 },
                "max_output_bytes": { "type": "integer", "minimum": 1024, "maximum": 1048576, "default": 32768 },
                "yield_time_ms": { "type": "integer", "minimum": 0, "maximum": 30000, "default": 1000 },
                "tty": { "type": "boolean", "default": false },
                "durable": {
                    "type": "boolean",
                    "default": false,
                    "description": "Opt in to the detached non-TTY supervisor so wait/read/kill can recover after MCP daemon restart."
                },
                "stdin": { "type": "string", "default": "" },
                "expected_exit_codes": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": 32,
                    "items": { "type": "integer" },
                    "default": [0],
                    "description": "Exit codes that should be treated as successful command completion. Transport, spawn, timeout, cancellation, and kill failures remain failures."
                },
                "allowed_exit_codes": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": 32,
                    "items": { "type": "integer" },
                    "description": "Alias for expected_exit_codes. Do not provide both fields with different values."
                },
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
                "recovery_key": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": 256,
                    "description": "Stable identity for retrying the same logical command after correcting arguments or environment."
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
                "include_history": { "type": "boolean", "default": false, "description": "Include consumed terminal command-session history. By default only running and terminal-unconsumed sessions are returned." },
                "include_terminal": { "type": "boolean", "description": "Compatibility alias: true includes terminal history; false requests running sessions only. Prefer include_history for new callers." },
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
        "git_merge" => json!({
            "type": "object",
            "properties": {
                "ref": { "type": "string", "minLength": 1, "maxLength": 255 }
            },
            "required": ["ref"],
            "additionalProperties": false
        }),
        "git_branch_create" => json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "minLength": 1, "maxLength": 255 },
                "start_point": { "type": "string", "minLength": 1, "maxLength": 255, "default": "HEAD" }
            },
            "required": ["name"],
            "additionalProperties": false
        }),
        "git_branch_delete" => json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "minLength": 1, "maxLength": 255 },
                "force": { "type": "boolean", "default": false }
            },
            "required": ["name"],
            "additionalProperties": false
        }),
        "git_is_ancestor" => json!({
            "type": "object",
            "properties": {
                "ancestor": { "type": "string", "minLength": 1, "maxLength": 255 },
                "descendant": { "type": "string", "minLength": 1, "maxLength": 255, "default": "HEAD" }
            },
            "required": ["ancestor"],
            "additionalProperties": false
        }),
        "git_switch" => json!({
            "type": "object",
            "properties": {
                "target": { "type": "string", "minLength": 1, "maxLength": 255 }
            },
            "required": ["target"],
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

    use super::{
        exposed_tool_names, input_schema, is_facade_operation_tool, list_tools_for_profile,
        output_schema, require_tool_profile,
    };

    #[test]
    fn core_catalog_publishes_facades_without_internal_operation_handlers() {
        let tools = list_tools_for_profile("core");
        let names: Vec<_> = tools
            .iter()
            .map(|tool| tool["name"].as_str().expect("tool name"))
            .collect();
        let unique: HashSet<_> = names.iter().copied().collect();

        assert_eq!(tools.len(), 25);
        assert_eq!(unique.len(), tools.len());
        assert!(names.contains(&"git"));
        assert!(names.contains(&"task"));
        assert!(names.contains(&"skill"));
        assert!(names.contains(&"environment"));
        assert!(names.contains(&"cwd"));
        assert!(names.contains(&"session"));
        assert!(!names
            .iter()
            .any(|name| name.starts_with("history_session_")));
        assert!(names.contains(&"begin_work_session"));
        assert!(names.contains(&"close_work_session"));
        assert!(names.contains(&"wait_command"));
        assert!(names.contains(&"list_command_sessions"));
        assert!(names.contains(&"browser_build_info"));
        assert!(names.contains(&"browser_wait_for_build"));
        assert!(names.contains(&"search"));
        assert!(!names.contains(&"grep"));
        assert!(!names.contains(&"search_text"));
        assert!(names.contains(&"replace_text"));
        assert!(!names.contains(&"command_cost_explain"));
        assert!(!names.contains(&"check_exec_environment"));
        assert!(!names.contains(&"get_default_cwd"));
        assert!(!names.contains(&"set_default_cwd"));
        assert!(names.contains(&"remove_path"));
        assert!(!names.contains(&"request_permissions"));
        assert!(names.iter().all(|name| !is_facade_operation_tool(name)));

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
    fn legacy_text_search_schemas_remain_available_without_public_exposure() {
        assert_eq!(input_schema("search_text"), input_schema("grep"));
        assert_eq!(output_schema("search_text"), output_schema("grep"));
        assert!(!exposed_tool_names("core").contains(&"search_text"));
        assert!(!exposed_tool_names("core").contains(&"grep"));
        assert!(exposed_tool_names("core").contains(&"search"));
    }

    #[test]
    fn unified_search_schema_is_distinct_from_legacy_text_contract() {
        let search_input = input_schema("search");
        let search_output = output_schema("search");

        assert_ne!(search_input, input_schema("grep"));
        assert!(search_input["properties"]["mode"]
            .get("enum")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|modes| modes.iter().any(|mode| mode == "impact")));
        assert!(search_output["properties"]["engine"].is_object());
        assert!(search_output["properties"]["degraded"].is_object());
        assert!(exposed_tool_names("core").contains(&"search"));
    }

    #[test]
    fn lazy_schema_tags_expose_a_bounded_core_workflow_bundle() {
        let tools =
            crate::tools::catalog::build_effective_catalog_from_parts("advanced", true, Vec::new())
                .expect("effective advanced catalog")
                .tools;
        let core = tools
            .iter()
            .filter(|tool| {
                tool["description"]
                    .as_str()
                    .is_some_and(|description| description.contains("anchor-core"))
            })
            .filter_map(|tool| tool["name"].as_str())
            .collect::<HashSet<_>>();
        for required in [
            "server_info",
            "session",
            "begin_work_session",
            "close_work_session",
            "read_file",
            "search",
            "replace_text",
            "apply_patch",
            "exec_command",
            "wait_command",
            "list_command_sessions",
            "write_stdin",
            "kill_session",
            "git",
            "task",
            "skill",
        ] {
            assert!(
                core.contains(required),
                "missing core schema tag: {required}"
            );
        }
        assert!(
            core.len() <= 20,
            "core schema group must stay bounded: {core:?}"
        );

        for tag in [
            "anchor-skill",
            "anchor-files",
            "anchor-command",
            "anchor-git",
        ] {
            assert!(
                tools.iter().any(|tool| {
                    tool["description"]
                        .as_str()
                        .is_some_and(|description| description.contains(tag))
                }),
                "missing lazy schema group tag: {tag}"
            );
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
    fn read_file_schema_requires_exactly_one_path_source() {
        let schema = input_schema("read_file");
        let validator = jsonschema::validator_for(&schema).expect("read_file input schema");

        assert!(validator.is_valid(&json!({"path": "src/lib.rs"})));
        assert!(validator.is_valid(&json!({"files": [{"path": "src/lib.rs"}]})));
        assert!(!validator.is_valid(&json!({"max_bytes": 1024})));
        assert!(!validator.is_valid(&json!({
            "path": "src/lib.rs",
            "files": [{"path": "src/lib.rs"}]
        })));
    }

    #[test]
    fn every_advanced_tool_has_a_valid_output_schema() {
        for name in exposed_tool_names("advanced") {
            let schema = output_schema(name);
            assert_eq!(schema["type"], "object", "{name} output root");
            jsonschema::meta::validate(&schema)
                .unwrap_or_else(|error| panic!("{name} output schema: {error}"));
        }
    }

    #[test]
    fn published_local_tools_do_not_repeat_internal_output_schemas() {
        for tool in list_tools_for_profile("advanced") {
            let name = tool["name"].as_str().expect("tool name");
            assert!(
                tool.get("outputSchema").is_none(),
                "{name} should rely on internal output validation instead of publishing a duplicate outputSchema"
            );
        }
    }

    #[test]
    fn omitting_published_output_schemas_materially_reduces_advanced_catalog_bytes() {
        let published = list_tools_for_profile("advanced");
        let published_bytes = serde_json::to_vec(&published)
            .expect("published catalog")
            .len();
        let mut expanded = published.clone();
        for tool in &mut expanded {
            let name = tool["name"].as_str().expect("tool name").to_string();
            tool["outputSchema"] = output_schema(&name);
        }
        let expanded_bytes = serde_json::to_vec(&expanded)
            .expect("expanded catalog")
            .len();
        let saved_bytes = expanded_bytes.saturating_sub(published_bytes);
        println!(
            "advanced local catalog: published={published_bytes} expanded={expanded_bytes} saved={saved_bytes}"
        );
        assert!(
            saved_bytes >= 20_000,
            "output schema elision should save meaningful context bytes: {saved_bytes}"
        );
    }

    #[test]
    fn view_image_publishes_mcp_apps_image_viewer_metadata() {
        let tools = list_tools_for_profile("advanced");
        let view_image = tools
            .iter()
            .find(|tool| tool["name"] == "view_image")
            .expect("view_image tool");
        assert_eq!(
            view_image["_meta"]["ui"]["resourceUri"],
            crate::mcp::ui::IMAGE_VIEWER_RESOURCE_URI
        );
        assert_eq!(
            view_image["_meta"]["openai/outputTemplate"],
            crate::mcp::ui::IMAGE_VIEWER_RESOURCE_URI
        );
        assert_eq!(
            view_image["_meta"]["ui"]["visibility"],
            json!(["model", "app"])
        );
    }

    #[test]
    fn task_facade_output_accepts_structured_operation_log_summary() {
        let schema = output_schema("task");
        let validator = jsonschema::validator_for(&schema).expect("task facade output schema");
        let payload = json!({
            "ok": true,
            "facade": "task",
            "operation": "operation_log",
            "operations": [],
            "summary": {
                "total_matches": 3,
                "returned_operations": 3,
                "failed_operations": 3,
                "running_operations": 0,
                "affected_files": [],
                "tool_counts": {"apply_patch": 3},
                "duration_ms": 0
            },
            "diagnostics": [],
            "total_matches": 3,
            "next_cursor": null,
            "filters": {}
        });
        validator
            .validate(&payload)
            .expect("structured operation-log summary must satisfy task facade output schema");
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
        assert!(!names.contains(&"get_default_cwd"));
        assert!(names.contains(&"environment"));
        assert!(names.contains(&"cwd"));
        assert!(names.contains(&"read_file"));

        let cwd = tools
            .iter()
            .find(|tool| tool["name"] == "cwd")
            .expect("cwd facade");
        assert_eq!(
            cwd["inputSchema"]["properties"]["operation"]["enum"],
            json!(["get"])
        );

        let environment = tools
            .iter()
            .find(|tool| tool["name"] == "environment")
            .expect("environment facade");
        assert_eq!(
            environment["inputSchema"]["properties"]["operation"]["enum"],
            json!(["check", "cost"])
        );

        let git = tools
            .iter()
            .find(|tool| tool["name"] == "git")
            .expect("git facade");
        let operations = git["inputSchema"]["properties"]["operation"]["enum"]
            .as_array()
            .expect("git operations");
        for allowed in ["status", "worktree_list", "diff", "log", "show", "blame"] {
            assert!(operations.iter().any(|operation| operation == allowed));
        }
        for denied in ["stage", "commit", "reset", "clean", "worktree_create"] {
            assert!(!operations.iter().any(|operation| operation == denied));
        }
    }

    #[test]
    fn facade_schemas_preserve_profile_specific_leaf_permissions() {
        let core = list_tools_for_profile("core");
        let task = core
            .iter()
            .find(|tool| tool["name"] == "task")
            .expect("core task facade");
        let core_task_operations = task["inputSchema"]["properties"]["operation"]["enum"]
            .as_array()
            .expect("core task operations");
        for allowed in [
            "verification_disposition",
            "accept_latest_baseline",
            "switch",
        ] {
            assert!(
                core_task_operations
                    .iter()
                    .any(|operation| operation == allowed),
                "missing core task operation {allowed}"
            );
        }
        for denied in ["status", "start", "finish", "pause", "abort", "resume"] {
            assert!(!core_task_operations
                .iter()
                .any(|operation| operation == denied));
        }
        assert!(
            task["inputSchema"]["properties"]["operation"]["description"]
                .as_str()
                .is_some_and(|description| description.contains("switch(task_id)"))
        );

        let advanced = list_tools_for_profile("advanced");
        let advanced_names = advanced
            .iter()
            .filter_map(|tool| tool["name"].as_str())
            .collect::<HashSet<_>>();
        assert!(advanced_names.contains("task"));
        assert!(advanced_names.contains("slice"));
        assert!(advanced_names.contains("commit_stage"));
        assert!(advanced_names.contains("environment"));
        assert!(advanced_names.contains("cwd"));
        let advanced_task = advanced
            .iter()
            .find(|tool| tool["name"] == "task")
            .expect("advanced task facade");
        assert!(
            advanced_task["inputSchema"]["properties"]["operation"]["enum"]
                .as_array()
                .is_some_and(|operations| operations.iter().any(|operation| operation == "abort"))
        );
        assert!(
            advanced_task["inputSchema"]["properties"]["operation"]["enum"]
                .as_array()
                .is_some_and(|operations| {
                    operations.iter().any(|operation| operation == "start")
                        && operations.iter().any(|operation| operation == "finish")
                })
        );
        for hidden_leaf in [
            "check_exec_environment",
            "exec_health_check",
            "command_cost_explain",
            "get_default_cwd",
            "set_default_cwd",
        ] {
            assert!(!advanced_names.contains(hidden_leaf));
        }
        for facade in ["task", "slice", "commit_stage"] {
            let tool = advanced
                .iter()
                .find(|tool| tool["name"] == facade)
                .expect("advanced facade");
            assert!(tool["inputSchema"]["properties"]["operation"]["enum"]
                .as_array()
                .is_some_and(|operations| !operations.is_empty()));
        }

        let read_only = list_tools_for_profile("read-only");
        assert!(!read_only.iter().any(|tool| tool["name"] == "task"));
        assert!(!read_only.iter().any(|tool| tool["name"] == "slice"));
        assert!(!read_only.iter().any(|tool| tool["name"] == "commit_stage"));

        for (profile, tools) in [
            ("core", core),
            ("advanced", advanced),
            ("read-only", read_only),
        ] {
            let skill = tools
                .iter()
                .find(|tool| tool["name"] == "skill")
                .unwrap_or_else(|| panic!("{profile} skill facade"));
            let operations = skill["inputSchema"]["properties"]["operation"]["enum"]
                .as_array()
                .expect("skill operations");
            let expected_operations = vec![json!("list"), json!("get"), json!("read_resource")];
            assert_eq!(operations, &expected_operations);
            assert_eq!(skill["annotations"]["readOnlyHint"], true);
            assert_eq!(skill["annotations"]["destructiveHint"], false);
        }
    }
}
