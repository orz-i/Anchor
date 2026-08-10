use crate::data::{AppData, DataStore};
use crate::error::AppResult;

const OAUTH_REFRESH_REPLAY_SCOPE: &str = "oauth_refresh_replay";

pub struct SecretStore;

impl SecretStore {
    pub fn consume_refresh_token(
        replay_key: &str,
        jti: &str,
        expires_at: u64,
        now: u64,
    ) -> AppResult<bool> {
        DataStore::update_file(|data| {
            consume_refresh_token_in(data, replay_key, jti, expires_at, now)
        })
    }

    #[cfg(feature = "desktop")]
    pub fn clear_refresh_replay_state(workspace_id: &str) -> AppResult<()> {
        DataStore::update_file(|data| {
            clear_refresh_replay_state_in(data, workspace_id);
            Ok(())
        })
    }

    pub fn set(profile_id: &str, key: &str, value: &str) -> AppResult<()> {
        DataStore::update_file(|data| {
            set_workspace_secret_in(data, profile_id, key, value);
            Ok(())
        })
    }

    pub fn get(profile_id: &str, key: &str) -> AppResult<Option<String>> {
        DataStore::read_file(|data| Ok(workspace_secret_from(data, profile_id, key)))
    }

    pub fn regenerate(profile_id: &str, key: &str) -> AppResult<String> {
        let value = random_secret();
        Self::set(profile_id, key, &value)?;
        Ok(value)
    }

    pub fn get_shared(key: &str) -> AppResult<Option<String>> {
        DataStore::read_file(|data| Ok(data.shared_secrets.get(key).cloned()))
    }

    pub fn get_app(scope: &str, item_id: &str) -> AppResult<Option<String>> {
        DataStore::read_file(|data| {
            Ok(data
                .app_secrets
                .get(scope)
                .and_then(|items| items.get(item_id))
                .filter(|value| !value.is_empty())
                .cloned())
        })
    }
}

fn consume_refresh_token_in(
    data: &mut AppData,
    replay_key: &str,
    jti: &str,
    expires_at: u64,
    now: u64,
) -> AppResult<bool> {
    let items = data
        .app_secrets
        .entry(OAUTH_REFRESH_REPLAY_SCOPE.into())
        .or_default();
    let mut used = items
        .get(replay_key)
        .and_then(|raw| serde_json::from_str::<std::collections::HashMap<String, u64>>(raw).ok())
        .unwrap_or_default();
    used.retain(|_, expiry| *expiry >= now);
    if jti.is_empty() || used.contains_key(jti) {
        return Ok(false);
    }
    used.insert(jti.to_string(), expires_at);
    items.insert(replay_key.to_string(), serde_json::to_string(&used)?);
    Ok(true)
}

#[cfg(any(feature = "desktop", test))]
fn clear_refresh_replay_state_in(data: &mut AppData, workspace_id: &str) {
    let Some(items) = data.app_secrets.get_mut(OAUTH_REFRESH_REPLAY_SCOPE) else {
        return;
    };
    let prefix = format!("{workspace_id}:");
    items.retain(|key, _| !key.starts_with(&prefix));
    if items.is_empty() {
        data.app_secrets.remove(OAUTH_REFRESH_REPLAY_SCOPE);
    }
}

fn set_workspace_secret_in(data: &mut AppData, profile_id: &str, key: &str, value: &str) {
    workspace_secret_map(data, profile_id).insert(key.to_string(), value.to_string());
}

fn workspace_secret_from(data: &AppData, profile_id: &str, key: &str) -> Option<String> {
    data.workspace_secrets
        .get(profile_id)
        .and_then(|secrets| secrets.get(key))
        .filter(|value| !value.is_empty())
        .cloned()
}

fn workspace_secret_map<'a>(
    data: &'a mut crate::data::AppData,
    profile_id: &str,
) -> &'a mut std::collections::HashMap<String, String> {
    data.workspace_secrets
        .entry(profile_id.to_string())
        .or_default()
}

fn random_secret() -> String {
    format!("{}{}", uuid::Uuid::new_v4(), uuid::Uuid::new_v4()).replace('-', "")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn random_secret_is_non_empty() {
        assert!(random_secret().len() > 32);
    }

    #[test]
    fn workspace_secret_roundtrip() {
        let id = uuid::Uuid::new_v4().to_string().replace('-', "");
        let mut data = AppData::default();
        set_workspace_secret_in(&mut data, &id, "oauth_client_secret", "roundtrip-secret");
        let loaded = workspace_secret_from(&data, &id, "oauth_client_secret");
        assert_eq!(loaded.as_deref(), Some("roundtrip-secret"));
    }

    #[test]
    fn refresh_token_consumption_persists_across_calls() {
        let workspace_id = uuid::Uuid::new_v4().simple().to_string();
        let replay_key = format!("{workspace_id}:mcp");
        let mut data = AppData::default();
        assert!(consume_refresh_token_in(&mut data, &replay_key, "jti-1", 200, 100).unwrap());
        assert!(!consume_refresh_token_in(&mut data, &replay_key, "jti-1", 200, 100).unwrap());
        assert!(consume_refresh_token_in(&mut data, &replay_key, "jti-2", 300, 201).unwrap());
        clear_refresh_replay_state_in(&mut data, &workspace_id);
        assert!(!data.app_secrets.contains_key(OAUTH_REFRESH_REPLAY_SCOPE));
    }
}
