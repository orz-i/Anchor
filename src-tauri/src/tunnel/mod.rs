mod access;
mod cloudflare;
mod download;
mod frp;
mod software;
mod supervisor;

use crate::settings::AppSettings;
use crate::workspace::WorkspaceProfile;

pub use access::{
    cleanup_orphan_for_runtime, drop_workspace, ensure_for_runtime,
    is_quick_tunnel_url_change_error, maybe_start_for_runtime, reconcile_mcp_gateway,
    stop_for_runtime, supervisor,
};

#[allow(unused_imports)]
pub use cloudflare::{
    extract_trycloudflare_url, resolve_cloudflared, spawn_cloudflare_tunnel, stop_child,
};
#[cfg(feature = "cli")]
pub use frp::resolve_frpc;
#[allow(unused_imports)]
pub use software::{install_software, list_software, uninstall_software, SoftwareStatus};
#[allow(unused_imports)]
pub use supervisor::{
    append_profile_log, log_dir_for_profile, TunnelServiceKind, TunnelStatus, TunnelSupervisor,
};

pub fn frp_snippet(
    profile: &WorkspaceProfile,
    kind: TunnelServiceKind,
) -> crate::error::AppResult<String> {
    let settings = AppSettings::load()?;
    frp::frp_snippet(profile, kind, &settings)
}
