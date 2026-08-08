use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use regex::Regex;
use serde_json::{json, Value};
use tokio::process::Command;

use crate::harness::model::TaskGitWorktree;
use crate::tools::workspace::{tool_ok, Workspace, WorkspaceError};

const MANAGED_WORKTREE_ROOT: &str = ".anchor/worktrees";

pub fn git_worktree_list(ws: &Workspace, _args: &Value) -> Result<Value, WorkspaceError> {
    ensure_git_repo(ws)?;
    let completed = run_git(
        ws.root(),
        &["worktree", "list", "--porcelain", "-z"],
        Duration::from_secs(15),
    )?;
    if !completed.success {
        return Err(git_error(&completed.stderr));
    }
    let main_root = ws
        .root()
        .canonicalize()
        .unwrap_or_else(|_| ws.root().to_path_buf());
    let managed_root = main_root.join(MANAGED_WORKTREE_ROOT);
    let worktrees = parse_worktree_porcelain(&completed.stdout)
        .into_iter()
        .map(|mut entry| {
            if let Some(path) = entry.get("path").and_then(Value::as_str) {
                let path = PathBuf::from(path);
                let canonical = path.canonicalize().unwrap_or(path);
                entry["is_main"] = json!(canonical == main_root);
                entry["managed"] = json!(canonical.starts_with(&managed_root));
                entry["managed_path"] = if canonical.starts_with(&managed_root) {
                    json!(relative_managed_worktree_display(ws, &canonical))
                } else {
                    Value::Null
                };
            }
            entry
        })
        .collect::<Vec<_>>();
    Ok(tool_ok(json!({
        "worktrees": worktrees,
        "count": worktrees.len(),
        "managed_root": MANAGED_WORKTREE_ROOT,
        "warnings": []
    })))
}

pub fn git_worktree_create(ws: &Workspace, args: &Value) -> Result<Value, WorkspaceError> {
    let branch = args.get("branch").and_then(Value::as_str);
    let base_ref = args
        .get("base_ref")
        .and_then(Value::as_str)
        .unwrap_or("HEAD");
    let name = args
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| WorkspaceError::invalid_argument("name is required"))?;
    let remove_on_close = args
        .get("remove_on_close")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let worktree = create_managed_worktree(ws, name, branch, base_ref, remove_on_close)?;
    Ok(tool_ok(json!({
        "path": relative_managed_worktree_display(ws, Path::new(&worktree.path)),
        "absolute_path": worktree.path,
        "branch": worktree.branch,
        "base_ref": worktree.base_ref,
        "managed": worktree.managed,
        "remove_on_close": worktree.remove_on_close,
        "created_at": worktree.created_at,
        "mutation_attributed": false,
        "warnings": []
    })))
}

pub fn git_worktree_remove(
    ws: &Workspace,
    args: &Value,
    dangerous_mode: bool,
) -> Result<Value, WorkspaceError> {
    ensure_git_repo(ws)?;
    let raw_path = args
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| WorkspaceError::invalid_argument("path is required"))?;
    let force = args.get("force").and_then(Value::as_bool).unwrap_or(false);
    if force && !dangerous_mode {
        return Err(WorkspaceError::ToolDetails {
            code: "DANGEROUS_OPERATION_REQUIRES_DANGEROUS_MODE",
            message: "Force-removing a Git worktree requires operator-enabled dangerous mode."
                .into(),
            category: "permission",
            retryable: false,
            details: json!({"path": raw_path, "suggestion": "Remove or commit worktree changes first, then retry without force."}),
        });
    }
    let path = managed_worktree_path(ws, raw_path)?;
    let status = run_git(
        &path,
        &["status", "--porcelain=v1"],
        Duration::from_secs(10),
    )?;
    if !status.success && !force {
        return Err(git_error(&status.stderr));
    }
    if !status.stdout.trim().is_empty() && !force {
        return Err(WorkspaceError::ToolDetails {
            code: "GIT_WORKTREE_NOT_CLEAN",
            message: "Git worktree contains uncommitted changes.".into(),
            category: "validation",
            retryable: true,
            details: json!({
                "path": relative_managed_worktree_display(ws, &path),
                "suggestion": "Commit or restore the worktree changes before removal."
            }),
        });
    }
    let mut command = vec!["worktree", "remove"];
    if force {
        command.push("--force");
    }
    let path_text = path.to_string_lossy().into_owned();
    command.push(path_text.as_str());
    let completed = run_git(ws.root(), &command, Duration::from_secs(60))?;
    if !completed.success {
        return Err(git_error(&completed.stderr));
    }
    Ok(tool_ok(json!({
        "path": relative_managed_worktree_display(ws, &path),
        "removed": true,
        "force": force,
        "mutation_attributed": false,
        "warnings": []
    })))
}

pub fn git_worktree_prune(ws: &Workspace, _args: &Value) -> Result<Value, WorkspaceError> {
    ensure_git_repo(ws)?;
    let before = run_git(
        ws.root(),
        &["worktree", "list", "--porcelain", "-z"],
        Duration::from_secs(15),
    )?;
    let completed = run_git(
        ws.root(),
        &["worktree", "prune", "--verbose"],
        Duration::from_secs(30),
    )?;
    if !completed.success {
        return Err(git_error(&completed.stderr));
    }
    let after = run_git(
        ws.root(),
        &["worktree", "list", "--porcelain", "-z"],
        Duration::from_secs(15),
    )?;
    let before_count = parse_worktree_porcelain(&before.stdout).len();
    let after_count = parse_worktree_porcelain(&after.stdout).len();
    Ok(tool_ok(json!({
        "pruned_count": before_count.saturating_sub(after_count),
        "remaining_count": after_count,
        "details": completed.stderr.lines().chain(completed.stdout.lines()).collect::<Vec<_>>(),
        "mutation_attributed": false,
        "warnings": []
    })))
}

pub(crate) fn create_managed_worktree(
    ws: &Workspace,
    name: &str,
    branch: Option<&str>,
    base_ref: &str,
    remove_on_close: bool,
) -> Result<TaskGitWorktree, WorkspaceError> {
    ensure_git_repo(ws)?;
    let name = validate_worktree_name(name)?;
    let base_ref = validate_git_ref(base_ref)?;
    let branch = branch
        .map(validate_worktree_branch)
        .transpose()?
        .map(str::to_string)
        .unwrap_or_else(|| format!("anchor/task/{name}"));
    validate_worktree_branch(&branch)?;
    let relative_path = format!("{MANAGED_WORKTREE_ROOT}/{name}");
    let resolved = ws.resolve_for_write(&relative_path)?;
    if resolved.path.exists() {
        return Err(WorkspaceError::ToolDetails {
            code: "GIT_WORKTREE_PATH_EXISTS",
            message: format!("Managed worktree path already exists: {relative_path}"),
            category: "conflict",
            retryable: true,
            details: json!({"path": relative_path}),
        });
    }
    if let Some(parent) = resolved.path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| WorkspaceError::ToolDetails {
            code: "GIT_WORKTREE_CREATE_FAILED",
            message: error.to_string(),
            category: "runtime",
            retryable: true,
            details: json!({"path": relative_path}),
        })?;
    }
    let command = vec![
        "worktree".to_string(),
        "add".to_string(),
        "-b".to_string(),
        branch.clone(),
        relative_path.clone(),
        base_ref.to_string(),
    ];
    let completed = run_git_owned(ws.root(), &command, Duration::from_secs(120))?;
    if !completed.success {
        return Err(WorkspaceError::ToolDetails {
            code: "GIT_WORKTREE_CREATE_FAILED",
            message: completed.stderr.trim().to_string(),
            category: "runtime",
            retryable: true,
            details: json!({"path": relative_path, "branch": branch, "base_ref": base_ref}),
        });
    }
    let path = resolved
        .path
        .canonicalize()
        .map_err(|error| WorkspaceError::ToolDetails {
            code: "GIT_WORKTREE_CREATE_FAILED",
            message: error.to_string(),
            category: "runtime",
            retryable: true,
            details: json!({"path": relative_path}),
        })?;
    Ok(TaskGitWorktree {
        path: path.to_string_lossy().into_owned(),
        branch,
        base_ref: base_ref.to_string(),
        managed: true,
        remove_on_close,
        created_at: chrono::Utc::now().to_rfc3339(),
    })
}

pub(crate) fn remove_managed_task_worktree(
    ws: &Workspace,
    worktree: &TaskGitWorktree,
) -> Result<(), WorkspaceError> {
    if !worktree.managed {
        return Err(WorkspaceError::invalid_argument(
            "Only Anchor-managed worktrees can be removed automatically",
        ));
    }
    let path = managed_worktree_path(ws, &worktree.path)?;
    let path_text = path.to_string_lossy().into_owned();
    let completed = run_git(
        ws.root(),
        &["worktree", "remove", path_text.as_str()],
        Duration::from_secs(60),
    )?;
    if !completed.success {
        return Err(git_error(&completed.stderr));
    }
    Ok(())
}

fn parse_worktree_porcelain(raw: &str) -> Vec<Value> {
    let mut records = Vec::new();
    let mut current = serde_json::Map::new();
    for token in raw
        .split('\0')
        .map(str::trim)
        .filter(|token| !token.is_empty())
    {
        if let Some(path) = token.strip_prefix("worktree ") {
            if !current.is_empty() {
                records.push(Value::Object(std::mem::take(&mut current)));
            }
            current.insert("path".into(), json!(path));
        } else if let Some(head) = token.strip_prefix("HEAD ") {
            current.insert("head".into(), json!(head));
        } else if let Some(branch) = token.strip_prefix("branch ") {
            current.insert(
                "branch".into(),
                json!(branch.strip_prefix("refs/heads/").unwrap_or(branch)),
            );
        } else if token == "detached" {
            current.insert("detached".into(), json!(true));
        } else if let Some(reason) = token.strip_prefix("locked") {
            current.insert("locked".into(), json!(true));
            current.insert("locked_reason".into(), json!(reason.trim()));
        } else if let Some(reason) = token.strip_prefix("prunable") {
            current.insert("prunable".into(), json!(true));
            current.insert("prunable_reason".into(), json!(reason.trim()));
        }
    }
    if !current.is_empty() {
        records.push(Value::Object(current));
    }
    for record in &mut records {
        if let Some(object) = record.as_object_mut() {
            object.entry("branch").or_insert(Value::Null);
            object.entry("detached").or_insert(json!(false));
            object.entry("locked").or_insert(json!(false));
            object.entry("locked_reason").or_insert(Value::Null);
            object.entry("prunable").or_insert(json!(false));
            object.entry("prunable_reason").or_insert(Value::Null);
        }
    }
    records
}

fn validate_worktree_name(name: &str) -> Result<&str, WorkspaceError> {
    let name = name.trim();
    if name.is_empty()
        || name.len() > 128
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(WorkspaceError::invalid_argument(
            "worktree name must contain only ASCII letters, digits, hyphen, or underscore",
        ));
    }
    Ok(name)
}

fn validate_worktree_branch(branch: &str) -> Result<&str, WorkspaceError> {
    let branch = validate_git_ref(branch)?;
    if branch == "HEAD" || branch.starts_with("refs/") {
        return Err(WorkspaceError::invalid_argument(
            "worktree branch must be a local branch name",
        ));
    }
    Ok(branch)
}

fn managed_worktree_path(ws: &Workspace, raw_path: &str) -> Result<PathBuf, WorkspaceError> {
    let managed_root = ws.root().join(MANAGED_WORKTREE_ROOT);
    let candidate = PathBuf::from(raw_path);
    let candidate = if candidate.is_absolute() {
        candidate
    } else {
        ws.root().join(candidate)
    };
    let canonical = candidate
        .canonicalize()
        .map_err(|_| WorkspaceError::not_found("Managed Git worktree path does not exist"))?;
    let canonical_root = managed_root.canonicalize().unwrap_or(managed_root);
    if !canonical.starts_with(&canonical_root) || canonical == canonical_root {
        return Err(WorkspaceError::ToolDetails {
            code: "GIT_WORKTREE_PATH_NOT_MANAGED",
            message: "Only worktrees under .anchor/worktrees can be managed by this tool.".into(),
            category: "security",
            retryable: false,
            details: json!({"path": raw_path, "managed_root": MANAGED_WORKTREE_ROOT}),
        });
    }
    Ok(canonical)
}

fn relative_managed_worktree_display(ws: &Workspace, path: &Path) -> String {
    path.strip_prefix(ws.root())
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

pub fn git_status(ws: &Workspace, args: &Value) -> Result<Value, WorkspaceError> {
    let path = args.get("path").and_then(Value::as_str).unwrap_or(".");
    let resolved = ws.resolve_existing(path)?;
    let max_entries = args
        .get("max_entries")
        .and_then(Value::as_u64)
        .unwrap_or(1000) as usize;
    let include_untracked = args
        .get("include_untracked")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let diagnose_metadata_only = args
        .get("diagnose_metadata_only")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let refresh_index = args
        .get("refresh_index")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let root_check = run_git(
        &resolved.path,
        &["rev-parse", "--show-toplevel"],
        Duration::from_secs(10),
    )?;
    if !root_check.success {
        return Ok(tool_ok(json!({
            "is_repo": false,
            "clean": true,
            "entries": [],
            "warnings": [root_check.stderr.trim()]
        })));
    }

    let mut status_args = vec!["status", "--porcelain=v1", "-b"];
    if !include_untracked {
        status_args.push("--untracked-files=no");
    }
    let completed = run_git(&resolved.path, &status_args, Duration::from_secs(10))?;
    if !completed.success && completed.exit_code != 0 {
        return Err(git_error(&completed.stderr));
    }

    let mut branch = String::new();
    let mut upstream = String::new();
    let mut ahead = 0i64;
    let mut behind = 0i64;
    let mut entries = Vec::new();
    let mut metadata_only_entries = Vec::new();
    let mut warnings = Vec::new();
    let lines: Vec<_> = completed.stdout.lines().collect();
    let total_lines = lines.len();

    for line in lines {
        if let Some(rest) = line.strip_prefix("## ") {
            (branch, upstream, ahead, behind) = parse_branch_line(rest);
            continue;
        }
        if line.len() < 4 {
            continue;
        }
        let index_status = line.chars().next().unwrap_or(' ').to_string();
        let worktree_status = line.chars().nth(1).unwrap_or(' ').to_string();
        let mut path_text = line[3..].to_string();
        let original = if let Some((orig, new)) = path_text.split_once(" -> ") {
            let orig = orig.to_string();
            path_text = new.to_string();
            Some(orig)
        } else {
            None
        };
        let mut entry = json!({
            "path": path_text,
            "index_status": index_status,
            "worktree_status": worktree_status
        });
        if let Some(orig) = original {
            entry["original_path"] = json!(orig);
        }
        if diagnose_metadata_only
            && index_status == " "
            && worktree_status == "M"
            && !path_text.starts_with('"')
        {
            if let Some(diagnostic) =
                diagnose_metadata_only_change(&resolved.path, &path_text, refresh_index)
            {
                metadata_only_entries.push(diagnostic);
                continue;
            }
        }
        entries.push(entry);
        if entries.len() >= max_entries {
            break;
        }
    }

    if !upstream.is_empty() {
        match git_ahead_behind(&resolved.path, &upstream) {
            Ok((computed_ahead, computed_behind)) => {
                ahead = computed_ahead;
                behind = computed_behind;
            }
            Err(message) => warnings.push(message),
        }
    }

    let head = git_rev_parse(&resolved.path, "HEAD").unwrap_or_default();
    let metadata_only_count = metadata_only_entries.len();
    let index_refresh_performed = refresh_index
        && metadata_only_count > 0
        && metadata_only_entries
            .iter()
            .all(|entry| entry.get("index_refreshed").and_then(Value::as_bool) == Some(true));
    let index_refresh_failed_count = if refresh_index {
        metadata_only_entries
            .iter()
            .filter(|entry| entry.get("index_refreshed").and_then(Value::as_bool) != Some(true))
            .count()
    } else {
        0
    };
    Ok(tool_ok(json!({
        "is_repo": true,
        "branch": branch,
        "head": head,
        "upstream": upstream,
        "ahead": ahead,
        "behind": behind,
        "clean": entries.is_empty(),
        "raw_clean": entries.is_empty() && metadata_only_entries.is_empty(),
        "entries": entries,
        "metadata_only_entries": metadata_only_entries,
        "metadata_only_count": metadata_only_count,
        "content_changed_count": entries.len(),
        "index_refresh_performed": index_refresh_performed,
        "index_refresh_failed_count": index_refresh_failed_count,
        "truncated": entries.len() >= max_entries && total_lines > max_entries + 1,
        "warnings": warnings
    })))
}

pub fn git_stage(ws: &Workspace, args: &Value) -> Result<Value, WorkspaceError> {
    ensure_git_repo(ws)?;
    let paths = git_write_paths(ws, args)?;
    let mut command = vec!["add".to_string(), "--".to_string()];
    command.extend(paths.iter().cloned());
    let completed = run_git_owned(ws.root(), &command, Duration::from_secs(30))?;
    if !completed.success {
        return Err(git_error(&completed.stderr));
    }
    let staged_files = git_name_list(ws.root(), &["diff", "--cached", "--name-only"])?;
    Ok(tool_ok(json!({
        "staged_paths": paths,
        "staged_files": staged_files,
        "mutation_attributed": true,
        "warnings": []
    })))
}

pub fn git_commit(ws: &Workspace, args: &Value) -> Result<Value, WorkspaceError> {
    ensure_git_repo(ws)?;
    let message = args
        .get("message")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|message| !message.is_empty())
        .ok_or_else(|| WorkspaceError::invalid_argument("message is required"))?;
    if message.len() > 4000 || message.contains('\0') {
        return Err(WorkspaceError::invalid_argument(
            "commit message must be between 1 and 4000 bytes",
        ));
    }
    let staged_files = git_name_list(ws.root(), &["diff", "--cached", "--name-only"])?;
    if staged_files.is_empty() {
        return Err(WorkspaceError::ToolDetails {
            code: "GIT_NOTHING_STAGED",
            message: "No staged changes are available to commit.".into(),
            category: "validation",
            retryable: true,
            details: json!({
                "suggestion": "Call git with operation=stage and explicit workspace-relative paths first."
            }),
        });
    }
    let command = vec![
        "commit".to_string(),
        "--no-gpg-sign".to_string(),
        "--no-verify".to_string(),
        "-m".to_string(),
        message.to_string(),
    ];
    let completed = run_git_owned(ws.root(), &command, Duration::from_secs(120))?;
    if !completed.success {
        return Err(git_error(&completed.stderr));
    }
    let commit_sha = git_rev_parse(ws.root(), "HEAD").unwrap_or_default();
    let committed_files = git_name_list(
        ws.root(),
        &["diff-tree", "--no-commit-id", "--name-only", "-r", "HEAD"],
    )?;
    Ok(tool_ok(json!({
        "commit_sha": commit_sha,
        "message": message,
        "committed_files": committed_files,
        "previously_staged_files": staged_files,
        "mutation_attributed": true,
        "warnings": []
    })))
}

pub fn git_restore(ws: &Workspace, args: &Value) -> Result<Value, WorkspaceError> {
    ensure_git_repo(ws)?;
    let paths = git_write_paths(ws, args)?;
    let staged = args.get("staged").and_then(Value::as_bool).unwrap_or(false);
    let worktree = args
        .get("worktree")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    if !staged && !worktree {
        return Err(WorkspaceError::invalid_argument(
            "at least one of staged or worktree must be true",
        ));
    }
    if staged {
        let mut command = vec![
            "restore".to_string(),
            "--staged".to_string(),
            "--".to_string(),
        ];
        command.extend(paths.iter().cloned());
        let completed = run_git_owned(ws.root(), &command, Duration::from_secs(30))?;
        if !completed.success {
            return Err(git_error(&completed.stderr));
        }
    }
    if worktree {
        let mut command = vec![
            "restore".to_string(),
            "--worktree".to_string(),
            "--".to_string(),
        ];
        command.extend(paths.iter().cloned());
        let completed = run_git_owned(ws.root(), &command, Duration::from_secs(30))?;
        if !completed.success {
            return Err(git_error(&completed.stderr));
        }
    }
    Ok(tool_ok(json!({
        "restored_paths": paths,
        "staged": staged,
        "worktree": worktree,
        "mutation_attributed": true,
        "warnings": []
    })))
}

pub fn git_reset(
    ws: &Workspace,
    args: &Value,
    dangerous_mode: bool,
) -> Result<Value, WorkspaceError> {
    ensure_git_repo(ws)?;
    let revision = validate_git_ref(
        args.get("revision")
            .and_then(Value::as_str)
            .unwrap_or("HEAD"),
    )?;
    let mode = args.get("mode").and_then(Value::as_str).unwrap_or("mixed");
    if !matches!(mode, "soft" | "mixed" | "hard") {
        return Err(WorkspaceError::invalid_argument(
            "mode must be soft, mixed, or hard",
        ));
    }
    if mode == "hard" && !dangerous_mode {
        return Err(WorkspaceError::ToolDetails {
            code: "DANGEROUS_OPERATION_REQUIRES_DANGEROUS_MODE",
            message: "git_reset mode=hard requires operator-enabled dangerous mode.".into(),
            category: "permission",
            retryable: false,
            details: json!({
                "mode": mode,
                "revision": revision,
                "recoverable": true,
                "suggestion": "Use mode=soft or mode=mixed, or enable dangerous mode in the trusted control plane."
            }),
        });
    }
    let verify = format!("{revision}^{{commit}}");
    let target_head = git_rev_parse(ws.root(), &verify).ok_or_else(|| {
        WorkspaceError::invalid_argument(format!("Unknown commit revision: {revision}"))
    })?;
    let before_head = git_rev_parse(ws.root(), "HEAD").unwrap_or_default();
    let flag = format!("--{mode}");
    let completed = run_git(
        ws.root(),
        &["reset", flag.as_str(), target_head.as_str()],
        Duration::from_secs(60),
    )?;
    if !completed.success {
        return Err(git_error(&completed.stderr));
    }
    let after_head = git_rev_parse(ws.root(), "HEAD").unwrap_or_default();
    Ok(tool_ok(json!({
        "before_head": before_head,
        "target_head": target_head,
        "after_head": after_head,
        "mode": mode,
        "mutation_attributed": true,
        "warnings": []
    })))
}

pub fn git_revert(ws: &Workspace, args: &Value) -> Result<Value, WorkspaceError> {
    ensure_git_repo(ws)?;
    let abort = args.get("abort").and_then(Value::as_bool).unwrap_or(false);
    if abort {
        let completed = run_git(ws.root(), &["revert", "--abort"], Duration::from_secs(30))?;
        if !completed.success {
            return Err(git_error(&completed.stderr));
        }
        return Ok(tool_ok(json!({
            "aborted": true,
            "reverted_commit": null,
            "no_commit": true,
            "staged_files": [],
            "mutation_attributed": true,
            "warnings": []
        })));
    }
    let revision = args
        .get("revision")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            WorkspaceError::invalid_argument("revision is required unless abort=true")
        })?;
    let revision = validate_git_ref(revision)?;
    let verify = format!("{revision}^{{commit}}");
    let commit = git_rev_parse(ws.root(), &verify).ok_or_else(|| {
        WorkspaceError::invalid_argument(format!("Unknown commit revision: {revision}"))
    })?;
    let status = run_git(
        ws.root(),
        &["status", "--porcelain=v1"],
        Duration::from_secs(10),
    )?;
    if !status.success {
        return Err(git_error(&status.stderr));
    }
    if !status.stdout.trim().is_empty() {
        return Err(WorkspaceError::ToolDetails {
            code: "GIT_WORKTREE_NOT_CLEAN",
            message: "git_revert requires a clean index and working tree.".into(),
            category: "validation",
            retryable: true,
            details: json!({
                "recoverable": true,
                "suggestion": "Commit, restore, or clean the current changes before reverting a commit."
            }),
        });
    }
    let completed = run_git(
        ws.root(),
        &["revert", "--no-commit", commit.as_str()],
        Duration::from_secs(120),
    )?;
    if !completed.success {
        return Err(WorkspaceError::ToolDetails {
            code: "GIT_REVERT_CONFLICT",
            message: completed.stderr.trim().to_string(),
            category: "runtime",
            retryable: false,
            details: json!({
                "revision": revision,
                "commit": commit,
                "recoverable": true,
                "recovery_tool": "git",
                "recovery_args": {"operation": "revert", "abort": true},
                "suggestion": "Resolve the conflicts and commit, or call git with operation=revert and abort=true."
            }),
        });
    }
    let staged_files = git_name_list(ws.root(), &["diff", "--cached", "--name-only"])?;
    Ok(tool_ok(json!({
        "aborted": false,
        "reverted_commit": commit,
        "no_commit": true,
        "staged_files": staged_files,
        "mutation_attributed": true,
        "warnings": []
    })))
}

pub fn git_clean(
    ws: &Workspace,
    args: &Value,
    dangerous_mode: bool,
) -> Result<Value, WorkspaceError> {
    ensure_git_repo(ws)?;
    let dry_run = args.get("dry_run").and_then(Value::as_bool).unwrap_or(true);
    let directories = args
        .get("directories")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let include_ignored = args
        .get("include_ignored")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let paths = optional_git_write_paths(ws, args)?;
    if !dry_run && paths.is_empty() && !dangerous_mode {
        return Err(WorkspaceError::ToolDetails {
            code: "DANGEROUS_OPERATION_REQUIRES_DANGEROUS_MODE",
            message: "Repository-wide git_clean requires operator-enabled dangerous mode.".into(),
            category: "permission",
            retryable: false,
            details: json!({
                "recoverable": true,
                "suggestion": "Use dry_run=true, provide explicit paths, or enable dangerous mode."
            }),
        });
    }
    if !dry_run && include_ignored && !dangerous_mode {
        return Err(WorkspaceError::ToolDetails {
            code: "DANGEROUS_OPERATION_REQUIRES_DANGEROUS_MODE",
            message: "Deleting ignored files requires operator-enabled dangerous mode.".into(),
            category: "permission",
            retryable: false,
            details: json!({"recoverable": true}),
        });
    }
    let mut command = vec!["clean".to_string()];
    command.push(if dry_run { "-n" } else { "-f" }.to_string());
    if directories {
        command.push("-d".to_string());
    }
    if include_ignored {
        command.push("-x".to_string());
    }
    if !paths.is_empty() {
        command.push("--".to_string());
        command.extend(paths.iter().cloned());
    }
    let completed = run_git_owned(ws.root(), &command, Duration::from_secs(60))?;
    if !completed.success {
        return Err(git_error(&completed.stderr));
    }
    let candidates = parse_git_clean_paths(&completed.stdout);
    Ok(tool_ok(json!({
        "dry_run": dry_run,
        "directories": directories,
        "include_ignored": include_ignored,
        "paths": paths,
        "candidates": candidates,
        "removed_paths": if dry_run { Vec::<String>::new() } else { candidates.clone() },
        "mutation_attributed": !dry_run,
        "warnings": []
    })))
}

fn git_write_paths(ws: &Workspace, args: &Value) -> Result<Vec<String>, WorkspaceError> {
    let values = args
        .get("paths")
        .and_then(Value::as_array)
        .ok_or_else(|| WorkspaceError::invalid_argument("paths is required"))?;
    if values.is_empty() || values.len() > 256 {
        return Err(WorkspaceError::invalid_argument(
            "paths must contain between 1 and 256 entries",
        ));
    }
    let mut paths = Vec::with_capacity(values.len());
    for value in values {
        let path = value
            .as_str()
            .filter(|path| !path.trim().is_empty())
            .ok_or_else(|| WorkspaceError::invalid_argument("paths must contain strings"))?;
        ws.reject_unsafe_text(path)?;
        let resolved = ws.resolve_lexical_write_path(path)?;
        if resolved.display == ".git" || resolved.display.starts_with(".git/") {
            return Err(WorkspaceError::invalid_argument(
                "Git internal paths cannot be modified through Git tools",
            ));
        }
        paths.push(resolved.display);
    }
    paths.sort();
    paths.dedup();
    Ok(paths)
}

fn optional_git_write_paths(ws: &Workspace, args: &Value) -> Result<Vec<String>, WorkspaceError> {
    match args.get("paths") {
        None => Ok(Vec::new()),
        Some(Value::Array(values)) if values.is_empty() => Ok(Vec::new()),
        Some(Value::Array(_)) => git_write_paths(ws, args),
        Some(_) => Err(WorkspaceError::invalid_argument("paths must be an array")),
    }
}

fn parse_git_clean_paths(output: &str) -> Vec<String> {
    output
        .lines()
        .filter_map(|line| {
            line.trim()
                .strip_prefix("Would remove ")
                .or_else(|| line.trim().strip_prefix("Removing "))
                .map(str::to_string)
        })
        .collect()
}

fn ensure_git_repo(ws: &Workspace) -> Result<(), WorkspaceError> {
    if is_git_repo(ws.root()) {
        Ok(())
    } else {
        Err(WorkspaceError::Tool {
            code: "NOT_GIT_REPOSITORY",
            message: "Workspace is not a Git repository.".into(),
            category: "validation",
            retryable: false,
        })
    }
}

fn git_name_list(root: &std::path::Path, args: &[&str]) -> Result<Vec<String>, WorkspaceError> {
    let completed = run_git(root, args, Duration::from_secs(15))?;
    if !completed.success {
        return Err(git_error(&completed.stderr));
    }
    Ok(completed
        .stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect())
}

fn diagnose_metadata_only_change(
    root: &std::path::Path,
    path: &str,
    refresh_index: bool,
) -> Option<Value> {
    let staged = run_git(
        root,
        &["ls-files", "--stage", "--", path],
        Duration::from_secs(5),
    )
    .ok()?;
    if !staged.success {
        return None;
    }
    let index_blob = staged
        .stdout
        .split_whitespace()
        .nth(1)
        .filter(|value| value.len() == 40)?;
    let path_filter = format!("--path={path}");
    let filtered = run_git(
        root,
        &["hash-object", path_filter.as_str(), "--", path],
        Duration::from_secs(5),
    )
    .ok()?;
    if !filtered.success || filtered.stdout.trim() != index_blob {
        return None;
    }
    let raw = run_git(
        root,
        &["hash-object", "--no-filters", "--", path],
        Duration::from_secs(5),
    )
    .ok();
    let raw_matches = raw
        .as_ref()
        .is_some_and(|value| value.success && value.stdout.trim() == index_blob);
    let classification = if raw_matches {
        "stat_cache_stale"
    } else {
        "line_ending_or_clean_filter_only"
    };
    let mut refreshed = false;
    if refresh_index {
        refreshed = run_git(
            root,
            &["update-index", "--refresh", "--", path],
            Duration::from_secs(5),
        )
        .is_ok_and(|result| result.success);
    }
    Some(json!({
        "path": path,
        "classification": classification,
        "content_changed": false,
        "index_blob": index_blob,
        "worktree_filtered_blob": filtered.stdout.trim(),
        "worktree_raw_blob": raw.and_then(|value| value.success.then(|| value.stdout.trim().to_string())),
        "safe_index_refresh": true,
        "index_refreshed": refreshed,
        "suggestion": if refreshed {
            "Git index stat cache was refreshed safely because filtered content matched the index blob."
        } else {
            "Run git_status with refresh_index=true to refresh the index stat cache safely."
        }
    }))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::process::Command as StdCommand;
    use std::time::{Duration, Instant};

    use serde_json::json;

    use super::{
        diagnose_metadata_only_change, git_clean, git_commit, git_reset, git_restore, git_revert,
        git_stage, git_status, parse_branch_line, run_process_with_timeout,
    };
    use crate::tools::workspace::Workspace;

    #[test]
    fn git_process_helper_enforces_deadline() {
        let Ok(python) = which::which("python") else {
            return;
        };
        let temp = tempfile::tempdir().expect("tempdir");
        let started = Instant::now();
        let error = run_process_with_timeout(
            &python.display().to_string(),
            temp.path(),
            &["-c".into(), "import time; time.sleep(30)".into()],
            Duration::from_millis(100),
        )
        .expect_err("timeout");
        assert_eq!(error.to_error_value()["code"], "GIT_TIMEOUT");
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn branch_metadata_parses_bracketed_ahead_and_behind_counts() {
        assert_eq!(
            parse_branch_line("main...origin/main [ahead 6, behind 2]"),
            ("main".into(), "origin/main".into(), 6, 2)
        );
    }

    #[test]
    fn structured_reset_revert_and_clean_preserve_explicit_safety_boundaries() {
        let temp = tempfile::tempdir().expect("tempdir");
        let repo = temp.path().join("repo");
        fs::create_dir_all(&repo).expect("repo dir");
        git(&repo, &["init", "--initial-branch=main"]);
        git(
            &repo,
            &["config", "user.email", "anchor-tests@example.invalid"],
        );
        git(&repo, &["config", "user.name", "Anchor Tests"]);
        fs::write(repo.join("main.txt"), "initial\n").expect("initial file");
        git(&repo, &["add", "main.txt"]);
        git(&repo, &["commit", "-m", "initial"]);
        let initial = String::from_utf8(
            StdCommand::new("git")
                .arg("-C")
                .arg(&repo)
                .args(["rev-parse", "HEAD"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_string();
        fs::write(repo.join("main.txt"), "second\n").expect("second file");
        git(&repo, &["add", "main.txt"]);
        git(&repo, &["commit", "-m", "second"]);
        let second = String::from_utf8(
            StdCommand::new("git")
                .arg("-C")
                .arg(&repo)
                .args(["rev-parse", "HEAD"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_string();
        let workspace = Workspace::new(repo.clone()).expect("workspace");

        let soft = git_reset(
            &workspace,
            &json!({"revision": initial, "mode": "soft"}),
            false,
        )
        .expect("soft reset");
        assert_eq!(soft["mode"], "soft");
        assert_eq!(soft["after_head"], initial);
        let hard_denied = git_reset(
            &workspace,
            &json!({"revision": second, "mode": "hard"}),
            false,
        )
        .expect_err("hard reset gate");
        assert_eq!(
            hard_denied.to_error_value()["code"],
            "DANGEROUS_OPERATION_REQUIRES_DANGEROUS_MODE"
        );
        git(&repo, &["reset", "--hard", second.as_str()]);

        let reverted = git_revert(&workspace, &json!({"revision": second})).expect("revert");
        assert_eq!(reverted["reverted_commit"], second);
        assert_eq!(reverted["staged_files"], json!(["main.txt"]));
        git(&repo, &["reset", "--hard", "HEAD"]);

        fs::write(repo.join("scratch.txt"), "scratch\n").expect("scratch");
        let preview = git_clean(
            &workspace,
            &json!({"dry_run": true, "paths": ["scratch.txt"]}),
            false,
        )
        .expect("clean preview");
        assert_eq!(preview["candidates"], json!(["scratch.txt"]));
        assert!(repo.join("scratch.txt").exists());
        let cleaned = git_clean(
            &workspace,
            &json!({"dry_run": false, "paths": ["scratch.txt"]}),
            false,
        )
        .expect("clean path");
        assert_eq!(cleaned["removed_paths"], json!(["scratch.txt"]));
        assert!(!repo.join("scratch.txt").exists());
    }

    #[test]
    fn git_status_reports_real_ahead_count_against_upstream() {
        let temp = tempfile::tempdir().expect("tempdir");
        let repo = temp.path().join("repo");
        let remote = temp.path().join("remote.git");
        fs::create_dir_all(&repo).expect("repo dir");

        git(&repo, &["init", "--initial-branch=main"]);
        git(
            &repo,
            &["config", "user.email", "anchor-tests@example.invalid"],
        );
        git(&repo, &["config", "user.name", "Anchor Tests"]);
        fs::write(repo.join("main.txt"), "initial\n").expect("initial file");
        git(&repo, &["add", "main.txt"]);
        git(&repo, &["commit", "-m", "initial"]);

        git(
            temp.path(),
            &[
                "clone",
                "--bare",
                repo.to_str().unwrap(),
                remote.to_str().unwrap(),
            ],
        );
        git(
            &repo,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        git(&repo, &["fetch", "origin"]);
        git(&repo, &["branch", "--set-upstream-to=origin/main", "main"]);

        fs::write(repo.join("main.txt"), "ahead\n").expect("ahead file");
        git(&repo, &["add", "main.txt"]);
        git(&repo, &["commit", "-m", "ahead"]);

        let workspace = Workspace::new(repo).expect("workspace");
        let result = git_status(&workspace, &json!({})).expect("git status");
        assert_eq!(result["ahead"], 1);
        assert_eq!(result["behind"], 0);
        assert_eq!(result["upstream"], "origin/main");
    }

    #[test]
    fn unchanged_content_is_classified_as_safe_metadata_only_state() {
        let temp = tempfile::tempdir().expect("tempdir");
        let repo = temp.path().join("repo");
        fs::create_dir_all(&repo).expect("repo dir");
        git(&repo, &["init", "--initial-branch=main"]);
        git(
            &repo,
            &["config", "user.email", "anchor-tests@example.invalid"],
        );
        git(&repo, &["config", "user.name", "Anchor Tests"]);
        fs::write(repo.join("main.txt"), "same\n").expect("file");
        git(&repo, &["add", "main.txt"]);
        git(&repo, &["commit", "-m", "initial"]);

        let diagnostic = diagnose_metadata_only_change(&repo, "main.txt", false)
            .expect("matching worktree and index blob");
        assert_eq!(diagnostic["content_changed"], false);
        assert_eq!(diagnostic["classification"], "stat_cache_stale");
        assert_eq!(diagnostic["safe_index_refresh"], true);
    }

    #[test]
    fn structured_git_write_tools_stage_commit_and_restore_explicit_paths() {
        let temp = tempfile::tempdir().expect("tempdir");
        let repo = temp.path().join("repo");
        fs::create_dir_all(&repo).expect("repo dir");
        git(&repo, &["init", "--initial-branch=main"]);
        git(
            &repo,
            &["config", "user.email", "anchor-tests@example.invalid"],
        );
        git(&repo, &["config", "user.name", "Anchor Tests"]);
        fs::write(repo.join("main.txt"), "initial\n").expect("initial file");
        git(&repo, &["add", "main.txt"]);
        git(&repo, &["commit", "-m", "initial"]);

        let workspace = Workspace::new(repo.clone()).expect("workspace");
        fs::write(repo.join("main.txt"), "updated\n").expect("updated file");
        let staged = git_stage(&workspace, &json!({"paths": ["main.txt"]})).expect("stage");
        assert_eq!(staged["staged_files"], json!(["main.txt"]));
        assert_eq!(staged["mutation_attributed"], true);

        let committed = git_commit(&workspace, &json!({"message": "update main"})).expect("commit");
        assert_eq!(committed["committed_files"], json!(["main.txt"]));
        assert!(committed["commit_sha"]
            .as_str()
            .is_some_and(|value| value.len() >= 7));

        fs::write(repo.join("main.txt"), "discard me\n").expect("dirty file");
        let restored = git_restore(
            &workspace,
            &json!({"paths": ["main.txt"], "worktree": true, "staged": false}),
        )
        .expect("restore");
        assert_eq!(restored["restored_paths"], json!(["main.txt"]));
        assert_eq!(
            fs::read_to_string(repo.join("main.txt"))
                .expect("restored file")
                .replace("\r\n", "\n"),
            "updated\n"
        );
    }

    fn git(cwd: &std::path::Path, args: &[&str]) {
        let output = StdCommand::new("git")
            .arg("-C")
            .arg(cwd)
            .args(args)
            .output()
            .expect("git command");
        assert!(
            output.status.success(),
            "git {:?}: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

pub fn git_diff(ws: &Workspace, args: &Value) -> Result<Value, WorkspaceError> {
    let staged = args.get("staged").and_then(Value::as_bool).unwrap_or(false);
    let unstaged = args
        .get("unstaged")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let context = args
        .get("context_lines")
        .and_then(Value::as_u64)
        .unwrap_or(3);
    let max_bytes = args
        .get("max_bytes")
        .and_then(Value::as_u64)
        .unwrap_or(262_144) as usize;

    let mut path_filters: Vec<String> = Vec::new();
    if let Some(p) = args.get("path").and_then(Value::as_str) {
        path_filters.push(p.to_string());
    }
    if let Some(paths) = args.get("paths").and_then(Value::as_array) {
        for p in paths {
            if let Some(s) = p.as_str() {
                path_filters.push(s.to_string());
            }
        }
    }
    for p in &path_filters {
        ws.reject_unsafe_text(p)?;
    }

    if !is_git_repo(ws.root()) {
        return Ok(tool_ok(json!({
            "diff": "",
            "files": [],
            "truncated": false,
            "warnings": ["not a git repository"]
        })));
    }

    let mut chunks = Vec::new();
    if unstaged {
        chunks.push(run_git_diff(ws.root(), context, &path_filters, false)?);
    }
    if staged {
        chunks.push(run_git_diff(ws.root(), context, &path_filters, true)?);
    }
    let mut combined = chunks.join("\n");
    if !combined.is_empty() && !combined.ends_with('\n') {
        combined.push('\n');
    }
    let truncated = combined.len() > max_bytes;
    let diff_text = if truncated {
        String::from_utf8_lossy(&combined.as_bytes()[..max_bytes]).into_owned()
    } else {
        combined
    };
    let files = parse_diff_files(&diff_text);
    Ok(tool_ok(json!({
        "diff": diff_text,
        "files": files,
        "truncated": truncated,
        "warnings": if truncated { vec!["diff truncated"] } else { vec![] }
    })))
}

pub fn git_log(ws: &Workspace, args: &Value) -> Result<Value, WorkspaceError> {
    let path = args.get("path").and_then(Value::as_str).unwrap_or(".");
    let resolved = ws.resolve_existing(path)?;
    let ref_name = validate_git_ref(args.get("ref").and_then(Value::as_str).unwrap_or("HEAD"))?;
    let max_count = args
        .get("max_count")
        .and_then(Value::as_u64)
        .unwrap_or(20)
        .clamp(1, 100) as usize;
    let skip = args
        .get("skip")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(10_000) as usize;

    if !is_git_repo(ws.root()) {
        return Ok(tool_ok(json!({
            "is_repo": false,
            "commits": [],
            "truncated": false,
            "warnings": []
        })));
    }

    let max_count_arg = format!("--max-count={}", max_count + 1);
    let skip_arg = format!("--skip={skip}");
    let pretty = "--pretty=format:%H%x1f%h%x1f%an%x1f%ae%x1f%ad%x1f%s%x1e";
    let path_filter = if resolved.display.is_empty() {
        ".".to_string()
    } else {
        resolved.display.clone()
    };
    let mut cmd_args = vec![
        "log",
        max_count_arg.as_str(),
        skip_arg.as_str(),
        "--date=iso-strict",
        pretty,
        ref_name,
    ];
    if path_filter != "." {
        cmd_args.push("--");
        cmd_args.push(path_filter.as_str());
    }

    let completed = run_git(ws.root(), &cmd_args, Duration::from_secs(10))?;
    if !completed.success {
        return Err(git_error(&completed.stderr));
    }

    let mut commits = Vec::new();
    for record in completed.stdout.split('\u{1e}') {
        let fields: Vec<String> = record
            .trim()
            .split('\u{1f}')
            .map(str::trim)
            .map(str::to_string)
            .collect();
        if fields.len() < 6 || fields[0].is_empty() {
            continue;
        }
        commits.push(json!({
            "hash": fields[0],
            "short_hash": fields[1],
            "author_name": fields[2],
            "author_email": fields[3],
            "author_date": fields[4],
            "subject": fields[5],
        }));
    }
    let truncated = commits.len() > max_count;
    Ok(tool_ok(json!({
        "is_repo": true,
        "ref": ref_name,
        "path": path_filter,
        "commits": commits.into_iter().take(max_count).collect::<Vec<_>>(),
        "truncated": truncated,
        "warnings": if truncated { vec!["commit limit reached"] } else { Vec::<&str>::new() }
    })))
}

pub fn git_show(ws: &Workspace, args: &Value) -> Result<Value, WorkspaceError> {
    if !is_git_repo(ws.root()) {
        return Ok(tool_ok(json!({
            "is_repo": false,
            "content": "",
            "files": [],
            "truncated": false,
            "warnings": []
        })));
    }

    let rev = validate_git_ref(args.get("rev").and_then(Value::as_str).unwrap_or("HEAD"))?;
    let context = args
        .get("context_lines")
        .and_then(Value::as_u64)
        .unwrap_or(3);
    let max_bytes = args
        .get("max_bytes")
        .and_then(Value::as_u64)
        .unwrap_or(262_144) as usize;
    let include_diff = args
        .get("include_diff")
        .and_then(Value::as_bool)
        .unwrap_or(true);

    let mut path_filters: Vec<String> = Vec::new();
    if let Some(p) = args.get("path").and_then(Value::as_str) {
        path_filters.push(p.to_string());
    }
    if let Some(paths) = args.get("paths").and_then(Value::as_array) {
        for p in paths {
            if let Some(s) = p.as_str() {
                path_filters.push(s.to_string());
            }
        }
    }
    for p in &path_filters {
        ws.reject_unsafe_text(p)?;
    }

    let unified = format!("--unified={context}");
    let mut cmd_args = vec!["show", "--no-ext-diff", "--format=fuller", unified.as_str()];
    if !include_diff {
        cmd_args.push("--no-patch");
    }
    cmd_args.push(rev);
    if !path_filters.is_empty() {
        cmd_args.push("--");
        for p in &path_filters {
            cmd_args.push(p.as_str());
        }
    }

    let completed = run_git(ws.root(), &cmd_args, Duration::from_secs(10))?;
    if !completed.success {
        return Err(git_error(&completed.stderr));
    }

    let truncated = completed.stdout.len() > max_bytes;
    let content = if truncated {
        String::from_utf8_lossy(&completed.stdout.as_bytes()[..max_bytes]).into_owned()
    } else {
        completed.stdout.clone()
    };
    let files = parse_diff_files(&content);
    Ok(tool_ok(json!({
        "is_repo": true,
        "rev": rev,
        "content": content,
        "files": files,
        "truncated": truncated,
        "output_bytes": content.len(),
        "warnings": if truncated { vec!["output truncated"] } else { Vec::<&str>::new() }
    })))
}

pub fn git_blame(ws: &Workspace, args: &Value) -> Result<Value, WorkspaceError> {
    let path = args
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| WorkspaceError::invalid_argument("path is required"))?;
    let resolved = ws.resolve_existing(path)?;
    if resolved.path.is_dir() {
        return Err(WorkspaceError::Tool {
            code: "IS_DIRECTORY",
            message: "Path is a directory.".into(),
            category: "validation",
            retryable: false,
        });
    }
    if !is_git_repo(ws.root()) {
        return Ok(tool_ok(json!({
            "is_repo": false,
            "path": resolved.display,
            "lines": [],
            "truncated": false,
            "warnings": []
        })));
    }

    let ref_arg = args.get("rev").and_then(Value::as_str);
    let git_ref = ref_arg.map(validate_git_ref).transpose()?;
    let start_line = args
        .get("start_line")
        .and_then(Value::as_u64)
        .unwrap_or(1)
        .max(1) as usize;
    let end_line_arg = args
        .get("end_line")
        .and_then(Value::as_u64)
        .map(|v| v as usize);
    let max_lines = args
        .get("max_lines")
        .and_then(Value::as_u64)
        .unwrap_or(200)
        .clamp(1, 1000) as usize;

    let final_line = match end_line_arg {
        None => start_line + max_lines - 1,
        Some(end) if end < start_line => {
            return Err(WorkspaceError::invalid_argument(
                "end_line must be >= start_line.",
            ));
        }
        Some(end) => end,
    };
    let requested_lines = final_line - start_line + 1;
    let mut truncated = requested_lines > max_lines;
    let final_line = final_line.min(start_line + max_lines - 1);

    let line_range = format!("{start_line},{final_line}");
    let mut cmd_args = vec!["blame", "--line-porcelain", "-L", line_range.as_str()];
    if let Some(r) = git_ref {
        cmd_args.push(r);
    }
    cmd_args.push("--");
    cmd_args.push(resolved.display.as_str());

    let completed = run_git(ws.root(), &cmd_args, Duration::from_secs(10))?;
    if !completed.success {
        return Err(git_error(&completed.stderr));
    }

    let mut lines = parse_git_blame_porcelain(&completed.stdout);
    if lines.len() > max_lines {
        lines.truncate(max_lines);
        truncated = true;
    }

    Ok(tool_ok(json!({
        "is_repo": true,
        "path": resolved.display,
        "rev": ref_arg,
        "start_line": start_line,
        "end_line": final_line,
        "lines": lines,
        "truncated": truncated,
        "warnings": if truncated { vec!["line limit reached"] } else { Vec::<&str>::new() }
    })))
}

fn validate_git_ref(ref_name: &str) -> Result<&str, WorkspaceError> {
    if ref_name.is_empty()
        || ref_name.starts_with('-')
        || ref_name.contains('\0')
        || ref_name.contains('\n')
        || ref_name.contains('\r')
    {
        return Err(WorkspaceError::invalid_argument("Invalid git revision."));
    }
    Ok(ref_name)
}

fn parse_git_blame_porcelain(output: &str) -> Vec<Value> {
    let commit_re = Regex::new(r"^[0-9a-fA-F^]{40}").expect("valid regex");
    let mut rows = Vec::new();
    let mut current: serde_json::Map<String, Value> = serde_json::Map::new();

    for raw in output.lines() {
        let parts: Vec<&str> = raw.split_whitespace().collect();
        if parts.len() >= 3 && commit_re.is_match(parts[0]) {
            current = serde_json::Map::new();
            current.insert("commit".into(), json!(parts[0].trim_start_matches('^')));
            if parts[1].chars().all(|c| c.is_ascii_digit()) {
                current.insert("original_line".into(), json!(parts[1].parse::<i64>().ok()));
            }
            if parts[2].chars().all(|c| c.is_ascii_digit()) {
                current.insert("line".into(), json!(parts[2].parse::<i64>().ok()));
            }
            continue;
        }
        if let Some(author) = raw.strip_prefix("author ") {
            current.insert("author".into(), json!(author));
            continue;
        }
        if let Some(mail) = raw.strip_prefix("author-mail ") {
            current.insert(
                "author_mail".into(),
                json!(mail.trim_matches(|c| c == '<' || c == '>')),
            );
            continue;
        }
        if let Some(time) = raw.strip_prefix("author-time ") {
            let value = if time.chars().all(|c| c.is_ascii_digit()) {
                json!(time.parse::<i64>().ok())
            } else {
                json!(time)
            };
            current.insert("author_time".into(), value);
            continue;
        }
        if let Some(summary) = raw.strip_prefix("summary ") {
            current.insert("summary".into(), json!(summary));
            continue;
        }
        if let Some(content) = raw.strip_prefix('\t') {
            let mut row = current.clone();
            row.insert("content".into(), json!(content));
            rows.push(Value::Object(row));
        }
    }
    rows
}

#[derive(Debug)]
struct GitOutput {
    success: bool,
    exit_code: i32,
    stdout: String,
    stderr: String,
}

fn run_git(
    cwd: &std::path::Path,
    args: &[&str],
    limit: Duration,
) -> Result<GitOutput, WorkspaceError> {
    let mut process_args = vec!["-C".to_string(), cwd.display().to_string()];
    process_args.extend(args.iter().map(|arg| (*arg).to_string()));
    run_process_with_timeout("git", cwd, &process_args, limit)
}

fn run_git_owned(
    cwd: &std::path::Path,
    args: &[String],
    limit: Duration,
) -> Result<GitOutput, WorkspaceError> {
    let args = args.iter().map(String::as_str).collect::<Vec<_>>();
    run_git(cwd, &args, limit)
}

fn run_process_with_timeout(
    program: &str,
    cwd: &std::path::Path,
    args: &[String],
    limit: Duration,
) -> Result<GitOutput, WorkspaceError> {
    let output = crate::async_runtime::block_on(async {
        let mut cmd = Command::new(program);
        crate::platform::hide_tokio_console(&mut cmd);
        cmd.current_dir(cwd)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        match tokio::time::timeout(limit, cmd.output()).await {
            Ok(Ok(output)) => Ok(output),
            Ok(Err(error)) => Err(git_error(&format!("git not available: {error}"))),
            Err(_) => Err(WorkspaceError::ToolDetails {
                code: "GIT_TIMEOUT",
                message: format!(
                    "Git command timed out after {} seconds",
                    limit.as_secs_f64()
                ),
                category: "runtime",
                retryable: true,
                details: json!({
                    "termination_reason": "timeout",
                    "timeout_ms": limit.as_millis(),
                    "recoverable": true
                }),
            }),
        }
    })?;
    Ok(GitOutput {
        success: output.status.success(),
        exit_code: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

fn run_git_diff(
    root: &std::path::Path,
    context: u64,
    path_filters: &[String],
    cached: bool,
) -> Result<String, WorkspaceError> {
    let unified = format!("--unified={context}");
    let mut args = vec!["diff", unified.as_str()];
    if cached {
        args.push("--cached");
    }
    if !path_filters.is_empty() {
        args.push("--");
        for p in path_filters {
            args.push(p.as_str());
        }
    }
    let completed = run_git(root, &args, Duration::from_secs(10))?;
    if completed.exit_code != 0 && completed.exit_code != 1 {
        return Err(git_error(&completed.stderr));
    }
    Ok(completed.stdout)
}

fn is_git_repo(root: &std::path::Path) -> bool {
    run_git(root, &["rev-parse", "--git-dir"], Duration::from_secs(5))
        .map(|o| o.success)
        .unwrap_or(false)
}

fn git_rev_parse(cwd: &std::path::Path, rev: &str) -> Option<String> {
    run_git(cwd, &["rev-parse", rev], Duration::from_secs(5))
        .ok()
        .filter(|o| o.success)
        .map(|o| o.stdout.trim().to_string())
}

fn parse_branch_line(line: &str) -> (String, String, i64, i64) {
    let (branch_part, tracking) = line
        .split_once("...")
        .map(|(b, t)| (b.to_string(), t.to_string()))
        .unwrap_or((line.to_string(), String::new()));
    let branch = branch_part
        .split_once(' ')
        .map(|(b, _)| b.to_string())
        .unwrap_or(branch_part);
    let mut ahead = 0i64;
    let mut behind = 0i64;
    let mut upstream = tracking.clone();
    if let Some(idx) = tracking.find(' ') {
        upstream = tracking[..idx].to_string();
        let meta = tracking[idx + 1..].trim_matches(['[', ']']);
        for token in meta.split(',') {
            let token = token.trim();
            if let Some(n) = token.strip_prefix("ahead ") {
                ahead = n.trim().parse().unwrap_or(0);
            } else if let Some(n) = token.strip_prefix("behind ") {
                behind = n.trim().parse().unwrap_or(0);
            }
        }
    }
    (branch, upstream, ahead, behind)
}

fn git_ahead_behind(cwd: &std::path::Path, upstream: &str) -> Result<(i64, i64), String> {
    let range = format!("HEAD...{upstream}");
    let completed = run_git(
        cwd,
        &["rev-list", "--left-right", "--count", range.as_str()],
        Duration::from_secs(10),
    )
    .map_err(|error| error.message())?;
    if !completed.success {
        return Err(format!(
            "failed to compute ahead/behind against {upstream}: {}",
            completed.stderr.trim()
        ));
    }
    let mut counts = completed.stdout.split_whitespace();
    let ahead = counts
        .next()
        .and_then(|value| value.parse::<i64>().ok())
        .ok_or_else(|| format!("invalid ahead/behind output for {upstream}"))?;
    let behind = counts
        .next()
        .and_then(|value| value.parse::<i64>().ok())
        .ok_or_else(|| format!("invalid ahead/behind output for {upstream}"))?;
    Ok((ahead, behind))
}

fn parse_diff_files(diff: &str) -> Vec<Value> {
    let mut files = Vec::new();
    for line in diff.lines() {
        if let Some(path) = line.strip_prefix("+++ b/") {
            files.push(json!({
                "path": path,
                "status": "modified",
                "binary": false
            }));
        } else if line.starts_with("--- /dev/null") {
            continue;
        } else if let Some(path) = line.strip_prefix("--- a/") {
            if !files.iter().any(|f| f["path"] == path) {
                files.push(json!({
                    "path": path,
                    "status": "modified",
                    "binary": false
                }));
            }
        }
    }
    files
}

fn git_error(message: &str) -> WorkspaceError {
    WorkspaceError::Tool {
        code: "GIT_ERROR",
        message: message.to_string(),
        category: "runtime",
        retryable: false,
    }
}
