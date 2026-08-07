use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, OnceLock};
#[cfg(any(unix, test))]
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::sync::Notify;

use super::protocol::{ControlEvent, ControlEventKind, ControlService};
#[cfg(any(unix, test))]
use super::protocol::{ControlEventBatch, ControlEventCursor};

const MAX_RETAINED_EVENTS: usize = 256;
pub const MAX_EVENT_BATCH: u32 = 32;
pub const MAX_EVENT_WAIT_MS: u32 = 25_000;
const MAX_EVENT_STATE_BYTES: usize = 64;
const MAX_EVENT_MESSAGE_BYTES: usize = 512;

#[derive(Debug)]
struct WorkspaceEventStream {
    stream_id: String,
    next_sequence: u64,
    events: VecDeque<ControlEvent>,
    notify: Arc<Notify>,
}

fn bounded_text(mut value: String, max_bytes: usize) -> String {
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

impl WorkspaceEventStream {
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

fn streams() -> &'static Mutex<HashMap<String, WorkspaceEventStream>> {
    static STREAMS: OnceLock<Mutex<HashMap<String, WorkspaceEventStream>>> = OnceLock::new();
    STREAMS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn reset_workspace_event_stream(workspace_id: &str) {
    let mut streams = streams()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    match streams.get_mut(workspace_id) {
        Some(stream) => stream.reset(),
        None => {
            streams.insert(workspace_id.to_string(), WorkspaceEventStream::new());
        }
    }
}

pub fn publish_workspace_event(
    workspace_id: &str,
    kind: ControlEventKind,
    service: Option<ControlService>,
    state: impl Into<String>,
    message: impl Into<String>,
) {
    let notify = {
        let mut streams = streams()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let stream = streams
            .entry(workspace_id.to_string())
            .or_insert_with(WorkspaceEventStream::new);
        let sequence = stream.next_sequence;
        stream.next_sequence = stream.next_sequence.saturating_add(1);
        stream.events.push_back(ControlEvent {
            sequence,
            emitted_at_unix_ms: unix_time_ms(),
            kind,
            service,
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

#[cfg(any(unix, test))]
pub async fn read_workspace_events(
    workspace_id: &str,
    cursor: Option<&ControlEventCursor>,
    limit: u32,
    wait_ms: u32,
) -> ControlEventBatch {
    let limit = limit.clamp(1, MAX_EVENT_BATCH) as usize;
    let wait_ms = wait_ms.min(MAX_EVENT_WAIT_MS);

    let notify = event_notify(workspace_id);
    let mut notified = Box::pin(notify.notified());
    notified.as_mut().enable();
    let batch = snapshot_events(workspace_id, cursor, limit);
    if !batch.events.is_empty() || batch.reset || wait_ms == 0 {
        return batch;
    }

    let _ = tokio::time::timeout(Duration::from_millis(u64::from(wait_ms)), notified).await;
    snapshot_events(workspace_id, cursor, limit)
}

#[cfg(any(unix, test))]
fn event_notify(workspace_id: &str) -> Arc<Notify> {
    let mut streams = streams()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    Arc::clone(
        &streams
            .entry(workspace_id.to_string())
            .or_insert_with(WorkspaceEventStream::new)
            .notify,
    )
}

#[cfg(any(unix, test))]
fn snapshot_events(
    workspace_id: &str,
    cursor: Option<&ControlEventCursor>,
    limit: usize,
) -> ControlEventBatch {
    let mut streams = streams()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let stream = streams
        .entry(workspace_id.to_string())
        .or_insert_with(WorkspaceEventStream::new);
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

    ControlEventBatch {
        events,
        next_cursor: ControlEventCursor {
            stream_id: stream.stream_id.clone(),
            sequence: next_sequence,
        },
        reset,
    }
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

    #[tokio::test]
    async fn event_cursor_resumes_and_resets_across_stream_restart() {
        let workspace_id = format!("events-{}", uuid::Uuid::new_v4());
        reset_workspace_event_stream(&workspace_id);
        publish_workspace_event(
            &workspace_id,
            ControlEventKind::DaemonReady,
            None,
            "running",
            "ready",
        );
        let first = read_workspace_events(&workspace_id, None, 8, 0).await;
        assert_eq!(first.events.len(), 1);
        assert!(!first.reset);
        assert_eq!(first.events[0].sequence, 1);

        publish_workspace_event(
            &workspace_id,
            ControlEventKind::ServiceState,
            Some(ControlService::Mcp),
            "recovering",
            "retrying",
        );
        let resumed = read_workspace_events(&workspace_id, Some(&first.next_cursor), 8, 0).await;
        assert_eq!(resumed.events.len(), 1);
        assert_eq!(resumed.events[0].sequence, 2);
        assert!(!resumed.reset);

        let previous_cursor = resumed.next_cursor;
        reset_workspace_event_stream(&workspace_id);
        let reset = read_workspace_events(&workspace_id, Some(&previous_cursor), 8, 0).await;
        assert!(reset.reset);
        assert_ne!(reset.next_cursor.stream_id, previous_cursor.stream_id);
    }

    #[tokio::test]
    async fn event_long_poll_wakes_after_publish() {
        let workspace_id = format!("events-wait-{}", uuid::Uuid::new_v4());
        reset_workspace_event_stream(&workspace_id);
        let initial = read_workspace_events(&workspace_id, None, 8, 0).await;
        let cursor = initial.next_cursor;
        let waiter_workspace = workspace_id.clone();
        let waiter = tokio::spawn(async move {
            read_workspace_events(&waiter_workspace, Some(&cursor), 8, 5_000).await
        });
        tokio::task::yield_now().await;
        publish_workspace_event(
            &workspace_id,
            ControlEventKind::McpActivity,
            Some(ControlService::Mcp),
            "active",
            "tool call started",
        );
        let batch = waiter.await.expect("waiter");
        assert_eq!(batch.events.len(), 1);
        assert_eq!(batch.events[0].kind, ControlEventKind::McpActivity);
    }

    #[tokio::test]
    async fn event_batches_bound_hostile_content_and_frame_growth() {
        let workspace_id = format!("events-hostile-{}", uuid::Uuid::new_v4());
        reset_workspace_event_stream(&workspace_id);
        for _ in 0..64 {
            publish_workspace_event(
                &workspace_id,
                ControlEventKind::ServiceState,
                Some(ControlService::Mcp),
                "s".repeat(1_000),
                "\"\\".repeat(5_000),
            );
        }

        let batch = read_workspace_events(&workspace_id, None, 64, 0).await;
        assert_eq!(batch.events.len(), MAX_EVENT_BATCH as usize);
        assert!(batch
            .events
            .iter()
            .all(|event| event.state.len() <= MAX_EVENT_STATE_BYTES));
        assert!(batch
            .events
            .iter()
            .all(|event| event.message.len() <= MAX_EVENT_MESSAGE_BYTES));
        let encoded = serde_json::to_vec(&batch).expect("serialize event batch");
        assert!(encoded.len() < super::super::protocol::MAX_CONTROL_FRAME_BYTES);
    }
}
