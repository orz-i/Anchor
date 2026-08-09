use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use futures_util::future::join_all;
use serde::{Deserialize, Serialize};

use crate::error::{AppError, AppResult};
use crate::gateway_control::{
    self, GatewayControlStatus, GatewayEvent, GatewayEventBatch, GatewayEventCursor,
};
use crate::workspace::WorkspaceProfile;

use super::protocol::{ControlEvent, ControlEventBatch, ControlEventCursor, ControlService};
use super::WorkspaceControlStatus;

const MAX_CONTROL_PLANE_EVENT_BATCH: u32 = 64;
const MAX_CONTROL_PLANE_EVENT_WAIT_MS: u32 = 25_000;
const AGGREGATE_WAIT_SLICE_MS: u32 = 1_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlPlaneStatus {
    pub gateway: GatewayControlStatus,
    pub workspaces: Vec<ControlPlaneWorkspaceStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlPlaneWorkspaceStatus {
    #[serde(flatten)]
    pub status: WorkspaceControlStatus,
    pub mcp_state: String,
    pub actions_state: String,
}

pub async fn control_plane_status(profiles: &[WorkspaceProfile]) -> AppResult<ControlPlaneStatus> {
    let gateway_future = gateway_control::status_via_daemon_or_local();
    let workspace_futures = profiles.iter().cloned().map(|profile| async move {
        super::workspace_status_via_daemon_or_local(&profile)
            .await
            .map_err(|error| {
                AppError::Message(format!(
                    "Workspace {} 控制状态读取失败：{error}",
                    profile.name
                ))
            })
    });
    let (gateway, workspace_results) = tokio::join!(gateway_future, join_all(workspace_futures));
    let gateway = gateway?;
    let mut workspaces = Vec::with_capacity(workspace_results.len());
    for result in workspace_results {
        let status = result?;
        let mcp_state = workspace_service_state(&status, ControlService::Mcp, &gateway).into();
        let actions_state =
            workspace_service_state(&status, ControlService::Actions, &gateway).into();
        workspaces.push(ControlPlaneWorkspaceStatus {
            status,
            mcp_state,
            actions_state,
        });
    }
    Ok(ControlPlaneStatus {
        gateway,
        workspaces,
    })
}

pub fn workspace_service_state(
    status: &WorkspaceControlStatus,
    service: ControlService,
    gateway: &GatewayControlStatus,
) -> &'static str {
    if service == ControlService::Mcp
        && gateway.running
        && gateway.route_workspace_ids.contains(&status.id)
    {
        if gateway.state == "error" {
            return "error";
        }
        if status.mcp.listening
            && ((status.mcp.pid == gateway.pid && gateway.pid.is_some())
                || (!gateway.daemon_supported && status.mcp.owner == "server"))
        {
            return "running";
        }
        if status.mcp.listening {
            return "error";
        }
        return "recovering";
    }

    let (port, selected) = match service {
        ControlService::Mcp => (
            &status.mcp,
            status
                .daemon
                .state
                .as_ref()
                .filter(|_| status.daemon.running)
                .is_some_and(|state| state.service.includes_mcp()),
        ),
        ControlService::Actions => (
            &status.actions,
            status
                .daemon
                .state
                .as_ref()
                .filter(|_| status.daemon.running)
                .is_some_and(|state| state.service.includes_actions()),
        ),
    };
    if status.daemon.ambiguous || (status.daemon.stale && status.daemon.state.is_some()) {
        "error"
    } else if (selected && port.owner == "daemon") || port.owner == "server" {
        "running"
    } else if port.owner == "external" {
        "error"
    } else if selected && status.daemon.running {
        "recovering"
    } else {
        "stopped"
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ControlPlaneEventCursor {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway: Option<GatewayEventCursor>,
    #[serde(default)]
    pub workspaces: BTreeMap<String, ControlEventCursor>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum ControlPlaneEventSource {
    Gateway,
    Workspace {
        #[serde(rename = "workspaceId")]
        workspace_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum ControlPlaneEvent {
    Gateway {
        event: GatewayEvent,
    },
    Workspace {
        #[serde(rename = "workspaceId")]
        workspace_id: String,
        event: ControlEvent,
    },
}

impl ControlPlaneEvent {
    fn emitted_at_unix_ms(&self) -> u64 {
        match self {
            Self::Gateway { event } => event.emitted_at_unix_ms,
            Self::Workspace { event, .. } => event.emitted_at_unix_ms,
        }
    }

    fn sequence(&self) -> u64 {
        match self {
            Self::Gateway { event } => event.sequence,
            Self::Workspace { event, .. } => event.sequence,
        }
    }

    fn source_key(&self) -> &str {
        match self {
            Self::Gateway { .. } => "",
            Self::Workspace { workspace_id, .. } => workspace_id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlPlaneEventBatch {
    pub events: Vec<ControlPlaneEvent>,
    pub next_cursor: ControlPlaneEventCursor,
    pub reset_sources: Vec<ControlPlaneEventSource>,
}

struct SourceEventBatches {
    gateway: Option<GatewayEventBatch>,
    workspaces: Vec<(String, Option<ControlEventBatch>)>,
}

pub async fn control_plane_events(
    profiles: &[WorkspaceProfile],
    cursor: Option<ControlPlaneEventCursor>,
    limit: u32,
    wait_ms: u32,
) -> AppResult<ControlPlaneEventBatch> {
    let limit = limit.clamp(1, MAX_CONTROL_PLANE_EVENT_BATCH) as usize;
    let wait_ms = wait_ms.min(MAX_CONTROL_PLANE_EVENT_WAIT_MS);
    let mut cursor = cursor.unwrap_or_default();
    cursor
        .workspaces
        .retain(|workspace_id, _| profiles.iter().any(|profile| profile.id == *workspace_id));
    let deadline = tokio::time::Instant::now() + Duration::from_millis(u64::from(wait_ms));

    loop {
        let remaining_ms = deadline
            .saturating_duration_since(tokio::time::Instant::now())
            .as_millis()
            .min(u128::from(u32::MAX)) as u32;
        let slice_wait_ms = if wait_ms == 0 {
            0
        } else {
            remaining_ms.min(AGGREGATE_WAIT_SLICE_MS)
        };
        let source_limit = u32::try_from(limit)
            .unwrap_or(MAX_CONTROL_PLANE_EVENT_BATCH)
            .clamp(1, 32);
        let slice_started = tokio::time::Instant::now();
        let batches =
            read_source_event_batches(profiles, &cursor, source_limit, slice_wait_ms).await?;
        let batch = merge_event_batches(&cursor, batches, limit);
        cursor = batch.next_cursor.clone();
        if !batch.events.is_empty()
            || !batch.reset_sources.is_empty()
            || wait_ms == 0
            || tokio::time::Instant::now() >= deadline
        {
            return Ok(batch);
        }
        let slice_budget = Duration::from_millis(u64::from(slice_wait_ms));
        let elapsed = slice_started.elapsed();
        if elapsed < slice_budget {
            tokio::time::sleep(slice_budget - elapsed).await;
        }
    }
}

async fn read_source_event_batches(
    profiles: &[WorkspaceProfile],
    cursor: &ControlPlaneEventCursor,
    limit: u32,
    wait_ms: u32,
) -> AppResult<SourceEventBatches> {
    let gateway_cursor = cursor.gateway.clone();
    let gateway_future = async move {
        match gateway_control::request_events(gateway_cursor, limit, wait_ms).await {
            Ok(batch) => Ok(Some(batch)),
            Err(error) if error.is_unavailable() => Ok(None),
            Err(error) => Err(AppError::Message(format!(
                "Gateway event control failed: {error}"
            ))),
        }
    };
    let workspace_futures = profiles.iter().cloned().map(|profile| {
        let source_cursor = cursor.workspaces.get(&profile.id).cloned();
        async move {
            let workspace_id = profile.id.clone();
            match super::request_events(&profile, source_cursor, limit, wait_ms).await {
                Ok(batch) => Ok((workspace_id, Some(batch))),
                Err(error) if error.is_unavailable() => Ok((workspace_id, None)),
                Err(error) => Err(AppError::Message(format!(
                    "Workspace {} event control failed: {error}",
                    profile.name
                ))),
            }
        }
    });
    let (gateway, workspace_results) = tokio::join!(gateway_future, join_all(workspace_futures));
    let mut workspaces = Vec::with_capacity(workspace_results.len());
    for result in workspace_results {
        workspaces.push(result?);
    }
    Ok(SourceEventBatches {
        gateway: gateway?,
        workspaces,
    })
}

fn merge_event_batches(
    cursor: &ControlPlaneEventCursor,
    batches: SourceEventBatches,
    limit: usize,
) -> ControlPlaneEventBatch {
    let mut pending = Vec::new();
    let mut reset_sources = Vec::new();
    if let Some(batch) = &batches.gateway {
        if batch.reset {
            reset_sources.push(ControlPlaneEventSource::Gateway);
        }
        pending.extend(
            batch
                .events
                .iter()
                .cloned()
                .map(|event| ControlPlaneEvent::Gateway { event }),
        );
    }
    for (workspace_id, batch) in &batches.workspaces {
        let Some(batch) = batch else {
            continue;
        };
        if batch.reset {
            reset_sources.push(ControlPlaneEventSource::Workspace {
                workspace_id: workspace_id.clone(),
            });
        }
        pending.extend(
            batch
                .events
                .iter()
                .cloned()
                .map(|event| ControlPlaneEvent::Workspace {
                    workspace_id: workspace_id.clone(),
                    event,
                }),
        );
    }
    pending.sort_by(|left, right| {
        left.emitted_at_unix_ms()
            .cmp(&right.emitted_at_unix_ms())
            .then_with(|| left.source_key().cmp(right.source_key()))
            .then_with(|| left.sequence().cmp(&right.sequence()))
    });
    pending.truncate(limit);

    let included_gateway_sequence = pending
        .iter()
        .filter_map(|event| match event {
            ControlPlaneEvent::Gateway { event } => Some(event.sequence),
            ControlPlaneEvent::Workspace { .. } => None,
        })
        .max();
    let mut included_workspace_sequences = HashMap::new();
    for event in &pending {
        if let ControlPlaneEvent::Workspace {
            workspace_id,
            event,
        } = event
        {
            included_workspace_sequences
                .entry(workspace_id.clone())
                .and_modify(|sequence: &mut u64| *sequence = (*sequence).max(event.sequence))
                .or_insert(event.sequence);
        }
    }

    let mut next_cursor = cursor.clone();
    if let Some(batch) = &batches.gateway {
        next_cursor.gateway = Some(advance_gateway_cursor(
            cursor.gateway.as_ref(),
            batch,
            included_gateway_sequence,
        ));
    }
    for (workspace_id, batch) in &batches.workspaces {
        let Some(batch) = batch else {
            continue;
        };
        let next = advance_workspace_cursor(
            cursor.workspaces.get(workspace_id),
            batch,
            included_workspace_sequences.get(workspace_id).copied(),
        );
        next_cursor.workspaces.insert(workspace_id.clone(), next);
    }

    ControlPlaneEventBatch {
        events: pending,
        next_cursor,
        reset_sources,
    }
}

fn advance_gateway_cursor(
    previous: Option<&GatewayEventCursor>,
    batch: &GatewayEventBatch,
    included_sequence: Option<u64>,
) -> GatewayEventCursor {
    if let Some(sequence) = included_sequence {
        return GatewayEventCursor {
            stream_id: batch.next_cursor.stream_id.clone(),
            sequence,
        };
    }
    if batch.events.is_empty() {
        return batch.next_cursor.clone();
    }
    if !batch.reset
        && previous.is_some_and(|cursor| cursor.stream_id == batch.next_cursor.stream_id)
    {
        return previous.expect("cursor checked above").clone();
    }
    GatewayEventCursor {
        stream_id: batch.next_cursor.stream_id.clone(),
        sequence: batch.events[0].sequence.saturating_sub(1),
    }
}

fn advance_workspace_cursor(
    previous: Option<&ControlEventCursor>,
    batch: &ControlEventBatch,
    included_sequence: Option<u64>,
) -> ControlEventCursor {
    if let Some(sequence) = included_sequence {
        return ControlEventCursor {
            stream_id: batch.next_cursor.stream_id.clone(),
            sequence,
        };
    }
    if batch.events.is_empty() {
        return batch.next_cursor.clone();
    }
    if !batch.reset
        && previous.is_some_and(|cursor| cursor.stream_id == batch.next_cursor.stream_id)
    {
        return previous.expect("cursor checked above").clone();
    }
    ControlEventCursor {
        stream_id: batch.next_cursor.stream_id.clone(),
        sequence: batch.events[0].sequence.saturating_sub(1),
    }
}

#[cfg(test)]
mod tests {
    use super::super::PortStatus;
    use super::*;
    use crate::daemon::DaemonInspection;
    use crate::gateway_control::GatewayEventKind;

    fn gateway_status(route_workspace_ids: Vec<String>, pid: Option<u32>) -> GatewayControlStatus {
        GatewayControlStatus {
            daemon_supported: true,
            running: true,
            pid,
            build_identity: None,
            state: "running".into(),
            local_endpoint: "http://127.0.0.1:28765".into(),
            public_base_url: String::new(),
            route_count: route_workspace_ids.len(),
            route_workspace_ids,
            owner_workspace_id: "owner".into(),
            error: String::new(),
            detail: "running".into(),
        }
    }

    fn workspace_status(id: &str, mcp_pid: Option<u32>) -> WorkspaceControlStatus {
        WorkspaceControlStatus {
            id: id.into(),
            name: id.into(),
            path: ".".into(),
            daemon: DaemonInspection {
                supported: true,
                running: false,
                stale: false,
                ambiguous: false,
                pid_matches: false,
                state: None,
                detail: "stopped".into(),
            },
            mcp: PortStatus {
                service: "mcp".into(),
                port: 28_001,
                listening: mcp_pid.is_some(),
                pid: mcp_pid,
                owner: if mcp_pid.is_some() {
                    "external"
                } else {
                    "none"
                }
                .into(),
                endpoint: "http://127.0.0.1:28001/mcp".into(),
            },
            actions: PortStatus {
                service: "actions".into(),
                port: 28_002,
                listening: false,
                pid: None,
                owner: "none".into(),
                endpoint: "http://127.0.0.1:28002".into(),
            },
            mcp_activity: None,
            mcp_tunnel: None,
            actions_tunnel: None,
        }
    }

    #[test]
    fn gateway_route_pid_is_owned_by_gateway_control_domain() {
        let status = workspace_status("route-a", Some(42));
        let gateway = gateway_status(vec!["route-a".into()], Some(42));
        assert_eq!(
            workspace_service_state(&status, ControlService::Mcp, &gateway),
            "running"
        );

        let wrong_pid = workspace_status("route-a", Some(99));
        assert_eq!(
            workspace_service_state(&wrong_pid, ControlService::Mcp, &gateway),
            "error"
        );
    }

    #[test]
    fn aggregate_limit_does_not_advance_an_event_that_was_not_emitted() {
        let input = ControlPlaneEventCursor::default();
        let gateway_batch = GatewayEventBatch {
            events: vec![GatewayEvent {
                sequence: 1,
                emitted_at_unix_ms: 1,
                kind: GatewayEventKind::DaemonReady,
                state: "running".into(),
                message: "ready".into(),
            }],
            next_cursor: GatewayEventCursor {
                stream_id: "gateway-stream".into(),
                sequence: 1,
            },
            reset: false,
        };
        let workspace_batch = ControlEventBatch {
            events: vec![ControlEvent {
                sequence: 1,
                emitted_at_unix_ms: 2,
                kind: super::super::protocol::ControlEventKind::DaemonReady,
                service: None,
                state: "running".into(),
                message: "ready".into(),
            }],
            next_cursor: ControlEventCursor {
                stream_id: "workspace-stream".into(),
                sequence: 1,
            },
            reset: false,
        };
        let merged = merge_event_batches(
            &input,
            SourceEventBatches {
                gateway: Some(gateway_batch),
                workspaces: vec![("workspace-a".into(), Some(workspace_batch))],
            },
            1,
        );
        assert_eq!(merged.events.len(), 1);
        assert!(matches!(
            merged.events[0],
            ControlPlaneEvent::Gateway { .. }
        ));
        assert_eq!(
            merged.next_cursor.gateway.expect("gateway cursor").sequence,
            1
        );
        assert_eq!(
            merged
                .next_cursor
                .workspaces
                .get("workspace-a")
                .expect("workspace cursor")
                .sequence,
            0
        );
    }
}
