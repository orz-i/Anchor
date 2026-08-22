mod ilink;
mod model;
mod outbox;
mod state;
pub(crate) mod worker;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::data::DataStore;
use crate::harness::{Harness, TaskSession};
use crate::tunnel::append_profile_log;

use self::ilink::ILinkConfig;
use self::model::NotificationJob;

const MAX_MESSAGE_CHARS: usize = 1_800;
const RETRY_DELAYS: &[Duration] = &[
    Duration::from_secs(1),
    Duration::from_secs(5),
    Duration::from_secs(30),
    Duration::from_secs(120),
    Duration::from_secs(300),
];

pub(crate) fn task_completed(harness: &Harness, task: &TaskSession, verified: bool) {
    let Some(profile_id) = profile_id_for_workspace(harness.workspace_root()) else {
        return;
    };
    match load_ilink_config(&profile_id) {
        Ok(Some(_)) => {}
        Ok(None) => return,
        Err(error) => {
            log_failure(&profile_id, &task.id, &error);
            return;
        }
    }

    let job = NotificationJob::new(
        harness.workspace_id(),
        &profile_id,
        &task.id,
        completion_message(task, verified),
        unix_time_ms(),
    );
    match outbox::enqueue(harness.store_root(), &job) {
        Ok(_) => kick_dispatcher(
            harness.store_root().to_path_buf(),
            harness.workspace_id().to_string(),
        ),
        Err(error) => log_failure(
            &profile_id,
            &task.id,
            &format!("notification outbox enqueue failed: {error}"),
        ),
    }
}

pub(crate) use ilink::{
    poll_qr_status, request_qr_code, LoginCredentials, QrCode, QrStatus, DEFAULT_BASE_URL,
};

pub(crate) fn reset_ilink_cursor(profile_id: &str) -> Result<(), String> {
    state::reset_cursor(profile_id)
}

fn completion_message(task: &TaskSession, verified: bool) -> String {
    let branch = task.baseline.branch.as_deref().unwrap_or("unknown");
    let verification = if verified { "passed" } else { "unverified" };
    let objective = bounded(&task.objective, 1_200);
    bounded(
        &format!(
            "Anchor · Harness task completed\n\n任务：{}\n状态：completed\n验证：{}\n分支：{}\nTask：{}",
            objective, verification, branch, task.id
        ),
        MAX_MESSAGE_CHARS,
    )
}

fn kick_dispatcher(root: PathBuf, workspace_id: String) {
    let key = format!("{}:{workspace_id}", root.display());
    {
        let mut active = active_dispatchers()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !active.insert(key.clone()) {
            return;
        }
    }
    crate::async_runtime::spawn(async move {
        let exhausted = dispatch_pending(&root, &workspace_id).await;
        {
            let mut active = active_dispatchers()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            active.remove(&key);
        }
        if !exhausted && outbox::pending(&root, &workspace_id).is_ok_and(|jobs| !jobs.is_empty()) {
            kick_dispatcher(root, workspace_id);
        }
    });
}

async fn dispatch_pending(root: &Path, workspace_id: &str) -> bool {
    for attempt in 0..=RETRY_DELAYS.len() {
        let jobs = match outbox::pending(root, workspace_id) {
            Ok(jobs) => jobs,
            Err(_) => return true,
        };
        if jobs.is_empty() {
            return false;
        }
        let mut failed = false;
        for job in jobs {
            let config = match load_ilink_config(&job.profile_id) {
                Ok(Some(config)) => config,
                Ok(None) => return true,
                Err(error) => {
                    failed = true;
                    log_failure(&job.profile_id, &job.task_id, &error);
                    continue;
                }
            };
            match ilink::send_text(&config, &job.message).await {
                Ok(()) => {
                    if let Err(error) = outbox::mark_delivered(root, &job) {
                        failed = true;
                        log_failure(
                            &job.profile_id,
                            &job.task_id,
                            &format!("notification delivery marker failed: {error}"),
                        );
                    } else {
                        append_profile_log(
                            &job.profile_id,
                            "stdout.log",
                            &format!(
                                "[ilink] task completion notification delivered task={}",
                                job.task_id
                            ),
                        );
                    }
                }
                Err(error) => {
                    failed = true;
                    log_failure(&job.profile_id, &job.task_id, &error);
                }
            }
        }
        if !failed {
            return false;
        }
        let Some(delay) = RETRY_DELAYS.get(attempt) else {
            return true;
        };
        tokio::time::sleep(*delay).await;
    }
    true
}

fn profile_id_for_workspace(workspace_root: &Path) -> Option<String> {
    let canonical = workspace_root.canonicalize().ok()?;
    DataStore::read_file(|data| {
        Ok(data.profiles.iter().find_map(|profile| {
            let profile_root = Path::new(&profile.path).canonicalize().ok()?;
            let managed_worktrees = profile_root.join(".anchor").join("worktrees");
            (canonical == profile_root || canonical.starts_with(managed_worktrees))
                .then(|| profile.id.clone())
        }))
    })
    .ok()
    .flatten()
}

fn load_ilink_config(profile_id: &str) -> Result<Option<ILinkConfig>, String> {
    DataStore::read_file(|data| {
        let secrets = data.workspace_secrets.get(profile_id);
        let value = |key: &str| {
            secrets
                .and_then(|items| items.get(key))
                .map(|value| value.trim())
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        };
        let bot_token = value("ilink_bot_token");
        let target_user_id = value("ilink_target_user_id");
        let context_token = value("ilink_context_token");
        let base_url = value("ilink_base_url");
        if bot_token.is_none() && target_user_id.is_none() && context_token.is_none() {
            return Ok(None);
        }
        if bot_token.is_some() && (target_user_id.is_none() || context_token.is_none()) {
            // Logged in but not bound yet. Completion notifications remain a safe no-op
            // until the scanner explicitly sends /bind and the inbound worker persists
            // the target/context pair.
            return Ok(None);
        }
        let mut missing = Vec::new();
        if bot_token.is_none() {
            missing.push("ilink_bot_token");
        }
        if target_user_id.is_none() {
            missing.push("ilink_target_user_id");
        }
        if context_token.is_none() {
            missing.push("ilink_context_token");
        }
        if !missing.is_empty() {
            return Err(crate::error::AppError::Message(format!(
                "incomplete iLink notification configuration; missing {}",
                missing.join(", ")
            )));
        }
        ILinkConfig::new(
            bot_token.unwrap_or_default(),
            target_user_id.unwrap_or_default(),
            context_token.unwrap_or_default(),
            base_url,
        )
        .map(Some)
        .map_err(crate::error::AppError::Message)
    })
    .map_err(|error| error.to_string())
}

fn active_dispatchers() -> &'static Mutex<HashSet<String>> {
    static ACTIVE: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    ACTIVE.get_or_init(|| Mutex::new(HashSet::new()))
}

fn log_failure(profile_id: &str, task_id: &str, error: &str) {
    append_profile_log(
        profile_id,
        "stderr.log",
        &format!(
            "[ilink] task completion notification failed task={task_id}: {}",
            bounded(error, 400)
        ),
    );
}

fn bounded(value: &str, max_chars: usize) -> String {
    let count = value.chars().count();
    if count <= max_chars {
        return value.to_string();
    }
    if max_chars == 0 {
        return String::new();
    }
    let mut bounded = value
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect::<String>();
    bounded.push('…');
    bounded
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::model::{ExpectedWorkspaceState, ProjectBaseline, SCHEMA_VERSION};
    use crate::harness::{TaskContract, TaskPhase, TaskSession, TaskStatus, TaskWorkingSet};

    #[test]
    fn completion_message_is_bounded_and_has_stable_identity() {
        let task = TaskSession {
            schema_version: SCHEMA_VERSION,
            id: "task-1".into(),
            workspace_id: "workspace".into(),
            objective: "x".repeat(3_000),
            status: TaskStatus::Completed,
            phase: TaskPhase::Completed,
            contract: TaskContract::default(),
            slices: Vec::new(),
            current_slice_id: None,
            working_set: TaskWorkingSet::default(),
            recovery: None,
            termination: None,
            baseline: ProjectBaseline {
                schema_version: SCHEMA_VERSION,
                branch: Some("main".into()),
                head: None,
                worktree_fingerprint: "fingerprint".into(),
                object_id: "object".into(),
                captured_at: "0".into(),
                file_count: 0,
            },
            expected_state: ExpectedWorkspaceState {
                branch: Some("main".into()),
                head: None,
                worktree_fingerprint: "fingerprint".into(),
                accepted_at: "0".into(),
                accepted_by_operation_id: None,
            },
            completed_steps: Vec::new(),
            pending_steps: Vec::new(),
            latest_change_id: None,
            latest_verification_id: None,
            session_id: None,
            session_path: None,
            git_worktree: None,
            created_at: "0".into(),
            updated_at: "0".into(),
            last_activity_at: None,
        };
        let message = completion_message(&task, true);
        assert!(message.chars().count() <= MAX_MESSAGE_CHARS);
        assert!(message.contains("验证：passed"));
        assert!(message.contains("分支：main"));
        assert!(message.contains("Task：task-1"));
    }
}
