mod access;
mod cloudflare;
mod download;
mod frp;
mod software;
mod supervisor;

pub use access::{
    drop_workspace, ensure_for_runtime, is_quick_tunnel_url_change_error, maybe_start_for_runtime,
    reconcile_mcp_gateway, stop_for_runtime, supervisor,
};

pub use cloudflare::resolve_cloudflared;
pub use frp::resolve_frpc;
pub(crate) use frp::validate_workspace_frp_config;
pub(crate) use software::target_version as software_target_version;
pub(crate) use software::{
    install_software, is_supported_kind as is_supported_software_kind, list_software,
    resolve_managed_software_program, resolve_ripgrep, supported_kinds as supported_software_kinds,
    uninstall_software, SoftwareStatus,
};
pub use supervisor::{
    append_profile_log, log_dir_for_profile, TunnelServiceKind, TunnelStatus, TunnelSupervisor,
};
