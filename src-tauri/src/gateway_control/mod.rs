mod events;
mod ipc;
mod logs;
pub mod protocol;

use crate::data::DataStore;
use crate::error::AppResult;
use crate::settings::McpGatewayConfig;

pub(crate) use events::{publish_gateway_event, reset_gateway_event_stream};
pub use ipc::{
    control_channel, endpoint, logs_via_daemon_or_local, ping, request_apply_config,
    request_events, request_exit, request_logs, request_reload, request_status,
    status_via_daemon_or_local, GatewayControlClientError, GatewayControlCommand,
    GatewayControlReceiver, GatewayControlSender, GatewayControlServer, GatewayLocalEndpoint,
};
pub(crate) use ipc::{finish_reload_operation, mark_operation_running};
pub use logs::read_gateway_log;
pub use protocol::{
    GatewayAsyncOperation, GatewayAsyncState, GatewayControlStatus, GatewayEvent,
    GatewayEventBatch, GatewayEventCursor, GatewayEventKind, GatewayLogChunk, GatewayLogCursor,
    GatewayOperation,
};

pub fn persist_config(config: &McpGatewayConfig) -> AppResult<()> {
    DataStore::update_file(|data| {
        data.mcp_gateway = config.clone();
        Ok(())
    })
}
