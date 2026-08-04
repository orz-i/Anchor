use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

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
    session_task_ids: Mutex<HashMap<String, String>>,
    unbound_task_sessions: Mutex<HashSet<String>>,
    command_output_cursors: Mutex<HashMap<String, (usize, usize)>>,
    workspace_mutation_lock: Mutex<()>,
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
            session_task_ids: Mutex::new(HashMap::new()),
            unbound_task_sessions: Mutex::new(HashSet::new()),
            command_output_cursors: Mutex::new(HashMap::new()),
            workspace_mutation_lock: Mutex::new(()),
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
        self.session_task_ids
            .lock()
            .expect("session task lock")
            .remove(session_id);
        self.unbound_task_sessions
            .lock()
            .expect("unbound task session lock")
            .remove(session_id);
    }

    pub fn bind_task_for_session(
        &self,
        session_id: Option<&str>,
        task_id: &str,
    ) -> Result<crate::harness::model::TaskSession, String> {
        let task = self
            .harness
            .task(task_id)
            .map_err(|error| error.to_string())?;
        if !task.status.is_writable() {
            return Err(format!("Task {task_id} is not writable"));
        }
        if let Some(session_id) = session_id {
            self.session_task_ids
                .lock()
                .expect("session task lock")
                .insert(session_id.to_string(), task_id.to_string());
            self.unbound_task_sessions
                .lock()
                .expect("unbound task session lock")
                .remove(session_id);
        }
        Ok(task)
    }

    pub fn task_for_session(
        &self,
        session_id: Option<&str>,
    ) -> Option<crate::harness::model::TaskSession> {
        if let Some(session_id) = session_id {
            if let Some(task) = self.bound_task_for_session(Some(session_id)) {
                return Some(task);
            }
            if self
                .unbound_task_sessions
                .lock()
                .expect("unbound task session lock")
                .contains(session_id)
            {
                return None;
            }
        }
        let tasks = self.harness.list_tasks().ok()?;
        let active_tasks = tasks
            .iter()
            .filter(|task| {
                matches!(
                    task.status,
                    crate::harness::model::TaskStatus::Active
                        | crate::harness::model::TaskStatus::Verifying
                )
            })
            .cloned()
            .collect::<Vec<_>>();
        let task = if active_tasks.len() == 1 {
            active_tasks.into_iter().next()
        } else if active_tasks.is_empty() {
            let writable_tasks = tasks
                .into_iter()
                .filter(|task| task.status.is_writable())
                .collect::<Vec<_>>();
            (writable_tasks.len() == 1)
                .then(|| writable_tasks.into_iter().next())
                .flatten()
        } else {
            None
        };
        if let (Some(session_id), Some(task)) = (session_id, task.as_ref()) {
            self.session_task_ids
                .lock()
                .expect("session task lock")
                .insert(session_id.to_string(), task.id.clone());
        }
        task
    }

    pub fn bound_task_for_session(
        &self,
        session_id: Option<&str>,
    ) -> Option<crate::harness::model::TaskSession> {
        let session_id = session_id?;
        let task_id = self
            .session_task_ids
            .lock()
            .expect("session task lock")
            .get(session_id)
            .cloned()?;
        if let Ok(task) = self.harness.task(&task_id) {
            if task.status.is_writable() {
                return Some(task);
            }
        }
        self.session_task_ids
            .lock()
            .expect("session task lock")
            .remove(session_id);
        self.unbound_task_sessions
            .lock()
            .expect("unbound task session lock")
            .insert(session_id.to_string());
        None
    }

    pub fn workspace_mutation_guard(&self) -> MutexGuard<'_, ()> {
        self.workspace_mutation_lock
            .lock()
            .expect("workspace mutation lock")
    }

    pub fn apply_command_output_cursor(
        &self,
        mcp_session_id: Option<&str>,
        args: &mut serde_json::Value,
    ) {
        let Some(mcp_session_id) = mcp_session_id else {
            return;
        };
        let Some(command_session_id) = args
            .get("session_id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
        else {
            return;
        };
        let Some(object) = args.as_object_mut() else {
            return;
        };
        if object.contains_key("stdout_offset") || object.contains_key("stderr_offset") {
            return;
        }
        let key = format!("{mcp_session_id}\0{command_session_id}");
        let (stdout_offset, stderr_offset) = self
            .command_output_cursors
            .lock()
            .expect("command output cursors lock")
            .get(&key)
            .copied()
            .unwrap_or((0, 0));
        object.insert("stdout_offset".into(), serde_json::json!(stdout_offset));
        object.insert("stderr_offset".into(), serde_json::json!(stderr_offset));
    }

    pub fn update_command_output_cursor(
        &self,
        mcp_session_id: Option<&str>,
        args: &serde_json::Value,
        output: &serde_json::Value,
    ) {
        let (Some(mcp_session_id), Some(command_session_id)) = (
            mcp_session_id,
            args.get("session_id").and_then(serde_json::Value::as_str),
        ) else {
            return;
        };
        let next_offset = |stream: &str| {
            output
                .get(stream)
                .and_then(|value| value.get("next_offset"))
                .and_then(serde_json::Value::as_u64)
                .and_then(|value| usize::try_from(value).ok())
        };
        let (Some(stdout_offset), Some(stderr_offset)) =
            (next_offset("stdout"), next_offset("stderr"))
        else {
            return;
        };
        let key = format!("{mcp_session_id}\0{command_session_id}");
        self.command_output_cursors
            .lock()
            .expect("command output cursors lock")
            .insert(key, (stdout_offset, stderr_offset));
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
