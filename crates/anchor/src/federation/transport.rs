use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{AppError, AppResult};
use crate::secret::SecretStore;

use super::{
    valid_node_id, validate_peer_descriptor, validate_request_context, FederationContextRef,
    FederationContextScope, FederationPeerDescriptor, FederationReadOperation,
    FederationReadRequest, FederationReadResult, FEDERATION_CONTRACT, FEDERATION_SCHEMA_VERSION,
};

const OUTBOUND_TOKEN_SCOPE: &str = "federation_outbound_token";
const INBOUND_TOKEN_SCOPE: &str = "federation_inbound_token";
const REQUEST_REPLAY_SCOPE: &str = "federation_request_replay";
const TRANSPORT_TTL_MS: u64 = 30_000;
const TRANSPORT_MAX_TTL_MS: u64 = 60_000;
const TRANSPORT_CLOCK_SKEW_MS: u64 = 5_000;
const TRANSPORT_CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const TRANSPORT_REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
pub(crate) const FEDERATION_MAX_REQUEST_BYTES: usize = 64 * 1024;
pub(crate) const FEDERATION_MAX_RESPONSE_BYTES: usize = 512 * 1024;
pub(crate) const FEDERATION_NODE_HEADER: &str = "x-anchor-federation-node";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FederationRemoteTarget {
    pub node_id: String,
    pub endpoint: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FederationPeerCredentialStatus {
    pub node_id: String,
    pub endpoint: String,
    pub outbound_configured: bool,
    pub inbound_configured: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FederationTransportRequest {
    pub schema_version: u16,
    pub contract: String,
    pub request_id: String,
    pub issued_at_unix_ms: u64,
    pub expires_at_unix_ms: u64,
    pub caller_node_id: String,
    pub target_node_id: String,
    pub runtime_contract: String,
    pub runtime_schema_version: u16,
    pub read: FederationReadRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FederationTransportResponse {
    pub schema_version: u16,
    pub contract: String,
    pub request_id: String,
    pub responder_node_id: String,
    pub runtime_contract: String,
    pub runtime_schema_version: u16,
    pub result: FederationReadResult,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FederationTransportErrorKind {
    Unauthorized,
    InvalidRequest,
    Replay,
    Unavailable,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FederationTransportError {
    pub kind: FederationTransportErrorKind,
    pub code: &'static str,
    pub message: String,
    pub retryable: bool,
}

impl std::fmt::Display for FederationTransportError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for FederationTransportError {}

fn credential_binding_key(target: &FederationRemoteTarget) -> AppResult<String> {
    let target = canonical_remote_target(target)?;
    Ok(format!(
        "{:x}",
        Sha256::digest(format!("{}\0{}", target.node_id, target.endpoint).as_bytes())
    ))
}

pub(crate) fn canonical_remote_target(
    target: &FederationRemoteTarget,
) -> AppResult<FederationRemoteTarget> {
    validate_target(target)?;
    let endpoint = normalize_endpoint(&target.endpoint)?;
    Ok(FederationRemoteTarget {
        node_id: target.node_id.clone(),
        endpoint: endpoint.as_str().trim_end_matches('/').to_string(),
    })
}

pub fn set_peer_credential(target: &FederationRemoteTarget, token: &str) -> AppResult<()> {
    validate_token(token)?;
    let binding = credential_binding_key(target)?;
    SecretStore::set_app_many(&[
        (OUTBOUND_TOKEN_SCOPE, &binding, token),
        (INBOUND_TOKEN_SCOPE, &target.node_id, token),
    ])
}

pub fn clear_peer_credential(target: &FederationRemoteTarget) -> AppResult<()> {
    let binding = credential_binding_key(target)?;
    SecretStore::remove_app_many(&[
        (OUTBOUND_TOKEN_SCOPE, &binding),
        (INBOUND_TOKEN_SCOPE, &target.node_id),
        (REQUEST_REPLAY_SCOPE, &target.node_id),
    ])
}

pub fn peer_credential_status(
    target: &FederationRemoteTarget,
) -> AppResult<FederationPeerCredentialStatus> {
    let target = canonical_remote_target(target)?;
    let binding = credential_binding_key(&target)?;
    Ok(FederationPeerCredentialStatus {
        node_id: target.node_id.clone(),
        endpoint: target.endpoint.clone(),
        outbound_configured: SecretStore::get_app(OUTBOUND_TOKEN_SCOPE, &binding)?.is_some(),
        inbound_configured: SecretStore::get_app(INBOUND_TOKEN_SCOPE, &target.node_id)?.is_some(),
    })
}

pub async fn probe_remote_peer(
    target: &FederationRemoteTarget,
) -> AppResult<FederationPeerDescriptor> {
    let result = remote_read(
        target,
        &FederationReadRequest {
            node_id: target.node_id.clone(),
            workspace_id: None,
            operation: FederationReadOperation::WorkspaceCatalog,
            context: FederationContextRef {
                scope: FederationContextScope::NodeLocal,
                node_id: Some(target.node_id.clone()),
                workspace_id: None,
            },
        },
    )
    .await?;
    match result {
        FederationReadResult::WorkspaceCatalog(peer) => Ok(peer),
        _ => Err(AppError::Message(
            "FEDERATION_REMOTE_RESULT_MISMATCH: workspace_catalog returned an unexpected result kind"
                .into(),
        )),
    }
}

pub async fn remote_read(
    target: &FederationRemoteTarget,
    request: &FederationReadRequest,
) -> AppResult<FederationReadResult> {
    validate_target(target)?;
    validate_request_context(request)?;
    if request.node_id != target.node_id {
        return Err(AppError::Message(
            "FEDERATION_REMOTE_TARGET_MISMATCH: request nodeId does not match remote target".into(),
        ));
    }
    let endpoint = normalize_endpoint(&target.endpoint)?;
    let binding = credential_binding_key(target)?;
    let token = SecretStore::get_app(OUTBOUND_TOKEN_SCOPE, &binding)?.ok_or_else(|| {
        AppError::Message(
            "FEDERATION_CREDENTIAL_MISSING: no outbound credential is bound to this node and endpoint"
                .into(),
        )
    })?;
    let local_runtime = crate::runtime::capability_snapshot(None)?;
    if target.node_id == local_runtime.node.id {
        return Err(AppError::Message(
            "FEDERATION_SELF_PEER: authenticated remote transport cannot target the local node"
                .into(),
        ));
    }
    let now = unix_time_ms()?;
    let request_id = uuid::Uuid::new_v4().simple().to_string();
    let envelope = FederationTransportRequest {
        schema_version: FEDERATION_SCHEMA_VERSION,
        contract: FEDERATION_CONTRACT.into(),
        request_id: request_id.clone(),
        issued_at_unix_ms: now,
        expires_at_unix_ms: now.saturating_add(TRANSPORT_TTL_MS),
        caller_node_id: local_runtime.node.id.clone(),
        target_node_id: target.node_id.clone(),
        runtime_contract: local_runtime.contract.clone(),
        runtime_schema_version: local_runtime.schema_version,
        read: request.clone(),
    };
    let body = serde_json::to_vec(&envelope)?;
    if body.len() > FEDERATION_MAX_REQUEST_BYTES {
        return Err(AppError::Message(format!(
            "FEDERATION_REQUEST_TOO_LARGE: request body exceeds {} bytes",
            FEDERATION_MAX_REQUEST_BYTES
        )));
    }
    let url = endpoint
        .join("federation/v1/read")
        .map_err(|error| AppError::Message(format!("invalid federation endpoint: {error}")))?;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .connect_timeout(TRANSPORT_CONNECT_TIMEOUT)
        .timeout(TRANSPORT_REQUEST_TIMEOUT)
        .build()
        .map_err(|error| {
            AppError::Message(format!("failed to build federation client: {error}"))
        })?;
    let response = client
        .post(url)
        .header(FEDERATION_NODE_HEADER, &local_runtime.node.id)
        .bearer_auth(token)
        .header("content-type", "application/json")
        .header("accept", "application/json")
        .body(body)
        .send()
        .await
        .map_err(|error| {
            AppError::Message(format!(
                "FEDERATION_REMOTE_UNAVAILABLE: remote federation request failed: {error}"
            ))
        })?;
    let status = response.status();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if status.is_success() && !content_type.starts_with("application/json") {
        return Err(AppError::Message(
            "FEDERATION_REMOTE_RESPONSE_INVALID: successful federation responses must use application/json"
                .into(),
        ));
    }
    let bytes = read_bounded_response(response).await?;
    if !status.is_success() {
        return Err(AppError::Message(format!(
            "FEDERATION_REMOTE_REJECTED: remote endpoint returned HTTP {}",
            status.as_u16()
        )));
    }
    let response: FederationTransportResponse =
        serde_json::from_slice(&bytes).map_err(|error| {
            AppError::Message(format!(
                "FEDERATION_REMOTE_RESPONSE_INVALID: response contract is invalid: {error}"
            ))
        })?;
    validate_transport_response(&local_runtime, target, request, &request_id, &response)?;
    Ok(response.result)
}

pub async fn handle_inbound_transport(
    caller_header: Option<&str>,
    authorization: Option<&str>,
    body: &[u8],
) -> Result<FederationTransportResponse, FederationTransportError> {
    if body.len() > FEDERATION_MAX_REQUEST_BYTES {
        return Err(transport_error(
            FederationTransportErrorKind::InvalidRequest,
            "FEDERATION_REQUEST_TOO_LARGE",
            "federation request exceeds the transport body limit",
            false,
        ));
    }
    let envelope: FederationTransportRequest = serde_json::from_slice(body).map_err(|_| {
        transport_error(
            FederationTransportErrorKind::InvalidRequest,
            "FEDERATION_REQUEST_INVALID",
            "federation request envelope is invalid",
            false,
        )
    })?;
    validate_transport_request(caller_header, &envelope)?;
    authenticate_inbound(authorization, &envelope.caller_node_id)?;

    let now_ms = unix_time_ms().map_err(internal_error)?;
    let now_s = now_ms / 1000;
    let replay_expiry_s = envelope.expires_at_unix_ms / 1000 + 1;
    let fresh = SecretStore::consume_app_nonce(
        REQUEST_REPLAY_SCOPE,
        &envelope.caller_node_id,
        &envelope.request_id,
        replay_expiry_s,
        now_s,
    )
    .map_err(internal_error)?;
    if !fresh {
        return Err(transport_error(
            FederationTransportErrorKind::Replay,
            "FEDERATION_REQUEST_REPLAYED",
            "federation request id was already consumed",
            false,
        ));
    }

    let result = super::execute_local_read(&envelope.read)
        .await
        .map_err(|error| {
            transport_error(
                FederationTransportErrorKind::InvalidRequest,
                "FEDERATION_READ_REJECTED",
                &error.to_string(),
                false,
            )
        })?;
    let runtime = crate::runtime::capability_snapshot(None).map_err(internal_error)?;
    Ok(FederationTransportResponse {
        schema_version: FEDERATION_SCHEMA_VERSION,
        contract: FEDERATION_CONTRACT.into(),
        request_id: envelope.request_id,
        responder_node_id: runtime.node.id,
        runtime_contract: runtime.contract,
        runtime_schema_version: runtime.schema_version,
        result,
    })
}

fn validate_transport_request(
    caller_header: Option<&str>,
    envelope: &FederationTransportRequest,
) -> Result<(), FederationTransportError> {
    let runtime = crate::runtime::capability_snapshot(None).map_err(internal_error)?;
    if envelope.schema_version != FEDERATION_SCHEMA_VERSION
        || envelope.contract != FEDERATION_CONTRACT
    {
        return Err(invalid_request(
            "FEDERATION_CONTRACT_MISMATCH",
            "federation transport contract is incompatible",
        ));
    }
    if envelope.runtime_contract != runtime.contract
        || envelope.runtime_schema_version != runtime.schema_version
    {
        return Err(invalid_request(
            "RUNTIME_CAPABILITY_MISMATCH",
            "caller runtime capability contract is incompatible",
        ));
    }
    if !valid_node_id(&envelope.caller_node_id)
        || !valid_node_id(&envelope.target_node_id)
        || envelope.caller_node_id == envelope.target_node_id
    {
        return Err(invalid_request(
            "FEDERATION_NODE_ID_INVALID",
            "caller and target require distinct stable Anchor node ids",
        ));
    }
    if caller_header != Some(envelope.caller_node_id.as_str()) {
        return Err(transport_error(
            FederationTransportErrorKind::Unauthorized,
            "FEDERATION_CALLER_ID_MISMATCH",
            "caller identity header does not match the authenticated request envelope",
            false,
        ));
    }
    if envelope.target_node_id != runtime.node.id || envelope.read.node_id != runtime.node.id {
        return Err(invalid_request(
            "FEDERATION_TARGET_NODE_MISMATCH",
            "federation request is addressed to a different node",
        ));
    }
    if envelope.request_id.len() != 32
        || !envelope
            .request_id
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(invalid_request(
            "FEDERATION_REQUEST_ID_INVALID",
            "requestId must be a 32-character hexadecimal nonce",
        ));
    }
    let now = unix_time_ms().map_err(internal_error)?;
    if envelope.issued_at_unix_ms > now.saturating_add(TRANSPORT_CLOCK_SKEW_MS)
        || envelope.expires_at_unix_ms < now
        || envelope.expires_at_unix_ms < envelope.issued_at_unix_ms
        || envelope
            .expires_at_unix_ms
            .saturating_sub(envelope.issued_at_unix_ms)
            > TRANSPORT_MAX_TTL_MS
    {
        return Err(invalid_request(
            "FEDERATION_REQUEST_EXPIRED",
            "request timestamp is outside the accepted federation transport window",
        ));
    }
    validate_request_context(&envelope.read)
        .map_err(|error| invalid_request("FEDERATION_CONTEXT_INVALID", &error.to_string()))?;
    Ok(())
}

fn authenticate_inbound(
    authorization: Option<&str>,
    caller_node_id: &str,
) -> Result<(), FederationTransportError> {
    let Some(presented) = authorization.and_then(|value| value.strip_prefix("Bearer ")) else {
        return Err(unauthorized("FEDERATION_AUTH_REQUIRED"));
    };
    let expected = SecretStore::get_app(INBOUND_TOKEN_SCOPE, caller_node_id)
        .map_err(internal_error)?
        .ok_or_else(|| unauthorized("FEDERATION_PEER_NOT_TRUSTED"))?;
    if !constant_time_eq(expected.as_bytes(), presented.as_bytes()) {
        return Err(unauthorized("FEDERATION_AUTH_INVALID"));
    }
    Ok(())
}

fn validate_transport_response(
    local_runtime: &crate::runtime::RuntimeCapabilitySnapshot,
    target: &FederationRemoteTarget,
    request: &FederationReadRequest,
    request_id: &str,
    response: &FederationTransportResponse,
) -> AppResult<()> {
    if response.schema_version != FEDERATION_SCHEMA_VERSION
        || response.contract != FEDERATION_CONTRACT
        || response.request_id != request_id
        || response.responder_node_id != target.node_id
        || response.runtime_contract != local_runtime.contract
        || response.runtime_schema_version != local_runtime.schema_version
    {
        return Err(AppError::Message(
            "FEDERATION_REMOTE_RESPONSE_MISMATCH: response identity or contract does not match the request"
                .into(),
        ));
    }
    match (&request.operation, &response.result) {
        (
            FederationReadOperation::NodeCapabilities,
            FederationReadResult::NodeCapabilities(value),
        ) => {
            if value.node.id != target.node_id
                || value.workspace.is_some()
                || value.contract != local_runtime.contract
                || value.schema_version != local_runtime.schema_version
            {
                return Err(AppError::Message(
                    "FEDERATION_REMOTE_RESULT_MISMATCH: node capabilities do not match the target node"
                        .into(),
                ));
            }
        }
        (
            FederationReadOperation::NodeControlStatus,
            FederationReadResult::NodeControlStatus(value),
        ) => {
            if value.node_id != target.node_id {
                return Err(AppError::Message(
                    "FEDERATION_REMOTE_RESULT_MISMATCH: node control status identity mismatch"
                        .into(),
                ));
            }
        }
        (
            FederationReadOperation::WorkspaceCatalog,
            FederationReadResult::WorkspaceCatalog(peer),
        ) => {
            if peer.runtime.node.id != target.node_id {
                return Err(AppError::Message(
                    "FEDERATION_REMOTE_RESULT_MISMATCH: workspace catalog node identity mismatch"
                        .into(),
                ));
            }
            let validation = validate_peer_descriptor(local_runtime, peer);
            if !validation.accepted {
                return Err(AppError::Message(format!(
                    "{}: {}",
                    validation.code, validation.reason
                )));
            }
        }
        (
            FederationReadOperation::WorkspaceStatus,
            FederationReadResult::WorkspaceStatus(value),
        ) => {
            if value.node_id != target.node_id
                || Some(value.workspace_id.as_str()) != request.workspace_id.as_deref()
            {
                return Err(AppError::Message(
                    "FEDERATION_REMOTE_RESULT_MISMATCH: workspace status route identity mismatch"
                        .into(),
                ));
            }
        }
        _ => {
            return Err(AppError::Message(
                "FEDERATION_REMOTE_RESULT_MISMATCH: response result kind does not match requested operation"
                    .into(),
            ));
        }
    }
    Ok(())
}

async fn read_bounded_response(response: reqwest::Response) -> AppResult<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|length| length > FEDERATION_MAX_RESPONSE_BYTES as u64)
    {
        return Err(AppError::Message(format!(
            "FEDERATION_RESPONSE_TOO_LARGE: response exceeds {} bytes",
            FEDERATION_MAX_RESPONSE_BYTES
        )));
    }
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| {
            AppError::Message(format!(
                "FEDERATION_REMOTE_RESPONSE_FAILED: failed while reading response: {error}"
            ))
        })?;
        if body.len().saturating_add(chunk.len()) > FEDERATION_MAX_RESPONSE_BYTES {
            return Err(AppError::Message(format!(
                "FEDERATION_RESPONSE_TOO_LARGE: response exceeds {} bytes",
                FEDERATION_MAX_RESPONSE_BYTES
            )));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn validate_target(target: &FederationRemoteTarget) -> AppResult<()> {
    if !valid_node_id(&target.node_id) {
        return Err(AppError::Message(
            "FEDERATION_NODE_ID_INVALID: remote target node id is invalid".into(),
        ));
    }
    normalize_endpoint(&target.endpoint)?;
    Ok(())
}

fn normalize_endpoint(raw: &str) -> AppResult<reqwest::Url> {
    if raw.trim().len() > 256 {
        return Err(AppError::Message(
            "FEDERATION_ENDPOINT_INVALID: endpoint origin exceeds 256 bytes".into(),
        ));
    }
    let mut url = reqwest::Url::parse(raw.trim()).map_err(|error| {
        AppError::Message(format!(
            "FEDERATION_ENDPOINT_INVALID: invalid endpoint URL: {error}"
        ))
    })?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !matches!(url.path(), "" | "/")
    {
        return Err(AppError::Message(
            "FEDERATION_ENDPOINT_INVALID: endpoint must be an origin URL without credentials, path, query, or fragment"
                .into(),
        ));
    }
    let host = url.host_str().ok_or_else(|| {
        AppError::Message("FEDERATION_ENDPOINT_INVALID: endpoint host is required".into())
    })?;
    match url.scheme() {
        "https" => {}
        "http" if is_loopback_host(host) => {}
        _ => {
            return Err(AppError::Message(
                "FEDERATION_ENDPOINT_INSECURE: remote federation endpoints require HTTPS; HTTP is allowed only for loopback"
                    .into(),
            ));
        }
    }
    url.set_path("/");
    Ok(url)
}

fn is_loopback_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost") || host == "127.0.0.1" || host == "::1"
}

fn validate_token(token: &str) -> AppResult<()> {
    if token.len() < 32 || token.len() > 4096 || token.chars().any(char::is_control) {
        return Err(AppError::Message(
            "FEDERATION_CREDENTIAL_INVALID: peer token must be 32-4096 printable characters".into(),
        ));
    }
    Ok(())
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let left = Sha256::digest(left);
    let right = Sha256::digest(right);
    let mut diff = 0_u8;
    for (left_byte, right_byte) in left.iter().zip(right.iter()) {
        diff |= left_byte ^ right_byte;
    }
    diff == 0
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

fn unauthorized(code: &'static str) -> FederationTransportError {
    transport_error(
        FederationTransportErrorKind::Unauthorized,
        code,
        "federation peer authentication failed",
        false,
    )
}

fn invalid_request(code: &'static str, message: &str) -> FederationTransportError {
    transport_error(
        FederationTransportErrorKind::InvalidRequest,
        code,
        message,
        false,
    )
}

fn internal_error(error: impl std::fmt::Display) -> FederationTransportError {
    transport_error(
        FederationTransportErrorKind::Internal,
        "FEDERATION_INTERNAL_ERROR",
        &error.to_string(),
        true,
    )
}

fn transport_error(
    kind: FederationTransportErrorKind,
    code: &'static str,
    message: &str,
    retryable: bool,
) -> FederationTransportError {
    FederationTransportError {
        kind,
        code,
        message: message.to_string(),
        retryable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn transport_request() -> FederationTransportRequest {
        let runtime = crate::runtime::capability_snapshot(None).expect("runtime");
        let caller = if runtime.node.id == "node_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb" {
            "node_cccccccccccccccccccccccccccccccc"
        } else {
            "node_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
        };
        let now = unix_time_ms().expect("time");
        FederationTransportRequest {
            schema_version: FEDERATION_SCHEMA_VERSION,
            contract: FEDERATION_CONTRACT.into(),
            request_id: "0123456789abcdef0123456789abcdef".into(),
            issued_at_unix_ms: now,
            expires_at_unix_ms: now + TRANSPORT_TTL_MS,
            caller_node_id: caller.into(),
            target_node_id: runtime.node.id.clone(),
            runtime_contract: runtime.contract,
            runtime_schema_version: runtime.schema_version,
            read: FederationReadRequest {
                node_id: runtime.node.id.clone(),
                workspace_id: None,
                operation: FederationReadOperation::NodeCapabilities,
                context: FederationContextRef {
                    scope: FederationContextScope::NodeLocal,
                    node_id: Some(runtime.node.id),
                    workspace_id: None,
                },
            },
        }
    }

    #[test]
    fn endpoint_binding_requires_https_except_loopback_and_changes_with_origin() {
        let node_id = "node_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let local = FederationRemoteTarget {
            node_id: node_id.into(),
            endpoint: "http://127.0.0.1:28765".into(),
        };
        credential_binding_key(&local).expect("loopback endpoint");

        let insecure = FederationRemoteTarget {
            node_id: node_id.into(),
            endpoint: "http://example.com".into(),
        };
        assert!(credential_binding_key(&insecure)
            .expect_err("remote HTTP must fail closed")
            .to_string()
            .contains("FEDERATION_ENDPOINT_INSECURE"));

        let first = credential_binding_key(&FederationRemoteTarget {
            node_id: node_id.into(),
            endpoint: "https://node-a.example".into(),
        })
        .unwrap();
        let second = credential_binding_key(&FederationRemoteTarget {
            node_id: node_id.into(),
            endpoint: "https://node-b.example".into(),
        })
        .unwrap();
        assert_ne!(first, second);

        let path = FederationRemoteTarget {
            node_id: node_id.into(),
            endpoint: "https://node-a.example/not-an-origin".into(),
        };
        assert!(credential_binding_key(&path)
            .expect_err("endpoint path must fail closed")
            .to_string()
            .contains("FEDERATION_ENDPOINT_INVALID"));
    }

    #[test]
    fn token_compare_does_not_accept_prefixes_or_length_mismatch() {
        assert!(constant_time_eq(b"same-token", b"same-token"));
        assert!(!constant_time_eq(b"same-token", b"same"));
        assert!(!constant_time_eq(b"same-token", b"same-token-extra"));
    }

    #[test]
    fn transport_request_is_bound_to_caller_target_and_short_lived_window() {
        let request = transport_request();
        validate_transport_request(Some(&request.caller_node_id), &request)
            .expect("valid transport request");

        let header_error =
            validate_transport_request(Some("node_dddddddddddddddddddddddddddddddd"), &request)
                .expect_err("caller header mismatch must fail");
        assert_eq!(header_error.code, "FEDERATION_CALLER_ID_MISMATCH");

        let mut expired = request.clone();
        expired.issued_at_unix_ms = 1;
        expired.expires_at_unix_ms = 2;
        let expired_error = validate_transport_request(Some(&expired.caller_node_id), &expired)
            .expect_err("expired request must fail");
        assert_eq!(expired_error.code, "FEDERATION_REQUEST_EXPIRED");

        let mut wrong_target = request.clone();
        wrong_target.target_node_id = "node_eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee".into();
        let target_error =
            validate_transport_request(Some(&wrong_target.caller_node_id), &wrong_target)
                .expect_err("wrong target must fail");
        assert_eq!(target_error.code, "FEDERATION_TARGET_NODE_MISMATCH");
    }

    #[test]
    fn transport_response_must_match_request_node_contract_and_result_kind() {
        let local = crate::runtime::capability_snapshot(None).expect("local runtime");
        let target = FederationRemoteTarget {
            node_id: "node_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into(),
            endpoint: "https://node-b.example".into(),
        };
        let request = FederationReadRequest {
            node_id: target.node_id.clone(),
            workspace_id: None,
            operation: FederationReadOperation::NodeCapabilities,
            context: FederationContextRef {
                scope: FederationContextScope::NodeLocal,
                node_id: Some(target.node_id.clone()),
                workspace_id: None,
            },
        };
        let remote_runtime = crate::runtime::capability_snapshot_from_node(
            crate::runtime::NodeIdentity {
                id: target.node_id.clone(),
                platform: "test".into(),
                architecture: "test".into(),
            },
            None,
        );
        let response = FederationTransportResponse {
            schema_version: FEDERATION_SCHEMA_VERSION,
            contract: FEDERATION_CONTRACT.into(),
            request_id: "0123456789abcdef0123456789abcdef".into(),
            responder_node_id: target.node_id.clone(),
            runtime_contract: local.contract.clone(),
            runtime_schema_version: local.schema_version,
            result: FederationReadResult::NodeCapabilities(remote_runtime),
        };
        validate_transport_response(
            &local,
            &target,
            &request,
            "0123456789abcdef0123456789abcdef",
            &response,
        )
        .expect("matching response");

        let mut mismatched = response;
        mismatched.responder_node_id = "node_cccccccccccccccccccccccccccccccc".into();
        assert!(validate_transport_response(
            &local,
            &target,
            &request,
            "0123456789abcdef0123456789abcdef",
            &mismatched,
        )
        .expect_err("responder mismatch must fail")
        .to_string()
        .contains("FEDERATION_REMOTE_RESPONSE_MISMATCH"));
    }
}
