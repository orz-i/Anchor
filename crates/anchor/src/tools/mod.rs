mod cancellation;
pub mod catalog;
pub mod command_cost;
pub mod command_session;
pub mod context;
pub mod dispatch;
pub mod environment;
pub mod exec;
pub mod file;
pub mod git;
mod image_tool;
pub mod patch;
pub mod policy;
pub mod recovery;
pub mod registry;
pub mod resource_budget;
pub(crate) mod schema;
pub mod search;
pub mod session;
pub mod workspace;

pub use cancellation::CancellationToken;
pub use catalog::{build_effective_catalog, build_effective_catalog_from_parts, EffectiveCatalog};
pub use context::{SharedToolContext, ToolContext};
/// 唯一工具执行入口；MCP 与 Actions 必须调用此函数，不得分叉实现。
pub use dispatch::{call_tool, call_tool_for_session, call_tool_with_cancellation};
pub use policy::{validate_actions_exposure, PolicySettings};
pub use registry::{
    exposed_tool_names, is_allowed_tool, list_tools, list_tools_for_profile, MUTATING_TOOLS,
};
pub use workspace::{wrap_mcp_tool_result, wrap_tool_result, Workspace};
