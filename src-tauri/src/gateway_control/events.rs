use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::sync::Notify;

use super::protocol::{GatewayEvent, GatewayEventBatch, GatewayEventCursor, GatewayEventKind};

const MAX_RETAINED_EVENTS: usize = 256;
pub const MAX_GATEWAY_EVENT_BATCH: u32 = 32;
pub const MAX_GATEWAY_EVENT_WAIT_MS: u32 = 25_000;
const MAX_EVENT_STATE_BYTES: usize = 64;
const MAX_EVENT_MESSAGE_BYTES: usize = 512;

#[derive(Debug)]
struct GatewayEventStream {
    stream_id: String,
    next_sequence: u64,
    events: VecDeque<GatewayEvent>,
    notify: Arc<Notify>,
}

impl GatewayEventStream {
    fn new() -> Self {
        Self {
            stream_id: uuid::Uuid::new_v4().to_string(),
            next_sequence: 1,
            events: VecDeque::new(),
            notify: Arc::new(Notify::new()),
        }
    }

    fn reset(&mut self) {
        self.stream_id = uuid::Uuid::new_v4().to_string();
        self.next_sequence = 1;
        self.events.clear();
        self.notify.notify_waiters();
    }
}

fn streams() -> &'static Mutex<HashMap<String, GatewayEventStream>> {
    static STREAMS: OnceLock<Mutex<HashMap<String, GatewayEventStream>>> = OnceLock::new();
    STREAMS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn reset_gateway_event_stream(config_scope: &str) {
    let mut streams = streams()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    match streams.get_mut(config_scope) {
        Some(stream) => stream.reset(),
        None => {
            streams.insert(config_scope.to_string(), GatewayEventStream::new());
        }
    }
}

pub fn publish_gateway_event(
    config_scope: &str,
    kind: GatewayEventKind,
    state: impl Into<String>,
    message: impl Into<String>,
) {
    let notify = {
        let mut streams = streams()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let stream = streams
            .entry(config_scope.to_string())
            .or_insert_with(GatewayEventStream::new);
        let sequence = stream.next_sequence;
        stream.next_sequence = stream.next_sequence.saturating_add(1);
        stream.events.push_back(GatewayEvent {
            sequence,
            emitted_at_unix_ms: unix_time_ms(),
            kind,
            state: bounded_text(state.into(), MAX_EVENT_STATE_BYTES),
            message: bounded_text(message.into(), MAX_EVENT_MESSAGE_BYTES),
        });
        while stream.events.len() > MAX_RETAINED_EVENTS {
            stream.events.pop_front();
        }
        Arc::clone(&stream.notify)
    };
    notify.notify_waiters();
}

pub async fn read_gateway_events(
    config_scope: &str,
    cursor: Option<&GatewayEventCursor>,
    limit: u32,
    wait_ms: u32,
) -> GatewayEventBatch {
    let limit = limit.clamp(1, MAX_GATEWAY_EVENT_BATCH) as usize;
    let wait_ms = wait_ms.min(MAX_GATEWAY_EVENT_WAIT_MS);
    let notify = event_notify(config_scope);
    let mut notified = Box::pin(notify.notified());
    notified.as_mut().enable();
    let batch = snapshot_events(config_scope, cursor, limit);
    if !batch.events.is_empty() || batch.reset || wait_ms == 0 {
        return batch;
    }
    let _ = tokio::time::timeout(Duration::from_millis(u64::from(wait_ms)), notified).await;
    snapshot_events(config_scope, cursor, limit)
}

fn event_notify(config_scope: &str) -> Arc<Notify> {
    let mut streams = streams()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    Arc::clone(
        &streams
            .entry(config_scope.to_string())
            .or_insert_with(GatewayEventStream::new)
            .notify,
    )
}

fn snapshot_events(
    config_scope: &str,
    cursor: Option<&GatewayEventCursor>,
    limit: usize,
) -> GatewayEventBatch {
    let mut streams = streams()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let stream = streams
        .entry(config_scope.to_string())
        .or_insert_with(GatewayEventStream::new);
    let latest_sequence = stream.next_sequence.saturating_sub(1);
    let earliest_sequence = stream
        .events
        .front()
        .map(|event| event.sequence)
        .unwrap_or(stream.next_sequence);
    let (after_sequence, reset) = match cursor {
        None => (earliest_sequence.saturating_sub(1), false),
        Some(cursor) if cursor.stream_id != stream.stream_id => {
            (earliest_sequence.saturating_sub(1), true)
        }
        Some(cursor)
            if cursor.sequence > latest_sequence
                || cursor.sequence.saturating_add(1) < earliest_sequence =>
        {
            (earliest_sequence.saturating_sub(1), true)
        }
        Some(cursor) => (cursor.sequence, false),
    };
    let events = stream
        .events
        .iter()
        .filter(|event| event.sequence > after_sequence)
        .take(limit)
        .cloned()
        .collect::<Vec<_>>();
    let next_sequence = events
        .last()
        .map(|event| event.sequence)
        .unwrap_or_else(|| after_sequence.min(latest_sequence));
    GatewayEventBatch {
        events,
        next_cursor: GatewayEventCursor {
            stream_id: stream.stream_id.clone(),
            sequence: next_sequence,
        },
        reset,
    }
}

fn bounded_text(mut value: String, max_bytes: usize) -> String {
    if value.chars().any(should_replace_control) {
        value = value
            .chars()
            .map(|ch| {
                if should_replace_control(ch) {
                    '\u{fffd}'
                } else {
                    ch
                }
            })
            .collect();
    }
    if value.len() <= max_bytes {
        return value;
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    value.truncate(end);
    value
}

fn should_replace_control(ch: char) -> bool {
    ch.is_control() && !matches!(ch, '\n' | '\r' | '\t')
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway_control::protocol::{
        GatewayResponse, GatewayResult, MAX_GATEWAY_CONTROL_FRAME_BYTES,
    };

    #[tokio::test]
    async fn gateway_event_cursor_resumes_and_resets_across_restart() {
        let scope = format!("gateway-events-{}", uuid::Uuid::new_v4());
        reset_gateway_event_stream(&scope);
        publish_gateway_event(&scope, GatewayEventKind::DaemonReady, "running", "ready");
        let first = read_gateway_events(&scope, None, 8, 0).await;
        assert_eq!(first.events.len(), 1);
        assert_eq!(first.events[0].sequence, 1);
        assert!(!first.reset);

        publish_gateway_event(&scope, GatewayEventKind::TunnelState, "recovering", "retry");
        let resumed = read_gateway_events(&scope, Some(&first.next_cursor), 8, 0).await;
        assert_eq!(resumed.events.len(), 1);
        assert_eq!(resumed.events[0].sequence, 2);

        let previous_cursor = resumed.next_cursor;
        reset_gateway_event_stream(&scope);
        let reset = read_gateway_events(&scope, Some(&previous_cursor), 8, 0).await;
        assert!(reset.reset);
        assert_ne!(reset.next_cursor.stream_id, previous_cursor.stream_id);
    }

    #[tokio::test]
    async fn gateway_event_long_poll_wakes_after_publish() {
        let scope = format!("gateway-events-wait-{}", uuid::Uuid::new_v4());
        reset_gateway_event_stream(&scope);
        let initial = read_gateway_events(&scope, None, 8, 0).await;
        let cursor = initial.next_cursor;
        let waiter_scope = scope.clone();
        let waiter = tokio::spawn(async move {
            read_gateway_events(&waiter_scope, Some(&cursor), 8, 5_000).await
        });
        tokio::task::yield_now().await;
        publish_gateway_event(&scope, GatewayEventKind::GatewayState, "error", "failed");
        let batch = waiter.await.expect("waiter");
        assert_eq!(batch.events.len(), 1);
        assert_eq!(batch.events[0].kind, GatewayEventKind::GatewayState);
    }

    #[tokio::test]
    async fn hostile_gateway_events_stay_inside_control_frame_budget() {
        let scope = format!("gateway-events-hostile-{}", uuid::Uuid::new_v4());
        reset_gateway_event_stream(&scope);
        for _ in 0..64 {
            publish_gateway_event(
                &scope,
                GatewayEventKind::GatewayState,
                "\0".repeat(1_000),
                "\0".repeat(5_000),
            );
        }
        let batch = read_gateway_events(&scope, None, 64, 0).await;
        assert_eq!(batch.events.len(), MAX_GATEWAY_EVENT_BATCH as usize);
        assert!(batch
            .events
            .iter()
            .all(|event| event.state.len() <= MAX_EVENT_STATE_BYTES));
        assert!(batch
            .events
            .iter()
            .all(|event| event.message.len() <= MAX_EVENT_MESSAGE_BYTES));
        assert!(batch.events.iter().all(|event| {
            !event.state.chars().any(should_replace_control)
                && !event.message.chars().any(should_replace_control)
        }));
        let response =
            GatewayResponse::success("hostile-events".into(), GatewayResult::Events { batch });
        let encoded = serde_json::to_vec(&response).expect("serialize response");
        assert!(encoded.len() < MAX_GATEWAY_CONTROL_FRAME_BYTES);
    }
}
