use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[cfg(windows)]
use std::os::windows::process::CommandExt;
#[cfg(windows)]
use std::sync::OnceLock;

use serde_json::{json, Value};
use tokio::process::Command;

use crate::harness::state::{capture_baseline_entries, diff_baseline_entries};
use crate::tools::command_session::{
    finalize_execution_result, DurableCommandSpec, ExecSession, SessionHarnessMetadata,
    StreamEncoding,
};
use crate::tools::context::ToolContext;
use crate::tools::workspace::{tool_ok, WorkspaceError};
use crate::tools::CancellationToken;

const COMPLETION_GRACE: Duration = Duration::from_millis(50);
const COMPLETION_POLL_INTERVAL: Duration = Duration::from_millis(5);

fn apply_execution_environment(
    ctx: &ToolContext,
    execution: &Value,
    command: &mut Command,
    program: &str,
    child_parallelism: usize,
) -> Result<(), WorkspaceError> {
    if let Some(env) = execution.get("env").and_then(Value::as_object) {
        for (name, value) in env {
            if let Some(value) = value.as_str() {
                command.env(name, value);
            }
        }
    }

    #[cfg(windows)]
    command
        .env("PYTHONUTF8", "1")
        .env("PYTHONIOENCODING", "utf-8")
        .env("PYTHONLEGACYWINDOWSSTDIO", "0");
    #[cfg(windows)]
    configure_windows_command_environment(command, program);
    #[cfg(unix)]
    apply_standard_user_toolchain_path(command)?;
    ctx.resources
        .apply_child_environment(command, child_parallelism, program);
    let toolchain_paths = resolve_toolchain_paths(execution, ctx.workspace.root())?;
    apply_toolchain_path_overlay(command, &toolchain_paths)?;
    Ok(())
}

fn durable_supervisor_executable() -> Result<PathBuf, std::io::Error> {
    let current = std::env::current_exe()?;
    let current_name = current
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if current_name == crate::brand::CLI_NAME.to_ascii_lowercase() {
        return Ok(current);
    }

    // Cargo integration-test executables live in target/{profile}/deps while
    // CARGO_BIN_EXE_anchor lives one directory above. Resolving this sibling
    // keeps durable exec coverage on the real hidden CLI entrypoint instead of
    // accidentally launching the test harness with CLI arguments.
    if let Some(profile_dir) = current.parent().and_then(Path::parent) {
        let candidate = profile_dir.join(format!(
            "{}{}",
            crate::brand::CLI_NAME,
            std::env::consts::EXE_SUFFIX
        ));
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Ok(current)
}

pub fn exec_command(ctx: &ToolContext, args: &Value) -> Result<Value, WorkspaceError> {
    exec_command_with_cancellation(ctx, args, &CancellationToken::default(), None, None)
}

fn parse_expected_exit_codes(args: &Value) -> Result<Vec<i32>, WorkspaceError> {
    let expected = args.get("expected_exit_codes");
    let allowed = args.get("allowed_exit_codes");
    if expected.is_some() && allowed.is_some() && expected != allowed {
        return Err(WorkspaceError::invalid_argument(
            "expected_exit_codes and allowed_exit_codes cannot specify different values",
        ));
    }
    let Some(value) = expected.or(allowed) else {
        return Ok(vec![0]);
    };
    let values = value.as_array().ok_or_else(|| {
        WorkspaceError::invalid_argument("expected_exit_codes must be an array of integers")
    })?;
    if values.is_empty() || values.len() > 32 {
        return Err(WorkspaceError::invalid_argument(
            "expected_exit_codes must contain between 1 and 32 exit codes",
        ));
    }
    let mut codes = Vec::with_capacity(values.len());
    for value in values {
        let code = value.as_i64().ok_or_else(|| {
            WorkspaceError::invalid_argument("expected_exit_codes must contain only integers")
        })?;
        let code = i32::try_from(code).map_err(|_| {
            WorkspaceError::invalid_argument("expected_exit_codes contains an out-of-range value")
        })?;
        codes.push(code);
    }
    codes.sort_unstable();
    codes.dedup();
    Ok(codes)
}

pub(crate) fn effective_command_search_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(process_path) = std::env::var_os("PATH") {
        for path in std::env::split_paths(&process_path) {
            push_unique_path(&mut paths, path, false);
        }
    }

    #[cfg(unix)]
    append_unix_user_toolchain_paths(&mut paths);

    paths
}

pub(crate) fn effective_command_path() -> Option<std::ffi::OsString> {
    let paths = effective_command_search_paths();
    (!paths.is_empty())
        .then(|| std::env::join_paths(paths).ok())
        .flatten()
}

pub(crate) fn effective_command_path_additions() -> Vec<PathBuf> {
    let inherited = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .unwrap_or_default();
    effective_command_search_paths()
        .into_iter()
        .filter(|path| !inherited.iter().any(|current| current == path))
        .collect()
}

#[cfg(not(windows))]
pub(crate) fn resolve_effective_system_program(name: &str) -> Option<PathBuf> {
    let candidates = system_program_candidates(name);
    effective_command_search_paths()
        .into_iter()
        .flat_map(|directory| {
            candidates
                .iter()
                .map(move |candidate| directory.join(candidate))
        })
        .find(|candidate| command_candidate_is_executable(candidate))
        .map(|path| resolve_system_program_path(&path))
}

#[cfg(windows)]
pub(crate) fn resolve_effective_system_program(name: &str) -> Option<PathBuf> {
    which::which(name)
        .ok()
        .map(|path| resolve_system_program_path(&path))
}

#[cfg(not(windows))]
fn system_program_candidates(name: &str) -> Vec<String> {
    vec![name.to_string()]
}

#[cfg(unix)]
fn command_candidate_is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    std::fs::metadata(path)
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

fn push_unique_path(paths: &mut Vec<PathBuf>, path: PathBuf, require_directory: bool) {
    if require_directory && !path.is_dir() {
        return;
    }
    if !paths.iter().any(|current| current == &path) {
        paths.push(path);
    }
}

#[cfg(unix)]
fn append_unix_user_toolchain_paths(paths: &mut Vec<PathBuf>) {
    for (name, suffix) in [
        ("NVM_BIN", None),
        ("FNM_MULTISHELL_PATH", Some("bin")),
        ("PNPM_HOME", None),
        ("CARGO_HOME", Some("bin")),
        ("VOLTA_HOME", Some("bin")),
        ("BUN_INSTALL", Some("bin")),
        ("ASDF_DATA_DIR", Some("shims")),
        ("MISE_DATA_DIR", Some("shims")),
        ("GOBIN", None),
        ("GOROOT", Some("bin")),
    ] {
        if let Some(value) = std::env::var_os(name) {
            let mut path = PathBuf::from(value);
            if let Some(suffix) = suffix {
                path.push(suffix);
            }
            push_unique_path(paths, path, true);
        }
    }

    if let Some(gopath) = std::env::var_os("GOPATH") {
        for path in std::env::split_paths(&gopath) {
            push_unique_path(paths, path.join("bin"), true);
        }
    }

    let Some(home) = dirs::home_dir() else {
        push_unique_path(paths, PathBuf::from("/usr/local/go/bin"), true);
        return;
    };

    let nvm_root = std::env::var_os("NVM_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".nvm"));
    if let Some(bin) = resolve_nvm_default_bin(&nvm_root) {
        push_unique_path(paths, bin, true);
    }

    for path in [
        home.join(".local/bin"),
        home.join(".cargo/bin"),
        home.join(".local/share/pnpm"),
        home.join(".volta/bin"),
        home.join(".asdf/shims"),
        home.join(".local/share/mise/shims"),
        home.join(".bun/bin"),
        home.join("go/bin"),
    ] {
        push_unique_path(paths, path, true);
    }

    push_unique_path(paths, PathBuf::from("/usr/local/go/bin"), true);
}

#[cfg(unix)]
fn resolve_nvm_default_bin(nvm_root: &Path) -> Option<PathBuf> {
    let versions_root = nvm_root.join("versions/node");
    let installed = installed_nvm_versions(&versions_root);
    if installed.is_empty() {
        return None;
    }

    let default_alias = nvm_root.join("alias/default");
    if let Ok(target) = std::fs::read_to_string(default_alias) {
        if let Some(version) = resolve_nvm_target(nvm_root, target.trim(), &installed, 0) {
            return Some(versions_root.join(version).join("bin"));
        }
    }

    (installed.len() == 1).then(|| versions_root.join(&installed[0].1).join("bin"))
}

#[cfg(unix)]
fn resolve_nvm_target(
    nvm_root: &Path,
    target: &str,
    installed: &[((u64, u64, u64), String)],
    depth: usize,
) -> Option<String> {
    if target.is_empty() || depth >= 8 || target.eq_ignore_ascii_case("system") {
        return None;
    }
    let target = target.trim();
    if matches!(target, "node" | "stable") {
        return installed.first().map(|(_, version)| version.clone());
    }

    let normalized = target.trim_start_matches('v');
    let numeric_prefix = normalized
        .split('.')
        .map(str::parse::<u64>)
        .collect::<Result<Vec<_>, _>>()
        .ok()
        .filter(|parts| !parts.is_empty() && parts.len() <= 3);
    if let Some(prefix) = numeric_prefix {
        return installed
            .iter()
            .find(|(version, _)| nvm_version_matches_prefix(*version, &prefix))
            .map(|(_, version)| version.clone());
    }

    let alias_path = Path::new(target);
    if alias_path.is_absolute()
        || alias_path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
    {
        return None;
    }
    let nested = std::fs::read_to_string(nvm_root.join("alias").join(alias_path)).ok()?;
    resolve_nvm_target(nvm_root, nested.trim(), installed, depth + 1)
}

#[cfg(unix)]
fn installed_nvm_versions(root: &Path) -> Vec<((u64, u64, u64), String)> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut versions = entries
        .flatten()
        .filter_map(|entry| {
            if !entry.file_type().ok()?.is_dir() {
                return None;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let key = parse_nvm_version(&name)?;
            entry.path().join("bin").is_dir().then_some((key, name))
        })
        .collect::<Vec<_>>();
    versions.sort_by_key(|entry| std::cmp::Reverse(entry.0));
    versions
}

#[cfg(unix)]
fn parse_nvm_version(value: &str) -> Option<(u64, u64, u64)> {
    let mut parts = value.trim_start_matches('v').split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().unwrap_or("0").parse().ok()?;
    let patch = parts.next().unwrap_or("0").parse().ok()?;
    parts.next().is_none().then_some((major, minor, patch))
}

#[cfg(unix)]
fn nvm_version_matches_prefix(version: (u64, u64, u64), prefix: &[u64]) -> bool {
    let parts = [version.0, version.1, version.2];
    prefix
        .iter()
        .enumerate()
        .all(|(index, expected)| parts[index] == *expected)
}

fn apply_toolchain_path_overlay(
    command: &mut Command,
    toolchain_paths: &[PathBuf],
) -> Result<(), WorkspaceError> {
    let configured_path = command.as_std().get_envs().find_map(|(name, value)| {
        name.eq_ignore_ascii_case("PATH")
            .then(|| value.map(std::ffi::OsStr::to_os_string))
            .flatten()
    });
    let mut paths = toolchain_paths.to_vec();
    if let Some(existing) = configured_path.or_else(|| std::env::var_os("PATH")) {
        for path in std::env::split_paths(&existing) {
            push_unique_path(&mut paths, path, false);
        }
    }
    for path in effective_command_path_additions() {
        push_unique_path(&mut paths, path, true);
    }
    if paths.is_empty() {
        return Ok(());
    }
    let joined = std::env::join_paths(paths).map_err(|error| WorkspaceError::Tool {
        code: "TOOLCHAIN_PATH_INVALID",
        message: format!("Unable to construct workspace-local toolchain PATH: {error}"),
        category: "validation",
        retryable: false,
    })?;
    command.env("PATH", joined);
    Ok(())
}

#[cfg(unix)]
fn standard_user_toolchain_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let home = std::env::var_os("HOME").map(PathBuf::from);
    if let Some(cargo_home) = std::env::var_os("CARGO_HOME") {
        paths.push(PathBuf::from(cargo_home).join("bin"));
    } else if let Some(home) = home.as_ref() {
        paths.push(home.join(".cargo/bin"));
    }
    if let Some(pnpm_home) = std::env::var_os("PNPM_HOME") {
        paths.push(PathBuf::from(pnpm_home));
    }
    if let Some(volta_home) = std::env::var_os("VOLTA_HOME") {
        paths.push(PathBuf::from(volta_home).join("bin"));
    }
    if let Some(bun_install) = std::env::var_os("BUN_INSTALL") {
        paths.push(PathBuf::from(bun_install).join("bin"));
    }
    if let Some(home) = home {
        paths.extend([
            home.join(".local/bin"),
            home.join(".local/share/pnpm"),
            home.join(".volta/bin"),
            home.join(".bun/bin"),
            home.join(".asdf/shims"),
            home.join(".local/share/mise/shims"),
            home.join(".pyenv/shims"),
        ]);
    }
    paths.retain(|path| path.is_dir());
    paths.dedup();
    paths
}

#[cfg(unix)]
fn apply_standard_user_toolchain_path(command: &mut Command) -> Result<(), WorkspaceError> {
    let fallback = standard_user_toolchain_paths();
    if fallback.is_empty() {
        return Ok(());
    }
    let configured_path = command.as_std().get_envs().find_map(|(name, value)| {
        name.eq_ignore_ascii_case("PATH")
            .then(|| value.map(std::ffi::OsStr::to_os_string))
            .flatten()
    });
    let mut paths = configured_path
        .or_else(|| std::env::var_os("PATH"))
        .map(|value| std::env::split_paths(&value).collect::<Vec<_>>())
        .unwrap_or_default();
    for fallback in fallback {
        if !paths.contains(&fallback) {
            paths.push(fallback);
        }
    }
    let joined = std::env::join_paths(paths).map_err(|error| WorkspaceError::Tool {
        code: "TOOLCHAIN_PATH_INVALID",
        message: format!("Unable to construct standard user toolchain PATH: {error}"),
        category: "runtime",
        retryable: true,
    })?;
    command.env("PATH", joined);
    Ok(())
}

fn validate_toolchain_paths(value: Option<&Value>) -> Result<(), WorkspaceError> {
    let Some(value) = value else {
        return Ok(());
    };
    let paths = value.as_array().ok_or_else(|| {
        WorkspaceError::invalid_argument(
            "toolchain_paths must be an array of workspace-relative directories",
        )
    })?;
    if paths.len() > 16 {
        return Err(WorkspaceError::invalid_argument(
            "toolchain_paths accepts at most 16 directories",
        ));
    }
    for value in paths {
        let raw = value.as_str().ok_or_else(|| {
            WorkspaceError::invalid_argument("toolchain_paths must contain only strings")
        })?;
        let path = Path::new(raw);
        if raw.trim().is_empty()
            || path.is_absolute()
            || path.components().any(|component| {
                matches!(
                    component,
                    std::path::Component::ParentDir
                        | std::path::Component::RootDir
                        | std::path::Component::Prefix(_)
                )
            })
        {
            return Err(WorkspaceError::invalid_argument(
                "toolchain_paths entries must stay inside the workspace and cannot contain parent traversal",
            ));
        }
    }
    Ok(())
}

fn resolve_toolchain_paths(
    execution: &Value,
    workspace_root: &Path,
) -> Result<Vec<PathBuf>, WorkspaceError> {
    validate_toolchain_paths(execution.get("toolchain_paths"))?;
    let Some(paths) = execution.get("toolchain_paths").and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    let canonical_workspace = workspace_root
        .canonicalize()
        .map_err(|_| WorkspaceError::Tool {
            code: "COMMAND_REJECTED",
            message: "Workspace root is unavailable".into(),
            category: "runtime",
            retryable: true,
        })?;
    paths
        .iter()
        .map(|value| {
            let raw = value.as_str().expect("toolchain path already validated");
            let resolved =
                workspace_root
                    .join(raw)
                    .canonicalize()
                    .map_err(|_| WorkspaceError::Tool {
                        code: "TOOLCHAIN_PATH_NOT_FOUND",
                        message: format!("Workspace-local toolchain directory not found: {raw}"),
                        category: "runtime",
                        retryable: true,
                    })?;
            if !resolved.starts_with(&canonical_workspace) || !resolved.is_dir() {
                return Err(WorkspaceError::Tool {
                    code: "TOOLCHAIN_PATH_OUTSIDE_WORKSPACE",
                    message: format!(
                        "Toolchain directory must resolve inside the workspace: {raw}"
                    ),
                    category: "security",
                    retryable: false,
                });
            }
            Ok(resolved)
        })
        .collect()
}

#[cfg(not(windows))]
pub(crate) fn apply_preferred_shell(
    _args: &mut Value,
    _policy: &crate::tools::policy::PolicySettings,
) -> Result<(), WorkspaceError> {
    Ok(())
}

#[cfg(windows)]
pub(crate) fn apply_preferred_shell(
    args: &mut Value,
    policy: &crate::tools::policy::PolicySettings,
) -> Result<(), WorkspaceError> {
    let preferred = policy.preferred_shell.trim();
    if preferred.is_empty() || preferred == "auto" {
        return Ok(());
    }
    let object = args.as_object_mut().ok_or_else(|| {
        WorkspaceError::invalid_argument("exec_command arguments must be an object")
    })?;
    if object.contains_key("shell")
        || object.contains_key("executable")
        || object.contains_key("args")
    {
        return Ok(());
    }
    let Some(command) = object
        .get("cmd")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
    else {
        return Ok(());
    };
    let shell_args = match preferred {
        "pwsh" | "powershell" => vec![
            "-NoLogo".to_string(),
            "-NoProfile".to_string(),
            "-NonInteractive".to_string(),
            "-Command".to_string(),
            command,
        ],
        "cmd" => vec![
            "/d".to_string(),
            "/s".to_string(),
            "/c".to_string(),
            command,
        ],
        _ => {
            return Err(WorkspaceError::invalid_argument(
                "preferred shell must be auto, pwsh, powershell, or cmd",
            ))
        }
    };
    object.remove("cmd");
    object.insert("shell".into(), Value::String(preferred.to_string()));
    object.insert(
        "args".into(),
        Value::Array(shell_args.into_iter().map(Value::String).collect()),
    );
    Ok(())
}

fn parse_and_resolve_execution(
    execution: &Value,
    cmd: &str,
    cwd: &Path,
    workspace_root: &Path,
    policy: &crate::tools::policy::PolicySettings,
) -> Result<(String, Vec<String>), WorkspaceError> {
    let toolchain_paths = resolve_toolchain_paths(execution, workspace_root)?;
    let structured = execution.get("executable").is_some() || execution.get("shell").is_some();
    if !structured {
        return parse_and_resolve(cmd, cwd, workspace_root, policy, &toolchain_paths);
    }
    let shell = execution
        .get("shell")
        .and_then(Value::as_str)
        .unwrap_or("direct");
    let raw_program = match shell {
        "direct" => execution
            .get("executable")
            .and_then(Value::as_str)
            .ok_or_else(|| WorkspaceError::invalid_argument("executable is required"))?,
        "pwsh" => "pwsh",
        "powershell" => "powershell",
        "cmd" => "cmd.exe",
        _ => return Err(WorkspaceError::invalid_argument("unsupported shell")),
    };
    let program = resolve_program(raw_program, cwd, workspace_root, policy, &toolchain_paths)?;
    Ok((program, structured_args(execution.get("args"))?))
}

fn execution_mode(execution: &Value) -> &str {
    match execution.get("shell").and_then(Value::as_str) {
        Some("pwsh") => "pwsh",
        Some("powershell") => "powershell",
        Some("cmd") => "cmd",
        Some("direct") if execution.get("executable").is_some() => "structured_direct",
        _ if execution.get("executable").is_some() => "structured_direct",
        _ => "direct",
    }
}

pub(crate) fn normalize_exec_arguments(args: &mut Value) -> Result<(), WorkspaceError> {
    let object = args.as_object_mut().ok_or_else(|| {
        WorkspaceError::invalid_argument("exec_command arguments must be an object")
    })?;
    let cmd = object
        .get("cmd")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let executable = object
        .get("executable")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let shell = object
        .get("shell")
        .and_then(Value::as_str)
        .unwrap_or("direct");
    let has_structured_program = executable.is_some() || object.get("shell").is_some();
    let has_structured_args = object.get("args").is_some();

    if cmd.is_some() && (has_structured_program || has_structured_args) {
        return Err(WorkspaceError::invalid_argument(
            "cmd cannot be combined with executable, args, or shell",
        ));
    }
    if cmd.is_some() {
        validate_exec_env(object.get("env"))?;
        validate_toolchain_paths(object.get("toolchain_paths"))?;
        return Ok(());
    }

    let program = match shell {
        "direct" => executable.ok_or_else(|| {
            WorkspaceError::invalid_argument(
                "executable is required for direct structured execution",
            )
        })?,
        "pwsh" => {
            if executable.is_some() {
                return Err(WorkspaceError::invalid_argument(
                    "executable cannot be combined with a named shell",
                ));
            }
            "pwsh"
        }
        "powershell" => {
            if executable.is_some() {
                return Err(WorkspaceError::invalid_argument(
                    "executable cannot be combined with a named shell",
                ));
            }
            "powershell"
        }
        "cmd" => {
            if executable.is_some() {
                return Err(WorkspaceError::invalid_argument(
                    "executable cannot be combined with a named shell",
                ));
            }
            "cmd.exe"
        }
        _ => {
            return Err(WorkspaceError::invalid_argument(
                "shell must be direct, pwsh, powershell, or cmd",
            ))
        }
    };
    let command_args = structured_args(object.get("args"))?;
    validate_exec_env(object.get("env"))?;
    validate_toolchain_paths(object.get("toolchain_paths"))?;
    let mut tokens = Vec::with_capacity(command_args.len() + 1);
    tokens.push(program.to_string());
    tokens.extend(command_args);
    object.insert(
        "cmd".into(),
        Value::String(crate::tools::policy::join_command_tokens(&tokens)),
    );
    Ok(())
}

fn structured_args(value: Option<&Value>) -> Result<Vec<String>, WorkspaceError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let array = value
        .as_array()
        .ok_or_else(|| WorkspaceError::invalid_argument("args must be an array of strings"))?;
    array
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| WorkspaceError::invalid_argument("args must contain only strings"))
        })
        .collect()
}

fn validate_exec_env(value: Option<&Value>) -> Result<(), WorkspaceError> {
    let Some(value) = value else {
        return Ok(());
    };
    let env = value.as_object().ok_or_else(|| {
        WorkspaceError::invalid_argument("env must be an object of string values")
    })?;
    for (name, value) in env {
        if name.is_empty() || name.contains(['=', '\0']) {
            return Err(WorkspaceError::invalid_argument(
                "env contains an invalid variable name",
            ));
        }
        let value = value.as_str().ok_or_else(|| {
            WorkspaceError::invalid_argument("env must contain only string values")
        })?;
        if value.contains('\0') {
            return Err(WorkspaceError::invalid_argument("env contains a NUL byte"));
        }
    }
    Ok(())
}

#[cfg(not(windows))]
pub(crate) fn resolve_system_program_path(path: &Path) -> PathBuf {
    path.to_path_buf()
}

#[cfg(windows)]
pub(crate) fn resolve_system_program_path(path: &Path) -> PathBuf {
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if matches!(stem.as_str(), "cargo" | "rustc" | "rustdoc") {
        if let Some(resolved) = rustup_tool_path(&stem) {
            return PathBuf::from(resolved);
        }
    }
    let Ok(target) = std::fs::read_link(path) else {
        return path.to_path_buf();
    };
    let target = if target.is_absolute() {
        target
    } else {
        path.parent().unwrap_or_else(|| Path::new(".")).join(target)
    };
    let normalized = PathBuf::from(windows_command_path(&target.to_string_lossy()));
    if normalized.is_file() {
        normalized
    } else {
        path.to_path_buf()
    }
}

#[cfg(windows)]
pub(crate) fn configure_windows_command_environment(command: &mut Command, program: &str) {
    let stem = Path::new(program)
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if stem != "cargo" {
        return;
    }
    if let Some(rustc) = rustup_tool_path("rustc") {
        command.env("RUSTC", rustc);
    }
    if let Some(rustdoc) = rustup_tool_path("rustdoc") {
        command.env("RUSTDOC", rustdoc);
    }
    if let Some(cargo) = rustup_tool_path("cargo") {
        if let Some(toolchain_bin) = Path::new(cargo).parent() {
            let mut paths = vec![toolchain_bin.to_path_buf()];
            if let Some(existing) = std::env::var_os("PATH") {
                paths.extend(std::env::split_paths(&existing));
            }
            if let Ok(joined) = std::env::join_paths(paths) {
                command.env("PATH", joined);
            }
        }
    }
}

#[cfg(windows)]
fn rustup_tool_path(tool: &str) -> Option<&'static str> {
    static CARGO: OnceLock<Option<String>> = OnceLock::new();
    static RUSTC: OnceLock<Option<String>> = OnceLock::new();
    static RUSTDOC: OnceLock<Option<String>> = OnceLock::new();
    let slot = match tool {
        "cargo" => &CARGO,
        "rustc" => &RUSTC,
        "rustdoc" => &RUSTDOC,
        _ => return None,
    };
    slot.get_or_init(|| {
        let mut command = std::process::Command::new("rustup");
        crate::platform::hide_std_console(&mut command);
        let output = command.args(["which", tool]).output().ok()?;
        if !output.status.success() {
            return None;
        }
        let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
        (!path.is_empty() && Path::new(&path).is_file()).then_some(path)
    })
    .as_deref()
}

pub fn command_cost_explain(ctx: &ToolContext, args: &Value) -> Result<Value, WorkspaceError> {
    let cmd = args
        .get("cmd")
        .and_then(Value::as_str)
        .ok_or_else(|| WorkspaceError::invalid_argument("cmd is required"))?;
    let timeout_ms = args
        .get("timeout_ms")
        .and_then(Value::as_u64)
        .unwrap_or(30_000);
    let cost_intent = args
        .get("cost_intent")
        .and_then(Value::as_str)
        .unwrap_or("auto");
    let network_mode = args
        .get("network_mode")
        .and_then(Value::as_str)
        .unwrap_or("auto");
    let result = ctx.command_cost.explain(
        ctx.workspace.root(),
        cmd,
        timeout_ms,
        cost_intent,
        network_mode,
        &ctx.policy,
    )?;
    Ok(tool_ok(result))
}

fn cancelled_error(session: Option<Value>) -> WorkspaceError {
    WorkspaceError::ToolDetails {
        code: "REQUEST_CANCELLED",
        message: "Command execution was cancelled.".into(),
        category: "runtime",
        retryable: true,
        details: json!({
            "termination_reason": "cancelled",
            "recoverable": true,
            "suggestion": "Retry the request if it is still needed",
            "session": session
        }),
    }
}

pub fn exec_command_with_cancellation(
    ctx: &ToolContext,
    args: &Value,
    cancellation: &CancellationToken,
    task_id: Option<&str>,
    mcp_session_id: Option<&str>,
) -> Result<Value, WorkspaceError> {
    if cancellation.is_cancelled() {
        return Err(cancelled_error(None));
    }
    let cmd = args
        .get("cmd")
        .and_then(Value::as_str)
        .ok_or_else(|| WorkspaceError::invalid_argument("cmd is required"))?;
    let workdir_raw = args
        .get("workdir")
        .or_else(|| args.get("cwd"))
        .and_then(Value::as_str)
        .unwrap_or(".");
    let workdir = ctx.workspace.resolve_existing(workdir_raw)?;
    if !workdir.path.is_dir() {
        return Err(WorkspaceError::not_a_directory(
            "workdir is not a directory",
        ));
    }
    let filesystem_scope = args
        .get("filesystem_scope")
        .and_then(Value::as_str)
        .unwrap_or("workspace")
        .to_string();
    validate_child_process_scope(ctx, args)?;
    let requested_timeout_ms = args
        .get("timeout_ms")
        .and_then(Value::as_u64)
        .unwrap_or(30_000);
    let cost_intent = args
        .get("cost_intent")
        .and_then(Value::as_str)
        .unwrap_or("auto");
    let network_mode = args
        .get("network_mode")
        .and_then(Value::as_str)
        .unwrap_or("auto");
    let cost_decision = ctx.command_cost.evaluate(
        ctx.workspace.root(),
        cmd,
        requested_timeout_ms,
        cost_intent,
        network_mode,
        &ctx.policy,
    )?;
    let timeout_ms = cost_decision.effective_timeout_ms();
    let include_diagnostics = args
        .get("include_diagnostics")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if args.get("executable").is_none() && args.get("shell").is_none() {
        if let Some(result) = run_native_diagnostic(ctx, cmd, &workdir.path)? {
            if cancellation.is_cancelled() {
                return Err(cancelled_error(None));
            }
            let mut result = result;
            if let Some(object) = result.as_object_mut() {
                object.insert("child_process".into(), Value::Bool(false));
                object.insert("transport_ok".into(), Value::Bool(true));
                object.insert("command_ok".into(), Value::Bool(true));
                if include_diagnostics {
                    object.insert(
                        "filesystem_scope".into(),
                        Value::String(filesystem_scope.clone()),
                    );
                    object.insert("sandbox_enforced".into(), Value::Bool(false));
                    object.insert(
                        "execution_boundary".into(),
                        Value::String("policy_only".into()),
                    );
                    object.insert("cost_policy".into(), cost_decision.to_value());
                }
            }
            return Ok(finalize_execution_result(result));
        }
    }
    let max_output = args
        .get("max_output_bytes")
        .and_then(Value::as_u64)
        .unwrap_or(32_768) as usize;
    let yield_ms = args
        .get("yield_time_ms")
        .and_then(Value::as_u64)
        .unwrap_or(1000)
        .min(30_000);
    let tty = args.get("tty").and_then(Value::as_bool).unwrap_or(false);
    let stdin_text = args.get("stdin").and_then(Value::as_str).unwrap_or("");
    let verification_kind = args
        .get("verification_kind")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let verification_key = args
        .get("verification_key")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let recovery_key = args
        .get("recovery_key")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let test_file = args
        .get("test_file")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let test_name = args
        .get("test_name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let verification_level = args
        .get("verification_level")
        .and_then(Value::as_str)
        .unwrap_or("blocking");
    let supersede_previous_failures = args
        .get("supersede_previous_failures")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let expected_exit_codes = parse_expected_exit_codes(args)?;
    let workspace_before = capture_baseline_entries(ctx.workspace.root());

    ctx.workspace.ensure_child_process_boundary()?;

    let result = crate::async_runtime::block_on(async {
        run_command(
            ctx,
            args,
            cmd,
            &workdir.path,
            Duration::from_millis(timeout_ms),
            Duration::from_millis(yield_ms),
            max_output,
            tty,
            stdin_text,
            verification_kind,
            verification_key,
            recovery_key,
            test_file,
            test_name,
            workspace_before.clone(),
            expected_exit_codes.clone(),
            verification_level,
            supersede_previous_failures,
            cancellation,
            task_id,
            mcp_session_id,
        )
        .await
    });

    match result {
        Ok(mut out) => {
            attach_command_file_changes(ctx, &workspace_before, &mut out);
            if include_diagnostics {
                if let Some(object) = out.as_object_mut() {
                    object.insert("filesystem_scope".into(), Value::String(filesystem_scope));
                    object.insert("sandbox_enforced".into(), Value::Bool(false));
                    object.insert(
                        "execution_boundary".into(),
                        Value::String("policy_only".into()),
                    );
                    object.insert("cost_policy".into(), cost_decision.to_value());
                }
            }
            if let Some(object) = out.as_object_mut() {
                object.insert("child_process".into(), Value::Bool(true));
            }
            Ok(finalize_execution_result(out))
        }
        Err(error) => match execution_failure_result(&error, cmd, &workdir.path) {
            Some(mut result) => {
                attach_command_file_changes(ctx, &workspace_before, &mut result);
                if include_diagnostics {
                    if let Some(object) = result.as_object_mut() {
                        object.insert("filesystem_scope".into(), Value::String(filesystem_scope));
                        object.insert("sandbox_enforced".into(), Value::Bool(false));
                        object.insert(
                            "execution_boundary".into(),
                            Value::String("policy_only".into()),
                        );
                        object.insert("cost_policy".into(), cost_decision.to_value());
                    }
                } else if let Some(object) = result.as_object_mut() {
                    for key in [
                        "filesystem_scope",
                        "sandbox_enforced",
                        "execution_boundary",
                        "cost_policy",
                    ] {
                        object.remove(key);
                    }
                }
                Ok(finalize_execution_result(result))
            }
            None => Err(error),
        },
    }
}

fn validate_child_process_scope(_ctx: &ToolContext, args: &Value) -> Result<(), WorkspaceError> {
    let scope = args
        .get("filesystem_scope")
        .and_then(Value::as_str)
        .unwrap_or("workspace");
    match scope {
        "workspace" => Ok(()),
        "host" => Err(WorkspaceError::ToolDetails {
            code: "EXTERNAL_EXECUTION_NOT_ALLOWED",
            message: "exec_command 只允许在 Workspace 内执行，Workspace 外执行已禁用。".into(),
            category: "permission",
            retryable: false,
            details: json!({
                "stage": "policy",
                "filesystem_scope": "host",
                "sandbox_enforced": false,
                "recoverable": false,
                "suggestion": "将 filesystem_scope 设置为 workspace，并在当前 Workspace 内执行"
            }),
        }),
        _ => Err(WorkspaceError::invalid_argument(
            "filesystem_scope must be workspace",
        )),
    }
}

fn attach_command_file_changes(
    ctx: &ToolContext,
    workspace_before: &[crate::harness::model::BaselineEntry],
    output: &mut Value,
) {
    if output.get("status").and_then(Value::as_str) == Some("running")
        || output.get("termination_reason").and_then(Value::as_str) == Some("running")
    {
        return;
    }
    let workspace_after = capture_baseline_entries(ctx.workspace.root());
    let affected_files = diff_baseline_entries(workspace_before, &workspace_after);
    let mutation_attributed = !affected_files.is_empty();
    if let Some(object) = output.as_object_mut() {
        object.insert(
            "affected_files".into(),
            serde_json::to_value(affected_files).unwrap_or_else(|_| json!([])),
        );
        object.insert(
            "mutation_attributed".into(),
            Value::Bool(mutation_attributed),
        );
    }
}

fn run_native_diagnostic(
    ctx: &ToolContext,
    cmd: &str,
    cwd: &Path,
) -> Result<Option<Value>, WorkspaceError> {
    let parts =
        crate::tools::policy::split_command_line(cmd).map_err(WorkspaceError::invalid_argument)?;
    if parts.is_empty() {
        return Ok(None);
    }

    let command = parts[0].to_ascii_lowercase();
    let stdout = match command.as_str() {
        "pwd" if parts.len() == 1 => Some(format!("{}\n", cwd.display())),
        "ls" | "dir" => Some(list_directory(ctx, cwd, &parts[1..])?),
        "which" if parts.len() == 2 => {
            let path = resolve_effective_system_program(&parts[1]).ok_or_else(|| {
                WorkspaceError::Tool {
                    code: "COMMAND_NOT_FOUND",
                    message: format!("Program not found on PATH: {}", parts[1]),
                    category: "runtime",
                    retryable: false,
                }
            })?;
            Some(format!("{}\n", path.display()))
        }
        "echo" => Some(format!("{}\n", parts[1..].join(" "))),
        _ => None,
    };

    Ok(stdout.map(|stdout| {
        json!({
            "command": cmd,
            "resolved_cwd": cwd.display().to_string(),
            "status": "exited",
            "termination_reason": "exited",
            "recoverable": false,
            "suggestion": "命令已完成",
            "exit_code": 0,
            "stdout": stdout,
            "stderr": "",
            "stdout_truncated": false,
            "stderr_truncated": false,
            "stdout_complete": true,
            "stderr_complete": true,
            "stdout_complete": true,
            "stderr_complete": true,
            "duration_ms": 0,
            "elapsed_ms": 0,
            "execution_mode": "native_builtin",
            "execution_started": true,
            "command_runner": "native_builtin",
            "affected_files": [],
            "mutation_attributed": false,
            "warnings": ["native diagnostic without child process"]
        })
    }))
}

fn list_directory(
    ctx: &ToolContext,
    cwd: &Path,
    args: &[String],
) -> Result<String, WorkspaceError> {
    let target = match args {
        [] => cwd.to_path_buf(),
        [path] => ctx.workspace.resolve_existing(path)?.path,
        _ => {
            return Err(WorkspaceError::invalid_argument(
                "ls/dir accepts at most one directory path",
            ))
        }
    };
    if !target.is_dir() {
        return Err(WorkspaceError::not_a_directory(
            "ls/dir target is not a directory",
        ));
    }

    let mut entries = std::fs::read_dir(target)
        .map_err(|error| WorkspaceError::ToolDetails {
            code: "DIRECTORY_READ_FAILED",
            message: format!("Failed to read directory: {error}"),
            category: "runtime",
            retryable: true,
            details: json!({
                "stage": "native_builtin",
                "reason": "directory_read_failed",
                "retryable": true
            }),
        })?
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    entries.sort_unstable();
    Ok(if entries.is_empty() {
        String::new()
    } else {
        format!("{}\n", entries.join("\n"))
    })
}

#[allow(clippy::too_many_arguments)]
async fn run_command(
    ctx: &ToolContext,
    execution: &Value,
    cmd: &str,
    cwd: &Path,
    limit: Duration,
    yield_time: Duration,
    max_output: usize,
    tty: bool,
    stdin_text: &str,
    verification_kind: Option<&str>,
    verification_key: Option<&str>,
    recovery_key: Option<&str>,
    test_file: Option<&str>,
    test_name: Option<&str>,
    workspace_before: Vec<crate::harness::model::BaselineEntry>,
    expected_exit_codes: Vec<i32>,
    verification_level: &str,
    supersede_previous_failures: bool,
    cancellation: &CancellationToken,
    task_id: Option<&str>,
    mcp_session_id: Option<&str>,
) -> Result<Value, WorkspaceError> {
    if cancellation.is_cancelled() {
        return Err(cancelled_error(None));
    }
    let (program, mut args) =
        parse_and_resolve_execution(execution, cmd, cwd, ctx.workspace.root(), &ctx.policy)?;
    let execution_mode = execution_mode(execution);
    let start = Instant::now();

    // Retained-result capacity and live CPU capacity are separate concerns.
    // Check the former before spawning so an already-full result store cannot
    // briefly create a child only to kill it after insertion fails.
    ctx.sessions.ensure_capacity()?;
    let heavy_command = ctx.resources.is_cpu_intensive(&program, &args);
    let resource_lease = ctx.resources.acquire(heavy_command, cancellation).await?;
    let child_parallelism = resource_lease.parallelism();
    ctx.resources
        .clamp_parallel_args(&program, &mut args, child_parallelism);
    let execution_start_guard = ctx.sessions.execution_start_guard();
    ctx.sessions
        .ensure_execution_capacity(ctx.resources.max_running_commands())?;

    let harness_metadata = task_id
        .and_then(|task_id| ctx.harness.task(task_id).ok())
        .map(|task| SessionHarnessMetadata {
            task_id: task.id,
            command: cmd.to_string(),
            recovery_key: recovery_key.map(str::to_string),
            verification_kind: verification_kind.map(str::to_string),
            verification_key: verification_key.map(str::to_string),
            test_file: test_file.map(str::to_string),
            test_name: test_name.map(str::to_string),
            workspace_before,
            verification_level: verification_level.to_string(),
            supersede_previous_failures,
        });
    let owner_scope = ctx.command_owner_scope_for_session(mcp_session_id);
    let transport_session_id = mcp_session_id.map(str::to_string);
    let output_encoding = stream_encoding_for_program(&program);
    let durable_requested = execution
        .get("durable")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if durable_requested && tty {
        return Err(WorkspaceError::invalid_argument(
            "durable commands do not support tty mode",
        ));
    }

    let session = if durable_requested && ctx.sessions.durable_enabled() {
        let session_id = uuid::Uuid::new_v4().to_string();
        let started_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let execution_resources = resource_lease.to_value();
        let spec = DurableCommandSpec {
            schema_version: 1,
            session_id,
            command: cmd.to_string(),
            program: program.clone(),
            args: args.clone(),
            cwd: cwd.display().to_string(),
            timeout_ms: limit.as_millis().min(u64::MAX as u128) as u64,
            initial_stdin: stdin_text.to_string(),
            output_encoding,
            expected_exit_codes: expected_exit_codes.clone(),
            harness_metadata: harness_metadata.clone(),
            owner_scope: owner_scope.clone(),
            transport_session_id: transport_session_id.clone(),
            execution_resources: Some(execution_resources),
            started_at,
        };
        let job = ctx.sessions.create_durable_job(spec)?;
        let executable =
            durable_supervisor_executable().map_err(|error| WorkspaceError::ToolDetails {
                code: "DURABLE_COMMAND_LAUNCH_FAILED",
                message: format!("Failed to resolve durable supervisor executable: {error}"),
                category: "runtime",
                retryable: true,
                details: json!({"session_id": job.session_id()}),
            })?;
        let mut supervisor = Command::new(executable);
        supervisor
            .arg("exec-supervisor-run")
            .arg(job.spec_path())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        crate::platform::configure_durable_supervisor_tokio_process(&mut supervisor);
        apply_execution_environment(ctx, execution, &mut supervisor, &program, child_parallelism)?;
        let supervisor_child = match supervisor.spawn() {
            Ok(child) => child,
            Err(error) => {
                job.mark_launcher_failed(&format!("durable supervisor launch failed: {error}\n"));
                return Err(WorkspaceError::ToolDetails {
                    code: "DURABLE_COMMAND_LAUNCH_FAILED",
                    message: format!("Failed to launch durable command supervisor: {error}"),
                    category: "runtime",
                    retryable: true,
                    details: json!({"session_id": job.session_id()}),
                });
            }
        };
        let session = ExecSession::new_with_details_encoding_and_resources(
            supervisor_child,
            false,
            cmd.to_string(),
            cwd.display().to_string(),
            harness_metadata.clone(),
            owner_scope.clone(),
            transport_session_id.clone(),
            output_encoding,
            Some(resource_lease),
        )
        .with_expected_exit_codes(expected_exit_codes.clone())
        .attach_durable_job(job);
        match ctx.sessions.insert(session) {
            Ok(session) => session,
            Err(rejected) => {
                drop(execution_start_guard);
                rejected.mark_termination_reason("session_limit");
                rejected.kill_and_wait().await;
                return Err(ctx.sessions.capacity_error());
            }
        }
    } else {
        let mut command = command_for_program(&program, &args);
        crate::platform::configure_exec_tokio_process(&mut command);
        command
            .current_dir(platform_command_path(cwd))
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        apply_execution_environment(ctx, execution, &mut command, &program, child_parallelism)?;

        let child = command.spawn().map_err(|e| WorkspaceError::ToolDetails {
            code: "COMMAND_SPAWN_FAILED",
            message: format!("Failed to start command: {e}"),
            category: "runtime",
            retryable: true,
            details: json!({
                "termination_reason": "spawn_failed",
                "recoverable": true,
                "suggestion": "检查命令路径、权限和运行时环境后重试"
            }),
        })?;
        crate::platform::lower_exec_child_priority(&child);
        match ctx.sessions.insert(
            ExecSession::new_with_details_encoding_and_resources(
                child,
                tty,
                cmd.to_string(),
                cwd.display().to_string(),
                harness_metadata,
                owner_scope,
                transport_session_id,
                output_encoding,
                Some(resource_lease),
            )
            .with_expected_exit_codes(expected_exit_codes),
        ) {
            Ok(session) => session,
            Err(rejected) => {
                drop(execution_start_guard);
                rejected.mark_termination_reason("session_limit");
                rejected.kill_and_wait().await;
                return Err(ctx.sessions.capacity_error());
            }
        }
    };
    drop(execution_start_guard);
    session.spawn_readers().await;
    let deadline = start + limit;

    if yield_time.is_zero() {
        session.mark_externally_retained();
        let snapshot = session.snapshot(max_output);
        if !session.is_durable() {
            spawn_timeout_monitor(session.clone(), deadline);
        }
        return Ok(merge_exec_result(
            snapshot,
            start,
            cmd,
            cwd,
            true,
            execution_mode,
        ));
    }

    if !session.is_durable() && !tty && !stdin_text.is_empty() {
        let mut stdin_guard = session.stdin.lock().await;
        if let Some(stdin) = stdin_guard.as_mut() {
            use tokio::io::AsyncWriteExt;
            if !stdin_text.is_empty() {
                stdin
                    .write_all(stdin_text.as_bytes())
                    .await
                    .map_err(|_| WorkspaceError::Tool {
                        code: "SESSION_CLOSED",
                        message: "Failed to write stdin.".into(),
                        category: "runtime",
                        retryable: false,
                    })?;
            }
            let _ = stdin.shutdown().await;
        }
        *stdin_guard = None;
        session.mark_stdin_closed();
    }

    loop {
        session.refresh_status().await;
        if cancellation.is_cancelled() {
            session.mark_termination_reason("cancelled");
            session.kill_and_wait().await;
            session.refresh_status().await;
            session.wait_for_readers().await;
            session.mark_terminal_observed();
            let snapshot = session.snapshot(max_output);
            return Err(cancelled_error(Some(snapshot)));
        }
        if session.has_exited() {
            session.wait_for_readers().await;
            session.mark_terminal_observed();
            let snapshot = session.snapshot(max_output);
            return Ok(merge_exec_result(
                snapshot,
                start,
                cmd,
                cwd,
                false,
                execution_mode,
            ));
        }
        if !tty && Instant::now() >= deadline {
            session.mark_termination_reason("timeout");
            session.kill_and_wait().await;
            session.refresh_status().await;
            session.wait_for_readers().await;
            let snapshot = session.snapshot(max_output);
            return Err(WorkspaceError::ToolDetails {
                code: "TIMEOUT",
                message: "Command timed out.".into(),
                category: "runtime",
                retryable: true,
                details: json!({
                    "termination_reason": "timeout",
                    "recoverable": true,
                    "suggestion": "读取 output_refs，调整 timeout_ms 后重试",
                    "session": snapshot
                }),
            });
        }
        if Instant::now() - start >= yield_time || tty {
            if !tty && !yield_time.is_zero() {
                let grace =
                    COMPLETION_GRACE.min(deadline.saturating_duration_since(Instant::now()));
                let grace_deadline = Instant::now() + grace;
                while !session.has_exited()
                    && !cancellation.is_cancelled()
                    && Instant::now() < grace_deadline
                {
                    tokio::time::sleep(COMPLETION_POLL_INTERVAL).await;
                    session.refresh_status().await;
                }
                if cancellation.is_cancelled()
                    || (!session.has_exited() && Instant::now() >= deadline)
                {
                    continue;
                }
                if session.has_exited() {
                    session.wait_for_readers().await;
                    session.mark_terminal_observed();
                    let snapshot = session.snapshot(max_output);
                    return Ok(merge_exec_result(
                        snapshot,
                        start,
                        cmd,
                        cwd,
                        false,
                        execution_mode,
                    ));
                }
            }
            session.mark_externally_retained();
            let snapshot = session.snapshot(max_output);
            if !session.is_durable() {
                spawn_timeout_monitor(session.clone(), deadline);
            }
            return Ok(merge_exec_result(
                snapshot,
                start,
                cmd,
                cwd,
                true,
                execution_mode,
            ));
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn spawn_timeout_monitor(session: std::sync::Arc<ExecSession>, deadline: Instant) {
    crate::async_runtime::spawn(async move {
        loop {
            session.refresh_status().await;
            if session.has_exited() {
                session.wait_for_readers().await;
                break;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                session.mark_termination_reason("timeout");
                session.kill_and_wait().await;
                session.refresh_status().await;
                session.wait_for_readers().await;
                break;
            }
            tokio::time::sleep(remaining.min(Duration::from_millis(50))).await;
        }
    });
}

pub fn exec_health_check(ctx: &ToolContext) -> Result<Value, WorkspaceError> {
    let start = Instant::now();
    let cwd = ctx.workspace.root().to_path_buf();
    #[cfg(windows)]
    let probe = r#"cmd.exe /d /c "echo exec-health && echo exec-health-stderr 1>&2""#;
    #[cfg(not(windows))]
    let probe = r#"sh -c "printf exec-health; printf exec-health-stderr >&2""#;
    let execution = json!({"cmd": probe});

    let result = crate::async_runtime::block_on(run_command(
        ctx,
        &execution,
        probe,
        &cwd,
        Duration::from_secs(5),
        Duration::from_secs(5),
        16_384,
        false,
        "",
        None,
        None,
        None,
        None,
        None,
        capture_baseline_entries(&cwd),
        vec![0],
        "blocking",
        true,
        &CancellationToken::default(),
        None,
        None,
    ));

    let mut response = json!({
        "worker": {"alive": true},
        "session_create": false,
        "command_run": false,
        "stdout_capture": false,
        "stderr_capture": false,
        "duration_ms": start.elapsed().as_millis(),
        "next_actions": []
    });

    match result {
        Ok(snapshot) => {
            let session_created = snapshot.get("session_id").is_some();
            let command_run = snapshot.get("exit_code").and_then(Value::as_i64) == Some(0);
            let stdout_capture = snapshot
                .get("stdout")
                .and_then(Value::as_str)
                .is_some_and(|value| value.contains("exec-health"));
            let stderr_capture = snapshot
                .get("stderr")
                .and_then(Value::as_str)
                .is_some_and(|value| value.contains("exec-health-stderr"));
            let healthy = session_created && command_run && stdout_capture && stderr_capture;
            response["session_create"] = Value::Bool(session_created);
            response["command_run"] = Value::Bool(command_run);
            response["stdout_capture"] = Value::Bool(stdout_capture);
            response["stderr_capture"] = Value::Bool(stderr_capture);
            response["status"] = Value::String(if healthy { "success" } else { "error" }.into());
            response["summary"] = Value::String(if healthy {
                "exec worker、session、命令执行和 stdout/stderr 捕获均正常".into()
            } else {
                "exec health check 未通过，请查看 probe 结果".into()
            });
            response["probe"] = snapshot;
            if !healthy {
                response["next_actions"] = json!(["检查 exec worker 日志", "重启运行时"]);
            }
        }
        Err(error) => {
            response["status"] = Value::String("error".into());
            response["summary"] = Value::String("exec session 创建或探针执行失败".into());
            response["error"] = error.to_error_value();
            response["next_actions"] = json!(["检查 exec worker 日志", "重启运行时"]);
        }
    }
    response["duration_ms"] = json!(start.elapsed().as_millis());
    Ok(tool_ok(response))
}

fn execution_failure_result(error: &WorkspaceError, command: &str, cwd: &Path) -> Option<Value> {
    let code = match &error {
        WorkspaceError::Tool { code, .. } | WorkspaceError::ToolDetails { code, .. } => *code,
    };
    if !matches!(
        code,
        "COMMAND_REJECTED" | "COMMAND_SPAWN_FAILED" | "TIMEOUT"
    ) {
        return None;
    }

    let error_value = error.to_error_value();
    let details = error_value
        .get("details")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let termination_reason = match code {
        "TIMEOUT" => "timeout",
        "COMMAND_REJECTED" => "command_rejected",
        _ => "spawn_failed",
    };
    let status = if code == "TIMEOUT" {
        "exited"
    } else {
        termination_reason
    };
    let suggestion = details
        .get("suggestion")
        .and_then(Value::as_str)
        .unwrap_or(match code {
            "TIMEOUT" => "读取保留输出，调整 timeout_ms 后重试",
            "COMMAND_REJECTED" => "检查命令白名单、路径和 Workspace 执行策略",
            _ => "检查命令路径、权限和运行时环境后重试",
        });
    let mut result = details.get("session").cloned().unwrap_or_else(|| {
        json!({
            "status": status,
            "termination_reason": termination_reason,
            "recoverable": error_value["retryable"].as_bool().unwrap_or(false),
            "suggestion": suggestion,
            "exit_code": Value::Null,
            "stdout": "",
            "stderr": "",
            "stdout_truncated": false,
            "stderr_truncated": false,
            "duration_ms": 0,
            "elapsed_ms": 0,
            "warnings": []
        })
    });
    if let Some(object) = result.as_object_mut() {
        let elapsed_ms = object
            .get("elapsed_ms")
            .cloned()
            .unwrap_or_else(|| json!(0));
        object.entry("status").or_insert_with(|| json!(status));
        object
            .entry("termination_reason")
            .or_insert_with(|| json!(termination_reason));
        object
            .entry("recoverable")
            .or_insert_with(|| error_value["retryable"].clone());
        object
            .entry("suggestion")
            .or_insert_with(|| json!(suggestion));
        object.entry("exit_code").or_insert(Value::Null);
        object.entry("stdout").or_insert_with(|| json!(""));
        object.entry("stderr").or_insert_with(|| json!(""));
        object
            .entry("stdout_truncated")
            .or_insert_with(|| Value::Bool(false));
        object
            .entry("stderr_truncated")
            .or_insert_with(|| Value::Bool(false));
        object
            .entry("stdout_complete")
            .or_insert_with(|| Value::Bool(true));
        object
            .entry("stderr_complete")
            .or_insert_with(|| Value::Bool(true));
        object
            .entry("elapsed_ms")
            .or_insert_with(|| elapsed_ms.clone());
        object.entry("duration_ms").or_insert(elapsed_ms);
        object.entry("warnings").or_insert_with(|| json!([]));
        object.insert("command".into(), json!(command));
        object.insert("resolved_cwd".into(), json!(cwd.display().to_string()));
        object.insert("execution_mode".into(), json!("direct"));
        object.insert("filesystem_scope".into(), json!("workspace"));
        object.insert("sandbox_enforced".into(), Value::Bool(false));
        object.insert("execution_boundary".into(), json!("policy_only"));
        object.insert("child_process".into(), Value::Bool(code == "TIMEOUT"));
        object.insert("execution_started".into(), Value::Bool(code == "TIMEOUT"));
        object.insert("transport_ok".into(), Value::Bool(true));
        object.insert("command_ok".into(), Value::Bool(false));
        object.insert("error".into(), error_value);
    }
    Some(result)
}

fn merge_exec_result(
    mut snapshot: Value,
    start: Instant,
    command: &str,
    cwd: &Path,
    _keep_session: bool,
    execution_mode: &str,
) -> Value {
    if let Some(obj) = snapshot.as_object_mut() {
        let duration_ms = start.elapsed().as_millis();
        obj.insert("command".into(), json!(command));
        obj.insert("resolved_cwd".into(), json!(cwd.display().to_string()));
        obj.insert("duration_ms".into(), json!(duration_ms));
        obj.insert("elapsed_ms".into(), json!(duration_ms));
        obj.insert("transport_ok".into(), Value::Bool(true));
        let command_ok = obj.get("command_ok").and_then(Value::as_bool).or_else(|| {
            match obj
                .get("termination_reason")
                .and_then(Value::as_str)
                .unwrap_or("running")
            {
                "exited" => obj
                    .get("exit_code")
                    .and_then(Value::as_i64)
                    .map(|exit_code| exit_code == 0)
                    .or(Some(false)),
                "running" => None,
                _ => Some(false),
            }
        });
        obj.insert(
            "command_ok".into(),
            command_ok.map(Value::Bool).unwrap_or(Value::Null),
        );
        obj.insert("execution_mode".into(), json!(execution_mode));
        obj.insert("execution_started".into(), Value::Bool(true));
        obj.insert("warnings".into(), json!([]));
    }
    snapshot
}

fn parse_and_resolve(
    cmd: &str,
    cwd: &Path,
    workspace_root: &Path,
    policy: &crate::tools::policy::PolicySettings,
    toolchain_paths: &[PathBuf],
) -> Result<(String, Vec<String>), WorkspaceError> {
    let parts =
        crate::tools::policy::split_command_line(cmd).map_err(WorkspaceError::invalid_argument)?;
    if parts.is_empty() {
        return Err(WorkspaceError::invalid_argument("Empty command"));
    }

    let program = resolve_program(&parts[0], cwd, workspace_root, policy, toolchain_paths)?;
    Ok((program, parts[1..].to_vec()))
}

fn resolve_program(
    raw: &str,
    cwd: &Path,
    workspace_root: &Path,
    policy: &crate::tools::policy::PolicySettings,
    toolchain_paths: &[PathBuf],
) -> Result<String, WorkspaceError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(WorkspaceError::invalid_argument("Empty program"));
    }

    let explicit_path = trimmed.contains(['/', '\\']);
    let candidate = if Path::new(trimmed).is_absolute() {
        Path::new(trimmed).to_path_buf()
    } else {
        cwd.join(trimmed)
    };
    if candidate.is_file() {
        let resolved = candidate.canonicalize().map_err(|_| WorkspaceError::Tool {
            code: "COMMAND_REJECTED",
            message: format!("Program not found: {trimmed}"),
            category: "runtime",
            retryable: false,
        })?;
        let canonical_workspace =
            workspace_root
                .canonicalize()
                .map_err(|_| WorkspaceError::Tool {
                    code: "COMMAND_REJECTED",
                    message: "Workspace root is unavailable".into(),
                    category: "runtime",
                    retryable: true,
                })?;
        if !resolved.starts_with(&canonical_workspace) {
            return Err(WorkspaceError::Tool {
                code: "EXECUTABLE_OUTSIDE_WORKSPACE",
                message: format!("Workspace 外可执行文件被拒绝: {trimmed}"),
                category: "security",
                retryable: false,
            });
        }
        let extension = resolved
            .extension()
            .and_then(|value| value.to_str())
            .map(|value| format!(".{}", value.to_ascii_lowercase()))
            .unwrap_or_default();
        if policy.workspace_local_entries
            && (extension.is_empty() || policy.workspace_script_extensions.contains(&extension))
        {
            return Ok(resolved.to_string_lossy().into_owned());
        }
        return Err(WorkspaceError::Tool {
            code: "COMMAND_REJECTED",
            message: format!("Workspace 本地入口未获允许: {trimmed}"),
            category: "policy",
            retryable: false,
        });
    }

    if explicit_path {
        return Err(WorkspaceError::Tool {
            code: "COMMAND_REJECTED",
            message: format!("Program not found: {trimmed}"),
            category: "runtime",
            retryable: false,
        });
    }

    if !toolchain_paths.is_empty() {
        if !policy.workspace_local_entries {
            return Err(WorkspaceError::Tool {
                code: "COMMAND_REJECTED",
                message: "Workspace-local toolchain entries are disabled by policy".into(),
                category: "policy",
                retryable: false,
            });
        }
        for directory in toolchain_paths {
            let candidate = directory.join(trimmed);
            if candidate.is_file() {
                return Ok(candidate.to_string_lossy().into_owned());
            }
            #[cfg(windows)]
            for extension in ["exe", "cmd", "bat", "ps1"] {
                let candidate = directory.join(format!("{trimmed}.{extension}"));
                if candidate.is_file() {
                    return Ok(candidate.to_string_lossy().into_owned());
                }
            }
        }
    }

    resolve_effective_system_program(trimmed)
        .map(|path| path.to_string_lossy().into_owned())
        .ok_or_else(|| WorkspaceError::Tool {
            code: "COMMAND_REJECTED",
            message: format!("Program not found on PATH: {trimmed}"),
            category: "runtime",
            retryable: false,
        })
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use super::*;
    use crate::tools::context::ToolContext;
    use crate::tools::dispatch::{call_tool, call_tool_with_cancellation};
    use crate::tools::CancellationToken;
    use serde_json::json;
    use tempfile::tempdir;

    #[test]
    fn expected_exit_codes_accept_alias_and_reject_conflicting_aliases() {
        assert_eq!(
            parse_expected_exit_codes(&json!({})).expect("default exit codes"),
            vec![0]
        );
        assert_eq!(
            parse_expected_exit_codes(&json!({"expected_exit_codes": [1, 0, 1]}))
                .expect("expected exit codes"),
            vec![0, 1]
        );
        assert_eq!(
            parse_expected_exit_codes(&json!({"allowed_exit_codes": [0, 2]}))
                .expect("allowed exit codes"),
            vec![0, 2]
        );
        assert!(parse_expected_exit_codes(&json!({
            "expected_exit_codes": [0, 1],
            "allowed_exit_codes": [0]
        }))
        .is_err());
    }

    #[test]
    fn expected_nonzero_exit_code_is_a_successful_command_result() {
        let workspace = tempdir().expect("workspace");
        let harness = tempdir().expect("harness");
        let ctx =
            ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                .expect("context");
        #[cfg(windows)]
        let command = "python -c \"import sys; sys.exit(1)\"";
        #[cfg(not(windows))]
        let command = "python3 -c \"import sys; sys.exit(1)\"";
        let output = call_tool(
            &ctx,
            "exec_command",
            &json!({
                "cmd": command,
                "expected_exit_codes": [0, 1],
                "timeout_ms": 10_000,
                "yield_time_ms": 10_000
            }),
        );
        assert_eq!(output["ok"], true, "{output}");
        assert_eq!(output["command_ok"], true, "{output}");
        assert_eq!(output["success"], true, "{output}");
        assert_eq!(output["exit_code"], 1, "{output}");
        assert!(output.get("error").is_none(), "{output}");
    }

    #[test]
    fn immediate_terminal_command_is_observed_but_retains_readable_output_refs() {
        let workspace = tempdir().expect("workspace");
        let harness = tempdir().expect("harness");
        let ctx =
            ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                .expect("context");
        #[cfg(windows)]
        let command = "python -c \"print('terminal-output')\"";
        #[cfg(not(windows))]
        let command = "python3 -c \"print('terminal-output')\"";

        let output = call_tool(
            &ctx,
            "exec_command",
            &json!({
                "cmd": command,
                "timeout_ms": 10_000,
                "yield_time_ms": 10_000
            }),
        );
        assert_eq!(output["ok"], true, "{output}");
        assert_eq!(output["execution_status"], "succeeded", "{output}");
        assert_eq!(output["result_observed"], true, "{output}");
        let session_id = output["session_id"].as_str().expect("session id");
        let retained = ctx
            .sessions
            .get(session_id)
            .expect("retained terminal session");
        assert!(retained.terminal_observed());

        let stdout_ref = output["output_refs"]["stdout"]
            .as_str()
            .expect("stdout ref");
        let read = call_tool(
            &ctx,
            "read_output",
            &json!({"output_ref": stdout_ref, "offset": 0, "limit": 1024}),
        );
        assert_eq!(read["ok"], true, "{read}");
        assert!(
            read["content"]
                .as_str()
                .is_some_and(|content| content.contains("terminal-output")),
            "{read}"
        );

        let sessions = call_tool(
            &ctx,
            "list_command_sessions",
            &json!({"include_terminal": true, "max_output_bytes": 0}),
        );
        assert_eq!(sessions["pending_result_count"], 0, "{sessions}");
    }

    #[cfg(unix)]
    #[test]
    fn nvm_default_alias_selects_the_configured_installed_version() {
        let root = tempdir().expect("nvm root");
        for version in ["v20.18.1", "v24.18.0"] {
            std::fs::create_dir_all(root.path().join("versions/node").join(version).join("bin"))
                .expect("nvm version bin");
        }
        std::fs::create_dir_all(root.path().join("alias")).expect("alias dir");
        std::fs::write(root.path().join("alias/default"), "20\n").expect("default alias");

        assert_eq!(
            resolve_nvm_default_bin(root.path()),
            Some(root.path().join("versions/node/v20.18.1/bin"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn nvm_default_alias_can_follow_a_bounded_named_alias() {
        let root = tempdir().expect("nvm root");
        std::fs::create_dir_all(root.path().join("versions/node/v24.18.0/bin"))
            .expect("nvm version bin");
        std::fs::create_dir_all(root.path().join("alias/lts")).expect("alias dir");
        std::fs::write(root.path().join("alias/default"), "lts/hydrogen\n").expect("default alias");
        std::fs::write(root.path().join("alias/lts/hydrogen"), "v24.18.0\n").expect("named alias");

        assert_eq!(
            resolve_nvm_default_bin(root.path()),
            Some(root.path().join("versions/node/v24.18.0/bin"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn nvm_without_default_only_uses_an_unambiguous_single_version() {
        let root = tempdir().expect("nvm root");
        std::fs::create_dir_all(root.path().join("versions/node/v22.12.0/bin"))
            .expect("single version");
        assert_eq!(
            resolve_nvm_default_bin(root.path()),
            Some(root.path().join("versions/node/v22.12.0/bin"))
        );

        std::fs::create_dir_all(root.path().join("versions/node/v24.18.0/bin"))
            .expect("second version");
        assert_eq!(resolve_nvm_default_bin(root.path()), None);
    }

    #[cfg(windows)]
    #[test]
    fn preferred_pwsh_applies_only_when_shell_is_omitted() {
        let policy = crate::tools::policy::PolicySettings {
            preferred_shell: "pwsh".into(),
            ..Default::default()
        };
        let mut args = json!({"cmd": "Write-Output $env:TEMP"});
        apply_preferred_shell(&mut args, &policy).expect("preferred shell");
        assert_eq!(args["shell"], "pwsh");
        assert_eq!(args["args"][3], "-Command");
        assert_eq!(args["args"][4], "Write-Output $env:TEMP");
        normalize_exec_arguments(&mut args).expect("normalize");
        assert!(args["cmd"].as_str().unwrap().contains("pwsh"));

        let mut explicit = json!({"shell": "cmd", "args": ["/d", "/c", "echo explicit"]});
        apply_preferred_shell(&mut explicit, &policy).expect("explicit shell");
        assert_eq!(explicit["shell"], "cmd");
    }

    fn assert_failure_result(
        error: WorkspaceError,
        expected_code: &str,
        expected_status: &str,
        expected_reason: &str,
    ) {
        let result = finalize_execution_result(
            execution_failure_result(&error, "missing-command", Path::new("C:/workspace"))
                .expect("应转换为统一执行结果"),
        );
        jsonschema::validator_for(&crate::tools::registry::output_schema("exec_command"))
            .expect("exec output schema")
            .validate(&result)
            .expect("failure result must satisfy exec output schema");
        assert_eq!(result["ok"], false);
        assert_eq!(result["transport_ok"], true);
        assert_eq!(result["transport_status"], "ok");
        assert_eq!(result["success"], false);
        assert!(result["execution_status"].is_string());
        assert_eq!(result["session_id"], Value::Null);
        assert_eq!(result["command_ok"], false);
        assert_eq!(result["status"], expected_status);
        assert_eq!(result["termination_reason"], expected_reason);
        assert_eq!(result["child_process"], false);
        assert_eq!(result["execution_started"], false);
        assert_eq!(result["error"]["code"], expected_code);
        assert!(result["suggestion"].is_string());
        assert!(result["duration_ms"].is_u64());
        assert!(result["elapsed_ms"].is_u64());
        assert!(result["warnings"].is_array());
    }

    #[test]
    fn 程序不存在时返回统一执行结果() {
        assert_failure_result(
            WorkspaceError::Tool {
                code: "COMMAND_REJECTED",
                message: "Program not found on PATH: missing-command".into(),
                category: "runtime",
                retryable: false,
            },
            "COMMAND_REJECTED",
            "command_rejected",
            "command_rejected",
        );
    }

    #[test]
    fn 启动失败时返回统一执行结果() {
        assert_failure_result(
            WorkspaceError::ToolDetails {
                code: "COMMAND_SPAWN_FAILED",
                message: "Failed to start command".into(),
                category: "runtime",
                retryable: true,
                details: json!({"recoverable": true}),
            },
            "COMMAND_SPAWN_FAILED",
            "spawn_failed",
            "spawn_failed",
        );
    }

    #[test]
    fn resolves_an_arbitrarily_named_workspace_local_entry() {
        let workspace = tempdir().expect("workspace");
        let entry = workspace.path().join("scripts").join("anything.cmd");
        std::fs::create_dir_all(entry.parent().expect("parent")).expect("scripts");
        std::fs::write(&entry, "echo test").expect("entry");
        let resolved = resolve_program(
            "scripts/anything.cmd",
            workspace.path(),
            workspace.path(),
            &crate::tools::policy::PolicySettings::default(),
            &[],
        )
        .expect("workspace entry resolves");
        assert_eq!(
            std::path::Path::new(&resolved),
            entry.canonicalize().unwrap()
        );
    }

    #[test]
    fn skill_script_requires_operator_dangerous_mode() {
        let workspace = tempdir().expect("workspace");
        let harness = tempdir().expect("harness");
        let skill = workspace.path().join(".agents/skills/example");
        std::fs::create_dir_all(skill.join("scripts")).expect("skill scripts");
        std::fs::write(
            skill.join("SKILL.md"),
            "---\nname: example\ndescription: Example skill.\n---\nUse it.\n",
        )
        .expect("skill");
        std::fs::write(skill.join("scripts/run.py"), "print('ok')\n").expect("script");
        let mut ctx =
            ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                .expect("context");

        let output = call_tool(
            &ctx,
            "exec_command",
            &json!({"cmd": "python .agents/skills/example/scripts/run.py"}),
        );
        assert_eq!(output["ok"], false, "{output}");
        assert_eq!(
            output["error"]["code"], "SKILL_SCRIPT_REQUIRES_DANGEROUS_MODE",
            "{output}"
        );
        assert_eq!(output["error"]["details"]["skill"], "example");
        assert_eq!(output["error"]["details"]["script"], "scripts/run.py");

        let indirect = call_tool(
            &ctx,
            "exec_command",
            &json!({
                "cmd": "python -c \"import runpy; runpy.run_path('run.py')\"",
                "workdir": ".agents/skills/example/scripts"
            }),
        );
        assert_eq!(
            indirect["error"]["code"], "SKILL_SCRIPT_REQUIRES_DANGEROUS_MODE",
            "{indirect}"
        );

        ctx.permission_mode = "dangerous".into();
        ctx.policy.permission_mode = "dangerous".into();
        let approved_by_control_plane = call_tool(
            &ctx,
            "exec_command",
            &json!({"cmd": "python .agents/skills/example/scripts/run.py"}),
        );
        assert_eq!(
            approved_by_control_plane["ok"], true,
            "{approved_by_control_plane}"
        );

        std::fs::write(skill.join("scripts/run.py"), "print('changed')\n").expect("change");
        let stale = call_tool(
            &ctx,
            "exec_command",
            &json!({
                "cmd": "python .agents/skills/example/scripts/run.py"
            }),
        );
        assert_eq!(stale["ok"], false, "{stale}");
        assert_eq!(
            stale["error"]["code"], "SKILL_SCRIPT_SNAPSHOT_STALE",
            "{stale}"
        );
    }

    #[test]
    fn cancellation_terminates_a_running_command() {
        if which::which("python").is_err() {
            return;
        }
        let workspace = tempdir().expect("workspace");
        let harness = tempdir().expect("harness");
        let ctx = Arc::new(
            ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                .expect("context"),
        );
        let token = CancellationToken::default();
        let worker_ctx = ctx.clone();
        let worker_token = token.clone();
        let worker = std::thread::spawn(move || {
            call_tool_with_cancellation(
                worker_ctx.as_ref(),
                "exec_command",
                &json!({
                    "cmd": "python -c \"import time; time.sleep(30)\"",
                    "timeout_ms": 60_000,
                    "yield_time_ms": 30_000
                }),
                &worker_token,
            )
        });

        std::thread::sleep(Duration::from_millis(250));
        token.cancel();
        let output = worker.join().expect("worker");
        assert_eq!(output["ok"], false, "{output}");
        assert_eq!(output["error"]["code"], "REQUEST_CANCELLED", "{output}");
        assert_eq!(
            output["error"]["details"]["termination_reason"], "cancelled",
            "{output}"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_scripts_use_their_platform_runners() {
        let batch = command_for_program("C:/workspace/run-anything.cmd", &[]);
        assert_eq!(batch.as_std().get_program().to_string_lossy(), "cmd.exe");
        assert!(batch.as_std().get_args().any(|arg| arg == "/c"));
        assert_eq!(
            windows_batch_command_line(
                r"\\?\C:\workspace\Life Brain\run & tooling.cmd",
                &["argument & value".to_string()]
            ),
            r#"chcp 65001>nul & call "C:\workspace\Life Brain\run & tooling.cmd" "argument & value""#
        );

        let script = command_for_program("C:/workspace/run-anything.ps1", &[]);
        let runner = script
            .as_std()
            .get_program()
            .to_string_lossy()
            .to_ascii_lowercase();
        assert!(runner.contains("powershell") || runner.contains("pwsh"));
        assert!(script.as_std().get_args().any(|arg| arg == "-File"));
    }

    #[cfg(windows)]
    #[test]
    fn windows_workspace_scripts_and_python_unicode_execute_successfully() {
        let workspace = tempdir().expect("workspace");
        let harness = tempdir().expect("harness");
        std::fs::write(
            workspace.path().join("any-name.cmd"),
            "@echo off\r\npython -c \"print('批处理中文✓│')\"\r\n",
        )
        .expect("cmd script");
        std::fs::write(
            workspace.path().join("any-name.ps1"),
            "Write-Output 'tooling-powershell-ok'\r\n",
        )
        .expect("powershell script");
        std::fs::write(
            workspace.path().join("workflow_probe.py"),
            "print('workflow-ok')\n",
        )
        .expect("python module");
        let mut ctx =
            ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                .expect("context");
        ctx.policy.allowed_commands.insert("any-name".into());

        for command in [
            "any-name.cmd",
            "any-name.ps1",
            "cmd /c echo tooling-cmd-ok",
            "powershell -NoProfile -Command \"Write-Output tooling-powershell-ok\"",
            "python -c \"print('中文输出正常 ✅')\"",
        ] {
            let output = call_tool(
                &ctx,
                "exec_command",
                &json!({ "cmd": command, "timeout_ms": 10_000, "yield_time_ms": 10_000 }),
            );
            assert_eq!(output["ok"], true, "{command}: {output}");
            assert_eq!(output["command_ok"], true, "{command}: {output}");
        }

        for _ in 0..10 {
            let output = call_tool(
                &ctx,
                "exec_command",
                &json!({ "cmd": "python -m workflow_probe", "timeout_ms": 10_000 }),
            );
            assert_eq!(output["command_ok"], true, "{output}");
            assert!(output["stdout"]
                .as_str()
                .unwrap_or_default()
                .contains("workflow-ok"));
        }

        let batch_unicode = call_tool(
            &ctx,
            "exec_command",
            &json!({ "cmd": "any-name.cmd", "timeout_ms": 10_000, "yield_time_ms": 10_000 }),
        );
        assert_eq!(batch_unicode["command_ok"], true, "{batch_unicode}");
        assert!(
            batch_unicode["stdout"]
                .as_str()
                .unwrap_or_default()
                .contains("批处理中文✓│"),
            "{batch_unicode}"
        );

        let cmd_unicode = call_tool(
            &ctx,
            "exec_command",
            &json!({ "cmd": "cmd /d /c echo 中文输出", "timeout_ms": 10_000, "yield_time_ms": 10_000 }),
        );
        assert_eq!(cmd_unicode["command_ok"], true, "{cmd_unicode}");
        assert!(
            cmd_unicode["stdout"]
                .as_str()
                .unwrap_or_default()
                .contains("中文输出"),
            "{cmd_unicode}"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_batch_scripts_preserve_space_paths_and_arguments() {
        let parent = tempdir().expect("workspace parent");
        let workspace = parent.path().join("Life Brain 中文");
        std::fs::create_dir_all(&workspace).expect("workspace");
        let harness = tempdir().expect("harness");
        let mut ctx = ToolContext::for_test(workspace.clone(), harness.path().to_path_buf())
            .expect("context");
        ctx.policy.allowed_commands.insert("run & tooling".into());

        for extension in ["cmd", "bat"] {
            let script_name = format!("run & tooling.{extension}");
            std::fs::write(
                workspace.join(&script_name),
                "@echo off\r\nif not \"%~1\"==\"argument & value\" exit /b 7\r\necho tooling-space-path-ok\r\n",
            )
            .expect("batch script");

            let command = format!(r#""{script_name}" "argument & value""#);
            let output = call_tool(
                &ctx,
                "exec_command",
                &json!({ "cmd": command, "timeout_ms": 10_000, "yield_time_ms": 10_000 }),
            );
            assert_eq!(output["command_ok"], true, "{script_name}: {output}");
            let stdout = output["stdout"].as_str().unwrap_or_default();
            assert!(
                stdout.contains("tooling-space-path-ok"),
                "{script_name}: {output}"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_program_resolution_follows_winget_style_symlink() {
        let directory = tempfile::tempdir().expect("program directory");
        let target = directory.path().join("real-rg.exe");
        std::fs::write(&target, b"stub").expect("target executable");
        let link = directory.path().join("rg.exe");
        if std::os::windows::fs::symlink_file(&target, &link).is_err() {
            return;
        }

        assert_eq!(resolve_system_program_path(&link), target);
    }

    #[cfg(windows)]
    #[test]
    fn windows_cargo_proxy_resolves_to_the_real_rustup_toolchain() {
        let Some(real_cargo) = rustup_tool_path("cargo") else {
            return;
        };
        let Ok(proxy) = which::which("cargo") else {
            return;
        };
        let resolved = resolve_system_program_path(&proxy);
        assert_eq!(resolved, PathBuf::from(real_cargo));
        assert!(resolved.is_file());
    }

    #[cfg(windows)]
    #[test]
    fn windows_cargo_environment_exposes_toolchain_subcommands() {
        let Some(real_cargo) = rustup_tool_path("cargo") else {
            return;
        };
        let toolchain_bin = Path::new(real_cargo).parent().expect("toolchain bin");
        let mut command = Command::new(real_cargo);

        configure_windows_command_environment(&mut command, real_cargo);

        let path = command
            .as_std()
            .get_envs()
            .find_map(|(key, value)| {
                key.eq_ignore_ascii_case("PATH")
                    .then(|| value.map(std::ffi::OsStr::to_os_string))
                    .flatten()
            })
            .expect("configured PATH");
        assert_eq!(
            std::env::split_paths(&path).next().as_deref(),
            Some(toolchain_bin)
        );
        assert_eq!(
            command
                .as_std()
                .get_envs()
                .find_map(|(key, value)| (key == "RUSTC").then_some(value))
                .flatten(),
            rustup_tool_path("rustc").map(std::ffi::OsStr::new)
        );
    }

    #[test]
    fn read_only_command_does_not_claim_a_workspace_mutation() {
        let workspace = tempdir().expect("workspace");
        let harness = tempdir().expect("harness");
        let ctx =
            ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                .expect("context");
        let output = call_tool(&ctx, "exec_command", &json!({"cmd": "echo read-only"}));
        assert_eq!(output["command_ok"], true, "{output}");
        assert_eq!(output["mutation_attributed"], false, "{output}");
        assert_eq!(output["affected_files"], json!([]), "{output}");
    }

    #[cfg(unix)]
    #[test]
    fn unix_workspace_scripts_preserve_space_paths_and_arguments() {
        use std::os::unix::fs::PermissionsExt;

        let parent = tempdir().expect("workspace parent");
        let workspace = parent.path().join("Life Brain 中文");
        std::fs::create_dir_all(&workspace).expect("workspace");
        let harness = tempdir().expect("harness");
        let script_name = "run tooling";
        let script_path = workspace.join(script_name);
        std::fs::write(
            &script_path,
            "#!/bin/sh\nprintf 'tooling-space-path-ok\\n'\nprintf 'argument=[%s]\\n' \"$1\"\n",
        )
        .expect("shell script");
        let mut permissions = std::fs::metadata(&script_path)
            .expect("script metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&script_path, permissions).expect("script executable");

        let mut ctx =
            ToolContext::for_test(workspace, harness.path().to_path_buf()).expect("context");
        ctx.policy.allowed_commands.insert(script_name.into());
        let command = format!(r#""{script_name}" "argument with spaces""#);
        let output = call_tool(
            &ctx,
            "exec_command",
            &json!({ "cmd": command, "timeout_ms": 10_000, "yield_time_ms": 10_000 }),
        );
        assert_eq!(output["command_ok"], true, "{output}");
        let stdout = output["stdout"].as_str().unwrap_or_default();
        assert!(stdout.contains("tooling-space-path-ok"), "{output}");
        assert!(
            stdout.contains("argument=[argument with spaces]"),
            "{output}"
        );
    }
}

fn command_for_program(program: &str, args: &[String]) -> Command {
    #[cfg(windows)]
    {
        let extension = Path::new(program)
            .extension()
            .and_then(|value| value.to_str())
            .map(str::to_ascii_lowercase);
        match extension.as_deref() {
            Some("bat") | Some("cmd") => {
                let mut command = Command::new("cmd.exe");
                command.args(["/d", "/s", "/c"]);
                command
                    .as_std_mut()
                    .raw_arg(windows_batch_command_line(program, args));
                return command;
            }
            Some("ps1") => {
                let shell = which::which("pwsh")
                    .or_else(|_| which::which("powershell"))
                    .unwrap_or_else(|_| std::path::PathBuf::from("powershell.exe"));
                let mut command = Command::new(shell);
                command
                    .args([
                        "-NoLogo",
                        "-NoProfile",
                        "-NonInteractive",
                        "-ExecutionPolicy",
                        "Bypass",
                        "-File",
                        windows_command_path(program).as_str(),
                    ])
                    .args(args);
                return command;
            }
            _ => {}
        }
    }

    let mut command = Command::new(program);
    command.args(args);
    command
}

#[cfg(windows)]
fn windows_batch_command_line(program: &str, args: &[String]) -> String {
    let mut command_line = String::from("chcp 65001>nul & call ");
    command_line.push_str(&windows_batch_token(&windows_command_path(program)));
    for arg in args {
        command_line.push(' ');
        command_line.push_str(&windows_batch_token(arg));
    }
    command_line
}

#[cfg(windows)]
fn windows_batch_token(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

#[cfg(windows)]
fn stream_encoding_for_program(program: &str) -> StreamEncoding {
    let path = Path::new(program);
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if stem == "cmd" {
        return StreamEncoding::WindowsOem;
    }
    StreamEncoding::Utf8
}

#[cfg(not(windows))]
fn stream_encoding_for_program(_program: &str) -> StreamEncoding {
    StreamEncoding::Utf8
}

fn platform_command_path(path: &Path) -> std::path::PathBuf {
    #[cfg(windows)]
    {
        std::path::PathBuf::from(windows_command_path(&path.to_string_lossy()))
    }
    #[cfg(not(windows))]
    path.to_path_buf()
}

#[cfg(windows)]
fn windows_command_path(path: &str) -> String {
    path.strip_prefix("\\\\?\\").unwrap_or(path).to_string()
}
