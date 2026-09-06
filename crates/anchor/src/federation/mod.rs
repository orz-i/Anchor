use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::error::{AppError, AppResult};
use crate::runtime::RuntimeCapabilitySnapshot;
use crate::workspace::WorkspaceProfile;

mod discovery;
mod registry;
mod signing;
mod transport;

pub(crate) use discovery::{
    fetch_discovery_document, local_discovery_document, local_legacy_discovery_document,
    validate_discovery_document, FederationDiscoveryDocument, FederationDiscoveryInspection,
    FederationDiscoveryState, FEDERATION_MAX_DISCOVERY_BYTES,
};

pub(crate) use registry::{
    accept_discovered_rebootstrap, get_peer, inspect_candidate, inspect_registered_peer_discovery,
    list_peers, probe_registered_peer, read_trusted_peer, register_peer, remove_peer,
    require_registered_target, revoke_peer, rotate_peer_credential, trust_peer, update_peer,
    FederationPeerCandidate, FederationPeerRegistration, FederationPeerUpdate, FederationPeerView,
};
pub(crate) use signing::{
    local_bootstrap_bundle, local_rotation_history, local_signing_status,
    rotate_local_signing_identity, verify_bootstrap_bundle, verify_rotation_chain,
    verify_rotation_chain_from, verify_rotation_notice, FederationBootstrapBundle,
    FederationDetachedSignature, FederationLocalSigningStatus, FederationNodeSigningPublic,
    FederationSigningRotationNotice,
};
pub(crate) use transport::{
    canonical_federation_endpoint, canonical_remote_target, clear_peer_credential,
    handle_inbound_transport, peer_credential_status, probe_remote_peer, remote_read_verified,
    set_peer_credential, FederationPeerCredentialStatus, FederationRemoteTarget,
    FederationTransportError, FederationTransportErrorKind, FEDERATION_MAX_REQUEST_BYTES,
    FEDERATION_MAX_RESPONSE_BYTES, FEDERATION_NODE_HEADER, TRANSPORT_CONNECT_TIMEOUT,
    TRANSPORT_REQUEST_TIMEOUT,
};

pub const FEDERATION_SCHEMA_VERSION: u16 = 2;
pub const FEDERATION_CONTRACT: &str = "anchor-federation-v2";

const FEDERATION_TRANSPORT_MODE: &str = "gateway_authenticated_signed_read_only";
const MAX_FEDERATION_WORKSPACE_ROUTES: usize = 256;
const MAX_FEDERATION_DISPLAY_NAME_BYTES: usize = 200;
const READ_ONLY_OPERATIONS: &[FederationReadOperation] = &[
    FederationReadOperation::NodeCapabilities,
    FederationReadOperation::NodeControlStatus,
    FederationReadOperation::WorkspaceCatalog,
    FederationReadOperation::WorkspaceStatus,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FederationAccessMode {
    ReadOnly,
}

pub async fn execute_local_read(
    request: &FederationReadRequest,
) -> AppResult<FederationReadResult> {
    let store = crate::data::DataStore::load()?;
    let profiles = store.list().to_vec();
    drop(store);
    let local = local_peer_descriptor(&profiles)?;
    let plan = resolve_local_read(&local, request)?;

    match plan.operation {
        FederationReadOperation::NodeCapabilities => Ok(FederationReadResult::NodeCapabilities(
            crate::runtime::capability_snapshot(None)?,
        )),
        FederationReadOperation::WorkspaceCatalog => {
            Ok(FederationReadResult::WorkspaceCatalog(local))
        }
        FederationReadOperation::NodeControlStatus => {
            let status = crate::control::control_plane_status(&profiles).await?;
            let mcp_active_count = status
                .workspaces
                .iter()
                .filter(|workspace| workspace.mcp_state == "running")
                .count();
            Ok(FederationReadResult::NodeControlStatus(
                FederationNodeControlStatus {
                    node_id: local.runtime.node.id,
                    gateway_state: status.gateway.state,
                    workspace_count: status.workspaces.len(),
                    mcp_active_count,
                },
            ))
        }
        FederationReadOperation::WorkspaceStatus => {
            let workspace_id = plan.workspace_id.as_deref().ok_or_else(|| {
                AppError::Message(
                    "FEDERATION_WORKSPACE_REQUIRED: workspace_status requires workspaceId".into(),
                )
            })?;
            let profile = profiles
                .iter()
                .find(|profile| profile.id == workspace_id)
                .ok_or_else(|| {
                    AppError::Message(format!(
                        "FEDERATION_WORKSPACE_NOT_FOUND: workspace {workspace_id} is not registered on the local node"
                    ))
                })?;
            let status =
                crate::control::control_plane_status(std::slice::from_ref(profile)).await?;
            let workspace = status.workspaces.into_iter().next().ok_or_else(|| {
                AppError::Message(
                    "FEDERATION_WORKSPACE_STATUS_UNAVAILABLE: local control plane returned no workspace status"
                        .into(),
                )
            })?;
            Ok(FederationReadResult::WorkspaceStatus(
                FederationWorkspaceStatus {
                    node_id: local.runtime.node.id,
                    workspace_id: profile.id.clone(),
                    display_name: bounded_display_name(&profile.name, &profile.id),
                    mcp_state: workspace.mcp_state,
                },
            ))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FederationNodeControlStatus {
    pub node_id: String,
    pub gateway_state: String,
    pub workspace_count: usize,
    pub mcp_active_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FederationWorkspaceStatus {
    pub node_id: String,
    pub workspace_id: String,
    pub display_name: String,
    pub mcp_state: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum FederationReadResult {
    NodeCapabilities(RuntimeCapabilitySnapshot),
    NodeControlStatus(FederationNodeControlStatus),
    WorkspaceCatalog(FederationPeerDescriptor),
    WorkspaceStatus(FederationWorkspaceStatus),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FederationReadOperation {
    NodeCapabilities,
    NodeControlStatus,
    WorkspaceCatalog,
    WorkspaceStatus,
}

impl FederationReadOperation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NodeCapabilities => "node_capabilities",
            Self::NodeControlStatus => "node_control_status",
            Self::WorkspaceCatalog => "workspace_catalog",
            Self::WorkspaceStatus => "workspace_status",
        }
    }
}

pub fn validate_context_ref(reference: &FederationContextRef) -> AppResult<()> {
    match reference.scope {
        FederationContextScope::GlobalShared => {
            if reference.node_id.is_some() || reference.workspace_id.is_some() {
                return Err(AppError::Message(
                    "FEDERATION_CONTEXT_SCOPE_INVALID: global_shared context must not be bound to a node or workspace"
                        .into(),
                ));
            }
        }
        FederationContextScope::NodeLocal => {
            let node_id = reference.node_id.as_deref().ok_or_else(|| {
                AppError::Message(
                    "FEDERATION_CONTEXT_NODE_REQUIRED: node_local context requires nodeId".into(),
                )
            })?;
            if !valid_node_id(node_id) || reference.workspace_id.is_some() {
                return Err(AppError::Message(
                    "FEDERATION_CONTEXT_SCOPE_INVALID: node_local context requires a valid nodeId and no workspaceId"
                        .into(),
                ));
            }
        }
        FederationContextScope::WorkspaceLocal => {
            let node_id = reference.node_id.as_deref().ok_or_else(|| {
                AppError::Message(
                    "FEDERATION_CONTEXT_NODE_REQUIRED: workspace_local context requires nodeId"
                        .into(),
                )
            })?;
            let workspace_id = reference.workspace_id.as_deref().ok_or_else(|| {
                AppError::Message(
                    "FEDERATION_CONTEXT_WORKSPACE_REQUIRED: workspace_local context requires workspaceId"
                        .into(),
                )
            })?;
            if !valid_node_id(node_id) || !valid_workspace_id(workspace_id) {
                return Err(AppError::Message(
                    "FEDERATION_CONTEXT_SCOPE_INVALID: workspace_local context requires valid nodeId and workspaceId"
                        .into(),
                ));
            }
        }
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FederationContextPolicy {
    pub scopes: Vec<FederationContextScope>,
    pub export_workspace_paths: bool,
    pub export_secrets: bool,
    pub export_harness_state: bool,
    pub remote_mutation: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FederationContextScope {
    GlobalShared,
    NodeLocal,
    WorkspaceLocal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FederationContextRef {
    pub scope: FederationContextScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
}

impl Default for FederationContextPolicy {
    fn default() -> Self {
        Self {
            scopes: vec![
                FederationContextScope::GlobalShared,
                FederationContextScope::NodeLocal,
                FederationContextScope::WorkspaceLocal,
            ],
            export_workspace_paths: false,
            export_secrets: false,
            export_harness_state: false,
            remote_mutation: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FederationWorkspaceRoute {
    pub node_id: String,
    pub workspace_id: String,
    pub display_name: String,
    pub access: FederationAccessMode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FederationPeerDescriptor {
    pub schema_version: u16,
    pub contract: String,
    pub runtime: RuntimeCapabilitySnapshot,
    pub access: FederationAccessMode,
    pub operations: Vec<FederationReadOperation>,
    pub workspaces: Vec<FederationWorkspaceRoute>,
    pub context_policy: FederationContextPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FederationHandshakeValidation {
    pub accepted: bool,
    pub code: String,
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peer_node_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FederationReadRequest {
    pub node_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    pub operation: FederationReadOperation,
    pub context: FederationContextRef,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FederationReadSource {
    RuntimeCapabilityProvider,
    ExistingControlPlane,
    WorkspaceRegistry,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FederationReadPlan {
    pub node_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    pub operation: FederationReadOperation,
    pub access: FederationAccessMode,
    pub source: FederationReadSource,
    pub transport: String,
}

pub fn local_peer_descriptor(profiles: &[WorkspaceProfile]) -> AppResult<FederationPeerDescriptor> {
    if profiles.len() > MAX_FEDERATION_WORKSPACE_ROUTES {
        return Err(AppError::Message(format!(
            "FEDERATION_ROUTE_LIMIT_EXCEEDED: local workspace count {} exceeds the federation catalog limit {}",
            profiles.len(), MAX_FEDERATION_WORKSPACE_ROUTES
        )));
    }
    let runtime = crate::runtime::capability_snapshot(None)?;
    let node_id = runtime.node.id.clone();
    let mut workspaces = Vec::with_capacity(profiles.len());
    for profile in profiles {
        if !valid_workspace_id(&profile.id) {
            return Err(AppError::Message(format!(
                "FEDERATION_LOCAL_WORKSPACE_ID_INVALID: registered workspace {} has an invalid stable id",
                profile.name
            )));
        }
        workspaces.push(FederationWorkspaceRoute {
            node_id: node_id.clone(),
            workspace_id: profile.id.clone(),
            display_name: bounded_display_name(&profile.name, &profile.id),
            access: FederationAccessMode::ReadOnly,
        });
    }
    workspaces.sort_by(|left, right| left.workspace_id.cmp(&right.workspace_id));

    Ok(FederationPeerDescriptor {
        schema_version: FEDERATION_SCHEMA_VERSION,
        contract: FEDERATION_CONTRACT.into(),
        runtime,
        access: FederationAccessMode::ReadOnly,
        operations: READ_ONLY_OPERATIONS.to_vec(),
        workspaces,
        context_policy: FederationContextPolicy::default(),
    })
}

fn validate_request_context(request: &FederationReadRequest) -> AppResult<()> {
    validate_context_ref(&request.context)?;
    match request.operation {
        FederationReadOperation::NodeCapabilities
        | FederationReadOperation::NodeControlStatus
        | FederationReadOperation::WorkspaceCatalog => {
            if request.context.scope != FederationContextScope::NodeLocal
                || request.context.node_id.as_deref() != Some(request.node_id.as_str())
                || request.context.workspace_id.is_some()
            {
                return Err(AppError::Message(format!(
                    "FEDERATION_CONTEXT_ROUTE_MISMATCH: operation {} requires node_local context bound to target node {}",
                    request.operation.as_str(), request.node_id
                )));
            }
        }
        FederationReadOperation::WorkspaceStatus => {
            let workspace_id = request.workspace_id.as_deref().ok_or_else(|| {
                AppError::Message(
                    "FEDERATION_WORKSPACE_REQUIRED: workspace_status requires workspaceId".into(),
                )
            })?;
            if request.context.scope != FederationContextScope::WorkspaceLocal
                || request.context.node_id.as_deref() != Some(request.node_id.as_str())
                || request.context.workspace_id.as_deref() != Some(workspace_id)
            {
                return Err(AppError::Message(format!(
                    "FEDERATION_CONTEXT_ROUTE_MISMATCH: workspace_status requires workspace_local context bound to node {} workspace {}",
                    request.node_id, workspace_id
                )));
            }
        }
    }
    Ok(())
}

pub fn validate_peer_descriptor(
    local_runtime: &RuntimeCapabilitySnapshot,
    peer: &FederationPeerDescriptor,
) -> FederationHandshakeValidation {
    let peer_node_id = Some(peer.runtime.node.id.clone());
    let reject = |code: &str, reason: String| FederationHandshakeValidation {
        accepted: false,
        code: code.into(),
        reason,
        peer_node_id: peer_node_id.clone(),
    };

    if peer.schema_version != FEDERATION_SCHEMA_VERSION || peer.contract != FEDERATION_CONTRACT {
        return reject(
            "FEDERATION_CONTRACT_MISMATCH",
            format!(
                "expected {FEDERATION_CONTRACT} schema {FEDERATION_SCHEMA_VERSION}, received {} schema {}",
                peer.contract, peer.schema_version
            ),
        );
    }
    if !peer.runtime.features.workspace_first
        || !peer.runtime.features.federation_read_only
        || peer.runtime.features.state_authority != "existing_daemon_control_plane"
    {
        return reject(
            "FEDERATION_RUNTIME_AUTHORITY_UNSUPPORTED",
            "peer must preserve Workspace-first routing, read-only federation, and the existing daemon/control-plane state authority"
                .into(),
        );
    }
    if peer.runtime.contract != local_runtime.contract
        || peer.runtime.schema_version != local_runtime.schema_version
    {
        return reject(
            "RUNTIME_CAPABILITY_MISMATCH",
            "peer runtime capability contract is incompatible with this node".into(),
        );
    }
    if !valid_node_id(&peer.runtime.node.id) {
        return reject(
            "FEDERATION_NODE_ID_INVALID",
            "peer node identity is not a valid stable Anchor node id".into(),
        );
    }
    if peer.runtime.node.id == local_runtime.node.id {
        return reject(
            "FEDERATION_SELF_PEER",
            "a node cannot establish a federation peer relationship with itself".into(),
        );
    }
    if peer.runtime.workspace.is_some() {
        return reject(
            "FEDERATION_NODE_DESCRIPTOR_SCOPED_TO_WORKSPACE",
            "federation peer descriptors must advertise node capabilities, not a workspace-scoped runtime snapshot".into(),
        );
    }
    if peer.runtime.transports.federation != FEDERATION_TRANSPORT_MODE {
        return reject(
            "FEDERATION_TRANSPORT_UNSUPPORTED",
            format!(
                "peer federation transport mode `{}` is not supported by this protocol stage",
                peer.runtime.transports.federation
            ),
        );
    }
    if peer.access != FederationAccessMode::ReadOnly {
        return reject(
            "FEDERATION_ACCESS_UNSUPPORTED",
            "P2 federation foundation accepts read-only peers only".into(),
        );
    }
    if peer.workspaces.len() > MAX_FEDERATION_WORKSPACE_ROUTES {
        return reject(
            "FEDERATION_ROUTE_LIMIT_EXCEEDED",
            format!(
                "peer advertises {} workspace routes; maximum is {}",
                peer.workspaces.len(),
                MAX_FEDERATION_WORKSPACE_ROUTES
            ),
        );
    }

    let allowed = READ_ONLY_OPERATIONS
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    let observed = peer.operations.iter().copied().collect::<BTreeSet<_>>();
    if observed.len() != peer.operations.len() || observed != allowed {
        return reject(
            "FEDERATION_OPERATION_SET_INVALID",
            "peer operation set must exactly match the unique read-only federation v1 operations"
                .into(),
        );
    }

    let mut workspace_ids = BTreeSet::new();
    for route in &peer.workspaces {
        if route.node_id != peer.runtime.node.id {
            return reject(
                "FEDERATION_ROUTE_NODE_MISMATCH",
                format!(
                    "workspace route {} is bound to a different node identity",
                    route.workspace_id
                ),
            );
        }
        if !valid_workspace_id(&route.workspace_id)
            || route.display_name.trim().is_empty()
            || route.display_name.len() > MAX_FEDERATION_DISPLAY_NAME_BYTES
        {
            return reject(
                "FEDERATION_ROUTE_INVALID",
                "workspace routes require a stable workspace id and non-empty display name".into(),
            );
        }
        if route.access != FederationAccessMode::ReadOnly {
            return reject(
                "FEDERATION_ROUTE_WRITE_FORBIDDEN",
                "workspace federation routes are read-only in protocol v1".into(),
            );
        }
        if !workspace_ids.insert(route.workspace_id.clone()) {
            return reject(
                "FEDERATION_ROUTE_DUPLICATE",
                format!("duplicate workspace route {}", route.workspace_id),
            );
        }
    }

    let policy = &peer.context_policy;
    let context_scopes = policy.scopes.iter().copied().collect::<BTreeSet<_>>();
    let required_context_scopes = [
        FederationContextScope::GlobalShared,
        FederationContextScope::NodeLocal,
        FederationContextScope::WorkspaceLocal,
    ]
    .into_iter()
    .collect::<BTreeSet<_>>();
    if context_scopes != required_context_scopes
        || context_scopes.len() != policy.scopes.len()
        || policy.export_workspace_paths
        || policy.export_secrets
        || policy.export_harness_state
        || policy.remote_mutation
    {
        return reject(
            "FEDERATION_CONTEXT_POLICY_UNSAFE",
            "peer context policy must keep local paths, secrets, Harness state, and mutation outside the federation envelope".into(),
        );
    }

    FederationHandshakeValidation {
        accepted: true,
        code: "FEDERATION_PEER_ACCEPTED".into(),
        reason: "peer is compatible with the read-only federation v1 foundation".into(),
        peer_node_id,
    }
}

pub fn resolve_local_read(
    local: &FederationPeerDescriptor,
    request: &FederationReadRequest,
) -> AppResult<FederationReadPlan> {
    validate_request_context(request)?;
    if request.node_id != local.runtime.node.id {
        return Err(AppError::Message(format!(
            "FEDERATION_REMOTE_TRANSPORT_UNAVAILABLE: node {} is not local; P2 foundation does not yet provide cross-node transport",
            request.node_id
        )));
    }
    if !local.operations.contains(&request.operation) {
        return Err(AppError::Message(format!(
            "FEDERATION_OPERATION_UNAVAILABLE: operation {} is not advertised by the local federation descriptor",
            request.operation.as_str()
        )));
    }

    let (workspace_id, source) = match request.operation {
        FederationReadOperation::NodeCapabilities => {
            require_no_workspace(request)?;
            (None, FederationReadSource::RuntimeCapabilityProvider)
        }
        FederationReadOperation::NodeControlStatus => {
            require_no_workspace(request)?;
            (None, FederationReadSource::ExistingControlPlane)
        }
        FederationReadOperation::WorkspaceCatalog => {
            require_no_workspace(request)?;
            (None, FederationReadSource::WorkspaceRegistry)
        }
        FederationReadOperation::WorkspaceStatus => {
            let workspace_id = request.workspace_id.as_deref().ok_or_else(|| {
                AppError::Message(
                    "FEDERATION_WORKSPACE_REQUIRED: workspace_status requires workspaceId".into(),
                )
            })?;
            if !local
                .workspaces
                .iter()
                .any(|route| route.workspace_id == workspace_id)
            {
                return Err(AppError::Message(format!(
                    "FEDERATION_WORKSPACE_NOT_FOUND: workspace {workspace_id} is not registered on the local node"
                )));
            }
            (
                Some(workspace_id.to_string()),
                FederationReadSource::ExistingControlPlane,
            )
        }
    };

    Ok(FederationReadPlan {
        node_id: local.runtime.node.id.clone(),
        workspace_id,
        operation: request.operation,
        access: FederationAccessMode::ReadOnly,
        source,
        transport: FEDERATION_TRANSPORT_MODE.into(),
    })
}

fn require_no_workspace(request: &FederationReadRequest) -> AppResult<()> {
    if request.workspace_id.is_some() {
        return Err(AppError::Message(format!(
            "FEDERATION_WORKSPACE_SCOPE_INVALID: operation {} is node-scoped and must not include workspaceId",
            request.operation.as_str()
        )));
    }
    Ok(())
}

pub(crate) fn valid_node_id(value: &str) -> bool {
    value.strip_prefix("node_").is_some_and(|suffix| {
        suffix.len() == 32 && suffix.bytes().all(|byte| byte.is_ascii_hexdigit())
    })
}

fn valid_workspace_id(value: &str) -> bool {
    value.len() == 32 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn bounded_display_name(value: &str, fallback: &str) -> String {
    let trimmed = value.trim();
    let source = if trimmed.is_empty() {
        fallback
    } else {
        trimmed
    };
    if source.len() <= MAX_FEDERATION_DISPLAY_NAME_BYTES {
        return source.to_string();
    }
    let mut end = MAX_FEDERATION_DISPLAY_NAME_BYTES;
    while end > 0 && !source.is_char_boundary(end) {
        end -= 1;
    }
    source[..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::{NodeIdentity, RuntimeWorkspaceIdentity};

    fn runtime(node_id: &str) -> RuntimeCapabilitySnapshot {
        crate::runtime::capability_snapshot_from_node(
            NodeIdentity {
                id: node_id.into(),
                platform: "test".into(),
                architecture: "test".into(),
            },
            None,
        )
    }

    fn peer(node_id: &str) -> FederationPeerDescriptor {
        FederationPeerDescriptor {
            schema_version: FEDERATION_SCHEMA_VERSION,
            contract: FEDERATION_CONTRACT.into(),
            runtime: runtime(node_id),
            access: FederationAccessMode::ReadOnly,
            operations: READ_ONLY_OPERATIONS.to_vec(),
            workspaces: vec![FederationWorkspaceRoute {
                node_id: node_id.into(),
                workspace_id: "0123456789abcdef0123456789abcdef".into(),
                display_name: "Remote workspace".into(),
                access: FederationAccessMode::ReadOnly,
            }],
            context_policy: FederationContextPolicy::default(),
        }
    }

    #[test]
    fn peer_handshake_accepts_only_compatible_read_only_descriptors() {
        let local = runtime("node_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        let compatible = peer("node_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        let accepted = validate_peer_descriptor(&local, &compatible);
        assert!(accepted.accepted);

        let mut unsafe_peer = compatible.clone();
        unsafe_peer.context_policy.export_workspace_paths = true;
        let rejected = validate_peer_descriptor(&local, &unsafe_peer);
        assert!(!rejected.accepted);
        assert_eq!(rejected.code, "FEDERATION_CONTEXT_POLICY_UNSAFE");

        let mut mismatched = compatible;
        mismatched.runtime.contract = "future-runtime-contract".into();
        let rejected = validate_peer_descriptor(&local, &mismatched);
        assert_eq!(rejected.code, "RUNTIME_CAPABILITY_MISMATCH");

        let mut wrong_authority = peer("node_cccccccccccccccccccccccccccccccc");
        wrong_authority.runtime.features.state_authority = "peer_private_state".into();
        let rejected = validate_peer_descriptor(&local, &wrong_authority);
        assert_eq!(rejected.code, "FEDERATION_RUNTIME_AUTHORITY_UNSUPPORTED");

        let mut incomplete_operations = peer("node_dddddddddddddddddddddddddddddddd");
        incomplete_operations.operations.pop();
        let rejected = validate_peer_descriptor(&local, &incomplete_operations);
        assert_eq!(rejected.code, "FEDERATION_OPERATION_SET_INVALID");
    }

    #[test]
    fn route_descriptor_never_contains_workspace_paths() {
        let route = FederationWorkspaceRoute {
            node_id: "node_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into(),
            workspace_id: "0123456789abcdef0123456789abcdef".into(),
            display_name: "Workspace".into(),
            access: FederationAccessMode::ReadOnly,
        };
        let value = serde_json::to_value(route).expect("route json");
        assert!(value.get("path").is_none());
        assert!(value.get("workspacePath").is_none());
    }

    #[test]
    fn peer_payload_rejects_unknown_workspace_path_fields() {
        let mut value =
            serde_json::to_value(peer("node_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")).expect("peer json");
        value["workspaces"][0]["path"] = serde_json::Value::String("/remote/repo".into());
        let error = serde_json::from_value::<FederationPeerDescriptor>(value)
            .expect_err("unknown route path must fail closed");
        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn display_names_are_bounded_without_breaking_utf8() {
        let value = "节点".repeat(200);
        let bounded = bounded_display_name(&value, "fallback");
        assert!(bounded.len() <= MAX_FEDERATION_DISPLAY_NAME_BYTES);
        assert!(!bounded.is_empty());
        assert!(bounded.is_char_boundary(bounded.len()));
    }

    #[test]
    fn local_router_plans_only_authoritative_read_sources() {
        let local = peer("node_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        let workspace_id = local.workspaces[0].workspace_id.clone();
        let plan = resolve_local_read(
            &local,
            &FederationReadRequest {
                node_id: local.runtime.node.id.clone(),
                workspace_id: Some(workspace_id.clone()),
                operation: FederationReadOperation::WorkspaceStatus,
                context: FederationContextRef {
                    scope: FederationContextScope::WorkspaceLocal,
                    node_id: Some(local.runtime.node.id.clone()),
                    workspace_id: Some(workspace_id.clone()),
                },
            },
        )
        .expect("local route");
        assert_eq!(plan.workspace_id.as_deref(), Some(workspace_id.as_str()));
        assert_eq!(plan.source, FederationReadSource::ExistingControlPlane);
        assert_eq!(plan.access, FederationAccessMode::ReadOnly);

        let remote = resolve_local_read(
            &local,
            &FederationReadRequest {
                node_id: "node_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into(),
                workspace_id: None,
                operation: FederationReadOperation::NodeCapabilities,
                context: FederationContextRef {
                    scope: FederationContextScope::NodeLocal,
                    node_id: Some("node_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into()),
                    workspace_id: None,
                },
            },
        )
        .expect_err("remote transport is intentionally absent");
        assert!(remote
            .to_string()
            .contains("FEDERATION_REMOTE_TRANSPORT_UNAVAILABLE"));

        let mismatched_context = resolve_local_read(
            &local,
            &FederationReadRequest {
                node_id: local.runtime.node.id.clone(),
                workspace_id: Some(workspace_id.clone()),
                operation: FederationReadOperation::WorkspaceStatus,
                context: FederationContextRef {
                    scope: FederationContextScope::NodeLocal,
                    node_id: Some(local.runtime.node.id.clone()),
                    workspace_id: None,
                },
            },
        )
        .expect_err("workspace status must require workspace-local context");
        assert!(mismatched_context
            .to_string()
            .contains("FEDERATION_CONTEXT_ROUTE_MISMATCH"));
    }

    #[test]
    fn context_refs_enforce_global_node_and_workspace_boundaries() {
        validate_context_ref(&FederationContextRef {
            scope: FederationContextScope::GlobalShared,
            node_id: None,
            workspace_id: None,
        })
        .expect("global context");

        let invalid_global = validate_context_ref(&FederationContextRef {
            scope: FederationContextScope::GlobalShared,
            node_id: Some("node_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into()),
            workspace_id: None,
        })
        .expect_err("global context must not be node-bound");
        assert!(invalid_global
            .to_string()
            .contains("FEDERATION_CONTEXT_SCOPE_INVALID"));

        validate_context_ref(&FederationContextRef {
            scope: FederationContextScope::WorkspaceLocal,
            node_id: Some("node_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into()),
            workspace_id: Some("0123456789abcdef0123456789abcdef".into()),
        })
        .expect("workspace-local context");
    }

    #[test]
    fn workspace_scoped_runtime_snapshot_is_rejected_as_a_node_handshake() {
        let local = runtime("node_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        let mut remote = peer("node_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        remote.runtime.workspace = Some(RuntimeWorkspaceIdentity::new(
            "0123456789abcdef0123456789abcdef",
            "workspace",
            "/remote/path/that/must/not/be/trusted",
        ));
        let validation = validate_peer_descriptor(&local, &remote);
        assert!(!validation.accepted);
        assert_eq!(
            validation.code,
            "FEDERATION_NODE_DESCRIPTOR_SCOPED_TO_WORKSPACE"
        );
    }
}
