//! Anchor-managed external software.
//!
//! Software can be installed into Anchor-managed storage (downloaded from
//! GitHub, honoring the mirror + proxy config). Managed installations are
//! uninstallable here; binaries found on PATH remain externally owned.

use std::io::Cursor;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::error::{AppError, AppResult};
use crate::platform::platform;
use crate::tunnel::cloudflare::resolve_cloudflared;
use crate::tunnel::cloudflare::{cached_cloudflared_path, download_cloudflared_to_cache};
use crate::tunnel::frp::{cached_frpc_path, download_frpc_to_cache, resolve_frpc};

// 15.2.0 currently has an open regression in the published x86_64 Linux musl
// binary. Keep the managed build on the previous release until upstream ships
// a fixed binary; system-installed newer rg remains usable and is detected.
const RIPGREP_VERSION: &str = "15.1.0";
const CODEGRAPH_VERSION: &str = "v1.5.0";
const SUPPORTED_SOFTWARE_KINDS: &[&str] = &["frpc", "cloudflared", "ripgrep", "codegraph"];

/// Status of Anchor-managed external software, serialized to the frontend.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SoftwareStatus {
    /// Stable software kind used by the CLI/admin API.
    pub kind: String,
    /// Human-facing display name.
    pub name: String,
    /// Whether the binary was found anywhere (cache, PATH, or system dir).
    pub installed: bool,
    /// Resolved path if found.
    pub path: String,
    /// True when the resolved binary lives in the app cache dir (uninstallable).
    pub managed: bool,
    /// Pinned version used by managed installs for this software kind.
    pub target_version: String,
}

fn codegraph_install_root() -> Option<PathBuf> {
    platform()
        .app_config_dir()
        .ok()
        .map(|dir| dir.join("software").join("codegraph"))
}

fn managed_codegraph_version_root() -> Option<PathBuf> {
    codegraph_install_root().map(|root| root.join(CODEGRAPH_VERSION))
}

pub(crate) fn cached_codegraph_path() -> Option<PathBuf> {
    managed_codegraph_version_root().map(|root| {
        root.join("bin").join(if cfg!(windows) {
            "codegraph.cmd"
        } else {
            "codegraph"
        })
    })
}

pub(crate) fn resolve_codegraph() -> Option<PathBuf> {
    cached_codegraph_path()
        .filter(|path| path.is_file())
        .or_else(|| which::which("codegraph").ok())
}

pub(crate) fn managed_codegraph_available() -> bool {
    cached_codegraph_path().is_some_and(|path| path.is_file())
}

pub(crate) fn resolve_managed_software_program(name: &str) -> Option<PathBuf> {
    match name {
        "rg" | "ripgrep" => cached_ripgrep_path().filter(|path| path.is_file()),
        _ => None,
    }
}

fn codegraph_status() -> SoftwareStatus {
    let cache = cached_codegraph_path().filter(|path| path.is_file());
    let resolved = resolve_codegraph();
    let (path, managed, installed) = match (&cache, &resolved) {
        (Some(cache_path), _) => (cache_path.clone(), true, true),
        (None, Some(found)) => (found.clone(), false, true),
        (None, None) => (PathBuf::new(), false, false),
    };
    SoftwareStatus {
        kind: "codegraph".into(),
        name: "CodeGraph".into(),
        installed,
        path: path.to_string_lossy().to_string(),
        managed,
        target_version: CODEGRAPH_VERSION.into(),
    }
}

fn ripgrep_status() -> SoftwareStatus {
    let cache = cached_ripgrep_path().filter(|path| path.is_file());
    let resolved = resolve_ripgrep();
    let (path, managed, installed) = match (&cache, &resolved) {
        (Some(cache_path), _) => (cache_path.clone(), true, true),
        (None, Some(found)) => (found.clone(), false, true),
        (None, None) => (PathBuf::new(), false, false),
    };
    SoftwareStatus {
        kind: "ripgrep".into(),
        name: "ripgrep (rg)".into(),
        installed,
        path: path.to_string_lossy().to_string(),
        managed,
        target_version: RIPGREP_VERSION.into(),
    }
}

pub fn is_supported_kind(kind: &str) -> bool {
    SUPPORTED_SOFTWARE_KINDS.contains(&kind)
}

pub fn supported_kinds() -> &'static [&'static str] {
    SUPPORTED_SOFTWARE_KINDS
}

pub fn target_version(kind: &str) -> AppResult<&'static str> {
    match kind {
        "frpc" => Ok(crate::tunnel::frp::VERSION),
        "cloudflared" => Ok(crate::tunnel::cloudflare::VERSION),
        "ripgrep" => Ok(RIPGREP_VERSION),
        "codegraph" => Ok(CODEGRAPH_VERSION),
        other => Err(AppError::Message(format!("未知软件: {other}"))),
    }
}

pub(crate) fn cached_ripgrep_path() -> Option<PathBuf> {
    platform()
        .app_config_dir()
        .ok()
        .map(|dir| dir.join("bin").join(ripgrep_binary_name()))
}

pub(crate) fn resolve_ripgrep() -> Option<PathBuf> {
    cached_ripgrep_path()
        .filter(|path| path.is_file())
        .or_else(|| which::which("rg").ok())
}

fn ripgrep_binary_name() -> &'static str {
    #[cfg(windows)]
    {
        "rg.exe"
    }
    #[cfg(not(windows))]
    {
        "rg"
    }
}

fn frpc_status() -> SoftwareStatus {
    let cache = cached_frpc_path().filter(|p| p.is_file());
    let resolved = resolve_frpc().ok();
    // Prefer showing the cache-managed copy when present.
    let (path, managed, installed) = match (&cache, &resolved) {
        (Some(cache_path), _) => (cache_path.clone(), true, true),
        (None, Some(found)) => (found.clone(), false, true),
        (None, None) => (PathBuf::new(), false, false),
    };
    SoftwareStatus {
        kind: "frpc".into(),
        name: "frp 客户端 (frpc)".into(),
        installed,
        path: path.to_string_lossy().to_string(),
        managed,
        target_version: crate::tunnel::frp::VERSION.into(),
    }
}

fn cloudflared_status() -> SoftwareStatus {
    let cache = cached_cloudflared_path().filter(|p| p.is_file());
    let resolved = resolve_cloudflared().ok();
    let (path, managed, installed) = match (&cache, &resolved) {
        (Some(cache_path), _) => (cache_path.clone(), true, true),
        (None, Some(found)) => (found.clone(), false, true),
        (None, None) => (PathBuf::new(), false, false),
    };
    SoftwareStatus {
        kind: "cloudflared".into(),
        name: "Cloudflare Tunnel (cloudflared)".into(),
        installed,
        path: path.to_string_lossy().to_string(),
        managed,
        target_version: crate::tunnel::cloudflare::VERSION.into(),
    }
}

/// Report install status for all supported external software.
pub fn list_software() -> Vec<SoftwareStatus> {
    vec![
        frpc_status(),
        cloudflared_status(),
        ripgrep_status(),
        codegraph_status(),
    ]
}

/// Install the requested software into Anchor-managed storage.
pub async fn install_software(kind: &str) -> AppResult<SoftwareStatus> {
    match kind {
        "frpc" => {
            if cached_frpc_path().is_some_and(|path| path.is_file()) {
                return Ok(frpc_status());
            }
            download_frpc_to_cache().await?;
            Ok(frpc_status())
        }
        "cloudflared" => {
            if cached_cloudflared_path().is_some_and(|path| path.is_file()) {
                return Ok(cloudflared_status());
            }
            download_cloudflared_to_cache().await?;
            Ok(cloudflared_status())
        }
        "ripgrep" => {
            if cached_ripgrep_path().is_some_and(|path| path.is_file()) {
                return Ok(ripgrep_status());
            }
            download_ripgrep_to_cache().await?;
            Ok(ripgrep_status())
        }
        "codegraph" => {
            if cached_codegraph_path().is_some_and(|path| path.is_file()) {
                return Ok(codegraph_status());
            }
            download_codegraph_to_cache().await?;
            Ok(codegraph_status())
        }
        other => Err(AppError::Message(format!("未知软件: {other}"))),
    }
}

/// Uninstall Anchor-managed software. System-managed installations remain
/// untouched and may still be discovered after the managed copy is removed.
pub fn uninstall_software(kind: &str) -> AppResult<SoftwareStatus> {
    if kind == "codegraph" {
        if let Some(root) = codegraph_install_root() {
            if root.exists() {
                std::fs::remove_dir_all(root)?;
            }
        }
        return Ok(codegraph_status());
    }
    let cache_path = match kind {
        "frpc" => cached_frpc_path(),
        "cloudflared" => cached_cloudflared_path(),
        "ripgrep" => cached_ripgrep_path(),
        other => return Err(AppError::Message(format!("未知软件: {other}"))),
    };

    let Some(path) = cache_path else {
        return Err(AppError::Message("无法解析缓存目录。".into()));
    };

    if path.is_file() {
        std::fs::remove_file(&path)?;
    } else {
        return Err(AppError::Message(
            "该软件不是由本应用安装的，无法在此卸载。".into(),
        ));
    }

    // Also clear any cached download archives for frpc to force a fresh fetch.
    if kind == "frpc" {
        if let Ok(dir) = platform().app_config_dir() {
            let downloads = dir.join("bin").join("downloads");
            let _ = std::fs::remove_dir_all(&downloads);
        }
    }

    Ok(match kind {
        "frpc" => frpc_status(),
        "cloudflared" => cloudflared_status(),
        "ripgrep" => ripgrep_status(),
        _ => unreachable!("validated software kind"),
    })
}

fn ripgrep_release_asset() -> AppResult<String> {
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    {
        return Ok(format!(
            "ripgrep-{RIPGREP_VERSION}-x86_64-pc-windows-msvc.zip"
        ));
    }
    #[cfg(all(target_os = "windows", target_arch = "aarch64"))]
    {
        return Ok(format!(
            "ripgrep-{RIPGREP_VERSION}-aarch64-pc-windows-msvc.zip"
        ));
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        return Ok(format!(
            "ripgrep-{RIPGREP_VERSION}-x86_64-unknown-linux-musl.tar.gz"
        ));
    }
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    {
        return Ok(format!(
            "ripgrep-{RIPGREP_VERSION}-aarch64-unknown-linux-gnu.tar.gz"
        ));
    }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        return Ok(format!(
            "ripgrep-{RIPGREP_VERSION}-x86_64-apple-darwin.tar.gz"
        ));
    }
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        return Ok(format!(
            "ripgrep-{RIPGREP_VERSION}-aarch64-apple-darwin.tar.gz"
        ));
    }
    #[allow(unreachable_code)]
    Err(AppError::Message(
        "当前平台暂不支持自动下载 ripgrep。".into(),
    ))
}

async fn download_ripgrep_to_cache() -> AppResult<PathBuf> {
    let settings = crate::settings::AppSettings::load()?;
    let asset = ripgrep_release_asset()?;
    let url = format!(
        "https://github.com/BurntSushi/ripgrep/releases/download/{RIPGREP_VERSION}/{asset}"
    );
    let bytes = crate::tunnel::download::download_release_asset(&settings, &url, "ripgrep").await?;
    let dest = cached_ripgrep_path()
        .ok_or_else(|| AppError::Message("无法解析 ripgrep 缓存目录。".into()))?;
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    extract_ripgrep_binary(&bytes, &asset, &dest)?;
    make_executable(&dest)?;
    if dest.is_file() {
        Ok(dest)
    } else {
        Err(AppError::Message("ripgrep 自动安装失败。".into()))
    }
}

fn extract_ripgrep_binary(bytes: &[u8], asset: &str, dest: &Path) -> AppResult<()> {
    if asset.ends_with(".zip") {
        let reader = Cursor::new(bytes);
        let mut archive = zip::ZipArchive::new(reader)
            .map_err(|err| AppError::Message(format!("解压 ripgrep 安装包失败: {err}")))?;
        for index in 0..archive.len() {
            let mut entry = archive
                .by_index(index)
                .map_err(|err| AppError::Message(format!("读取 ripgrep 安装包失败: {err}")))?;
            if entry.name().replace('\\', "/").ends_with("/rg.exe") {
                let mut output = std::fs::File::create(dest)?;
                std::io::copy(&mut entry, &mut output)?;
                return Ok(());
            }
        }
    } else {
        let decoder = flate2::read::GzDecoder::new(Cursor::new(bytes));
        let mut archive = tar::Archive::new(decoder);
        for entry in archive
            .entries()
            .map_err(|err| AppError::Message(format!("解压 ripgrep 安装包失败: {err}")))?
        {
            let mut entry = entry
                .map_err(|err| AppError::Message(format!("读取 ripgrep 安装包失败: {err}")))?;
            let path = entry
                .path()
                .map_err(|err| AppError::Message(format!("读取 ripgrep 安装路径失败: {err}")))?;
            if path.to_string_lossy().replace('\\', "/").ends_with("/rg") {
                let mut output = std::fs::File::create(dest)?;
                std::io::copy(&mut entry, &mut output)?;
                return Ok(());
            }
        }
    }
    Err(AppError::Message(
        "ripgrep 安装包中未找到 rg 可执行文件。".into(),
    ))
}

fn make_executable(path: &Path) -> AppResult<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(path)?.permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(path, permissions)?;
    }
    Ok(())
}

fn codegraph_release_asset() -> AppResult<&'static str> {
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    {
        return Ok("codegraph-win32-x64.zip");
    }
    #[cfg(all(target_os = "windows", target_arch = "aarch64"))]
    {
        return Ok("codegraph-win32-arm64.zip");
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        return Ok("codegraph-linux-x64.tar.gz");
    }
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    {
        return Ok("codegraph-linux-arm64.tar.gz");
    }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        return Ok("codegraph-darwin-x64.tar.gz");
    }
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        return Ok("codegraph-darwin-arm64.tar.gz");
    }
    #[allow(unreachable_code)]
    Err(AppError::Message(
        "当前平台暂不支持自动下载 CodeGraph。".into(),
    ))
}

async fn download_codegraph_to_cache() -> AppResult<PathBuf> {
    let settings = crate::settings::AppSettings::load()?;
    let asset = codegraph_release_asset()?;
    let url = format!(
        "https://github.com/colbymchenry/codegraph/releases/download/{CODEGRAPH_VERSION}/{asset}"
    );
    let bytes =
        crate::tunnel::download::download_release_asset(&settings, &url, "CodeGraph").await?;
    let install_root = codegraph_install_root()
        .ok_or_else(|| AppError::Message("无法解析 CodeGraph 安装目录。".into()))?;
    let target = managed_codegraph_version_root()
        .ok_or_else(|| AppError::Message("无法解析 CodeGraph 版本目录。".into()))?;
    std::fs::create_dir_all(&install_root)?;
    let staging = install_root.join(format!(".install-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&staging)?;

    let install_result = (|| -> AppResult<()> {
        extract_codegraph_bundle(&bytes, asset, &staging)?;
        let launcher = staging.join("bin").join(if cfg!(windows) {
            "codegraph.cmd"
        } else {
            "codegraph"
        });
        if !launcher.is_file() {
            return Err(AppError::Message(
                "CodeGraph 安装包中未找到 launcher。".into(),
            ));
        }
        make_executable(&launcher)?;
        if target.exists() {
            std::fs::remove_dir_all(&target)?;
        }
        std::fs::rename(&staging, &target)?;
        // The active version is already installed atomically at this point.
        // Stale-version cleanup is best-effort so a filesystem cleanup issue
        // cannot turn a successful installation into a reported failure.
        let _ = prune_old_codegraph_versions(&install_root, &target);
        Ok(())
    })();
    if install_result.is_err() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    install_result?;

    cached_codegraph_path()
        .filter(|path| path.is_file())
        .ok_or_else(|| AppError::Message("CodeGraph 自动安装失败。".into()))
}

fn safe_bundle_relative(path: &Path) -> AppResult<Option<PathBuf>> {
    use std::path::Component;

    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => parts.push(part.to_os_string()),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(AppError::Message(
                    "CodeGraph 安装包包含不安全的路径。".into(),
                ));
            }
        }
    }
    if parts.len() <= 1 {
        return Ok(None);
    }
    Ok(Some(parts.into_iter().skip(1).collect()))
}

fn extract_codegraph_bundle(bytes: &[u8], asset: &str, dest: &Path) -> AppResult<()> {
    if asset.ends_with(".zip") {
        let mut archive = zip::ZipArchive::new(Cursor::new(bytes))
            .map_err(|err| AppError::Message(format!("解压 CodeGraph 安装包失败: {err}")))?;
        for index in 0..archive.len() {
            let mut entry = archive
                .by_index(index)
                .map_err(|err| AppError::Message(format!("读取 CodeGraph 安装包失败: {err}")))?;
            let enclosed = entry
                .enclosed_name()
                .ok_or_else(|| AppError::Message("CodeGraph zip 包含不安全的路径。".into()))?;
            let Some(relative) = safe_bundle_relative(&enclosed)? else {
                continue;
            };
            let output = dest.join(relative);
            if entry.is_dir() {
                std::fs::create_dir_all(&output)?;
                continue;
            }
            if let Some(parent) = output.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut file = std::fs::File::create(output)?;
            std::io::copy(&mut entry, &mut file)?;
        }
        return Ok(());
    }

    let decoder = flate2::read::GzDecoder::new(Cursor::new(bytes));
    let mut archive = tar::Archive::new(decoder);
    for entry in archive
        .entries()
        .map_err(|err| AppError::Message(format!("解压 CodeGraph 安装包失败: {err}")))?
    {
        let mut entry =
            entry.map_err(|err| AppError::Message(format!("读取 CodeGraph 安装包失败: {err}")))?;
        let original = entry
            .path()
            .map_err(|err| AppError::Message(format!("读取 CodeGraph 安装路径失败: {err}")))?;
        let Some(relative) = safe_bundle_relative(&original)? else {
            continue;
        };
        let output = dest.join(relative);
        let kind = entry.header().entry_type();
        if kind.is_dir() {
            std::fs::create_dir_all(output)?;
        } else if kind.is_file() {
            if let Some(parent) = output.parent() {
                std::fs::create_dir_all(parent)?;
            }
            entry
                .unpack(output)
                .map_err(|err| AppError::Message(format!("写入 CodeGraph 文件失败: {err}")))?;
        }
    }
    Ok(())
}

fn prune_old_codegraph_versions(install_root: &Path, keep: &Path) -> AppResult<()> {
    for entry in std::fs::read_dir(install_root)? {
        let entry = entry?;
        let path = entry.path();
        if path == keep || !path.is_dir() {
            continue;
        }
        std::fs::remove_dir_all(path)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn software_catalog_contains_supported_managed_runtimes() {
        let statuses = list_software();
        assert_eq!(statuses.len(), supported_kinds().len());
        assert!(statuses.iter().any(|status| {
            status.kind == "frpc" && status.target_version == crate::tunnel::frp::VERSION
        }));
        assert!(statuses.iter().any(|status| {
            status.kind == "cloudflared"
                && status.target_version == crate::tunnel::cloudflare::VERSION
        }));
        assert!(statuses.iter().any(|status| {
            status.kind == "ripgrep" && status.target_version == RIPGREP_VERSION
        }));
        assert!(statuses.iter().any(|status| {
            status.kind == "codegraph" && status.target_version == CODEGRAPH_VERSION
        }));
        assert_eq!(CODEGRAPH_VERSION, "v1.5.0");
        assert!(resolve_managed_software_program("codegraph").is_none());
    }

    #[test]
    fn codegraph_bundle_extracts_after_stripping_release_root() {
        let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        let mut archive = tar::Builder::new(encoder);
        let content = b"#!/bin/sh\necho codegraph\n";
        let mut header = tar::Header::new_gnu();
        header.set_size(content.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        archive
            .append_data(
                &mut header,
                "codegraph-linux-x64/bin/codegraph",
                &content[..],
            )
            .expect("fixture entry");
        let encoder = archive.into_inner().expect("tar bytes");
        let bytes = encoder.finish().expect("gzip bytes");
        let root = tempdir().expect("extract root");

        extract_codegraph_bundle(&bytes, "codegraph-linux-x64.tar.gz", root.path())
            .expect("extract bundle");

        assert_eq!(
            std::fs::read(root.path().join("bin/codegraph")).expect("launcher"),
            content
        );
        assert!(!root.path().join("codegraph-linux-x64").exists());
    }

    #[test]
    fn codegraph_bundle_rejects_parent_path_components() {
        let error = safe_bundle_relative(Path::new("codegraph-linux-x64/../../escape"))
            .expect_err("parent traversal must be rejected");
        assert!(error.to_string().contains("不安全"));
    }

    #[test]
    fn codegraph_version_pruning_keeps_only_active_bundle() {
        let root = tempdir().expect("install root");
        let keep = root.path().join(CODEGRAPH_VERSION);
        let old = root.path().join("v0.9.5");
        std::fs::create_dir_all(&keep).expect("keep");
        std::fs::create_dir_all(&old).expect("old");

        prune_old_codegraph_versions(root.path(), &keep).expect("prune");

        assert!(keep.is_dir());
        assert!(!old.exists());
    }
}
