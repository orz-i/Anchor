use std::mem;
use std::thread::sleep;
use std::time::Duration;

use std::path::Path;

use windows::core::PWSTR;
use windows::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0};
use windows::Win32::System::Threading::{
    GetExitCodeProcess, OpenProcess, QueryFullProcessImageNameW, TerminateProcess,
    WaitForSingleObject, PROCESS_NAME_FORMAT, PROCESS_QUERY_LIMITED_INFORMATION,
    PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
};

use crate::error::{AppError, AppResult};

const DURABLE_JOB_MAX_TRACKED_PROCESSES: usize = 4096;
const DURABLE_JOB_TERMINATION_POLL: Duration = Duration::from_millis(25);

/// Process-lifetime Job Object owned by the detached durable-command supervisor.
///
/// The supervisor itself is assigned to this Job before it creates the real
/// workspace child. Windows therefore associates every non-breakaway child and
/// descendant with the same Job from process creation onward. The raw Job handle
/// intentionally remains open until the supervisor process exits; closing the last
/// handle while the supervisor is still a member would also terminate the
/// supervisor because KILL_ON_JOB_CLOSE is enabled.
pub(crate) struct DurableSupervisorJob {
    handle: HANDLE,
    supervisor_pid: u32,
}

// Windows kernel handles are process-wide capabilities. This wrapper never
// closes or transfers ownership of the raw Job handle; moving the supervisor
// future between Tokio worker threads therefore preserves the same process-
// lifetime Job identity.
unsafe impl Send for DurableSupervisorJob {}

impl DurableSupervisorJob {
    pub(crate) fn terminate_descendants_and_wait(&self, timeout: Duration) -> AppResult<()> {
        let deadline = std::time::Instant::now() + timeout;
        let mut quiet_passes = 0_u8;
        loop {
            let members = job_process_ids(self.handle)?;
            let descendants = members
                .into_iter()
                .filter(|pid| *pid != self.supervisor_pid)
                .collect::<Vec<_>>();
            if descendants.is_empty() {
                quiet_passes = quiet_passes.saturating_add(1);
                if quiet_passes >= 3 {
                    return Ok(());
                }
            } else {
                quiet_passes = 0;
                for pid in descendants {
                    terminate_job_member_nowait(self.handle, pid)?;
                }
            }
            if std::time::Instant::now() >= deadline {
                let remaining = job_process_ids(self.handle)?
                    .into_iter()
                    .filter(|pid| *pid != self.supervisor_pid)
                    .collect::<Vec<_>>();
                return Err(AppError::Message(format!(
                    "durable supervisor Job 未能在终态前清空后代进程：remaining={remaining:?}"
                )));
            }
            sleep(DURABLE_JOB_TERMINATION_POLL);
        }
    }
}

fn create_current_process_kill_on_close_job(allow_breakaway: bool) -> AppResult<HANDLE> {
    use std::ffi::c_void;

    use windows::core::PCWSTR;
    use windows::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_BREAKAWAY_OK, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    use windows::Win32::System::Threading::GetCurrentProcess;

    unsafe {
        let job = CreateJobObjectW(None, PCWSTR::null())
            .map_err(|err| AppError::Message(format!("CreateJobObjectW failed: {err}")))?;
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if allow_breakaway {
            limits.BasicLimitInformation.LimitFlags |= JOB_OBJECT_LIMIT_BREAKAWAY_OK;
        }
        if let Err(err) = SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &limits as *const _ as *const c_void,
            mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        ) {
            let _ = CloseHandle(job);
            return Err(AppError::Message(format!(
                "SetInformationJobObject(KILL_ON_JOB_CLOSE) failed: {err}"
            )));
        }
        if let Err(err) = AssignProcessToJobObject(job, GetCurrentProcess()) {
            let _ = CloseHandle(job);
            return Err(AppError::Message(format!(
                "AssignProcessToJobObject(current process) failed: {err}"
            )));
        }
        Ok(job)
    }
}

pub fn install_kill_on_close_job() -> AppResult<()> {
    // This Job deliberately permits the detached durable supervisor to break
    // away. Ordinary daemon children remain lifecycle-bound to the daemon.
    let _job = create_current_process_kill_on_close_job(true)?;
    Ok(())
}

pub(crate) fn install_durable_supervisor_job() -> AppResult<DurableSupervisorJob> {
    // Unlike the daemon Job, the durable Job never permits breakaway. The
    // supervisor already escaped the daemon Job before reaching this function,
    // and every workspace descendant must remain contained here.
    Ok(DurableSupervisorJob {
        handle: create_current_process_kill_on_close_job(false)?,
        supervisor_pid: std::process::id(),
    })
}

fn job_process_ids(job: HANDLE) -> AppResult<Vec<u32>> {
    use std::ffi::c_void;

    use windows::Win32::System::JobObjects::{
        JobObjectBasicProcessIdList, QueryInformationJobObject, JOBOBJECT_BASIC_PROCESS_ID_LIST,
    };

    let mut last_error = None;
    for capacity in [64_usize, 256, 1024, DURABLE_JOB_MAX_TRACKED_PROCESSES] {
        let header_bytes = mem::size_of::<JOBOBJECT_BASIC_PROCESS_ID_LIST>()
            .saturating_sub(mem::size_of::<usize>());
        let buffer_bytes = header_bytes.saturating_add(capacity * mem::size_of::<usize>());
        let word_count = buffer_bytes.div_ceil(mem::size_of::<usize>());
        let mut buffer = vec![0_usize; word_count];
        match unsafe {
            QueryInformationJobObject(
                Some(job),
                JobObjectBasicProcessIdList,
                buffer.as_mut_ptr().cast::<c_void>(),
                buffer_bytes as u32,
                None,
            )
        } {
            Ok(()) => unsafe {
                let list = &*(buffer.as_ptr().cast::<JOBOBJECT_BASIC_PROCESS_ID_LIST>());
                let count = list.NumberOfProcessIdsInList as usize;
                if count > capacity || list.NumberOfAssignedProcesses as usize > capacity {
                    continue;
                }
                let first = std::ptr::addr_of!(list.ProcessIdList).cast::<usize>();
                return Ok(std::slice::from_raw_parts(first, count)
                    .iter()
                    .filter_map(|pid| u32::try_from(*pid).ok())
                    .collect());
            },
            Err(error) => last_error = Some(error),
        }
    }
    Err(AppError::Message(format!(
        "QueryInformationJobObject(process ids) failed or exceeded {DURABLE_JOB_MAX_TRACKED_PROCESSES} members: {}",
        last_error
            .map(|error| error.to_string())
            .unwrap_or_else(|| "process list exceeded the supported bound".into())
    )))
}

fn terminate_job_member_nowait(job: HANDLE, pid: u32) -> AppResult<()> {
    unsafe {
        let handle = match OpenProcess(PROCESS_TERMINATE | PROCESS_SYNCHRONIZE, false, pid) {
            Ok(handle) => handle,
            Err(error) => {
                return if job_process_ids(job)?.contains(&pid) {
                    Err(AppError::Message(format!(
                        "OpenProcess durable Job member failed: pid={pid}: {error}"
                    )))
                } else {
                    Ok(())
                };
            }
        };
        if handle == INVALID_HANDLE_VALUE {
            return Ok(());
        }
        if WaitForSingleObject(handle, 0) == WAIT_OBJECT_0 {
            let _ = CloseHandle(handle);
            return Ok(());
        }

        // Re-check membership only after opening the process handle. If the
        // original Job member exited and Windows reused its raw PID for an
        // unrelated process, the opened handle pins that process object while
        // the second Job query prevents us from terminating it.
        if !job_process_ids(job)?.contains(&pid) {
            let _ = CloseHandle(handle);
            return Ok(());
        }
        let result = TerminateProcess(handle, 1);
        if result.is_err() && WaitForSingleObject(handle, 0) == WAIT_OBJECT_0 {
            let _ = CloseHandle(handle);
            return Ok(());
        }
        let _ = CloseHandle(handle);
        result.map_err(|err| AppError::Message(format!("TerminateProcess failed: {err}")))?;
        Ok(())
    }
}

pub fn is_process_alive(pid: u32) -> bool {
    unsafe {
        // Prefer a synchronizable handle so an exited process is observed via
        // its signaled process object. The previous implementation fell back
        // to a query-only handle and still called WaitForSingleObject on it;
        // that produces WAIT_FAILED and was incorrectly treated as alive.
        if let Ok(handle) = OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            false,
            pid,
        ) {
            if handle == INVALID_HANDLE_VALUE {
                return false;
            }
            let still_running = WaitForSingleObject(handle, 0) != WAIT_OBJECT_0;
            let _ = CloseHandle(handle);
            return still_running;
        }

        let Ok(handle) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            // Protected processes can deny OpenProcess while still appearing
            // in the process snapshot. Conservatively keep those PIDs alive so
            // a listener owned by a system process is never treated as free.
            return process_ids()
                .map(|pids| pids.contains(&pid))
                .unwrap_or(false);
        };
        if handle == INVALID_HANDLE_VALUE {
            return false;
        }

        let mut exit_code = 0u32;
        let queried = GetExitCodeProcess(handle, &mut exit_code).is_ok();
        let _ = CloseHandle(handle);
        if queried {
            // Win32 STILL_ACTIVE is 259. This branch is only used when the OS
            // denied synchronization access but allowed limited query access.
            exit_code == 259
        } else {
            process_ids()
                .map(|pids| pids.contains(&pid))
                .unwrap_or(false)
        }
    }
}

pub fn process_image_path(pid: u32) -> AppResult<Option<String>> {
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid)
            .map_err(|err| AppError::Message(format!("OpenProcess failed: {err}")))?;
        if handle == INVALID_HANDLE_VALUE {
            return Ok(None);
        }

        let mut size = 32_768u32;
        let mut buffer = vec![0u16; size as usize];
        let ok = QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_FORMAT(0),
            PWSTR(buffer.as_mut_ptr()),
            &mut size,
        );
        let _ = CloseHandle(handle);
        if ok.is_err() {
            return Ok(None);
        }
        buffer.truncate(size as usize);
        Ok(Some(String::from_utf16_lossy(&buffer)))
    }
}

pub fn terminate_process_tree(root_pid: u32) -> AppResult<()> {
    // A single ToolHelp snapshot is not sufficient on Windows. Some children
    // (notably browser sandboxes such as Chrome renderers) can outlive the
    // snapshot or create additional descendants while shutdown is in flight.
    // Re-scan after each termination pass until no descendants remain.
    // Browser based children (Chrome renderer/utility processes in particular)
    // may create a short lived descendant while their parent is being killed.
    // Require several consecutive quiet snapshots before terminating the root;
    // a single empty snapshot still has a race with a concurrently spawned child.
    let mut quiet_passes = 0_u8;
    for _ in 0..40 {
        let children = collect_child_pids(root_pid)?;
        if children.is_empty() {
            quiet_passes = quiet_passes.saturating_add(1);
            if quiet_passes >= 3 {
                break;
            }
        } else {
            quiet_passes = 0;
            for pid in children.into_iter().rev() {
                let _ = terminate_pid(pid);
            }
        }

        // Chrome can spawn replacement utility/renderer processes while a
        // parent is handling termination. Give the kernel time to update the
        // process snapshot before the next pass; otherwise the next snapshot
        // can miss short-lived descendants.
        sleep(Duration::from_millis(50));
    }

    let remaining = collect_child_pids(root_pid)?;
    if !remaining.is_empty() {
        return Err(AppError::Message(format!(
            "无法在终止根进程前清空其后代进程：root_pid={root_pid}, remaining={remaining:?}"
        )));
    }

    // Kill the root only after descendants have stayed absent. Avoid sweeping
    // previously observed raw PIDs after the root exits: Windows can reuse a
    // PID, and killing a recycled PID would risk terminating an unrelated process.
    let result = terminate_pid(root_pid);
    match result {
        Ok(()) => Ok(()),
        Err(_err) if !is_process_alive(root_pid) => Ok(()),
        Err(err) => Err(err),
    }
}

/// 只终止镜像路径完全匹配的进程，避免误杀用户自行运行的其它 frpc。
pub fn terminate_processes_by_image_path(image_path: &Path) -> AppResult<usize> {
    let matched = process_ids_by_image_path(image_path)?;

    let mut terminated = 0;
    for pid in matched {
        if terminate_process_tree(pid).is_ok() {
            terminated += 1;
        }
    }
    Ok(terminated)
}

pub fn process_ids_by_image_path(image_path: &Path) -> AppResult<Vec<u32>> {
    let expected = normalize_image_path(image_path);
    let mut matched = Vec::new();
    for pid in process_ids()? {
        // Processes that deny image queries are skipped rather than treated as
        // matches; this primitive only acts on an exact executable identity.
        if let Ok(Some(actual)) = process_image_path(pid) {
            if normalize_image_path(Path::new(&actual)) == expected {
                matched.push(pid);
            }
        }
    }
    Ok(matched)
}

fn normalize_image_path(path: &Path) -> String {
    let normalized = std::fs::canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .replace('/', "\\")
        .trim_matches('"')
        .to_ascii_lowercase();

    // QueryFullProcessImageNameW may return either a DOS path or an NT path
    // with the `\\?\\` prefix. Treat both forms as the same executable so a
    // stale frpc from a previous application instance cannot escape cleanup.
    normalized
        .strip_prefix("\\\\?\\")
        .unwrap_or(&normalized)
        .to_string()
}

fn process_ids() -> AppResult<Vec<u32>> {
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };

    let mut pids = Vec::new();
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0)
            .map_err(|err| AppError::Message(format!("CreateToolhelp32Snapshot failed: {err}")))?;
        if snapshot == INVALID_HANDLE_VALUE {
            return Err(AppError::Message("invalid process snapshot".into()));
        }

        let mut entry = PROCESSENTRY32W {
            dwSize: mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        if Process32FirstW(snapshot, &mut entry).is_ok() {
            loop {
                pids.push(entry.th32ProcessID);
                if Process32NextW(snapshot, &mut entry).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snapshot);
    }
    Ok(pids)
}

fn collect_child_pids(root_pid: u32) -> AppResult<Vec<u32>> {
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };

    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0)
            .map_err(|err| AppError::Message(format!("CreateToolhelp32Snapshot failed: {err}")))?;
        if snapshot == INVALID_HANDLE_VALUE {
            return Err(AppError::Message("invalid process snapshot".into()));
        }

        let mut entry = PROCESSENTRY32W {
            dwSize: mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };

        let mut children_by_parent: std::collections::HashMap<u32, Vec<u32>> =
            std::collections::HashMap::new();

        if Process32FirstW(snapshot, &mut entry).is_ok() {
            loop {
                let parent = entry.th32ParentProcessID;
                let pid = entry.th32ProcessID;
                children_by_parent.entry(parent).or_default().push(pid);
                if Process32NextW(snapshot, &mut entry).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snapshot);

        let mut pending = vec![root_pid];
        let mut seen = std::collections::HashSet::from([root_pid]);
        let mut ordered = Vec::new();

        while let Some(parent) = pending.pop() {
            if let Some(children) = children_by_parent.get(&parent) {
                for &child in children {
                    if seen.insert(child) {
                        ordered.push(child);
                        pending.push(child);
                    }
                }
            }
        }

        Ok(ordered)
    }
}

fn terminate_pid(pid: u32) -> AppResult<()> {
    unsafe {
        let (handle, can_wait): (HANDLE, bool) =
            match OpenProcess(PROCESS_TERMINATE | PROCESS_SYNCHRONIZE, false, pid) {
                Ok(handle) => (handle, true),
                Err(_) => (
                    OpenProcess(PROCESS_TERMINATE, false, pid).map_err(|err| {
                        AppError::Message(format!("OpenProcess terminate failed: {err}"))
                    })?,
                    false,
                ),
            };
        if handle == INVALID_HANDLE_VALUE {
            return Ok(());
        }
        // Already exited — treat as success so stop does not fail on zombies.
        if can_wait && WaitForSingleObject(handle, 0) == WAIT_OBJECT_0 {
            let _ = CloseHandle(handle);
            return Ok(());
        }
        let result = TerminateProcess(handle, 1);
        // Give the kernel a moment to signal exit before returning.
        if can_wait {
            let _ = WaitForSingleObject(handle, 1_000);
        }
        let _ = CloseHandle(handle);
        result.map_err(|err| AppError::Message(format!("TerminateProcess failed: {err}")))?;
        Ok(())
    }
}
