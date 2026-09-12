use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use fs2::FileExt;
use serde_json::{json, Value};
use tokio::process::Command;

use crate::tools::workspace::WorkspaceError;
use crate::tools::CancellationToken;

const DEFAULT_CPU_TARGET_PERCENT: usize = 75;
const MIN_CPU_TARGET_PERCENT: usize = 25;
const MAX_RUNNING_COMMANDS: usize = 4;
const RESOURCE_QUEUE_TIMEOUT: Duration = Duration::from_secs(15);
const RESOURCE_LOCK_POLL: Duration = Duration::from_millis(100);
const MIB: u64 = 1024 * 1024;
const GIB: u64 = 1024 * 1024 * 1024;
const MIN_MEMORY_RESERVE_BYTES: u64 = 256 * MIB;
const MAX_MEMORY_RESERVE_BYTES: u64 = 2 * GIB;
const MEMORY_PER_ADDITIONAL_COMMAND_BYTES: u64 = 2 * GIB;
const RESOURCE_GOVERNOR_DIR: &str = "resource-governor";
const HOST_ADMISSION_LOCK_FILE: &str = "host-admission.lock";
const HOST_SLOT_FILE_PREFIX: &str = "command-slot";
const MAX_DURABLE_DEBT_SCAN_JOBS: usize = 4096;
const MAX_DURABLE_STATE_BYTES: u64 = 128 * 1024;

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

#[derive(Debug, Clone, PartialEq, Eq)]
struct MemoryPressureSnapshot {
    detected_available_memory_bytes: Option<u64>,
    cgroup_memory_current_bytes: Option<u64>,
    cgroup_memory_available_bytes: Option<u64>,
    effective_available_memory_bytes: Option<u64>,
    reserve_memory_bytes: Option<u64>,
    pressure_max_running_commands: usize,
    state: &'static str,
}

impl MemoryPressureSnapshot {
    fn to_value(&self) -> Value {
        json!({
            "detected_available_memory_bytes": self.detected_available_memory_bytes,
            "cgroup_memory_current_bytes": self.cgroup_memory_current_bytes,
            "cgroup_memory_available_bytes": self.cgroup_memory_available_bytes,
            "effective_available_memory_bytes": self.effective_available_memory_bytes,
            "reserve_memory_bytes": self.reserve_memory_bytes,
            "pressure_max_running_commands": self.pressure_max_running_commands,
            "state": self.state,
        })
    }
}

struct HostSlotLease {
    file: File,
    pressure: MemoryPressureSnapshot,
    active_before: usize,
}

async fn acquire_host_slot(
    harness_root: &Path,
    governor_root: &Path,
    deadline: Instant,
    cancellation: &CancellationToken,
    policy: &ExecutionResourcePolicy,
    heavy: bool,
) -> Result<HostSlotLease, WorkspaceError> {
    std::fs::create_dir_all(governor_root).map_err(|error| {
        resource_lock_error(&format!("create governor directory failed: {error}"))
    })?;
    let admission = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(governor_root.join(HOST_ADMISSION_LOCK_FILE))
        .map_err(|error| {
            resource_lock_error(&format!("open host admission lock failed: {error}"))
        })?;
    let mut last_pressure = None;
    let mut last_active = None;

    loop {
        if cancellation.is_cancelled() {
            return Err(resource_cancelled_error());
        }
        match FileExt::try_lock_exclusive(&admission) {
            Ok(()) => {
                let pressure = detect_memory_pressure(policy);
                let result =
                    try_reserve_host_slot(harness_root, governor_root, policy, &pressure, heavy);
                let _ = FileExt::unlock(&admission);
                match result? {
                    HostSlotAttempt::Acquired {
                        file,
                        active_before,
                    } => {
                        return Ok(HostSlotLease {
                            file,
                            pressure,
                            active_before,
                        });
                    }
                    HostSlotAttempt::Busy { active_before } => {
                        last_active = Some(active_before);
                        last_pressure = Some(pressure);
                    }
                }
            }
            Err(error) if lock_is_contended(&error) => {}
            Err(error) => {
                return Err(resource_lock_error(&format!(
                    "host admission lock acquisition failed: {error}"
                )))
            }
        }

        if Instant::now() >= deadline {
            return Err(resource_queue_timeout_error(
                policy,
                heavy,
                last_pressure.as_ref(),
                last_active,
            ));
        }
        tokio::select! {
            _ = cancellation.cancelled() => return Err(resource_cancelled_error()),
            _ = tokio::time::sleep(RESOURCE_LOCK_POLL) => {}
        }
    }
}

enum HostSlotAttempt {
    Acquired { file: File, active_before: usize },
    Busy { active_before: usize },
}

fn try_reserve_host_slot(
    harness_root: &Path,
    governor_root: &Path,
    policy: &ExecutionResourcePolicy,
    pressure: &MemoryPressureSnapshot,
    heavy: bool,
) -> Result<HostSlotAttempt, WorkspaceError> {
    let debt = orphaned_durable_debt(harness_root);
    let mut busy_slots = 0usize;
    let mut candidate = None;

    for index in 0..MAX_RUNNING_COMMANDS {
        let path = governor_root.join(format!("{HOST_SLOT_FILE_PREFIX}-{index}.lock"));
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)
            .map_err(|error| {
                resource_lock_error(&format!("open host command slot failed: {error}"))
            })?;
        match FileExt::try_lock_exclusive(&file) {
            Ok(()) if candidate.is_none() => candidate = Some(file),
            Ok(()) => {
                let _ = FileExt::unlock(&file);
            }
            Err(error) if lock_is_contended(&error) => busy_slots += 1,
            Err(error) => {
                return Err(resource_lock_error(&format!(
                    "host command slot inspection failed: {error}"
                )))
            }
        }
    }

    let active_before = busy_slots.saturating_add(debt.total);
    let admission_limit = policy
        .max_running_commands
        .min(pressure.pressure_max_running_commands);
    let blocked_by_heavy_orphan = heavy && debt.heavy > 0;
    if admission_limit > 0 && active_before < admission_limit && !blocked_by_heavy_orphan {
        if let Some(file) = candidate {
            return Ok(HostSlotAttempt::Acquired {
                file,
                active_before,
            });
        }
    }

    if let Some(file) = candidate {
        let _ = FileExt::unlock(&file);
    }
    Ok(HostSlotAttempt::Busy { active_before })
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct OrphanedDurableDebt {
    total: usize,
    heavy: usize,
}

fn constrain_unknown_memory(policy: &mut ExecutionResourcePolicy, required: bool) {
    if required && policy.detected_memory_bytes.is_none() {
        // Windows normally provides total physical memory through
        // GlobalMemoryStatusEx. If that system call ever fails, retain the
        // missing telemetry instead of fabricating a capacity value, but fail
        // closed on command concurrency so a detection failure cannot recreate
        // the pre-governor resource-exhaustion risk.
        policy.max_running_commands = 1;
    }
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
        [
            "build",
            "test",
            "check",
            "lint",
            "clippy",
            "compile",
            "typecheck",
        ]
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
        let mut policy = Self::from_capacity(
            detected_cpus,
            cgroup_cpu_limit,
            detected_memory_bytes,
            cgroup_memory_limit_bytes,
            cpu_target_percent,
            max_running_override,
        );
        constrain_unknown_memory(
            &mut policy,
            cfg!(any(
                target_os = "linux",
                target_os = "windows",
                target_os = "macos"
            )),
        );
        policy
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
        let cpu_target_percent =
            cpu_target_percent.clamp(MIN_CPU_TARGET_PERCENT, DEFAULT_CPU_TARGET_PERCENT);
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
            "host_wide_command_admission": true,
            "memory_pressure_admission": true,
            "cross_daemon_heavy_serialization": true,
            "child_priority": "below_normal",
            "retained_session_limit_is_execution_limit": false
        })
    }
}

pub struct ExecutionResourceManager {
    policy: ExecutionResourcePolicy,
    harness_root: PathBuf,
    governor_root: PathBuf,
    heavy_lock_path: PathBuf,
}

pub struct ExecutionLease {
    _host_slot: File,
    _heavy_lock: Option<File>,
    heavy: bool,
    parallelism: usize,
    policy: ExecutionResourcePolicy,
    pressure: MemoryPressureSnapshot,
    host_active_commands_before_admission: usize,
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
            object.insert("memory_pressure".into(), self.pressure.to_value());
            object.insert(
                "host_active_commands_before_admission".into(),
                json!(self.host_active_commands_before_admission),
            );
        }
        value
    }
}

impl ExecutionResourceManager {
    pub fn new(harness_root: &Path) -> Self {
        Self::with_policy(harness_root, ExecutionResourcePolicy::detect())
    }

    fn with_policy(harness_root: &Path, policy: ExecutionResourcePolicy) -> Self {
        let governor_root = harness_root.join(RESOURCE_GOVERNOR_DIR);
        Self {
            harness_root: harness_root.to_path_buf(),
            heavy_lock_path: governor_root.join("heavy-command.lock"),
            governor_root,
            policy,
        }
    }

    pub fn policy_value(&self) -> Value {
        let mut value = self.policy.to_value();
        if let Some(object) = value.as_object_mut() {
            object.insert(
                "memory_pressure".into(),
                detect_memory_pressure(&self.policy).to_value(),
            );
        }
        value
    }

    pub fn max_running_commands(&self) -> usize {
        self.policy.max_running_commands
    }

    pub fn is_cpu_intensive(&self, program: &str, args: &[String]) -> bool {
        is_cpu_intensive_command(program, args)
    }

    pub fn parallelism_for(&self, heavy: bool) -> usize {
        if heavy {
            self.policy.execution_cpu_budget
        } else {
            (self.policy.execution_cpu_budget / self.policy.max_running_commands).max(1)
        }
    }

    pub fn planned_execution_resources(&self, heavy: bool) -> Value {
        let mut value = self.policy_value();
        if let Some(object) = value.as_object_mut() {
            object.insert("heavy_command".into(), Value::Bool(heavy));
            object.insert(
                "child_parallelism".into(),
                json!(self.parallelism_for(heavy)),
            );
        }
        value
    }

    pub async fn acquire(
        &self,
        heavy: bool,
        cancellation: &CancellationToken,
    ) -> Result<ExecutionLease, WorkspaceError> {
        let queue_timeout = Duration::from_millis(self.policy.queue_timeout_ms);
        let deadline = Instant::now() + queue_timeout;

        let heavy_lock = if heavy {
            Some(
                acquire_heavy_lock(&self.heavy_lock_path, deadline, cancellation, &self.policy)
                    .await?,
            )
        } else {
            None
        };

        let host_slot = acquire_host_slot(
            &self.harness_root,
            &self.governor_root,
            deadline,
            cancellation,
            &self.policy,
            heavy,
        )
        .await?;

        let parallelism = self.parallelism_for(heavy);
        Ok(ExecutionLease {
            _host_slot: host_slot.file,
            _heavy_lock: heavy_lock,
            heavy,
            parallelism,
            policy: self.policy.clone(),
            pressure: host_slot.pressure,
            host_active_commands_before_admission: host_slot.active_before,
        })
    }

    pub fn clamp_parallel_args(&self, program: &str, args: &mut [String], limit: usize) {
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
    deadline: Instant,
    cancellation: &CancellationToken,
    policy: &ExecutionResourcePolicy,
) -> Result<File, WorkspaceError> {
    let parent = path
        .parent()
        .ok_or_else(|| resource_lock_error("invalid lock path"))?;
    std::fs::create_dir_all(parent)
        .map_err(|error| resource_lock_error(&format!("create lock directory failed: {error}")))?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .map_err(|error| resource_lock_error(&format!("open lock file failed: {error}")))?;
    loop {
        if cancellation.is_cancelled() {
            return Err(resource_cancelled_error());
        }
        match FileExt::try_lock_exclusive(&file) {
            Ok(()) => return Ok(file),
            Err(error) if lock_is_contended(&error) => {
                if Instant::now() >= deadline {
                    return Err(resource_queue_timeout_error(policy, true, None, None));
                }
                tokio::select! {
                    _ = cancellation.cancelled() => return Err(resource_cancelled_error()),
                    _ = tokio::time::sleep(RESOURCE_LOCK_POLL) => {}
                }
            }
            Err(error) => {
                return Err(resource_lock_error(&format!(
                    "lock acquisition failed: {error}"
                )))
            }
        }
    }
}

fn detect_memory_pressure(policy: &ExecutionResourcePolicy) -> MemoryPressureSnapshot {
    let detected_available_memory_bytes = detect_available_memory_bytes();
    let cgroup_memory_current_bytes = detect_cgroup_memory_current();
    memory_pressure_from_values(
        policy.effective_memory_bytes,
        detected_available_memory_bytes,
        policy.cgroup_memory_limit_bytes,
        cgroup_memory_current_bytes,
    )
}

fn memory_pressure_from_values(
    effective_memory_bytes: Option<u64>,
    detected_available_memory_bytes: Option<u64>,
    cgroup_memory_limit_bytes: Option<u64>,
    cgroup_memory_current_bytes: Option<u64>,
) -> MemoryPressureSnapshot {
    let cgroup_memory_available_bytes = match (
        cgroup_memory_limit_bytes.filter(|value| *value > 0),
        cgroup_memory_current_bytes,
    ) {
        (Some(limit), Some(current)) => Some(limit.saturating_sub(current)),
        _ => None,
    };
    let pressure_complete = detected_available_memory_bytes.is_some()
        && (cgroup_memory_limit_bytes.is_none() || cgroup_memory_current_bytes.is_some());
    let effective_available_memory_bytes = pressure_complete.then(|| {
        min_optional(
            detected_available_memory_bytes,
            cgroup_memory_available_bytes,
        )
        .unwrap_or(0)
    });
    let reserve_memory_bytes = effective_memory_bytes.map(memory_reserve_bytes);
    let pressure_max_running_commands = match (
        effective_memory_bytes,
        effective_available_memory_bytes,
        reserve_memory_bytes,
    ) {
        (Some(_), Some(available), Some(reserve)) if available <= reserve => 0,
        (Some(_), Some(available), Some(reserve)) => (1
            + (available.saturating_sub(reserve) / MEMORY_PER_ADDITIONAL_COMMAND_BYTES) as usize)
            .clamp(1, MAX_RUNNING_COMMANDS),
        _ => 1,
    };
    let state = if !pressure_complete || effective_memory_bytes.is_none() {
        "unknown"
    } else if pressure_max_running_commands == 0 {
        "critical"
    } else if pressure_max_running_commands < MAX_RUNNING_COMMANDS {
        "constrained"
    } else {
        "normal"
    };

    MemoryPressureSnapshot {
        detected_available_memory_bytes,
        cgroup_memory_current_bytes,
        cgroup_memory_available_bytes,
        effective_available_memory_bytes,
        reserve_memory_bytes,
        pressure_max_running_commands,
        state,
    }
}

fn memory_reserve_bytes(total: u64) -> u64 {
    let half = (total / 2).max(1);
    (total / 8)
        .clamp(MIN_MEMORY_RESERVE_BYTES, MAX_MEMORY_RESERVE_BYTES)
        .min(half)
}

fn orphaned_durable_debt(harness_root: &Path) -> OrphanedDurableDebt {
    let mut debt = OrphanedDurableDebt::default();
    let workspaces = harness_root.join("workspaces");
    let Ok(workspace_entries) = std::fs::read_dir(workspaces) else {
        return debt;
    };
    let mut scanned = 0usize;

    'workspaces: for workspace in workspace_entries.flatten() {
        let jobs_root = workspace.path().join("command-jobs");
        let Ok(job_entries) = std::fs::read_dir(jobs_root) else {
            continue;
        };
        for job in job_entries.flatten() {
            if scanned >= MAX_DURABLE_DEBT_SCAN_JOBS {
                break 'workspaces;
            }
            scanned += 1;
            let state_path = job.path().join("state.json");
            let Ok(metadata) = std::fs::metadata(&state_path) else {
                continue;
            };
            if metadata.len() > MAX_DURABLE_STATE_BYTES {
                continue;
            }
            let Ok(raw) = std::fs::read(&state_path) else {
                continue;
            };
            let Ok(state) = serde_json::from_slice::<Value>(&raw) else {
                continue;
            };
            let Some(status) = state.get("status").and_then(Value::as_str) else {
                continue;
            };
            if !matches!(status, "starting" | "running") {
                continue;
            }
            let supervisor_alive = state
                .get("supervisor_pid")
                .and_then(Value::as_u64)
                .and_then(|pid| u32::try_from(pid).ok())
                .is_some_and(|pid| crate::platform::platform().is_process_alive(pid));
            if supervisor_alive {
                continue;
            }
            let child_alive = state
                .get("child_pid")
                .and_then(Value::as_u64)
                .and_then(|pid| u32::try_from(pid).ok())
                .is_some_and(crate::platform::exec_process_tree_is_alive);
            if !child_alive {
                continue;
            }

            debt.total = debt.total.saturating_add(1);
            let heavy = read_durable_heavy_flag(&job.path().join("spec.json"));
            if heavy {
                debt.heavy = debt.heavy.saturating_add(1);
            }
        }
    }
    debt
}

fn read_durable_heavy_flag(spec_path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(spec_path) else {
        return false;
    };
    if metadata.len() > MAX_DURABLE_STATE_BYTES {
        return false;
    }
    std::fs::read(spec_path)
        .ok()
        .and_then(|raw| serde_json::from_slice::<Value>(&raw).ok())
        .and_then(|spec| {
            spec.get("execution_resources")
                .and_then(|resources| resources.get("heavy_command"))
                .and_then(Value::as_bool)
        })
        .unwrap_or(false)
}

fn resource_queue_timeout_error(
    policy: &ExecutionResourcePolicy,
    heavy: bool,
    pressure: Option<&MemoryPressureSnapshot>,
    active_commands: Option<usize>,
) -> WorkspaceError {
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
            "host_active_commands": active_commands,
            "memory_pressure": pressure.map(MemoryPressureSnapshot::to_value),
            "suggestion": "Wait for an existing command to finish, consume its result, or retry later"
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
        "rustc" | "make" | "gmake" | "ninja" | "cmake" | "msbuild" | "gradle" | "gradlew"
        | "mvn" | "pytest" | "py.test" => true,
        "go" => args
            .first()
            .is_some_and(|arg| matches!(arg.as_str(), "build" | "test" | "install")),
        "npm" | "pnpm" | "yarn" | "bun" => package_manager_cpu_intensive(args),
        "docker" => {
            args.first().is_some_and(|arg| arg == "build")
                || (args.first().is_some_and(|arg| arg == "compose")
                    && args.get(1).is_some_and(|arg| arg == "build"))
        }
        "dotnet" => args
            .first()
            .is_some_and(|arg| matches!(arg.as_str(), "build" | "test" | "publish" | "restore")),
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

fn clamp_jobs_flags(args: &mut [String], limit: usize, support_long: bool) {
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
    if before == parts
        && !parts
            .iter()
            .any(|part| part.starts_with("-j") || part.starts_with("--jobs"))
    {
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
    parse_linux_meminfo_bytes(&contents, "MemTotal:")
}

#[cfg(target_os = "windows")]
fn detect_physical_memory_bytes() -> Option<u64> {
    windows_memory_bytes().map(|(total, _)| total)
}

#[cfg(target_os = "macos")]
fn detect_physical_memory_bytes() -> Option<u64> {
    macos_sysctl_u64(b"hw.memsize\0").filter(|bytes| *bytes > 0)
}

#[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
fn detect_physical_memory_bytes() -> Option<u64> {
    None
}

#[cfg(target_os = "linux")]
fn detect_available_memory_bytes() -> Option<u64> {
    let contents = std::fs::read_to_string("/proc/meminfo").ok()?;
    parse_linux_meminfo_bytes(&contents, "MemAvailable:")
}

#[cfg(target_os = "windows")]
fn detect_available_memory_bytes() -> Option<u64> {
    windows_memory_bytes().map(|(_, available)| available)
}

#[cfg(target_os = "macos")]
fn detect_available_memory_bytes() -> Option<u64> {
    let page_size = macos_sysctl_u64(b"hw.pagesize\0")?;
    let free = macos_sysctl_u64(b"vm.page_free_count\0")?;
    let inactive = macos_sysctl_u64(b"vm.page_inactive_count\0")?;
    let speculative = macos_sysctl_u64(b"vm.page_speculative_count\0").unwrap_or(0);
    let pages = free.checked_add(inactive)?.checked_add(speculative)?;
    let available = pages.checked_mul(page_size)?;
    detect_physical_memory_bytes().map(|total| available.min(total))
}

#[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
fn detect_available_memory_bytes() -> Option<u64> {
    None
}

#[cfg(target_os = "linux")]
fn parse_linux_meminfo_bytes(contents: &str, key: &str) -> Option<u64> {
    let line = contents.lines().find(|line| line.starts_with(key))?;
    let kib = line.split_whitespace().nth(1)?.parse::<u64>().ok()?;
    kib.checked_mul(1024)
}

#[cfg(target_os = "windows")]
fn windows_memory_bytes() -> Option<(u64, u64)> {
    use std::mem;

    use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};

    let mut status = MEMORYSTATUSEX {
        dwLength: mem::size_of::<MEMORYSTATUSEX>() as u32,
        ..Default::default()
    };
    unsafe { GlobalMemoryStatusEx(&mut status).ok()? };
    (status.ullTotalPhys > 0 && status.ullAvailPhys > 0)
        .then_some((status.ullTotalPhys, status.ullAvailPhys))
}

#[cfg(target_os = "macos")]
extern "C" {
    fn sysctlbyname(
        name: *const libc::c_char,
        oldp: *mut libc::c_void,
        oldlenp: *mut libc::size_t,
        newp: *mut libc::c_void,
        newlen: libc::size_t,
    ) -> libc::c_int;
}

#[cfg(target_os = "macos")]
fn macos_sysctl_u64(name: &[u8]) -> Option<u64> {
    if name.last().copied() != Some(0) {
        return None;
    }
    let mut bytes = [0u8; 8];
    let mut len = bytes.len();
    let status = unsafe {
        sysctlbyname(
            name.as_ptr().cast(),
            bytes.as_mut_ptr().cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if status != 0 {
        return None;
    }
    match len {
        4 => Some(u32::from_ne_bytes(bytes[..4].try_into().ok()?) as u64),
        8 => Some(u64::from_ne_bytes(bytes)),
        _ => None,
    }
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

#[cfg(target_os = "linux")]
fn detect_cgroup_memory_current() -> Option<u64> {
    if let Ok(value) = std::fs::read_to_string("/sys/fs/cgroup/memory.current") {
        if let Ok(bytes) = value.trim().parse::<u64>() {
            return Some(bytes);
        }
    }
    std::fs::read_to_string("/sys/fs/cgroup/memory/memory.usage_in_bytes")
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
}

#[cfg(not(target_os = "linux"))]
fn detect_cgroup_memory_current() -> Option<u64> {
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
    fn physical_memory_thresholds_cap_command_concurrency() {
        let below_four = ExecutionResourcePolicy::from_capacity(
            32,
            None,
            Some(4 * GIB - 1),
            None,
            DEFAULT_CPU_TARGET_PERCENT,
            None,
        );
        assert_eq!(below_four.max_running_commands, 1);

        let four = policy(32, 4);
        assert_eq!(four.max_running_commands, 2);

        let eight = policy(32, 8);
        assert_eq!(eight.max_running_commands, 3);

        let sixteen = policy(32, 16);
        assert_eq!(sixteen.max_running_commands, 4);
    }

    #[test]
    fn required_memory_detection_failure_fails_closed_without_fabricating_capacity() {
        let mut unknown = ExecutionResourcePolicy::from_capacity(
            32,
            None,
            None,
            None,
            DEFAULT_CPU_TARGET_PERCENT,
            None,
        );
        assert_eq!(unknown.max_running_commands, 4);

        constrain_unknown_memory(&mut unknown, true);
        assert_eq!(unknown.max_running_commands, 1);
        assert_eq!(unknown.detected_memory_bytes, None);
        assert_eq!(unknown.effective_memory_bytes, None);
    }

    #[test]
    fn memory_pressure_uses_available_headroom_and_reserve() {
        let normal = memory_pressure_from_values(Some(16 * GIB), Some(12 * GIB), None, None);
        assert_eq!(normal.reserve_memory_bytes, Some(2 * GIB));
        assert_eq!(normal.pressure_max_running_commands, 4);
        assert_eq!(normal.state, "normal");

        let constrained = memory_pressure_from_values(Some(16 * GIB), Some(3 * GIB), None, None);
        assert_eq!(constrained.pressure_max_running_commands, 1);
        assert_eq!(constrained.state, "constrained");

        let critical = memory_pressure_from_values(Some(16 * GIB), Some(2 * GIB), None, None);
        assert_eq!(critical.pressure_max_running_commands, 0);
        assert_eq!(critical.state, "critical");
    }

    #[test]
    fn cgroup_remaining_memory_can_force_critical_pressure() {
        let pressure = memory_pressure_from_values(
            Some(4 * GIB),
            Some(3 * GIB),
            Some(4 * GIB),
            Some(4 * GIB - 256 * MIB),
        );
        assert_eq!(pressure.cgroup_memory_available_bytes, Some(256 * MIB));
        assert_eq!(pressure.effective_available_memory_bytes, Some(256 * MIB));
        assert_eq!(pressure.reserve_memory_bytes, Some(512 * MIB));
        assert_eq!(pressure.pressure_max_running_commands, 0);
        assert_eq!(pressure.state, "critical");

        let unknown =
            memory_pressure_from_values(Some(4 * GIB), Some(3 * GIB), Some(4 * GIB), None);
        assert_eq!(unknown.effective_available_memory_bytes, None);
        assert_eq!(unknown.pressure_max_running_commands, 1);
        assert_eq!(unknown.state, "unknown");
    }

    #[test]
    fn critical_memory_pressure_refuses_a_free_host_slot() {
        let temp = tempfile::tempdir().expect("temp");
        let governor_root = temp.path().join(RESOURCE_GOVERNOR_DIR);
        std::fs::create_dir_all(&governor_root).expect("governor root");
        let policy = policy(32, 16);
        let critical = memory_pressure_from_values(Some(16 * GIB), Some(2 * GIB), None, None);
        let attempt = try_reserve_host_slot(temp.path(), &governor_root, &policy, &critical, false)
            .expect("host slot attempt");
        assert!(matches!(
            attempt,
            HostSlotAttempt::Busy { active_before: 0 }
        ));

        let normal = memory_pressure_from_values(Some(16 * GIB), Some(12 * GIB), None, None);
        let attempt = try_reserve_host_slot(temp.path(), &governor_root, &policy, &normal, false)
            .expect("host slot attempt");
        assert!(matches!(attempt, HostSlotAttempt::Acquired { .. }));
    }

    #[cfg(windows)]
    #[test]
    fn windows_physical_memory_detection_reports_nonzero_capacity() {
        assert!(detect_physical_memory_bytes().is_some_and(|bytes| bytes > 0));
        let available = detect_available_memory_bytes().expect("available physical memory");
        let total = detect_physical_memory_bytes().expect("physical memory");
        assert!(available > 0 && available <= total);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_available_memory_detection_reports_sane_capacity() {
        let available = detect_available_memory_bytes().expect("MemAvailable");
        let total = detect_physical_memory_bytes().expect("MemTotal");
        assert!(available > 0 && available <= total);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_memory_detection_reports_sane_capacity() {
        let available = detect_available_memory_bytes().expect("macOS available memory");
        let total = detect_physical_memory_bytes().expect("macOS physical memory");
        assert!(available > 0 && available <= total);
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
        assert!(!is_cpu_intensive_command("cargo", &["--version".into()]));
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
    async fn host_execution_budget_is_shared_across_managers() {
        let temp = tempfile::tempdir().expect("temp");
        let manager = ExecutionResourceManager::with_policy(temp.path(), policy(2, 8));
        let second_manager = ExecutionResourceManager::with_policy(temp.path(), policy(2, 8));
        let token = CancellationToken::default();
        let first = manager.acquire(false, &token).await.expect("first permit");
        assert_eq!(first.parallelism(), 1);
        let telemetry = first.to_value();
        assert_eq!(telemetry["host_wide_command_admission"], true);
        assert!(telemetry["memory_pressure"].is_object());
        assert_eq!(telemetry["host_active_commands_before_admission"], json!(0));

        let cancelled = CancellationToken::default();
        let cancellation = cancelled.clone();
        let waiter = second_manager.acquire(false, &cancelled);
        tokio::pin!(waiter);
        tokio::select! {
            result = &mut waiter => panic!("second manager acquired host slot early: {result:?}"),
            _ = tokio::time::sleep(Duration::from_millis(150)) => {}
        }
        cancellation.cancel();
        let error = waiter.await.expect_err("cancelled waiter");
        assert_eq!(error.to_error_value()["code"], "REQUEST_CANCELLED");
        drop(first);
    }

    const HOST_SLOT_PROBE_ROOT_ENV: &str = "ANCHOR_HOST_SLOT_PROBE_ROOT";
    const HOST_SLOT_PROBE_READY_ENV: &str = "ANCHOR_HOST_SLOT_PROBE_READY";

    #[test]
    #[ignore = "invoked as a child process by host resource governor tests"]
    fn host_slot_probe_child() {
        let Some(root) = std::env::var_os(HOST_SLOT_PROBE_ROOT_ENV) else {
            return;
        };
        let ready = std::env::var_os(HOST_SLOT_PROBE_READY_ENV).expect("probe ready path");
        let manager = ExecutionResourceManager::with_policy(Path::new(&root), policy(2, 8));
        let token = CancellationToken::default();
        let lease = crate::async_runtime::block_on(manager.acquire(false, &token))
            .expect("child host slot");
        std::fs::write(ready, b"ready").expect("publish child host slot");
        std::thread::sleep(Duration::from_secs(2));
        drop(lease);
    }

    #[tokio::test]
    async fn host_execution_budget_is_shared_across_processes() {
        let temp = tempfile::tempdir().expect("temp");
        let ready = temp.path().join("host-slot-ready");
        let mut child = std::process::Command::new(std::env::current_exe().expect("test exe"))
            .arg("tools::resource_budget::tests::host_slot_probe_child")
            .arg("--ignored")
            .arg("--exact")
            .arg("--nocapture")
            .env(HOST_SLOT_PROBE_ROOT_ENV, temp.path())
            .env(HOST_SLOT_PROBE_READY_ENV, &ready)
            .spawn()
            .expect("spawn host slot probe child");
        let deadline = Instant::now() + Duration::from_secs(5);
        while !ready.is_file() && Instant::now() < deadline {
            if child.try_wait().expect("probe child status").is_some() {
                panic!("host slot probe child exited before publishing readiness");
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert!(ready.is_file(), "host slot probe did not become ready");

        let manager = ExecutionResourceManager::with_policy(temp.path(), policy(2, 8));
        let token = CancellationToken::default();
        let cancellation = token.clone();
        let waiter = manager.acquire(false, &token);
        tokio::pin!(waiter);
        tokio::select! {
            result = &mut waiter => panic!("cross-process host slot was not exclusive: {result:?}"),
            _ = tokio::time::sleep(Duration::from_millis(150)) => {}
        }
        cancellation.cancel();
        let error = waiter.await.expect_err("cancel cross-process waiter");
        assert_eq!(error.to_error_value()["code"], "REQUEST_CANCELLED");
        let _ = child.kill();
        let _ = child.wait();
    }

    #[cfg(unix)]
    #[test]
    fn orphaned_durable_child_is_counted_as_host_resource_debt() {
        use std::os::unix::process::CommandExt;

        let temp = tempfile::tempdir().expect("temp");
        let job = temp
            .path()
            .join("workspaces")
            .join("workspace")
            .join("command-jobs")
            .join("job");
        std::fs::create_dir_all(&job).expect("job dir");
        let dead_supervisor_pid = (2_000_000_000_u32..2_000_000_100_u32)
            .find(|pid| !crate::platform::platform().is_process_alive(*pid))
            .expect("unused pid");
        let mut child = std::process::Command::new("sh")
            .arg("-c")
            .arg("sleep 60 & wait")
            .process_group(0)
            .spawn()
            .expect("orphan debt process group");
        let child_pid = child.id();
        std::fs::write(
            job.join("state.json"),
            serde_json::to_vec(&json!({
                "status": "running",
                "supervisor_pid": dead_supervisor_pid,
                "child_pid": child_pid,
            }))
            .expect("state json"),
        )
        .expect("state");
        std::fs::write(
            job.join("spec.json"),
            serde_json::to_vec(&json!({
                "execution_resources": {"heavy_command": true}
            }))
            .expect("spec json"),
        )
        .expect("spec");

        assert_eq!(
            orphaned_durable_debt(temp.path()),
            OrphanedDurableDebt { total: 1, heavy: 1 }
        );

        crate::platform::signal_exec_process_tree(child_pid, "KILL").expect("kill debt tree");
        let _ = child.wait();
        let deadline = Instant::now() + Duration::from_secs(2);
        while crate::platform::exec_process_tree_is_alive(child_pid) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(25));
        }
        assert_eq!(
            orphaned_durable_debt(temp.path()),
            OrphanedDurableDebt::default()
        );
    }

    #[tokio::test]
    async fn heavy_lock_serializes_managers_sharing_a_harness_root() {
        let temp = tempfile::tempdir().expect("temp");
        let policy = policy(8, 32);
        let first_manager = ExecutionResourceManager::with_policy(temp.path(), policy.clone());
        let second_manager = ExecutionResourceManager::with_policy(temp.path(), policy);
        let token = CancellationToken::default();
        let first = first_manager
            .acquire(true, &token)
            .await
            .expect("heavy lease");

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
