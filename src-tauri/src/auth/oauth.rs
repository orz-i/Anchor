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

/// Resolve the external OAuth/MCP base URL for a request.
/// Prefer trusted reverse-proxy forwarding headers, then the configured URL,
/// then `Host`, and finally localhost. This prevents quick-tunnel OAuth
/// metadata from remaining pinned to a stale public hostname.
pub fn external_base_url(headers: &HeaderMap, bind_port: u16, configured_url: &str) -> String {
    let configured = configured_url.trim().trim_end_matches('/');
    let proto = {
        let value = first_header_value(headers, "x-forwarded-proto");
        if value.is_empty() {
            forwarded_header_param(headers, "proto")
        } else {
            value
        }
    };
    let forwarded_host = {
        let value = safe_external_host(&first_header_value(headers, "x-forwarded-host"));
        if !value.is_empty() {
            value
        } else {
            safe_external_host(&forwarded_header_param(headers, "host"))
        }
    };

    if !forwarded_host.is_empty() {
        let proto = resolve_external_proto(
            if proto.is_empty() {
                None
            } else {
                Some(proto.as_str())
            },
            &forwarded_host,
        );
        return format!("{proto}://{forwarded_host}");
    }

    if !configured.is_empty() {
        return configured.to_string();
    }

    let host = safe_external_host(&first_header_value(headers, "host"));

    let host = if host.is_empty() {
        format!("127.0.0.1:{bind_port}")
    } else {
        host
    };
    let proto = resolve_external_proto(
        if proto.is_empty() {
            None
        } else {
            Some(proto.as_str())
        },
        &host,
    );
    format!("{proto}://{host}")
}

fn first_header_value(headers: &HeaderMap, name: &str) -> String {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.split(',').next().unwrap_or("").trim().to_string())
        .unwrap_or_default()
}

fn forwarded_header_param(headers: &HeaderMap, name: &str) -> String {
    let first = first_header_value(headers, "forwarded");
    for part in first.split(';') {
        let part = part.trim();
        if let Some((key, value)) = part.split_once('=') {
            if key.trim().eq_ignore_ascii_case(name) {
                return value.trim().trim_matches('"').to_string();
            }
        }
    }
    String::new()
}

fn safe_external_host(host: &str) -> String {
    let host = host.trim();
    if host.is_empty() || host.chars().any(|ch| matches!(ch, '\r' | '\n' | '/' | '\\')) {
        String::new()
    } else {
        host.to_string()
    }
}

fn resolve_external_proto(proto: Option<&str>, host: &str) -> &'static str {
    if let Some(proto) = proto {
        let proto = proto.trim().to_ascii_lowercase();
        if proto == "http" {
            return "http";
        }
        if proto == "https" {
            return "https";
        }
    }

    let host_without_port = host
        .rsplit_once(':')
        .map(|(value, _)| value.trim_matches('[').trim_matches(']'))
        .unwrap_or_else(|| host.trim_matches('[').trim_matches(']'));
    if is_loopback_host(host_without_port) {
        "http"
    } else {
        "https"
    }
}

fn is_loopback_host(host: &str) -> bool {
    matches!(host, "127.0.0.1" | "localhost" | "::1")
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
    fn external_base_url_prefers_forwarded_host_over_stale_configured_url() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-proto", "https".parse().unwrap());
        headers.insert("x-forwarded-host", "new-tunnel.example.com".parse().unwrap());
        assert_eq!(
            external_base_url(&headers, 28767, "https://old-tunnel.example.com"),
            "https://new-tunnel.example.com"
        );
    }

    #[test]
    fn external_base_url_uses_forwarded_host() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-proto", "https".parse().unwrap());
        headers.insert("x-forwarded-host", "lb.frp-tx1.evwali.com".parse().unwrap());
        assert_eq!(
            external_base_url(&headers, 28767, ""),
            "https://lb.frp-tx1.evwali.com"
        );
    }

    #[test]
    fn external_base_url_uses_host_header() {
        let mut headers = HeaderMap::new();
        headers.insert("host", "lb.frp-tx1.evwali.com".parse().unwrap());
        assert_eq!(
            external_base_url(&headers, 28767, ""),
            "https://lb.frp-tx1.evwali.com"
        );
    }
}
