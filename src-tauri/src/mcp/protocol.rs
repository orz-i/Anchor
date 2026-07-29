use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::tools::CancellationToken;

pub const CURRENT_PROTOCOL_VERSION: &str = "2025-11-25";
pub const SUPPORTED_PROTOCOL_VERSIONS: &[&str] =
    &[CURRENT_PROTOCOL_VERSION, "2025-06-18", "2025-03-26"];

#[derive(Debug, Clone, PartialEq)]
pub enum ClientMessage {
    Request { id: Value, method: String },
    Notification { method: String },
    Response,
}

#[derive(Clone)]
pub struct KeyedRateLimiter {
    inner: Arc<Mutex<HashMap<String, VecDeque<Instant>>>>,
    max_requests: usize,
    max_keys: usize,
    window: Duration,
}

impl KeyedRateLimiter {
    pub fn new(max_requests: usize, max_keys: usize, window: Duration) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            max_requests: max_requests.max(1),
            max_keys: max_keys.max(1),
            window,
        }
    }

    pub fn allow(&self, key: &str) -> bool {
        let now = Instant::now();
        let mut buckets = self.inner.lock().expect("MCP keyed rate limiter lock");
        buckets.retain(|_, requests| {
            while requests
                .front()
                .is_some_and(|created| now.duration_since(*created) >= self.window)
            {
                requests.pop_front();
            }
            !requests.is_empty()
        });

        if !buckets.contains_key(key) && buckets.len() >= self.max_keys {
            let oldest = buckets
                .iter()
                .min_by_key(|(_, requests)| requests.back().copied())
                .map(|(key, _)| key.clone());
            if let Some(oldest) = oldest {
                buckets.remove(&oldest);
            }
        }

        let requests = buckets.entry(key.to_string()).or_default();
        if requests.len() >= self.max_requests {
            return false;
        }
        requests.push_back(now);
        true
    }
}

#[derive(Clone, Default)]
pub struct InFlightRequests {
    inner: Arc<Mutex<HashMap<String, CancellationToken>>>,
}

pub enum InFlightReservation {
    Inserted(CancellationToken),
    Duplicate,
    SessionLimit,
    InvalidRequestId,
}

impl InFlightRequests {
    pub fn insert(&self, session_id: &str, request_id: &Value) -> Option<CancellationToken> {
        match self.insert_with_session_limit(session_id, request_id, usize::MAX) {
            InFlightReservation::Inserted(token) => Some(token),
            _ => None,
        }
    }

    pub fn insert_with_session_limit(
        &self,
        session_id: &str,
        request_id: &Value,
        max_in_flight: usize,
    ) -> InFlightReservation {
        let Some(key) = request_key(session_id, request_id) else {
            return InFlightReservation::InvalidRequestId;
        };
        let mut requests = self.inner.lock().expect("MCP in-flight request lock");
        if requests.contains_key(&key) {
            return InFlightReservation::Duplicate;
        }
        let prefix = format!("{session_id}:");
        if requests.keys().filter(|key| key.starts_with(&prefix)).count() >= max_in_flight.max(1) {
            return InFlightReservation::SessionLimit;
        }
        let token = CancellationToken::default();
        requests.insert(key, token.clone());
        InFlightReservation::Inserted(token)
    }

    pub fn cancel(&self, session_id: &str, request_id: &Value) -> bool {
        let Some(key) = request_key(session_id, request_id) else {
            return false;
        };
        let token = self
            .inner
            .lock()
            .expect("MCP in-flight request lock")
            .get(&key)
            .cloned();
        if let Some(token) = token {
            token.cancel();
            true
        } else {
            false
        }
    }

    pub fn remove(&self, session_id: &str, request_id: &Value) {
        if let Some(key) = request_key(session_id, request_id) {
            self.inner
                .lock()
                .expect("MCP in-flight request lock")
                .remove(&key);
        }
    }

    pub fn cancel_session(&self, session_id: &str) {
        let prefix = format!("{session_id}:");
        let mut requests = self.inner.lock().expect("MCP in-flight request lock");
        let tokens = requests
            .iter()
            .filter(|(key, _)| key.starts_with(&prefix))
            .map(|(key, token)| (key.clone(), token.clone()))
            .collect::<Vec<_>>();
        for (key, token) in tokens {
            token.cancel();
            requests.remove(&key);
        }
    }
}

fn request_key(session_id: &str, request_id: &Value) -> Option<String> {
    let request_id = serialized_request_id(request_id)?;
    Some(format!("{session_id}:{request_id}"))
}

fn serialized_request_id(request_id: &Value) -> Option<String> {
    if !valid_request_id(request_id) {
        return None;
    }
    serde_json::to_string(request_id).ok()
}

pub fn validate_client_message(body: &Value) -> Result<ClientMessage, Value> {
    let Some(object) = body.as_object() else {
        return Err(invalid_request(
            "MCP does not support JSON-RPC batch messages",
        ));
    };
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err(invalid_request("jsonrpc must be exactly \"2.0\""));
    }
    if let Some(params) = object.get("params") {
        if !params.is_object() {
            return Err(invalid_request("MCP params must be an object"));
        }
    }

    if let Some(method) = object.get("method") {
        let Some(method) = method.as_str().filter(|method| !method.is_empty()) else {
            return Err(invalid_request("method must be a non-empty string"));
        };
        if object.contains_key("result") || object.contains_key("error") {
            return Err(invalid_request(
                "JSON-RPC requests and notifications cannot contain result or error",
            ));
        }
        return match object.get("id") {
            Some(id) if valid_request_id(id) => Ok(ClientMessage::Request {
                id: id.clone(),
                method: method.to_string(),
            }),
            Some(_) => Err(invalid_request(
                "request id must be a string or integer and cannot be null",
            )),
            None => Ok(ClientMessage::Notification {
                method: method.to_string(),
            }),
        };
    }

    let Some(id) = object.get("id") else {
        return Err(invalid_request("JSON-RPC message is missing method and id"));
    };
    if !valid_request_id(id) {
        return Err(invalid_request(
            "response id must be a string or integer and cannot be null",
        ));
    }
    if object.contains_key("result") == object.contains_key("error") {
        return Err(invalid_request(
            "JSON-RPC response must contain exactly one of result or error",
        ));
    }
    if let Some(result) = object.get("result") {
        if !result.is_object() {
            return Err(invalid_request("MCP result must be an object"));
        }
    }
    if let Some(error) = object.get("error") {
        let Some(error) = error.as_object() else {
            return Err(invalid_request("JSON-RPC error must be an object"));
        };
        if error
            .get("code")
            .and_then(Value::as_number)
            .is_none_or(|number| !number.is_i64() && !number.is_u64())
            || error
                .get("message")
                .and_then(Value::as_str)
                .is_none_or(str::is_empty)
        {
            return Err(invalid_request(
                "JSON-RPC error.code must be an integer and error.message must be a non-empty string",
            ));
        }
    }
    Ok(ClientMessage::Response)
}

pub fn requested_protocol_version(body: &Value) -> Result<&str, Value> {
    let params = body
        .get("params")
        .and_then(Value::as_object)
        .ok_or_else(|| invalid_params("initialize params must be an object"))?;
    let version = params
        .get("protocolVersion")
        .and_then(Value::as_str)
        .filter(|version| !version.is_empty())
        .ok_or_else(|| invalid_params("initialize protocolVersion is required"))?;
    if !params.get("capabilities").is_some_and(Value::is_object) {
        return Err(invalid_params("initialize capabilities must be an object"));
    }
    let client_info = params
        .get("clientInfo")
        .and_then(Value::as_object)
        .ok_or_else(|| invalid_params("initialize clientInfo must be an object"))?;
    if client_info
        .get("name")
        .and_then(Value::as_str)
        .is_none_or(str::is_empty)
        || client_info
            .get("version")
            .and_then(Value::as_str)
            .is_none_or(str::is_empty)
    {
        return Err(invalid_params(
            "initialize clientInfo.name and clientInfo.version are required",
        ));
    }
    Ok(version)
}

pub fn negotiate_protocol_version(requested: &str) -> &'static str {
    SUPPORTED_PROTOCOL_VERSIONS
        .iter()
        .copied()
        .find(|supported| *supported == requested)
        .unwrap_or(CURRENT_PROTOCOL_VERSION)
}

pub fn protocol_version_supported(version: &str) -> bool {
    SUPPORTED_PROTOCOL_VERSIONS.contains(&version)
}

fn valid_request_id(id: &Value) -> bool {
    id.is_string()
        || id
            .as_number()
            .is_some_and(|number| number.is_i64() || number.is_u64())
}

fn invalid_request(message: &str) -> Value {
    json!({ "code": -32600, "message": message })
}

fn invalid_params(message: &str) -> Value {
    json!({ "code": -32602, "message": message })
}

const DEFAULT_SESSION_IDLE_TTL: Duration = Duration::from_secs(24 * 60 * 60);
const DEFAULT_UNINITIALIZED_SESSION_TTL: Duration = Duration::from_secs(5 * 60);
const DEFAULT_MAX_SESSIONS: usize = 512;
const DEFAULT_MAX_REQUEST_IDS_PER_SESSION: usize = 16_384;

#[derive(Debug, Clone)]
struct Session {
    protocol_version: String,
    initialized: bool,
    last_seen: Instant,
    used_request_ids: HashSet<String>,
}

#[derive(Debug, Default)]
struct SessionStoreState {
    sessions: HashMap<String, Session>,
    retired: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionInfo {
    pub protocol_version: String,
    pub initialized: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestReservation {
    Reserved,
    Duplicate,
    Exhausted,
    UnknownSession,
}

#[derive(Clone)]
pub struct SessionStore {
    inner: Arc<Mutex<SessionStoreState>>,
    session_idle_ttl: Duration,
    uninitialized_ttl: Duration,
    max_sessions: usize,
    max_request_ids_per_session: usize,
}

impl Default for SessionStore {
    fn default() -> Self {
        Self::with_limits(
            DEFAULT_SESSION_IDLE_TTL,
            DEFAULT_UNINITIALIZED_SESSION_TTL,
            DEFAULT_MAX_SESSIONS,
            DEFAULT_MAX_REQUEST_IDS_PER_SESSION,
        )
    }
}

impl SessionStore {
    pub fn with_limits(
        session_idle_ttl: Duration,
        uninitialized_ttl: Duration,
        max_sessions: usize,
        max_request_ids_per_session: usize,
    ) -> Self {
        Self {
            inner: Arc::new(Mutex::new(SessionStoreState::default())),
            session_idle_ttl,
            uninitialized_ttl,
            max_sessions: max_sessions.max(1),
            max_request_ids_per_session: max_request_ids_per_session.max(1),
        }
    }

    pub fn create(&self, protocol_version: &str, initialize_request_id: &Value) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        let mut state = self.inner.lock().expect("MCP session lock");
        self.prune_locked(&mut state);
        while state.sessions.len() >= self.max_sessions {
            let Some(oldest) = state
                .sessions
                .iter()
                .min_by_key(|(_, session)| session.last_seen)
                .map(|(session_id, _)| session_id.clone())
            else {
                break;
            };
            state.sessions.remove(&oldest);
            retire_session(&mut state, oldest);
        }
        let mut used_request_ids = HashSet::new();
        if let Some(request_id) = serialized_request_id(initialize_request_id) {
            used_request_ids.insert(request_id);
        }
        state.sessions.insert(
            id.clone(),
            Session {
                protocol_version: protocol_version.to_string(),
                initialized: false,
                last_seen: Instant::now(),
                used_request_ids,
            },
        );
        id
    }

    pub fn inspect(&self, id: &str) -> Option<SessionInfo> {
        let mut state = self.inner.lock().expect("MCP session lock");
        self.prune_locked(&mut state);
        let session = state.sessions.get(id)?;
        Some(SessionInfo {
            protocol_version: session.protocol_version.clone(),
            initialized: session.initialized,
        })
    }

    pub fn mark_initialized(&self, id: &str) -> bool {
        let mut state = self.inner.lock().expect("MCP session lock");
        self.prune_locked(&mut state);
        let Some(session) = state.sessions.get_mut(id) else {
            return false;
        };
        session.initialized = true;
        session.last_seen = Instant::now();
        true
    }

    pub fn touch(&self, id: &str) -> bool {
        let mut state = self.inner.lock().expect("MCP session lock");
        self.prune_locked(&mut state);
        let Some(session) = state.sessions.get_mut(id) else {
            return false;
        };
        session.last_seen = Instant::now();
        true
    }

    pub fn reserve_request_id(&self, session_id: &str, request_id: &Value) -> RequestReservation {
        let Some(request_id) = serialized_request_id(request_id) else {
            return RequestReservation::Duplicate;
        };
        let mut state = self.inner.lock().expect("MCP session lock");
        self.prune_locked(&mut state);
        let Some(session) = state.sessions.get_mut(session_id) else {
            return RequestReservation::UnknownSession;
        };
        if session.used_request_ids.contains(&request_id) {
            return RequestReservation::Duplicate;
        }
        if session.used_request_ids.len() >= self.max_request_ids_per_session {
            return RequestReservation::Exhausted;
        }
        session.used_request_ids.insert(request_id);
        session.last_seen = Instant::now();
        RequestReservation::Reserved
    }

    pub fn remove(&self, id: &str) -> bool {
        self.inner
            .lock()
            .expect("MCP session lock")
            .sessions
            .remove(id)
            .is_some()
    }

    pub fn take_retired(&self) -> Vec<String> {
        std::mem::take(&mut self.inner.lock().expect("MCP session lock").retired)
    }

    fn prune_locked(&self, state: &mut SessionStoreState) {
        let now = Instant::now();
        let expired = state
            .sessions
            .iter()
            .filter_map(|(id, session)| {
                let ttl = if session.initialized {
                    self.session_idle_ttl
                } else {
                    self.uninitialized_ttl
                };
                (now.duration_since(session.last_seen) > ttl).then(|| id.clone())
            })
            .collect::<Vec<_>>();
        for id in expired {
            state.sessions.remove(&id);
            retire_session(state, id);
        }
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.inner.lock().expect("MCP session lock").sessions.len()
    }
}

fn retire_session(state: &mut SessionStoreState, id: String) {
    if !state.retired.iter().any(|existing| existing == &id) {
        state.retired.push(id);
    }
}

#[derive(Clone)]
pub struct RateLimiter {
    inner: Arc<Mutex<VecDeque<Instant>>>,
    max_requests: usize,
    window: Duration,
}

impl RateLimiter {
    pub fn new(max_requests: usize, window: Duration) -> Self {
        Self {
            inner: Arc::new(Mutex::new(VecDeque::new())),
            max_requests,
            window,
        }
    }

    pub fn allow(&self) -> bool {
        let now = Instant::now();
        let mut requests = self.inner.lock().expect("MCP rate limiter lock");
        while requests
            .front()
            .is_some_and(|created| now.duration_since(*created) >= self.window)
        {
            requests.pop_front();
        }
        if requests.len() >= self.max_requests {
            return false;
        }
        requests.push_back(now);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_requests_notifications_and_responses() {
        assert!(matches!(
            validate_client_message(&json!({
                "jsonrpc": "2.0", "id": 1, "method": "ping", "params": {}
            })),
            Ok(ClientMessage::Request { .. })
        ));
        assert!(matches!(
            validate_client_message(&json!({
                "jsonrpc": "2.0", "method": "notifications/initialized"
            })),
            Ok(ClientMessage::Notification { .. })
        ));
        assert!(matches!(
            validate_client_message(&json!({"jsonrpc": "2.0", "id": 1, "result": {}})),
            Ok(ClientMessage::Response)
        ));
        assert!(validate_client_message(&json!([])).is_err());
        assert!(validate_client_message(&json!({
            "jsonrpc": "2.0", "id": null, "method": "ping"
        }))
        .is_err());
        assert!(validate_client_message(&json!({
            "jsonrpc": "2.0", "id": 2, "method": "ping", "params": []
        }))
        .is_err());
        assert!(validate_client_message(&json!({
            "jsonrpc": "2.0", "id": 2, "error": {"code": "bad", "message": "x"}
        }))
        .is_err());
    }

    #[test]
    fn negotiates_current_and_legacy_versions() {
        assert_eq!(negotiate_protocol_version("2025-11-25"), "2025-11-25");
        assert_eq!(negotiate_protocol_version("2025-06-18"), "2025-06-18");
        assert_eq!(negotiate_protocol_version("unknown"), "2025-11-25");
    }

    #[test]
    fn session_store_tracks_initialized_state() {
        let store = SessionStore::default();
        let id = store.create("2025-11-25", &json!(1));
        assert_eq!(
            store.inspect(&id),
            Some(SessionInfo {
                protocol_version: "2025-11-25".into(),
                initialized: false
            })
        );
        assert!(store.mark_initialized(&id));
        assert_eq!(
            store.inspect(&id).map(|session| session.initialized),
            Some(true)
        );
        assert!(store.remove(&id));
        assert!(store.inspect(&id).is_none());
    }

    #[test]
    fn session_rejects_reused_request_ids() {
        let store = SessionStore::default();
        let id = store.create("2025-11-25", &json!(1));
        assert_eq!(
            store.reserve_request_id(&id, &json!(1)),
            RequestReservation::Duplicate
        );
        assert_eq!(
            store.reserve_request_id(&id, &json!(2)),
            RequestReservation::Reserved
        );
        assert_eq!(
            store.reserve_request_id(&id, &json!(2)),
            RequestReservation::Duplicate
        );
    }

    #[test]
    fn session_store_bounds_request_ids_and_total_sessions() {
        let store =
            SessionStore::with_limits(Duration::from_secs(60), Duration::from_secs(60), 2, 2);
        let first = store.create("2025-11-25", &json!(1));
        assert_eq!(
            store.reserve_request_id(&first, &json!(2)),
            RequestReservation::Reserved
        );
        assert_eq!(
            store.reserve_request_id(&first, &json!(3)),
            RequestReservation::Exhausted
        );
        let second = store.create("2025-11-25", &json!(1));
        let third = store.create("2025-11-25", &json!(1));
        assert_eq!(store.len(), 2);
        assert!(store.inspect(&first).is_none());
        assert!(store.inspect(&second).is_some());
        assert!(store.inspect(&third).is_some());
        assert_eq!(store.take_retired(), vec![first]);
    }

    #[test]
    fn uninitialized_sessions_expire_without_touching_other_sessions() {
        let store = SessionStore::with_limits(Duration::from_secs(60), Duration::ZERO, 4, 4);
        let expired = store.create("2025-11-25", &json!(1));
        std::thread::sleep(Duration::from_millis(1));
        assert!(store.inspect(&expired).is_none());
        assert_eq!(store.take_retired(), vec![expired]);
    }

    #[test]
    fn rate_limiter_rejects_excess_requests() {
        let limiter = RateLimiter::new(2, Duration::from_secs(60));
        assert!(limiter.allow());
        assert!(limiter.allow());
        assert!(!limiter.allow());
    }

    #[test]
    fn keyed_rate_limiter_keeps_client_budgets_independent() {
        let limiter = KeyedRateLimiter::new(1, 4, Duration::from_secs(60));
        assert!(limiter.allow("client-a"));
        assert!(!limiter.allow("client-a"));
        assert!(limiter.allow("client-b"));
    }

    #[test]
    fn in_flight_registry_cancels_matching_request() {
        let requests = InFlightRequests::default();
        let id = json!(7);
        let token = requests.insert("session", &id).expect("insert");
        assert!(!token.is_cancelled());
        assert!(requests.cancel("session", &id));
        assert!(token.is_cancelled());
        requests.remove("session", &id);
        assert!(!requests.cancel("session", &id));
    }

    #[test]
    fn in_flight_registry_enforces_per_session_limit() {
        let requests = InFlightRequests::default();
        assert!(matches!(
            requests.insert_with_session_limit("session", &json!(1), 2),
            InFlightReservation::Inserted(_)
        ));
        assert!(matches!(
            requests.insert_with_session_limit("session", &json!(2), 2),
            InFlightReservation::Inserted(_)
        ));
        assert!(matches!(
            requests.insert_with_session_limit("session", &json!(3), 2),
            InFlightReservation::SessionLimit
        ));
        assert!(matches!(
            requests.insert_with_session_limit("other", &json!(1), 2),
            InFlightReservation::Inserted(_)
        ));
    }

    #[test]
    fn in_flight_registry_cancels_entire_session() {
        let requests = InFlightRequests::default();
        let first = requests.insert("session", &json!(1)).expect("first");
        let second = requests.insert("session", &json!(2)).expect("second");
        let other = requests.insert("other", &json!(1)).expect("other");
        requests.cancel_session("session");
        assert!(first.is_cancelled());
        assert!(second.is_cancelled());
        assert!(!other.is_cancelled());
    }
}
