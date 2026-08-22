use std::time::Duration;

use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine};
use reqwest::Url;
use serde_json::{json, Value};

pub const DEFAULT_BASE_URL: &str = "https://ilinkai.weixin.qq.com";
const CHANNEL_VERSION: &str = "2.4.6";
const SEND_TIMEOUT: Duration = Duration::from_secs(15);
const QR_TIMEOUT: Duration = Duration::from_secs(35);
const GET_UPDATES_DEFAULT_TIMEOUT_MS: u64 = 35_000;
const GET_UPDATES_MIN_TIMEOUT_MS: u64 = 5_000;
const GET_UPDATES_MAX_TIMEOUT_MS: u64 = 60_000;
pub const STALE_TOKEN_ERRCODE: i64 = -14;

#[derive(Debug, Clone)]
pub struct ILinkAccount {
    pub bot_token: String,
    pub base_url: String,
}

pub async fn request_qr_code(local_tokens: &[String]) -> Result<QrCode, String> {
    let endpoint = format!("{DEFAULT_BASE_URL}/ilink/bot/get_bot_qrcode?bot_type=3");
    let client = reqwest::Client::builder()
        .timeout(SEND_TIMEOUT)
        .build()
        .map_err(|error| format!("iLink QR client init failed: {error}"))?;
    let response = common_headers(client.post(endpoint), None)?
        .json(&json!({ "local_token_list": local_tokens }))
        .send()
        .await
        .map_err(|error| format!("iLink QR request failed: {error}"))?;
    let status = response.status();
    let body: Value = response
        .json()
        .await
        .map_err(|error| format!("iLink QR response decode failed ({status}): {error}"))?;
    if !status.is_success() {
        return Err(format!("iLink QR HTTP failure: {status}"));
    }
    let id = required_string(&body, "qrcode", "iLink QR response")?;
    let url = required_string(&body, "qrcode_img_content", "iLink QR response")?;
    if id.len() > 4096 || url.len() > 8192 {
        return Err("iLink QR response exceeded size limits".into());
    }
    Ok(QrCode { id, url })
}

pub async fn poll_qr_status(
    base_url: &str,
    qrcode: &str,
    verify_code: Option<&str>,
) -> Result<QrStatus, String> {
    let base_url = normalize_base_url(base_url)?;
    let mut endpoint = Url::parse(&format!("{base_url}/ilink/bot/get_qrcode_status"))
        .map_err(|_| "invalid iLink QR status URL".to_string())?;
    {
        let mut query = endpoint.query_pairs_mut();
        query.append_pair("qrcode", qrcode);
        if let Some(code) = verify_code.filter(|value| !value.trim().is_empty()) {
            query.append_pair("verify_code", code.trim());
        }
    }
    let client = reqwest::Client::builder()
        .timeout(QR_TIMEOUT)
        .build()
        .map_err(|error| format!("iLink QR status client init failed: {error}"))?;
    let response = match common_headers(client.get(endpoint), None)?.send().await {
        Ok(response) => response,
        Err(error) if error.is_timeout() => return Ok(QrStatus::Wait),
        Err(error) => return Err(format!("iLink QR status request failed: {error}")),
    };
    let http_status = response.status();
    let body: Value = response
        .json()
        .await
        .map_err(|error| format!("iLink QR status decode failed ({http_status}): {error}"))?;
    if !http_status.is_success() {
        return Err(format!("iLink QR status HTTP failure: {http_status}"));
    }
    parse_qr_status(&body)
}

pub async fn get_updates(
    account: &ILinkAccount,
    cursor: &str,
    timeout_hint_ms: u64,
) -> Result<GetUpdatesOutcome, PollError> {
    let timeout_ms = if timeout_hint_ms == 0 {
        GET_UPDATES_DEFAULT_TIMEOUT_MS
    } else {
        timeout_hint_ms.clamp(GET_UPDATES_MIN_TIMEOUT_MS, GET_UPDATES_MAX_TIMEOUT_MS)
    };
    let endpoint = format!("{}/ilink/bot/getupdates", account.base_url);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_millis(timeout_ms.saturating_add(5_000)))
        .build()
        .map_err(|error| PollError::Transient(format!("iLink poll client init failed: {error}")))?;
    let request = common_headers(client.post(endpoint), Some(&account.bot_token))
        .map_err(PollError::Transient)?;
    let response = match request
        .json(&json!({
            "get_updates_buf": cursor,
            "base_info": base_info()
        }))
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) if error.is_timeout() => return Ok(GetUpdatesOutcome::Timeout),
        Err(error) => {
            return Err(PollError::Transient(format!(
                "iLink getupdates request failed: {error}"
            )))
        }
    };
    let http_status = response.status();
    let body: Value = response.json().await.map_err(|error| {
        PollError::Transient(format!(
            "iLink getupdates response decode failed ({http_status}): {error}"
        ))
    })?;
    if !http_status.is_success() {
        return Err(PollError::Transient(format!(
            "iLink getupdates HTTP failure: {http_status}"
        )));
    }
    let ret = body.get("ret").and_then(Value::as_i64).unwrap_or(0);
    let errcode = body.get("errcode").and_then(Value::as_i64).unwrap_or(0);
    if ret == STALE_TOKEN_ERRCODE || errcode == STALE_TOKEN_ERRCODE {
        return Err(PollError::StaleToken);
    }
    if ret != 0 || errcode != 0 {
        return Err(PollError::Transient(format!(
            "iLink getupdates rejected request: ret={ret}, errcode={errcode}"
        )));
    }
    let next_cursor = body
        .get("get_updates_buf")
        .and_then(Value::as_str)
        .unwrap_or(cursor)
        .to_string();
    let next_timeout_ms = body
        .get("longpolling_timeout_ms")
        .and_then(Value::as_u64)
        .unwrap_or(GET_UPDATES_DEFAULT_TIMEOUT_MS)
        .clamp(GET_UPDATES_MIN_TIMEOUT_MS, GET_UPDATES_MAX_TIMEOUT_MS);
    Ok(GetUpdatesOutcome::Batch(GetUpdatesBatch {
        messages: parse_inbound_text_messages(body.get("msgs")),
        cursor: next_cursor,
        next_timeout_ms,
    }))
}

impl ILinkAccount {
    pub fn new(bot_token: String, base_url: Option<String>) -> Result<Self, String> {
        if bot_token.trim().is_empty() {
            return Err("iLink bot token is empty".into());
        }
        Ok(Self {
            bot_token,
            base_url: normalize_base_url(base_url.as_deref().unwrap_or(DEFAULT_BASE_URL))?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QrCode {
    pub id: String,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginCredentials {
    pub bot_token: String,
    pub bot_id: String,
    pub base_url: String,
    pub login_user_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QrStatus {
    Wait,
    Scanned,
    Confirmed(LoginCredentials),
    Expired,
    NeedVerifyCode,
    VerifyCodeBlocked,
    Redirect(String),
    AlreadyBound,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboundTextMessage {
    pub from_user_id: String,
    pub context_token: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GetUpdatesBatch {
    pub messages: Vec<InboundTextMessage>,
    pub cursor: String,
    pub next_timeout_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GetUpdatesOutcome {
    Batch(GetUpdatesBatch),
    Timeout,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PollError {
    StaleToken,
    Transient(String),
}

impl PollError {
    pub fn safe_message(&self) -> String {
        match self {
            Self::StaleToken => "iLink bot token is stale; QR login is required".into(),
            Self::Transient(message) => bounded(message, 300),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ILinkConfig {
    pub bot_token: String,
    pub target_user_id: String,
    pub context_token: String,
    pub base_url: String,
}

impl ILinkConfig {
    pub fn new(
        bot_token: String,
        target_user_id: String,
        context_token: String,
        base_url: Option<String>,
    ) -> Result<Self, String> {
        let base_url = normalize_base_url(base_url.as_deref().unwrap_or(DEFAULT_BASE_URL))?;
        Ok(Self {
            bot_token,
            target_user_id,
            context_token,
            base_url,
        })
    }
}

pub async fn send_text(config: &ILinkConfig, text: &str) -> Result<(), String> {
    let endpoint = format!("{}/ilink/bot/sendmessage", config.base_url);
    let client = reqwest::Client::builder()
        .timeout(SEND_TIMEOUT)
        .build()
        .map_err(|error| format!("iLink client init failed: {error}"))?;
    let response = client
        .post(endpoint)
        .header("AuthorizationType", "ilink_bot_token")
        .bearer_auth(&config.bot_token)
        .header("X-WECHAT-UIN", random_wechat_uin()?)
        .header("iLink-App-Id", "bot")
        .header(
            "iLink-App-ClientVersion",
            encoded_client_version(CHANNEL_VERSION).to_string(),
        )
        .json(&text_message_body(
            &config.target_user_id,
            &config.context_token,
            text,
        ))
        .send()
        .await
        .map_err(|error| format!("iLink send request failed: {error}"))?;
    let status = response.status();
    let body: Value = response
        .json()
        .await
        .map_err(|error| format!("iLink response decode failed ({status}): {error}"))?;
    if !status.is_success() {
        return Err(format!("iLink HTTP failure: {status}"));
    }
    let ret = body.get("ret").and_then(Value::as_i64).unwrap_or(0);
    if ret != 0 {
        let errmsg = body
            .get("errmsg")
            .and_then(Value::as_str)
            .map(|value| bounded(value, 240))
            .unwrap_or_else(|| "unknown iLink error".into());
        return Err(format!(
            "iLink rejected message: ret={ret}, errmsg={errmsg}"
        ));
    }
    Ok(())
}

fn text_message_body(to_user_id: &str, context_token: &str, text: &str) -> Value {
    json!({
        "msg": {
            "from_user_id": "",
            "to_user_id": to_user_id,
            "client_id": uuid::Uuid::new_v4().simple().to_string(),
            "message_type": 2,
            "message_state": 2,
            "item_list": [{
                "type": 1,
                "text_item": { "text": text }
            }],
            "context_token": context_token
        },
        "base_info": {
            "channel_version": CHANNEL_VERSION,
            "bot_agent": format!("Anchor/{}", env!("CARGO_PKG_VERSION"))
        }
    })
}

fn random_wechat_uin() -> Result<String, String> {
    let mut bytes = [0_u8; 4];
    getrandom::fill(&mut bytes).map_err(|error| format!("random UIN failed: {error}"))?;
    let decimal = u32::from_le_bytes(bytes).to_string();
    Ok(BASE64_STANDARD.encode(decimal.as_bytes()))
}

fn encoded_client_version(version: &str) -> u32 {
    let mut parts = version
        .split('.')
        .take(3)
        .map(|part| part.parse::<u32>().unwrap_or(0).min(255));
    let major = parts.next().unwrap_or(0);
    let minor = parts.next().unwrap_or(0);
    let patch = parts.next().unwrap_or(0);
    (major << 16) | (minor << 8) | patch
}

pub fn normalize_base_url(raw: &str) -> Result<String, String> {
    let mut url = Url::parse(raw.trim()).map_err(|_| "invalid iLink base URL".to_string())?;
    if url.scheme() != "https" {
        return Err("iLink base URL must use https".into());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("iLink base URL must not contain credentials".into());
    }
    if url.port().is_some_and(|port| port != 443) {
        return Err("iLink base URL must use the default HTTPS port".into());
    }
    let host = url
        .host_str()
        .ok_or_else(|| "iLink base URL has no host".to_string())?;
    if host != "ilinkai.weixin.qq.com" && !host.ends_with(".weixin.qq.com") {
        return Err("iLink base URL must be hosted under weixin.qq.com".into());
    }
    if !matches!(url.path(), "" | "/") || url.query().is_some() || url.fragment().is_some() {
        return Err("iLink base URL must be an origin without path, query, or fragment".into());
    }
    url.set_path("");
    Ok(url.to_string().trim_end_matches('/').to_string())
}

pub fn redirect_base_url(host: &str) -> Result<String, String> {
    if host.contains('/') || host.contains('@') || host.contains(':') {
        return Err("invalid iLink redirect host".into());
    }
    normalize_base_url(&format!("https://{}", host.trim()))
}

fn parse_qr_status(body: &Value) -> Result<QrStatus, String> {
    match body.get("status").and_then(Value::as_str).unwrap_or("wait") {
        "wait" => Ok(QrStatus::Wait),
        "scaned" => Ok(QrStatus::Scanned),
        "expired" => Ok(QrStatus::Expired),
        "need_verifycode" => Ok(QrStatus::NeedVerifyCode),
        "verify_code_blocked" => Ok(QrStatus::VerifyCodeBlocked),
        "binded_redirect" => Ok(QrStatus::AlreadyBound),
        "scaned_but_redirect" => {
            let host = required_string(body, "redirect_host", "iLink QR redirect")?;
            Ok(QrStatus::Redirect(redirect_base_url(&host)?))
        }
        "confirmed" => {
            let bot_token = required_string(body, "bot_token", "iLink login confirmation")?;
            let bot_id = required_string(body, "ilink_bot_id", "iLink login confirmation")?;
            let login_user_id = required_string(body, "ilink_user_id", "iLink login confirmation")?;
            let base_url = normalize_base_url(
                body.get("baseurl")
                    .and_then(Value::as_str)
                    .filter(|value| !value.trim().is_empty())
                    .unwrap_or(DEFAULT_BASE_URL),
            )?;
            Ok(QrStatus::Confirmed(LoginCredentials {
                bot_token,
                bot_id,
                base_url,
                login_user_id,
            }))
        }
        other => Err(format!(
            "unsupported iLink QR status: {}",
            bounded(other, 80)
        )),
    }
}

fn parse_inbound_text_messages(value: Option<&Value>) -> Vec<InboundTextMessage> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|message| message.get("message_type").and_then(Value::as_i64) == Some(1))
        .filter(|message| message.get("message_state").and_then(Value::as_i64) != Some(1))
        .filter_map(|message| {
            let from_user_id = message.get("from_user_id")?.as_str()?.trim();
            let context_token = message.get("context_token")?.as_str()?.trim();
            if from_user_id.is_empty() || context_token.is_empty() {
                return None;
            }
            let text = message
                .get("item_list")?
                .as_array()?
                .iter()
                .find(|item| item.get("type").and_then(Value::as_i64) == Some(1))?
                .get("text_item")?
                .get("text")?
                .as_str()?
                .to_string();
            Some(InboundTextMessage {
                from_user_id: from_user_id.to_string(),
                context_token: context_token.to_string(),
                text,
            })
        })
        .collect()
}

fn common_headers(
    request: reqwest::RequestBuilder,
    token: Option<&str>,
) -> Result<reqwest::RequestBuilder, String> {
    let request = request
        .header("Content-Type", "application/json")
        .header("AuthorizationType", "ilink_bot_token")
        .header("X-WECHAT-UIN", random_wechat_uin()?)
        .header("iLink-App-Id", "bot")
        .header(
            "iLink-App-ClientVersion",
            encoded_client_version(CHANNEL_VERSION).to_string(),
        );
    Ok(match token.filter(|value| !value.trim().is_empty()) {
        Some(token) => request.bearer_auth(token.trim()),
        None => request,
    })
}

fn base_info() -> Value {
    json!({
        "channel_version": CHANNEL_VERSION,
        "bot_agent": format!("Anchor/{}", env!("CARGO_PKG_VERSION"))
    })
}

fn required_string(body: &Value, key: &str, context: &str) -> Result<String, String> {
    body.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("{context} missing {key}"))
}

fn bounded(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let bounded = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        format!("{bounded}…")
    } else {
        bounded
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_protocol_compatible_text_message() {
        let body = text_message_body("user", "context", "hello");
        assert_eq!(body["msg"]["to_user_id"], "user");
        assert_eq!(body["msg"]["context_token"], "context");
        assert_eq!(body["msg"]["message_type"], 2);
        assert_eq!(body["msg"]["message_state"], 2);
        assert_eq!(body["msg"]["item_list"][0]["type"], 1);
        assert_eq!(body["msg"]["item_list"][0]["text_item"]["text"], "hello");
        assert_eq!(body["base_info"]["channel_version"], CHANNEL_VERSION);
    }

    #[test]
    fn encodes_channel_version_like_ilink_client() {
        assert_eq!(encoded_client_version("2.4.6"), 132_102);
    }

    #[test]
    fn only_accepts_weixin_https_base_urls() {
        assert_eq!(
            normalize_base_url("https://ilinkai.weixin.qq.com/").expect("default"),
            DEFAULT_BASE_URL
        );
        assert!(normalize_base_url("http://ilinkai.weixin.qq.com").is_err());
        assert!(normalize_base_url("https://example.com").is_err());
        assert!(normalize_base_url("https://evil.weixin.qq.com.example.com").is_err());
        assert!(normalize_base_url("https://user@ilinkai.weixin.qq.com").is_err());
        assert!(normalize_base_url("https://ilinkai.weixin.qq.com:8443").is_err());
        assert!(normalize_base_url("https://ilinkai.weixin.qq.com/other").is_err());
        assert!(normalize_base_url("https://ilinkai.weixin.qq.com/?x=1").is_err());
        assert!(normalize_base_url("https://shard.weixin.qq.com").is_ok());
    }

    #[test]
    fn parses_qr_confirmation_and_rejects_untrusted_redirect() {
        let confirmed = parse_qr_status(&json!({
            "status": "confirmed",
            "bot_token": "token",
            "ilink_bot_id": "bot",
            "baseurl": "https://shard.weixin.qq.com",
            "ilink_user_id": "scanner"
        }))
        .expect("confirmed");
        assert!(matches!(confirmed, QrStatus::Confirmed(_)));
        assert!(parse_qr_status(&json!({
            "status": "scaned_but_redirect",
            "redirect_host": "evil.example.com"
        }))
        .is_err());
    }

    #[test]
    fn parses_only_finished_user_text_messages() {
        let messages = parse_inbound_text_messages(Some(&json!([
            {
                "message_type": 1,
                "message_state": 2,
                "from_user_id": "user",
                "context_token": "ctx",
                "item_list": [{"type": 1, "text_item": {"text": "/bind"}}]
            },
            {
                "message_type": 2,
                "message_state": 2,
                "from_user_id": "bot",
                "context_token": "bot-ctx",
                "item_list": [{"type": 1, "text_item": {"text": "ignore"}}]
            },
            {
                "message_type": 1,
                "message_state": 2,
                "from_user_id": "media-user",
                "context_token": "media-ctx",
                "item_list": [{"type": 2}]
            }
        ])));
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].from_user_id, "user");
        assert_eq!(messages[0].context_token, "ctx");
        assert_eq!(messages[0].text, "/bind");
    }

    #[test]
    fn uin_header_is_base64_decimal_u32() {
        let encoded = random_wechat_uin().expect("uin");
        let decoded = BASE64_STANDARD.decode(encoded).expect("base64");
        let value = String::from_utf8(decoded).expect("utf8");
        assert!(value.parse::<u32>().is_ok());
    }
}
