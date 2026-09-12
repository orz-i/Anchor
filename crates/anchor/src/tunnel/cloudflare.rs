use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot};
use tokio::time;

use crate::error::{AppError, AppResult};
use crate::logging::{forward_bounded_lines, BoundedAsyncLogWriter, PROCESS_LOG_CHANNEL_CAPACITY};
use crate::platform::platform;
use crate::settings::ProxyConfig;

const READY_TIMEOUT: Duration = Duration::from_secs(30);

/// Handle to a supervised `cloudflared` child process.
pub struct CloudflareTunnelHandle {
    pub child: Child,
    pub public_url: String,
    pub pid: Option<u32>,
}

pub fn resolve_cloudflared() -> AppResult<PathBuf> {
    cached_cloudflared_path()
        .filter(|path| path.is_file())
        .or_else(|| {
            platform()
                .cloudflared_candidates()
                .into_iter()
                .find(|path| path.is_file())
        })
        .ok_or_else(|| {
            AppError::Message(
                "未找到 cloudflared。CLI 可执行 `anchor software install cloudflared` 自动安装；也可自行安装 Cloudflare Tunnel CLI。\n\
                 Windows 可执行：winget install Cloudflare.cloudflared"
                    .into(),
            )
        })
}

pub async fn ensure_cloudflared() -> AppResult<PathBuf> {
    if let Ok(path) = resolve_cloudflared() {
        return Ok(path);
    }
    download_cloudflared_to_cache().await
}

/// Path where the app caches a self-managed cloudflared binary.
pub(crate) fn cached_cloudflared_path() -> Option<PathBuf> {
    platform()
        .app_config_dir()
        .ok()
        .map(|dir| dir.join("bin").join(cloudflared_binary_name()))
}

pub(crate) fn cloudflared_binary_name() -> &'static str {
    #[cfg(windows)]
    {
        "cloudflared.exe"
    }
    #[cfg(not(windows))]
    {
        "cloudflared"
    }
}

/// GitHub release asset name for the current platform.
fn cloudflared_release_asset() -> AppResult<&'static str> {
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    {
        Ok("cloudflared-windows-amd64.exe")
    }
    #[cfg(all(target_os = "windows", target_arch = "aarch64"))]
    {
        Ok("cloudflared-windows-arm64.exe")
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        Ok("cloudflared-linux-amd64")
    }
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    {
        Ok("cloudflared-linux-arm64")
    }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        Ok("cloudflared-darwin-amd64.tgz")
    }
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        Ok("cloudflared-darwin-arm64.tgz")
    }
    #[cfg(not(any(
        all(target_os = "windows", target_arch = "x86_64"),
        all(target_os = "windows", target_arch = "aarch64"),
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "x86_64"),
        all(target_os = "macos", target_arch = "aarch64"),
    )))]
    {
        Err(AppError::Message(
            "当前平台暂不支持自动下载 cloudflared。".into(),
        ))
    }
}

/// Latest cloudflared release. Pinned for reproducibility; bump as needed.
pub(crate) const VERSION: &str = "2025.6.1";

/// Download cloudflared into the app cache `bin/` directory, honoring the
/// configured mirror + proxy. Windows/Linux assets are raw binaries; macOS
/// assets are `.tgz` archives that need extraction.
pub(crate) async fn download_cloudflared_to_cache() -> AppResult<PathBuf> {
    let settings = crate::settings::AppSettings::load()?;
    let asset = cloudflared_release_asset()?;
    let url =
        format!("https://github.com/cloudflare/cloudflared/releases/download/{VERSION}/{asset}");
    let dest =
        cached_cloudflared_path().ok_or_else(|| AppError::Message("无法解析缓存目录。".into()))?;
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let bytes =
        crate::tunnel::download::download_release_asset(&settings, &url, "cloudflared").await?;

    if asset.ends_with(".tgz") {
        extract_cloudflared_from_tar_gz(&bytes, &dest)?;
    } else {
        std::fs::write(&dest, &bytes)?;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(&dest) {
            let mut perms = meta.permissions();
            perms.set_mode(0o755);
            let _ = std::fs::set_permissions(&dest, perms);
        }
    }

    if dest.is_file() {
        Ok(dest)
    } else {
        Err(AppError::Message("cloudflared 自动安装失败。".into()))
    }
}

#[cfg(target_os = "macos")]
fn extract_cloudflared_from_tar_gz(bytes: &[u8], dest: &Path) -> AppResult<()> {
    let decoder = flate2::read::GzDecoder::new(bytes);
    let mut archive = tar::Archive::new(decoder);
    for entry in archive
        .entries()
        .map_err(|err| AppError::Message(format!("解压 cloudflared 安装包失败: {err}")))?
    {
        let mut entry = entry
            .map_err(|err| AppError::Message(format!("读取 cloudflared 安装包失败: {err}")))?;
        let path = entry
            .path()
            .map_err(|err| AppError::Message(err.to_string()))?
            .to_string_lossy()
            .replace('\\', "/");
        if path.ends_with("cloudflared") {
            let mut out = std::fs::File::create(dest)?;
            std::io::copy(&mut entry, &mut out)?;
            return Ok(());
        }
    }
    Err(AppError::Message(
        "cloudflared 安装包中未找到可执行文件。".into(),
    ))
}

#[cfg(not(target_os = "macos"))]
fn extract_cloudflared_from_tar_gz(_bytes: &[u8], _dest: &Path) -> AppResult<()> {
    Err(AppError::Message(
        "当前平台的 cloudflared 无需解压。".into(),
    ))
}

pub fn extract_trycloudflare_url(line: &str) -> Option<String> {
    const PREFIX: &str = "https://";
    const SUFFIX: &str = ".trycloudflare.com";
    let lower = line.to_ascii_lowercase();
    let mut search_from = 0;

    while let Some(rel) = lower[search_from..].find(PREFIX) {
        let start = search_from + rel;
        let Some(suffix_rel) = lower[start..].find(SUFFIX) else {
            break;
        };
        let end = start + suffix_rel + SUFFIX.len();
        let host = &line[start + PREFIX.len()..end - SUFFIX.len()];
        if host.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') && !host.is_empty() {
            return Some(line[start..end].trim_end_matches('/').to_string());
        }
        search_from = start + PREFIX.len();
    }
    None
}

/// Apply the global proxy to a tunnel child process environment.
pub(crate) fn apply_proxy_env(cmd: &mut Command, proxy: &ProxyConfig) {
    let url = match proxy.mode.as_str() {
        "manual" if !proxy.url.trim().is_empty() => Some(proxy.url.trim().to_string()),
        "system" => std::env::var("HTTPS_PROXY")
            .ok()
            .filter(|s| !s.is_empty())
            .or_else(|| std::env::var("HTTP_PROXY").ok().filter(|s| !s.is_empty()))
            .or_else(|| std::env::var("ALL_PROXY").ok().filter(|s| !s.is_empty())),
        _ => None,
    };
    if let Some(url) = url {
        for key in [
            "HTTPS_PROXY",
            "HTTP_PROXY",
            "https_proxy",
            "http_proxy",
            "ALL_PROXY",
            "all_proxy",
        ] {
            cmd.env(key, &url);
        }
        // Some cloudflared builds consult this dedicated variable.
        cmd.env("TUNNEL_HTTP_PROXY", &url);
    }
}

/// Spawn `cloudflared tunnel --url http://127.0.0.1:{port}` (quick) or named `tunnel run --token`.
pub async fn spawn_cloudflare_tunnel(
    port: u16,
    cwd: &Path,
    log_path: &Path,
    cloudflare_mode: &str,
    cloudflare_token: &str,
    named_public_url: &str,
    use_proxy: bool,
) -> AppResult<CloudflareTunnelHandle> {
    let cloudflared = ensure_cloudflared().await?;
    let quick = cloudflare_mode != "named";

    if !quick {
        if cloudflare_token.trim().is_empty() {
            return Err(AppError::Message(
                "Cloudflare 命名隧道模式需要填写 Tunnel Token。".into(),
            ));
        }
        if named_public_url.trim().is_empty() {
            return Err(AppError::Message(
                "Cloudflare 命名隧道模式需要填写固定公网地址。".into(),
            ));
        }
    }

    let mut cmd = Command::new(&cloudflared);
    cmd.current_dir(cwd);
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    crate::platform::configure_supervised_tokio_process(&mut cmd);

    let settings = crate::settings::AppSettings::load()?;
    if use_proxy {
        apply_proxy_env(&mut cmd, &settings.proxy);
    }

    if quick {
        cmd.args(["tunnel", "--url", &format!("http://127.0.0.1:{port}")]);
    } else {
        cmd.args(["tunnel", "run", "--token", cloudflare_token.trim()]);
    }

    let mut child = cmd
        .spawn()
        .map_err(|err| AppError::Message(format!("启动 cloudflared 失败: {err}")))?;
    let pid = child.id();

    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let (ready_tx, ready_rx) = oneshot::channel();
    let log_path = log_path.to_path_buf();
    let named_url = named_public_url.trim_end_matches('/').to_string();
    let log_path_for_error = log_path.clone();

    if let Some(stdout) = child.stdout.take() {
        let stderr = child.stderr.take();
        tokio::spawn(async move {
            stream_cloudflare_output(stdout, stderr, &log_path, quick, named_url, ready_tx).await;
        });
    } else {
        let _ = ready_tx.send(QuickTunnelReady {
            public_url: if quick {
                None
            } else {
                Some(named_public_url.trim_end_matches('/').to_string())
            },
        });
    }

    let ready = time::timeout(READY_TIMEOUT, ready_rx)
        .await
        .map_err(|_| {
            AppError::Message(format!(
                "cloudflared 已启动，但在 {} 秒内没有返回 trycloudflare.com 公网地址。\n\
                 请检查：1) MCP 服务是否已在本机端口 {port} 运行；2) 设置 → 通用 → 网络代理 是否配置为手动代理（如 http://127.0.0.1:7890）；\
                 3) 查看日志 {log_hint}",
                READY_TIMEOUT.as_secs(),
                log_hint = log_path_for_error.display()
            ))
        })?
        .map_err(|_| AppError::Message("cloudflared 输出流意外结束。".into()))?;

    let public_url = if quick {
        ready.public_url.ok_or_else(|| {
            AppError::Message(format!(
                "cloudflared 已启动，但没有解析到 trycloudflare.com 地址。请查看日志：{}",
                log_path_for_error.display()
            ))
        })?
    } else {
        named_public_url.trim_end_matches('/').to_string()
    };

    Ok(CloudflareTunnelHandle {
        child,
        public_url,
        pid,
    })
}

struct QuickTunnelReady {
    public_url: Option<String>,
}

async fn stream_cloudflare_output<R, E>(
    stdout: R,
    stderr: Option<E>,
    log_path: &Path,
    quick: bool,
    named_url: String,
    ready_tx: oneshot::Sender<QuickTunnelReady>,
) where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
    E: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    let mut ready_tx = Some(ready_tx);
    let mut public_url: Option<String> = None;

    // Logging failure must not stop draining cloudflared pipes. Otherwise a
    // full child pipe can stall the tunnel process itself. Keep the writer
    // optional and continue readiness detection even when disk logging fails.
    let mut log = BoundedAsyncLogWriter::open(log_path).await.ok();

    let send_ready = |tx: &mut Option<oneshot::Sender<QuickTunnelReady>>, url: Option<String>| {
        if let Some(sender) = tx.take() {
            let _ = sender.send(QuickTunnelReady { public_url: url });
        }
    };

    let handle_line =
        |line: &str,
         public_url: &mut Option<String>,
         ready_tx: &mut Option<oneshot::Sender<QuickTunnelReady>>| {
            if quick {
                if public_url.is_none() {
                    if let Some(url) = extract_trycloudflare_url(line) {
                        *public_url = Some(url.clone());
                        send_ready(ready_tx, Some(url));
                    }
                }
            } else {
                let lowered = line.to_ascii_lowercase();
                if lowered.contains("registered tunnel connection")
                    || lowered.contains("starting metrics server")
                {
                    send_ready(ready_tx, Some(named_url.clone()));
                }
            }
        };

    // cloudflared logs primarily to stderr. A fixed-capacity channel applies
    // backpressure to both pipe readers when disk is slow, preventing an
    // unbounded in-memory log backlog.
    let (line_tx, mut line_rx) = mpsc::channel::<String>(PROCESS_LOG_CHANNEL_CAPACITY);

    let stdout_line_tx = line_tx.clone();
    tokio::spawn(async move {
        forward_bounded_lines(stdout, stdout_line_tx).await;
    });

    if let Some(stderr) = stderr {
        let stderr_line_tx = line_tx.clone();
        tokio::spawn(async move {
            forward_bounded_lines(stderr, stderr_line_tx).await;
        });
    }
    drop(line_tx);

    while let Some(line) = line_rx.recv().await {
        // Readiness is more important than log persistence latency; detect it
        // before touching disk so a slow filesystem cannot delay the public URL.
        handle_line(&line, &mut public_url, &mut ready_tx);
        if let Some(writer) = log.as_mut() {
            if writer.write_line(&line).await.is_err() {
                log = None;
            }
        }
    }

    if let Some(writer) = log.as_mut() {
        let _ = writer.flush().await;
    }

    send_ready(&mut ready_tx, public_url);
}

pub async fn stop_child(mut child: Child, pid: Option<u32>) -> AppResult<()> {
    if let Some(pid) = pid {
        let _ = platform().terminate_process_tree(pid);
    }

    let _ = child.kill().await;
    let _ = time::timeout(Duration::from_secs(3), child.wait()).await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{extract_trycloudflare_url, stream_cloudflare_output};

    use tokio::io::AsyncWriteExt;

    #[test]
    fn extracts_trycloudflare_url_from_log_line() {
        let line = "INF | https://abc-def.trycloudflare.com is your tunnel URL";
        assert_eq!(
            extract_trycloudflare_url(line).as_deref(),
            Some("https://abc-def.trycloudflare.com")
        );
    }

    #[test]
    fn ignores_invalid_hosts() {
        let line = "https://bad_host.trycloudflare.com";
        assert!(extract_trycloudflare_url(line).is_none());
    }

    #[tokio::test]
    async fn stdout_only_stream_completes_and_publishes_quick_tunnel_url() {
        let temp = tempfile::tempdir().expect("log root");
        let log_path = temp.path().join("cloudflared.log");
        let (mut producer, stdout) = tokio::io::duplex(4096);
        producer
            .write_all(b"INF https://bounded-test.trycloudflare.com ready\n")
            .await
            .expect("write cloudflared output");
        producer.shutdown().await.expect("close cloudflared output");

        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            stream_cloudflare_output(
                stdout,
                Option::<tokio::io::DuplexStream>::None,
                &log_path,
                true,
                String::new(),
                ready_tx,
            ),
        )
        .await
        .expect("stdout-only stream must terminate");

        let ready = ready_rx.await.expect("quick tunnel readiness");
        assert_eq!(
            ready.public_url.as_deref(),
            Some("https://bounded-test.trycloudflare.com")
        );
        let log = tokio::fs::read_to_string(&log_path)
            .await
            .expect("cloudflared log");
        assert!(log.contains("bounded-test.trycloudflare.com"));
    }
}
