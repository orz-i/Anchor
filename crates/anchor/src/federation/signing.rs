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

const SIGNING_IDENTITY_SCHEMA_VERSION: u16 = 1;
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

pub fn rotate_local_signing_identity() -> AppResult<FederationLocalSigningStatus> {
    Ok(FederationLocalSigningStatus {
        node_id: crate::runtime::capability_snapshot(None)?.node.id,
        signing: rotate_local_signing_key()?,
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

pub fn rotate_local_signing_key() -> AppResult<FederationNodeSigningPublic> {
    let root = crate::platform::platform().app_config_dir()?;
    rotate_signing_identity_at(&root)
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
    let rng = SystemRandom::new();
    let document = Ed25519KeyPair::generate_pkcs8(&rng).map_err(|_| {
        AppError::Message(
            "FEDERATION_SIGNING_KEY_GENERATION_FAILED: Ed25519 key generation failed".into(),
        )
    })?;
    let record = StoredSigningIdentity {
        schema_version: SIGNING_IDENTITY_SCHEMA_VERSION,
        algorithm: SIGNING_ALGORITHM.into(),
        key_epoch,
        private_key_pkcs8_base64: BASE64.encode(document.as_ref()),
    };
    let mut bytes = serde_json::to_vec_pretty(&record)?;
    bytes.push(b'\n');
    crate::data::atomic_write(path, &bytes)?;
    identity_from_record(record)
}

fn read_signing_identity(path: &Path) -> AppResult<LocalSigningIdentity> {
    let raw = std::fs::read(path)?;
    let record: StoredSigningIdentity = serde_json::from_slice(&raw).map_err(|error| {
        AppError::Message(format!(
            "FEDERATION_SIGNING_KEY_INVALID: could not parse {}: {error}",
            path.display()
        ))
    })?;
    identity_from_record(record)
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
    let private = BASE64
        .decode(record.private_key_pkcs8_base64.as_bytes())
        .map_err(|_| {
            AppError::Message(
                "FEDERATION_SIGNING_KEY_INVALID: private key is not valid base64".into(),
            )
        })?;
    let key_pair = Ed25519KeyPair::from_pkcs8(&private).map_err(|_| {
        AppError::Message(
            "FEDERATION_SIGNING_KEY_INVALID: private key is not valid Ed25519 PKCS#8".into(),
        )
    })?;
    let public_key = key_pair.public_key().as_ref();
    let public = FederationNodeSigningPublic {
        contract: SIGNATURE_CONTRACT.into(),
        algorithm: SIGNING_ALGORITHM.into(),
        key_epoch: record.key_epoch,
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
        let rotated = rotate_signing_identity_at(root.path()).expect("rotate");
        assert_eq!(rotated.key_epoch, first.public.key_epoch + 1);
        assert_ne!(rotated.fingerprint, first.public.fingerprint);
        assert_ne!(rotated.public_key_base64, first.public.public_key_base64);
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
