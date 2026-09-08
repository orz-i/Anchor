use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub const SESSION_INDEX_VERSION: u32 = 3;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionIndex {
    pub version: u32,
    pub sessions: BTreeMap<String, IndexEntry>,
    pub host_scopes: BTreeMap<String, String>,
}

impl Default for SessionIndex {
    fn default() -> Self {
        Self {
            version: SESSION_INDEX_VERSION,
            sessions: BTreeMap::new(),
            host_scopes: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexEntry {
    pub path: String,
    pub title: String,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SessionDocument {
    pub session_id: String,
    pub path: String,
    pub title: String,
    pub size_bytes: u64,
    pub host_session_scope: Option<String>,
    pub parent_session_id: Option<String>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    pub status: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ScanReport {
    pub documents: Vec<SessionDocument>,
    pub duplicate_session_ids: Vec<String>,
    pub duplicate_host_session_scopes: Vec<String>,
    pub invalid_files: Vec<String>,
    pub empty_files: Vec<String>,
}

impl ScanReport {
    pub fn sequence_valid(&self) -> bool {
        self.duplicate_session_ids.is_empty()
            && self.duplicate_host_session_scopes.is_empty()
            && self.invalid_files.is_empty()
            && self.empty_files.is_empty()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckpointRecord {
    pub turn_id: String,
    #[serde(default)]
    pub timestamp: String,
    #[serde(default)]
    pub user_intent: String,
    #[serde(default)]
    pub findings: Vec<String>,
    #[serde(default)]
    pub decisions: Vec<String>,
    #[serde(default)]
    pub files_changed: Vec<String>,
    #[serde(default)]
    pub tests: Vec<String>,
    #[serde(default)]
    pub runtime_state: Vec<String>,
    #[serde(default)]
    pub remaining_issues: Vec<String>,
    #[serde(default)]
    pub next_actions: Vec<String>,
    #[serde(default)]
    pub notes: String,
}
