mod client;

use crate::error::{AppError, AppResult};
use crate::settings::AppSettings;
use crate::workspace::WorkspaceProfile;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use super::TunnelServiceKind;

pub(crate) use client::{
    acquire_frpc_operation_lock, clear_managed_frpc_pid, managed_frpc_config_matches,
    stop_recorded_frpc_instance,
};
pub(crate) use client::{cached_frpc_path, download_frpc_to_cache};
pub use client::{resolve_frpc, spawn_frpc};

const FRP_VERSION: &str = "0.61.2";
pub(crate) const VERSION: &str = FRP_VERSION;
const FRP_PROXY_HTTP: &str = "http";
const FRP_PROXY_HTTPS2HTTP: &str = "https2http";

pub fn frp_snippet(
    profile: &WorkspaceProfile,
    kind: TunnelServiceKind,
    settings: &AppSettings,
) -> AppResult<String> {
    let config = prepare_frp_server_config(frp_server_config(profile, kind, settings, None))?;
    Ok(build_proxy_snippet(&config.proxy))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FrpProxyConfig {
    pub proxy_name: String,
    pub local_port: u16,
    pub subdomain: String,
    pub proxy_type: String,
    pub cert_path: String,
    pub key_path: String,
    pub workspace_root: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FrpServerConfig {
    pub server_addr: String,
    pub server_port: u16,
    pub token: Option<String>,
    pub public_url: String,
    pub proxy: FrpProxyConfig,
}

pub fn frp_public_url(
    profile: &WorkspaceProfile,
    kind: TunnelServiceKind,
    settings: &AppSettings,
) -> String {
    let config = frp_server_config(profile, kind, settings, None);
    let explicit = config.public_url.trim().trim_end_matches('/');
    if !explicit.is_empty() {
        return explicit.to_string();
    }
    if config.server_addr.is_empty() || config.proxy.subdomain.trim().is_empty() {
        return String::new();
    }
    format!(
        "https://{}.{}",
        config.proxy.subdomain.trim(),
        config.server_addr.trim().trim_end_matches('/')
    )
}

pub fn frp_server_config(
    profile: &WorkspaceProfile,
    kind: TunnelServiceKind,
    settings: &AppSettings,
    token_override: Option<String>,
) -> FrpServerConfig {
    let proxy = frp_proxy_config(profile, kind);
    let (profile_id, server_addr, server_port, public_url) = match kind {
        TunnelServiceKind::Mcp => (
            profile.tunnel.frp_profile_id.as_str(),
            profile.tunnel.frp_server.clone(),
            profile.tunnel.frp_server_port,
            profile.tunnel.public_url.clone(),
        ),
        TunnelServiceKind::Actions => (
            profile.actions.frp_profile_id.as_str(),
            profile.actions.frp_server.clone(),
            profile.actions.frp_server_port,
            profile.actions.public_url.clone(),
        ),
    };

    let (server_addr, server_port) =
        if let Some(frp_profile) = settings.find_frp_profile(profile_id) {
            (frp_profile.server.clone(), frp_profile.server_port)
        } else {
            (server_addr, server_port)
        };

    let token = token_override.or_else(|| resolve_frp_token(profile_id, profile, kind, settings));

    FrpServerConfig {
        server_addr,
        server_port,
        token,
        public_url,
        proxy,
    }
}

fn resolve_frp_token(
    profile_id: &str,
    workspace: &WorkspaceProfile,
    kind: TunnelServiceKind,
    settings: &AppSettings,
) -> Option<String> {
    if !profile_id.trim().is_empty() {
        if let Ok(Some(token)) =
            crate::secret::SecretStore::get_app("frp_profile_token", profile_id)
        {
            if !token.trim().is_empty() {
                return Some(token);
            }
        }
    }

    let workspace_key = match kind {
        TunnelServiceKind::Mcp => "frp_token",
        TunnelServiceKind::Actions => "actions_frp_token",
    };
    if let Ok(Some(token)) = crate::secret::SecretStore::get(&workspace.id, workspace_key) {
        if !token.trim().is_empty() {
            return Some(token);
        }
    }

    // Manual inline server: reuse token from a global profile with the same host.
    let inline_server = match kind {
        TunnelServiceKind::Mcp => workspace.tunnel.frp_server.as_str(),
        TunnelServiceKind::Actions => workspace.actions.frp_server.as_str(),
    };
    let inline_server = inline_server.trim();
    if !inline_server.is_empty() {
        for profile in &settings.frp_profiles {
            if profile.server.trim().eq_ignore_ascii_case(inline_server) {
                if let Ok(Some(token)) =
                    crate::secret::SecretStore::get_app("frp_profile_token", &profile.id)
                {
                    if !token.trim().is_empty() {
                        return Some(token);
                    }
                }
            }
        }
    }

    None
}

/// Build one frpc configuration containing all active proxies.
///
/// A single frpc process can serve multiple workspaces, but all proxies must
/// share the same server connection. The supervisor validates that invariant
/// before calling this function.
pub(crate) fn build_frpc_toml_for_routes(configs: &[FrpServerConfig]) -> String {
    let Some(first) = configs.first() else {
        return String::new();
    };

    let mut lines = vec![
        format!("serverAddr = {}", toml_string(first.server_addr.trim())),
        format!("serverPort = {}", first.server_port),
        String::new(),
    ];
    if let Some(token) = first.token.as_ref().filter(|t| !t.trim().is_empty()) {
        lines.push("auth.method = \"token\"".to_string());
        lines.push(format!("auth.token = {}", toml_string(token.trim())));
        lines.push(String::new());
    }

    let mut used_names = HashSet::new();
    for config in configs {
        let mut proxy = config.proxy.clone();
        let base_name = proxy.proxy_name.clone();
        let mut name = base_name.clone();
        let mut suffix = 2;
        while !used_names.insert(name.clone()) {
            name = format!("{base_name}-{suffix}");
            suffix += 1;
        }
        proxy.proxy_name = name;
        lines.push(build_proxy_snippet(&proxy));
        lines.push(String::new());
    }

    lines.pop();
    lines.join("\n")
}

pub(crate) fn build_frpc_toml_for_route_refs(
    routes: &[(&WorkspaceProfile, TunnelServiceKind)],
    settings: &AppSettings,
) -> AppResult<String> {
    let configs: Vec<FrpServerConfig> = routes
        .iter()
        .map(|(profile, kind)| {
            prepare_frp_server_config(frp_server_config(profile, *kind, settings, None))
        })
        .collect::<AppResult<_>>()?;
    Ok(build_frpc_toml_for_routes(&configs))
}

pub(crate) fn prepare_frp_server_config(mut config: FrpServerConfig) -> AppResult<FrpServerConfig> {
    let mode = config.proxy.proxy_type.trim().to_ascii_lowercase();
    match mode.as_str() {
        FRP_PROXY_HTTP => {
            config.proxy.proxy_type = FRP_PROXY_HTTP.to_string();
            config.proxy.cert_path.clear();
            config.proxy.key_path.clear();
        }
        FRP_PROXY_HTTPS2HTTP => {
            let (cert_path, key_path) = resolve_https2http_certificates(&config.proxy)?;
            config.proxy.proxy_type = FRP_PROXY_HTTPS2HTTP.to_string();
            config.proxy.cert_path = cert_path;
            config.proxy.key_path = key_path;
        }
        _ => {
            return Err(AppError::Message(
                "FRP 代理模式无效，仅支持 http 或 https2http。".into(),
            ));
        }
    }
    Ok(config)
}

fn resolve_https2http_certificates(proxy: &FrpProxyConfig) -> AppResult<(String, String)> {
    let root = PathBuf::from(proxy.workspace_root.trim());
    if root.as_os_str().is_empty() {
        return Err(AppError::Message(
            "工作区路径为空，无法解析 FRP 证书。".into(),
        ));
    }
    let canonical_root = root.canonicalize().map_err(|error| {
        AppError::Message(format!("无法解析工作区路径 {}：{error}", root.display()))
    })?;

    let cert_input = proxy.cert_path.trim();
    let key_input = proxy.key_path.trim();
    let (cert, key) = match (cert_input.is_empty(), key_input.is_empty()) {
        (false, false) => (
            resolve_workspace_certificate(&canonical_root, cert_input, "证书")?,
            resolve_workspace_certificate(&canonical_root, key_input, "私钥")?,
        ),
        (false, true) => {
            let cert = resolve_workspace_certificate(&canonical_root, cert_input, "证书")?;
            let key = cert.with_extension("key");
            (
                cert,
                resolve_workspace_certificate_path(&canonical_root, &key, "私钥")?,
            )
        }
        (true, false) => {
            let key = resolve_workspace_certificate(&canonical_root, key_input, "私钥")?;
            let cert = find_certificate_for_key(&canonical_root, &key)?;
            (cert, key)
        }
        (true, true) => discover_certificate_pair(&canonical_root)?,
    };

    Ok((frp_path_string(&cert), frp_path_string(&key)))
}

fn frp_path_string(path: &Path) -> String {
    let value = path.to_string_lossy();
    #[cfg(windows)]
    {
        if let Some(rest) = value.strip_prefix(r"\\?\UNC\") {
            return format!(r"\\{rest}");
        }
        if let Some(rest) = value.strip_prefix(r"\\?\") {
            return rest.to_string();
        }
    }
    value.into_owned()
}

fn resolve_workspace_certificate(root: &Path, input: &str, label: &str) -> AppResult<PathBuf> {
    let path = PathBuf::from(input);
    let candidate = if path.is_absolute() {
        path
    } else {
        root.join(path)
    };
    resolve_workspace_certificate_path(root, &candidate, label)
}

fn resolve_workspace_certificate_path(
    root: &Path,
    candidate: &Path,
    label: &str,
) -> AppResult<PathBuf> {
    let link_metadata = std::fs::symlink_metadata(candidate).map_err(|error| {
        AppError::Message(format!(
            "FRP {label}文件不存在 {}：{error}",
            candidate.display()
        ))
    })?;
    if link_metadata.file_type().is_symlink() {
        return Err(AppError::Message(format!(
            "FRP {label}文件不能是符号链接：{}",
            candidate.display()
        )));
    }
    if !link_metadata.is_file() || link_metadata.len() == 0 {
        return Err(AppError::Message(format!(
            "FRP {label}必须是非空普通文件：{}",
            candidate.display()
        )));
    }
    let canonical = candidate.canonicalize().map_err(|error| {
        AppError::Message(format!(
            "无法解析 FRP {label}路径 {}：{error}",
            candidate.display()
        ))
    })?;
    if !canonical.starts_with(root) {
        return Err(AppError::Message(format!(
            "FRP {label}必须位于工作区内：{}",
            candidate.display()
        )));
    }
    Ok(canonical)
}

fn discover_certificate_pair(root: &Path) -> AppResult<(PathBuf, PathBuf)> {
    let cert_dir = root.join(".anchor").join("cert");
    let entries = std::fs::read_dir(&cert_dir).map_err(|error| {
        AppError::Message(format!(
            "未找到 FRP 证书目录 {}：{error}",
            cert_dir.display()
        ))
    })?;
    let mut pairs = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !is_certificate_extension(&path) {
            continue;
        }
        let key = path.with_extension("key");
        if key.is_file() {
            pairs.push((path, key));
        }
    }
    if pairs.len() != 1 {
        return Err(AppError::Message(if pairs.is_empty() {
            format!(
                "FRP HTTPS 模式需要在 {} 中放置同名证书和 .key 私钥，或显式填写路径。",
                cert_dir.display()
            )
        } else {
            format!(
                "FRP 证书目录 {} 中存在多个证书对，请显式选择证书和私钥路径。",
                cert_dir.display()
            )
        }));
    }
    let (cert, key) = pairs.remove(0);
    Ok((
        resolve_workspace_certificate_path(root, &cert, "证书")?,
        resolve_workspace_certificate_path(root, &key, "私钥")?,
    ))
}

fn find_certificate_for_key(root: &Path, key: &Path) -> AppResult<PathBuf> {
    for extension in ["pem", "crt", "cer"] {
        let cert = key.with_extension(extension);
        if cert.is_file() {
            return resolve_workspace_certificate_path(root, &cert, "证书");
        }
    }
    Err(AppError::Message(format!(
        "未找到与私钥同名的 .pem/.crt/.cer 证书：{}",
        key.display()
    )))
}

fn is_certificate_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "pem" | "crt" | "cer"
            )
        })
}

fn toml_string(value: &str) -> String {
    format!("{value:?}")
}

fn frp_proxy_config(profile: &WorkspaceProfile, kind: TunnelServiceKind) -> FrpProxyConfig {
    let prefix = workspace_proxy_prefix(&profile.id);
    match kind {
        TunnelServiceKind::Mcp => FrpProxyConfig {
            proxy_name: format!("{prefix}-mcp"),
            local_port: profile.runtime.local_port,
            subdomain: profile.tunnel.frp_subdomain.clone(),
            proxy_type: profile.tunnel.frp_proxy_type.clone(),
            cert_path: profile.tunnel.frp_cert_path.clone(),
            key_path: profile.tunnel.frp_key_path.clone(),
            workspace_root: profile.path.clone(),
        },
        TunnelServiceKind::Actions => FrpProxyConfig {
            proxy_name: format!("{prefix}-actions"),
            local_port: profile.actions.local_port,
            subdomain: profile.actions.frp_subdomain.clone(),
            proxy_type: profile.actions.frp_proxy_type.clone(),
            cert_path: profile.actions.frp_cert_path.clone(),
            key_path: profile.actions.frp_key_path.clone(),
            workspace_root: profile.path.clone(),
        },
    }
}

fn build_proxy_snippet(proxy: &FrpProxyConfig) -> String {
    if proxy.proxy_type == FRP_PROXY_HTTPS2HTTP {
        return [
            "[[proxies]]".to_string(),
            format!("name = {}", toml_string(&proxy.proxy_name)),
            "type = \"https\"".to_string(),
            format!("subdomain = {}", toml_string(proxy.subdomain.trim())),
            String::new(),
            "[proxies.plugin]".to_string(),
            "type = \"https2http\"".to_string(),
            format!(
                "localAddr = {}",
                toml_string(&format!("127.0.0.1:{}", proxy.local_port))
            ),
            format!("crtPath = {}", toml_string(&proxy.cert_path)),
            format!("keyPath = {}", toml_string(&proxy.key_path)),
        ]
        .join("\n");
    }

    [
        "[[proxies]]".to_string(),
        format!("name = {}", toml_string(&proxy.proxy_name)),
        "type = \"http\"".to_string(),
        "localIP = \"127.0.0.1\"".to_string(),
        format!("localPort = {}", proxy.local_port),
        format!("subdomain = {}", toml_string(proxy.subdomain.trim())),
    ]
    .join("\n")
}

fn workspace_proxy_prefix(workspace_id: &str) -> String {
    let stable_id: String = workspace_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(12)
        .collect();
    if stable_id.is_empty() {
        "workspace".to_string()
    } else {
        format!("ws-{}", stable_id.to_ascii_lowercase())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::FrpProfile;
    use crate::workspace::WorkspaceProfile;

    #[test]
    fn mcp_snippet_uses_tunnel_subdomain() {
        let mut profile = WorkspaceProfile::new("/tmp/demo".into(), Some("Demo WS".into()));
        profile.tunnel.frp_subdomain = "demo-mcp".into();
        profile.runtime.local_port = 28766;
        let settings = AppSettings {
            frp_profiles: vec![FrpProfile {
                id: "p1".into(),
                name: "Main".into(),
                server: "frp.example.com".into(),
                server_port: 7000,
            }],
            ..AppSettings::default()
        };
        profile.tunnel.frp_profile_id = "p1".into();

        let snippet =
            frp_snippet(&profile, TunnelServiceKind::Mcp, &settings).expect("build FRP snippet");
        let proxy_name = frp_server_config(&profile, TunnelServiceKind::Mcp, &settings, None)
            .proxy
            .proxy_name;
        assert!(snippet.contains(&format!("name = \"{proxy_name}\"")));
        assert!(snippet.contains("localPort = 28766"));
        assert!(snippet.contains("subdomain = \"demo-mcp\""));
    }

    #[test]
    fn https2http_auto_discovers_workspace_certificate_pair() {
        let temp = tempfile::tempdir().expect("tempdir");
        let cert_dir = temp.path().join(".anchor/cert");
        std::fs::create_dir_all(&cert_dir).expect("cert dir");
        std::fs::write(cert_dir.join("demo.pem"), "certificate").expect("certificate");
        std::fs::write(cert_dir.join("demo.key"), "private-key").expect("private key");

        let mut profile = WorkspaceProfile::new(
            temp.path().to_string_lossy().into_owned(),
            Some("Demo".into()),
        );
        profile.tunnel.frp_server = "frp.example.com".into();
        profile.tunnel.frp_subdomain = "demo".into();
        profile.tunnel.public_url = "https://demo.frp.example.com".into();
        profile.tunnel.frp_proxy_type = "https2http".into();

        let config = prepare_frp_server_config(frp_server_config(
            &profile,
            TunnelServiceKind::Mcp,
            &AppSettings::default(),
            None,
        ))
        .expect("prepare HTTPS config");
        let toml = build_frpc_toml_for_routes(std::slice::from_ref(&config));

        assert_eq!(config.proxy.proxy_type, "https2http");
        assert!(config.proxy.cert_path.ends_with("demo.pem"));
        assert!(config.proxy.key_path.ends_with("demo.key"));
        assert!(toml.contains("type = \"https\""));
        assert!(toml.contains("subdomain = \"demo\""));
        assert!(toml.contains("[proxies.plugin]"));
        assert!(toml.contains("type = \"https2http\""));
        assert!(toml.contains("localAddr = \"127.0.0.1:28766\""));
        assert!(toml.contains("crtPath = "));
        assert!(toml.contains("keyPath = "));
        assert!(!toml.contains("localIP = "));
    }

    #[test]
    fn https2http_rejects_certificate_paths_outside_workspace() {
        let workspace = tempfile::tempdir().expect("workspace");
        let outside = tempfile::tempdir().expect("outside");
        let cert = outside.path().join("outside.pem");
        let key = outside.path().join("outside.key");
        std::fs::write(&cert, "certificate").expect("certificate");
        std::fs::write(&key, "private-key").expect("private key");

        let mut profile = WorkspaceProfile::new(
            workspace.path().to_string_lossy().into_owned(),
            Some("Demo".into()),
        );
        profile.tunnel.frp_server = "frp.example.com".into();
        profile.tunnel.frp_subdomain = "demo".into();
        profile.tunnel.frp_proxy_type = "https2http".into();
        profile.tunnel.frp_cert_path = cert.to_string_lossy().into_owned();
        profile.tunnel.frp_key_path = key.to_string_lossy().into_owned();

        let error = prepare_frp_server_config(frp_server_config(
            &profile,
            TunnelServiceKind::Mcp,
            &AppSettings::default(),
            None,
        ))
        .expect_err("outside certificate must fail");
        assert!(error.to_string().contains("必须位于工作区内"));
    }

    #[test]
    fn build_frpc_toml_uses_global_profile_server() {
        let mut profile = WorkspaceProfile::new("/tmp/demo".into(), Some("Demo".into()));
        profile.tunnel.frp_subdomain = "demo".into();
        profile.tunnel.frp_profile_id = "p1".into();
        let settings = AppSettings {
            frp_profiles: vec![FrpProfile {
                id: "p1".into(),
                name: "Main".into(),
                server: "frp.example.com".into(),
                server_port: 7000,
            }],
            ..AppSettings::default()
        };
        let config = frp_server_config(
            &profile,
            TunnelServiceKind::Mcp,
            &settings,
            Some("secret".into()),
        );
        let toml = build_frpc_toml_for_routes(std::slice::from_ref(&config));
        assert!(toml.contains("serverAddr = \"frp.example.com\""));
        assert!(toml.contains("auth.token = \"secret\""));
    }

    #[test]
    fn build_frpc_toml_for_routes_contains_all_proxies() {
        let mut first = WorkspaceProfile::new("/tmp/first".into(), Some("First".into()));
        first.tunnel.frp_server = "frp.example.com".into();
        first.tunnel.frp_server_port = 7000;
        first.tunnel.frp_subdomain = "first".into();
        first.runtime.local_port = 28766;

        let mut second = WorkspaceProfile::new("/tmp/second".into(), Some("Second".into()));
        second.tunnel.frp_server = "frp.example.com".into();
        second.tunnel.frp_server_port = 7000;
        second.tunnel.frp_subdomain = "second".into();
        second.runtime.local_port = 28767;

        let settings = AppSettings::default();
        let configs = vec![
            frp_server_config(&first, TunnelServiceKind::Mcp, &settings, None),
            frp_server_config(&second, TunnelServiceKind::Mcp, &settings, None),
        ];
        let first_name = configs[0].proxy.proxy_name.clone();
        let second_name = configs[1].proxy.proxy_name.clone();
        let toml = build_frpc_toml_for_routes(&configs);

        assert_eq!(toml.matches("[[proxies]]").count(), 2);
        assert!(toml.contains("serverAddr = \"frp.example.com\""));
        assert!(toml.contains(&format!("name = \"{first_name}\"")));
        assert!(toml.contains(&format!("name = \"{second_name}\"")));
        assert!(toml.contains("localPort = 28766"));
        assert!(toml.contains("localPort = 28767"));
    }

    #[test]
    fn build_frpc_toml_for_routes_supports_mcp_and_actions_together() {
        let mut mcp = WorkspaceProfile::new("/tmp/mcp".into(), Some("MCP".into()));
        mcp.tunnel.frp_server = "frp.example.com".into();
        mcp.tunnel.frp_server_port = 7000;
        mcp.tunnel.frp_subdomain = "mcp".into();
        mcp.runtime.local_port = 28766;

        let mut actions = WorkspaceProfile::new("/tmp/actions".into(), Some("Actions".into()));
        actions.actions.frp_server = "frp.example.com".into();
        actions.actions.frp_server_port = 7000;
        actions.actions.frp_subdomain = "actions".into();
        actions.actions.local_port = 8787;

        let settings = AppSettings::default();
        let configs = vec![
            frp_server_config(&mcp, TunnelServiceKind::Mcp, &settings, None),
            frp_server_config(&actions, TunnelServiceKind::Actions, &settings, None),
        ];
        let mcp_name = configs[0].proxy.proxy_name.clone();
        let actions_name = configs[1].proxy.proxy_name.clone();
        let toml = build_frpc_toml_for_routes(&configs);

        assert_eq!(toml.matches("[[proxies]]").count(), 2);
        assert!(toml.contains(&format!("name = \"{mcp_name}\"")));
        assert!(toml.contains(&format!("name = \"{actions_name}\"")));
        assert!(toml.contains("localPort = 28766"));
        assert!(toml.contains("localPort = 8787"));
    }

    #[test]
    fn build_frpc_toml_for_routes_keeps_workspace_proxy_names_unique() {
        let mut first = WorkspaceProfile::new("/tmp/first".into(), Some("Same Name".into()));
        first.tunnel.frp_server = "frp.example.com".into();
        first.tunnel.frp_server_port = 7000;
        first.tunnel.frp_subdomain = "first".into();

        let mut second = WorkspaceProfile::new("/tmp/second".into(), Some("Same Name".into()));
        second.tunnel.frp_server = "frp.example.com".into();
        second.tunnel.frp_server_port = 7000;
        second.tunnel.frp_subdomain = "second".into();

        let settings = AppSettings::default();
        let configs = vec![
            frp_server_config(&first, TunnelServiceKind::Mcp, &settings, None),
            frp_server_config(&second, TunnelServiceKind::Mcp, &settings, None),
        ];
        let first_name = configs[0].proxy.proxy_name.clone();
        let second_name = configs[1].proxy.proxy_name.clone();
        let toml = build_frpc_toml_for_routes(&configs);

        assert_ne!(first_name, second_name);
        assert!(toml.contains(&format!("name = \"{first_name}\"")));
        assert!(toml.contains(&format!("name = \"{second_name}\"")));
    }

    #[test]
    fn build_frpc_toml_for_routes_returns_empty_for_no_routes() {
        assert!(build_frpc_toml_for_routes(&[]).is_empty());
    }

    #[test]
    fn same_name_workspaces_receive_distinct_proxy_names() {
        let first = WorkspaceProfile::new("/tmp/first".into(), Some("Same Name".into()));
        let second = WorkspaceProfile::new("/tmp/second".into(), Some("Same Name".into()));
        let settings = AppSettings::default();

        let first_config = frp_server_config(&first, TunnelServiceKind::Mcp, &settings, None);
        let second_config = frp_server_config(&second, TunnelServiceKind::Mcp, &settings, None);

        assert_ne!(
            first_config.proxy.proxy_name,
            second_config.proxy.proxy_name
        );
    }

    #[test]
    fn proxy_name_is_stable_when_workspace_is_renamed() {
        let original = WorkspaceProfile::new("/tmp/demo".into(), Some("Before".into()));
        let mut renamed = original.clone();
        renamed.name = "After".into();
        let settings = AppSettings::default();

        let before = frp_server_config(&original, TunnelServiceKind::Mcp, &settings, None);
        let after = frp_server_config(&renamed, TunnelServiceKind::Mcp, &settings, None);

        assert_eq!(before.proxy.proxy_name, after.proxy.proxy_name);
    }

    #[test]
    fn frp_public_url_prefers_explicit_url_over_control_server() {
        let mut profile = WorkspaceProfile::new("/tmp/demo".into(), Some("Demo".into()));
        profile.tunnel.tunnel_type = "frp".into();
        profile.tunnel.frp_server = "43.157.17.95".into();
        profile.tunnel.frp_server_port = 17001;
        profile.tunnel.frp_subdomain = "anchor".into();
        profile.tunnel.public_url = "https://anchor.taoyan.icu/".into();

        assert_eq!(
            frp_public_url(&profile, TunnelServiceKind::Mcp, &AppSettings::default()),
            "https://anchor.taoyan.icu"
        );
    }

    #[test]
    fn explicit_token_override_is_used_for_manual_server() {
        let mut profile = WorkspaceProfile::new("/tmp/demo".into(), Some("Demo".into()));
        profile.tunnel.frp_server = "frp.example.com".into();
        profile.tunnel.frp_subdomain = "demo".into();
        let settings = AppSettings {
            frp_profiles: vec![FrpProfile {
                id: "p1".into(),
                name: "Main".into(),
                server: "frp.example.com".into(),
                server_port: 7000,
            }],
            ..AppSettings::default()
        };
        let config = frp_server_config(
            &profile,
            TunnelServiceKind::Mcp,
            &settings,
            Some("shared-token".into()),
        );
        assert_eq!(config.token.as_deref(), Some("shared-token"));
    }
}
