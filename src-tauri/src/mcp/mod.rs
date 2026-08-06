mod activity;
pub(crate) mod gateway;
mod listener;
pub(crate) mod protocol;
pub(crate) mod proxy;
mod server;

#[cfg(any(unix, test))]
pub(crate) use activity::activity_snapshot;
pub(crate) use activity::{register_activity, McpActivityTracker};
pub use listener::{spawn_listener, ShutdownSender};
