mod activity;
pub(crate) mod gateway;
mod listener;
pub(crate) mod protocol;
pub(crate) mod proxy;
mod server;
pub(crate) mod ui;

pub(crate) use activity::activity_snapshot;
pub(crate) use activity::{register_activity, McpActivityTracker};
pub(crate) use listener::spawn_listener_with_handoff;
pub use listener::ShutdownSender;
pub(crate) use listener::{McpHandoffReadiness, McpHandoffSnapshot};
