mod access;
mod cloudflare;
mod download;
mod frp;
#[cfg(any(feature = "desktop", feature = "cli"))]
mod software;
mod supervisor;

#[cfg(feature = "desktop")]
use crate::settings::AppSettings;
#[cfg(feature = "desktop")]
use crate::workspace::WorkspaceProfile;

#[cfg(feature = "desktop")]
pub use access::cleanup_orphan_for_runtime;
pub use access::{
    drop_workspace, ensure_for_runtime, is_quick_tunnel_url_change_error, maybe_start_for_runtime,
    reconcile_mcp_gateway, stop_for_runtime, supervisor,
};

pub use cloudflare::resolve_cloudflared;
#[cfg(feature = "cli")]
pub use frp::resolve_frpc;
#[cfg(feature = "cli")]
pub(crate) use frp::validate_workspace_frp_config;
#[cfg(any(feature = "desktop", feature = "cli"))]
pub(crate) use software::target_version as software_target_version;
#[cfg(all(feature = "cli", not(feature = "desktop")))]
pub(crate) use software::{install_software, list_software, uninstall_software, SoftwareStatus};
#[cfg(feature = "desktop")]
pub use software::{install_software, list_software, uninstall_software, SoftwareStatus};
pub use supervisor::{
    append_profile_log, log_dir_for_profile, TunnelServiceKind, TunnelStatus, TunnelSupervisor,
};

#[cfg(feature = "desktop")]
pub fn frp_snippet(
    profile: &WorkspaceProfile,
    kind: TunnelServiceKind,
) -> crate::error::AppResult<String> {
    let settings = AppSettings::load()?;
    frp::frp_snippet(profile, kind, &settings)
}
