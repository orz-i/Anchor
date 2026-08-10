mod model;

#[cfg(feature = "desktop")]
pub use model::FrpProfileInput;
pub use model::{AppSettings, DownloadConfig, FrpProfile, McpGatewayConfig, ProxyConfig};
