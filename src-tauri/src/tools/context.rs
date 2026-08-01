use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::harness::Harness;
use crate::tools::catalog::EffectiveCatalog;
use crate::tools::command_cost::CommandCostGuard;
use crate::tools::policy::PolicySettings;
use crate::tools::session::SessionStore;
use crate::tools::workspace::{relative_display, Workspace};
use crate::workspace::AuthConfig;

pub struct ToolContext {
    pub workspace: Workspace,
    pub auth: AuthConfig,
    pub policy: PolicySettings,
    pub tool_profile: String,
    pub permission_mode: String,
    pub harness: Harness,
    pub mcp_proxies: crate::mcp::proxy::McpProxyRegistry,
    pub skills: crate::skills::SkillCatalog,
    default_cwd: Mutex<PathBuf>,
    session_default_cwds: Mutex<HashMap<String, PathBuf>>,
    pub sessions: SessionStore,
    pub command_cost: CommandCostGuard,
    published_catalog: Mutex<Option<EffectiveCatalog>>,
}

pub type SharedToolContext = Arc<ToolContext>;

impl ToolContext {
    pub fn new(workspace_path: PathBuf) -> Result<Self, String> {
        let workspace = Workspace::new(workspace_path).map_err(|e| e.message())?;
        let auth = AuthConfig {
            auth_type: "noauth".into(),
            ..AuthConfig::default()
        };
        Ok(Self::from_workspace(
            workspace,
            auth,
            PolicySettings::default(),
            "core".into(),
            "trusted".into(),
        ))
    }

    pub fn from_workspace(
        workspace: Workspace,
        auth: AuthConfig,
        policy: PolicySettings,
        tool_profile: String,
        permission_mode: String,
    ) -> Self {
        let harness_root = Harness::default_root().expect("无法初始化 Harness 数据目录");
        Self::from_workspace_with_harness_root(
            workspace,
            auth,
            policy,
            crate::tools::registry::require_tool_profile(&tool_profile)
                .expect("tool profile must be validated")
                .into(),
            permission_mode,
            harness_root,
        )
    }

    pub fn from_workspace_with_harness_root(
        workspace: Workspace,
        auth: AuthConfig,
        policy: PolicySettings,
        tool_profile: String,
        permission_mode: String,
        harness_root: PathBuf,
    ) -> Self {
        let root = workspace.root().to_path_buf();
        let command_cost = CommandCostGuard::new(&harness_root, &root);
        let context = Self {
            workspace,
            auth,
            policy,
            tool_profile: crate::tools::registry::require_tool_profile(&tool_profile)
                .expect("tool profile must be validated")
                .into(),
            permission_mode,
            harness: Harness::new(root.clone(), harness_root).expect("无法初始化 Harness"),
            mcp_proxies: crate::mcp::proxy::McpProxyRegistry::default(),
            skills: crate::skills::SkillCatalog::new(root.clone()),
            default_cwd: Mutex::new(root),
            session_default_cwds: Mutex::new(HashMap::new()),
            sessions: SessionStore::new(),
            command_cost,
            published_catalog: Mutex::new(None),
        };
        let _ = crate::harness::tools::recover_close_outboxes(&context);
        context
    }

    pub fn for_test(workspace_path: PathBuf, harness_root: PathBuf) -> Result<Self, String> {
        let workspace = Workspace::new(workspace_path).map_err(|e| e.message())?;
        Ok(Self::from_workspace_with_harness_root(
            workspace,
            AuthConfig {
                auth_type: "noauth".into(),
                ..AuthConfig::default()
            },
            PolicySettings::default(),
            "core".into(),
            "trusted".into(),
            harness_root,
        ))
    }

    pub fn workspace_path(&self) -> String {
        self.workspace.root_display()
    }

    pub fn default_cwd_display(&self) -> String {
        self.default_cwd_display_for(None)
    }

    pub fn set_default_cwd(&self, path: PathBuf) {
        self.set_default_cwd_for(None, path);
    }

    pub fn default_cwd_path(&self) -> PathBuf {
        self.default_cwd_path_for(None)
    }

    pub fn default_cwd_display_for(&self, session_id: Option<&str>) -> String {
        let display = relative_display(
            self.workspace.root(),
            &self.default_cwd_path_for(session_id),
        );
        if display.is_empty() {
            ".".to_string()
        } else {
            display
        }
    }

    pub fn default_cwd_path_for(&self, session_id: Option<&str>) -> PathBuf {
        if let Some(session_id) = session_id {
            return self
                .session_default_cwds
                .lock()
                .expect("session cwd lock")
                .get(session_id)
                .cloned()
                .unwrap_or_else(|| self.workspace.root().to_path_buf());
        }
        self.default_cwd.lock().expect("cwd lock").clone()
    }

    pub fn set_default_cwd_for(&self, session_id: Option<&str>, path: PathBuf) {
        if let Some(session_id) = session_id {
            self.session_default_cwds
                .lock()
                .expect("session cwd lock")
                .insert(session_id.to_string(), path);
        } else {
            *self.default_cwd.lock().expect("cwd lock") = path;
        }
    }

    pub fn clear_session_state(&self, session_id: &str) {
        self.session_default_cwds
            .lock()
            .expect("session cwd lock")
            .remove(session_id);
    }

    pub fn publish_catalog(&self, current: EffectiveCatalog) -> (EffectiveCatalog, bool) {
        let mut published = self
            .published_catalog
            .lock()
            .expect("published catalog lock");
        if let Some(snapshot) = published.as_ref() {
            let changed = snapshot.digest != current.digest;
            return (snapshot.clone(), changed);
        }
        *published = Some(current.clone());
        (current, false)
    }

    pub fn published_catalog(&self) -> Option<EffectiveCatalog> {
        self.published_catalog
            .lock()
            .expect("published catalog lock")
            .clone()
    }

    pub fn is_published_tool(&self, name: &str) -> Option<bool> {
        self.published_catalog().map(|catalog| {
            catalog.tools.iter().any(|tool| {
                tool.get("name")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|published| published == name)
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::catalog::build_effective_catalog_from_parts;

    #[test]
    fn published_catalog_remains_stable_and_requires_reconnect_on_drift() {
        let workspace = tempfile::tempdir().expect("workspace");
        let harness = tempfile::tempdir().expect("harness");
        let ctx =
            ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                .expect("context");
        let core = build_effective_catalog_from_parts("core", true, Vec::new()).expect("core");
        let advanced =
            build_effective_catalog_from_parts("advanced", true, Vec::new()).expect("advanced");

        let (published, changed) = ctx.publish_catalog(core.clone());
        assert!(!changed);
        assert_eq!(published.digest, core.digest);

        let (still_published, changed) = ctx.publish_catalog(advanced.clone());
        assert!(changed);
        assert_eq!(still_published.digest, core.digest);
        assert_ne!(still_published.digest, advanced.digest);
        assert_eq!(ctx.is_published_tool("stage_commit_status"), Some(false));
        assert_eq!(ctx.is_published_tool("read_file"), Some(true));
    }
}
