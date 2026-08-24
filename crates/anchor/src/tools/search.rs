use serde_json::Value;

use crate::tools::file;
use crate::tools::workspace::{Workspace, WorkspaceError};
use crate::tools::CancellationToken;

/// Unified code-search entry point.
///
/// The public tool contract is intentionally decoupled from its concrete
/// engines. Text search currently delegates to the existing Anchor/ripgrep
/// implementation; semantic graph routing is layered here rather than exposed
/// as separate MCP tools.
pub fn search(
    ws: &Workspace,
    args: &Value,
    cancellation: &CancellationToken,
) -> Result<Value, WorkspaceError> {
    file::grep(ws, args, cancellation)
}
