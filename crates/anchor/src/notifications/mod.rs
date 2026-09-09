mod ilink;
mod model;
mod outbox;
mod state;
pub(crate) mod worker;

use std::collections::HashSet;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine};
use serde::Serialize;

use crate::error::{AppError, AppResult};
use crate::harness::{Harness, TaskSession};
use crate::secret::SecretStore;

use self::ilink::ILinkConfig;
use self::model::{NotificationChannel, NotificationJob};

const MAX_MESSAGE_CHARS: usize = 1_800;
pub(crate) const ILINK_SECRET_SCOPE: &str = "notification.ilink";
const RETRY_DELAYS: &[Duration] = &[
    Duration::from_secs(1),
    Duration::from_secs(5),
    Duration::from_secs(30),
    Duration::from_secs(120),
    Duration::from_secs(300),
];

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NotificationChannelStatus {
    pub id: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub worker: worker::ILinkWorkerStatus,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ILinkLoginChallenge {
    pub qr_id: String,
    pub qr_url: String,
    pub qr_image_data_url: String,
    pub base_url: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ILinkLoginPollResult {
    pub state: &'static str,
    pub base_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worker: Option<worker::ILinkWorkerStatus>,
}

pub(crate) fn channel_statuses() -> AppResult<Vec<NotificationChannelStatus>> {
    Ok(vec![NotificationChannelStatus {
        id: "ilink",
        label: "iLink",
        description: "微信 iLink 通知渠道",
        worker: worker::status()?,
    }])
}

pub(crate) fn start_ilink_channel() -> AppResult<NotificationChannelStatus> {
    Ok(NotificationChannelStatus {
        id: "ilink",
        label: "iLink",
        description: "微信 iLink 通知渠道",
        worker: worker::start()?,
    })
}

pub(crate) fn stop_ilink_channel() -> AppResult<NotificationChannelStatus> {
    Ok(NotificationChannelStatus {
        id: "ilink",
        label: "iLink",
        description: "微信 iLink 通知渠道",
        worker: worker::stop()?,
    })
}

pub(crate) async fn begin_ilink_login() -> AppResult<ILinkLoginChallenge> {
    let existing_token = SecretStore::get_app(ILINK_SECRET_SCOPE, "bot_token")?;
    let existing_scanner = SecretStore::get_app(ILINK_SECRET_SCOPE, "login_user_id")?;
    worker::stop()?;
    let local_tokens = if existing_scanner.is_some() {
        existing_token.into_iter().collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let qr = request_qr_code(&local_tokens)
        .await
        .map_err(AppError::Message)?;
    Ok(ILinkLoginChallenge {
        qr_image_data_url: qr_data_url(&qr.url)?,
        qr_id: qr.id,
        qr_url: qr.url,
        base_url: DEFAULT_BASE_URL.to_string(),
    })
}

pub(crate) async fn poll_ilink_login(
    qr_id: &str,
    base_url: &str,
    verify_code: Option<&str>,
) -> AppResult<ILinkLoginPollResult> {
    if qr_id.is_empty() || qr_id.len() > 4096 {
        return Err(AppError::Message("iLink QR id 无效".into()));
    }
    let verify_code = verify_code.map(str::trim).filter(|value| !value.is_empty());
    if verify_code
        .is_some_and(|code| code.len() > 16 || !code.chars().all(|ch| ch.is_ascii_digit()))
    {
        return Err(AppError::Message("配对数字格式无效".into()));
    }
    let status = poll_qr_status(base_url, qr_id, verify_code)
        .await
        .map_err(AppError::Message)?;
    let result = match status {
        QrStatus::Wait => ILinkLoginPollResult {
            state: "wait",
            base_url: base_url.to_string(),
            worker: None,
        },
        QrStatus::Scanned => ILinkLoginPollResult {
            state: "scanned",
            base_url: base_url.to_string(),
            worker: None,
        },
        QrStatus::NeedVerifyCode => ILinkLoginPollResult {
            state: "need_verify_code",
            base_url: base_url.to_string(),
            worker: None,
        },
        QrStatus::Redirect(next_base_url) => ILinkLoginPollResult {
            state: "redirect",
            base_url: next_base_url,
            worker: None,
        },
        QrStatus::Expired => ILinkLoginPollResult {
            state: "expired",
            base_url: base_url.to_string(),
            worker: None,
        },
        QrStatus::VerifyCodeBlocked => ILinkLoginPollResult {
            state: "verify_code_blocked",
            base_url: base_url.to_string(),
            worker: None,
        },
        QrStatus::AlreadyBound => {
            let token = SecretStore::get_app(ILINK_SECRET_SCOPE, "bot_token")?;
            let scanner = SecretStore::get_app(ILINK_SECRET_SCOPE, "login_user_id")?;
            if token.is_none() || scanner.is_none() {
                return Err(AppError::Message(
                    "iLink 返回已绑定，但本机缺少可安全复用的完整登录身份；请重新扫码登录".into(),
                ));
            }
            ILinkLoginPollResult {
                state: "already_bound",
                base_url: base_url.to_string(),
                worker: Some(worker::start()?),
            }
        }
        QrStatus::Confirmed(credentials) => {
            persist_ilink_login(&credentials)?;
            reset_ilink_cursor().map_err(AppError::Message)?;
            worker::clear_runtime_status()?;
            ILinkLoginPollResult {
                state: "confirmed",
                base_url: credentials.base_url,
                worker: Some(worker::start()?),
            }
        }
    };
    Ok(result)
}

pub(crate) fn persist_ilink_login(credentials: &LoginCredentials) -> AppResult<()> {
    SecretStore::set_app_many(&[
        (ILINK_SECRET_SCOPE, "bot_token", &credentials.bot_token),
        (ILINK_SECRET_SCOPE, "bot_id", &credentials.bot_id),
        (
            ILINK_SECRET_SCOPE,
            "login_user_id",
            &credentials.login_user_id,
        ),
        (ILINK_SECRET_SCOPE, "base_url", &credentials.base_url),
        (ILINK_SECRET_SCOPE, "target_user_id", ""),
        (ILINK_SECRET_SCOPE, "context_token", ""),
    ])
}

fn qr_data_url(content: &str) -> AppResult<String> {
    let code = qrcode::QrCode::new(content.as_bytes())
        .map_err(|error| AppError::Message(format!("生成 iLink 二维码失败：{error}")))?;
    const QUIET: usize = 4;
    const SCALE: usize = 6;
    let width = code.width();
    let dimension = (width + QUIET * 2) * SCALE;
    let mut image =
        image::GrayImage::from_pixel(dimension as u32, dimension as u32, image::Luma([255_u8]));
    for (index, color) in code.to_colors().into_iter().enumerate() {
        if color != qrcode::Color::Dark {
            continue;
        }
        let x = index % width;
        let y = index / width;
        let origin_x = (x + QUIET) * SCALE;
        let origin_y = (y + QUIET) * SCALE;
        for dx in 0..SCALE {
            for dy in 0..SCALE {
                image.put_pixel(
                    (origin_x + dx) as u32,
                    (origin_y + dy) as u32,
                    image::Luma([0_u8]),
                );
            }
        }
    }
    let mut output = Cursor::new(Vec::new());
    image::DynamicImage::ImageLuma8(image)
        .write_to(&mut output, image::ImageFormat::Png)
        .map_err(|error| AppError::Message(format!("编码 iLink 二维码失败：{error}")))?;
    Ok(format!(
        "data:image/png;base64,{}",
        BASE64_STANDARD.encode(output.into_inner())
    ))
}

pub(crate) fn task_completed(harness: &Harness, task: &TaskSession, verified: bool) {
    match load_ilink_config() {
        Ok(Some(_)) => {}
        Ok(None) => return,
        Err(error) => {
            log_failure(harness.workspace_id(), &task.id, &error);
            return;
        }
    }

    let job = NotificationJob::new(
        NotificationChannel::Ilink,
        harness.workspace_id(),
        &task.id,
        completion_message(task, verified),
        unix_time_ms(),
    );
    match outbox::enqueue(harness.store_root(), &job) {
        Ok(_) => kick_dispatcher(harness.store_root().to_path_buf()),
        Err(error) => log_failure(
            harness.workspace_id(),
            &task.id,
            &format!("notification outbox enqueue failed: {error}"),
        ),
    }
}

pub(crate) use ilink::{
    poll_qr_status, request_qr_code, LoginCredentials, QrCode, QrStatus, DEFAULT_BASE_URL,
};

pub(crate) fn reset_ilink_cursor() -> Result<(), String> {
    state::reset_cursor()
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

fn kick_dispatcher(root: PathBuf) {
    let key = root.display().to_string();
    {
        let mut active = active_dispatchers()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !active.insert(key.clone()) {
            return;
        }
    }
    crate::async_runtime::spawn(async move {
        let exhausted = dispatch_pending(&root).await;
        {
            let mut active = active_dispatchers()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            active.remove(&key);
        }
        if !exhausted && outbox::pending(&root, "ilink").is_ok_and(|jobs| !jobs.is_empty()) {
            kick_dispatcher(root);
        }
    });
}

async fn dispatch_pending(root: &Path) -> bool {
    for attempt in 0..=RETRY_DELAYS.len() {
        let jobs = match outbox::pending(root, "ilink") {
            Ok(jobs) => jobs,
            Err(_) => return true,
        };
        if jobs.is_empty() {
            return false;
        }
        let mut failed = false;
        for job in jobs {
            let config = match load_ilink_config() {
                Ok(Some(config)) => config,
                Ok(None) => return true,
                Err(error) => {
                    failed = true;
                    log_failure(&job.workspace_id, &job.task_id, &error);
                    continue;
                }
            };
            match ilink::send_text(&config, &job.message).await {
                Ok(()) => {
                    if let Err(error) = outbox::mark_delivered(root, &job) {
                        failed = true;
                        log_failure(
                            &job.workspace_id,
                            &job.task_id,
                            &format!("notification delivery marker failed: {error}"),
                        );
                    } else {
                        append_notification_log(
                            "stdout.log",
                            &format!(
                                "[ilink] task completion notification delivered workspace={} task={}",
                                job.workspace_id, job.task_id
                            ),
                        );
                    }
                }
                Err(error) => {
                    failed = true;
                    log_failure(&job.workspace_id, &job.task_id, &error);
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

fn load_ilink_config() -> Result<Option<ILinkConfig>, String> {
    let value = |key: &str| {
        SecretStore::get_app(ILINK_SECRET_SCOPE, key).map_err(|error| error.to_string())
    };
    let bot_token = value("bot_token")?;
    let target_user_id = value("target_user_id")?;
    let context_token = value("context_token")?;
    let base_url = value("base_url")?;
    if bot_token.is_none() && target_user_id.is_none() && context_token.is_none() {
        return Ok(None);
    }
    if bot_token.is_some() && (target_user_id.is_none() || context_token.is_none()) {
        return Ok(None);
    }
    let mut missing = Vec::new();
    if bot_token.is_none() {
        missing.push("bot_token");
    }
    if target_user_id.is_none() {
        missing.push("target_user_id");
    }
    if context_token.is_none() {
        missing.push("context_token");
    }
    if !missing.is_empty() {
        return Err(format!(
            "incomplete iLink notification channel configuration; missing {}",
            missing.join(", ")
        ));
    }
    ILinkConfig::new(
        bot_token.unwrap_or_default(),
        target_user_id.unwrap_or_default(),
        context_token.unwrap_or_default(),
        base_url,
    )
    .map(Some)
}

fn active_dispatchers() -> &'static Mutex<HashSet<String>> {
    static ACTIVE: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    ACTIVE.get_or_init(|| Mutex::new(HashSet::new()))
}

fn log_failure(workspace_id: &str, task_id: &str, error: &str) {
    append_notification_log(
        "stderr.log",
        &format!(
            "[ilink] task completion notification failed workspace={workspace_id} task={task_id}: {}",
            bounded(error, 400)
        ),
    );
}

fn append_notification_log(file_name: &str, line: &str) {
    use std::io::Write;

    let Ok(root) = crate::platform::platform().app_config_dir() else {
        return;
    };
    let dir = root.join("logs").join("notifications").join("ilink");
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let path = dir.join(file_name);
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let line = crate::logging::timestamped_line(line);
        let _ = writeln!(file, "{line}");
    }
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

    #[test]
    fn browser_login_qr_is_rendered_as_local_png_data_url() {
        let data_url = qr_data_url("https://example.invalid/ilink-login").expect("qr png");
        assert!(data_url.starts_with("data:image/png;base64,"));
        assert!(data_url.len() > 100);
    }
}
