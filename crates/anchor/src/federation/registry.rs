use std::collections::BTreeSet;
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{AppError, AppResult};

use super::{
    canonical_remote_target, clear_peer_credential, peer_credential_status, probe_remote_peer,
    remote_read_verified, set_peer_credential, valid_node_id, validate_peer_descriptor,
    FederationBootstrapBundle, FederationNodeSigningPublic, FederationPeerCredentialStatus,
    FederationPeerDescriptor, FederationReadRequest, FederationReadResult, FederationRemoteTarget,
};

const PEER_REGISTRY_SCHEMA_VERSION: u16 = 2;
const MAX_PEERS: usize = 128;
const MAX_DISPLAY_NAME_BYTES: usize = 200;
const TRUST_PROBE_MAX_AGE_MS: u64 = 10 * 60 * 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FederationPeerTrustStatus {
    Untrusted,
    Trusted,
    Drifted,
    Revoked,
}

fn apply_successful_probe(
    record: &mut FederationPeerRecord,
    digest: &str,
    signer: &FederationNodeSigningPublic,
    now: u64,
) {
    record.last_probe_at_unix_ms = Some(now);
    record.last_probe_code = Some("FEDERATION_PEER_PROBE_OK".into());
    record.last_descriptor_digest = Some(digest.to_string());
    record.last_signing = Some(signer.clone());
    if record.trust_status == FederationPeerTrustStatus::Trusted
        && (record.trusted_descriptor_digest.as_deref() != Some(digest)
            || record.trusted_signing.as_ref() != Some(signer))
    {
        record.trust_status = FederationPeerTrustStatus::Drifted;
        record.last_probe_code = Some("FEDERATION_PEER_TRUST_DRIFT".into());
    }
    record.updated_at_unix_ms = now;
}

fn recent_successful_probe_material(
    record: &FederationPeerRecord,
    now: u64,
) -> AppResult<(String, FederationNodeSigningPublic)> {
    let probed_at = record.last_probe_at_unix_ms.ok_or_else(|| {
        AppError::Message(
            "FEDERATION_TRUST_REQUIRES_PROBE: trust requires a successful recent peer probe".into(),
        )
    })?;
    let digest = record.last_descriptor_digest.clone().ok_or_else(|| {
        AppError::Message(
            "FEDERATION_TRUST_REQUIRES_PROBE: trust requires a successful recent peer descriptor"
                .into(),
        )
    })?;
    let signing = record.last_signing.clone().ok_or_else(|| {
        AppError::Message(
            "FEDERATION_TRUST_REQUIRES_SIGNED_PROBE: trust requires a verified peer signing identity"
                .into(),
        )
    })?;
    if !matches!(
        record.last_probe_code.as_deref(),
        Some("FEDERATION_PEER_PROBE_OK" | "FEDERATION_PEER_TRUST_DRIFT")
    ) || now.saturating_sub(probed_at) > TRUST_PROBE_MAX_AGE_MS
    {
        return Err(AppError::Message(
            "FEDERATION_TRUST_PROBE_STALE: trust requires a successful probe from the last 10 minutes"
                .into(),
        ));
    }
    if digest != record.bootstrap_descriptor_digest || signing != record.bootstrap_signing {
        return Err(AppError::Message(
            "FEDERATION_TRUST_BOOTSTRAP_MISMATCH: recent probe does not match the active out-of-band bootstrap pin"
                .into(),
        ));
    }
    Ok((digest, signing))
}

pub fn require_registered_target(
    target: &FederationRemoteTarget,
) -> AppResult<FederationPeerRecord> {
    let canonical = canonical_remote_target(target)?;
    let record = record_at(
        &crate::platform::platform().app_config_dir()?,
        &canonical.node_id,
    )?;
    if record.endpoint != canonical.endpoint {
        return Err(AppError::Message(format!(
            "FEDERATION_PEER_ENDPOINT_MISMATCH: registered endpoint {} does not match requested endpoint {}",
            record.endpoint, canonical.endpoint
        )));
    }
    Ok(record)
}

pub fn require_trusted_target(target: &FederationRemoteTarget) -> AppResult<FederationPeerRecord> {
    let record = require_registered_target(target)?;
    ensure_read_trust(&record)?;
    Ok(record)
}

fn ensure_read_trust(record: &FederationPeerRecord) -> AppResult<()> {
    match record.trust_status {
        FederationPeerTrustStatus::Trusted => Ok(()),
        FederationPeerTrustStatus::Drifted => Err(AppError::Message(
            "FEDERATION_PEER_TRUST_DRIFT: peer trust has drifted and requires an explicit re-trust"
                .into(),
        )),
        FederationPeerTrustStatus::Revoked => Err(AppError::Message(
            "FEDERATION_PEER_REVOKED: revoked peer cannot serve remote reads".into(),
        )),
        FederationPeerTrustStatus::Untrusted => Err(AppError::Message(
            "FEDERATION_PEER_UNTRUSTED: remote reads require an explicitly trusted peer".into(),
        )),
    }
}

pub async fn read_trusted_peer(
    target: &FederationRemoteTarget,
    request: &FederationReadRequest,
) -> AppResult<FederationReadResult> {
    let root = crate::platform::platform().app_config_dir()?;
    let record = require_trusted_target(target)?;
    if request.node_id != record.node_id {
        return Err(AppError::Message(
            "FEDERATION_REMOTE_TARGET_MISMATCH: request nodeId does not match the trusted peer"
                .into(),
        ));
    }
    let verified = remote_read_verified(&target_from(&record), request).await?;
    if record.trusted_signing.as_ref() != Some(&verified.signer) {
        let now = unix_time_ms()?;
        update_registry_at(&root, |registry| {
            let current = find_peer_mut(registry, &record.node_id)?;
            current.trust_status = FederationPeerTrustStatus::Drifted;
            current.last_signing = Some(verified.signer.clone());
            current.last_probe_code = Some("FEDERATION_PEER_SIGNING_KEY_DRIFT".into());
            current.updated_at_unix_ms = now;
            Ok(())
        })?;
        return Err(AppError::Message(
            "FEDERATION_PEER_SIGNING_KEY_DRIFT: remote response signer does not match the trusted key pin"
                .into(),
        ));
    }
    Ok(verified.result)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FederationPeerRecord {
    pub node_id: String,
    pub endpoint: String,
    pub display_name: String,
    pub bootstrap_descriptor_digest: String,
    pub bootstrap_signing: FederationNodeSigningPublic,
    pub trust_status: FederationPeerTrustStatus,
    pub registered_at_unix_ms: u64,
    pub updated_at_unix_ms: u64,
    pub credential_revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_probe_at_unix_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_probe_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_descriptor_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trusted_descriptor_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_signing: Option<FederationNodeSigningPublic>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trusted_signing: Option<FederationNodeSigningPublic>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FederationPeerView {
    #[serde(flatten)]
    pub peer: FederationPeerRecord,
    pub credential: FederationPeerCredentialStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FederationPeerRegistration {
    pub endpoint: String,
    pub display_name: String,
    pub bundle: FederationBootstrapBundle,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FederationPeerUpdate {
    pub node_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bootstrap: Option<FederationBootstrapBundle>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FederationPeerCandidate {
    pub node_id: String,
    pub endpoint: String,
    pub display_name: String,
    pub descriptor_digest: String,
    pub signing: FederationNodeSigningPublic,
    pub trust_status: FederationPeerTrustStatus,
    pub persisted: bool,
    pub credential_sent: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FederationPeerRegistryFile {
    schema_version: u16,
    peers: Vec<FederationPeerRecord>,
}

impl Default for FederationPeerRegistryFile {
    fn default() -> Self {
        Self {
            schema_version: PEER_REGISTRY_SCHEMA_VERSION,
            peers: Vec::new(),
        }
    }
}

pub fn list_peers() -> AppResult<Vec<FederationPeerView>> {
    let root = crate::platform::platform().app_config_dir()?;
    list_peers_at(&root)
}

pub fn get_peer(node_id: &str) -> AppResult<FederationPeerView> {
    let root = crate::platform::platform().app_config_dir()?;
    get_peer_at(&root, node_id)
}

pub fn register_peer(input: &FederationPeerRegistration) -> AppResult<FederationPeerView> {
    let root = crate::platform::platform().app_config_dir()?;
    let candidate = inspect_candidate(&input.endpoint, &input.display_name, &input.bundle)?;
    let target = canonical_remote_target(&FederationRemoteTarget {
        node_id: candidate.node_id.clone(),
        endpoint: candidate.endpoint.clone(),
    })?;
    reject_self_peer(&target.node_id)?;
    let registry = load_registry_at(&root)?;
    if registry.peers.len() >= MAX_PEERS {
        return Err(AppError::Message(format!(
            "FEDERATION_PEER_LIMIT_EXCEEDED: maximum peer count is {MAX_PEERS}"
        )));
    }
    if registry
        .peers
        .iter()
        .any(|peer| peer.node_id == target.node_id)
    {
        return Err(AppError::Message(format!(
            "FEDERATION_PEER_EXISTS: peer {} is already registered",
            target.node_id
        )));
    }
    ensure_endpoint_unique(&registry, &target.endpoint, None)?;
    clear_peer_credential(&target)?;
    register_peer_at(&root, input)
}

pub fn update_peer(input: &FederationPeerUpdate) -> AppResult<FederationPeerView> {
    let root = crate::platform::platform().app_config_dir()?;
    let existing = record_at(&root, &input.node_id)?;
    let next_endpoint = if let Some(endpoint) = input.endpoint.as_deref() {
        canonical_remote_target(&FederationRemoteTarget {
            node_id: input.node_id.clone(),
            endpoint: endpoint.into(),
        })?
        .endpoint
    } else {
        existing.endpoint.clone()
    };
    let next_display_name = if let Some(name) = input.display_name.as_deref() {
        normalize_display_name(name, &input.node_id)?
    } else {
        existing.display_name.clone()
    };
    let endpoint_changed = next_endpoint != existing.endpoint;
    let bootstrap_candidate = input
        .bootstrap
        .as_ref()
        .map(|bundle| inspect_candidate(&next_endpoint, &next_display_name, bundle))
        .transpose()?;
    if let Some(candidate) = bootstrap_candidate.as_ref() {
        if candidate.node_id != input.node_id {
            return Err(AppError::Message(
                "FEDERATION_BOOTSTRAP_NODE_MISMATCH: re-bootstrap bundle belongs to a different node"
                    .into(),
            ));
        }
    } else if endpoint_changed {
        return Err(AppError::Message(
            "FEDERATION_PEER_REBOOTSTRAP_REQUIRED: endpoint changes require a signed bootstrap bundle"
                .into(),
        ));
    }
    let registry = load_registry_at(&root)?;
    ensure_endpoint_unique(&registry, &next_endpoint, Some(&input.node_id))?;
    if endpoint_changed {
        clear_peer_credential(&target_from(&existing))?;
    }
    update_peer_at(&root, input)
}

pub fn remove_peer(node_id: &str) -> AppResult<()> {
    let root = crate::platform::platform().app_config_dir()?;
    let record = record_at(&root, node_id)?;
    clear_peer_credential(&target_from(&record))?;
    remove_peer_at(&root, node_id)
}

pub fn rotate_peer_credential(node_id: &str, token: &str) -> AppResult<FederationPeerView> {
    let root = crate::platform::platform().app_config_dir()?;
    rotate_peer_credential_at(&root, node_id, token)
}

pub fn revoke_peer(node_id: &str) -> AppResult<FederationPeerView> {
    let root = crate::platform::platform().app_config_dir()?;
    let record = record_at(&root, node_id)?;
    clear_peer_credential(&target_from(&record))?;
    revoke_peer_at(&root, node_id)
}

pub async fn probe_registered_peer(node_id: &str) -> AppResult<FederationPeerView> {
    let root = crate::platform::platform().app_config_dir()?;
    let peer = record_at(&root, node_id)?;
    if peer.trust_status == FederationPeerTrustStatus::Revoked {
        return Err(AppError::Message(
            "FEDERATION_PEER_REVOKED: revoked peers cannot be probed until explicitly updated and re-established"
                .into(),
        ));
    }
    let target = target_from(&peer);
    let now = unix_time_ms()?;
    match probe_remote_peer(&target).await {
        Ok(probe) => {
            let digest = descriptor_trust_digest(&probe.descriptor)?;
            if digest != peer.bootstrap_descriptor_digest || probe.signer != peer.bootstrap_signing
            {
                update_registry_at(&root, |registry| {
                    let record = find_peer_mut(registry, node_id)?;
                    record.last_probe_at_unix_ms = Some(now);
                    record.last_probe_code = Some("FEDERATION_PEER_BOOTSTRAP_DRIFT".into());
                    record.last_descriptor_digest = Some(digest.clone());
                    record.last_signing = Some(probe.signer.clone());
                    if matches!(
                        record.trust_status,
                        FederationPeerTrustStatus::Trusted | FederationPeerTrustStatus::Drifted
                    ) {
                        record.trust_status = FederationPeerTrustStatus::Drifted;
                    }
                    record.updated_at_unix_ms = now;
                    Ok(())
                })?;
                return Err(AppError::Message(
                    "FEDERATION_PEER_BOOTSTRAP_DRIFT: live peer signing identity or security descriptor does not match the out-of-band bootstrap pin"
                        .into(),
                ));
            }
            update_registry_at(&root, |registry| {
                let record = find_peer_mut(registry, node_id)?;
                apply_successful_probe(record, &digest, &probe.signer, now);
                Ok(())
            })?;
            get_peer_at(&root, node_id)
        }
        Err(error) => {
            let code = error_code(&error.to_string());
            update_registry_at(&root, |registry| {
                let record = find_peer_mut(registry, node_id)?;
                record.last_probe_at_unix_ms = Some(now);
                record.last_probe_code = Some(code.clone());
                if record.trust_status == FederationPeerTrustStatus::Trusted
                    && contract_drift_error(&code)
                {
                    record.trust_status = FederationPeerTrustStatus::Drifted;
                }
                record.updated_at_unix_ms = now;
                Ok(())
            })?;
            Err(error)
        }
    }
}

pub fn trust_peer(node_id: &str) -> AppResult<FederationPeerView> {
    let root = crate::platform::platform().app_config_dir()?;
    trust_peer_at(&root, node_id)
}

pub fn inspect_candidate(
    endpoint: &str,
    display_name: &str,
    bundle: &FederationBootstrapBundle,
) -> AppResult<FederationPeerCandidate> {
    let signing = super::verify_bootstrap_bundle(bundle)?;
    let descriptor = &bundle.descriptor;
    let local_runtime = crate::runtime::capability_snapshot(None)?;
    let validation = validate_peer_descriptor(&local_runtime, descriptor);
    if !validation.accepted {
        return Err(AppError::Message(format!(
            "{}: {}",
            validation.code, validation.reason
        )));
    }
    let target = canonical_remote_target(&FederationRemoteTarget {
        node_id: descriptor.runtime.node.id.clone(),
        endpoint: endpoint.into(),
    })?;
    let display_name = normalize_display_name(display_name, &target.node_id)?;
    Ok(FederationPeerCandidate {
        node_id: target.node_id,
        endpoint: target.endpoint,
        display_name,
        descriptor_digest: descriptor_trust_digest(descriptor)?,
        signing,
        trust_status: FederationPeerTrustStatus::Untrusted,
        persisted: false,
        credential_sent: false,
    })
}

pub fn descriptor_trust_digest(descriptor: &FederationPeerDescriptor) -> AppResult<String> {
    let mut operations = descriptor
        .operations
        .iter()
        .map(|operation| operation.as_str())
        .collect::<Vec<_>>();
    operations.sort_unstable();
    let material = serde_json::json!({
        "schemaVersion": descriptor.schema_version,
        "contract": descriptor.contract,
        "runtimeSchemaVersion": descriptor.runtime.schema_version,
        "runtimeContract": descriptor.runtime.contract,
        "node": descriptor.runtime.node,
        "transports": descriptor.runtime.transports,
        "features": descriptor.runtime.features,
        "access": descriptor.access,
        "operations": operations,
        "contextPolicy": descriptor.context_policy,
    });
    let bytes = serde_json::to_vec(&material)?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn list_peers_at(root: &Path) -> AppResult<Vec<FederationPeerView>> {
    let registry = load_registry_at(root)?;
    registry
        .peers
        .into_iter()
        .map(view_for_record)
        .collect::<AppResult<Vec<_>>>()
}

fn get_peer_at(root: &Path, node_id: &str) -> AppResult<FederationPeerView> {
    view_for_record(record_at(root, node_id)?)
}

fn record_at(root: &Path, node_id: &str) -> AppResult<FederationPeerRecord> {
    validate_node_selector(node_id)?;
    load_registry_at(root)?
        .peers
        .into_iter()
        .find(|peer| peer.node_id == node_id)
        .ok_or_else(|| AppError::Message(format!("FEDERATION_PEER_NOT_FOUND: {node_id}")))
}

fn register_peer_at(
    root: &Path,
    input: &FederationPeerRegistration,
) -> AppResult<FederationPeerView> {
    let candidate = inspect_candidate(&input.endpoint, &input.display_name, &input.bundle)?;
    let target = canonical_remote_target(&FederationRemoteTarget {
        node_id: candidate.node_id.clone(),
        endpoint: candidate.endpoint.clone(),
    })?;
    reject_self_peer(&target.node_id)?;
    let display_name = candidate.display_name.clone();
    let now = unix_time_ms()?;
    update_registry_at(root, |registry| {
        if registry.peers.len() >= MAX_PEERS {
            return Err(AppError::Message(format!(
                "FEDERATION_PEER_LIMIT_EXCEEDED: maximum peer count is {MAX_PEERS}"
            )));
        }
        if registry
            .peers
            .iter()
            .any(|peer| peer.node_id == target.node_id)
        {
            return Err(AppError::Message(format!(
                "FEDERATION_PEER_EXISTS: peer {} is already registered",
                target.node_id
            )));
        }
        ensure_endpoint_unique(registry, &target.endpoint, None)?;
        ensure_bootstrap_signer_unique(registry, &candidate.signing, None)?;
        registry.peers.push(FederationPeerRecord {
            node_id: target.node_id.clone(),
            endpoint: target.endpoint.clone(),
            display_name: display_name.clone(),
            bootstrap_descriptor_digest: candidate.descriptor_digest.clone(),
            bootstrap_signing: candidate.signing.clone(),
            trust_status: FederationPeerTrustStatus::Untrusted,
            registered_at_unix_ms: now,
            updated_at_unix_ms: now,
            credential_revision: 0,
            last_probe_at_unix_ms: None,
            last_probe_code: None,
            last_descriptor_digest: None,
            trusted_descriptor_digest: None,
            last_signing: None,
            trusted_signing: None,
        });
        registry
            .peers
            .sort_by(|left, right| left.node_id.cmp(&right.node_id));
        Ok(())
    })?;
    get_peer_at(root, &target.node_id)
}

fn update_peer_at(root: &Path, input: &FederationPeerUpdate) -> AppResult<FederationPeerView> {
    validate_node_selector(&input.node_id)?;
    let existing = record_at(root, &input.node_id)?;
    let next_endpoint = if let Some(endpoint) = input.endpoint.as_deref() {
        canonical_remote_target(&FederationRemoteTarget {
            node_id: input.node_id.clone(),
            endpoint: endpoint.into(),
        })?
        .endpoint
    } else {
        existing.endpoint.clone()
    };
    let next_display_name = if let Some(name) = input.display_name.as_deref() {
        normalize_display_name(name, &input.node_id)?
    } else {
        existing.display_name.clone()
    };
    let endpoint_changed = next_endpoint != existing.endpoint;
    let bootstrap_candidate = input
        .bootstrap
        .as_ref()
        .map(|bundle| inspect_candidate(&next_endpoint, &next_display_name, bundle))
        .transpose()?;
    if let Some(candidate) = bootstrap_candidate.as_ref() {
        if candidate.node_id != input.node_id {
            return Err(AppError::Message(
                "FEDERATION_BOOTSTRAP_NODE_MISMATCH: re-bootstrap bundle belongs to a different node"
                    .into(),
            ));
        }
    } else if endpoint_changed {
        return Err(AppError::Message(
            "FEDERATION_PEER_REBOOTSTRAP_REQUIRED: endpoint changes require a signed bootstrap bundle"
                .into(),
        ));
    }
    let now = unix_time_ms()?;
    update_registry_at(root, |registry| {
        apply_peer_update_metadata(
            registry,
            &input.node_id,
            &next_endpoint,
            &next_display_name,
            endpoint_changed,
            bootstrap_candidate.as_ref(),
            now,
        )
    })?;
    get_peer_at(root, &input.node_id)
}

fn apply_peer_update_metadata(
    registry: &mut FederationPeerRegistryFile,
    node_id: &str,
    next_endpoint: &str,
    next_display_name: &str,
    endpoint_changed: bool,
    bootstrap: Option<&FederationPeerCandidate>,
    now: u64,
) -> AppResult<()> {
    ensure_endpoint_unique(registry, next_endpoint, Some(node_id))?;
    if let Some(candidate) = bootstrap {
        ensure_bootstrap_signer_unique(registry, &candidate.signing, Some(node_id))?;
    }
    let record = find_peer_mut(registry, node_id)?;
    record.endpoint = next_endpoint.to_string();
    record.display_name = next_display_name.to_string();
    record.updated_at_unix_ms = now;
    if let Some(candidate) = bootstrap {
        record.bootstrap_descriptor_digest = candidate.descriptor_digest.clone();
        record.bootstrap_signing = candidate.signing.clone();
        record.trust_status = FederationPeerTrustStatus::Untrusted;
        if endpoint_changed {
            record.credential_revision = record.credential_revision.saturating_add(1);
        }
        record.last_probe_at_unix_ms = None;
        record.last_probe_code = Some(if endpoint_changed {
            "FEDERATION_PEER_ENDPOINT_REBOOTSTRAPPED".into()
        } else {
            "FEDERATION_PEER_SIGNING_REBOOTSTRAPPED".into()
        });
        record.last_descriptor_digest = None;
        record.trusted_descriptor_digest = None;
        record.last_signing = None;
        record.trusted_signing = None;
    }
    Ok(())
}

fn ensure_bootstrap_signer_unique(
    registry: &FederationPeerRegistryFile,
    signer: &FederationNodeSigningPublic,
    except_node_id: Option<&str>,
) -> AppResult<()> {
    if registry.peers.iter().any(|peer| {
        peer.bootstrap_signing.fingerprint == signer.fingerprint
            && except_node_id != Some(peer.node_id.as_str())
    }) {
        return Err(AppError::Message(
            "FEDERATION_SIGNING_KEY_ALREADY_REGISTERED: one bootstrap signing key cannot identify multiple peer nodes"
                .into(),
        ));
    }
    Ok(())
}

fn remove_peer_at(root: &Path, node_id: &str) -> AppResult<()> {
    record_at(root, node_id)?;
    update_registry_at(root, |registry| {
        registry.peers.retain(|peer| peer.node_id != node_id);
        Ok(())
    })
}

fn rotate_peer_credential_at(
    root: &Path,
    node_id: &str,
    token: &str,
) -> AppResult<FederationPeerView> {
    let record = record_at(root, node_id)?;
    set_peer_credential(&target_from(&record), token)?;
    let now = unix_time_ms()?;
    update_registry_at(root, |registry| {
        let record = find_peer_mut(registry, node_id)?;
        record.credential_revision = record.credential_revision.saturating_add(1);
        record.updated_at_unix_ms = now;
        if record.trust_status == FederationPeerTrustStatus::Revoked {
            record.trust_status = FederationPeerTrustStatus::Untrusted;
            record.trusted_descriptor_digest = None;
            record.trusted_signing = None;
        }
        Ok(())
    })?;
    get_peer_at(root, node_id)
}

fn revoke_peer_at(root: &Path, node_id: &str) -> AppResult<FederationPeerView> {
    record_at(root, node_id)?;
    let now = unix_time_ms()?;
    update_registry_at(root, |registry| {
        let record = find_peer_mut(registry, node_id)?;
        record.trust_status = FederationPeerTrustStatus::Revoked;
        record.credential_revision = record.credential_revision.saturating_add(1);
        record.updated_at_unix_ms = now;
        record.trusted_descriptor_digest = None;
        record.trusted_signing = None;
        record.last_probe_code = Some("FEDERATION_PEER_REVOKED".into());
        Ok(())
    })?;
    get_peer_at(root, node_id)
}

fn trust_peer_at(root: &Path, node_id: &str) -> AppResult<FederationPeerView> {
    let record = record_at(root, node_id)?;
    let credential = peer_credential_status(&target_from(&record))?;
    if !credential.outbound_configured || !credential.inbound_configured {
        return Err(AppError::Message(
            "FEDERATION_CREDENTIAL_MISSING: a peer must have pairwise credentials before it can be trusted"
                .into(),
        ));
    }
    let now = unix_time_ms()?;
    let (digest, signing) = recent_successful_probe_material(&record, now)?;
    update_registry_at(root, |registry| {
        let record = find_peer_mut(registry, node_id)?;
        record.trust_status = FederationPeerTrustStatus::Trusted;
        record.trusted_descriptor_digest = Some(digest.clone());
        record.trusted_signing = Some(signing.clone());
        record.updated_at_unix_ms = now;
        Ok(())
    })?;
    get_peer_at(root, node_id)
}

fn view_for_record(peer: FederationPeerRecord) -> AppResult<FederationPeerView> {
    let credential = peer_credential_status(&target_from(&peer))?;
    Ok(FederationPeerView { peer, credential })
}

fn target_from(peer: &FederationPeerRecord) -> FederationRemoteTarget {
    FederationRemoteTarget {
        node_id: peer.node_id.clone(),
        endpoint: peer.endpoint.clone(),
    }
}

fn find_peer_mut<'a>(
    registry: &'a mut FederationPeerRegistryFile,
    node_id: &str,
) -> AppResult<&'a mut FederationPeerRecord> {
    registry
        .peers
        .iter_mut()
        .find(|peer| peer.node_id == node_id)
        .ok_or_else(|| AppError::Message(format!("FEDERATION_PEER_NOT_FOUND: {node_id}")))
}

fn ensure_endpoint_unique(
    registry: &FederationPeerRegistryFile,
    endpoint: &str,
    except_node_id: Option<&str>,
) -> AppResult<()> {
    if registry
        .peers
        .iter()
        .any(|peer| peer.endpoint == endpoint && except_node_id != Some(peer.node_id.as_str()))
    {
        return Err(AppError::Message(format!(
            "FEDERATION_ENDPOINT_ALREADY_REGISTERED: endpoint {endpoint} is already pinned to another node"
        )));
    }
    Ok(())
}

fn reject_self_peer(node_id: &str) -> AppResult<()> {
    if crate::runtime::capability_snapshot(None)?.node.id == node_id {
        return Err(AppError::Message(
            "FEDERATION_SELF_PEER: the local node cannot be registered as its own peer".into(),
        ));
    }
    Ok(())
}

fn validate_node_selector(node_id: &str) -> AppResult<()> {
    if valid_node_id(node_id) {
        Ok(())
    } else {
        Err(AppError::Message(
            "FEDERATION_NODE_ID_INVALID: peer node id is invalid".into(),
        ))
    }
}

fn normalize_display_name(value: &str, fallback: &str) -> AppResult<String> {
    let trimmed = value.trim();
    let value = if trimmed.is_empty() {
        fallback
    } else {
        trimmed
    };
    if value.len() > MAX_DISPLAY_NAME_BYTES || value.chars().any(char::is_control) {
        return Err(AppError::Message(format!(
            "FEDERATION_PEER_NAME_INVALID: display name must be at most {MAX_DISPLAY_NAME_BYTES} bytes and contain no control characters"
        )));
    }
    Ok(value.to_string())
}

fn contract_drift_error(code: &str) -> bool {
    matches!(
        code,
        "FEDERATION_REMOTE_RESPONSE_MISMATCH"
            | "FEDERATION_REMOTE_RESULT_MISMATCH"
            | "FEDERATION_CONTRACT_MISMATCH"
            | "FEDERATION_RUNTIME_AUTHORITY_UNSUPPORTED"
            | "RUNTIME_CAPABILITY_MISMATCH"
            | "FEDERATION_NODE_ID_INVALID"
            | "FEDERATION_SELF_PEER"
            | "FEDERATION_NODE_DESCRIPTOR_SCOPED_TO_WORKSPACE"
            | "FEDERATION_TRANSPORT_UNSUPPORTED"
            | "FEDERATION_ACCESS_UNSUPPORTED"
            | "FEDERATION_ROUTE_LIMIT_EXCEEDED"
            | "FEDERATION_OPERATION_SET_INVALID"
            | "FEDERATION_ROUTE_NODE_MISMATCH"
            | "FEDERATION_ROUTE_INVALID"
            | "FEDERATION_ROUTE_WRITE_FORBIDDEN"
            | "FEDERATION_ROUTE_DUPLICATE"
            | "FEDERATION_CONTEXT_POLICY_UNSAFE"
    )
}

fn error_code(message: &str) -> String {
    let candidate = message
        .split(':')
        .next()
        .unwrap_or("FEDERATION_PEER_PROBE_FAILED");
    if candidate.starts_with("FEDERATION_") || candidate.starts_with("RUNTIME_") {
        candidate.chars().take(96).collect()
    } else {
        "FEDERATION_PEER_PROBE_FAILED".into()
    }
}

fn registry_path(root: &Path) -> PathBuf {
    root.join("data").join("federation-peers.json")
}

fn registry_lock_path(root: &Path) -> PathBuf {
    root.join("data").join(".federation-peers.lock")
}

fn load_registry_at(root: &Path) -> AppResult<FederationPeerRegistryFile> {
    let path = registry_path(root);
    if !path.exists() {
        return Ok(FederationPeerRegistryFile::default());
    }
    let raw = std::fs::read(&path)?;
    let registry: FederationPeerRegistryFile = serde_json::from_slice(&raw).map_err(|error| {
        AppError::Message(format!(
            "FEDERATION_PEER_REGISTRY_INVALID: cannot parse {}: {error}",
            path.display()
        ))
    })?;
    validate_registry(&registry)?;
    Ok(registry)
}

fn validate_registry(registry: &FederationPeerRegistryFile) -> AppResult<()> {
    if registry.schema_version != PEER_REGISTRY_SCHEMA_VERSION {
        return Err(AppError::Message(format!(
            "FEDERATION_PEER_REGISTRY_VERSION_UNSUPPORTED: {}",
            registry.schema_version
        )));
    }
    if registry.peers.len() > MAX_PEERS {
        return Err(AppError::Message(format!(
            "FEDERATION_PEER_LIMIT_EXCEEDED: maximum peer count is {MAX_PEERS}"
        )));
    }
    let mut node_ids = BTreeSet::new();
    let mut endpoints = BTreeSet::new();
    let mut bootstrap_fingerprints = BTreeSet::new();
    for peer in &registry.peers {
        validate_node_selector(&peer.node_id)?;
        let canonical = canonical_remote_target(&target_from(peer))?;
        if canonical.endpoint != peer.endpoint {
            return Err(AppError::Message(
                "FEDERATION_PEER_REGISTRY_INVALID: peer endpoint is not canonical".into(),
            ));
        }
        normalize_display_name(&peer.display_name, &peer.node_id)?;
        if peer.registered_at_unix_ms > peer.updated_at_unix_ms {
            return Err(AppError::Message(
                "FEDERATION_PEER_REGISTRY_INVALID: peer timestamps are inconsistent".into(),
            ));
        }
        if peer
            .last_probe_code
            .as_deref()
            .is_some_and(|code| code.len() > 96 || code.chars().any(char::is_control))
        {
            return Err(AppError::Message(
                "FEDERATION_PEER_REGISTRY_INVALID: last probe code is invalid".into(),
            ));
        }
        if !node_ids.insert(peer.node_id.clone()) || !endpoints.insert(peer.endpoint.clone()) {
            return Err(AppError::Message(
                "FEDERATION_PEER_REGISTRY_INVALID: duplicate peer node or endpoint".into(),
            ));
        }
        if !bootstrap_fingerprints.insert(peer.bootstrap_signing.fingerprint.clone()) {
            return Err(AppError::Message(
                "FEDERATION_PEER_REGISTRY_INVALID: one bootstrap signing key cannot identify multiple peer nodes"
                    .into(),
            ));
        }
        for digest in [
            Some(peer.bootstrap_descriptor_digest.as_str()),
            peer.last_descriptor_digest.as_deref(),
            peer.trusted_descriptor_digest.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err(AppError::Message(
                    "FEDERATION_PEER_REGISTRY_INVALID: descriptor digest is invalid".into(),
                ));
            }
        }
        if peer.last_descriptor_digest.is_some() != peer.last_signing.is_some() {
            return Err(AppError::Message(
                "FEDERATION_PEER_REGISTRY_INVALID: observed descriptor and signing identity must be persisted together"
                    .into(),
            ));
        }
        for signing in [
            Some(&peer.bootstrap_signing),
            peer.last_signing.as_ref(),
            peer.trusted_signing.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            super::signing::validate_public_signing_identity(signing).map_err(|_| {
                AppError::Message(
                    "FEDERATION_PEER_REGISTRY_INVALID: persisted signing identity is invalid"
                        .into(),
                )
            })?;
        }
        match peer.trust_status {
            FederationPeerTrustStatus::Trusted | FederationPeerTrustStatus::Drifted => {
                if peer.trusted_descriptor_digest.is_none() || peer.trusted_signing.is_none() {
                    return Err(AppError::Message(
                        "FEDERATION_PEER_REGISTRY_INVALID: trusted or drifted peer is missing its pinned descriptor or signing identity"
                            .into(),
                    ));
                }
                if peer.trusted_descriptor_digest.as_deref()
                    != Some(peer.bootstrap_descriptor_digest.as_str())
                    || peer.trusted_signing.as_ref() != Some(&peer.bootstrap_signing)
                {
                    return Err(AppError::Message(
                        "FEDERATION_PEER_REGISTRY_INVALID: trusted pins must originate from the active bootstrap pin"
                            .into(),
                    ));
                }
            }
            FederationPeerTrustStatus::Untrusted | FederationPeerTrustStatus::Revoked => {
                if peer.trusted_descriptor_digest.is_some() || peer.trusted_signing.is_some() {
                    return Err(AppError::Message(
                        "FEDERATION_PEER_REGISTRY_INVALID: untrusted or revoked peer must not retain trusted descriptor or signing pins"
                            .into(),
                    ));
                }
            }
        }
    }
    Ok(())
}

fn update_registry_at<T>(
    root: &Path,
    mutate: impl FnOnce(&mut FederationPeerRegistryFile) -> AppResult<T>,
) -> AppResult<T> {
    let data_dir = root.join("data");
    std::fs::create_dir_all(&data_dir)?;
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let lock = options.open(registry_lock_path(root))?;
    lock.lock_exclusive()?;
    let result = (|| {
        let mut registry = load_registry_at(root)?;
        let output = mutate(&mut registry)?;
        validate_registry(&registry)?;
        let mut bytes = serde_json::to_vec_pretty(&registry)?;
        bytes.push(b'\n');
        crate::data::atomic_write(&registry_path(root), &bytes)?;
        Ok(output)
    })();
    let _ = FileExt::unlock(&lock);
    result
}

fn unix_time_ms() -> AppResult<u64> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| {
            AppError::Message(format!("system clock is before Unix epoch: {error}"))
        })?;
    u64::try_from(duration.as_millis())
        .map_err(|_| AppError::Message("system time exceeds federation timestamp range".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signing(node_id: &str) -> FederationNodeSigningPublic {
        let root = tempfile::tempdir().expect("signing root");
        let descriptor = descriptor(node_id);
        super::super::signing::bootstrap_bundle_for_test(
            root.path(),
            &descriptor,
            unix_time_ms().expect("now"),
        )
        .expect("signed bootstrap")
        .signing
        .signer
    }

    fn target(root: &Path, node: char, endpoint: &str) -> FederationPeerRegistration {
        let node_id = format!("node_{}", node.to_string().repeat(32));
        let descriptor = descriptor(&node_id);
        let signing_root = root.join(format!("signer-{node}"));
        let bundle = super::super::signing::bootstrap_bundle_for_test(
            &signing_root,
            &descriptor,
            unix_time_ms().expect("now"),
        )
        .expect("bootstrap");
        FederationPeerRegistration {
            endpoint: endpoint.into(),
            display_name: format!("Node {node}"),
            bundle,
        }
    }

    fn descriptor(node_id: &str) -> FederationPeerDescriptor {
        FederationPeerDescriptor {
            schema_version: super::super::FEDERATION_SCHEMA_VERSION,
            contract: super::super::FEDERATION_CONTRACT.into(),
            runtime: crate::runtime::capability_snapshot_from_node(
                crate::runtime::NodeIdentity {
                    id: node_id.into(),
                    platform: "test".into(),
                    architecture: "test".into(),
                },
                None,
            ),
            access: super::super::FederationAccessMode::ReadOnly,
            operations: vec![
                super::super::FederationReadOperation::NodeCapabilities,
                super::super::FederationReadOperation::NodeControlStatus,
                super::super::FederationReadOperation::WorkspaceCatalog,
                super::super::FederationReadOperation::WorkspaceStatus,
            ],
            workspaces: vec![super::super::FederationWorkspaceRoute {
                node_id: node_id.into(),
                workspace_id: "0123456789abcdef0123456789abcdef".into(),
                display_name: "Workspace".into(),
                access: super::super::FederationAccessMode::ReadOnly,
            }],
            context_policy: super::super::FederationContextPolicy::default(),
        }
    }

    #[test]
    fn registry_is_local_file_with_canonical_unique_endpoints() {
        let root = tempfile::tempdir().expect("registry root");
        let first = register_peer_at(
            root.path(),
            &target(root.path(), 'b', "https://node-b.example/"),
        )
        .expect("register first");
        assert_eq!(first.peer.endpoint, "https://node-b.example");
        assert_eq!(
            first.peer.trust_status,
            FederationPeerTrustStatus::Untrusted
        );
        assert!(registry_path(root.path()).ends_with("data/federation-peers.json"));

        let duplicate = register_peer_at(
            root.path(),
            &target(root.path(), 'c', "https://node-b.example"),
        )
        .expect_err("same endpoint must not map to another node");
        assert!(duplicate
            .to_string()
            .contains("FEDERATION_ENDPOINT_ALREADY_REGISTERED"));
    }

    #[test]
    fn registry_rejects_one_bootstrap_signer_claiming_multiple_node_ids() {
        let registry_root = tempfile::tempdir().expect("registry root");
        let signing_root = tempfile::tempdir().expect("signing root");
        let now = unix_time_ms().expect("now");

        let node_b = "node_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let bundle_b = super::super::signing::bootstrap_bundle_for_test(
            signing_root.path(),
            &descriptor(node_b),
            now,
        )
        .expect("node b bootstrap");
        register_peer_at(
            registry_root.path(),
            &FederationPeerRegistration {
                endpoint: "https://node-b.example".into(),
                display_name: "Node B".into(),
                bundle: bundle_b,
            },
        )
        .expect("register node b");

        let node_c = "node_cccccccccccccccccccccccccccccccc";
        let bundle_c = super::super::signing::bootstrap_bundle_for_test(
            signing_root.path(),
            &descriptor(node_c),
            now,
        )
        .expect("node c bootstrap");
        assert!(register_peer_at(
            registry_root.path(),
            &FederationPeerRegistration {
                endpoint: "https://node-c.example".into(),
                display_name: "Node C".into(),
                bundle: bundle_c,
            },
        )
        .expect_err("one signing key must not identify two registered nodes")
        .to_string()
        .contains("FEDERATION_SIGNING_KEY_ALREADY_REGISTERED"));
    }

    #[test]
    fn endpoint_change_revokes_old_credentials_and_resets_trust_material() {
        let root = tempfile::tempdir().expect("registry root");
        let mut peer = register_peer_at(
            root.path(),
            &target(root.path(), 'b', "https://node-b.example"),
        )
        .expect("register");
        peer.peer.trust_status = FederationPeerTrustStatus::Trusted;
        peer.peer.trusted_descriptor_digest = Some(peer.peer.bootstrap_descriptor_digest.clone());
        peer.peer.trusted_signing = Some(peer.peer.bootstrap_signing.clone());
        update_registry_at(root.path(), |registry| {
            *find_peer_mut(registry, &peer.peer.node_id)? = peer.peer.clone();
            Ok(())
        })
        .expect("seed trusted state");

        let bootstrap = target(root.path(), 'b', "https://node-b-new.example").bundle;
        update_peer_at(
            root.path(),
            &FederationPeerUpdate {
                node_id: peer.peer.node_id.clone(),
                endpoint: Some("https://node-b-new.example".into()),
                display_name: None,
                bootstrap: Some(bootstrap),
            },
        )
        .expect("update endpoint metadata");
        let updated = load_registry_at(root.path())
            .expect("load updated registry")
            .peers
            .into_iter()
            .find(|record| record.node_id == peer.peer.node_id)
            .expect("updated peer");
        assert_eq!(updated.trust_status, FederationPeerTrustStatus::Untrusted);
        assert!(updated.trusted_descriptor_digest.is_none());
        assert_eq!(updated.credential_revision, 1);
    }

    #[test]
    fn candidate_inspection_never_persists_or_sends_credentials() {
        let local = crate::runtime::capability_snapshot_from_node(
            crate::runtime::NodeIdentity {
                id: "node_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
                platform: "test".into(),
                architecture: "test".into(),
            },
            None,
        );
        let descriptor = descriptor("node_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        let validation = validate_peer_descriptor(&local, &descriptor);
        assert!(validation.accepted);
        let root = tempfile::tempdir().expect("bootstrap root");
        let bundle = super::super::signing::bootstrap_bundle_for_test(
            root.path(),
            &descriptor,
            unix_time_ms().expect("now"),
        )
        .expect("bootstrap");
        let candidate = inspect_candidate("https://node-b.example", "Imported candidate", &bundle)
            .expect("candidate");
        assert_eq!(candidate.trust_status, FederationPeerTrustStatus::Untrusted);
        assert!(!candidate.persisted);
        assert!(!candidate.credential_sent);
        assert_eq!(candidate.signing, bundle.signing.signer);
    }

    #[test]
    fn trust_digest_ignores_workspace_catalog_churn_but_tracks_security_contract() {
        let mut descriptor = descriptor("node_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        let first = descriptor_trust_digest(&descriptor).expect("first digest");
        descriptor.workspaces.clear();
        let second = descriptor_trust_digest(&descriptor).expect("catalog digest");
        assert_eq!(first, second);
        descriptor.runtime.features.state_authority = "different_authority".into();
        let third = descriptor_trust_digest(&descriptor).expect("changed digest");
        assert_ne!(first, third);
    }

    #[test]
    fn trusted_peer_becomes_drifted_when_security_digest_changes() {
        let trusted_signing = signing("node_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        let mut record = FederationPeerRecord {
            node_id: "node_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into(),
            endpoint: "https://node-b.example".into(),
            display_name: "Node B".into(),
            bootstrap_descriptor_digest: "a".repeat(64),
            bootstrap_signing: trusted_signing.clone(),
            trust_status: FederationPeerTrustStatus::Trusted,
            registered_at_unix_ms: 10,
            updated_at_unix_ms: 20,
            credential_revision: 1,
            last_probe_at_unix_ms: Some(20),
            last_probe_code: Some("FEDERATION_PEER_PROBE_OK".into()),
            last_descriptor_digest: Some("a".repeat(64)),
            trusted_descriptor_digest: Some("a".repeat(64)),
            last_signing: Some(trusted_signing.clone()),
            trusted_signing: Some(trusted_signing.clone()),
        };
        apply_successful_probe(&mut record, &"b".repeat(64), &trusted_signing, 30);
        assert_eq!(record.trust_status, FederationPeerTrustStatus::Drifted);
        assert_eq!(
            record.last_probe_code.as_deref(),
            Some("FEDERATION_PEER_TRUST_DRIFT")
        );
        assert_eq!(
            record.trusted_descriptor_digest.as_deref(),
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        );
    }

    #[test]
    fn registry_rejects_unknown_fields_and_inconsistent_trust_state() {
        let bootstrap_signing = signing("node_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        let value = serde_json::json!({
            "schemaVersion": PEER_REGISTRY_SCHEMA_VERSION,
            "peers": [{
                "nodeId": "node_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                "endpoint": "https://node-b.example",
                "displayName": "Node B",
                "trustStatus": "trusted",
                "registeredAtUnixMs": 1,
                "updatedAtUnixMs": 1,
                "credentialRevision": 1,
                "unknown": true
            }]
        });
        assert!(serde_json::from_value::<FederationPeerRegistryFile>(value).is_err());

        let invalid = FederationPeerRegistryFile {
            schema_version: PEER_REGISTRY_SCHEMA_VERSION,
            peers: vec![FederationPeerRecord {
                node_id: "node_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into(),
                endpoint: "https://node-b.example".into(),
                display_name: "Node B".into(),
                bootstrap_descriptor_digest: "a".repeat(64),
                bootstrap_signing,
                trust_status: FederationPeerTrustStatus::Trusted,
                registered_at_unix_ms: 1,
                updated_at_unix_ms: 1,
                credential_revision: 1,
                last_probe_at_unix_ms: None,
                last_probe_code: None,
                last_descriptor_digest: None,
                trusted_descriptor_digest: None,
                last_signing: None,
                trusted_signing: None,
            }],
        };
        assert!(validate_registry(&invalid).is_err());
    }

    #[test]
    fn remote_read_gate_allows_only_trusted_registry_state() {
        let observed_signing = signing("node_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        let base = FederationPeerRecord {
            node_id: "node_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into(),
            endpoint: "https://node-b.example".into(),
            display_name: "Node B".into(),
            bootstrap_descriptor_digest: "a".repeat(64),
            bootstrap_signing: observed_signing.clone(),
            trust_status: FederationPeerTrustStatus::Untrusted,
            registered_at_unix_ms: 1,
            updated_at_unix_ms: 1,
            credential_revision: 1,
            last_probe_at_unix_ms: Some(1),
            last_probe_code: Some("FEDERATION_PEER_PROBE_OK".into()),
            last_descriptor_digest: Some("a".repeat(64)),
            trusted_descriptor_digest: None,
            last_signing: Some(observed_signing.clone()),
            trusted_signing: None,
        };
        assert!(ensure_read_trust(&base).is_err());

        let mut trusted = base.clone();
        trusted.trust_status = FederationPeerTrustStatus::Trusted;
        trusted.trusted_descriptor_digest = Some("a".repeat(64));
        trusted.trusted_signing = Some(observed_signing);
        ensure_read_trust(&trusted).expect("trusted read");

        let mut drifted = trusted.clone();
        drifted.trust_status = FederationPeerTrustStatus::Drifted;
        assert!(ensure_read_trust(&drifted)
            .expect_err("drifted read must fail")
            .to_string()
            .contains("FEDERATION_PEER_TRUST_DRIFT"));

        let mut revoked = base;
        revoked.trust_status = FederationPeerTrustStatus::Revoked;
        assert!(ensure_read_trust(&revoked).is_err());
    }
}
