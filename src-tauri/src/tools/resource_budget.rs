use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use fs2::FileExt;
use serde_json::{json, Value};
use tokio::process::Command;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::tools::workspace::WorkspaceError;
use crate::tools::CancellationToken;

const DEFAULT_CPU_TARGET_PERCENT: usize = 75;
const MIN_CPU_TARGET_PERCENT: usize = 25;
const MAX_RUNNING_COMMANDS: usize = 4;
const RESOURCE_QUEUE_TIMEOUT: Duration = Duration::from_secs(15);
const RESOURCE_LOCK_POLL: Duration = Duration::from_millis(100);
const GIB: u64 = 1024 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionResourcePolicy {
    pub detected_cpus: usize,
    pub cgroup_cpu_limit: Option<usize>,
    pub effective_cpus: usize,
    pub detected_memory_bytes: Option<u64>,
    pub cgroup_memory_limit_bytes: Option<u64>,
    pub effective_memory_bytes: Option<u64>,
    pub cpu_target_percent: usize,
    pub reserved_cpus: usize,
    pub execution_cpu_budget: usize,
    pub max_running_commands: usize,
    pub queue_timeout_ms: u64,
}

fn package_manager_cpu_intensive(args: &[String]) -> bool {
    let Some(first) = args.first().map(String::as_str) else {
        return false;
    };
    if matches!(first, "build" | "test" | "install" | "ci" | "rebuild") {
        return true;
    }
    if !matches!(first, "run" | "exec") {
        return false;
    }
    args.iter().skip(1).any(|arg| {
        let arg = arg.to_ascii_lowercase();
        ["build", "test", "check", "lint", "clippy", "compile", "typecheck"]
            .iter()
            .any(|marker| arg.contains(marker))
    })
}

fn lock_is_contended(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::WouldBlock || matches!(error.raw_os_error(), Some(32 | 33))
}

impl ExecutionResourcePolicy {
    pub fn detect() -> Self {
        let detected_cpus = std::thread::available_parallelism()
            .map(usize::from)
            .unwrap_or(1)
            .max(1);
        let cgroup_cpu_limit = detect_cgroup_cpu_limit();
        let detected_memory_bytes = detect_physical_memory_bytes();
        let cgroup_memory_limit_bytes = detect_cgroup_memory_limit();
        let cpu_target_percent = configured_cpu_target_percent();
        let max_running_override = configured_max_running_commands();
        Self::from_capacity(
            detected_cpus,
            cgroup_cpu_limit,
            detected_memory_bytes,
            cgroup_memory_limit_bytes,
            cpu_target_percent,
            max_running_override,
        )
    }

    fn from_capacity(
        detected_cpus: usize,
        cgroup_cpu_limit: Option<usize>,
        detected_memory_bytes: Option<u64>,
        cgroup_memory_limit_bytes: Option<u64>,
        cpu_target_percent: usize,
        max_running_override: Option<usize>,
    ) -> Self {
        let detected_cpus = detected_cpus.max(1);
        let effective_cpus = cgroup_cpu_limit
            .map(|limit| detected_cpus.min(limit.max(1)))
            .unwrap_or(detected_cpus)
            .max(1);
        let effective_memory_bytes = min_optional(
            detected_memory_bytes,
            cgroup_memory_limit_bytes.filter(|value| *value > 0),
        );
        let cpu_target_percent = cpu_target_percent
            .clamp(MIN_CPU_TARGET_PERCENT, DEFAULT_CPU_TARGET_PERCENT);
        let percentage_budget = (effective_cpus * cpu_target_percent / 100).max(1);
        let execution_cpu_budget = if effective_cpus > 1 {
            percentage_budget.min(effective_cpus - 1)
        } else {
            1
        };
        let reserved_cpus = effective_cpus.saturating_sub(execution_cpu_budget);

        let cpu_command_cap = match execution_cpu_budget {
            0 | 1 => 1,
            2..=5 => 2,
            6..=11 => 3,
            _ => 4,
        };
        let memory_command_cap = match effective_memory_bytes {
            Some(bytes) if bytes < 4 * GIB => 1,
            Some(bytes) if bytes < 8 * GIB => 2,
            Some(bytes) if bytes < 16 * GIB => 3,
            _ => MAX_RUNNING_COMMANDS,
        };
        let automatic_max = cpu_command_cap
            .min(memory_command_cap)
            .clamp(1, MAX_RUNNING_COMMANDS);
        let max_running_commands = max_running_override
            .map(|value| automatic_max.min(value.max(1)))
            .unwrap_or(automatic_max);

        Self {
            detected_cpus,
            cgroup_cpu_limit,
            effective_cpus,
            detected_memory_bytes,
            cgroup_memory_limit_bytes,
            effective_memory_bytes,
            cpu_target_percent,
            reserved_cpus,
            execution_cpu_budget,
            max_running_commands,
            queue_timeout_ms: RESOURCE_QUEUE_TIMEOUT.as_millis() as u64,
        }
    }

    pub fn to_value(&self) -> Value {
        json!({
            "mode": "adaptive",
            "detected_cpus": self.detected_cpus,
            "cgroup_cpu_limit": self.cgroup_cpu_limit,
            "effective_cpus": self.effective_cpus,
            "detected_memory_bytes": self.detected_memory_bytes,
            "cgroup_memory_limit_bytes": self.cgroup_memory_limit_bytes,
            "effective_memory_bytes": self.effective_memory_bytes,
            "cpu_target_percent": self.cpu_target_percent,
            "reserved_cpus": self.reserved_cpus,
            "execution_cpu_budget": self.execution_cpu_budget,
            "max_running_commands": self.max_running_commands,
            "heavy_command_parallelism": self.execution_cpu_budget,
            "queue_timeout_ms": self.queue_timeout_ms,
            "cross_daemon_heavy_serialization": true,
            "child_priority": "below_normal",
            "retained_session_limit_is_execution_limit": false
        })
    }
}

pub struct ExecutionResourceManager {
    policy: ExecutionResourcePolicy,
    permits: Arc<Semaphore>,
    heavy_lock_path: PathBuf,
}

pub struct ExecutionLease {
    _permit: OwnedSemaphorePermit,
    _heavy_lock: Option<File>,
    heavy: bool,
    parallelism: usize,
    policy: ExecutionResourcePolicy,
}

impl std::fmt::Debug for ExecutionLease {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ExecutionLease")
            .field("heavy", &self.heavy)
            .field("parallelism", &self.parallelism)
            .finish_non_exhaustive()
    }
}

impl ExecutionLease {
    pub fn parallelism(&self) -> usize {
        self.parallelism
    }

    pub fn is_heavy(&self) -> bool {
        self.heavy
    }

    pub fn to_value(&self) -> Value {
        let mut value = self.policy.to_value();
        if let Some(object) = value.as_object_mut() {
            object.insert("heavy_command".into(), Value::Bool(self.heavy));
            object.insert("child_parallelism".into(), json!(self.parallelism));
        }
        value
    }
}

impl ExecutionResourceManager {
    pub fn new(harness_root: &Path) -> Self {
        Self::with_policy(harness_root, ExecutionResourcePolicy::detect())
    }

    fn with_policy(harness_root: &Path, policy: ExecutionResourcePolicy) -> Self {
        Self {
            permits: Arc::new(Semaphore::new(policy.max_running_commands)),
            heavy_lock_path: harness_root
                .join("resource-governor")
                .join("heavy-command.lock"),
            policy,
        }
    }

    pub fn policy_value(&self) -> Value {
        self.policy.to_value()
    }

    pub fn is_cpu_intensive(&self, program: &str, args: &[String]) -> bool {
        is_cpu_intensive_command(program, args)
    }

    pub async fn acquire(
        &self,
        heavy: bool,
        cancellation: &CancellationToken,
    ) -> Result<ExecutionLease, WorkspaceError> {
        let queue_timeout = Duration::from_millis(self.policy.queue_timeout_ms);
        let permit = tokio::select! {
            _ = cancellation.cancelled() => return Err(resource_cancelled_error()),
            result = tokio::time::timeout(queue_timeout, self.permits.clone().acquire_owned()) => {
                match result {
                    Ok(Ok(permit)) => permit,
                    Ok(Err(_)) => return Err(resource_queue_closed_error(&self.policy)),
                    Err(_) => return Err(resource_queue_timeout_error(&self.policy, heavy)),
                }
            }
        };

        let heavy_lock = if heavy {
            Some(
                acquire_heavy_lock(&self.heavy_lock_path, queue_timeout, cancellation, &self.policy)
                    .await?,
            )
        } else {
            None
        };

        let parallelism = if heavy {
            self.policy.execution_cpu_budget
        } else {
            (self.policy.execution_cpu_budget / self.policy.max_running_commands).max(1)
        };
        Ok(ExecutionLease {
            _permit: permit,
            _heavy_lock: heavy_lock,
            heavy,
            parallelism,
            policy: self.policy.clone(),
        })
    }

    pub fn clamp_parallel_args(&self, program: &str, args: &mut Vec<String>, limit: usize) {
        let executable = executable_name(program);
        match executable.as_str() {
            "cargo" | "make" | "gmake" | "ninja" => clamp_jobs_flags(args, limit, true),
            "cmake" => clamp_cmake_parallel(args, limit),
            "pytest" | "py.test" => clamp_pytest_workers(args, limit),
            _ => {}
        }
    }

    pub fn apply_child_environment(&self, command: &mut Command, limit: usize, program: &str) {
        for name in [
            "CARGO_BUILD_JOBS",
            "RUST_TEST_THREADS",
            "RAYON_NUM_THREADS",
            "GOMAXPROCS",
            "CMAKE_BUILD_PARALLEL_LEVEL",
            "UV_THREADPOOL_SIZE",
            "OMP_NUM_THREADS",
            "OPENBLAS_NUM_THREADS",
            "MKL_NUM_THREADS",
            "NUMEXPR_NUM_THREADS",
        ] {
            set_clamped_numeric_env(command, name, limit);
        }

        if matches!(executable_name(program).as_str(), "make" | "gmake") {
            clamp_makeflags(command, limit);
        }
    }
}

async fn acquire_heavy_lock(
    path: &Path,
    timeout: Duration,
    cancellation: &CancellationToken,
    policy: &ExecutionResourcePolicy,
) -> Result<File, WorkspaceError> {
    let parent = path.parent().ok_or_else(|| resource_lock_error("invalid lock path"))?;
    std::fs::create_dir_all(parent)
        .map_err(|error| resource_lock_error(&format!("create lock directory failed: {error}")))?;
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(path)
        .map_err(|error| resource_lock_error(&format!("open lock file failed: {error}")))?;
    let deadline = Instant::now() + timeout;
    loop {
        if cancellation.is_cancelled() {
            return Err(resource_cancelled_error());
        }
        match FileExt::try_lock_exclusive(&file) {
            Ok(()) => return Ok(file),
            Err(error) if lock_is_contended(&error) => {
                if Instant::now() >= deadline {
                    return Err(resource_queue_timeout_error(policy, true));
                }
                tokio::select! {
                    _ = cancellation.cancelled() => return Err(resource_cancelled_error()),
                    _ = tokio::time::sleep(RESOURCE_LOCK_POLL) => {}
                }
            }
            Err(error) => {
                return Err(resource_lock_error(&format!("lock acquisition failed: {error}")))
            }
        }
    }
}

fn resource_queue_timeout_error(policy: &ExecutionResourcePolicy, heavy: bool) -> WorkspaceError {
    WorkspaceError::ToolDetails {
        code: "EXECUTION_RESOURCE_BUSY",
        message: "Host execution budget is busy; command was not started.".into(),
        category: "runtime",
        retryable: true,
        details: json!({
            "stage": "resource_governor",
            "execution_started": false,
            "heavy_command": heavy,
            "max_running_commands": policy.max_running_commands,
            "execution_cpu_budget": policy.execution_cpu_budget,
            "queue_timeout_ms": policy.queue_timeout_ms,
            "suggestion": "Wait for an existing command to finish, consume its result, or retry later"
        }),
    }
}

fn resource_queue_closed_error(policy: &ExecutionResourcePolicy) -> WorkspaceError {
    WorkspaceError::ToolDetails {
        code: "EXECUTION_RESOURCE_UNAVAILABLE",
        message: "Host execution resource governor is unavailable.".into(),
        category: "runtime",
        retryable: true,
        details: json!({
            "stage": "resource_governor",
            "execution_started": false,
            "max_running_commands": policy.max_running_commands
        }),
    }
}

fn resource_lock_error(message: &str) -> WorkspaceError {
    WorkspaceError::ToolDetails {
        code: "EXECUTION_RESOURCE_LOCK_FAILED",
        message: format!("Unable to coordinate CPU-intensive commands: {message}"),
        category: "runtime",
        retryable: true,
        details: json!({
            "stage": "resource_governor",
            "execution_started": false,
            "suggestion": "Check the Anchor application data directory permissions and retry"
        }),
    }
}

fn resource_cancelled_error() -> WorkspaceError {
    WorkspaceError::ToolDetails {
        code: "REQUEST_CANCELLED",
        message: "Command execution was cancelled before resource allocation.".into(),
        category: "runtime",
        retryable: true,
        details: json!({
            "stage": "resource_governor",
            "termination_reason": "cancelled",
            "execution_started": false,
            "recoverable": true,
            "suggestion": "Retry the request if it is still needed"
        }),
    }
}

fn configured_cpu_target_percent() -> usize {
    std::env::var("ANCHOR_EXEC_CPU_PERCENT")
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .map(|value| value.clamp(MIN_CPU_TARGET_PERCENT, DEFAULT_CPU_TARGET_PERCENT))
        .unwrap_or(DEFAULT_CPU_TARGET_PERCENT)
}

fn configured_max_running_commands() -> Option<usize> {
    std::env::var("ANCHOR_EXEC_MAX_CONCURRENT")
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .filter(|value| *value > 0)
        .map(|value| value.min(MAX_RUNNING_COMMANDS))
}

fn min_optional(left: Option<u64>, right: Option<u64>) -> Option<u64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

fn executable_name(program: &str) -> String {
    Path::new(program)
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or(program)
        .to_ascii_lowercase()
}

fn is_cpu_intensive_command(program: &str, args: &[String]) -> bool {
    let executable = executable_name(program);
    match executable.as_str() {
        "cargo" => args.iter().any(|arg| {
            matches!(
                arg.as_str(),
                "build" | "check" | "test" | "clippy" | "bench" | "install"
            )
        }),
        "rustc" | "make" | "gmake" | "ninja" | "cmake" | "msbuild" | "gradle"
        | "gradlew" | "mvn" | "pytest" | "py.test" => true,
        "go" => args
            .first()
            .is_some_and(|arg| matches!(arg.as_str(), "build" | "test" | "install")),
        "npm" | "pnpm" | "yarn" | "bun" => package_manager_cpu_intensive(args),
        "docker" => {
            args.first().is_some_and(|arg| arg == "build")
                || (args.first().is_some_and(|arg| arg == "compose")
                    && args.get(1).is_some_and(|arg| arg == "build"))
        }
        "dotnet" => args.first().is_some_and(|arg| {
            matches!(arg.as_str(), "build" | "test" | "publish" | "restore")
        }),
        "sh" | "bash" | "zsh" | "cmd" | "powershell" | "pwsh" => {
            shell_payload_is_cpu_intensive(args)
        }
        _ => false,
    }
}

fn shell_payload_is_cpu_intensive(args: &[String]) -> bool {
    let payload = args.join(" ").to_ascii_lowercase();
    [
        "cargo build",
        "cargo check",
        "cargo test",
        "cargo clippy",
        "cargo bench",
        "pnpm build",
        "pnpm run build",
        "pnpm run test",
        "pnpm run check",
        "pnpm run lint",
        "pnpm test",
        "npm run build",
        "npm run test",
        "npm run check",
        "npm run lint",
        "npm test",
        "yarn build",
        "yarn test",
        "bun run build",
        "bun run test",
        "go build",
        "go test",
        "dotnet build",
        "dotnet test",
        "pytest",
        "ninja",
        "cmake --build",
        "make ",
        "gmake ",
    ]
    .iter()
    .any(|marker| payload.contains(marker))
}

fn clamp_jobs_flags(args: &mut Vec<String>, limit: usize, support_long: bool) {
    let mut index = 0;
    while index < args.len() {
        let current = args[index].clone();
        if current == "-j" || (support_long && current == "--jobs") {
            if let Some(next) = args.get_mut(index + 1) {
                if next.parse::<usize>().is_ok() {
                    clamp_numeric_string(next, limit);
                    index += 2;
                    continue;
                }
            }
            args[index] = format!("-j{limit}");
        } else if let Some(value) = current.strip_prefix("-j") {
            if !value.is_empty() && value.parse::<usize>().is_ok() {
                let clamped = numeric_clamp(value, limit);
                args[index] = format!("-j{clamped}");
            }
        } else if support_long {
            if let Some(value) = current.strip_prefix("--jobs=") {
                if value.parse::<usize>().is_ok() {
                    let clamped = numeric_clamp(value, limit);
                    args[index] = format!("--jobs={clamped}");
                }
            }
        }
        index += 1;
    }
}

fn clamp_cmake_parallel(args: &mut [String], limit: usize) {
    let mut index = 0;
    while index < args.len() {
        if args[index] == "--parallel" {
            if index + 1 < args.len() {
                if args[index + 1].parse::<usize>().is_ok() {
                    clamp_numeric_string(&mut args[index + 1], limit);
                } else {
                    args[index] = format!("--parallel={limit}");
                }
            } else {
                args[index] = format!("--parallel={limit}");
            }
        } else if let Some(value) = args[index].strip_prefix("--parallel=") {
            if value.parse::<usize>().is_ok() {
                let clamped = numeric_clamp(value, limit);
                args[index] = format!("--parallel={clamped}");
            }
        }
        index += 1;
    }
}

fn clamp_pytest_workers(args: &mut [String], limit: usize) {
    let mut index = 0;
    while index + 1 < args.len() {
        if args[index] == "-n" || args[index] == "--numprocesses" {
            if args[index + 1] == "auto" || args[index + 1] == "logical" {
                args[index + 1] = limit.to_string();
            } else if args[index + 1].parse::<usize>().is_ok() {
                clamp_numeric_string(&mut args[index + 1], limit);
            }
            index += 2;
        } else {
            index += 1;
        }
    }
}

fn clamp_numeric_string(value: &mut String, limit: usize) {
    *value = numeric_clamp(value, limit).to_string();
}

fn numeric_clamp(value: &str, limit: usize) -> usize {
    value
        .parse::<usize>()
        .ok()
        .filter(|value| *value > 0)
        .map(|value| value.min(limit))
        .unwrap_or(limit)
}

fn set_clamped_numeric_env(command: &mut Command, name: &str, limit: usize) {
    let configured = command
        .as_std()
        .get_envs()
        .find_map(|(candidate, value)| {
            candidate
                .eq_ignore_ascii_case(name)
                .then(|| value.and_then(|value| value.to_str()).map(str::to_string))
                .flatten()
        })
        .or_else(|| std::env::var(name).ok());
    let value = configured
        .as_deref()
        .map(|value| numeric_clamp(value, limit))
        .unwrap_or(limit);
    command.env(name, value.to_string());
}

fn clamp_makeflags(command: &mut Command, limit: usize) {
    let current = command
        .as_std()
        .get_envs()
        .find_map(|(candidate, value)| {
            candidate
                .eq_ignore_ascii_case("MAKEFLAGS")
                .then(|| value.and_then(|value| value.to_str()).map(str::to_string))
                .flatten()
        })
        .or_else(|| std::env::var("MAKEFLAGS").ok())
        .unwrap_or_default();
    let mut parts = current
        .split_whitespace()
        .map(str::to_string)
        .collect::<Vec<_>>();
    let before = parts.clone();
    clamp_jobs_flags(&mut parts, limit, true);
    if before == parts && !parts.iter().any(|part| part.starts_with("-j") || part.starts_with("--jobs")) {
        parts.push(format!("-j{limit}"));
    }
    command.env("MAKEFLAGS", parts.join(" "));
}

#[cfg(target_os = "linux")]
fn detect_cgroup_cpu_limit() -> Option<usize> {
    if let Ok(value) = std::fs::read_to_string("/sys/fs/cgroup/cpu.max") {
        if let Some(limit) = parse_cgroup_v2_cpu_max(&value) {
            return Some(limit);
        }
    }
    let quota = std::fs::read_to_string("/sys/fs/cgroup/cpu/cpu.cfs_quota_us")
        .ok()?
        .trim()
        .parse::<i64>()
        .ok()?;
    let period = std::fs::read_to_string("/sys/fs/cgroup/cpu/cpu.cfs_period_us")
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()?;
    quota_to_cpu_limit(quota, period)
}

#[cfg(not(target_os = "linux"))]
fn detect_cgroup_cpu_limit() -> Option<usize> {
    None
}

fn parse_cgroup_v2_cpu_max(value: &str) -> Option<usize> {
    let mut parts = value.split_whitespace();
    let quota = parts.next()?;
    if quota == "max" {
        return None;
    }
    let quota = quota.parse::<i64>().ok()?;
    let period = parts.next()?.parse::<u64>().ok()?;
    quota_to_cpu_limit(quota, period)
}

fn quota_to_cpu_limit(quota: i64, period: u64) -> Option<usize> {
    if quota <= 0 || period == 0 {
        return None;
    }
    Some(((quota as u64) / period).max(1) as usize)
}

#[cfg(target_os = "linux")]
fn detect_physical_memory_bytes() -> Option<u64> {
    let contents = std::fs::read_to_string("/proc/meminfo").ok()?;
    let line = contents.lines().find(|line| line.starts_with("MemTotal:"))?;
    let kib = line
        .split_whitespace()
        .nth(1)?
        .parse::<u64>()
        .ok()?;
    kib.checked_mul(1024)
}

#[cfg(not(target_os = "linux"))]
fn detect_physical_memory_bytes() -> Option<u64> {
    None
}

#[cfg(target_os = "linux")]
fn detect_cgroup_memory_limit() -> Option<u64> {
    if let Ok(value) = std::fs::read_to_string("/sys/fs/cgroup/memory.max") {
        let value = value.trim();
        if value != "max" {
            if let Ok(bytes) = value.parse::<u64>() {
                return Some(bytes);
            }
        }
    }
    std::fs::read_to_string("/sys/fs/cgroup/memory/memory.limit_in_bytes")
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
}

#[cfg(not(target_os = "linux"))]
fn detect_cgroup_memory_limit() -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(cpus: usize, memory_gib: u64) -> ExecutionResourcePolicy {
        ExecutionResourcePolicy::from_capacity(
            cpus,
            None,
            Some(memory_gib * GIB),
            None,
            DEFAULT_CPU_TARGET_PERCENT,
            None,
        )
    }

    #[test]
    fn two_cpu_host_reserves_one_cpu_and_runs_one_command() {
        let policy = policy(2, 8);
        assert_eq!(policy.execution_cpu_budget, 1);
        assert_eq!(policy.reserved_cpus, 1);
        assert_eq!(policy.max_running_commands, 1);
    }

    #[test]
    fn larger_hosts_scale_conservatively() {
        let eight = policy(8, 32);
        assert_eq!(eight.execution_cpu_budget, 6);
        assert_eq!(eight.reserved_cpus, 2);
        assert_eq!(eight.max_running_commands, 3);

        let sixteen = policy(16, 32);
        assert_eq!(sixteen.execution_cpu_budget, 12);
        assert_eq!(sixteen.max_running_commands, 4);
    }

    #[test]
    fn memory_and_cgroup_limits_reduce_execution_budget() {
        let constrained = ExecutionResourcePolicy::from_capacity(
            16,
            Some(2),
            Some(32 * GIB),
            Some(3 * GIB),
            DEFAULT_CPU_TARGET_PERCENT,
            None,
        );
        assert_eq!(constrained.effective_cpus, 2);
        assert_eq!(constrained.execution_cpu_budget, 1);
        assert_eq!(constrained.max_running_commands, 1);
    }

    #[test]
    fn operator_override_can_only_tighten_command_concurrency() {
        let constrained = ExecutionResourcePolicy::from_capacity(
            16,
            None,
            Some(32 * GIB),
            None,
            DEFAULT_CPU_TARGET_PERCENT,
            Some(2),
        );
        assert_eq!(constrained.max_running_commands, 2);
    }

    #[test]
    fn cgroup_cpu_max_parsing_is_conservative() {
        assert_eq!(parse_cgroup_v2_cpu_max("max 100000"), None);
        assert_eq!(parse_cgroup_v2_cpu_max("200000 100000"), Some(2));
        assert_eq!(parse_cgroup_v2_cpu_max("150000 100000"), Some(1));
        assert_eq!(parse_cgroup_v2_cpu_max("50000 100000"), Some(1));
    }

    #[test]
    fn cpu_intensive_command_detection_avoids_simple_diagnostics() {
        assert!(is_cpu_intensive_command(
            "cargo",
            &["test".into(), "--lib".into()]
        ));
        assert!(is_cpu_intensive_command(
            "pnpm",
            &["run".into(), "build".into()]
        ));
        assert!(!is_cpu_intensive_command(
            "cargo",
            &["--version".into()]
        ));
        assert!(!is_cpu_intensive_command("git", &["status".into()]));
        assert!(is_cpu_intensive_command(
            "sh",
            &["-c".into(), "cargo test --lib".into()]
        ));
    }

    #[test]
    fn command_line_parallelism_is_clamped_but_lower_values_are_preserved() {
        let temp = tempfile::tempdir().expect("temp");
        let manager = ExecutionResourceManager::with_policy(temp.path(), policy(8, 32));
        let mut cargo = vec!["test".into(), "-j16".into()];
        manager.clamp_parallel_args("cargo", &mut cargo, 6);
        assert_eq!(cargo, vec!["test", "-j6"]);

        let mut make = vec!["-j".into(), "2".into()];
        manager.clamp_parallel_args("make", &mut make, 6);
        assert_eq!(make, vec!["-j", "2"]);

        let mut ninja = vec!["-j".into()];
        manager.clamp_parallel_args("ninja", &mut ninja, 3);
        assert_eq!(ninja, vec!["-j3"]);
    }

    #[tokio::test]
    async fn local_execution_budget_is_acquired_before_another_command_can_run() {
        let temp = tempfile::tempdir().expect("temp");
        let manager = ExecutionResourceManager::with_policy(temp.path(), policy(2, 8));
        let token = CancellationToken::default();
        let first = manager.acquire(false, &token).await.expect("first permit");
        assert_eq!(first.parallelism(), 1);

        let cancelled = CancellationToken::default();
        cancelled.cancel();
        let error = manager
            .acquire(false, &cancelled)
            .await
            .expect_err("cancelled waiter");
        assert_eq!(error.to_error_value()["code"], "REQUEST_CANCELLED");
        drop(first);
    }

    #[tokio::test]
    async fn heavy_lock_serializes_managers_sharing_a_harness_root() {
        let temp = tempfile::tempdir().expect("temp");
        let policy = policy(8, 32);
        let first_manager = ExecutionResourceManager::with_policy(temp.path(), policy.clone());
        let second_manager = ExecutionResourceManager::with_policy(temp.path(), policy);
        let token = CancellationToken::default();
        let first = first_manager.acquire(true, &token).await.expect("heavy lease");

        let cancelled = CancellationToken::default();
        let cancellation = cancelled.clone();
        let waiter = second_manager.acquire(true, &cancelled);
        tokio::pin!(waiter);
        tokio::select! {
            result = &mut waiter => panic!("competing heavy command acquired early: {result:?}"),
            _ = tokio::time::sleep(Duration::from_millis(150)) => {}
        }
        cancellation.cancel();
        let error = waiter.await.expect_err("cancelled competing heavy lease");
        assert_eq!(error.to_error_value()["code"], "REQUEST_CANCELLED");
        drop(first);
    }
}
