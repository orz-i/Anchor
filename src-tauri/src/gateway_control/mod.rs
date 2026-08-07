mod ipc;
pub mod protocol;

use crate::data::DataStore;
use crate::error::AppResult;
use crate::settings::McpGatewayConfig;

pub use ipc::{
    control_channel, endpoint, ping, request_apply_config, request_exit, request_reload,
    request_status, status_via_daemon_or_local, GatewayControlClientError, GatewayControlCommand,
    GatewayControlReceiver, GatewayControlSender, GatewayControlServer, GatewayLocalEndpoint,
};
pub(crate) use ipc::{finish_reload_operation, mark_operation_running};
pub use protocol::{
    GatewayAsyncOperation, GatewayAsyncState, GatewayControlStatus, GatewayOperation,
};

pub fn persist_config(config: &McpGatewayConfig) -> AppResult<()> {
    DataStore::update_file(|data| {
        data.mcp_gateway = config.clone();
        Ok(())
    })
}
