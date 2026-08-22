use std::io::{self, Write};
use std::time::{Duration, Instant};

use qrcode::render::unicode;
use qrcode::QrCode as TerminalQrCode;
use serde::Serialize;

use crate::data::DataStore;
use crate::error::{AppError, AppResult};
use crate::notifications::{
    poll_qr_status, request_qr_code, reset_ilink_cursor, worker, LoginCredentials, QrCode,
    QrStatus, DEFAULT_BASE_URL,
};
use crate::secret::SecretStore;
use crate::workspace::WorkspaceProfile;

use super::args::ILinkCommand;

const LOGIN_DEADLINE: Duration = Duration::from_secs(10 * 60);
const MAX_QR_REFRESHES: usize = 3;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct LoginResult {
    connected: bool,
    already_connected: bool,
    worker: worker::ILinkWorkerStatus,
    next_step: &'static str,
}

pub async fn execute(command: ILinkCommand, as_json: bool) -> AppResult<i32> {
    match command {
        ILinkCommand::Login { workspace } => login(&workspace, as_json).await.map(|_| 0),
        ILinkCommand::Start { workspace } => {
            let profile = profile(&workspace)?;
            print_status(worker::start(&profile)?, as_json)?;
            Ok(0)
        }
        ILinkCommand::Stop { workspace } => {
            let profile = profile(&workspace)?;
            print_status(worker::stop(&profile)?, as_json)?;
            Ok(0)
        }
        ILinkCommand::Status { workspace } => {
            let profile = profile(&workspace)?;
            print_status(worker::status(&profile)?, as_json)?;
            Ok(0)
        }
        ILinkCommand::Run { workspace } => {
            let profile = profile(&workspace)?;
            worker::run(&profile.id).await?;
            Ok(0)
        }
    }
}

async fn login(workspace: &str, as_json: bool) -> AppResult<()> {
    let profile = profile(workspace)?;
    let existing_token = SecretStore::get(&profile.id, "ilink_bot_token")?;
    worker::stop(&profile)?;
    let existing_scanner = SecretStore::get(&profile.id, "ilink_login_user_id")?;
    let local_tokens = if existing_scanner.is_some() {
        existing_token.clone().into_iter().collect::<Vec<_>>()
    } else {
        // Stage-one/manual credentials did not persist the QR scanner identity.
        // Do not advertise such a token as already bound: a fresh confirmation
        // is required so /bind can be authorized to the actual scanner.
        Vec::new()
    };
    let mut qr = request_qr_code(&local_tokens)
        .await
        .map_err(AppError::Message)?;
    display_qr(&qr, as_json);

    let deadline = Instant::now() + LOGIN_DEADLINE;
    let mut api_base = DEFAULT_BASE_URL.to_string();
    let mut verify_code: Option<String> = None;
    let mut qr_refreshes = 0_usize;
    let mut scanned_printed = false;
    loop {
        if Instant::now() >= deadline {
            return Err(AppError::Message(
                "iLink QR 登录超时，请重新执行 login".into(),
            ));
        }
        let status = poll_qr_status(&api_base, &qr.id, verify_code.as_deref())
            .await
            .map_err(AppError::Message)?;
        match status {
            QrStatus::Wait => {}
            QrStatus::Scanned => {
                verify_code = None;
                if !scanned_printed && !as_json {
                    println!("已扫码，等待微信确认…");
                    scanned_printed = true;
                }
            }
            QrStatus::NeedVerifyCode => {
                verify_code = Some(read_verify_code()?);
            }
            QrStatus::Redirect(base_url) => {
                api_base = base_url;
            }
            QrStatus::Expired | QrStatus::VerifyCodeBlocked => {
                qr_refreshes = qr_refreshes.saturating_add(1);
                if qr_refreshes > MAX_QR_REFRESHES {
                    return Err(AppError::Message(
                        "iLink 二维码多次过期或配对码被阻止，请稍后重试".into(),
                    ));
                }
                qr = request_qr_code(&local_tokens)
                    .await
                    .map_err(AppError::Message)?;
                api_base = DEFAULT_BASE_URL.to_string();
                verify_code = None;
                scanned_printed = false;
                display_qr(&qr, as_json);
            }
            QrStatus::AlreadyBound => {
                if existing_token.is_none() || existing_scanner.is_none() {
                    return Err(AppError::Message(
                        "iLink 返回已绑定，但本机缺少可安全复用的完整登录身份；请重新执行 QR 登录"
                            .into(),
                    ));
                }
                let worker_status = worker::start(&profile)?;
                print_login_result(
                    LoginResult {
                        connected: true,
                        already_connected: true,
                        worker: worker_status,
                        next_step: "在微信中向 ClawBot 发送 /bind",
                    },
                    as_json,
                )?;
                return Ok(());
            }
            QrStatus::Confirmed(credentials) => {
                persist_login(&profile, &credentials)?;
                reset_ilink_cursor(&profile.id).map_err(AppError::Message)?;
                worker::clear_runtime_status(&profile.id)?;
                let worker_status = worker::start(&profile)?;
                print_login_result(
                    LoginResult {
                        connected: true,
                        already_connected: false,
                        worker: worker_status,
                        next_step: "在微信中向 ClawBot 发送 /bind",
                    },
                    as_json,
                )?;
                return Ok(());
            }
        }
    }
}

fn persist_login(profile: &WorkspaceProfile, credentials: &LoginCredentials) -> AppResult<()> {
    SecretStore::set_many(
        &profile.id,
        &[
            ("ilink_bot_token", &credentials.bot_token),
            ("ilink_bot_id", &credentials.bot_id),
            ("ilink_login_user_id", &credentials.login_user_id),
            ("ilink_base_url", &credentials.base_url),
            ("ilink_target_user_id", ""),
            ("ilink_context_token", ""),
        ],
    )
}

fn display_qr(qr: &QrCode, as_json: bool) {
    if as_json {
        eprintln!("iLink QR URL: {}", qr.url);
    } else {
        println!("请使用微信扫描 ClawBot 登录二维码：");
        match TerminalQrCode::new(qr.url.as_bytes()) {
            Ok(code) => println!(
                "{}",
                code.render::<unicode::Dense1x2>().quiet_zone(true).build()
            ),
            Err(_) => println!("（终端二维码生成失败，请使用下面的备用链接）"),
        }
        println!("{}", qr.url);
        println!("等待扫码确认…");
    }
}

fn read_verify_code() -> AppResult<String> {
    print!("请输入手机微信显示的配对数字：");
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let code = input.trim();
    if code.is_empty() || code.len() > 16 || !code.chars().all(|ch| ch.is_ascii_digit()) {
        return Err(AppError::Message("配对数字格式无效".into()));
    }
    Ok(code.to_string())
}

fn print_login_result(result: LoginResult, as_json: bool) -> AppResult<()> {
    if as_json {
        println!("{}", serde_json::to_string(&result)?);
    } else {
        if result.already_connected {
            println!("iLink 已复用本机现有登录凭据。");
        } else {
            println!("iLink 登录成功，后台消息 worker 已启动。");
        }
        println!("下一步：{}", result.next_step);
    }
    Ok(())
}

fn print_status(status: worker::ILinkWorkerStatus, as_json: bool) -> AppResult<()> {
    if as_json {
        println!("{}", serde_json::to_string(&status)?);
    } else {
        println!("iLink worker: {}", status.state);
        println!("logged in: {}", status.logged_in);
        println!("bound: {}", status.bound);
        if let Some(pid) = status.pid {
            println!("pid: {pid}");
        }
        if status.reauthorization_required {
            println!("需要重新扫码登录。 ");
        }
        if !status.last_error.is_empty() {
            println!("last error: {}", status.last_error);
        }
    }
    Ok(())
}

fn profile(selector: &str) -> AppResult<WorkspaceProfile> {
    let store = DataStore::load()?;
    super::resolve_workspace(store.list(), selector).cloned()
}
