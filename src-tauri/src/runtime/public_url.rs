use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, RwLock, Weak};

pub type SharedPublicUrl = Arc<RwLock<String>>;
type PublicUrlKey = (String, String);
type PublicUrlRegistry = HashMap<PublicUrlKey, Weak<RwLock<String>>>;

static PUBLIC_URLS: OnceLock<Mutex<PublicUrlRegistry>> = OnceLock::new();

fn registry() -> &'static Mutex<PublicUrlRegistry> {
    PUBLIC_URLS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn current_public_url(workspace_id: &str, service: &str) -> Option<String> {
    let key = (workspace_id.to_string(), service.to_string());
    let shared = {
        let mut urls = registry().lock().expect("public URL registry lock");
        let shared = urls.get(&key).and_then(Weak::upgrade);
        if shared.is_none() {
            urls.remove(&key);
        }
        shared
    }?;
    Some(read_public_url(&shared))
}

pub fn register_public_url(
    workspace_id: &str,
    service: &str,
    initial_url: String,
) -> SharedPublicUrl {
    let shared = Arc::new(RwLock::new(normalize_url(&initial_url)));
    let mut urls = registry().lock().expect("public URL registry lock");
    urls.retain(|_, value| value.strong_count() > 0);
    urls.insert(
        (workspace_id.to_string(), service.to_string()),
        Arc::downgrade(&shared),
    );
    shared
}

pub fn update_public_url(workspace_id: &str, service: &str, url: &str) -> bool {
    let key = (workspace_id.to_string(), service.to_string());
    let shared = {
        let mut urls = registry().lock().expect("public URL registry lock");
        let shared = urls.get(&key).and_then(Weak::upgrade);
        if shared.is_none() {
            urls.remove(&key);
        }
        shared
    };
    let Some(shared) = shared else {
        return false;
    };
    *shared.write().expect("public URL write lock") = normalize_url(url);
    true
}

pub fn read_public_url(shared: &SharedPublicUrl) -> String {
    shared.read().expect("public URL read lock").clone()
}

fn normalize_url(url: &str) -> String {
    url.trim().trim_end_matches('/').to_string()
}

#[cfg(test)]
mod tests {
    use super::{current_public_url, read_public_url, register_public_url, update_public_url};

    #[test]
    fn registered_listener_url_can_be_updated_without_restart() {
        let url = register_public_url("workspace", "mcp", String::new());
        assert_eq!(read_public_url(&url), "");
        assert!(update_public_url(
            "workspace",
            "mcp",
            "https://example.com/"
        ));
        assert_eq!(read_public_url(&url), "https://example.com");
        assert_eq!(
            current_public_url("workspace", "mcp").as_deref(),
            Some("https://example.com")
        );
    }

    #[test]
    fn stale_listener_registration_is_removed() {
        let url = register_public_url("stale", "actions", String::new());
        drop(url);
        assert!(!update_public_url(
            "stale",
            "actions",
            "https://example.com"
        ));
        assert!(current_public_url("stale", "actions").is_none());
    }
}
