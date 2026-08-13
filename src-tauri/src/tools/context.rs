use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use crate::harness::Harness;
use crate::tools::catalog::EffectiveCatalog;
use crate::tools::command_cost::CommandCostGuard;
use crate::tools::command_session::CommandSessionStore;
use crate::tools::policy::PolicySettings;
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
    pub(crate) ui_widget_domain: Option<String>,
    primary_workspace_root: PathBuf,
    default_cwd: Arc<Mutex<PathBuf>>,
    session_default_cwds: Arc<Mutex<HashMap<String, PathBuf>>>,
    session_task_ids: Arc<Mutex<HashMap<String, String>>>,
    unbound_task_sessions: Arc<Mutex<HashSet<String>>>,
    session_cursor_scopes: Arc<Mutex<HashMap<String, String>>>,
    command_output_cursors: Arc<Mutex<HashMap<String, (usize, usize)>>>,
    workspace_mutation_lock: Arc<Mutex<()>>,
    pub sessions: Arc<CommandSessionStore>,
    pub command_cost: Arc<CommandCostGuard>,
    published_catalog: Arc<Mutex<Option<EffectiveCatalog>>>,
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

    pub fn scoped_for_task(
        &self,
        task: &crate::harness::model::TaskSession,
        session_id: Option<&str>,
    ) -> Result<Option<Self>, String> {
        let Some(worktree) = task.git_worktree.as_ref() else {
            return Ok(None);
        };
        let root = PathBuf::from(&worktree.path);
        let workspace = Workspace::new(root.clone())
            .map_err(|error| error.message())?
            .with_strict_read_boundary(self.workspace.strict_read_boundary());
        let harness = self
            .harness
            .with_workspace_root(root.clone())
            .map_err(|error| error.to_string())?;
        let scoped = Self {
            workspace,
            auth: self.auth.clone(),
            policy: self.policy.clone(),
            tool_profile: self.tool_profile.clone(),
            permission_mode: self.permission_mode.clone(),
            harness,
            mcp_proxies: self.mcp_proxies.clone(),
            skills: crate::skills::SkillCatalog::new(root.clone()),
            ui_widget_domain: self.ui_widget_domain.clone(),
            primary_workspace_root: self.primary_workspace_root.clone(),
            default_cwd: Arc::new(Mutex::new(root.clone())),
            session_default_cwds: self.session_default_cwds.clone(),
            session_task_ids: self.session_task_ids.clone(),
            unbound_task_sessions: self.unbound_task_sessions.clone(),
            session_cursor_scopes: self.session_cursor_scopes.clone(),
            command_output_cursors: self.command_output_cursors.clone(),
            workspace_mutation_lock: self.workspace_mutation_lock.clone(),
            sessions: self.sessions.clone(),
            command_cost: self.command_cost.clone(),
            published_catalog: self.published_catalog.clone(),
        };
        if let Some(session_id) = session_id {
            let current = scoped
                .session_default_cwds
                .lock()
                .expect("session cwd lock")
                .get(session_id)
                .cloned();
            if current.as_ref().is_none_or(|path| !path.starts_with(&root)) {
                scoped.set_default_cwd_for(Some(session_id), root);
            }
        }
        Ok(Some(scoped))
    }

    pub fn is_primary_workspace(&self) -> bool {
        self.workspace.root() == self.primary_workspace_root
    }

    pub(crate) fn primary_workspace_root(&self) -> &Path {
        &self.primary_workspace_root
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

    pub(crate) fn with_ui_widget_domain(mut self, domain: Option<String>) -> Self {
        self.ui_widget_domain = domain;
        self
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
            ui_widget_domain: None,
            primary_workspace_root: root.clone(),
            default_cwd: Arc::new(Mutex::new(root)),
            session_default_cwds: Arc::new(Mutex::new(HashMap::new())),
            session_task_ids: Arc::new(Mutex::new(HashMap::new())),
            unbound_task_sessions: Arc::new(Mutex::new(HashSet::new())),
            session_cursor_scopes: Arc::new(Mutex::new(HashMap::new())),
            command_output_cursors: Arc::new(Mutex::new(HashMap::new())),
            workspace_mutation_lock: Arc::new(Mutex::new(())),
            sessions: Arc::new(CommandSessionStore::new()),
            command_cost: Arc::new(command_cost),
            published_catalog: Arc::new(Mutex::new(None)),
        };
        // Durable close-outbox recovery is already performed before normal Harness
        // tool calls. Keep ToolContext construction side-effect free: MCP listener
        // startup must bind its port before potentially expensive historical task
        // recovery, otherwise a valid daemon can look alive while never becoming
        // reachable on its configured port.
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
        self.session_cursor_scopes
            .lock()
            .expect("session cursor scope lock")
            .remove(session_id);
    }

    pub fn bind_cursor_scope_for_session(&self, session_id: &str, scope: Option<&str>) {
        let mut scopes = self
            .session_cursor_scopes
            .lock()
            .expect("session cursor scope lock");
        match scope.map(str::trim).filter(|scope| !scope.is_empty()) {
            Some(scope) => {
                scopes.insert(session_id.to_string(), scope.to_string());
            }
            None => {
                scopes.remove(session_id);
            }
        }
    }

    pub fn command_owner_scope_for_session(&self, session_id: Option<&str>) -> Option<String> {
        let session_id = session_id?;
        self.session_cursor_scopes
            .lock()
            .expect("session cursor scope lock")
            .get(session_id)
            .cloned()
            .or_else(|| Some(format!("transport:{session_id}")))
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
            let execution_root = task
                .git_worktree
                .as_ref()
                .map(|worktree| PathBuf::from(&worktree.path))
                .unwrap_or_else(|| self.primary_workspace_root.clone());
            self.set_default_cwd_for(Some(session_id), execution_root);
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
        let task = self
            .harness
            .current_task()
            .ok()
            .flatten()
            .filter(|task| {
                matches!(
                    task.status,
                    crate::harness::model::TaskStatus::Active
                        | crate::harness::model::TaskStatus::Verifying
                )
            })
            .or_else(|| {
                let tasks = self.harness.list_tasks().ok()?;
                let writable_tasks = tasks
                    .into_iter()
                    .filter(|task| task.status.is_writable())
                    .collect::<Vec<_>>();
                (writable_tasks.len() == 1)
                    .then(|| writable_tasks.into_iter().next())
                    .flatten()
            });
        if let (Some(session_id), Some(task)) = (session_id, task.as_ref()) {
            let _ = self.bind_task_for_session(Some(session_id), &task.id);
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
        task_id: Option<&str>,
        args: &mut serde_json::Value,
    ) {
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
        let cursors = self
            .command_output_cursors
            .lock()
            .expect("command output cursors lock");
        let transport_key = mcp_session_id
            .map(|session_id| format!("transport:{session_id}\0{command_session_id}"));
        let task_key = task_id.map(|task_id| format!("task:{task_id}\0{command_session_id}"));
        let principal_key = mcp_session_id.and_then(|session_id| {
            self.session_cursor_scopes
                .lock()
                .expect("session cursor scope lock")
                .get(session_id)
                .map(|scope| format!("principal:{scope}\0{command_session_id}"))
        });
        let (stdout_offset, stderr_offset) = transport_key
            .as_ref()
            .and_then(|key| cursors.get(key))
            .or_else(|| task_key.as_ref().and_then(|key| cursors.get(key)))
            .or_else(|| principal_key.as_ref().and_then(|key| cursors.get(key)))
            .copied()
            .unwrap_or((0, 0));
        object.insert("stdout_offset".into(), serde_json::json!(stdout_offset));
        object.insert("stderr_offset".into(), serde_json::json!(stderr_offset));
    }

    pub fn update_command_output_cursor(
        &self,
        mcp_session_id: Option<&str>,
        task_id: Option<&str>,
        args: &serde_json::Value,
        output: &serde_json::Value,
    ) {
        let Some(command_session_id) = args.get("session_id").and_then(serde_json::Value::as_str)
        else {
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
        let mut cursors = self
            .command_output_cursors
            .lock()
            .expect("command output cursors lock");
        if let Some(mcp_session_id) = mcp_session_id {
            cursors.insert(
                format!("transport:{mcp_session_id}\0{command_session_id}"),
                (stdout_offset, stderr_offset),
            );
        }
        if let Some(task_id) = task_id {
            cursors.insert(
                format!("task:{task_id}\0{command_session_id}"),
                (stdout_offset, stderr_offset),
            );
        }
        if let Some(scope) = mcp_session_id.and_then(|session_id| {
            self.session_cursor_scopes
                .lock()
                .expect("session cursor scope lock")
                .get(session_id)
                .cloned()
        }) {
            cursors.insert(
                format!("principal:{scope}\0{command_session_id}"),
                (stdout_offset, stderr_offset),
            );
        }
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
    use crate::harness::model::{
        HarnessSessionStatus, WorkSessionCloseOutbox, WorkSessionClosePhase, SCHEMA_VERSION,
    };
    use crate::tools::catalog::build_effective_catalog_from_parts;
    use serde_json::json;

    #[test]
    fn tool_context_construction_does_not_recover_close_outboxes() {
        let workspace = tempfile::tempdir().expect("workspace");
        let harness_root = tempfile::tempdir().expect("harness");
        let harness = Harness::new(
            workspace.path().to_path_buf(),
            harness_root.path().to_path_buf(),
        )
        .expect("harness");
        let task_id = "missing-task-for-startup";
        harness
            .save_close_outbox(&WorkSessionCloseOutbox {
                schema_version: SCHEMA_VERSION,
                task_id: task_id.into(),
                session_id: "startup-test-session".into(),
                session_path: "docs/session/ses_startup-test.md".into(),
                session_status: HarnessSessionStatus::Paused,
                finish_args: json!({"task_id": task_id}),
                checkpoint_args: json!({
                    "session_id": "startup-test-session",
                    "expected_path": "docs/session/ses_startup-test.md"
                }),
                phase: WorkSessionClosePhase::Prepared,
                attempts: 0,
                last_error: None,
                created_at: "1".into(),
                updated_at: "1".into(),
            })
            .expect("save outbox");

        let ctx = ToolContext::for_test(
            workspace.path().to_path_buf(),
            harness_root.path().to_path_buf(),
        )
        .expect("context");
        let persisted = ctx
            .harness
            .load_close_outbox(task_id)
            .expect("load outbox")
            .expect("persisted outbox");

        assert_eq!(persisted.phase, WorkSessionClosePhase::Prepared);
        assert_eq!(persisted.attempts, 0);
    }

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

    #[test]
    fn command_output_cursor_falls_back_to_isolated_authenticated_principal_scope() {
        let workspace = tempfile::tempdir().expect("workspace");
        let harness = tempfile::tempdir().expect("harness");
        let ctx =
            ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                .expect("context");
        let command_session_id = "command-1";

        ctx.bind_cursor_scope_for_session("transport-a1", Some("oauth-client:a"));
        let args = serde_json::json!({"session_id": command_session_id});
        ctx.update_command_output_cursor(
            Some("transport-a1"),
            None,
            &args,
            &serde_json::json!({
                "stdout": {"next_offset": 17},
                "stderr": {"next_offset": 3}
            }),
        );
        ctx.clear_session_state("transport-a1");

        ctx.bind_cursor_scope_for_session("transport-a2", Some("oauth-client:a"));
        let mut same_principal = serde_json::json!({"session_id": command_session_id});
        ctx.apply_command_output_cursor(Some("transport-a2"), None, &mut same_principal);
        assert_eq!(same_principal["stdout_offset"], 17);
        assert_eq!(same_principal["stderr_offset"], 3);

        ctx.bind_cursor_scope_for_session("transport-b1", Some("oauth-client:b"));
        let mut other_principal = serde_json::json!({"session_id": command_session_id});
        ctx.apply_command_output_cursor(Some("transport-b1"), None, &mut other_principal);
        assert_eq!(other_principal["stdout_offset"], 0);
        assert_eq!(other_principal["stderr_offset"], 0);

        let mut anonymous = serde_json::json!({"session_id": command_session_id});
        ctx.apply_command_output_cursor(Some("transport-anonymous"), None, &mut anonymous);
        assert_eq!(anonymous["stdout_offset"], 0);
        assert_eq!(anonymous["stderr_offset"], 0);
    }
}
