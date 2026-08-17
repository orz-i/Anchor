mod listener_handoff;
mod maintenance;
mod port;
mod public_url;
mod supervisor;

#[cfg(feature = "desktop")]
pub use maintenance::spawn_desktop_maintenance;

#[cfg(unix)]
pub(crate) use listener_handoff::InheritableListener;
pub(crate) use listener_handoff::{bind_loopback_listener, HandoffListener};
pub use port::{await_listener_shutdown, is_own_process};
#[cfg(feature = "desktop")]
pub use port::{port_busy_message, try_reclaim_previous_macos_app_port, wait_for_port_free};
pub(crate) use public_url::{
    current_public_url, read_public_url, register_public_url, update_public_url, SharedPublicUrl,
};
pub use supervisor::{RuntimeSupervisor, ServiceKind};
