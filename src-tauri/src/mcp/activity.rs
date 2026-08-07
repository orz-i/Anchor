use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, Weak};
#[cfg(any(unix, test))]
use std::time::{Duration, Instant, SystemTime};

#[cfg(any(unix, test))]
use chrono::{SecondsFormat, Utc};
use serde_json::Value;

#[cfg(any(unix, test))]
use crate::workspace::McpActivityDto;

#[cfg(any(unix, test))]
const RECENT_ACTIVITY_WINDOW: Duration = Duration::from_secs(15);
#[cfg(any(unix, test))]
// wait_command can legitimately hold one MCP request for up to 60 seconds, while the
// listener itself has a 90-second hard request timeout. Only warn beyond both normal
// windows so a healthy long poll is not reported as stalled.
const SUSPECTED_STALL_AFTER: Duration = Duration::from_secs(120);

#[cfg(any(unix, test))]
#[derive(Debug, Clone, Copy)]
struct ActivityPoint {
    monotonic: Instant,
    wall_clock: SystemTime,
}

fn tracks_conversation_activity(method: &str) -> bool {
    matches!(method, "tools/call" | "resources/read" | "prompts/get")
}

#[derive(Debug, Clone)]
struct ActiveCall {
    session_id: String,
    #[cfg(any(unix, test))]
    started: ActivityPoint,
    #[cfg(any(unix, test))]
    method: String,
    #[cfg(any(unix, test))]
    tool: String,
}

#[derive(Default)]
struct ActivityState {
    in_flight: HashMap<String, ActiveCall>,
    #[cfg(any(unix, test))]
    last_activity: Option<ActivityPoint>,
    #[cfg(any(unix, test))]
    last_completed_at: Option<SystemTime>,
    #[cfg(any(unix, test))]
    completed_requests: u64,
}

#[derive(Clone, Default)]
pub(crate) struct McpActivityTracker {
    inner: Arc<Mutex<ActivityState>>,
}

pub(crate) struct McpActivityRequestGuard {
    tracker: McpActivityTracker,
    key: Option<String>,
}

impl McpActivityRequestGuard {
    pub(crate) fn complete(mut self) {
        if let Some(key) = self.key.take() {
            self.tracker.finish_request(&key, true);
        }
    }
}

impl Drop for McpActivityRequestGuard {
    fn drop(&mut self) {
        if let Some(key) = self.key.take() {
            self.tracker.finish_request(&key, false);
        }
    }
}

fn registry() -> &'static Mutex<HashMap<String, Weak<Mutex<ActivityState>>>> {
    static REGISTRY: OnceLock<Mutex<HashMap<String, Weak<Mutex<ActivityState>>>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(crate) fn register_activity(workspace_id: &str) -> McpActivityTracker {
    let tracker = McpActivityTracker::default();
    registry()
        .lock()
        .expect("MCP activity registry lock")
        .insert(workspace_id.to_string(), Arc::downgrade(&tracker.inner));
    tracker
}

#[cfg(any(unix, test))]
pub(crate) fn activity_snapshot(workspace_id: &str) -> McpActivityDto {
    let inner = {
        let mut entries = registry().lock().expect("MCP activity registry lock");
        match entries.get(workspace_id).and_then(Weak::upgrade) {
            Some(inner) => Some(inner),
            None => {
                entries.remove(workspace_id);
                None
            }
        }
    };
    inner
        .map(|inner| McpActivityTracker { inner }.snapshot())
        .unwrap_or_else(unknown_snapshot)
}

impl McpActivityTracker {
    pub(crate) fn request_started(
        &self,
        session_id: &str,
        request_id: &Value,
        method: &str,
        _tool: &str,
    ) -> Option<McpActivityRequestGuard> {
        if !tracks_conversation_activity(method) {
            return None;
        }
        let key = request_key(session_id, request_id)?;
        let mut state = self.inner.lock().expect("MCP activity lock");
        #[cfg(any(unix, test))]
        let point = {
            let point = activity_point();
            state.last_activity = Some(point);
            point
        };
        state.in_flight.insert(
            key.clone(),
            ActiveCall {
                session_id: session_id.to_string(),
                #[cfg(any(unix, test))]
                started: point,
                #[cfg(any(unix, test))]
                method: method.to_string(),
                #[cfg(any(unix, test))]
                tool: _tool.to_string(),
            },
        );
        Some(McpActivityRequestGuard {
            tracker: self.clone(),
            key: Some(key),
        })
    }

    fn finish_request(&self, key: &str, completed: bool) {
        let mut state = self.inner.lock().expect("MCP activity lock");
        let removed = state.in_flight.remove(key).is_some();
        #[cfg(any(unix, test))]
        if removed {
            let point = activity_point();
            state.last_activity = Some(point);
            if completed {
                state.last_completed_at = Some(point.wall_clock);
                state.completed_requests = state.completed_requests.saturating_add(1);
            }
        }
        #[cfg(not(any(unix, test)))]
        let _ = (removed, completed);
    }

    pub(crate) fn cancel_session(&self, session_id: &str) {
        let mut state = self.inner.lock().expect("MCP activity lock");
        #[cfg(any(unix, test))]
        let before = state.in_flight.len();
        state
            .in_flight
            .retain(|_, call| call.session_id != session_id);
        #[cfg(any(unix, test))]
        if state.in_flight.len() != before {
            state.last_activity = Some(activity_point());
        }
    }

    #[cfg(any(unix, test))]
    pub(crate) fn snapshot(&self) -> McpActivityDto {
        self.snapshot_with_thresholds(RECENT_ACTIVITY_WINDOW, SUSPECTED_STALL_AFTER)
    }

    #[cfg(any(unix, test))]
    fn snapshot_with_thresholds(
        &self,
        recent_window: Duration,
        suspected_stall_after: Duration,
    ) -> McpActivityDto {
        let now = Instant::now();
        let state = self.inner.lock().expect("MCP activity lock");
        let oldest = state
            .in_flight
            .values()
            .min_by_key(|call| call.started.monotonic);
        let oldest_age = oldest.map(|call| now.saturating_duration_since(call.started.monotonic));
        let last_activity_age = state
            .last_activity
            .map(|point| now.saturating_duration_since(point.monotonic));

        let (activity_state, message) =
            if oldest_age.is_some_and(|age| age >= suspected_stall_after) {
                let seconds = oldest_age.unwrap_or_default().as_secs();
                (
                    "suspected_stalled",
                    format!("最早的 MCP 调用已持续 {seconds} 秒，超过正常请求窗口，可能异常"),
                )
            } else if !state.in_flight.is_empty() {
                (
                    "active",
                    format!("正在处理 {} 个 MCP 调用", state.in_flight.len()),
                )
            } else if last_activity_age.is_some_and(|age| age <= recent_window) {
                ("recent", "刚刚完成或收到 MCP 调用".to_string())
            } else if state.last_activity.is_some() {
                ("idle", "当前没有在途 MCP 调用".to_string())
            } else {
                ("idle", "尚未收到 MCP 调用".to_string())
            };

        McpActivityDto {
            state: activity_state.into(),
            message,
            in_flight_requests: state.in_flight.len() as u64,
            oldest_in_flight_ms: oldest_age.map(duration_ms),
            last_activity_at: state
                .last_activity
                .map(|point| format_system_time(point.wall_clock)),
            last_activity_age_ms: last_activity_age.map(duration_ms),
            last_completed_at: state.last_completed_at.map(format_system_time),
            current_method: oldest.map(|call| call.method.clone()).unwrap_or_default(),
            current_tool: oldest.map(|call| call.tool.clone()).unwrap_or_default(),
            completed_requests: state.completed_requests,
            recent_window_ms: duration_ms(recent_window),
            suspected_stall_after_ms: duration_ms(suspected_stall_after),
        }
    }
}

#[cfg(any(unix, test))]
fn unknown_snapshot() -> McpActivityDto {
    McpActivityDto {
        state: "unknown".into(),
        message: "当前控制进程无法读取 MCP 调用活跃度".into(),
        in_flight_requests: 0,
        oldest_in_flight_ms: None,
        last_activity_at: None,
        last_activity_age_ms: None,
        last_completed_at: None,
        current_method: String::new(),
        current_tool: String::new(),
        completed_requests: 0,
        recent_window_ms: duration_ms(RECENT_ACTIVITY_WINDOW),
        suspected_stall_after_ms: duration_ms(SUSPECTED_STALL_AFTER),
    }
}

fn request_key(session_id: &str, request_id: &Value) -> Option<String> {
    serde_json::to_string(request_id)
        .ok()
        .map(|request_id| format!("{session_id}:{request_id}"))
}

#[cfg(any(unix, test))]
fn activity_point() -> ActivityPoint {
    ActivityPoint {
        monotonic: Instant::now(),
        wall_clock: SystemTime::now(),
    }
}

#[cfg(any(unix, test))]
fn duration_ms(duration: Duration) -> u64 {
    duration.as_millis().min(u64::MAX as u128) as u64
}

#[cfg(any(unix, test))]
fn format_system_time(time: SystemTime) -> String {
    chrono::DateTime::<Utc>::from(time).to_rfc3339_opts(SecondsFormat::Millis, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn activity_transitions_between_idle_active_stalled_and_recent() {
        let tracker = McpActivityTracker::default();
        assert_eq!(tracker.snapshot().state, "idle");

        let request = tracker
            .request_started("session", &json!(1), "tools/call", "read_file")
            .expect("tracked request");
        let active =
            tracker.snapshot_with_thresholds(Duration::from_secs(15), Duration::from_secs(60));
        assert_eq!(active.state, "active");
        assert_eq!(active.in_flight_requests, 1);
        assert_eq!(active.current_tool, "read_file");

        let stalled = tracker.snapshot_with_thresholds(Duration::from_secs(15), Duration::ZERO);
        assert_eq!(stalled.state, "suspected_stalled");

        request.complete();
        let recent = tracker.snapshot();
        assert_eq!(recent.state, "recent");
        assert_eq!(recent.in_flight_requests, 0);
        assert_eq!(recent.completed_requests, 1);
    }

    #[test]
    fn dropped_request_guard_clears_in_flight_without_counting_completion() {
        let tracker = McpActivityTracker::default();
        {
            let _request = tracker
                .request_started("session", &json!(1), "tools/call", "wait_command")
                .expect("tracked request");
            assert_eq!(tracker.snapshot().in_flight_requests, 1);
        }

        let snapshot = tracker.snapshot();
        assert_eq!(snapshot.in_flight_requests, 0);
        assert_eq!(snapshot.completed_requests, 0);
        assert_eq!(snapshot.state, "recent");
    }

    #[test]
    fn protocol_and_catalog_requests_do_not_keep_conversation_activity_alive() {
        let tracker = McpActivityTracker::default();
        for (id, method) in [(1, "ping"), (2, "initialize"), (3, "tools/list")] {
            assert!(tracker
                .request_started("session", &json!(id), method, "")
                .is_none());
        }

        let snapshot = tracker.snapshot();
        assert_eq!(snapshot.state, "idle");
        assert_eq!(snapshot.completed_requests, 0);
    }
}
