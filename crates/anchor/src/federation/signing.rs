use std::fs::OpenOptions;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use fs2::FileExt;
use ring::rand::SystemRandom;
use ring::signature::{Ed25519KeyPair, KeyPair, UnparsedPublicKey, ED25519};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{AppError, AppResult};

use super::{
    valid_node_id, FederationPeerDescriptor, FEDERATION_CONTRACT, FEDERATION_SCHEMA_VERSION,
};

const LEGACY_SIGNING_IDENTITY_SCHEMA_VERSION: u16 = 1;
const SIGNING_IDENTITY_SCHEMA_VERSION: u16 = 2;
const SIGNING_ALGORITHM: &str = "ed25519";
const SIGNATURE_CONTRACT: &str = "anchor-node-signature-v1";
const BOOTSTRAP_SCHEMA_VERSION: u16 = 1;
const BOOTSTRAP_CONTRACT: &str = "anchor-federation-bootstrap-v1";
const BOOTSTRAP_DOMAIN: &str = "anchor:federation:bootstrap:v1";
pub(crate) const TRANSPORT_RESPONSE_DOMAIN: &str = "anchor:federation:transport-response:v1";
const BOOTSTRAP_TTL_MS: u64 = 30 * 60 * 1000;
const BOOTSTRAP_CLOCK_SKEW_MS: u64 = 5 * 60 * 1000;
const MAX_BOOTSTRAP_BYTES: usize = 512 * 1024;
const ED25519_PUBLIC_KEY_BYTES: usize = 32;
const ED25519_SIGNATURE_BYTES: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FederationNodeSigningPublic {
    pub contract: String,
    pub algorithm: String,
    pub key_epoch: u64,
    pub public_key_base64: String,
    pub fingerprint: String,
}

fn rotate_signing_identity_with_notice_at(
    root: &Path,
    descriptor: &FederationPeerDescriptor,
    now: u64,
) -> AppResult<FederationNodeSigningPublic> {
    let node_id = descriptor.runtime.node.id.as_str();
    if !valid_node_id(node_id) {
        return Err(AppError::Message(
            "FEDERATION_ROTATION_NODE_INVALID: rotation descriptor requires a valid node id".into(),
        ));
    }
    let data_dir = root.join("data");
    std::fs::create_dir_all(&data_dir)?;
    let lock_file = open_private_lock(&data_dir.join(".federation-signing.lock"))?;
    lock_file.lock_exclusive()?;
    let result = {
        let path = data_dir.join("federation-signing.json");
        let previous = if path.exists() {
            read_signing_identity(&path)?
        } else {
            create_signing_identity(&path, 1)?
        };
        let next_epoch = previous.public.key_epoch.saturating_add(1);
        if next_epoch == 0 {
            return Err(AppError::Message(
                "FEDERATION_SIGNING_EPOCH_INVALID: signing key epoch overflow".into(),
            ));
        }
        let next_record = generate_signing_record(next_epoch)?;
        let next = identity_from_record(next_record.clone())?;
        let bootstrap_descriptor_digest = super::registry::descriptor_trust_digest(descriptor)?;
        let material = RotationNoticeSigningMaterial {
            schema_version: ROTATION_NOTICE_SCHEMA_VERSION,
            contract: ROTATION_NOTICE_CONTRACT,
            issued_at_unix_ms: now,
            expires_at_unix_ms: now.saturating_add(ROTATION_NOTICE_TTL_MS),
            node_id,
            federation_contract: FEDERATION_CONTRACT,
            federation_schema_version: FEDERATION_SCHEMA_VERSION,
            previous_signing: &previous.public,
            next_signing: &next.public,
            bootstrap_descriptor_digest: &bootstrap_descriptor_digest,
        };
        let rotation_payload = serde_json::to_vec(&material)?;
        let notice = FederationSigningRotationNotice {
            schema_version: ROTATION_NOTICE_SCHEMA_VERSION,
            contract: ROTATION_NOTICE_CONTRACT.into(),
            issued_at_unix_ms: now,
            expires_at_unix_ms: now.saturating_add(ROTATION_NOTICE_TTL_MS),
            node_id: node_id.into(),
            federation_contract: FEDERATION_CONTRACT.into(),
            federation_schema_version: FEDERATION_SCHEMA_VERSION,
            previous_signing: previous.public.clone(),
            next_signing: next.public.clone(),
            bootstrap_descriptor_digest,
            signing: FederationDetachedSignature {
                signer: previous.public.clone(),
                signature_base64: BASE64.encode(sign_payload(
                    &previous.key_pair,
                    ROTATION_NOTICE_DOMAIN,
                    &rotation_payload,
                )),
            },
        };
        validate_rotation_notice_shape(&notice)?;
        let mut history = load_rotation_history_at(root)?;
        if history
            .last()
            .is_some_and(|previous_notice| previous_notice.next_signing != previous.public)
        {
            history.clear();
        }
        history.push(notice.clone());
        if history.len() > MAX_ROTATION_HISTORY_ENTRIES {
            history.drain(..history.len() - MAX_ROTATION_HISTORY_ENTRIES);
        }
        write_rotation_history(root, &history)?;
        let mut notice_bytes = serde_json::to_vec_pretty(&notice)?;
        notice_bytes.push(b'\n');
        crate::data::atomic_write(&rotation_notice_path(root), &notice_bytes)?;
        write_signing_record(&path, &next_record)?;
        Ok(next.public)
    };
    let _ = FileExt::unlock(&lock_file);
    result
}

pub fn verify_rotation_notice(
    notice: &FederationSigningRotationNotice,
    bootstrap: &FederationBootstrapBundle,
) -> AppResult<FederationNodeSigningPublic> {
    validate_rotation_notice_shape(notice)?;
    let now = unix_time_ms()?;
    validate_rotation_notice_time(notice, now)?;
    let bootstrap_signer = verify_bootstrap_bundle(bootstrap)?;
    if bootstrap.node_id != notice.node_id || bootstrap_signer != notice.next_signing {
        return Err(AppError::Message(
            "FEDERATION_ROTATION_BOOTSTRAP_MISMATCH: bootstrap signer does not match the announced next key"
                .into(),
        ));
    }
    let digest = super::registry::descriptor_trust_digest(&bootstrap.descriptor)?;
    if digest != notice.bootstrap_descriptor_digest {
        return Err(AppError::Message(
            "FEDERATION_ROTATION_BOOTSTRAP_MISMATCH: bootstrap security descriptor does not match the rotation notice"
                .into(),
        ));
    }
    Ok(notice.previous_signing.clone())
}

pub fn verify_rotation_chain(
    notices: &[FederationSigningRotationNotice],
    bootstrap: &FederationBootstrapBundle,
) -> AppResult<Option<FederationNodeSigningPublic>> {
    if notices.is_empty() {
        return Ok(None);
    }
    if notices.len() > MAX_ROTATION_HISTORY_ENTRIES {
        return Err(AppError::Message(format!(
            "FEDERATION_ROTATION_CHAIN_TOO_LONG: rotation chain exceeds {MAX_ROTATION_HISTORY_ENTRIES} entries"
        )));
    }
    let now = unix_time_ms()?;
    for notice in notices {
        validate_rotation_notice_shape(notice)?;
        validate_rotation_notice_time(notice, now)?;
    }
    for pair in notices.windows(2) {
        if pair[0].node_id != pair[1].node_id || pair[0].next_signing != pair[1].previous_signing {
            return Err(AppError::Message(
                "FEDERATION_ROTATION_CHAIN_INVALID: rotation notices do not form a contiguous signing chain"
                    .into(),
            ));
        }
    }
    let last = notices.last().expect("non-empty rotation chain");
    verify_rotation_notice(last, bootstrap)?;
    Ok(notices
        .first()
        .map(|notice| notice.previous_signing.clone()))
}

pub fn verify_rotation_chain_from(
    notices: &[FederationSigningRotationNotice],
    bootstrap: &FederationBootstrapBundle,
    expected_previous: &FederationNodeSigningPublic,
) -> AppResult<bool> {
    verify_rotation_chain(notices, bootstrap)?;
    Ok(notices
        .iter()
        .any(|notice| &notice.previous_signing == expected_previous))
}

fn validate_rotation_notice_time(
    notice: &FederationSigningRotationNotice,
    now: u64,
) -> AppResult<()> {
    if notice.issued_at_unix_ms > now.saturating_add(BOOTSTRAP_CLOCK_SKEW_MS)
        || notice.expires_at_unix_ms < now
    {
        return Err(AppError::Message(
            "FEDERATION_ROTATION_NOTICE_EXPIRED: rotation notice is outside the accepted time window"
                .into(),
        ));
    }
    Ok(())
}

fn validate_rotation_notice_shape(notice: &FederationSigningRotationNotice) -> AppResult<()> {
    let encoded = serde_json::to_vec(notice)?;
    if encoded.len() > MAX_ROTATION_NOTICE_BYTES {
        return Err(AppError::Message(format!(
            "FEDERATION_ROTATION_NOTICE_TOO_LARGE: rotation notice exceeds {MAX_ROTATION_NOTICE_BYTES} bytes"
        )));
    }
    if notice.schema_version != ROTATION_NOTICE_SCHEMA_VERSION
        || notice.contract != ROTATION_NOTICE_CONTRACT
        || notice.federation_contract != FEDERATION_CONTRACT
        || notice.federation_schema_version != FEDERATION_SCHEMA_VERSION
        || !valid_node_id(&notice.node_id)
        || notice.expires_at_unix_ms < notice.issued_at_unix_ms
        || notice
            .expires_at_unix_ms
            .saturating_sub(notice.issued_at_unix_ms)
            > ROTATION_NOTICE_TTL_MS
    {
        return Err(AppError::Message(
            "FEDERATION_ROTATION_NOTICE_INVALID: rotation notice contract or time window is invalid"
                .into(),
        ));
    }
    validate_public_signing_identity(&notice.previous_signing)?;
    validate_public_signing_identity(&notice.next_signing)?;
    if notice.previous_signing.contract != notice.next_signing.contract
        || notice.previous_signing.algorithm != notice.next_signing.algorithm
        || notice.next_signing.key_epoch != notice.previous_signing.key_epoch.saturating_add(1)
        || notice.previous_signing.fingerprint == notice.next_signing.fingerprint
        || notice.bootstrap_descriptor_digest.len() != 64
        || !notice
            .bootstrap_descriptor_digest
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
        || notice.signing.signer != notice.previous_signing
    {
        return Err(AppError::Message(
            "FEDERATION_ROTATION_NOTICE_INVALID: signing continuity or bootstrap digest is invalid"
                .into(),
        ));
    }
    let material = RotationNoticeSigningMaterial {
        schema_version: notice.schema_version,
        contract: &notice.contract,
        issued_at_unix_ms: notice.issued_at_unix_ms,
        expires_at_unix_ms: notice.expires_at_unix_ms,
        node_id: &notice.node_id,
        federation_contract: &notice.federation_contract,
        federation_schema_version: notice.federation_schema_version,
        previous_signing: &notice.previous_signing,
        next_signing: &notice.next_signing,
        bootstrap_descriptor_digest: &notice.bootstrap_descriptor_digest,
    };
    verify_detached_signature(
        &notice.previous_signing,
        ROTATION_NOTICE_DOMAIN,
        &serde_json::to_vec(&material)?,
        &notice.signing.signature_base64,
    )
}

pub fn local_rotation_history() -> AppResult<Vec<FederationSigningRotationNotice>> {
    let root = crate::platform::platform().app_config_dir()?;
    let history = load_rotation_history_at(&root)?;
    let now = unix_time_ms()?;
    let first_valid = history
        .iter()
        .position(|notice| validate_rotation_notice_time(notice, now).is_ok())
        .unwrap_or(history.len());
    Ok(history[first_valid..].to_vec())
}

const ROTATION_NOTICE_SCHEMA_VERSION: u16 = 1;
const ROTATION_NOTICE_CONTRACT: &str = "anchor-federation-key-rotation-v1";
const ROTATION_NOTICE_DOMAIN: &str = "anchor:federation:key-rotation:v1";
const ROTATION_NOTICE_TTL_MS: u64 = 30 * 24 * 60 * 60 * 1000;
const MAX_ROTATION_NOTICE_BYTES: usize = 64 * 1024;
const ROTATION_HISTORY_SCHEMA_VERSION: u16 = 1;
const MAX_ROTATION_HISTORY_ENTRIES: usize = 8;
const MAX_ROTATION_HISTORY_BYTES: usize = 512 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StoredRotationHistory {
    schema_version: u16,
    notices: Vec<FederationSigningRotationNotice>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FederationSigningRotationNotice {
    pub schema_version: u16,
    pub contract: String,
    pub issued_at_unix_ms: u64,
    pub expires_at_unix_ms: u64,
    pub node_id: String,
    pub federation_contract: String,
    pub federation_schema_version: u16,
    pub previous_signing: FederationNodeSigningPublic,
    pub next_signing: FederationNodeSigningPublic,
    pub bootstrap_descriptor_digest: String,
    pub signing: FederationDetachedSignature,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RotationNoticeSigningMaterial<'a> {
    schema_version: u16,
    contract: &'a str,
    issued_at_unix_ms: u64,
    expires_at_unix_ms: u64,
    node_id: &'a str,
    federation_contract: &'a str,
    federation_schema_version: u16,
    previous_signing: &'a FederationNodeSigningPublic,
    next_signing: &'a FederationNodeSigningPublic,
    bootstrap_descriptor_digest: &'a str,
}

pub fn rotate_local_signing_identity(
    descriptor: &FederationPeerDescriptor,
) -> AppResult<FederationLocalSigningStatus> {
    let root = crate::platform::platform().app_config_dir()?;
    let signing = rotate_signing_identity_with_notice_at(&root, descriptor, unix_time_ms()?)?;
    Ok(FederationLocalSigningStatus {
        node_id: crate::runtime::capability_snapshot(None)?.node.id,
        signing,
    })
}

pub fn local_signing_status() -> AppResult<FederationLocalSigningStatus> {
    Ok(FederationLocalSigningStatus {
        node_id: crate::runtime::capability_snapshot(None)?.node.id,
        signing: local_signing_public()?,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FederationLocalSigningStatus {
    pub node_id: String,
    pub signing: FederationNodeSigningPublic,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FederationDetachedSignature {
    pub signer: FederationNodeSigningPublic,
    pub signature_base64: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FederationBootstrapBundle {
    pub schema_version: u16,
    pub contract: String,
    pub issued_at_unix_ms: u64,
    pub expires_at_unix_ms: u64,
    pub node_id: String,
    pub federation_contract: String,
    pub federation_schema_version: u16,
    pub runtime_contract: String,
    pub runtime_schema_version: u16,
    pub descriptor_digest: String,
    pub descriptor: FederationPeerDescriptor,
    pub signing: FederationDetachedSignature,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StoredSigningIdentity {
    schema_version: u16,
    algorithm: String,
    key_epoch: u64,
    protection: String,
    protected_private_key_base64: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LegacyStoredSigningIdentity {
    schema_version: u16,
    algorithm: String,
    key_epoch: u64,
    private_key_pkcs8_base64: String,
}

struct LocalSigningIdentity {
    public: FederationNodeSigningPublic,
    key_pair: Ed25519KeyPair,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BootstrapSigningMaterial<'a> {
    schema_version: u16,
    contract: &'a str,
    issued_at_unix_ms: u64,
    expires_at_unix_ms: u64,
    node_id: &'a str,
    federation_contract: &'a str,
    federation_schema_version: u16,
    runtime_contract: &'a str,
    runtime_schema_version: u16,
    descriptor_digest: &'a str,
    descriptor: &'a FederationPeerDescriptor,
    signer: &'a FederationNodeSigningPublic,
}

pub fn local_signing_public() -> AppResult<FederationNodeSigningPublic> {
    let root = crate::platform::platform().app_config_dir()?;
    Ok(load_or_create_signing_identity_at(&root)?.public)
}

pub fn local_bootstrap_bundle(
    descriptor: &FederationPeerDescriptor,
) -> AppResult<FederationBootstrapBundle> {
    let identity =
        load_or_create_signing_identity_at(&crate::platform::platform().app_config_dir()?)?;
    let node_id = descriptor.runtime.node.id.as_str();
    let local_node_id = crate::runtime::capability_snapshot(None)?.node.id;
    if node_id != local_node_id {
        return Err(AppError::Message(
            "FEDERATION_BOOTSTRAP_NODE_MISMATCH: bootstrap descriptor must describe the local node"
                .into(),
        ));
    }
    build_bootstrap_bundle(&identity, descriptor, unix_time_ms()?)
}

fn build_bootstrap_bundle(
    identity: &LocalSigningIdentity,
    descriptor: &FederationPeerDescriptor,
    now: u64,
) -> AppResult<FederationBootstrapBundle> {
    let node_id = descriptor.runtime.node.id.as_str();
    let runtime = &descriptor.runtime;
    let descriptor_digest = full_descriptor_digest(descriptor)?;
    let material = BootstrapSigningMaterial {
        schema_version: BOOTSTRAP_SCHEMA_VERSION,
        contract: BOOTSTRAP_CONTRACT,
        issued_at_unix_ms: now,
        expires_at_unix_ms: now.saturating_add(BOOTSTRAP_TTL_MS),
        node_id,
        federation_contract: FEDERATION_CONTRACT,
        federation_schema_version: FEDERATION_SCHEMA_VERSION,
        runtime_contract: &runtime.contract,
        runtime_schema_version: runtime.schema_version,
        descriptor_digest: &descriptor_digest,
        descriptor,
        signer: &identity.public,
    };
    let payload = serde_json::to_vec(&material)?;
    let signature = sign_payload(&identity.key_pair, BOOTSTRAP_DOMAIN, &payload);
    let bundle = FederationBootstrapBundle {
        schema_version: BOOTSTRAP_SCHEMA_VERSION,
        contract: BOOTSTRAP_CONTRACT.into(),
        issued_at_unix_ms: now,
        expires_at_unix_ms: now.saturating_add(BOOTSTRAP_TTL_MS),
        node_id: node_id.into(),
        federation_contract: FEDERATION_CONTRACT.into(),
        federation_schema_version: FEDERATION_SCHEMA_VERSION,
        runtime_contract: runtime.contract.clone(),
        runtime_schema_version: runtime.schema_version,
        descriptor_digest,
        descriptor: descriptor.clone(),
        signing: FederationDetachedSignature {
            signer: identity.public.clone(),
            signature_base64: BASE64.encode(signature),
        },
    };
    let bytes = serde_json::to_vec(&bundle)?;
    if bytes.len() > MAX_BOOTSTRAP_BYTES {
        return Err(AppError::Message(format!(
            "FEDERATION_BOOTSTRAP_TOO_LARGE: bootstrap bundle exceeds {MAX_BOOTSTRAP_BYTES} bytes"
        )));
    }
    Ok(bundle)
}

#[cfg(test)]
pub(crate) fn bootstrap_bundle_for_test(
    root: &Path,
    descriptor: &FederationPeerDescriptor,
    now: u64,
) -> AppResult<FederationBootstrapBundle> {
    let identity = load_or_create_signing_identity_at(root)?;
    build_bootstrap_bundle(&identity, descriptor, now)
}

#[cfg(test)]
pub(crate) fn sign_transport_payload_for_test(
    root: &Path,
    payload: &[u8],
) -> AppResult<FederationDetachedSignature> {
    let identity = load_or_create_signing_identity_at(root)?;
    Ok(FederationDetachedSignature {
        signer: identity.public,
        signature_base64: BASE64.encode(sign_payload(
            &identity.key_pair,
            TRANSPORT_RESPONSE_DOMAIN,
            payload,
        )),
    })
}

pub fn verify_bootstrap_bundle(
    bundle: &FederationBootstrapBundle,
) -> AppResult<FederationNodeSigningPublic> {
    let encoded = serde_json::to_vec(bundle)?;
    if encoded.len() > MAX_BOOTSTRAP_BYTES {
        return Err(AppError::Message(format!(
            "FEDERATION_BOOTSTRAP_TOO_LARGE: bootstrap bundle exceeds {MAX_BOOTSTRAP_BYTES} bytes"
        )));
    }
    if bundle.schema_version != BOOTSTRAP_SCHEMA_VERSION
        || bundle.contract != BOOTSTRAP_CONTRACT
        || bundle.federation_contract != FEDERATION_CONTRACT
        || bundle.federation_schema_version != FEDERATION_SCHEMA_VERSION
    {
        return Err(AppError::Message(
            "FEDERATION_BOOTSTRAP_CONTRACT_MISMATCH: bootstrap contract is incompatible".into(),
        ));
    }
    if !valid_node_id(&bundle.node_id)
        || bundle.node_id != bundle.descriptor.runtime.node.id
        || bundle.runtime_contract != bundle.descriptor.runtime.contract
        || bundle.runtime_schema_version != bundle.descriptor.runtime.schema_version
    {
        return Err(AppError::Message(
            "FEDERATION_BOOTSTRAP_IDENTITY_MISMATCH: bootstrap identity does not match its descriptor"
                .into(),
        ));
    }
    let now = unix_time_ms()?;
    if bundle.issued_at_unix_ms > now.saturating_add(BOOTSTRAP_CLOCK_SKEW_MS)
        || bundle.expires_at_unix_ms < now
        || bundle.expires_at_unix_ms < bundle.issued_at_unix_ms
        || bundle
            .expires_at_unix_ms
            .saturating_sub(bundle.issued_at_unix_ms)
            > BOOTSTRAP_TTL_MS
    {
        return Err(AppError::Message(
            "FEDERATION_BOOTSTRAP_EXPIRED: bootstrap bundle is outside the accepted time window"
                .into(),
        ));
    }
    let digest = full_descriptor_digest(&bundle.descriptor)?;
    if digest != bundle.descriptor_digest {
        return Err(AppError::Message(
            "FEDERATION_BOOTSTRAP_DESCRIPTOR_TAMPERED: descriptor digest mismatch".into(),
        ));
    }
    validate_public_signing_identity(&bundle.signing.signer)?;
    let material = BootstrapSigningMaterial {
        schema_version: bundle.schema_version,
        contract: &bundle.contract,
        issued_at_unix_ms: bundle.issued_at_unix_ms,
        expires_at_unix_ms: bundle.expires_at_unix_ms,
        node_id: &bundle.node_id,
        federation_contract: &bundle.federation_contract,
        federation_schema_version: bundle.federation_schema_version,
        runtime_contract: &bundle.runtime_contract,
        runtime_schema_version: bundle.runtime_schema_version,
        descriptor_digest: &bundle.descriptor_digest,
        descriptor: &bundle.descriptor,
        signer: &bundle.signing.signer,
    };
    verify_detached_signature(
        &bundle.signing.signer,
        BOOTSTRAP_DOMAIN,
        &serde_json::to_vec(&material)?,
        &bundle.signing.signature_base64,
    )?;
    Ok(bundle.signing.signer.clone())
}

pub(crate) fn sign_transport_payload_with_signer<F>(
    build_payload: F,
) -> AppResult<FederationDetachedSignature>
where
    F: FnOnce(&FederationNodeSigningPublic) -> AppResult<Vec<u8>>,
{
    let identity =
        load_or_create_signing_identity_at(&crate::platform::platform().app_config_dir()?)?;
    let payload = build_payload(&identity.public)?;
    Ok(FederationDetachedSignature {
        signer: identity.public,
        signature_base64: BASE64.encode(sign_payload(
            &identity.key_pair,
            TRANSPORT_RESPONSE_DOMAIN,
            &payload,
        )),
    })
}

pub(crate) fn verify_transport_signature(
    signature: &FederationDetachedSignature,
    payload: &[u8],
) -> AppResult<FederationNodeSigningPublic> {
    validate_public_signing_identity(&signature.signer)?;
    verify_detached_signature(
        &signature.signer,
        TRANSPORT_RESPONSE_DOMAIN,
        payload,
        &signature.signature_base64,
    )?;
    Ok(signature.signer.clone())
}

fn load_or_create_signing_identity_at(root: &Path) -> AppResult<LocalSigningIdentity> {
    let data_dir = root.join("data");
    std::fs::create_dir_all(&data_dir)?;
    let lock_file = open_private_lock(&data_dir.join(".federation-signing.lock"))?;
    lock_file.lock_exclusive()?;
    let result = {
        let path = data_dir.join("federation-signing.json");
        if path.exists() {
            read_signing_identity(&path)
        } else {
            create_signing_identity(&path, 1)
        }
    };
    let _ = FileExt::unlock(&lock_file);
    result
}

#[cfg(test)]
fn rotate_signing_identity_at(root: &Path) -> AppResult<FederationNodeSigningPublic> {
    let data_dir = root.join("data");
    std::fs::create_dir_all(&data_dir)?;
    let lock_file = open_private_lock(&data_dir.join(".federation-signing.lock"))?;
    lock_file.lock_exclusive()?;
    let result = {
        let path = data_dir.join("federation-signing.json");
        let next_epoch = if path.exists() {
            read_signing_identity(&path)?
                .public
                .key_epoch
                .saturating_add(1)
        } else {
            1
        };
        if next_epoch == 0 {
            return Err(AppError::Message(
                "FEDERATION_SIGNING_EPOCH_INVALID: signing key epoch overflow".into(),
            ));
        }
        Ok(create_signing_identity(&path, next_epoch)?.public)
    };
    let _ = FileExt::unlock(&lock_file);
    result
}

fn create_signing_identity(path: &Path, key_epoch: u64) -> AppResult<LocalSigningIdentity> {
    let record = generate_signing_record(key_epoch)?;
    write_signing_record(path, &record)?;
    identity_from_record(record)
}

fn generate_signing_record(key_epoch: u64) -> AppResult<StoredSigningIdentity> {
    let rng = SystemRandom::new();
    let document = Ed25519KeyPair::generate_pkcs8(&rng).map_err(|_| {
        AppError::Message(
            "FEDERATION_SIGNING_KEY_GENERATION_FAILED: Ed25519 key generation failed".into(),
        )
    })?;
    let (protection, protected) = crate::data::protect_machine_secret_bytes(document.as_ref())
        .map_err(|error| {
            AppError::Message(format!("FEDERATION_SIGNING_KEY_PROTECTION_FAILED: {error}"))
        })?;
    Ok(StoredSigningIdentity {
        schema_version: SIGNING_IDENTITY_SCHEMA_VERSION,
        algorithm: SIGNING_ALGORITHM.into(),
        key_epoch,
        protection: protection.into(),
        protected_private_key_base64: BASE64.encode(protected),
    })
}

fn write_signing_record(path: &Path, record: &StoredSigningIdentity) -> AppResult<()> {
    let mut bytes = serde_json::to_vec_pretty(&record)?;
    bytes.push(b'\n');
    crate::data::atomic_write(path, &bytes)?;
    Ok(())
}

fn rotation_notice_path(root: &Path) -> std::path::PathBuf {
    root.join("data").join("federation-rotation-notice.json")
}

fn rotation_history_path(root: &Path) -> std::path::PathBuf {
    root.join("data").join("federation-rotation-history.json")
}

fn load_rotation_history_at(root: &Path) -> AppResult<Vec<FederationSigningRotationNotice>> {
    let history_path = rotation_history_path(root);
    let mut notices = if history_path.exists() {
        let raw = std::fs::read(&history_path)?;
        if raw.len() > MAX_ROTATION_HISTORY_BYTES {
            return Err(AppError::Message(format!(
                "FEDERATION_ROTATION_HISTORY_TOO_LARGE: {} exceeds {MAX_ROTATION_HISTORY_BYTES} bytes",
                history_path.display()
            )));
        }
        let stored: StoredRotationHistory = serde_json::from_slice(&raw).map_err(|error| {
            AppError::Message(format!(
                "FEDERATION_ROTATION_HISTORY_INVALID: cannot parse {}: {error}",
                history_path.display()
            ))
        })?;
        if stored.schema_version != ROTATION_HISTORY_SCHEMA_VERSION
            || stored.notices.len() > MAX_ROTATION_HISTORY_ENTRIES
        {
            return Err(AppError::Message(
                "FEDERATION_ROTATION_HISTORY_INVALID: unsupported schema or entry count".into(),
            ));
        }
        stored.notices
    } else {
        let legacy_path = rotation_notice_path(root);
        if !legacy_path.exists() {
            Vec::new()
        } else {
            let raw = std::fs::read(&legacy_path)?;
            let notice: FederationSigningRotationNotice =
                serde_json::from_slice(&raw).map_err(|error| {
                    AppError::Message(format!(
                        "FEDERATION_ROTATION_NOTICE_INVALID: cannot parse {}: {error}",
                        legacy_path.display()
                    ))
                })?;
            vec![notice]
        }
    };
    for notice in &notices {
        validate_rotation_notice_shape(notice)?;
    }
    for pair in notices.windows(2) {
        if pair[0].node_id != pair[1].node_id || pair[0].next_signing != pair[1].previous_signing {
            return Err(AppError::Message(
                "FEDERATION_ROTATION_HISTORY_INVALID: stored notices are not contiguous".into(),
            ));
        }
    }
    if notices.len() > MAX_ROTATION_HISTORY_ENTRIES {
        notices.drain(..notices.len() - MAX_ROTATION_HISTORY_ENTRIES);
    }
    Ok(notices)
}

fn write_rotation_history(
    root: &Path,
    notices: &[FederationSigningRotationNotice],
) -> AppResult<()> {
    let stored = StoredRotationHistory {
        schema_version: ROTATION_HISTORY_SCHEMA_VERSION,
        notices: notices.to_vec(),
    };
    let mut bytes = serde_json::to_vec_pretty(&stored)?;
    if bytes.len() > MAX_ROTATION_HISTORY_BYTES {
        return Err(AppError::Message(format!(
            "FEDERATION_ROTATION_HISTORY_TOO_LARGE: rotation history exceeds {MAX_ROTATION_HISTORY_BYTES} bytes"
        )));
    }
    bytes.push(b'\n');
    crate::data::atomic_write(&rotation_history_path(root), &bytes)
}

fn read_signing_identity(path: &Path) -> AppResult<LocalSigningIdentity> {
    let raw = std::fs::read(path)?;
    let value: serde_json::Value = serde_json::from_slice(&raw).map_err(|error| {
        AppError::Message(format!(
            "FEDERATION_SIGNING_KEY_INVALID: could not parse {}: {error}",
            path.display()
        ))
    })?;
    let schema_version = value
        .get("schemaVersion")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| {
            AppError::Message(
                "FEDERATION_SIGNING_KEY_INVALID: signing identity schemaVersion is missing".into(),
            )
        })? as u16;
    match schema_version {
        SIGNING_IDENTITY_SCHEMA_VERSION => {
            let record =
                serde_json::from_value::<StoredSigningIdentity>(value).map_err(|error| {
                    AppError::Message(format!(
                        "FEDERATION_SIGNING_KEY_INVALID: could not parse {}: {error}",
                        path.display()
                    ))
                })?;
            identity_from_record(record)
        }
        LEGACY_SIGNING_IDENTITY_SCHEMA_VERSION => {
            let legacy =
                serde_json::from_value::<LegacyStoredSigningIdentity>(value).map_err(|error| {
                    AppError::Message(format!(
                        "FEDERATION_SIGNING_KEY_INVALID: could not parse legacy {}: {error}",
                        path.display()
                    ))
                })?;
            migrate_legacy_signing_identity(path, legacy)
        }
        version => Err(AppError::Message(format!(
            "FEDERATION_SIGNING_KEY_INVALID: unsupported signing identity schema version {version}"
        ))),
    }
}

fn identity_from_record(record: StoredSigningIdentity) -> AppResult<LocalSigningIdentity> {
    if record.schema_version != SIGNING_IDENTITY_SCHEMA_VERSION
        || record.algorithm != SIGNING_ALGORITHM
        || record.key_epoch == 0
    {
        return Err(AppError::Message(
            "FEDERATION_SIGNING_KEY_INVALID: unsupported signing identity record".into(),
        ));
    }
    let protected = BASE64
        .decode(record.protected_private_key_base64.as_bytes())
        .map_err(|_| {
            AppError::Message(
                "FEDERATION_SIGNING_KEY_INVALID: protected private key is not valid base64".into(),
            )
        })?;
    let private = crate::data::unprotect_machine_secret_bytes(&record.protection, &protected)
        .map_err(|error| {
            AppError::Message(format!(
                "FEDERATION_SIGNING_KEY_INVALID: private key protection cannot be opened: {error}"
            ))
        })?;
    identity_from_private_key(record.key_epoch, &private)
}

fn migrate_legacy_signing_identity(
    path: &Path,
    legacy: LegacyStoredSigningIdentity,
) -> AppResult<LocalSigningIdentity> {
    if legacy.schema_version != LEGACY_SIGNING_IDENTITY_SCHEMA_VERSION
        || legacy.algorithm != SIGNING_ALGORITHM
        || legacy.key_epoch == 0
    {
        return Err(AppError::Message(
            "FEDERATION_SIGNING_KEY_INVALID: unsupported legacy signing identity record".into(),
        ));
    }
    let private = BASE64
        .decode(legacy.private_key_pkcs8_base64.as_bytes())
        .map_err(|_| {
            AppError::Message(
                "FEDERATION_SIGNING_KEY_INVALID: legacy private key is not valid base64".into(),
            )
        })?;
    let identity = identity_from_private_key(legacy.key_epoch, &private)?;
    let (protection, protected) =
        crate::data::protect_machine_secret_bytes(&private).map_err(|error| {
            AppError::Message(format!("FEDERATION_SIGNING_KEY_PROTECTION_FAILED: {error}"))
        })?;
    let migrated = StoredSigningIdentity {
        schema_version: SIGNING_IDENTITY_SCHEMA_VERSION,
        algorithm: SIGNING_ALGORITHM.into(),
        key_epoch: legacy.key_epoch,
        protection: protection.into(),
        protected_private_key_base64: BASE64.encode(protected),
    };
    write_signing_record(path, &migrated)?;
    Ok(identity)
}

fn identity_from_private_key(key_epoch: u64, private: &[u8]) -> AppResult<LocalSigningIdentity> {
    let key_pair = Ed25519KeyPair::from_pkcs8(private).map_err(|_| {
        AppError::Message(
            "FEDERATION_SIGNING_KEY_INVALID: private key is not valid Ed25519 PKCS#8".into(),
        )
    })?;
    let public_key = key_pair.public_key().as_ref();
    let public = FederationNodeSigningPublic {
        contract: SIGNATURE_CONTRACT.into(),
        algorithm: SIGNING_ALGORITHM.into(),
        key_epoch,
        public_key_base64: BASE64.encode(public_key),
        fingerprint: public_key_fingerprint(public_key),
    };
    Ok(LocalSigningIdentity { public, key_pair })
}

pub(crate) fn validate_public_signing_identity(
    value: &FederationNodeSigningPublic,
) -> AppResult<Vec<u8>> {
    if value.contract != SIGNATURE_CONTRACT
        || value.algorithm != SIGNING_ALGORITHM
        || value.key_epoch == 0
    {
        return Err(AppError::Message(
            "FEDERATION_SIGNER_INVALID: unsupported signer contract, algorithm, or epoch".into(),
        ));
    }
    let public_key = BASE64
        .decode(value.public_key_base64.as_bytes())
        .map_err(|_| {
            AppError::Message("FEDERATION_SIGNER_INVALID: public key is not valid base64".into())
        })?;
    if public_key.len() != ED25519_PUBLIC_KEY_BYTES
        || value.fingerprint != public_key_fingerprint(&public_key)
    {
        return Err(AppError::Message(
            "FEDERATION_SIGNER_INVALID: public key fingerprint mismatch".into(),
        ));
    }
    Ok(public_key)
}

fn verify_detached_signature(
    signer: &FederationNodeSigningPublic,
    domain: &str,
    payload: &[u8],
    signature_base64: &str,
) -> AppResult<()> {
    let public_key = validate_public_signing_identity(signer)?;
    let signature = BASE64.decode(signature_base64.as_bytes()).map_err(|_| {
        AppError::Message("FEDERATION_SIGNATURE_INVALID: signature is not valid base64".into())
    })?;
    if signature.len() != ED25519_SIGNATURE_BYTES {
        return Err(AppError::Message(
            "FEDERATION_SIGNATURE_INVALID: Ed25519 signature must be 64 bytes".into(),
        ));
    }
    let message = domain_separated_message(domain, payload);
    UnparsedPublicKey::new(&ED25519, public_key)
        .verify(&message, &signature)
        .map_err(|_| {
            AppError::Message(
                "FEDERATION_SIGNATURE_INVALID: Ed25519 signature verification failed".into(),
            )
        })
}

fn sign_payload(key_pair: &Ed25519KeyPair, domain: &str, payload: &[u8]) -> Vec<u8> {
    key_pair
        .sign(&domain_separated_message(domain, payload))
        .as_ref()
        .to_vec()
}

fn domain_separated_message(domain: &str, payload: &[u8]) -> Vec<u8> {
    let mut message = Vec::with_capacity(domain.len() + 1 + payload.len());
    message.extend_from_slice(domain.as_bytes());
    message.push(0);
    message.extend_from_slice(payload);
    message
}

fn full_descriptor_digest(descriptor: &FederationPeerDescriptor) -> AppResult<String> {
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(descriptor)?)
    ))
}

fn public_key_fingerprint(public_key: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(public_key))
}

fn open_private_lock(path: &Path) -> AppResult<std::fs::File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    Ok(options.open(path)?)
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

    fn local_descriptor() -> FederationPeerDescriptor {
        super::super::local_peer_descriptor(&[]).expect("local federation descriptor")
    }

    #[test]
    fn signing_identity_is_stable_and_rotation_changes_only_key_epoch_and_key() {
        let root = tempfile::tempdir().expect("signing root");
        let first = load_or_create_signing_identity_at(root.path()).expect("first identity");
        let second = load_or_create_signing_identity_at(root.path()).expect("second identity");
        assert_eq!(first.public, second.public);
        let stored: serde_json::Value = serde_json::from_slice(
            &std::fs::read(root.path().join("data/federation-signing.json"))
                .expect("stored signing identity"),
        )
        .expect("stored signing json");
        assert_eq!(
            stored["schemaVersion"],
            serde_json::json!(SIGNING_IDENTITY_SCHEMA_VERSION)
        );
        assert!(stored.get("privateKeyPkcs8Base64").is_none());
        assert!(stored.get("protectedPrivateKeyBase64").is_some());
        #[cfg(windows)]
        assert_eq!(stored["protection"], "windows-dpapi-local-machine-v1");
        #[cfg(not(windows))]
        assert_eq!(stored["protection"], "private-file-permissions-v1");
        let rotated = rotate_signing_identity_at(root.path()).expect("rotate");
        assert_eq!(rotated.key_epoch, first.public.key_epoch + 1);
        assert_ne!(rotated.fingerprint, first.public.fingerprint);
        assert_ne!(rotated.public_key_base64, first.public.public_key_base64);
    }

    #[test]
    fn legacy_plaintext_signing_record_migrates_without_rotating_identity() {
        let root = tempfile::tempdir().expect("signing root");
        let data_dir = root.path().join("data");
        std::fs::create_dir_all(&data_dir).expect("data dir");
        let rng = SystemRandom::new();
        let document = Ed25519KeyPair::generate_pkcs8(&rng).expect("legacy private key");
        let expected = identity_from_private_key(7, document.as_ref())
            .expect("legacy identity")
            .public;
        let legacy = LegacyStoredSigningIdentity {
            schema_version: LEGACY_SIGNING_IDENTITY_SCHEMA_VERSION,
            algorithm: SIGNING_ALGORITHM.into(),
            key_epoch: 7,
            private_key_pkcs8_base64: BASE64.encode(document.as_ref()),
        };
        let path = data_dir.join("federation-signing.json");
        crate::data::atomic_write(
            &path,
            &serde_json::to_vec_pretty(&legacy).expect("legacy json"),
        )
        .expect("write legacy signing identity");

        let migrated = read_signing_identity(&path).expect("migrate legacy identity");
        assert_eq!(migrated.public, expected);
        let stored: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).expect("migrated file"))
                .expect("migrated json");
        assert_eq!(
            stored["schemaVersion"],
            serde_json::json!(SIGNING_IDENTITY_SCHEMA_VERSION)
        );
        assert_eq!(stored["keyEpoch"], 7);
        assert!(stored.get("privateKeyPkcs8Base64").is_none());
        let reopened = read_signing_identity(&path).expect("reopen migrated identity");
        assert_eq!(reopened.public, expected);
    }

    #[test]
    fn rotation_notice_is_signed_by_previous_key_and_binds_next_bootstrap() {
        let root = tempfile::tempdir().expect("signing root");
        let descriptor = local_descriptor();
        let now = unix_time_ms().expect("now");
        let previous = load_or_create_signing_identity_at(root.path()).expect("previous identity");
        let next = rotate_signing_identity_with_notice_at(root.path(), &descriptor, now)
            .expect("signed rotation");
        assert_eq!(next.key_epoch, previous.public.key_epoch + 1);

        let notice_raw = std::fs::read(rotation_notice_path(root.path())).expect("rotation notice");
        let notice: FederationSigningRotationNotice =
            serde_json::from_slice(&notice_raw).expect("parse rotation notice");
        let current = load_or_create_signing_identity_at(root.path()).expect("current identity");
        let bootstrap = build_bootstrap_bundle(&current, &descriptor, now + 1).expect("bootstrap");
        let signer = verify_rotation_notice(&notice, &bootstrap).expect("rotation continuity");
        assert_eq!(signer, previous.public);
        assert_eq!(notice.next_signing, next);

        let mut tampered = notice;
        tampered.next_signing.fingerprint = "sha256:deadbeef".into();
        assert!(verify_rotation_notice(&tampered, &bootstrap).is_err());
    }

    #[test]
    fn bounded_rotation_history_verifies_multi_hop_continuity() {
        let root = tempfile::tempdir().expect("signing root");
        let descriptor = local_descriptor();
        let now = unix_time_ms().expect("now");
        let origin = load_or_create_signing_identity_at(root.path())
            .expect("origin identity")
            .public;
        for offset in 0..3 {
            rotate_signing_identity_with_notice_at(root.path(), &descriptor, now + offset)
                .expect("rotation");
        }
        let history = load_rotation_history_at(root.path()).expect("rotation history");
        assert_eq!(history.len(), 3);
        let current = load_or_create_signing_identity_at(root.path()).expect("current identity");
        let bootstrap =
            build_bootstrap_bundle(&current, &descriptor, now + 4).expect("current bootstrap");
        assert_eq!(
            verify_rotation_chain(&history, &bootstrap).expect("rotation chain"),
            Some(origin)
        );
        assert!(
            verify_rotation_chain_from(&history, &bootstrap, &history[1].previous_signing)
                .expect("intermediate retained key must verify to current bootstrap")
        );

        let mut reordered = history.clone();
        reordered.swap(0, 1);
        assert!(verify_rotation_chain(&reordered, &bootstrap).is_err());
    }

    #[test]
    fn rotation_history_is_bounded_and_remains_contiguous() {
        let root = tempfile::tempdir().expect("signing root");
        let descriptor = local_descriptor();
        let now = unix_time_ms().expect("now");
        load_or_create_signing_identity_at(root.path()).expect("origin identity");
        for offset in 0..(MAX_ROTATION_HISTORY_ENTRIES + 2) {
            rotate_signing_identity_with_notice_at(root.path(), &descriptor, now + offset as u64)
                .expect("rotation");
        }
        let history = load_rotation_history_at(root.path()).expect("rotation history");
        assert_eq!(history.len(), MAX_ROTATION_HISTORY_ENTRIES);
        for pair in history.windows(2) {
            assert_eq!(pair[0].next_signing, pair[1].previous_signing);
        }
        let current = load_or_create_signing_identity_at(root.path()).expect("current identity");
        let bootstrap = build_bootstrap_bundle(
            &current,
            &descriptor,
            now + MAX_ROTATION_HISTORY_ENTRIES as u64 + 3,
        )
        .expect("bootstrap");
        verify_rotation_chain(&history, &bootstrap).expect("bounded chain");
    }

    #[test]
    fn detached_signature_rejects_tampered_payload() {
        let root = tempfile::tempdir().expect("signing root");
        let identity = load_or_create_signing_identity_at(root.path()).expect("identity");
        let payload = b"signed payload";
        let signature = BASE64.encode(sign_payload(
            &identity.key_pair,
            TRANSPORT_RESPONSE_DOMAIN,
            payload,
        ));
        verify_detached_signature(
            &identity.public,
            TRANSPORT_RESPONSE_DOMAIN,
            payload,
            &signature,
        )
        .expect("valid signature");
        assert!(verify_detached_signature(
            &identity.public,
            TRANSPORT_RESPONSE_DOMAIN,
            b"tampered payload",
            &signature,
        )
        .is_err());
    }

    #[test]
    fn bootstrap_bundle_rejects_descriptor_tamper_and_expiry() {
        let root = tempfile::tempdir().expect("signing root");
        let now = unix_time_ms().expect("now");
        let descriptor = local_descriptor();
        let bundle = bootstrap_bundle_for_test(root.path(), &descriptor, now).expect("bootstrap");
        verify_bootstrap_bundle(&bundle).expect("valid bootstrap");

        let mut tampered = bundle.clone();
        tampered
            .descriptor
            .workspaces
            .push(super::super::FederationWorkspaceRoute {
                node_id: tampered.node_id.clone(),
                workspace_id: "0123456789abcdef0123456789abcdef".into(),
                display_name: "tampered".into(),
                access: super::super::FederationAccessMode::ReadOnly,
            });
        assert!(verify_bootstrap_bundle(&tampered)
            .expect_err("descriptor tamper must fail")
            .to_string()
            .contains("FEDERATION_BOOTSTRAP_DESCRIPTOR_TAMPERED"));

        let expired = build_bootstrap_bundle(
            &load_or_create_signing_identity_at(root.path()).expect("identity"),
            &descriptor,
            now.saturating_sub(BOOTSTRAP_TTL_MS + BOOTSTRAP_CLOCK_SKEW_MS + 1),
        )
        .expect("expired bootstrap fixture");
        assert!(verify_bootstrap_bundle(&expired)
            .expect_err("expired bootstrap must fail")
            .to_string()
            .contains("FEDERATION_BOOTSTRAP_EXPIRED"));
    }
}
