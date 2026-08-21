use std::time::Duration;

use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine};
use reqwest::Url;
use serde_json::{json, Value};

const DEFAULT_BASE_URL: &str = "https://ilinkai.weixin.qq.com";
const CHANNEL_VERSION: &str = "2.4.6";
const SEND_TIMEOUT: Duration = Duration::from_secs(15);

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

fn normalize_base_url(raw: &str) -> Result<String, String> {
    let mut url = Url::parse(raw.trim()).map_err(|_| "invalid iLink base URL".to_string())?;
    if url.scheme() != "https" {
        return Err("iLink base URL must use https".into());
    }
    let host = url
        .host_str()
        .ok_or_else(|| "iLink base URL has no host".to_string())?;
    if host != "ilinkai.weixin.qq.com" && !host.ends_with(".weixin.qq.com") {
        return Err("iLink base URL must be hosted under weixin.qq.com".into());
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err("iLink base URL must not contain query or fragment".into());
    }
    if !matches!(url.path(), "" | "/") {
        return Err("iLink base URL must not contain a path".into());
    }
    let normalized_path = url.path().trim_end_matches('/').to_string();
    url.set_path(&normalized_path);
    Ok(url.to_string().trim_end_matches('/').to_string())
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
        assert!(normalize_base_url("https://shard.weixin.qq.com").is_ok());
    }

    #[test]
    fn uin_header_is_base64_decimal_u32() {
        let encoded = random_wechat_uin().expect("uin");
        let decoded = BASE64_STANDARD.decode(encoded).expect("base64");
        let value = String::from_utf8(decoded).expect("utf8");
        assert!(value.parse::<u32>().is_ok());
    }
}
