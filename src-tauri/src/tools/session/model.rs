use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionIndex {
    pub version: u32,
    pub sessions: BTreeMap<String, IndexEntry>,
    #[serde(default)]
    pub host_sessions: BTreeMap<String, String>,
}

impl Default for SessionIndex {
    fn default() -> Self {
        Self {
            version: 2,
            sessions: BTreeMap::new(),
            host_sessions: BTreeMap::new(),
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
}

#[derive(Debug, Clone)]
pub struct SessionDocument {
    pub session_id: String,
    pub path: String,
    pub title: String,
    pub size_bytes: u64,
    pub host_session_key: Option<String>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    pub status: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ScanReport {
    pub documents: Vec<SessionDocument>,
    pub duplicate_session_ids: Vec<String>,
    pub duplicate_host_session_keys: Vec<String>,
    pub invalid_files: Vec<String>,
    pub empty_files: Vec<String>,
}

impl ScanReport {
    pub fn sequence_valid(&self) -> bool {
        self.duplicate_session_ids.is_empty()
            && self.duplicate_host_session_keys.is_empty()
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
