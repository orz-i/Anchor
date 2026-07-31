use std::fs;
use std::path::{Component, Path, PathBuf};

#[cfg(windows)]
use std::os::windows::fs::MetadataExt;

use serde_json::{json, Value};
use thiserror::Error;

pub const DEFAULT_EXCLUDED_NAMES: &[&str] = &[
    ".git",
    ".reference",
    "node_modules",
    "target",
    "dist",
    "build",
    ".venv",
    "venv",
    ".tox",
    ".mypy_cache",
    ".pytest_cache",
    ".ruff_cache",
    "__pycache__",
];

const MAX_CHILD_PROCESS_BOUNDARY_ENTRIES: usize = 250_000;

#[derive(Debug, Clone)]
pub struct ResolvedPath {
    pub display: String,
    pub path: PathBuf,
    pub existed: bool,
}

#[cfg(windows)]
fn windows_hidden_attribute(path: &Path) -> bool {
    use std::os::windows::fs::MetadataExt;

    const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
    path.symlink_metadata()
        .map(|metadata| metadata.file_attributes() & FILE_ATTRIBUTE_HIDDEN != 0)
        .unwrap_or(false)
}

#[cfg(not(windows))]
fn windows_hidden_attribute(_path: &Path) -> bool {
    false
}

fn is_link_like(path: &Path) -> bool {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return false;
    };
    #[cfg(windows)]
    {
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

#[derive(Debug, Error)]
pub enum WorkspaceError {
    #[error("{message}")]
    Tool {
        code: &'static str,
        message: String,
        category: &'static str,
        retryable: bool,
    },
    #[error("{message}")]
    ToolDetails {
        code: &'static str,
        message: String,
        category: &'static str,
        retryable: bool,
        details: Value,
    },
}

impl WorkspaceError {
    pub fn message(&self) -> String {
        match self {
            Self::Tool { message, .. } | Self::ToolDetails { message, .. } => message.clone(),
        }
    }

    pub fn invalid_argument(message: impl Into<String>) -> Self {
        Self::Tool {
            code: "INVALID_ARGUMENT",
            message: message.into(),
            category: "validation",
            retryable: false,
        }
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::Tool {
            code: "NOT_FOUND",
            message: message.into(),
            category: "not_found",
            retryable: false,
        }
    }

    pub fn absolute_path_denied() -> Self {
        Self::Tool {
            code: "ABSOLUTE_PATH_DENIED",
            message: "Absolute paths are denied.".into(),
            category: "security",
            retryable: false,
        }
    }

    pub fn path_outside_workspace() -> Self {
        Self::Tool {
            code: "PATH_OUTSIDE_WORKSPACE",
            message: "Path escapes the configured workspace.".into(),
            category: "security",
            retryable: false,
        }
    }

    pub fn symlink_escape() -> Self {
        Self::Tool {
            code: "SYMLINK_ESCAPE",
            message: "Path escapes the configured workspace.".into(),
            category: "security",
            retryable: false,
        }
    }

    pub fn not_a_directory(message: impl Into<String>) -> Self {
        Self::Tool {
            code: "NOT_A_DIRECTORY",
            message: message.into(),
            category: "validation",
            retryable: false,
        }
    }

    pub fn to_error_value(&self) -> Value {
        match self {
            Self::Tool {
                code,
                message,
                category,
                retryable,
            } => json!({
                "code": code,
                "message": message,
                "category": category,
                "retryable": retryable,
                "details": {}
            }),
            Self::ToolDetails {
                code,
                message,
                category,
                retryable,
                details,
            } => json!({
                "code": code,
                "message": message,
                "category": category,
                "retryable": retryable,
                "details": details
            }),
        }
    }
}

pub type WorkspaceResult<T> = Result<T, WorkspaceError>;

#[derive(Debug, Clone)]
pub struct Workspace {
    root: PathBuf,
    allow_external_reads: bool,
}

impl Workspace {
    pub fn new(root: PathBuf) -> WorkspaceResult<Self> {
        let root = root
            .canonicalize()
            .map_err(|_| WorkspaceError::invalid_argument("Workspace root must exist"))?;
        if !root.is_dir() {
            return Err(WorkspaceError::invalid_argument(
                "Workspace root must be a directory",
            ));
        }
        Ok(Self {
            root,
            allow_external_reads: false,
        })
    }

    pub fn with_strict_read_boundary(mut self, strict: bool) -> Self {
        self.allow_external_reads = !strict;
        self
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn root_display(&self) -> String {
        self.root.to_string_lossy().into_owned()
    }

    pub fn strict_read_boundary(&self) -> bool {
        !self.allow_external_reads
    }

    pub fn reject_unsafe_text(&self, raw_path: &str) -> WorkspaceResult<()> {
        if raw_path.is_empty() {
            return Err(WorkspaceError::invalid_argument(
                "Path must be a non-empty string",
            ));
        }
        if raw_path.contains('\0') {
            return Err(WorkspaceError::invalid_argument("Path contains a NUL byte"));
        }
        if raw_path.starts_with('/') || raw_path.starts_with('\\') {
            return Err(WorkspaceError::absolute_path_denied());
        }
        if raw_path.len() >= 2 {
            let bytes = raw_path.as_bytes();
            if bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
                return Err(WorkspaceError::absolute_path_denied());
            }
        }
        for part in Path::new(raw_path).components() {
            if matches!(part, Component::ParentDir) {
                return Err(WorkspaceError::path_outside_workspace());
            }
        }
        Ok(())
    }

    pub fn resolve_existing(&self, raw_path: &str) -> WorkspaceResult<ResolvedPath> {
        self.resolve_existing_at(&self.root, raw_path)
    }

    /// Child processes are policy-limited rather than OS-sandboxed. Recursively
    /// reject symlinks or Windows junctions that resolve outside the workspace so
    /// an interpreter cannot escape through an otherwise relative path.
    pub fn ensure_child_process_boundary(&self) -> WorkspaceResult<()> {
        let mut pending = vec![self.root.clone()];
        let mut scanned = 0usize;
        while let Some(directory) = pending.pop() {
            let entries = fs::read_dir(&directory).map_err(|error| WorkspaceError::ToolDetails {
                code: "WORKSPACE_SCAN_FAILED",
                message: format!(
                    "Failed to inspect workspace boundary at {}: {error}",
                    relative_display(&self.root, &directory)
                ),
                category: "runtime",
                retryable: true,
                details: json!({
                    "stage": "workspace_boundary_scan",
                    "path": relative_display(&self.root, &directory),
                    "retryable": true
                }),
            })?;
            for entry in entries {
                let entry = entry.map_err(|error| WorkspaceError::ToolDetails {
                    code: "WORKSPACE_SCAN_FAILED",
                    message: format!("Failed to inspect workspace entry: {error}"),
                    category: "runtime",
                    retryable: true,
                    details: json!({"stage": "workspace_boundary_scan", "retryable": true}),
                })?;
                scanned = scanned.saturating_add(1);
                if scanned > MAX_CHILD_PROCESS_BOUNDARY_ENTRIES {
                    return Err(WorkspaceError::ToolDetails {
                        code: "WORKSPACE_SCAN_LIMIT_EXCEEDED",
                        message: "Workspace boundary scan exceeded its safety limit".into(),
                        category: "security",
                        retryable: false,
                        details: json!({
                            "stage": "workspace_boundary_scan",
                            "maximum_entries": MAX_CHILD_PROCESS_BOUNDARY_ENTRIES,
                            "sandbox_enforced": false,
                            "recoverable": true,
                            "suggestion": "Reduce generated dependency trees or use an OS-sandboxed execution backend"
                        }),
                    });
                }

                let path = entry.path();
                if is_link_like(&path) {
                    let link_path = relative_display(&self.root, &path);
                    let resolved = path.canonicalize().map_err(|error| WorkspaceError::ToolDetails {
                        code: "WORKSPACE_LINK_UNRESOLVED",
                        message: format!(
                            "Workspace contains an unresolved symlink/junction at {link_path}: {error}"
                        ),
                        category: "security",
                        retryable: false,
                        details: json!({
                            "stage": "workspace_boundary_scan",
                            "reason": "unresolved_workspace_link",
                            "link_path": link_path,
                            "sandbox_enforced": false,
                            "recoverable": true,
                            "suggestion": "Remove or repair the workspace-local symlink/junction"
                        }),
                    })?;
                    if !resolved.starts_with(&self.root) {
                        return Err(WorkspaceError::ToolDetails {
                            code: "WORKSPACE_LINK_ESCAPE",
                            message: format!(
                                "Workspace contains an external directory link: {link_path}. Remove the symlink/junction before running child processes."
                            ),
                            category: "security",
                            retryable: false,
                            details: json!({
                                "stage": "workspace_boundary_scan",
                                "reason": "external_workspace_link",
                                "link_path": link_path,
                                "sandbox_enforced": false,
                                "recoverable": true,
                                "suggestion": "Remove the workspace-local symlink/junction; do not delete its target directory"
                            }),
                        });
                    }
                    continue;
                }
                let is_directory = entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false);
                let generated_subtree = entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| DEFAULT_EXCLUDED_NAMES.contains(&name));
                if is_directory && generated_subtree {
                    continue;
                }
                if is_directory {
                    pending.push(path);
                }
            }
        }
        Ok(())
    }

    /// Resolve a read-only path. Workspace reads are strict by default; an
    /// explicit opt-out is reserved for operator-enabled dangerous mode.
    pub fn resolve_read_path(&self, raw_path: &str) -> WorkspaceResult<ResolvedPath> {
        let raw = if raw_path.is_empty() { "." } else { raw_path };
        self.validate_read_text(raw)?;
        let input = Path::new(raw);
        let explicit_external = input.is_absolute()
            || input
                .components()
                .any(|part| matches!(part, Component::ParentDir));
        let candidate = if input.is_absolute() {
            input.to_path_buf()
        } else {
            self.root
                .join(raw.replace('/', std::path::MAIN_SEPARATOR_STR))
        };
        let resolved = candidate
            .canonicalize()
            .map_err(|_| WorkspaceError::not_found(format!("Path not found: {raw}")))?;
        if !self.allow_external_reads && !resolved.starts_with(&self.root) {
            return Err(WorkspaceError::path_outside_workspace());
        }
        if !explicit_external && candidate.starts_with(&self.root) {
            self.ensure_inside_workspace(&candidate, &resolved)?;
        }
        Ok(ResolvedPath {
            display: relative_display(&self.root, &resolved),
            path: resolved,
            existed: true,
        })
    }

    pub fn resolve_existing_at(
        &self,
        base: &Path,
        raw_path: &str,
    ) -> WorkspaceResult<ResolvedPath> {
        let raw = if raw_path.is_empty() { "." } else { raw_path };
        self.reject_unsafe_text(raw)?;
        let base = self.validate_base(base)?;
        let candidate = base.join(raw.replace('/', std::path::MAIN_SEPARATOR_STR));
        let resolved = candidate
            .canonicalize()
            .map_err(|_| WorkspaceError::not_found(format!("Path not found: {raw}")))?;
        self.ensure_inside_workspace(&candidate, &resolved)?;
        Ok(ResolvedPath {
            display: relative_display(&self.root, &resolved),
            path: resolved,
            existed: true,
        })
    }

    pub fn resolve_for_write(&self, raw_path: &str) -> WorkspaceResult<ResolvedPath> {
        self.reject_unsafe_text(raw_path)?;
        self.reject_protected_write_path(raw_path)?;
        let pure = Path::new(raw_path);
        if pure.file_name().is_none() || raw_path == "." || raw_path == ".." {
            return Err(WorkspaceError::invalid_argument("Invalid write target"));
        }
        let candidate = self
            .root
            .join(raw_path.replace('/', std::path::MAIN_SEPARATOR_STR));
        if candidate.exists() || candidate.is_symlink() {
            let resolved = candidate
                .canonicalize()
                .map_err(|_| WorkspaceError::not_found(format!("Path not found: {raw_path}")))?;
            self.ensure_inside_workspace(&candidate, &resolved)?;
            return Ok(ResolvedPath {
                display: relative_display(&self.root, &resolved),
                path: resolved,
                existed: true,
            });
        }
        let parent = candidate.parent().unwrap_or(&self.root);
        let resolved_parent = if parent.exists() {
            parent
                .canonicalize()
                .map_err(|_| WorkspaceError::not_found("Parent directory not found"))?
        } else {
            self.ensure_parent_chain(parent)?;
            parent.to_path_buf()
        };
        if !resolved_parent.starts_with(&self.root) {
            return Err(WorkspaceError::path_outside_workspace());
        }
        Ok(ResolvedPath {
            display: raw_path.replace('\\', "/"),
            path: candidate,
            existed: false,
        })
    }

    fn ensure_parent_chain(&self, parent: &Path) -> WorkspaceResult<()> {
        let mut cursor = parent;
        while !cursor.exists() {
            if cursor == self.root || cursor.parent() == Some(cursor) {
                break;
            }
            cursor = cursor.parent().unwrap_or(cursor);
        }
        if cursor.exists() {
            let resolved = cursor
                .canonicalize()
                .map_err(|_| WorkspaceError::not_found("Parent directory not found"))?;
            if !resolved.starts_with(&self.root) {
                return Err(WorkspaceError::path_outside_workspace());
            }
        }
        Ok(())
    }

    fn validate_base(&self, base: &Path) -> WorkspaceResult<PathBuf> {
        let resolved = base
            .canonicalize()
            .map_err(|_| WorkspaceError::not_found("Base path not found"))?;
        if !resolved.is_dir() {
            return Err(WorkspaceError::not_a_directory("Base is not a directory"));
        }
        if !resolved.starts_with(&self.root) {
            return Err(WorkspaceError::path_outside_workspace());
        }
        Ok(resolved)
    }

    fn ensure_inside_workspace(&self, candidate: &Path, resolved: &Path) -> WorkspaceResult<()> {
        if !resolved.starts_with(&self.root) {
            if candidate.is_symlink() {
                return Err(WorkspaceError::symlink_escape());
            }
            return Err(WorkspaceError::path_outside_workspace());
        }
        Ok(())
    }

    pub fn reject_write_symlink(&self, raw_path: &str) -> WorkspaceResult<()> {
        self.reject_unsafe_text(raw_path)?;
        let candidate = self
            .root
            .join(raw_path.replace('/', std::path::MAIN_SEPARATOR_STR));
        if candidate.is_symlink() {
            return Err(WorkspaceError::symlink_escape());
        }
        Ok(())
    }

    pub fn reject_protected_write_path(&self, raw_path: &str) -> WorkspaceResult<()> {
        let normalized = raw_path.replace('\\', "/");
        let first = normalized.split('/').next().unwrap_or("");
        if matches!(first, ".git" | ".github") {
            return Err(WorkspaceError::Tool {
                code: "PROTECTED_PATH",
                message: format!("禁止普通文件操作写入受保护目录: {raw_path}"),
                category: "security",
                retryable: false,
            });
        }
        Ok(())
    }

    fn validate_read_text(&self, raw_path: &str) -> WorkspaceResult<()> {
        if raw_path.contains('\0') {
            return Err(WorkspaceError::invalid_argument("Path contains a NUL byte"));
        }
        Ok(())
    }

    pub fn is_ignored_path(
        &self,
        path: &Path,
        include_hidden: bool,
        include_ignored: bool,
    ) -> bool {
        (!include_hidden && self.is_hidden_path(path))
            || (!include_ignored && self.is_default_ignored_path(path))
    }

    pub fn is_hidden_path(&self, path: &Path) -> bool {
        let Ok(scan_path) = path.strip_prefix(&self.root) else {
            // Workspace 外的读取路径不套用 Workspace 内部的隐藏/构建目录过滤，
            // 否则 Windows 临时目录等路径会被误判为隐藏目录而无法读取。
            return false;
        };
        let hidden_by_name = scan_path
            .components()
            .filter_map(|part| match part {
                Component::Normal(name) => Some(name.to_string_lossy().into_owned()),
                _ => None,
            })
            .any(|part| part.starts_with('.') && part != ".");
        hidden_by_name || windows_hidden_attribute(path)
    }

    pub fn is_default_ignored_path(&self, path: &Path) -> bool {
        let Ok(scan_path) = path.strip_prefix(&self.root) else {
            return false;
        };
        scan_path
            .components()
            .filter_map(|part| match part {
                Component::Normal(name) => Some(name.to_string_lossy().into_owned()),
                _ => None,
            })
            .any(|part| DEFAULT_EXCLUDED_NAMES.contains(&part.as_str()))
    }

    pub fn is_safe_existing_path(&self, path: &Path) -> bool {
        path.canonicalize()
            .map(|p| p.starts_with(&self.root))
            .unwrap_or(false)
    }

    pub fn is_safe_read_path(&self, path: &Path) -> bool {
        path.exists() || path.is_symlink()
    }
}

pub fn relative_display(root: &Path, path: &Path) -> String {
    let display = path
        .strip_prefix(root)
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| path.to_string_lossy().replace('\\', "/"));
    #[cfg(windows)]
    {
        if let Some(unc) = display.strip_prefix("//?/UNC/") {
            return format!("//{unc}");
        }
        if let Some(normal) = display.strip_prefix("//?/") {
            return normal.to_string();
        }
    }
    display
}

pub fn tool_ok(mut value: Value) -> Value {
    if value.get("ok").is_none() {
        value
            .as_object_mut()
            .expect("tool result object")
            .insert("ok".into(), Value::Bool(true));
    }
    value
}

pub fn tool_err(error: WorkspaceError) -> Value {
    json!({
        "ok": false,
        "status": "error",
        "summary": error.message(),
        "error": error.to_error_value()
    })
}

pub fn tool_err_code(
    code: &'static str,
    message: impl Into<String>,
    category: &'static str,
) -> Value {
    let message = message.into();
    json!({
        "ok": false,
        "status": "error",
        "summary": message.clone(),
        "error": {
            "code": code,
            "message": message,
            "category": category,
            "retryable": false,
            "details": {}
        }
    })
}

pub fn wrap_tool_result(structured: Value) -> Value {
    wrap_mcp_tool_result("", &serde_json::json!({}), structured)
}

pub fn wrap_mcp_tool_result(tool_name: &str, args: &Value, mut structured: Value) -> Value {
    let is_error = structured.get("ok").and_then(Value::as_bool) == Some(false);
    let image_payload = if tool_name == "view_image"
        && args
            .get("output")
            .and_then(Value::as_str)
            .unwrap_or("mcp_image")
            == "mcp_image"
        && !is_error
    {
        let data = structured
            .as_object_mut()
            .and_then(|object| object.remove("base64"))
            .and_then(|value| value.as_str().map(str::to_string));
        data.map(|data| {
            json!({
                "type": "image",
                "data": data,
                "mimeType": structured
                    .get("mime_type")
                    .and_then(Value::as_str)
                    .unwrap_or("application/octet-stream")
            })
        })
    } else {
        None
    };

    if let Err(error) = jsonschema::validator_for(&crate::tools::registry::output_schema(tool_name))
        .and_then(|validator| validator.validate(&structured))
    {
        structured = tool_err(WorkspaceError::ToolDetails {
            code: "TOOL_OUTPUT_SCHEMA_VIOLATION",
            message: format!("Tool output violates outputSchema: {error}"),
            category: "internal",
            retryable: false,
            details: json!({"tool": tool_name}),
        });
    }

    let is_error = structured.get("ok").and_then(Value::as_bool) == Some(false);
    let text_value = if tool_name == "view_image" && !is_error {
        let mut metadata = structured.clone();
        if let Some(object) = metadata.as_object_mut() {
            object.remove("data_url");
            object.remove("base64");
        }
        metadata
    } else {
        structured.clone()
    };
    let mut content = Vec::new();
    if let Some(image) = image_payload {
        content.push(image);
    }
    content.push(json!({
        "type": "text",
        "text": text_value.to_string()
    }));
    json!({
        "content": content,
        "structuredContent": structured,
        "isError": is_error
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{wrap_mcp_tool_result, Workspace};

    #[test]
    fn strict_read_boundary_rejects_explicit_external_paths() {
        let root = tempfile::tempdir().expect("workspace");
        let external = tempfile::NamedTempFile::new().expect("external file");
        let strict = Workspace::new(root.path().to_path_buf())
            .expect("workspace")
            .with_strict_read_boundary(true);
        let error = strict
            .resolve_read_path(&external.path().display().to_string())
            .expect_err("external read must be rejected");
        assert_eq!(error.to_error_value()["code"], "PATH_OUTSIDE_WORKSPACE");
    }

    #[test]
    fn child_process_boundary_rejects_nested_external_links() {
        let root = tempfile::tempdir().expect("workspace");
        let external = tempfile::tempdir().expect("external");
        let nested = root.path().join("nested");
        std::fs::create_dir_all(&nested).expect("nested");
        let link = nested.join("escape");
        #[cfg(unix)]
        std::os::unix::fs::symlink(external.path(), &link).expect("symlink");
        #[cfg(windows)]
        if std::os::windows::fs::symlink_dir(external.path(), &link).is_err() {
            return;
        }

        let workspace = Workspace::new(root.path().to_path_buf()).expect("workspace");
        let error = workspace
            .ensure_child_process_boundary()
            .expect_err("nested external link must block child processes");
        assert_eq!(error.to_error_value()["code"], "WORKSPACE_LINK_ESCAPE");
        assert_eq!(error.to_error_value()["details"]["link_path"], "nested/escape");
    }

    #[test]
    fn child_process_boundary_skips_generated_dependency_link_trees() {
        let root = tempfile::tempdir().expect("workspace");
        let dependencies = root.path().join("node_modules").join("package");
        std::fs::create_dir_all(&dependencies).expect("dependencies");
        let broken = dependencies.join("missing-target");
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.path().join("does-not-exist"), &broken)
            .expect("broken symlink");
        #[cfg(windows)]
        if std::os::windows::fs::symlink_dir(root.path().join("does-not-exist"), &broken).is_err()
        {
            return;
        }

        let workspace = Workspace::new(root.path().to_path_buf()).expect("workspace");
        workspace
            .ensure_child_process_boundary()
            .expect("generated dependency internals are not part of the recursive boundary scan");
    }

    #[test]
    fn explicit_operator_override_can_allow_external_read_paths() {
        let root = tempfile::tempdir().expect("workspace");
        let external = tempfile::NamedTempFile::new().expect("external file");
        let workspace = Workspace::new(root.path().to_path_buf())
            .expect("workspace")
            .with_strict_read_boundary(false);
        assert!(workspace
            .resolve_read_path(&external.path().display().to_string())
            .is_ok());
    }

    #[test]
    fn workspace_reads_are_strict_by_default() {
        let root = tempfile::tempdir().expect("workspace");
        let external = tempfile::NamedTempFile::new().expect("external file");
        let workspace = Workspace::new(root.path().to_path_buf()).expect("workspace");
        let error = workspace
            .resolve_read_path(&external.path().display().to_string())
            .expect_err("default workspace must reject external reads");
        assert_eq!(error.to_error_value()["code"], "PATH_OUTSIDE_WORKSPACE");
    }

    #[test]
    fn image_result_contains_binary_payload_only_once() {
        let result = wrap_mcp_tool_result(
            "view_image",
            &json!({"output": "mcp_image"}),
            json!({
                "ok": true,
                "path": "image.png",
                "mime_type": "image/png",
                "bytes": 1,
                "width": 1,
                "height": 1,
                "resized": false,
                "original": {},
                "warnings": [],
                "base64": "AA=="
            }),
        );
        assert_eq!(result["content"][0]["type"], "image");
        assert_eq!(result["content"][0]["data"], "AA==");
        assert_eq!(result["content"][1]["type"], "text");
        assert!(result["structuredContent"].get("base64").is_none());
        assert!(!result["content"][1]["text"]
            .as_str()
            .unwrap_or_default()
            .contains("AA=="));
    }

    #[test]
    fn data_url_result_does_not_duplicate_payload_in_text_fallback() {
        let result = wrap_mcp_tool_result(
            "view_image",
            &json!({"output": "data_url"}),
            json!({
                "ok": true,
                "path": "image.png",
                "mime_type": "image/png",
                "bytes": 1,
                "width": 1,
                "height": 1,
                "resized": false,
                "original": {},
                "warnings": [],
                "data_url": "data:image/png;base64,AA=="
            }),
        );
        assert_eq!(result["content"].as_array().unwrap().len(), 1);
        assert!(result["structuredContent"]["data_url"]
            .as_str()
            .unwrap_or_default()
            .contains("AA=="));
        assert!(!result["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .contains("AA=="));
    }

    #[test]
    fn invalid_local_structured_output_becomes_a_tool_error() {
        let result = wrap_mcp_tool_result(
            "read_file",
            &json!({"path": "README.md"}),
            json!({"content": "missing ok"}),
        );
        assert_eq!(result["isError"], true);
        assert_eq!(
            result["structuredContent"]["error"]["code"],
            "TOOL_OUTPUT_SCHEMA_VIOLATION"
        );
    }
}
