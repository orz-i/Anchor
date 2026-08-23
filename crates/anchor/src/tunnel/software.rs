//! Anchor-managed external software.
//!
//! Both can be installed into the app cache `bin/` directory (downloaded from
//! GitHub, honoring the mirror + proxy config). Binaries found in the cache dir
//! are "managed" (uninstallable); binaries found on PATH or in system install
//! locations are reported but cannot be removed from here.

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
const SUPPORTED_SOFTWARE_KINDS: &[&str] = &["frpc", "cloudflared", "ripgrep"];

/// Status of a managed tunnel binary, serialized to the frontend.
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
    vec![frpc_status(), cloudflared_status(), ripgrep_status()]
}

/// Install (download into cache) the requested binary.
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
        other => Err(AppError::Message(format!("未知软件: {other}"))),
    }
}

/// Uninstall a cache-managed binary. Refuses if the binary is not in the cache
/// dir (i.e. it was installed by the system / winget / apt and is not ours).
pub fn uninstall_software(kind: &str) -> AppResult<SoftwareStatus> {
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
        _ => ripgrep_status(),
    })
}

fn ripgrep_release_asset() -> AppResult<&'static str> {
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    {
        return Ok("ripgrep-15.1.0-x86_64-pc-windows-msvc.zip");
    }
    #[cfg(all(target_os = "windows", target_arch = "aarch64"))]
    {
        return Ok("ripgrep-15.1.0-aarch64-pc-windows-msvc.zip");
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        return Ok("ripgrep-15.1.0-x86_64-unknown-linux-musl.tar.gz");
    }
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    {
        return Ok("ripgrep-15.1.0-aarch64-unknown-linux-gnu.tar.gz");
    }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        return Ok("ripgrep-15.1.0-x86_64-apple-darwin.tar.gz");
    }
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        return Ok("ripgrep-15.1.0-aarch64-apple-darwin.tar.gz");
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
    extract_ripgrep_binary(&bytes, asset, &dest)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn software_catalog_contains_both_supported_tunnel_binaries() {
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
    }
}
