use std::fs;
use std::path::Path;

use serde_json::{json, Value};

use crate::tools::context::ToolContext;
use crate::tools::workspace::{is_link_like, tool_ok, WorkspaceError};

pub fn remove_path(ctx: &ToolContext, args: &Value) -> Result<Value, WorkspaceError> {
    let raw_path = args
        .get("path")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .ok_or_else(|| WorkspaceError::invalid_argument("path is required"))?;
    let recursive = args
        .get("recursive")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let resolved = ctx.workspace.resolve_lexical_write_path(raw_path)?;
    let metadata = fs::symlink_metadata(&resolved.path)
        .map_err(|_| WorkspaceError::not_found(format!("Path not found: {raw_path}")))?;
    let link_like = is_link_like(&resolved.path);
    let kind = if link_like {
        "symlink_or_junction"
    } else if metadata.is_dir() {
        "directory"
    } else {
        "file"
    };

    if link_like {
        remove_link_entry(&resolved.path, metadata.is_dir())?;
    } else if metadata.is_dir() {
        if recursive {
            if !ctx.policy.skip_permission_gates() {
                return Err(WorkspaceError::ToolDetails {
                    code: "DANGEROUS_OPERATION_REQUIRES_DANGEROUS_MODE",
                    message:
                        "Recursive directory removal requires operator-enabled dangerous mode."
                            .into(),
                    category: "permission",
                    retryable: false,
                    details: json!({
                        "path": resolved.display,
                        "recursive": true,
                        "recoverable": true,
                        "suggestion": "Enable dangerous mode in the trusted control plane, or remove children explicitly."
                    }),
                });
            }
            fs::remove_dir_all(&resolved.path).map_err(|error| remove_error(raw_path, error))?;
        } else {
            fs::remove_dir(&resolved.path).map_err(|error| remove_error(raw_path, error))?;
        }
    } else {
        fs::remove_file(&resolved.path).map_err(|error| remove_error(raw_path, error))?;
    }

    Ok(tool_ok(json!({
        "path": resolved.display,
        "kind": kind,
        "recursive": recursive,
        "link_like": link_like,
        "target_preserved": link_like,
        "affected_files": [{"path": resolved.display, "operation": "delete"}],
        "mutation_attributed": true,
        "warnings": []
    })))
}

fn remove_link_entry(path: &Path, directory_hint: bool) -> Result<(), WorkspaceError> {
    let first = if directory_hint {
        fs::remove_dir(path)
    } else {
        fs::remove_file(path)
    };
    if first.is_ok() {
        return Ok(());
    }
    let second = if directory_hint {
        fs::remove_file(path)
    } else {
        fs::remove_dir(path)
    };
    second.map_err(|error| remove_error(&path.to_string_lossy(), error))
}

fn remove_error(path: &str, error: std::io::Error) -> WorkspaceError {
    WorkspaceError::ToolDetails {
        code: "REMOVE_PATH_FAILED",
        message: format!("Failed to remove {path}: {error}"),
        category: "runtime",
        retryable: false,
        details: json!({
            "path": path,
            "io_error_kind": format!("{:?}", error.kind()),
            "recoverable": true
        }),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use serde_json::json;

    use super::remove_path;
    use crate::tools::ToolContext;

    #[test]
    fn removes_link_entry_without_deleting_target() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        let target = temp.path().join("target");
        fs::create_dir_all(&workspace).expect("workspace");
        fs::create_dir_all(&target).expect("target");
        fs::write(target.join("keep.txt"), "keep\n").expect("target file");
        let link = workspace.join("linked");
        if !create_directory_link(&link, &target) {
            eprintln!("skip link removal test: platform did not allow link creation");
            return;
        }
        let ctx = ToolContext::for_test(workspace, temp.path().join("harness")).expect("context");

        let result = remove_path(&ctx, &json!({"path": "linked"})).expect("remove link");

        assert_eq!(result["link_like"], true);
        assert_eq!(result["target_preserved"], true);
        assert!(fs::symlink_metadata(&link).is_err());
        assert_eq!(
            fs::read_to_string(target.join("keep.txt")).unwrap(),
            "keep\n"
        );
    }

    #[test]
    fn removes_broken_link_without_workspace_scan() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        let missing = temp.path().join("missing-target");
        fs::create_dir_all(&workspace).expect("workspace");
        let link = workspace.join("broken");
        if !create_directory_link(&link, &missing) {
            eprintln!("skip broken link test: platform did not allow link creation");
            return;
        }
        let ctx = ToolContext::for_test(workspace, temp.path().join("harness")).expect("context");

        let result = remove_path(&ctx, &json!({"path": "broken"})).expect("remove broken link");

        assert_eq!(result["kind"], "symlink_or_junction");
        assert!(fs::symlink_metadata(&link).is_err());
    }

    fn create_directory_link(link: &Path, target: &Path) -> bool {
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(target, link).is_ok()
        }
        #[cfg(windows)]
        {
            if std::os::windows::fs::symlink_dir(target, link).is_ok() {
                return true;
            }
            std::process::Command::new("cmd.exe")
                .args(["/d", "/s", "/c", "mklink", "/J"])
                .arg(link)
                .arg(target)
                .status()
                .map(|status| status.success())
                .unwrap_or(false)
        }
    }
}
