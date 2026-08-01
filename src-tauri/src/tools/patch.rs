use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use serde_json::{json, Value};
use uuid::Uuid;

use crate::tools::context::ToolContext;
use crate::tools::workspace::{tool_ok, Workspace, WorkspaceError};

pub fn apply_patch(ctx: &ToolContext, args: &Value) -> Result<Value, WorkspaceError> {
    let ws = &ctx.workspace;
    let patch = args
        .get("patch")
        .and_then(Value::as_str)
        .ok_or_else(|| WorkspaceError::invalid_argument("patch is required"))?;
    let dry_run = args
        .get("dry_run")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let diagnostic_mode = args.get("mode").and_then(Value::as_str).unwrap_or("exact");
    let validation_mode = args
        .get("validation_mode")
        .and_then(Value::as_str)
        .unwrap_or("syntax");
    if !matches!(diagnostic_mode, "exact" | "fuzzy") {
        return Err(WorkspaceError::invalid_argument(
            "mode must be exact or fuzzy",
        ));
    }
    let file_patches = parse_unified_diff(patch)?;
    if file_patches.is_empty() {
        return Err(patch_failed("No files were modified."));
    }
    if let Some(path) = file_patches
        .iter()
        .find(|file| is_protected_repository_asset(&file.path))
        .map(|file| file.path.as_str())
    {
        return Err(protected_repository_asset(format!(
            "禁止删除仓库保护资产: {path}"
        )));
    }
    if !ctx.policy.skip_permission_gates() {
        if let Some(path) = file_patches
            .iter()
            .find(|file| file.is_deleted && is_critical_file(&file.path))
            .map(|file| file.path.as_str())
        {
            return Err(dangerous_operation(format!(
                "删除关键项目文件需要操作者在受信任控制面启用 dangerous 权限模式: {path}"
            )));
        }
    }

    let mut affected = Vec::new();
    let mut summaries = Vec::new();
    let mut hunk_matches = Vec::new();
    let mut staged: HashMap<String, Option<String>> = HashMap::new();

    for fp in &file_patches {
        ws.reject_unsafe_text(&fp.path)?;
        let resolved = if fp.is_new_file {
            ws.resolve_for_write(&fp.path)?
        } else {
            ws.resolve_existing(&fp.path)?
        };
        ws.reject_write_symlink(&fp.path)?;

        let original = if fp.is_new_file {
            // An Add File envelope is replacement content even when an earlier
            // Delete File for the same path exists in this transaction.
            String::new()
        } else if resolved.existed {
            fs::read_to_string(&resolved.path)
                .map_err(|_| WorkspaceError::not_found(format!("File not found: {}", fp.path)))?
        } else if fp.is_new_file || fp.is_deleted {
            String::new()
        } else {
            return Err(patch_failed(format!("File not found: {}", fp.path)));
        };

        if fp.is_deleted {
            staged.insert(resolved.display.clone(), None);
            affected.push(json!({ "path": resolved.display, "operation": "delete" }));
            summaries.push(format!("D {}", resolved.display));
            continue;
        }

        let (updated, matches) =
            apply_hunks(&resolved.display, &original, &fp.hunks, diagnostic_mode)?;
        hunk_matches.extend(matches);
        let op = if resolved.existed { "update" } else { "add" };
        staged.insert(resolved.display.clone(), Some(updated));
        affected.push(json!({ "path": resolved.display, "operation": op }));
        summaries.push(format!(
            "{} {}",
            if op == "add" { "A" } else { "M" },
            resolved.display
        ));
    }

    let files_created = affected_paths(&affected, "add");
    let files_modified = affected_paths(&affected, "update");
    let files_deleted = affected_paths(&affected, "delete");
    let post_validation = validate_staged_post_images(&staged, validation_mode)?;

    if !dry_run {
        let _transaction_backups = commit_staged(ws, &staged)?;
        let change_id = Uuid::new_v4().simple().to_string();
        return Ok(tool_ok(json!({
            "dry_run": false,
            "clean": true,
            "change_id": change_id,
            "summary": summaries.join("\n"),
            "affected_files": affected,
            "files_created": files_created,
            "files_modified": files_modified,
            "files_deleted": files_deleted,
            "post_validation": post_validation,
            "hunk_matches": hunk_matches,
            "transaction": {
                "committed": true,
                "atomic": true,
                "created": files_created,
                "modified": files_modified,
                "deleted": files_deleted,
                "renamed": []
            },
            "recovery": "git",
            "warnings": []
        })));
    }

    Ok(tool_ok(json!({
        "dry_run": true,
        "preflight": true,
        "clean": true,
        "summary": summaries.join("\n"),
        "affected_files": affected,
        "would_create": files_created,
        "would_modify": files_modified,
        "would_delete": files_deleted,
        "post_validation": post_validation,
        "hunk_matches": hunk_matches,
        "transaction": {
            "committed": false,
            "atomic": true,
            "created": files_created,
            "modified": files_modified,
            "deleted": files_deleted,
            "renamed": []
        },
        "warnings": []
    })))
}

fn validation_failure(
    path: &str,
    validator: &str,
    message: String,
    line: usize,
    column: usize,
    content: &str,
) -> Value {
    json!({
        "path": path,
        "validator": validator,
        "status": "failed",
        "failure_kind": "target_file_syntax",
        "message": message,
        "line": line.max(1),
        "column": column.max(1),
        "snippet": validation_snippet(content, line.max(1)),
        "workspace_modified": false
    })
}

fn balanced_error_position(content: &str, message: &str) -> (usize, usize) {
    let character = message
        .split_once("character ")
        .and_then(|(_, value)| value.split_whitespace().next())
        .and_then(|value| {
            value
                .trim_end_matches(|ch: char| !ch.is_ascii_digit())
                .parse::<usize>()
                .ok()
        })
        .unwrap_or_else(|| content.chars().count().saturating_add(1));
    line_column_for_character(content, character.saturating_sub(1))
}

fn line_column_for_character(content: &str, character: usize) -> (usize, usize) {
    let mut line = 1usize;
    let mut column = 1usize;
    for (index, ch) in content.chars().enumerate() {
        if index >= character {
            break;
        }
        if ch == '\n' {
            line += 1;
            column = 1;
        } else {
            column += 1;
        }
    }
    (line, column)
}

fn validation_snippet(content: &str, line: usize) -> String {
    let lines = content.lines().collect::<Vec<_>>();
    if lines.is_empty() {
        return String::new();
    }
    let start = line.saturating_sub(2);
    let end = (line + 1).min(lines.len());
    lines[start..end]
        .iter()
        .enumerate()
        .map(|(offset, value)| {
            let line_number = start + offset + 1;
            let value = value.chars().take(240).collect::<String>();
            format!("{line_number:>5} | {value}")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn validate_staged_post_images(
    staged: &HashMap<String, Option<String>>,
    mode: &str,
) -> Result<Vec<Value>, WorkspaceError> {
    if mode == "none" {
        return Ok(Vec::new());
    }
    let mut paths = staged.keys().cloned().collect::<Vec<_>>();
    paths.sort();
    let mut results = Vec::new();
    for path in paths {
        let Some(content) = staged.get(&path).and_then(Option::as_ref) else {
            continue;
        };
        let extension = PathBuf::from(&path)
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        let validation = match extension.as_str() {
            "json" => serde_json::from_str::<Value>(content)
                .map(|_| json!({"path": path, "validator": "json", "status": "passed"}))
                .map_err(|error| {
                    validation_failure(
                        &path,
                        "json",
                        error.to_string(),
                        error.line(),
                        error.column(),
                        content,
                    )
                }),
            "yaml" | "yml" => serde_yaml::from_str::<Value>(content)
                .map(|_| json!({"path": path, "validator": "yaml", "status": "passed"}))
                .map_err(|error| {
                    let location = error.location();
                    validation_failure(
                        &path,
                        "yaml",
                        error.to_string(),
                        location.as_ref().map(|value| value.line()).unwrap_or(1),
                        location.as_ref().map(|value| value.column()).unwrap_or(1),
                        content,
                    )
                }),
            "rs" | "ts" | "tsx" | "js" | "jsx" | "svelte" => {
                validate_balanced_structure(content, extension == "rs")
                    .map(|_| {
                        json!({
                            "path": path,
                            "validator": "balanced_structure",
                            "status": "passed"
                        })
                    })
                    .map_err(|error| {
                        let (line, column) = balanced_error_position(content, &error);
                        validation_failure(
                            &path,
                            "balanced_structure",
                            error,
                            line,
                            column,
                            content,
                        )
                    })
            }
            _ => {
                results.push(json!({
                    "path": path,
                    "validator": "not_applicable",
                    "status": "skipped"
                }));
                continue;
            }
        };
        match validation {
            Ok(result) => results.push(result),
            Err(diagnostic) => results.push(diagnostic),
        }
    }
    if results
        .iter()
        .any(|result| result.get("status").and_then(Value::as_str) == Some("failed"))
    {
        return Err(WorkspaceError::ToolDetails {
            code: "PATCH_POST_VALIDATION_FAILED",
            message: "Patched file images failed syntax or structural validation before write."
                .into(),
            category: "validation",
            retryable: true,
            details: json!({
                "validation_mode": mode,
                "failure_kind": "target_file_syntax",
                "post_validation": results,
                "workspace_modified": false,
                "suggestion": "修正失败文件的 patch 后重新运行 patch_check；验证发生在事务写盘前。"
            }),
        });
    }
    Ok(results)
}

fn validate_balanced_structure(content: &str, rust_lifetimes: bool) -> Result<(), String> {
    let chars = content.chars().collect::<Vec<_>>();
    let mut stack = Vec::<(char, usize)>::new();
    let mut quote = None::<char>;
    let mut escaped = false;
    let mut line_comment = false;
    let mut block_comment = false;
    let mut index = 0usize;
    while index < chars.len() {
        let ch = chars[index];
        let next = chars.get(index + 1).copied();
        if line_comment {
            if ch == '\n' {
                line_comment = false;
            }
            index += 1;
            continue;
        }
        if block_comment {
            if ch == '*' && next == Some('/') {
                block_comment = false;
                index += 2;
            } else {
                index += 1;
            }
            continue;
        }
        if let Some(active_quote) = quote {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == active_quote {
                quote = None;
            }
            index += 1;
            continue;
        }
        if ch == '/' && next == Some('/') {
            line_comment = true;
            index += 2;
            continue;
        }
        if ch == '/' && next == Some('*') {
            block_comment = true;
            index += 2;
            continue;
        }
        if ch == '\''
            && rust_lifetimes
            && next.is_some_and(|value| value.is_ascii_alphabetic() || value == '_')
            && chars.get(index + 2).copied() != Some('\'')
        {
            index += 1;
            continue;
        }
        if matches!(ch, '\'' | '"' | '`') {
            quote = Some(ch);
            index += 1;
            continue;
        }
        match ch {
            '(' | '[' | '{' => stack.push((ch, index)),
            ')' | ']' | '}' => {
                let expected = match ch {
                    ')' => '(',
                    ']' => '[',
                    _ => '{',
                };
                let Some((opened, opened_at)) = stack.pop() else {
                    return Err(format!(
                        "unexpected closing {ch} at character {}",
                        index + 1
                    ));
                };
                if opened != expected {
                    return Err(format!(
                        "closing {ch} at character {} does not match {opened} opened at character {}",
                        index + 1,
                        opened_at + 1
                    ));
                }
            }
            _ => {}
        }
        index += 1;
    }
    if let Some(active_quote) = quote {
        return Err(format!("unterminated {active_quote} string"));
    }
    if block_comment {
        return Err("unterminated block comment".into());
    }
    if let Some((opened, opened_at)) = stack.pop() {
        return Err(format!(
            "unclosed {opened} opened at character {}",
            opened_at + 1
        ));
    }
    Ok(())
}

fn hunk_context_mismatch(
    file: &str,
    hunk_index: usize,
    lines: &[String],
    expected: &[String],
    diagnostic_mode: &str,
    line_ending: &str,
) -> WorkspaceError {
    let (nearest_index, nearest_context, matched_lines) = nearest_hunk_context(lines, expected);
    let denominator = expected.len().max(1) as f64;
    let confidence = matched_lines as f64 / denominator;
    WorkspaceError::ToolDetails {
        code: "PATCH_CONTEXT_MISMATCH",
        message: format!(
            "Patch hunk {} did not match {} near line {}.",
            hunk_index + 1,
            file,
            nearest_index + 1
        ),
        category: "validation",
        retryable: true,
        details: json!({
            "file": file,
            "hunk_index": hunk_index + 1,
            "hunk_index_zero_based": hunk_index,
            "failure_code": "PATCH_CONTEXT_MISMATCH",
            "failure_kind": "patch_context",
            "validator": "patch_hunk_matcher",
            "expected_context": expected.iter().take(24).collect::<Vec<_>>(),
            "nearest_context": nearest_context,
            "actual_context": nearest_context,
            "line_hint": nearest_index + 1,
            "nearest_match_line": nearest_index + 1,
            "encoding": "utf-8",
            "line_ending": line_ending,
            "match_confidence": confidence,
            "mode": diagnostic_mode,
            "can_retry_fuzzy": diagnostic_mode == "exact" && confidence >= 0.5,
            "file_line_count": lines.len(),
            "suggested_patch": {
                "action": "regenerate_from_current_file",
                "read_tool": "read_file",
                "arguments": {
                    "path": file,
                    "start_line": nearest_index.saturating_sub(3) + 1,
                    "end_line": (nearest_index + expected.len() + 3).min(lines.len()).max(1)
                }
            },
            "suggestion": "读取 line_hint 附近的当前文件内容，基于 nearest_context 重新生成 patch；不要盲目重复同一 hunk。"
        }),
    }
}

fn nearest_hunk_context(lines: &[String], expected: &[String]) -> (usize, Vec<String>, usize) {
    if lines.is_empty() || expected.is_empty() {
        return (0, Vec::new(), 0);
    }
    let window = expected.len().min(lines.len());
    let mut best_index = 0usize;
    let mut best_score = 0usize;
    for index in 0..=lines.len().saturating_sub(window) {
        let score = lines[index..index + window]
            .iter()
            .zip(expected.iter())
            .filter(|(actual, expected)| actual == expected)
            .count();
        if score > best_score {
            best_index = index;
            best_score = score;
        }
    }
    let end = (best_index + expected.len()).min(lines.len());
    (
        best_index,
        lines[best_index..end].iter().take(24).cloned().collect(),
        best_score,
    )
}

pub fn patch_check(ctx: &ToolContext, args: &Value) -> Result<Value, WorkspaceError> {
    let mut check_args = args.clone();
    check_args["dry_run"] = Value::Bool(true);
    let mut result = apply_patch(ctx, &check_args)?;
    if let Some(object) = result.as_object_mut() {
        object.insert("preflight".into(), Value::Bool(true));
    }
    Ok(result)
}

#[derive(Debug)]
struct FilePatch {
    path: String,
    hunks: Vec<Hunk>,
    is_new_file: bool,
    is_deleted: bool,
}

#[derive(Debug)]
struct Hunk {
    lines: Vec<HunkLine>,
}

#[derive(Debug)]
enum HunkLine {
    Context(String),
    Add(String),
    Remove(String),
}

fn parse_unified_diff(patch: &str) -> Result<Vec<FilePatch>, WorkspaceError> {
    if patch
        .lines()
        .any(|line| line.trim_end_matches('\r') == "*** Begin Patch")
    {
        return parse_codex_patch(patch);
    }

    let mut files = Vec::new();
    let mut current: Option<FilePatch> = None;
    let mut current_hunk: Option<Hunk> = None;

    for line in patch.lines() {
        if line.starts_with("--- ") {
            if let Some(h) = current_hunk.take() {
                if let Some(ref mut f) = current {
                    f.hunks.push(h);
                }
            }
            if let Some(f) = current.take() {
                files.push(f);
            }
            let path = parse_diff_path(line.strip_prefix("--- ").unwrap_or(""));
            current = Some(FilePatch {
                path,
                hunks: Vec::new(),
                is_new_file: line.contains("/dev/null"),
                is_deleted: false,
            });
        } else if line.starts_with("+++ ") {
            if let Some(ref mut f) = current {
                let new_path = parse_diff_path(line.strip_prefix("+++ ").unwrap_or(""));
                if !new_path.is_empty() && new_path != "/dev/null" {
                    f.path = new_path;
                }
                if line.contains("/dev/null") {
                    f.is_deleted = true;
                }
            }
        } else if line.starts_with("@@") {
            if let Some(h) = current_hunk.take() {
                if let Some(ref mut f) = current {
                    f.hunks.push(h);
                }
            }
            current_hunk = Some(Hunk { lines: Vec::new() });
        } else if let Some(ref mut hunk) = current_hunk {
            if let Some(rest) = line.strip_prefix('+') {
                hunk.lines.push(HunkLine::Add(rest.to_string()));
            } else if let Some(rest) = line.strip_prefix('-') {
                hunk.lines.push(HunkLine::Remove(rest.to_string()));
            } else if let Some(rest) = line.strip_prefix(' ') {
                hunk.lines.push(HunkLine::Context(rest.to_string()));
            } else if line.is_empty() {
                hunk.lines.push(HunkLine::Context(String::new()));
            }
        }
    }
    if let Some(h) = current_hunk.take() {
        if let Some(ref mut f) = current {
            f.hunks.push(h);
        }
    }
    if let Some(f) = current.take() {
        files.push(f);
    }
    Ok(files)
}

fn parse_codex_patch(patch: &str) -> Result<Vec<FilePatch>, WorkspaceError> {
    let mut files = Vec::new();
    let mut current: Option<FilePatch> = None;
    let mut current_hunk: Option<Hunk> = None;

    for raw_line in patch.lines() {
        let line = raw_line.trim_end_matches('\r');
        if line == "*** Begin Patch" {
            continue;
        }
        if line == "*** End Patch" {
            finish_codex_file(&mut files, &mut current, &mut current_hunk);
            continue;
        }

        let header = line
            .strip_prefix("*** Add File: ")
            .map(|path| (path, true, false))
            .or_else(|| {
                line.strip_prefix("*** Update File: ")
                    .map(|path| (path, false, false))
            })
            .or_else(|| {
                line.strip_prefix("*** Delete File: ")
                    .map(|path| (path, false, true))
            });
        if let Some((path, is_new_file, is_deleted)) = header {
            finish_codex_file(&mut files, &mut current, &mut current_hunk);
            current = Some(FilePatch {
                path: parse_diff_path(path),
                hunks: Vec::new(),
                is_new_file,
                is_deleted,
            });
            if is_new_file {
                current_hunk = Some(Hunk { lines: Vec::new() });
            }
            continue;
        }

        if line.starts_with("@@") {
            if let Some(hunk) = current_hunk.take() {
                if let Some(ref mut file) = current {
                    file.hunks.push(hunk);
                }
            }
            current_hunk = Some(Hunk { lines: Vec::new() });
            continue;
        }

        let Some(file) = current.as_ref() else {
            continue;
        };
        if file.is_deleted {
            continue;
        }
        let hunk = current_hunk.get_or_insert_with(|| Hunk { lines: Vec::new() });
        if let Some(rest) = line.strip_prefix('+') {
            hunk.lines.push(HunkLine::Add(rest.to_string()));
        } else if let Some(rest) = line.strip_prefix('-') {
            hunk.lines.push(HunkLine::Remove(rest.to_string()));
        } else if let Some(rest) = line.strip_prefix(' ') {
            hunk.lines.push(HunkLine::Context(rest.to_string()));
        } else if line.is_empty() {
            hunk.lines.push(HunkLine::Context(String::new()));
        }
    }

    finish_codex_file(&mut files, &mut current, &mut current_hunk);
    Ok(files)
}

fn finish_codex_file(
    files: &mut Vec<FilePatch>,
    current: &mut Option<FilePatch>,
    current_hunk: &mut Option<Hunk>,
) {
    if let Some(hunk) = current_hunk.take() {
        if let Some(file) = current.as_mut() {
            file.hunks.push(hunk);
        }
    }
    if let Some(file) = current.take() {
        files.push(file);
    }
}

fn affected_paths(affected: &[Value], operation: &str) -> Vec<String> {
    affected
        .iter()
        .filter(|file| file["operation"] == operation)
        .filter_map(|file| file["path"].as_str().map(str::to_string))
        .collect()
}

fn parse_diff_path(raw: &str) -> String {
    let trimmed = raw.trim();
    let path = trimmed
        .strip_prefix("a/")
        .or_else(|| trimmed.strip_prefix("b/"))
        .unwrap_or(trimmed);
    if path == "/dev/null" {
        return String::new();
    }
    path.replace('\\', "/")
}

fn apply_hunks(
    file: &str,
    original: &str,
    hunks: &[Hunk],
    diagnostic_mode: &str,
) -> Result<(String, Vec<Value>), WorkspaceError> {
    let line_ending = if original.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let had_trailing_newline = original.ends_with('\n');
    let mut lines: Vec<String> = if original.is_empty() {
        Vec::new()
    } else {
        original
            .split_terminator('\n')
            .map(|line| line.trim_end_matches('\r').to_string())
            .collect()
    };
    let mut offset: i64 = 0;
    let mut matches = Vec::new();

    for (hunk_index, hunk) in hunks.iter().enumerate() {
        let search_at = 0usize;
        let hunk_old: Vec<String> = hunk
            .lines
            .iter()
            .filter_map(|l| match l {
                HunkLine::Context(s) | HunkLine::Remove(s) => Some(s.clone()),
                HunkLine::Add(_) => None,
            })
            .collect();

        let exact = find_hunk_position(&lines, &hunk_old, search_at);
        let (pos, match_mode, confidence) = if let Some(pos) = exact {
            (pos, "exact", 1.0)
        } else if diagnostic_mode == "fuzzy" {
            find_fuzzy_hunk_position(&lines, &hunk_old, search_at).ok_or_else(|| {
                hunk_context_mismatch(
                    file,
                    hunk_index,
                    &lines,
                    &hunk_old,
                    diagnostic_mode,
                    if line_ending == "\r\n" { "crlf" } else { "lf" },
                )
            })?
        } else {
            return Err(hunk_context_mismatch(
                file,
                hunk_index,
                &lines,
                &hunk_old,
                diagnostic_mode,
                if line_ending == "\r\n" { "crlf" } else { "lf" },
            ));
        };
        matches.push(json!({
            "file": file,
            "hunk_index": hunk_index + 1,
            "line": pos + 1,
            "mode": match_mode,
            "confidence": confidence
        }));

        let mut idx = pos;
        for hl in &hunk.lines {
            match hl {
                HunkLine::Context(_) => idx += 1,
                HunkLine::Remove(_) => {
                    if idx < lines.len() {
                        lines.remove(idx);
                    }
                }
                HunkLine::Add(s) => {
                    lines.insert(idx, s.clone());
                    idx += 1;
                }
            }
        }
        offset += 0; // reserved for future fuzzy offset
        let _ = offset;
    }
    let mut output = lines.join(line_ending);
    if !output.is_empty() && (had_trailing_newline || original.is_empty()) {
        output.push_str(line_ending);
    }
    Ok((output, matches))
}

fn find_fuzzy_hunk_position(
    lines: &[String],
    pattern: &[String],
    start: usize,
) -> Option<(usize, &'static str, f64)> {
    if pattern.is_empty() {
        return Some((start, "fuzzy", 1.0));
    }
    if start > lines.len() || pattern.len() > lines.len().saturating_sub(start) {
        return None;
    }
    let minimum_matches = (pattern.len() * 4).div_ceil(5);
    let mut best = None::<(usize, usize)>;
    let mut best_count = 0usize;
    for index in start..=lines.len().saturating_sub(pattern.len()) {
        let score = lines[index..index + pattern.len()]
            .iter()
            .zip(pattern.iter())
            .filter(|(actual, expected)| actual.trim() == expected.trim())
            .count();
        if score > best_count {
            best = Some((index, score));
            best_count = score;
        } else if score == best_count && score > 0 {
            best = None;
        }
    }
    let (index, score) = best?;
    if score < minimum_matches || (pattern.len() == 1 && score != 1) {
        return None;
    }
    Some((index, "fuzzy", score as f64 / pattern.len() as f64))
}

fn find_hunk_position(lines: &[String], pattern: &[String], start: usize) -> Option<usize> {
    if pattern.is_empty() {
        return Some(start);
    }
    if start > lines.len() || pattern.len() > lines.len().saturating_sub(start) {
        return None;
    }
    for i in start..=lines.len().saturating_sub(pattern.len()) {
        if lines[i..i + pattern.len()]
            .iter()
            .zip(pattern.iter())
            .all(|(a, b)| a == b)
        {
            return Some(i);
        }
    }
    None
}

fn commit_staged(
    ws: &Workspace,
    staged: &HashMap<String, Option<String>>,
) -> Result<HashMap<PathBuf, Option<Vec<u8>>>, WorkspaceError> {
    let staged_bytes = staged
        .iter()
        .map(|(path, content)| {
            (
                path.clone(),
                content.as_ref().map(|value| value.as_bytes().to_vec()),
            )
        })
        .collect::<HashMap<_, _>>();
    commit_staged_bytes(ws, &staged_bytes)
}

pub(crate) fn commit_staged_bytes(
    ws: &Workspace,
    staged: &HashMap<String, Option<Vec<u8>>>,
) -> Result<HashMap<PathBuf, Option<Vec<u8>>>, WorkspaceError> {
    let mut backups: HashMap<PathBuf, Option<Vec<u8>>> = HashMap::new();
    let mut temporary_files = HashMap::new();
    for (rel, content) in staged {
        ws.reject_protected_write_path(rel)?;
        let resolved = if content.is_none() {
            ws.resolve_existing(rel)?
        } else {
            ws.resolve_for_write(rel)?
        };
        let path = resolved.path.clone();
        backups.insert(
            path.clone(),
            if path.exists() && path.is_file() {
                Some(fs::read(&path).unwrap_or_default())
            } else {
                None
            },
        );
        if let Some(bytes) = content {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).map_err(|err| patch_failed(err.to_string()))?;
            }
            let temp = path.with_file_name(format!(
                ".{}.harness-stage-{}",
                path.file_name().and_then(|v| v.to_str()).unwrap_or("file"),
                Uuid::new_v4().simple()
            ));
            if let Err(err) = fs::write(&temp, bytes) {
                cleanup_temporary_files(temporary_files.values());
                restore_backups(&backups);
                return Err(patch_failed(format!("Failed to stage file: {err}")));
            }
            temporary_files.insert(path.clone(), temp);
        }
    }

    for (rel, content) in staged {
        let resolved = if content.is_none() {
            ws.resolve_existing(rel)?
        } else {
            ws.resolve_for_write(rel)?
        };
        let path = resolved.path;
        let result = if content.is_some() {
            let temp = temporary_files
                .get(&path)
                .cloned()
                .ok_or_else(|| patch_failed("Staged file is missing"));
            match temp {
                Ok(temp) => replace_file(&temp, &path),
                Err(error) => Err(std::io::Error::other(error.to_string())),
            }
        } else if path.exists() && path.is_file() {
            fs::remove_file(&path)
        } else {
            Ok(())
        };
        if let Err(err) = result {
            cleanup_temporary_files(temporary_files.values());
            restore_backups(&backups);
            return Err(patch_failed(format!("Failed to write file: {err}")));
        }
    }
    cleanup_temporary_files(temporary_files.values());
    Ok(backups)
}

fn restore_backups(backups: &HashMap<PathBuf, Option<Vec<u8>>>) {
    for (path, data) in backups {
        match data {
            None => {
                let _ = fs::remove_file(path);
            }
            Some(bytes) => {
                if let Some(parent) = path.parent() {
                    let _ = fs::create_dir_all(parent);
                }
                let _ = fs::write(path, bytes);
            }
        }
    }
}

fn replace_file(temp: &PathBuf, path: &PathBuf) -> Result<(), std::io::Error> {
    #[cfg(windows)]
    {
        if path.exists() {
            fs::remove_file(path)?;
        }
    }
    fs::rename(temp, path)
}

fn cleanup_temporary_files<'a>(paths: impl Iterator<Item = &'a PathBuf>) {
    for path in paths {
        let _ = fs::remove_file(path);
    }
}

fn is_critical_file(path: &str) -> bool {
    let normalized = path.replace('\\', "/");
    let first = normalized.split('/').next().unwrap_or("");
    if matches!(first, ".git" | ".github") {
        return true;
    }
    let name = normalized.rsplit('/').next().unwrap_or(normalized.as_str());
    name == ".gitignore"
        || name == "Cargo.toml"
        || name == "Cargo.lock"
        || name == "package.json"
        || name == "package-lock.json"
        || name == "pnpm-lock.yaml"
        || name == "tauri.conf.json"
        || name.starts_with("README")
        || name.starts_with("LICENSE")
        || name.starts_with("vite.config.")
        || name == "pyproject.toml"
}

fn is_protected_repository_asset(path: &str) -> bool {
    let normalized = path.replace('\\', "/");
    let first = normalized.split('/').next().unwrap_or("");
    matches!(first, ".git" | ".github")
}

fn dangerous_operation(message: impl Into<String>) -> WorkspaceError {
    WorkspaceError::Tool {
        code: "DANGEROUS_OPERATION_REQUIRES_DANGEROUS_MODE",
        message: message.into(),
        category: "permission",
        retryable: false,
    }
}

fn protected_repository_asset(message: impl Into<String>) -> WorkspaceError {
    WorkspaceError::Tool {
        code: "PROTECTED_REPOSITORY_ASSET",
        message: message.into(),
        category: "security",
        retryable: false,
    }
}

fn patch_failed(message: impl Into<String>) -> WorkspaceError {
    WorkspaceError::Tool {
        code: "PATCH_FAILED",
        message: message.into(),
        category: "validation",
        retryable: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::context::ToolContext;
    use serde_json::json;
    use tempfile::tempdir;

    fn context_with_file() -> (tempfile::TempDir, tempfile::TempDir, ToolContext) {
        let workspace = tempdir().expect("workspace");
        let harness = tempdir().expect("harness");
        std::fs::write(workspace.path().join("main.rs"), "old\n").expect("file");
        let context =
            ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                .expect("context");
        (workspace, harness, context)
    }

    fn patch() -> Value {
        json!({
            "patch": "--- a/main.rs\n+++ b/main.rs\n@@\n-old\n+new\n"
        })
    }

    #[test]
    fn patch_check_does_not_modify_workspace() {
        let (_workspace, _harness, context) = context_with_file();
        let result = patch_check(&context, &patch()).expect("patch check");
        assert_eq!(result["preflight"], true);
        assert_eq!(
            std::fs::read_to_string(context.workspace.root().join("main.rs")).unwrap(),
            "old\n"
        );
    }

    #[test]
    fn fuzzy_mode_accepts_a_unique_whitespace_only_context_drift() {
        let input = "fn main() {\n    let value = 1;\n}\n";
        let hunk = Hunk {
            lines: vec![
                HunkLine::Context("fn main() {".into()),
                HunkLine::Remove("let value = 1;".into()),
                HunkLine::Add("let value = 2;".into()),
                HunkLine::Context("}".into()),
            ],
        };
        let (updated, matches) =
            apply_hunks("main.rs", input, &[hunk], "fuzzy").expect("fuzzy patch");
        assert_eq!(updated, "fn main() {\nlet value = 2;\n}\n");
        assert_eq!(matches[0]["mode"], "fuzzy");
        assert_eq!(matches[0]["hunk_index"], 1);
    }

    #[test]
    fn exact_context_mismatch_reports_retry_diagnostics() {
        let input = "alpha\nactual\nomega\n";
        let hunk = Hunk {
            lines: vec![
                HunkLine::Context("alpha".into()),
                HunkLine::Remove("expected".into()),
                HunkLine::Add("replacement".into()),
                HunkLine::Context("omega".into()),
            ],
        };
        let error = apply_hunks("main.txt", input, &[hunk], "exact").expect_err("exact mismatch");
        let value = error.to_error_value();
        assert_eq!(value["code"], "PATCH_CONTEXT_MISMATCH");
        assert_eq!(value["details"]["file"], "main.txt");
        assert_eq!(value["details"]["hunk_index"], 1);
        assert_eq!(value["details"]["nearest_match_line"], 1);
        assert_eq!(value["details"]["can_retry_fuzzy"], true);
    }

    #[test]
    fn preserves_crlf_when_inserting_multiple_lines() {
        let input = "one\r\ntwo\r\n";
        let hunk = Hunk {
            lines: vec![
                HunkLine::Context("one".into()),
                HunkLine::Add("insert-a".into()),
                HunkLine::Add("insert-b".into()),
                HunkLine::Context("two".into()),
            ],
        };
        assert_eq!(
            apply_hunks("main.txt", input, &[hunk], "exact")
                .expect("patch")
                .0,
            "one\r\ninsert-a\r\ninsert-b\r\ntwo\r\n"
        );
    }

    #[test]
    fn delete_then_add_same_path_replaces_instead_of_concatenating_old_content() {
        let (_workspace, _harness, context) = context_with_file();
        let result = apply_patch(
            &context,
            &json!({
                "patch": "*** Begin Patch\n*** Delete File: main.rs\n*** Add File: main.rs\n+fresh\n*** End Patch\n"
            }),
        )
        .expect("replace file");
        assert_eq!(result["files_modified"], json!(["main.rs"]));
        assert_eq!(
            std::fs::read_to_string(context.workspace.root().join("main.rs")).unwrap(),
            "fresh\n"
        );
    }

    #[test]
    fn validation_failure_in_later_file_keeps_all_files_unchanged() {
        let (_workspace, _harness, context) = context_with_file();
        let error = apply_patch(
            &context,
            &json!({
                "patch": "--- a/main.rs\n+++ b/main.rs\n@@\n-old\n+new\n--- a/missing.rs\n+++ b/missing.rs\n@@\n-old\n+new\n"
            }),
        )
        .expect_err("later file fails preflight");
        assert_eq!(error.to_error_value()["code"], "NOT_FOUND");
        assert_eq!(
            std::fs::read_to_string(context.workspace.root().join("main.rs")).unwrap(),
            "old\n"
        );
    }

    #[test]
    fn invalid_json_post_image_is_rejected_before_write() {
        let (_workspace, _harness, context) = context_with_file();
        std::fs::write(
            context.workspace.root().join("config.json"),
            "{\"ok\": true}\n",
        )
        .expect("config");
        let error = apply_patch(
            &context,
            &json!({
                "patch": "--- a/config.json\n+++ b/config.json\n@@\n-{\"ok\": true}\n+{\"ok\": true\n"
            }),
        )
        .expect_err("invalid JSON must fail before write");
        assert_eq!(
            error.to_error_value()["code"],
            "PATCH_POST_VALIDATION_FAILED"
        );
        let value = error.to_error_value();
        let diagnostic = &value["details"]["post_validation"][0];
        assert_eq!(diagnostic["path"], "config.json");
        assert_eq!(diagnostic["validator"], "json");
        assert_eq!(diagnostic["failure_kind"], "target_file_syntax");
        assert!(diagnostic["line"].as_u64().unwrap_or_default() >= 1);
        assert!(diagnostic["column"].as_u64().unwrap_or_default() >= 1);
        assert!(diagnostic["snippet"]
            .as_str()
            .unwrap_or_default()
            .contains("1 |"));
        assert_eq!(
            std::fs::read_to_string(context.workspace.root().join("config.json")).unwrap(),
            "{\"ok\": true}\n"
        );
    }

    #[test]
    fn balanced_structure_handles_rust_lifetimes_and_detects_unclosed_blocks() {
        validate_balanced_structure("fn borrow<'a>(value: &'a str) -> &'a str { value }", true)
            .expect("Rust lifetime is not a quote");
        assert!(validate_balanced_structure("export const broken = {", false).is_err());
    }

    #[test]
    fn balanced_structure_diagnostic_reports_location_and_snippet() {
        let content = "const ok = true;\nconst broken = 'value;\n";
        let message = validate_balanced_structure(content, false).expect_err("unclosed quote");
        let (line, column) = balanced_error_position(content, &message);
        let diagnostic = validation_failure(
            "example.ts",
            "balanced_structure",
            message,
            line,
            column,
            content,
        );
        assert_eq!(diagnostic["failure_kind"], "target_file_syntax");
        assert!(diagnostic["line"].as_u64().unwrap_or_default() >= 2);
        assert!(diagnostic["column"].as_u64().unwrap_or_default() >= 1);
        assert!(diagnostic["snippet"]
            .as_str()
            .unwrap_or_default()
            .contains("const broken"));
    }
}
