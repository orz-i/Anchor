use std::fs::OpenOptions;
use std::path::{Path, PathBuf};

use fs2::FileExt;
use serde::{Deserialize, Serialize};

use crate::error::{AppError, AppResult};

const NODE_IDENTITY_SCHEMA_VERSION: u16 = 1;
pub const RUNTIME_CAPABILITY_SCHEMA_VERSION: u16 = 1;
const RUNTIME_CONTRACT: &str = "anchor-runtime-capabilities-v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeIdentity {
    pub id: String,
    pub platform: String,
    pub architecture: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeWorkspaceIdentity {
    pub id: String,
    pub name: String,
    pub path: String,
}

impl RuntimeWorkspaceIdentity {
    pub fn new(id: impl Into<String>, name: impl Into<String>, path: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            path: path.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeTransportCapabilities {
    pub mcp: String,
    pub web_admin: String,
    pub local_control: String,
    pub federation: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeFeatureCapabilities {
    pub workspace_first: bool,
    pub dynamic_mcp: bool,
    pub skill_packages: bool,
    pub durable_commands: bool,
    pub gateway: bool,
    pub federation_read_only: bool,
    pub state_authority: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeCapabilitySnapshot {
    pub schema_version: u16,
    pub contract: String,
    pub node: NodeIdentity,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace: Option<RuntimeWorkspaceIdentity>,
    pub transports: RuntimeTransportCapabilities,
    pub features: RuntimeFeatureCapabilities,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StoredNodeIdentity {
    schema_version: u16,
    node_id: String,
}

pub fn capability_snapshot(
    workspace: Option<RuntimeWorkspaceIdentity>,
) -> AppResult<RuntimeCapabilitySnapshot> {
    Ok(capability_snapshot_from_node(node_identity()?, workspace))
}

pub(crate) fn capability_snapshot_from_node(
    node: NodeIdentity,
    workspace: Option<RuntimeWorkspaceIdentity>,
) -> RuntimeCapabilitySnapshot {
    RuntimeCapabilitySnapshot {
        schema_version: RUNTIME_CAPABILITY_SCHEMA_VERSION,
        contract: RUNTIME_CONTRACT.into(),
        node,
        workspace,
        transports: RuntimeTransportCapabilities {
            mcp: "workspace_listener".into(),
            web_admin: "loopback_http".into(),
            local_control: "local_ipc".into(),
            federation: "gateway_authenticated_read_only".into(),
        },
        features: RuntimeFeatureCapabilities {
            workspace_first: true,
            dynamic_mcp: true,
            skill_packages: true,
            durable_commands: true,
            gateway: true,
            federation_read_only: true,
            state_authority: "existing_daemon_control_plane".into(),
        },
    }
}

pub fn node_identity() -> AppResult<NodeIdentity> {
    let config_dir = crate::platform::platform().app_config_dir()?;
    let node_id = load_or_create_node_id(&config_dir)?;
    Ok(NodeIdentity {
        id: node_id,
        platform: std::env::consts::OS.into(),
        architecture: std::env::consts::ARCH.into(),
    })
}

fn load_or_create_node_id(config_dir: &Path) -> AppResult<String> {
    let data_dir = config_dir.join("data");
    std::fs::create_dir_all(&data_dir)?;
    let identity_path = data_dir.join("node.json");
    let lock_path = data_dir.join(".node.lock");
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let lock_file = options.open(lock_path)?;
    lock_file.lock_exclusive()?;

    let result = if identity_path.exists() {
        read_node_id(&identity_path)
    } else {
        let node_id = format!("node_{}", uuid::Uuid::new_v4().simple());
        let record = StoredNodeIdentity {
            schema_version: NODE_IDENTITY_SCHEMA_VERSION,
            node_id: node_id.clone(),
        };
        let mut bytes = serde_json::to_vec_pretty(&record)?;
        bytes.push(b'\n');
        crate::data::atomic_write(&identity_path, &bytes)?;
        Ok(node_id)
    };
    let _ = FileExt::unlock(&lock_file);
    result
}

fn read_node_id(path: &PathBuf) -> AppResult<String> {
    let raw = std::fs::read(path)?;
    let record: StoredNodeIdentity = serde_json::from_slice(&raw).map_err(|error| {
        AppError::Message(format!(
            "无法解析本机 Node identity {}：{error}",
            path.display()
        ))
    })?;
    if record.schema_version != NODE_IDENTITY_SCHEMA_VERSION {
        return Err(AppError::Message(format!(
            "不支持的 Node identity schema version {}",
            record.schema_version
        )));
    }
    if !valid_node_id(&record.node_id) {
        return Err(AppError::Message("本机 Node identity 格式无效".into()));
    }
    Ok(record.node_id)
}

fn valid_node_id(value: &str) -> bool {
    value.strip_prefix("node_").is_some_and(|suffix| {
        suffix.len() == 32 && suffix.bytes().all(|byte| byte.is_ascii_hexdigit())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_identity_is_stable_per_local_config_root() {
        let temp = tempfile::tempdir().expect("node identity root");
        let first = load_or_create_node_id(temp.path()).expect("first node identity");
        let second = load_or_create_node_id(temp.path()).expect("second node identity");
        assert_eq!(first, second);
        assert!(valid_node_id(&first));
    }

    #[test]
    fn independent_local_config_roots_get_distinct_node_identities() {
        let first_root = tempfile::tempdir().expect("first root");
        let second_root = tempfile::tempdir().expect("second root");
        let first = load_or_create_node_id(first_root.path()).expect("first identity");
        let second = load_or_create_node_id(second_root.path()).expect("second identity");
        assert_ne!(first, second);
    }

    #[test]
    fn runtime_snapshot_keeps_workspace_first_and_federation_read_only() {
        let workspace = RuntimeWorkspaceIdentity::new("workspace-id", "workspace", "/workspace");
        let temp = tempfile::tempdir().expect("node identity root");
        let snapshot = capability_snapshot_from_node(
            NodeIdentity {
                id: load_or_create_node_id(temp.path()).expect("node identity"),
                platform: std::env::consts::OS.into(),
                architecture: std::env::consts::ARCH.into(),
            },
            Some(workspace.clone()),
        );
        assert_eq!(snapshot.workspace, Some(workspace));
        assert!(snapshot.features.workspace_first);
        assert!(snapshot.features.federation_read_only);
        assert_eq!(
            snapshot.transports.federation,
            "gateway_authenticated_read_only"
        );
        assert_eq!(
            snapshot.features.state_authority,
            "existing_daemon_control_plane"
        );
    }
}
