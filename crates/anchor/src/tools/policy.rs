use std::collections::HashSet;
use std::path::{Component, Path};

use serde_json::Value;

use crate::tools::workspace::Workspace;
use crate::workspace::ActionsConfig;

use super::registry::is_allowed_tool;

static NETWORK_COMMAND_PATTERN: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
static DANGEROUS_COMMAND_PATTERN: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
static INTERPRETER_MUTATION_PATTERN: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
static POSIX_ABSOLUTE_PATH_PATTERN: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
static WINDOWS_ABSOLUTE_PATH_PATTERN: std::sync::OnceLock<regex::Regex> =
    std::sync::OnceLock::new();

const BASIC_READ_ONLY_COMMANDS: &[&str] = &[
    "pwd", "ls", "dir", "cat", "head", "tail", "grep", "find", "which", "echo",
];

const DEFAULT_ALLOWED_COMMANDS: &[&str] = &[
    "pytest",
    "python",
    "python3",
    "npm",
    "npx",
    "node",
    "pnpm",
    "corepack",
    "yarn",
    "make",
    "mvn",
    "mvnw",
    "gradle",
    "gradlew",
    "cargo",
    "rustup",
    "go",
    "golangci-lint",
    "ruff",
    "rg",
    "ripgrep",
    "mypy",
    "eslint",
    "tsc",
    "msbuild",
    "dotnet",
    "deno",
    "bun",
    "ruby",
    "java",
    "javac",
    "cmake",
    "clang",
    "gcc",
    "g++",
    "git",
    "cmd",
    "powershell",
    "pwsh",
];

#[derive(Debug, Clone)]
pub struct PolicySettings {
    pub allowed_commands: HashSet<String>,
    pub workspace_local_entries: bool,
    pub workspace_script_extensions: HashSet<String>,
    pub max_patch_bytes: usize,
    pub permission_mode: String,
    pub preferred_shell: String,
    pub external_paid_commands_enabled: bool,
    pub external_paid_max_runs_per_day: u64,
    pub external_paid_max_duration_seconds: u64,
}

fn safe_regex_pattern_literal(source: &str, start: usize) -> bool {
    let mut before = source[..start].trim_end().to_ascii_lowercase();
    // Python string prefixes occur immediately before the quote. Strip only a
    // bounded prefix sequence, then require a known regex-pattern call site.
    for _ in 0..3 {
        if before
            .chars()
            .last()
            .is_some_and(|ch| matches!(ch, 'r' | 'b' | 'u' | 'f'))
        {
            before.pop();
        } else {
            break;
        }
    }
    [
        "re.compile(",
        "re.search(",
        "re.match(",
        "re.fullmatch(",
        "re.findall(",
        "re.finditer(",
        "re.split(",
        "re.sub(",
        "re.subn(",
        "regex.compile(",
        "regex.search(",
        "regex.match(",
        "regexp(",
        "new regexp(",
    ]
    .iter()
    .any(|call| before.ends_with(call))
}

impl Default for PolicySettings {
    fn default() -> Self {
        Self {
            allowed_commands: default_allowed_command_set(),
            workspace_local_entries: true,
            workspace_script_extensions: default_workspace_script_extension_set(),
            max_patch_bytes: 200_000,
            permission_mode: "trusted".into(),
            preferred_shell: "auto".into(),
            external_paid_commands_enabled: false,
            external_paid_max_runs_per_day: 1,
            external_paid_max_duration_seconds: 1800,
        }
    }
}

impl PolicySettings {
    pub fn from_runtime(runtime: &crate::workspace::RuntimeConfig) -> Self {
        Self {
            allowed_commands: merge_default_allowed_commands(&runtime.allowed_commands),
            workspace_local_entries: runtime.workspace_local_entries,
            workspace_script_extensions: parse_workspace_script_extensions(
                &runtime.workspace_script_extensions,
            ),
            max_patch_bytes: 200_000,
            permission_mode: runtime.permission_mode.clone(),
            preferred_shell: runtime.preferred_shell.clone(),
            external_paid_commands_enabled: runtime.external_paid_commands_enabled,
            external_paid_max_runs_per_day: runtime.external_paid_max_runs_per_day.max(1),
            external_paid_max_duration_seconds: runtime
                .external_paid_max_duration_seconds
                .clamp(1, 3600),
        }
    }

    pub fn from_actions_config(actions: &ActionsConfig) -> Self {
        Self {
            allowed_commands: merge_default_allowed_commands(&actions.allowed_commands),
            workspace_local_entries: true,
            workspace_script_extensions: default_workspace_script_extension_set(),
            max_patch_bytes: actions.max_patch_bytes as usize,
            permission_mode: actions.permission_mode.clone(),
            preferred_shell: "auto".into(),
            external_paid_commands_enabled: false,
            external_paid_max_runs_per_day: 1,
            external_paid_max_duration_seconds: 1800,
        }
    }

    pub fn network_allowed(&self) -> bool {
        self.permission_mode == "trusted" || self.permission_mode == "dangerous"
    }

    pub fn skip_permission_gates(&self) -> bool {
        self.permission_mode == "dangerous"
    }
}

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct PolicyError(pub String);

pub(crate) fn split_command_line(command: &str) -> Result<Vec<String>, String> {
    #[cfg(windows)]
    {
        split_windows_command_line(command)
    }
    #[cfg(not(windows))]
    {
        shell_words::split(command).map_err(|_| "Invalid command syntax".to_string())
    }
}

#[cfg(windows)]
fn split_windows_command_line(command: &str) -> Result<Vec<String>, String> {
    let chars = command.chars().collect::<Vec<_>>();
    let mut parts = Vec::new();
    let mut index = 0usize;
    while index < chars.len() {
        while index < chars.len() && chars[index].is_whitespace() {
            index += 1;
        }
        if index >= chars.len() {
            break;
        }

        let mut value = String::new();
        let mut started = false;
        let mut in_double_quotes = false;
        let mut in_single_quotes = false;
        while index < chars.len() {
            let current = chars[index];
            if !in_double_quotes && !in_single_quotes && current.is_whitespace() {
                break;
            }
            if current == '\\' && !in_single_quotes {
                let start = index;
                while index < chars.len() && chars[index] == '\\' {
                    index += 1;
                }
                let count = index - start;
                if index < chars.len() && chars[index] == '"' {
                    value.extend(std::iter::repeat_n('\\', count / 2));
                    if count.is_multiple_of(2) {
                        in_double_quotes = !in_double_quotes;
                    } else {
                        value.push('"');
                    }
                    started = true;
                    index += 1;
                } else {
                    value.extend(std::iter::repeat_n('\\', count));
                    started = true;
                }
                continue;
            }
            if current == '"' && !in_single_quotes {
                in_double_quotes = !in_double_quotes;
                started = true;
                index += 1;
                continue;
            }
            if current == '\'' && !in_double_quotes {
                in_single_quotes = !in_single_quotes;
                started = true;
                index += 1;
                continue;
            }
            value.push(current);
            started = true;
            index += 1;
        }
        if in_double_quotes || in_single_quotes {
            return Err("Invalid command syntax: unterminated quote".to_string());
        }
        if started {
            parts.push(value);
        }
    }
    Ok(parts)
}

pub fn parse_allowed_commands(configured: &str) -> HashSet<String> {
    let trimmed = configured.trim();
    if trimmed.is_empty() {
        return default_allowed_command_set();
    }
    let mut commands: HashSet<String> = trimmed
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    // 基础诊断命令是工作区可用性的最低保障，不应因 Actions 配置遗漏而失效。
    commands.extend(BASIC_READ_ONLY_COMMANDS.iter().map(|s| s.to_string()));
    commands
}

pub fn parse_workspace_script_extensions(configured: &str) -> HashSet<String> {
    let mut extensions = configured
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            if value.starts_with('.') {
                value.to_ascii_lowercase()
            } else {
                format!(".{}", value.to_ascii_lowercase())
            }
        })
        .collect::<HashSet<_>>();
    if extensions.is_empty() {
        extensions = default_workspace_script_extension_set();
    }
    extensions
}

fn default_allowed_command_set() -> HashSet<String> {
    DEFAULT_ALLOWED_COMMANDS
        .iter()
        .map(|s| s.to_string())
        .chain(BASIC_READ_ONLY_COMMANDS.iter().map(|s| s.to_string()))
        .collect()
}

fn merge_default_allowed_commands(configured: &str) -> HashSet<String> {
    let mut commands = default_allowed_command_set();
    commands.extend(parse_allowed_commands(configured));
    commands
}

fn default_workspace_script_extension_set() -> HashSet<String> {
    [".exe", ".bat", ".cmd", ".ps1"]
        .into_iter()
        .map(str::to_string)
        .collect()
}

pub fn validate_tool_arguments(
    tool_name: &str,
    arguments: &Value,
    policy: &PolicySettings,
) -> Result<(), PolicyError> {
    validate_tool_arguments_for_workspace(tool_name, arguments, policy, None)
}

pub fn validate_tool_arguments_for_workspace(
    tool_name: &str,
    arguments: &Value,
    policy: &PolicySettings,
    workspace: Option<&Workspace>,
) -> Result<(), PolicyError> {
    match tool_name {
        "exec_command" => validate_command_for_workspace(arguments, policy, workspace),
        "apply_patch" | "patch_check" => validate_patch(arguments, policy),
        _ => Ok(()),
    }
}

/// Actions OpenAPI 暴露层校验：仅限制「能否调用」，不参与执行逻辑。
pub fn validate_actions_exposure(tool_name: &str) -> Result<(), PolicyError> {
    if is_allowed_tool(tool_name) {
        Ok(())
    } else {
        Err(PolicyError(format!("Tool is not exposed: {tool_name}")))
    }
}

pub fn validate_command(arguments: &Value, policy: &PolicySettings) -> Result<(), PolicyError> {
    validate_command_for_workspace(arguments, policy, None)
}

pub fn validate_command_for_workspace(
    arguments: &Value,
    policy: &PolicySettings,
    workspace: Option<&Workspace>,
) -> Result<(), PolicyError> {
    let command = arguments
        .get("cmd")
        .and_then(Value::as_str)
        .ok_or_else(|| PolicyError("exec_command requires a non-empty cmd".into()))?;
    if command.trim().is_empty() {
        return Err(PolicyError("exec_command requires a non-empty cmd".into()));
    }
    let structured_invocation =
        arguments.get("executable").is_some() || arguments.get("shell").is_some();
    if !structured_invocation && command.len() > 4_000 {
        return Err(PolicyError("Command is too long".into()));
    }
    let filesystem_scope = arguments
        .get("filesystem_scope")
        .and_then(Value::as_str)
        .unwrap_or("workspace");
    if filesystem_scope != "workspace" {
        return Err(PolicyError(
            "EXTERNAL_EXECUTION_NOT_ALLOWED: exec_command 只允许在 Workspace 内执行".into(),
        ));
    }
    for key in ["workdir", "cwd"] {
        if let Some(workdir) = arguments.get(key).and_then(Value::as_str) {
            let path = Path::new(workdir);
            if path.is_absolute() || path.components().any(|part| part == Component::ParentDir) {
                return Err(PolicyError(
                    "workdir must stay inside the configured workspace".into(),
                ));
            }
        }
    }
    let structured_direct_parts = structured_direct_command_parts(arguments);
    let parts = match structured_direct_parts.clone() {
        Some(parts) => parts,
        None => split_command_line(command).map_err(PolicyError)?,
    };
    let structured_direct_literal_argv = structured_direct_parts
        .as_ref()
        .and_then(|parts| parts.first())
        .is_some_and(|program| !program_uses_shell_syntax(program));
    if !structured_direct_literal_argv && has_forbidden_shell_syntax(command) {
        return Err(PolicyError(
            "Shell chaining, redirection and expansion are not allowed".into(),
        ));
    }
    if parts.is_empty() {
        return Err(PolicyError("Empty command".into()));
    }
    if workspace.is_some_and(Workspace::strict_read_boundary)
        && command_parts_contain_external_path(&parts)
    {
        return Err(PolicyError(
            "WORKSPACE_PATH_PROTECTED: Gateway workspace scope 禁止通过子进程访问绝对路径或父目录路径"
                .into(),
        ));
    }
    if (dangerous_command_pattern().is_match(command)
        || interpreter_mutation_pattern().is_match(command))
        && command_targets_protected_repository_asset(command)
    {
        return Err(PolicyError(
            "PROTECTED_REPOSITORY_ASSET: 禁止删除或递归清空 .git/.github".into(),
        ));
    }
    if interpreter_mutation_pattern().is_match(command)
        && command_parts_contain_external_path(&parts)
    {
        return Err(PolicyError(
            "WORKSPACE_PATH_PROTECTED: workspace scope 禁止通过子进程写入 Workspace 外部路径"
                .into(),
        ));
    }
    if dangerous_command_pattern().is_match(command) && !policy.skip_permission_gates() {
        return Err(PolicyError(
            "DANGEROUS_OPERATION_REQUIRES_DANGEROUS_MODE: dangerous commands require operator-enabled dangerous mode; model-supplied confirmation is not accepted"
                .into(),
        ));
    }
    if !policy.skip_permission_gates()
        && network_command_pattern().is_match(command)
        && !policy.network_allowed()
    {
        return Err(PolicyError(
            "Network-looking commands are blocked in safe permission mode".into(),
        ));
    }
    let executable = parts[0].trim_start_matches("./");
    let base_name = executable.rsplit(['/', '\\']).next().unwrap_or(executable);
    let stem = base_name
        .strip_suffix(".exe")
        .or_else(|| base_name.strip_suffix(".cmd"))
        .or_else(|| base_name.strip_suffix(".bat"))
        .or_else(|| base_name.strip_suffix(".ps1"))
        .unwrap_or(base_name);

    let workspace_entry_candidate = workspace_local_entry_exists(workspace, arguments, executable)
        || executable.contains(['/', '\\'])
        || policy
            .workspace_script_extensions
            .iter()
            .any(|extension| base_name.to_ascii_lowercase().ends_with(extension));
    let workspace_entry_allowed = policy.workspace_local_entries
        && workspace_entry_candidate
        && policy.allowed_commands.contains(stem);
    if !(policy.allowed_commands.contains(stem) || workspace_entry_allowed) {
        return Err(PolicyError(format!("Command is not allowlisted: {stem}")));
    }

    if let Some(wrapped_command) = wrapped_command_payload(&parts) {
        for nested_command in
            wrapped_command_segments(&parts, &wrapped_command).map_err(PolicyError)?
        {
            let mut nested_arguments = arguments.clone();
            if let Some(object) = nested_arguments.as_object_mut() {
                object.remove("executable");
                object.remove("args");
                object.remove("shell");
                object.insert("cmd".into(), Value::String(nested_command));
            }
            validate_command_for_workspace(&nested_arguments, policy, workspace)?;
        }
    }

    if let Some(environment) = arguments.get("env") {
        validate_model_environment(environment)?;
    }

    if let Some(timeout_ms) = arguments.get("timeout_ms").and_then(Value::as_u64) {
        if timeout_ms > 3_600_000 {
            return Err(PolicyError("Command timeout exceeds 60 minutes".into()));
        }
    }

    Ok(())
}

fn structured_direct_command_parts(arguments: &Value) -> Option<Vec<String>> {
    if arguments
        .get("shell")
        .and_then(Value::as_str)
        .unwrap_or("direct")
        != "direct"
    {
        return None;
    }
    let executable = arguments.get("executable")?.as_str()?.to_string();
    let mut parts = vec![executable];
    for argument in arguments.get("args").and_then(Value::as_array)? {
        parts.push(argument.as_str()?.to_string());
    }
    Some(parts)
}

fn program_uses_shell_syntax(program: &str) -> bool {
    matches!(
        command_stem(program).as_str(),
        "sh" | "bash" | "dash" | "zsh" | "ksh" | "fish" | "cmd" | "powershell" | "pwsh"
    )
}

fn validate_model_environment(environment: &Value) -> Result<(), PolicyError> {
    let variables = environment
        .as_object()
        .ok_or_else(|| PolicyError("Environment variables must be a string map".into()))?;
    for (name, value) in variables {
        let upper = name.to_ascii_uppercase();
        if environment_variable_is_sensitive(&upper) {
            return Err(PolicyError(format!(
                "Environment variable is protected and cannot be supplied by the model: {name}"
            )));
        }
        if !value.is_string() {
            return Err(PolicyError(format!(
                "Environment variable values must be strings: {name}"
            )));
        }
    }
    Ok(())
}

fn environment_variable_is_sensitive(name: &str) -> bool {
    const EXACT: &[&str] = &[
        "PATH",
        "PATHEXT",
        "COMSPEC",
        "SYSTEMROOT",
        "WINDIR",
        "HOME",
        "USERPROFILE",
        "APPDATA",
        "LOCALAPPDATA",
        "TEMP",
        "TMP",
        "SHELL",
        "ENV",
        "BASH_ENV",
        "LD_PRELOAD",
        "LD_LIBRARY_PATH",
        "NODE_OPTIONS",
        "PYTHONPATH",
        "PYTHONHOME",
        "RUSTC",
        "RUSTDOC",
        "RUSTFLAGS",
        "CARGO_HOME",
        "RUSTUP_HOME",
        "GIT_ASKPASS",
        "SSH_ASKPASS",
    ];
    EXACT.contains(&name)
        || name.starts_with("DYLD_")
        || name.starts_with("GIT_CONFIG_")
        || name.starts_with("GIT_SSH")
        || [
            "TOKEN",
            "SECRET",
            "PASSWORD",
            "PASSWD",
            "API_KEY",
            "APIKEY",
            "PRIVATE_KEY",
            "ACCESS_KEY",
            "CREDENTIAL",
            "COOKIE",
        ]
        .iter()
        .any(|marker| name.contains(marker))
}

pub(crate) fn wrapped_command_payload(parts: &[String]) -> Option<String> {
    let executable = parts
        .first()?
        .rsplit(['/', '\\'])
        .next()?
        .to_ascii_lowercase();
    let stem = executable
        .strip_suffix(".exe")
        .or_else(|| executable.strip_suffix(".cmd"))
        .or_else(|| executable.strip_suffix(".bat"))
        .unwrap_or(&executable);
    let switches: &[&str] = match stem {
        "powershell" | "pwsh" => &["-command", "-c"],
        "cmd" => &["/c", "/k"],
        "sh" | "bash" | "dash" | "zsh" | "ksh" | "fish" => &["-c"],
        _ => return None,
    };
    let index = parts.iter().position(|part| {
        switches
            .iter()
            .any(|switch| part.eq_ignore_ascii_case(switch))
            || (matches!(stem, "sh" | "bash" | "dash" | "zsh" | "ksh" | "fish")
                && part.starts_with('-')
                && part[1..].chars().any(|option| option == 'c'))
    })?;
    (index + 1 < parts.len()).then(|| parts[index + 1..].join(" "))
}

pub(crate) fn wrapped_command_segments(
    parts: &[String],
    payload: &str,
) -> Result<Vec<String>, String> {
    let executable = parts
        .first()
        .and_then(|part| part.rsplit(['/', '\\']).next())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let stem = executable.strip_suffix(".exe").unwrap_or(&executable);
    if !matches!(stem, "powershell" | "pwsh") {
        return Ok(vec![payload.trim().to_string()]);
    }
    let mut commands = Vec::new();
    for statement in split_powershell_statements(payload)? {
        let statement = statement.trim();
        if statement.is_empty() {
            continue;
        }
        if let Some(rhs) = powershell_assignment_rhs(statement) {
            let rhs = rhs.trim();
            if rhs.starts_with('(') && rhs.ends_with(')') {
                let nested = rhs[1..rhs.len() - 1].trim();
                if nested.is_empty() {
                    return Err("PowerShell assignment contains an empty subcommand".into());
                }
                commands.push(nested.to_string());
            } else if rhs.contains("$(") || rhs.contains("${") || rhs.contains('`') {
                return Err("PowerShell assignment contains unsupported dynamic expansion".into());
            }
            continue;
        }
        let mut nested = split_command_line(statement)?;
        let first = nested
            .first()
            .map(|value| value.to_ascii_lowercase())
            .unwrap_or_default();
        if is_safe_powershell_builtin(&first) {
            continue;
        }
        if first == "&" {
            if nested.get(1).is_none_or(|value| value.starts_with('$')) {
                return Err("Dynamic PowerShell command invocation is not allowed".into());
            }
            nested.remove(0);
            commands.push(join_command_tokens(&nested));
            continue;
        }
        if first.starts_with('$')
            || matches!(
                first.as_str(),
                "if" | "else" | "elseif" | "while" | "for" | "foreach" | "do" | "switch"
            )
        {
            return Err("Dynamic PowerShell control flow is not allowed".into());
        }
        commands.push(statement.to_string());
    }
    Ok(commands)
}

fn split_powershell_statements(payload: &str) -> Result<Vec<String>, String> {
    let mut statements = Vec::new();
    let mut current = String::new();
    let mut quote = None::<char>;
    let mut escaped = false;
    let mut parentheses = 0usize;
    for character in payload.chars() {
        if escaped {
            current.push(character);
            escaped = false;
            continue;
        }
        match quote {
            Some('\'') => {
                current.push(character);
                if character == '\'' {
                    quote = None;
                }
            }
            Some('"') => {
                current.push(character);
                if character == '`' {
                    escaped = true;
                } else if character == '"' {
                    quote = None;
                }
            }
            Some(_) => unreachable!(),
            None => match character {
                '\'' | '"' => {
                    quote = Some(character);
                    current.push(character);
                }
                '(' => {
                    parentheses = parentheses.saturating_add(1);
                    current.push(character);
                }
                ')' => {
                    if parentheses == 0 {
                        return Err(
                            "PowerShell wrapper has an unmatched closing parenthesis".into()
                        );
                    }
                    parentheses -= 1;
                    current.push(character);
                }
                ';' | '\r' | '\n' if parentheses == 0 => {
                    if !current.trim().is_empty() {
                        statements.push(current.trim().to_string());
                    }
                    current.clear();
                }
                '|' | '>' | '<' if parentheses == 0 => {
                    return Err(
                        "PowerShell wrapper pipelines and redirection are not allowed".into(),
                    );
                }
                '`' => {
                    return Err("PowerShell wrapper escape expansion is not allowed".into());
                }
                _ => current.push(character),
            },
        }
    }
    if quote.is_some() || parentheses != 0 {
        return Err("PowerShell wrapper contains an unterminated quote or subcommand".into());
    }
    if !current.trim().is_empty() {
        statements.push(current.trim().to_string());
    }
    Ok(statements)
}

fn powershell_assignment_rhs(statement: &str) -> Option<&str> {
    let (left, right) = statement.split_once('=')?;
    let left = left.trim();
    let name = left
        .strip_prefix("$env:")
        .or_else(|| left.strip_prefix('$'))?;
    (!name.is_empty()
        && name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_'))
    .then_some(right)
}

fn is_safe_powershell_builtin(command: &str) -> bool {
    matches!(
        command,
        "write-output"
            | "get-content"
            | "get-childitem"
            | "select-object"
            | "where-object"
            | "test-path"
            | "resolve-path"
            | "join-path"
            | "split-path"
            | "get-process"
            | "start-sleep"
    )
}

pub(crate) fn join_command_tokens(tokens: &[String]) -> String {
    tokens
        .iter()
        .map(|token| {
            if token.is_empty() || token.chars().any(char::is_whitespace) {
                format!("\"{}\"", token.replace('"', "\\\""))
            } else {
                token.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn workspace_local_entry_exists(
    workspace: Option<&Workspace>,
    arguments: &Value,
    executable: &str,
) -> bool {
    let Some(workspace) = workspace else {
        return false;
    };
    let workdir = arguments
        .get("workdir")
        .or_else(|| arguments.get("cwd"))
        .and_then(Value::as_str)
        .unwrap_or(".");
    let Ok(base) = workspace.resolve_existing(workdir) else {
        return false;
    };
    let candidate = if Path::new(executable).is_absolute() {
        Path::new(executable).to_path_buf()
    } else {
        base.path.join(executable)
    };
    candidate
        .canonicalize()
        .map(|path| path.is_file() && path.starts_with(workspace.root()))
        .unwrap_or(false)
}

pub fn validate_patch(arguments: &Value, policy: &PolicySettings) -> Result<(), PolicyError> {
    let patch = arguments
        .get("patch")
        .and_then(Value::as_str)
        .ok_or_else(|| PolicyError("apply_patch requires a patch".into()))?;
    if patch.trim().is_empty() {
        return Err(PolicyError("apply_patch requires a patch".into()));
    }

    if patch.len() > policy.max_patch_bytes {
        return Err(PolicyError("Patch is too large".into()));
    }

    Ok(())
}

fn has_forbidden_shell_syntax(command: &str) -> bool {
    if command.contains(['\r', '\n']) {
        return true;
    }

    let chars: Vec<char> = command.chars().collect();
    let mut quote = None;
    let mut escaped = false;
    let mut index = 0;
    while index < chars.len() {
        let ch = chars[index];
        if escaped {
            escaped = false;
            index += 1;
            continue;
        }

        match quote {
            Some('\'') => {
                if ch == '\'' {
                    quote = None;
                }
            }
            Some('"') => {
                if ch == '\\' {
                    escaped = true;
                } else if ch == '"' {
                    quote = None;
                }
            }
            Some(_) => {}
            None => {
                if ch == '\\' {
                    escaped = true;
                } else if ch == '\'' || ch == '"' {
                    quote = Some(ch);
                } else if matches!(ch, ';' | '&' | '|' | '>' | '<' | '`')
                    || (ch == '$'
                        && chars
                            .get(index + 1)
                            .is_some_and(|next| *next == '(' || *next == '{'))
                {
                    return true;
                }
            }
        }
        index += 1;
    }
    false
}

fn network_command_pattern() -> &'static regex::Regex {
    NETWORK_COMMAND_PATTERN.get_or_init(|| {
        regex::Regex::new(
            r"(?i)(https?://|urllib\.request|requests\.|http\.client|\bcurl\b|\bwget\b|\bssh\b|\bscp\b|\bftp\b)",
        )
        .expect("valid regex")
    })
}

fn dangerous_command_pattern() -> &'static regex::Regex {
    DANGEROUS_COMMAND_PATTERN.get_or_init(|| {
        regex::Regex::new(
            r"(?i)(git\s+reset\s+--hard|git\s+clean\s+-[^\r\n]*f|git\s+checkout\s+--\s+\.|(^|\s)rm\s+(-[^\r\n]*r[^\r\n]*f|--recursive)|remove-item\s+[^\r\n]*-recurse|(^|\s)(rmdir|del)\s+/s\b)",
        )
        .expect("valid regex")
    })
}

fn interpreter_mutation_pattern() -> &'static regex::Regex {
    INTERPRETER_MUTATION_PATTERN.get_or_init(|| {
        regex::Regex::new(
            r#"(?i)(shutil\.(rmtree|move)|os\.(remove|unlink|rmdir)|pathlib\.[^\s;]+\.(unlink|rename)|write_text|write_bytes|fs\.(writefile|writefilesync|unlink|rm)|set-content|out-file|new-item|files?\.(write|delete)|open\([^)]*['\"]w)"#,
        )
        .expect("valid regex")
    })
}

fn command_parts_contain_external_path(parts: &[String]) -> bool {
    let command = parts
        .first()
        .map(|part| command_stem(part))
        .unwrap_or_default();
    let inline_source_index = inline_source_argument_index(parts);
    parts.iter().enumerate().any(|(index, argument)| {
        if inline_source_index == Some(index) {
            inline_source_contains_external_path(argument)
        } else if index > 0 && is_windows_slash_switch_for_command(&command, argument) {
            false
        } else {
            text_contains_external_path(argument)
        }
    })
}

fn command_stem(executable: &str) -> String {
    let executable = executable
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(executable)
        .to_ascii_lowercase();
    executable
        .strip_suffix(".exe")
        .or_else(|| executable.strip_suffix(".cmd"))
        .or_else(|| executable.strip_suffix(".bat"))
        .or_else(|| executable.strip_suffix(".ps1"))
        .unwrap_or(&executable)
        .to_string()
}

fn inline_source_argument_index(parts: &[String]) -> Option<usize> {
    let stem = command_stem(parts.first()?);
    let switches: &[&str] = match stem.as_str() {
        "python" | "python3" | "py" => &["-c"],
        "node" | "deno" | "bun" => &["-e", "--eval"],
        "powershell" | "pwsh" => &["-command", "-c"],
        _ => return None,
    };
    parts
        .iter()
        .position(|part| {
            switches
                .iter()
                .any(|switch| part.eq_ignore_ascii_case(switch))
        })
        .and_then(|index| (index + 1 < parts.len()).then_some(index + 1))
}

fn inline_source_contains_external_path(source: &str) -> bool {
    let literals = quoted_literal_ranges(source);
    let mut outside = source.as_bytes().to_vec();
    for (start, content_start, content_end, end) in literals {
        outside[start..end].fill(b' ');
        let literal = &source[content_start..content_end];
        if !safe_separator_literal(source, start, end, literal)
            && !safe_regex_pattern_literal(source, start)
            && text_contains_external_path(literal)
            && !safe_diagnostic_path_literal(source, start, end)
        {
            return true;
        }
    }
    text_contains_external_path(&String::from_utf8_lossy(&outside))
}

fn safe_separator_literal(source: &str, start: usize, end: usize, value: &str) -> bool {
    if !matches!(value, "/" | "\\" | "\\\\") {
        return false;
    }
    let before = source[..start].trim_end().to_ascii_lowercase();
    let after = source[end..].trim_start().to_ascii_lowercase();
    if before.ends_with(".split(") || before.ends_with(".rsplit(") || after.starts_with(".join(") {
        return true;
    }

    let Some(replace_start) = source[..start].rfind(".replace(") else {
        return false;
    };
    let body_start = replace_start + ".replace(".len();
    if source[body_start..start].contains(')') {
        return false;
    }
    let Some(close_offset) = source[end..].find(')') else {
        return false;
    };
    source[body_start..end + close_offset].contains(',')
}

fn quoted_literal_ranges(source: &str) -> Vec<(usize, usize, usize, usize)> {
    let bytes = source.as_bytes();
    let mut ranges = Vec::new();
    let mut index = 0usize;
    while index < bytes.len() {
        let quote = bytes[index];
        if !matches!(quote, b'\'' | b'"' | b'`') {
            index += 1;
            continue;
        }
        let triple = quote != b'`'
            && index + 2 < bytes.len()
            && bytes[index + 1] == quote
            && bytes[index + 2] == quote;
        let delimiter_len = if triple { 3 } else { 1 };
        let content_start = index + delimiter_len;
        let mut cursor = content_start;
        let mut content_end = bytes.len();
        let mut end = bytes.len();
        while cursor < bytes.len() {
            let closes = if triple {
                cursor + 2 < bytes.len()
                    && bytes[cursor] == quote
                    && bytes[cursor + 1] == quote
                    && bytes[cursor + 2] == quote
            } else {
                bytes[cursor] == quote
            };
            if closes {
                content_end = cursor;
                end = cursor + delimiter_len;
                break;
            }
            let escaped = bytes[cursor] == b'\\' || (quote != b'`' && bytes[cursor] == b'`');
            if escaped && cursor + 1 < bytes.len() {
                cursor += 2;
                continue;
            }
            cursor += 1;
        }
        ranges.push((index, content_start, content_end, end));
        index = end.max(index + 1);
    }
    ranges
}

fn safe_diagnostic_path_literal(source: &str, start: usize, end: usize) -> bool {
    let before = source[..start].trim_end().to_ascii_lowercase();
    let after = source[end..].trim_start().to_ascii_lowercase();
    let safe_call = [
        "print(",
        "console.log(",
        "write-output",
        ".startswith(",
        ".endswith(",
        ".includes(",
        ".contains(",
    ]
    .iter()
    .any(|suffix| before.ends_with(suffix));
    let compared_after = after.starts_with("in ")
        || after.starts_with("==")
        || after.starts_with("!=")
        || after.starts_with("===")
        || after.starts_with("!==");
    let compared_before = ["==", "!=", "===", "!=="]
        .iter()
        .any(|operator| before.ends_with(operator));
    safe_call || compared_after || compared_before
}

fn text_contains_external_path(text: &str) -> bool {
    let normalized = text.replace('\\', "/");
    let posix_absolute = POSIX_ABSOLUTE_PATH_PATTERN
        .get_or_init(|| regex::Regex::new(r#"(?i)(^|["'\s])(/[^\s"']*)"#).expect("valid regex"))
        .is_match(&normalized);
    normalized == ".."
        || normalized.contains("../")
        || posix_absolute
        || WINDOWS_ABSOLUTE_PATH_PATTERN
            .get_or_init(|| regex::Regex::new(r"(?i)\b[A-Z]:/").expect("valid regex"))
            .is_match(&normalized)
}

#[cfg(windows)]
fn is_windows_slash_switch_for_command(command: &str, value: &str) -> bool {
    let value = value.to_ascii_lowercase();
    match command {
        "cmd" => matches!(
            value.as_str(),
            "/a" | "/c"
                | "/d"
                | "/e:off"
                | "/e:on"
                | "/f:off"
                | "/f:on"
                | "/k"
                | "/q"
                | "/s"
                | "/u"
                | "/v:off"
                | "/v:on"
        ),
        "find" => matches!(
            value.as_str(),
            "/v" | "/c" | "/n" | "/i" | "/off" | "/offline" | "/?"
        ),
        "findstr" => {
            matches!(
                value.as_str(),
                "/b" | "/e"
                    | "/l"
                    | "/r"
                    | "/s"
                    | "/i"
                    | "/x"
                    | "/v"
                    | "/n"
                    | "/m"
                    | "/o"
                    | "/p"
                    | "/off"
                    | "/offline"
                    | "/?"
            ) || ["/a:", "/f:", "/c:", "/g:", "/d:"]
                .iter()
                .any(|prefix| value.starts_with(prefix))
        }
        "dir" => {
            matches!(
                value.as_str(),
                "/a" | "/b"
                    | "/c"
                    | "/d"
                    | "/l"
                    | "/n"
                    | "/o"
                    | "/p"
                    | "/q"
                    | "/r"
                    | "/s"
                    | "/t"
                    | "/w"
                    | "/x"
                    | "/4"
                    | "/-c"
                    | "/?"
            ) || ["/a:", "/o:", "/t:"]
                .iter()
                .any(|prefix| value.starts_with(prefix))
        }
        "msbuild" => [
            "/bl",
            "/binarylogger",
            "/clp",
            "/consoleloggerparameters",
            "/dl",
            "/distributedlogger",
            "/ds",
            "/detailedsummary",
            "/fl",
            "/filelogger",
            "/flp",
            "/fileloggerparameters",
            "/graphbuild",
            "/isolateprojects",
            "/m",
            "/maxcpucount",
            "/noconlog",
            "/noconsolelogger",
            "/noautoresponse",
            "/nr",
            "/nodereuse",
            "/p",
            "/property",
            "/restore",
            "/t",
            "/target",
            "/tl",
            "/terminallogger",
            "/v",
            "/verbosity",
            "/version",
            "/warnaserror",
            "/warnasmessage",
        ]
        .iter()
        .any(|switch| value == *switch || value.starts_with(&format!("{switch}:"))),
        _ => false,
    }
}

#[cfg(not(windows))]
fn is_windows_slash_switch_for_command(_command: &str, _value: &str) -> bool {
    false
}

fn command_targets_protected_repository_asset(command: &str) -> bool {
    let normalized_command = command.to_ascii_lowercase().replace('\\', "/");
    let references_protected_asset =
        normalized_command.contains(".git") || normalized_command.contains(".github");
    if !references_protected_asset {
        return false;
    }

    let mutating_operation = [
        "rm ",
        "remove-item",
        "rmdir",
        "del ",
        "unlink",
        "rmtree",
        "write_text",
        "writefile",
        "rename",
        "move",
        "checkout",
        "clean ",
    ]
    .iter()
    .any(|needle| normalized_command.contains(needle));
    if mutating_operation {
        return true;
    }

    command.split_whitespace().any(|part| {
        let token = part
            .trim_matches(|ch: char| matches!(ch, '\'' | '"' | '`' | ',' | ';'))
            .replace('\\', "/");
        let token = token.strip_prefix("./").unwrap_or(&token);
        token == ".git"
            || token.starts_with(".git/")
            || token == ".github"
            || token.starts_with(".github/")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn default_policy_allows_text_search_accelerators_but_not_internal_codegraph() {
        let policy = PolicySettings::default();
        for command in ["rg", "ripgrep"] {
            assert!(
                policy.allowed_commands.contains(command),
                "{command} must be available to the managed software toolchain"
            );
        }
        assert!(!policy.allowed_commands.contains("codegraph"));
    }

    #[test]
    fn structured_direct_arguments_treat_multiline_source_as_literal_argv() {
        let source = "print('left && right')\nprint('done')".to_string();
        let command =
            join_command_tokens(&["python3".to_string(), "-c".to_string(), source.clone()]);
        assert!(has_forbidden_shell_syntax(&command));

        validate_command(
            &json!({
                "cmd": command,
                "executable": "python3",
                "args": ["-c", source],
                "shell": "direct",
                "filesystem_scope": "workspace"
            }),
            &PolicySettings::default(),
        )
        .expect("direct argv must not reinterpret literal source as shell syntax");
    }

    #[test]
    fn configured_shell_wrapper_still_validates_nested_payload() {
        let mut policy = PolicySettings::default();
        policy.allowed_commands.insert("sh".into());
        let payload = "echo safe && echo chained";
        let command =
            join_command_tokens(&["sh".to_string(), "-c".to_string(), payload.to_string()]);

        let error = validate_command(
            &json!({
                "cmd": command,
                "executable": "sh",
                "args": ["-c", payload],
                "shell": "direct",
                "filesystem_scope": "workspace"
            }),
            &policy,
        )
        .expect_err("shell wrapper payload must remain governed");
        assert!(error.0.contains("Shell chaining"), "{error}");
    }

    #[test]
    fn strict_workspace_rejects_external_paths_in_commands() {
        let dir = tempfile::tempdir().expect("workspace");
        let workspace = Workspace::new(dir.path().to_path_buf())
            .expect("workspace")
            .with_strict_read_boundary(true);
        let policy = PolicySettings::default();
        assert!(validate_command_for_workspace(
            &json!({"cmd": "cat ../other/secret.txt"}),
            &policy,
            Some(&workspace),
        )
        .is_err());
        assert!(validate_command_for_workspace(
            &json!({"cmd": "python -c \"print(open('/tmp/secret').read())\""}),
            &policy,
            Some(&workspace),
        )
        .is_err());
        assert!(validate_command_for_workspace(
            &json!({"cmd": "cat local.txt"}),
            &policy,
            Some(&workspace),
        )
        .is_ok());
        #[cfg(windows)]
        assert!(validate_command_for_workspace(
            &json!({"cmd": "cmd /c echo local"}),
            &policy,
            Some(&workspace),
        )
        .is_ok());
    }

    #[test]
    fn strict_workspace_distinguishes_inline_path_literals_from_file_access() {
        let dir = tempfile::tempdir().expect("workspace");
        let workspace = Workspace::new(dir.path().to_path_buf())
            .expect("workspace")
            .with_strict_read_boundary(true);
        let policy = PolicySettings::default();

        for command in [
            "python -c \"from pathlib import Path; cwd=Path.cwd(); print('/.anchor/worktrees/', cwd)\"",
            "python -c \"print('/tmp/diagnostic-only')\"",
            "node -e \"console.log('/tmp/diagnostic-only')\"",
            "powershell -NoProfile -Command \"Write-Output '/tmp/diagnostic-only'\"",
        ] {
            validate_command_for_workspace(&json!({"cmd": command}), &policy, Some(&workspace))
                .unwrap_or_else(|error| panic!("literal-only source should be allowed: {error}"));
        }

        for command in [
            "python -c \"print(open('/tmp/secret').read())\"",
            "python -c \"from pathlib import Path; print(Path('/tmp/secret').read_text())\"",
            "python -c \"reader=open; print(reader('/tmp/secret').read())\"",
            "python -c \"target='/tmp/secret'; print(target)\"",
            "powershell -NoProfile -Command \"Get-Content '/tmp/secret'\"",
            "cat /tmp/secret",
            "python /tmp/script.py",
            "/usr/bin/python -c \"print('external executable')\"",
        ] {
            let error =
                validate_command_for_workspace(&json!({"cmd": command}), &policy, Some(&workspace))
                    .expect_err("real external file access must remain blocked");
            assert!(error.0.contains("WORKSPACE_PATH_PROTECTED"), "{error}");
        }
    }

    #[test]
    fn strict_workspace_allows_separator_literals_in_structured_inline_source() {
        let dir = tempfile::tempdir().expect("workspace");
        std::fs::create_dir_all(dir.path().join("crates/anchor/src/tools")).expect("tree");
        std::fs::write(
            dir.path().join("crates/anchor/src/tools/registry.rs"),
            "alpha/beta\n",
        )
        .expect("fixture");
        let workspace = Workspace::new(dir.path().to_path_buf())
            .expect("workspace")
            .with_strict_read_boundary(true);
        let policy = PolicySettings::default();
        let source = "from pathlib import Path; text=Path('crates/anchor/src/tools/registry.rs').read_text(); print(text.replace('\\\\', '/').split('/'))";
        let command =
            join_command_tokens(&["python3".to_string(), "-c".to_string(), source.to_string()]);

        validate_command_for_workspace(
            &json!({
                "cmd": command,
                "executable": "python3",
                "args": ["-c", source],
                "shell": "direct",
                "filesystem_scope": "workspace"
            }),
            &policy,
            Some(&workspace),
        )
        .expect("separator syntax must not be mistaken for an absolute path");

        let external_source =
            "from pathlib import Path; print(Path('/tmp/secret').read_text().split('/'))";
        let external_command = join_command_tokens(&[
            "python3".to_string(),
            "-c".to_string(),
            external_source.to_string(),
        ]);
        let error = validate_command_for_workspace(
            &json!({
                "cmd": external_command,
                "executable": "python3",
                "args": ["-c", external_source],
                "shell": "direct",
                "filesystem_scope": "workspace"
            }),
            &policy,
            Some(&workspace),
        )
        .expect_err("actual external path access must remain blocked");
        assert!(error.0.contains("WORKSPACE_PATH_PROTECTED"), "{error}");
    }

    #[test]
    fn strict_workspace_allows_regex_escape_literals_but_not_external_file_access() {
        let dir = tempfile::tempdir().expect("workspace");
        std::fs::create_dir_all(dir.path().join("src/tools")).expect("tree");
        std::fs::write(dir.path().join("src/tools/registry.rs"), "( alpha )\n").expect("fixture");
        let workspace = Workspace::new(dir.path().to_path_buf())
            .expect("workspace")
            .with_strict_read_boundary(true);
        let policy = PolicySettings::default();
        let source = "import re; from pathlib import Path; text=Path('src/tools/registry.rs').read_text(); print(re.findall(r'\\(\\s*([a-z]+)', text))";
        let command =
            join_command_tokens(&["python3".to_string(), "-c".to_string(), source.to_string()]);
        validate_command_for_workspace(
            &json!({
                "cmd": command,
                "executable": "python3",
                "args": ["-c", source],
                "shell": "direct",
                "filesystem_scope": "workspace"
            }),
            &policy,
            Some(&workspace),
        )
        .expect("regex escape syntax must remain data, not become an absolute path");

        let external = "import re; print(open('/tmp/secret').read()); print(re.search(r'\\/tmp\\/secret', 'x'))";
        let command = join_command_tokens(&[
            "python3".to_string(),
            "-c".to_string(),
            external.to_string(),
        ]);
        let error = validate_command_for_workspace(
            &json!({
                "cmd": command,
                "executable": "python3",
                "args": ["-c", external],
                "shell": "direct",
                "filesystem_scope": "workspace"
            }),
            &policy,
            Some(&workspace),
        )
        .expect_err("regex data must not mask a separate real external file access");
        assert!(error.0.contains("WORKSPACE_PATH_PROTECTED"), "{error}");
    }

    #[cfg(windows)]
    #[test]
    fn strict_workspace_distinguishes_windows_switches_from_external_paths() {
        let dir = tempfile::tempdir().expect("workspace");
        let workspace = Workspace::new(dir.path().to_path_buf())
            .expect("workspace")
            .with_strict_read_boundary(true);
        let policy = PolicySettings::default();

        for command in [
            r#"find /n "needle" crates/anchor/src/tools/policy.rs"#,
            r#"cmd /d /c echo local"#,
            r#"dir /s crates\anchor"#,
            r#"msbuild /t:Build crates/anchor/app.sln"#,
        ] {
            validate_command_for_workspace(&json!({"cmd": command}), &policy, Some(&workspace))
                .unwrap_or_else(|error| {
                    panic!("Windows switch should not be treated as a path: {command}: {error}")
                });
        }

        for command in [
            r#"find /n "needle" C:\outside.txt"#,
            r#"cat /tmp/secret"#,
            r#"git diff -- /tmp/secret"#,
            r#"cat /c"#,
        ] {
            let error =
                validate_command_for_workspace(&json!({"cmd": command}), &policy, Some(&workspace))
                    .expect_err("real external path must remain blocked");
            assert!(
                error.0.contains("WORKSPACE_PATH_PROTECTED"),
                "{command}: {error}"
            );
        }
    }

    #[test]
    fn workspace_allowed_commands_override_defaults() {
        let actions = ActionsConfig {
            allowed_commands: "cargo,go".into(),
            ..ActionsConfig::default()
        };
        let policy = PolicySettings::from_actions_config(&actions);
        assert!(policy.allowed_commands.contains("cargo"));
        assert!(policy.allowed_commands.contains("pytest"));
    }

    #[test]
    fn trusted_mode_requires_workspace_script_name_to_be_allowlisted() {
        let mut policy = PolicySettings {
            workspace_local_entries: true,
            workspace_script_extensions: parse_workspace_script_extensions(".cmd,.launcher"),
            ..PolicySettings::default()
        };
        assert!(
            validate_command(&serde_json::json!({ "cmd": "anything.launcher" }), &policy).is_err()
        );
        policy.allowed_commands.insert("anything.launcher".into());
        policy.allowed_commands.insert("another-name".into());
        assert!(
            validate_command(&serde_json::json!({ "cmd": "anything.launcher" }), &policy).is_ok()
        );
        assert!(validate_command(
            &serde_json::json!({ "cmd": "scripts/another-name.cmd" }),
            &policy
        )
        .is_ok());
    }

    #[test]
    fn trusted_mode_accepts_an_extensionless_workspace_entry() {
        let dir = tempfile::tempdir().expect("workspace");
        std::fs::write(dir.path().join("project-entry"), "#!/bin/sh\necho ok\n").expect("entry");
        let workspace = Workspace::new(dir.path().to_path_buf()).expect("workspace");
        let mut policy = PolicySettings::default();
        policy.allowed_commands.insert("project-entry".into());
        assert!(validate_command_for_workspace(
            &serde_json::json!({ "cmd": "project-entry", "workdir": "." }),
            &policy,
            Some(&workspace),
        )
        .is_ok());
    }

    #[test]
    fn patch_size_uses_workspace_limit() {
        let actions = ActionsConfig {
            max_patch_bytes: 10,
            ..ActionsConfig::default()
        };
        let policy = PolicySettings::from_actions_config(&actions);
        let err = validate_patch(&json!({ "patch": "01234567890" }), &policy).unwrap_err();
        assert!(err.0.contains("too large"));
    }

    #[test]
    fn basic_diagnostic_commands_are_allowed() {
        let policy = PolicySettings::default();
        for command in BASIC_READ_ONLY_COMMANDS {
            validate_command(&json!({"cmd": command}), &policy)
                .unwrap_or_else(|err| panic!("{command} should be allowed: {err}"));
        }
    }

    #[test]
    fn configured_commands_keep_basic_diagnostics() {
        let actions = ActionsConfig {
            allowed_commands: "cargo,go".into(),
            ..ActionsConfig::default()
        };
        let policy = PolicySettings::from_actions_config(&actions);
        assert!(validate_command(&json!({"cmd": "pwd"}), &policy).is_ok());
        assert!(validate_command(&json!({"cmd": "pytest"}), &policy).is_ok());
    }

    #[test]
    fn quoted_python_code_is_not_treated_as_shell_chaining() {
        let policy = PolicySettings::default();
        assert!(validate_command(
            &json!({"cmd": "python -c \"import os; print(os.getcwd())\""}),
            &policy
        )
        .is_ok());
        assert!(validate_command(
            &json!({"cmd": "python -c \"print(1)\" && echo nope"}),
            &policy
        )
        .is_err());
        assert!(validate_command(&json!({"cmd": "echo hello > output.txt"}), &policy).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn windows_command_line_preserves_unquoted_backslashes_and_quoted_spaces() {
        let parts =
            split_command_line(r#"python -c "print('ok')" foo\bar "C:\Program Files\tool.exe""#)
                .expect("windows command line");
        assert_eq!(parts[3], r"foo\bar");
        assert_eq!(parts[4], r"C:\Program Files\tool.exe");
    }

    #[test]
    fn shell_wrappers_cannot_bypass_the_inner_command_allowlist() {
        let mut policy = PolicySettings::default();
        policy
            .allowed_commands
            .retain(|command| command == "powershell");
        let blocked = validate_command(
            &json!({"cmd": "powershell -NoProfile -Command \"corepack --version\""}),
            &policy,
        )
        .expect_err("inner command must be checked");
        assert!(blocked.0.contains("corepack"));

        assert!(validate_command(
            &json!({"cmd": "powershell -NoProfile -Command \"Write-Output safe\""}),
            &policy,
        )
        .is_ok());
        assert!(validate_command(
            &json!({"cmd": "powershell -NoProfile -Command \"Write-Output safe; corepack --version\""}),
            &policy,
        )
        .is_err());
    }

    #[test]
    fn powershell_wrapper_allows_checked_environment_setup_and_sequential_commands() {
        let policy = PolicySettings::default();
        assert!(validate_command(&json!({
            "cmd": "powershell -NoProfile -Command \"$env:RUSTC=(rustup which rustc); rustup run stable cargo --version\""
        }), &policy).is_ok());
        assert!(validate_command(&json!({
            "cmd": "powershell -NoProfile -Command \"$env:RUSTC=(winget --version); rustup run stable cargo --version\""
        }), &policy).is_err());
        assert!(validate_command(
            &json!({
                "cmd": "powershell -NoProfile -Command \"rustup --version | Write-Output\""
            }),
            &policy
        )
        .is_err());
        assert!(validate_command(
            &json!({
                "cmd": "powershell -NoProfile -Command \"$tool='cargo'; & $tool --version\""
            }),
            &policy
        )
        .is_err());
    }

    #[test]
    fn model_supplied_confirmation_cannot_unlock_dangerous_commands() {
        let trusted = PolicySettings::default();
        let error = validate_command(
            &json!({"cmd": "git reset --hard HEAD", "confirm": true}),
            &trusted,
        )
        .expect_err("trusted mode must reject destructive commands");
        assert!(error
            .0
            .contains("DANGEROUS_OPERATION_REQUIRES_DANGEROUS_MODE"));

        let dangerous = PolicySettings {
            permission_mode: "dangerous".into(),
            ..PolicySettings::default()
        };
        assert!(validate_command(&json!({"cmd": "git reset --hard HEAD"}), &dangerous).is_ok());
    }

    #[test]
    fn actions_exposure_rejects_internal_facade_operation_tools() {
        assert!(validate_actions_exposure("read_file").is_ok());
        for name in ["list_skills", "load_skill", "read_skill_resource"] {
            assert!(
                validate_actions_exposure(name).is_err(),
                "MCP-only Skill helper leaked into Actions exposure: {name}"
            );
        }
    }
}
