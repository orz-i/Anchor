use axum::http::HeaderMap;
use serde_json::{json, Value};

use crate::workspace::AuthConfig;

impl AuthConfig {
    pub fn oauth_enabled(&self) -> bool {
        self.auth_type == "oauth"
    }

    pub fn bearer_enabled(&self) -> bool {
        self.auth_type == "bearer"
    }

    pub fn auth_enabled(&self) -> bool {
        self.auth_type != "noauth"
    }
}

/// Resolve the external OAuth/MCP base URL from trusted configuration.
/// Request-controlled Host/Forwarded headers are intentionally ignored: the
/// listener is reachable through local proxy processes and cannot reliably
/// distinguish a trusted proxy from another local process.
pub fn external_base_url(_headers: &HeaderMap, bind_port: u16, configured_url: &str) -> String {
    let configured = configured_url.trim().trim_end_matches('/');
    if !configured.is_empty() {
        return configured.to_string();
    }

    format!("http://127.0.0.1:{bind_port}")
}

pub fn request_origin_allowed(
    headers: &HeaderMap,
    bind_port: u16,
    configured_url: &str,
) -> bool {
    let Some(origin) = headers.get("origin") else {
        return true;
    };
    let Ok(origin) = origin.to_str() else {
        return false;
    };
    let Ok(origin) = reqwest::Url::parse(origin.trim()) else {
        return false;
    };
    if !origin.username().is_empty()
        || origin.password().is_some()
        || origin.query().is_some()
        || origin.fragment().is_some()
        || origin.path() != "/"
    {
        return false;
    }

    let host = origin.host_str().unwrap_or_default();
    let local = origin.scheme() == "http"
        && matches!(host, "127.0.0.1" | "localhost" | "::1")
        && origin.port_or_known_default() == Some(bind_port);
    if local {
        return true;
    }

    let Ok(configured) = reqwest::Url::parse(configured_url.trim()) else {
        return false;
    };
    origin.scheme() == configured.scheme()
        && origin.host_str() == configured.host_str()
        && origin.port_or_known_default() == configured.port_or_known_default()
}

fn token_endpoint_auth_methods(client_secret: Option<&str>) -> Vec<&'static str> {
    match client_secret {
        Some(secret) if !secret.is_empty() => {
            vec!["client_secret_post", "client_secret_basic"]
        }
        _ => vec!["none"],
    }
}

pub fn authorization_server_metadata(base_url: &str, client_secret: Option<&str>) -> Value {
    let base = base_url.trim_end_matches('/');
    let methods = token_endpoint_auth_methods(client_secret);
    json!({
        "issuer": base,
        "authorization_endpoint": format!("{base}/oauth/authorize"),
        "token_endpoint": format!("{base}/oauth/token"),
        "response_types_supported": ["code"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
        "code_challenge_methods_supported": ["S256"],
        "token_endpoint_auth_methods_supported": methods,
        "scopes_supported": ["mcp"],
    })
}

pub fn protected_resource_metadata(resource_url: &str, authorization_server_url: &str) -> Value {
    let resource = resource_url.trim_end_matches('/');
    let authorization_server = authorization_server_url.trim_end_matches('/');
    json!({
        "resource": resource,
        "authorization_servers": [authorization_server],
        "bearer_methods_supported": ["header"],
        "scopes_supported": ["mcp"],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oauth_enabled_only_for_oauth_type() {
        let mut auth = AuthConfig::default();
        assert!(auth.oauth_enabled());
        auth.auth_type = "bearer".into();
        assert!(!auth.oauth_enabled());
        auth.auth_type = "noauth".into();
        assert!(!auth.oauth_enabled());
    }

    #[test]
    fn authorization_metadata_includes_token_auth_methods() {
        let meta = authorization_server_metadata("https://example.com", None);
        assert_eq!(
            meta["grant_types_supported"],
            json!(["authorization_code", "refresh_token"])
        );
        assert_eq!(
            meta["token_endpoint_auth_methods_supported"],
            json!(["none"])
        );
        let meta = authorization_server_metadata("https://example.com", Some("secret"));
        assert_eq!(
            meta["token_endpoint_auth_methods_supported"],
            json!(["client_secret_post", "client_secret_basic"])
        );
    }

    #[test]
    fn protected_resource_metadata_lists_authorization_servers() {
        let meta = protected_resource_metadata(
            "https://example.com/mcp",
            "https://example.com",
        );
        assert_eq!(meta["authorization_servers"], json!(["https://example.com"]));
        assert_eq!(meta["resource"], "https://example.com/mcp");
    }

    #[test]
    fn external_base_url_prefers_configured_url() {
        let headers = HeaderMap::new();
        assert_eq!(
            external_base_url(&headers, 28767, "https://lb.frp-tx1.evwali.com"),
            "https://lb.frp-tx1.evwali.com"
        );
    }

    #[test]
    fn external_base_url_ignores_untrusted_forwarded_host() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-proto", "https".parse().unwrap());
        headers.insert("x-forwarded-host", "new-tunnel.example.com".parse().unwrap());
        assert_eq!(
            external_base_url(&headers, 28767, "https://old-tunnel.example.com"),
            "https://old-tunnel.example.com"
        );
    }

    #[test]
    fn external_base_url_uses_localhost_without_configuration() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-proto", "https".parse().unwrap());
        headers.insert("x-forwarded-host", "lb.frp-tx1.evwali.com".parse().unwrap());
        assert_eq!(
            external_base_url(&headers, 28767, ""),
            "http://127.0.0.1:28767"
        );
    }

    #[test]
    fn external_base_url_ignores_host_header() {
        let mut headers = HeaderMap::new();
        headers.insert("host", "lb.frp-tx1.evwali.com".parse().unwrap());
        assert_eq!(
            external_base_url(&headers, 28767, ""),
            "http://127.0.0.1:28767"
        );
    }

    #[test]
    fn origin_policy_accepts_local_and_configured_origins_only() {
        let mut headers = HeaderMap::new();
        assert!(request_origin_allowed(
            &headers,
            28767,
            "https://mcp.example.com/path"
        ));
        headers.insert("origin", "http://127.0.0.1:28767".parse().unwrap());
        assert!(request_origin_allowed(
            &headers,
            28767,
            "https://mcp.example.com/path"
        ));
        headers.insert("origin", "https://mcp.example.com".parse().unwrap());
        assert!(request_origin_allowed(
            &headers,
            28767,
            "https://mcp.example.com/path"
        ));
        headers.insert("origin", "https://attacker.example".parse().unwrap());
        assert!(!request_origin_allowed(
            &headers,
            28767,
            "https://mcp.example.com/path"
        ));
    }
}
