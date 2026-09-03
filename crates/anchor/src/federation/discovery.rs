use futures_util::StreamExt;
use serde::{Deserialize, Serialize};

use crate::error::{AppError, AppResult};
use crate::workspace::WorkspaceProfile;

use super::{
    canonical_federation_endpoint, latest_local_rotation_notice, local_bootstrap_bundle,
    local_peer_descriptor, verify_bootstrap_bundle, verify_rotation_notice,
    FederationBootstrapBundle, FederationNodeSigningPublic, FederationSigningRotationNotice,
    TRANSPORT_CONNECT_TIMEOUT, TRANSPORT_REQUEST_TIMEOUT,
};

const DISCOVERY_SCHEMA_VERSION: u16 = 1;
const DISCOVERY_CONTRACT: &str = "anchor-federation-discovery-v1";
pub(crate) const FEDERATION_MAX_DISCOVERY_BYTES: usize = 768 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FederationDiscoveryDocument {
    pub schema_version: u16,
    pub contract: String,
    pub node_id: String,
    pub bootstrap: FederationBootstrapBundle,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotation: Option<FederationSigningRotationNotice>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FederationDiscoveryState {
    Current,
    DescriptorDrift,
    RotationAvailable,
    IdentityDrift,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FederationDiscoveryInspection {
    pub node_id: String,
    pub endpoint: String,
    pub state: FederationDiscoveryState,
    pub discovered_signing: FederationNodeSigningPublic,
    pub descriptor_digest: String,
    pub continuity_verified: bool,
    pub discovery: FederationDiscoveryDocument,
}

pub fn local_discovery_document(
    profiles: &[WorkspaceProfile],
) -> AppResult<FederationDiscoveryDocument> {
    let descriptor = public_discovery_descriptor(local_peer_descriptor(profiles)?);
    let bootstrap = local_bootstrap_bundle(&descriptor)?;
    let rotation = latest_local_rotation_notice()?
        .filter(|notice| verify_rotation_notice(notice, &bootstrap).is_ok());
    let document = FederationDiscoveryDocument {
        schema_version: DISCOVERY_SCHEMA_VERSION,
        contract: DISCOVERY_CONTRACT.into(),
        node_id: bootstrap.node_id.clone(),
        bootstrap,
        rotation,
    };
    validate_discovery_document(&document, Some(&document.node_id))?;
    Ok(document)
}

fn public_discovery_descriptor(
    mut descriptor: super::FederationPeerDescriptor,
) -> super::FederationPeerDescriptor {
    // Discovery is intentionally unauthenticated public metadata. Workspace routes remain behind
    // the authenticated federation read transport and must not be leaked by bootstrap discovery.
    descriptor.workspaces.clear();
    descriptor
}

pub fn validate_discovery_document(
    document: &FederationDiscoveryDocument,
    expected_node_id: Option<&str>,
) -> AppResult<FederationNodeSigningPublic> {
    let encoded = serde_json::to_vec(document)?;
    if encoded.len() > FEDERATION_MAX_DISCOVERY_BYTES {
        return Err(AppError::Message(format!(
            "FEDERATION_DISCOVERY_TOO_LARGE: discovery document exceeds {FEDERATION_MAX_DISCOVERY_BYTES} bytes"
        )));
    }
    if document.schema_version != DISCOVERY_SCHEMA_VERSION
        || document.contract != DISCOVERY_CONTRACT
        || document.node_id != document.bootstrap.node_id
    {
        return Err(AppError::Message(
            "FEDERATION_DISCOVERY_INVALID: discovery identity or contract is invalid".into(),
        ));
    }
    if expected_node_id.is_some_and(|expected| expected != document.node_id) {
        return Err(AppError::Message(
            "FEDERATION_DISCOVERY_NODE_MISMATCH: discovery document belongs to a different node"
                .into(),
        ));
    }
    let signer = verify_bootstrap_bundle(&document.bootstrap)?;
    if let Some(rotation) = document.rotation.as_ref() {
        verify_rotation_notice(rotation, &document.bootstrap)?;
    }
    Ok(signer)
}

pub async fn fetch_discovery_document(
    endpoint: &str,
    expected_node_id: Option<&str>,
) -> AppResult<FederationDiscoveryDocument> {
    let origin = canonical_federation_endpoint(endpoint)?;
    let url = origin
        .join("federation/v2/bootstrap")
        .map_err(|error| AppError::Message(format!("invalid federation endpoint: {error}")))?;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .connect_timeout(TRANSPORT_CONNECT_TIMEOUT)
        .timeout(TRANSPORT_REQUEST_TIMEOUT)
        .build()
        .map_err(|error| {
            AppError::Message(format!(
                "failed to build federation discovery client: {error}"
            ))
        })?;
    let response = client
        .get(url)
        .header("accept", "application/json")
        .send()
        .await
        .map_err(|error| {
            AppError::Message(format!(
                "FEDERATION_DISCOVERY_UNAVAILABLE: bootstrap discovery request failed: {error}"
            ))
        })?;
    if !response.status().is_success() {
        return Err(AppError::Message(format!(
            "FEDERATION_DISCOVERY_REJECTED: discovery endpoint returned HTTP {}",
            response.status().as_u16()
        )));
    }
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !content_type.starts_with("application/json") {
        return Err(AppError::Message(
            "FEDERATION_DISCOVERY_RESPONSE_INVALID: discovery endpoint must return application/json"
                .into(),
        ));
    }
    if response
        .content_length()
        .is_some_and(|length| length > FEDERATION_MAX_DISCOVERY_BYTES as u64)
    {
        return Err(AppError::Message(format!(
            "FEDERATION_DISCOVERY_TOO_LARGE: discovery response exceeds {FEDERATION_MAX_DISCOVERY_BYTES} bytes"
        )));
    }
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| {
            AppError::Message(format!(
                "FEDERATION_DISCOVERY_RESPONSE_FAILED: failed while reading discovery response: {error}"
            ))
        })?;
        if body.len().saturating_add(chunk.len()) > FEDERATION_MAX_DISCOVERY_BYTES {
            return Err(AppError::Message(format!(
                "FEDERATION_DISCOVERY_TOO_LARGE: discovery response exceeds {FEDERATION_MAX_DISCOVERY_BYTES} bytes"
            )));
        }
        body.extend_from_slice(&chunk);
    }
    let document: FederationDiscoveryDocument = serde_json::from_slice(&body).map_err(|error| {
        AppError::Message(format!(
            "FEDERATION_DISCOVERY_RESPONSE_INVALID: discovery document is invalid: {error}"
        ))
    })?;
    validate_discovery_document(&document, expected_node_id)?;
    Ok(document)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_document_requires_signed_bootstrap_and_expected_node_identity() {
        let root = tempfile::tempdir().expect("discovery signing root");
        let descriptor = super::super::local_peer_descriptor(&[]).expect("descriptor");
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("time")
            .as_millis() as u64;
        let bundle =
            super::super::signing::bootstrap_bundle_for_test(root.path(), &descriptor, now)
                .expect("bootstrap");
        let document = FederationDiscoveryDocument {
            schema_version: DISCOVERY_SCHEMA_VERSION,
            contract: DISCOVERY_CONTRACT.into(),
            node_id: bundle.node_id.clone(),
            bootstrap: bundle,
            rotation: None,
        };
        validate_discovery_document(&document, Some(&document.node_id)).expect("valid discovery");
        assert!(validate_discovery_document(
            &document,
            Some("node_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
        )
        .expect_err("expected node mismatch")
        .to_string()
        .contains("FEDERATION_DISCOVERY_NODE_MISMATCH"));

        let mut tampered = document;
        tampered
            .bootstrap
            .descriptor
            .workspaces
            .push(super::super::FederationWorkspaceRoute {
                node_id: tampered.node_id.clone(),
                workspace_id: "0123456789abcdef0123456789abcdef".into(),
                display_name: "tampered".into(),
                access: super::super::FederationAccessMode::ReadOnly,
            });
        assert!(validate_discovery_document(&tampered, None).is_err());
    }

    #[test]
    fn public_discovery_descriptor_never_contains_workspace_catalog() {
        let profile =
            WorkspaceProfile::new("/tmp/private-workspace".into(), Some("Private".into()));
        let descriptor = super::super::local_peer_descriptor(&[profile]).expect("descriptor");
        assert_eq!(descriptor.workspaces.len(), 1);
        let public = public_discovery_descriptor(descriptor);
        assert!(public.workspaces.is_empty());
    }
}
