use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, Weak};

use super::oauth_flow::OAuthRuntime;

type OAuthRuntimeKey = (String, String);
type OAuthRuntimeRegistry = HashMap<OAuthRuntimeKey, Weak<OAuthRuntime>>;

static OAUTH_RUNTIMES: OnceLock<Mutex<OAuthRuntimeRegistry>> = OnceLock::new();

fn registry() -> &'static Mutex<OAuthRuntimeRegistry> {
    OAUTH_RUNTIMES.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn register_oauth_runtime(
    workspace_id: &str,
    service: &str,
    runtime: &Arc<OAuthRuntime>,
) {
    let mut runtimes = registry().lock().expect("oauth runtime registry lock");
    runtimes.retain(|_, value| value.strong_count() > 0);
    runtimes.insert(
        (workspace_id.to_string(), service.to_string()),
        Arc::downgrade(runtime),
    );
}

pub fn update_oauth_redirect_policy(
    workspace_id: &str,
    service: &str,
    redirect_uris: &str,
    redirect_hosts: &str,
) -> Result<bool, String> {
    let key = (workspace_id.to_string(), service.to_string());
    let runtime = {
        let mut runtimes = registry().lock().expect("oauth runtime registry lock");
        let runtime = runtimes.get(&key).and_then(Weak::upgrade);
        if runtime.is_none() {
            runtimes.remove(&key);
        }
        runtime
    };
    let Some(runtime) = runtime else {
        return Ok(false);
    };
    runtime.update_redirect_policy(redirect_uris, redirect_hosts)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn running_oauth_policy_can_be_hot_updated() {
        let runtime = Arc::new(OAuthRuntime::new(
            "https://service.example".into(),
            "client".into(),
            None,
            "password".into(),
            "token-secret".into(),
        ));
        register_oauth_runtime("workspace-hot-policy", "mcp", &runtime);
        assert!(update_oauth_redirect_policy(
            "workspace-hot-policy",
            "mcp",
            "https://chatgpt.com/callback/new",
            "*.chatgpt.com",
        )
        .expect("hot update"));
        assert!(runtime.redirect_uri_allowed("https://chatgpt.com/callback/new"));
        assert_eq!(
            runtime.redirect_uri_status_label("https://oauth.chatgpt.com/callback/dynamic"),
            "enrollment_required"
        );
    }
}
