use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::path::Path;

use futures_util::stream::{self, StreamExt};
use serde::{Deserialize, Serialize};

use crate::error::{AppError, AppResult};
use crate::federation::{
    FederationContextRef, FederationContextScope, FederationNodeControlStatus,
    FederationReadOperation, FederationReadRequest, FederationReadResult, FederationRemoteTarget,
    FederationWorkspaceStatus,
};
use crate::runtime::RuntimeCapabilitySnapshot;
use crate::workspace::WorkspaceProfile;

pub const ORCHESTRATION_SCHEMA_VERSION: u16 = 1;
pub const ORCHESTRATION_CONTRACT: &str = "anchor-orchestration-v1";

const MAX_TARGETS: usize = 32;
const MAX_STEPS: usize = 64;
const MAX_DEPENDENCIES_PER_STEP: usize = 16;
const MAX_REFERENCE_ID_BYTES: usize = 128;
const MAX_OBSERVATION_CONCURRENCY: usize = 4;
const ORCHESTRATION_STATE_AUTHORITY: &str = "existing_harness_federation_control_plane";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrchestrationTargetKind {
    Node,
    Workspace,
    HarnessTask,
}

fn observation_record(
    step: OrchestrationPlanStep,
    result: AppResult<OrchestrationObservation>,
) -> OrchestrationStepObservation {
    match result {
        Ok(observation) => OrchestrationStepObservation {
            step_id: step.id,
            target_id: step.target.id,
            state: OrchestrationObservationState::Observed,
            observation: Some(observation),
            error: None,
        },
        Err(error) => OrchestrationStepObservation {
            step_id: step.id,
            target_id: step.target.id,
            state: OrchestrationObservationState::Unavailable,
            observation: None,
            error: Some(bounded_error(&error.to_string())),
        },
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OrchestrationTarget {
    pub id: String,
    pub kind: OrchestrationTargetKind,
    pub node_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OrchestrationFleetSpec {
    pub id: String,
    pub targets: Vec<OrchestrationTarget>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrchestrationReadOperation {
    NodeCapabilities,
    NodeControlStatus,
    WorkspaceStatus,
    HarnessTaskStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OrchestrationStepSpec {
    pub id: String,
    pub target_id: String,
    pub operation: OrchestrationReadOperation,
    #[serde(default)]
    pub depends_on: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OrchestrationWorkflowSpec {
    pub schema_version: u16,
    pub contract: String,
    pub id: String,
    pub fleet: OrchestrationFleetSpec,
    pub steps: Vec<OrchestrationStepSpec>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrchestrationReadSource {
    RuntimeCapabilityProvider,
    ExistingControlPlane,
    HarnessStore,
    FederationRead,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OrchestrationPlanStep {
    pub id: String,
    pub target: OrchestrationTarget,
    pub operation: OrchestrationReadOperation,
    pub depends_on: Vec<String>,
    pub source: OrchestrationReadSource,
    pub remote: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub federation_request: Option<FederationReadRequest>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OrchestrationPlan {
    pub schema_version: u16,
    pub contract: String,
    pub workflow_id: String,
    pub fleet_id: String,
    pub read_only: bool,
    pub state_authority: String,
    pub waves: Vec<Vec<String>>,
    pub steps: Vec<OrchestrationPlanStep>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrchestrationObservationState {
    Observed,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OrchestrationHarnessTaskStatus {
    pub workspace_id: String,
    pub task_id: String,
    pub status: String,
    pub progress_percent: u8,
    pub current: bool,
    pub active: bool,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum OrchestrationObservation {
    NodeCapabilities(RuntimeCapabilitySnapshot),
    NodeControlStatus(FederationNodeControlStatus),
    WorkspaceStatus(FederationWorkspaceStatus),
    HarnessTaskStatus(OrchestrationHarnessTaskStatus),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OrchestrationStepObservation {
    pub step_id: String,
    pub target_id: String,
    pub state: OrchestrationObservationState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observation: Option<OrchestrationObservation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OrchestrationInspectionSummary {
    pub observed: usize,
    pub unavailable: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OrchestrationInspection {
    pub plan: OrchestrationPlan,
    pub observations: Vec<OrchestrationStepObservation>,
    pub summary: OrchestrationInspectionSummary,
}

pub fn plan_workflow(spec: &OrchestrationWorkflowSpec) -> AppResult<OrchestrationPlan> {
    validate_workflow_contract(spec)?;
    let local_node_id = crate::runtime::capability_snapshot(None)?.node.id;
    plan_workflow_for_node(spec, &local_node_id)
}

pub async fn inspect_workflow(
    spec: &OrchestrationWorkflowSpec,
) -> AppResult<OrchestrationInspection> {
    let plan = plan_workflow(spec)?;
    let local_node_id = crate::runtime::capability_snapshot(None)?.node.id;
    let store = crate::data::DataStore::load()?;
    let profiles = store
        .list()
        .iter()
        .cloned()
        .map(|profile| (profile.id.clone(), profile))
        .collect::<BTreeMap<_, _>>();
    drop(store);

    let results = execute_plan_waves(&plan, |step| {
        let profiles = &profiles;
        let local_node_id = &local_node_id;
        async move { observe_step(&step, local_node_id, profiles).await }
    })
    .await?;

    let observations = results
        .into_iter()
        .map(|(_, step, result)| observation_record(step, result))
        .collect::<Vec<_>>();
    let observed = observations
        .iter()
        .filter(|item| item.state == OrchestrationObservationState::Observed)
        .count();
    let unavailable = observations.len().saturating_sub(observed);

    Ok(OrchestrationInspection {
        plan,
        observations,
        summary: OrchestrationInspectionSummary {
            observed,
            unavailable,
        },
    })
}

async fn execute_plan_waves<T, F, Fut>(
    plan: &OrchestrationPlan,
    execute: F,
) -> AppResult<Vec<(usize, OrchestrationPlanStep, T)>>
where
    F: Fn(OrchestrationPlanStep) -> Fut,
    Fut: Future<Output = T>,
{
    let step_index = plan
        .steps
        .iter()
        .cloned()
        .enumerate()
        .map(|(index, step)| (step.id.clone(), (index, step)))
        .collect::<BTreeMap<_, _>>();
    let mut results = Vec::with_capacity(plan.steps.len());
    for wave in &plan.waves {
        let wave_steps = wave
            .iter()
            .map(|step_id| {
                step_index.get(step_id).cloned().ok_or_else(|| {
                    AppError::Message(format!(
                        "ORCHESTRATION_PLAN_INVALID: dependency wave references unknown step `{step_id}`"
                    ))
                })
            })
            .collect::<AppResult<Vec<_>>>()?;
        let futures = wave_steps.into_iter().map(|(index, step)| {
            let future = execute(step.clone());
            async move { (index, step, future.await) }
        });
        results.extend(
            stream::iter(futures)
                .buffer_unordered(MAX_OBSERVATION_CONCURRENCY)
                .collect::<Vec<_>>()
                .await,
        );
    }
    if results.len() != plan.steps.len() {
        return Err(AppError::Message(
            "ORCHESTRATION_PLAN_INVALID: dependency waves do not cover every planned step".into(),
        ));
    }
    results.sort_by_key(|(index, _, _)| *index);
    Ok(results)
}

fn target_identity(target: &OrchestrationTarget) -> String {
    format!(
        "{:?}\0{}\0{}\0{}",
        target.kind,
        target.node_id,
        target.workspace_id.as_deref().unwrap_or_default(),
        target.task_id.as_deref().unwrap_or_default()
    )
}

fn validate_workflow_contract(spec: &OrchestrationWorkflowSpec) -> AppResult<()> {
    if spec.schema_version != ORCHESTRATION_SCHEMA_VERSION
        || spec.contract != ORCHESTRATION_CONTRACT
    {
        return Err(AppError::Message(
            "ORCHESTRATION_CONTRACT_MISMATCH: workflow contract is incompatible".into(),
        ));
    }
    validate_reference_id(&spec.id, "workflow")?;
    validate_reference_id(&spec.fleet.id, "fleet")?;
    if spec.fleet.targets.is_empty() || spec.fleet.targets.len() > MAX_TARGETS {
        return Err(AppError::Message(format!(
            "ORCHESTRATION_TARGET_LIMIT: workflow must contain 1..={MAX_TARGETS} targets"
        )));
    }
    if spec.steps.is_empty() || spec.steps.len() > MAX_STEPS {
        return Err(AppError::Message(format!(
            "ORCHESTRATION_STEP_LIMIT: workflow must contain 1..={MAX_STEPS} steps"
        )));
    }
    Ok(())
}

fn plan_workflow_for_node(
    spec: &OrchestrationWorkflowSpec,
    local_node_id: &str,
) -> AppResult<OrchestrationPlan> {
    validate_workflow_contract(spec)?;
    if !crate::federation::valid_node_id(local_node_id) {
        return Err(AppError::Message(
            "ORCHESTRATION_LOCAL_NODE_INVALID: local node identity is invalid".into(),
        ));
    }

    let mut targets = BTreeMap::new();
    let mut target_identities = BTreeSet::new();
    for target in &spec.fleet.targets {
        validate_target(target)?;
        if !target_identities.insert(target_identity(target)) {
            return Err(AppError::Message(format!(
                "ORCHESTRATION_TARGET_IDENTITY_DUPLICATE: target {} duplicates another fleet target",
                target.id
            )));
        }
        if targets.insert(target.id.clone(), target.clone()).is_some() {
            return Err(AppError::Message(format!(
                "ORCHESTRATION_TARGET_DUPLICATE: target {} is duplicated",
                target.id
            )));
        }
    }

    let mut step_specs = BTreeMap::new();
    for step in &spec.steps {
        validate_reference_id(&step.id, "step")?;
        validate_reference_id(&step.target_id, "target reference")?;
        if step.depends_on.len() > MAX_DEPENDENCIES_PER_STEP {
            return Err(AppError::Message(format!(
                "ORCHESTRATION_DEPENDENCY_LIMIT: step {} exceeds {MAX_DEPENDENCIES_PER_STEP} dependencies",
                step.id
            )));
        }
        let unique_dependencies = step.depends_on.iter().collect::<BTreeSet<_>>();
        if unique_dependencies.len() != step.depends_on.len() {
            return Err(AppError::Message(format!(
                "ORCHESTRATION_DEPENDENCY_DUPLICATE: step {} contains duplicate dependencies",
                step.id
            )));
        }
        if step
            .depends_on
            .iter()
            .any(|dependency| dependency == &step.id)
        {
            return Err(AppError::Message(format!(
                "ORCHESTRATION_DEPENDENCY_SELF: step {} cannot depend on itself",
                step.id
            )));
        }
        if step_specs.insert(step.id.clone(), step.clone()).is_some() {
            return Err(AppError::Message(format!(
                "ORCHESTRATION_STEP_DUPLICATE: step {} is duplicated",
                step.id
            )));
        }
    }

    for step in step_specs.values() {
        if !targets.contains_key(&step.target_id) {
            return Err(AppError::Message(format!(
                "ORCHESTRATION_TARGET_UNKNOWN: step {} references unknown target {}",
                step.id, step.target_id
            )));
        }
        for dependency in &step.depends_on {
            if !step_specs.contains_key(dependency) {
                return Err(AppError::Message(format!(
                    "ORCHESTRATION_DEPENDENCY_UNKNOWN: step {} references unknown dependency {}",
                    step.id, dependency
                )));
            }
        }
    }

    let waves = dependency_waves(&step_specs)?;
    let mut steps = Vec::with_capacity(spec.steps.len());
    for step in &spec.steps {
        let target = targets.get(&step.target_id).cloned().ok_or_else(|| {
            AppError::Message(
                "ORCHESTRATION_TARGET_UNKNOWN: target disappeared during planning".into(),
            )
        })?;
        let remote = target.node_id != local_node_id;
        validate_operation_target(step.operation, &target, remote)?;
        let (source, federation_request) = plan_source(step.operation, &target, remote)?;
        steps.push(OrchestrationPlanStep {
            id: step.id.clone(),
            target,
            operation: step.operation,
            depends_on: step.depends_on.clone(),
            source,
            remote,
            federation_request,
        });
    }

    Ok(OrchestrationPlan {
        schema_version: ORCHESTRATION_SCHEMA_VERSION,
        contract: ORCHESTRATION_CONTRACT.into(),
        workflow_id: spec.id.clone(),
        fleet_id: spec.fleet.id.clone(),
        read_only: true,
        state_authority: ORCHESTRATION_STATE_AUTHORITY.into(),
        waves,
        steps,
    })
}

fn validate_target(target: &OrchestrationTarget) -> AppResult<()> {
    validate_reference_id(&target.id, "target")?;
    if !crate::federation::valid_node_id(&target.node_id) {
        return Err(AppError::Message(format!(
            "ORCHESTRATION_NODE_INVALID: target {} has an invalid nodeId",
            target.id
        )));
    }
    match target.kind {
        OrchestrationTargetKind::Node => {
            if target.workspace_id.is_some() || target.task_id.is_some() {
                return Err(AppError::Message(format!(
                    "ORCHESTRATION_TARGET_BINDING_INVALID: node target {} cannot bind workspaceId or taskId",
                    target.id
                )));
            }
        }
        OrchestrationTargetKind::Workspace => {
            let workspace_id = target.workspace_id.as_deref().ok_or_else(|| {
                AppError::Message(format!(
                    "ORCHESTRATION_WORKSPACE_REQUIRED: workspace target {} requires workspaceId",
                    target.id
                ))
            })?;
            validate_workspace_id(workspace_id)?;
            if target.task_id.is_some() {
                return Err(AppError::Message(format!(
                    "ORCHESTRATION_TARGET_BINDING_INVALID: workspace target {} cannot bind taskId",
                    target.id
                )));
            }
        }
        OrchestrationTargetKind::HarnessTask => {
            let workspace_id = target.workspace_id.as_deref().ok_or_else(|| {
                AppError::Message(format!(
                    "ORCHESTRATION_WORKSPACE_REQUIRED: Harness task target {} requires workspaceId",
                    target.id
                ))
            })?;
            validate_workspace_id(workspace_id)?;
            let task_id = target.task_id.as_deref().ok_or_else(|| {
                AppError::Message(format!(
                    "ORCHESTRATION_TASK_REQUIRED: Harness task target {} requires taskId",
                    target.id
                ))
            })?;
            validate_reference_id(task_id, "Harness task")?;
        }
    }
    Ok(())
}

fn validate_operation_target(
    operation: OrchestrationReadOperation,
    target: &OrchestrationTarget,
    remote: bool,
) -> AppResult<()> {
    let compatible = matches!(
        (operation, target.kind),
        (
            OrchestrationReadOperation::NodeCapabilities
                | OrchestrationReadOperation::NodeControlStatus,
            OrchestrationTargetKind::Node
        ) | (
            OrchestrationReadOperation::WorkspaceStatus,
            OrchestrationTargetKind::Workspace
        ) | (
            OrchestrationReadOperation::HarnessTaskStatus,
            OrchestrationTargetKind::HarnessTask
        )
    );
    if !compatible {
        return Err(AppError::Message(format!(
            "ORCHESTRATION_OPERATION_TARGET_MISMATCH: step operation {:?} is incompatible with target {}",
            operation, target.id
        )));
    }
    if remote && operation == OrchestrationReadOperation::HarnessTaskStatus {
        return Err(AppError::Message(
            "ORCHESTRATION_REMOTE_HARNESS_UNAVAILABLE: federation v2 does not export remote Harness task state"
                .into(),
        ));
    }
    Ok(())
}

fn plan_source(
    operation: OrchestrationReadOperation,
    target: &OrchestrationTarget,
    remote: bool,
) -> AppResult<(OrchestrationReadSource, Option<FederationReadRequest>)> {
    if remote {
        let request = federation_request(operation, target)?;
        return Ok((OrchestrationReadSource::FederationRead, Some(request)));
    }
    Ok((
        match operation {
            OrchestrationReadOperation::NodeCapabilities => {
                OrchestrationReadSource::RuntimeCapabilityProvider
            }
            OrchestrationReadOperation::NodeControlStatus
            | OrchestrationReadOperation::WorkspaceStatus => {
                OrchestrationReadSource::ExistingControlPlane
            }
            OrchestrationReadOperation::HarnessTaskStatus => OrchestrationReadSource::HarnessStore,
        },
        None,
    ))
}

fn dependency_waves(
    steps: &BTreeMap<String, OrchestrationStepSpec>,
) -> AppResult<Vec<Vec<String>>> {
    let mut remaining = steps
        .iter()
        .map(|(id, step)| {
            (
                id.clone(),
                step.depends_on.iter().cloned().collect::<BTreeSet<_>>(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut completed = BTreeSet::new();
    let mut waves = Vec::new();
    while !remaining.is_empty() {
        let ready = remaining
            .iter()
            .filter(|(_, dependencies)| dependencies.is_subset(&completed))
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        if ready.is_empty() {
            return Err(AppError::Message(
                "ORCHESTRATION_DAG_CYCLE: workflow dependencies contain a cycle".into(),
            ));
        }
        for id in &ready {
            remaining.remove(id);
            completed.insert(id.clone());
        }
        waves.push(ready);
    }
    Ok(waves)
}

async fn observe_step(
    step: &OrchestrationPlanStep,
    local_node_id: &str,
    profiles: &BTreeMap<String, WorkspaceProfile>,
) -> AppResult<OrchestrationObservation> {
    if step.operation == OrchestrationReadOperation::HarnessTaskStatus {
        return observe_harness_task(&step.target, profiles);
    }

    let request = step
        .federation_request
        .clone()
        .map(Ok)
        .unwrap_or_else(|| federation_request(step.operation, &step.target))?;
    let result = if step.target.node_id == local_node_id {
        crate::federation::execute_local_read(&request).await?
    } else {
        let peer = crate::federation::get_peer(&step.target.node_id)?;
        let remote = FederationRemoteTarget {
            node_id: peer.peer.node_id,
            endpoint: peer.peer.endpoint,
        };
        crate::federation::read_trusted_peer(&remote, &request).await?
    };
    observation_from_federation(step.operation, result)
}

fn observe_harness_task(
    target: &OrchestrationTarget,
    profiles: &BTreeMap<String, WorkspaceProfile>,
) -> AppResult<OrchestrationObservation> {
    let workspace_id = target.workspace_id.as_deref().ok_or_else(|| {
        AppError::Message(
            "ORCHESTRATION_WORKSPACE_REQUIRED: Harness task requires workspaceId".into(),
        )
    })?;
    let task_id = target.task_id.as_deref().ok_or_else(|| {
        AppError::Message("ORCHESTRATION_TASK_REQUIRED: Harness task requires taskId".into())
    })?;
    let profile = profiles.get(workspace_id).ok_or_else(|| {
        AppError::Message(format!(
            "ORCHESTRATION_WORKSPACE_NOT_FOUND: local workspace {workspace_id} is not registered"
        ))
    })?;
    let tasks = crate::canvs::list_workspace_tasks(Path::new(&profile.path))
        .map_err(|error| AppError::Message(error.to_string()))?;
    if tasks.workspace_id != workspace_id {
        return Err(AppError::Message(
            "ORCHESTRATION_HARNESS_WORKSPACE_MISMATCH: Harness store resolved a different workspace identity"
                .into(),
        ));
    }
    let task = tasks.tasks.into_iter().find(|task| task.id == task_id).ok_or_else(|| {
        AppError::Message(format!(
            "ORCHESTRATION_TASK_NOT_FOUND: Harness task {task_id} was not found in workspace {workspace_id}"
        ))
    })?;
    Ok(OrchestrationObservation::HarnessTaskStatus(
        harness_task_status(workspace_id, task),
    ))
}

fn harness_task_status(
    workspace_id: &str,
    task: crate::canvs::CanvsTask,
) -> OrchestrationHarnessTaskStatus {
    OrchestrationHarnessTaskStatus {
        workspace_id: workspace_id.into(),
        task_id: task.id,
        status: task.status,
        progress_percent: task.progress_percent,
        current: task.current,
        active: task.active,
        updated_at: task.updated_at,
    }
}

fn observation_from_federation(
    operation: OrchestrationReadOperation,
    result: FederationReadResult,
) -> AppResult<OrchestrationObservation> {
    match (operation, result) {
        (OrchestrationReadOperation::NodeCapabilities, FederationReadResult::NodeCapabilities(value)) => {
            Ok(OrchestrationObservation::NodeCapabilities(value))
        }
        (OrchestrationReadOperation::NodeControlStatus, FederationReadResult::NodeControlStatus(value)) => {
            Ok(OrchestrationObservation::NodeControlStatus(value))
        }
        (OrchestrationReadOperation::WorkspaceStatus, FederationReadResult::WorkspaceStatus(value)) => {
            Ok(OrchestrationObservation::WorkspaceStatus(value))
        }
        _ => Err(AppError::Message(
            "ORCHESTRATION_RESULT_MISMATCH: authoritative provider returned an unexpected result kind"
                .into(),
        )),
    }
}

fn federation_request(
    operation: OrchestrationReadOperation,
    target: &OrchestrationTarget,
) -> AppResult<FederationReadRequest> {
    let (operation, scope, workspace_id) = match operation {
        OrchestrationReadOperation::NodeCapabilities => (
            FederationReadOperation::NodeCapabilities,
            FederationContextScope::NodeLocal,
            None,
        ),
        OrchestrationReadOperation::NodeControlStatus => (
            FederationReadOperation::NodeControlStatus,
            FederationContextScope::NodeLocal,
            None,
        ),
        OrchestrationReadOperation::WorkspaceStatus => (
            FederationReadOperation::WorkspaceStatus,
            FederationContextScope::WorkspaceLocal,
            target.workspace_id.clone(),
        ),
        OrchestrationReadOperation::HarnessTaskStatus => {
            return Err(AppError::Message(
                "ORCHESTRATION_REMOTE_HARNESS_UNAVAILABLE: Harness state is not a federation v2 read operation"
                    .into(),
            ));
        }
    };
    Ok(FederationReadRequest {
        node_id: target.node_id.clone(),
        workspace_id: workspace_id.clone(),
        operation,
        context: FederationContextRef {
            scope,
            node_id: Some(target.node_id.clone()),
            workspace_id,
        },
    })
}

fn validate_reference_id(value: &str, label: &str) -> AppResult<()> {
    if value.is_empty()
        || value.len() > MAX_REFERENCE_ID_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(AppError::Message(format!(
            "ORCHESTRATION_REFERENCE_INVALID: {label} id must be 1..={MAX_REFERENCE_ID_BYTES} ASCII letters, numbers, hyphens, or underscores"
        )));
    }
    Ok(())
}

fn validate_workspace_id(value: &str) -> AppResult<()> {
    if value.len() != 32 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(AppError::Message(
            "ORCHESTRATION_WORKSPACE_INVALID: workspaceId must be a stable 32-character hex id"
                .into(),
        ));
    }
    Ok(())
}

fn bounded_error(value: &str) -> String {
    const MAX_ERROR_BYTES: usize = 600;
    if value.len() <= MAX_ERROR_BYTES {
        return value.to_string();
    }
    let mut end = MAX_ERROR_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local_node() -> String {
        "node_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into()
    }

    fn remote_node() -> String {
        "node_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into()
    }

    fn workspace_id() -> String {
        "0123456789abcdef0123456789abcdef".into()
    }

    fn base_spec() -> OrchestrationWorkflowSpec {
        OrchestrationWorkflowSpec {
            schema_version: ORCHESTRATION_SCHEMA_VERSION,
            contract: ORCHESTRATION_CONTRACT.into(),
            id: "workflow_a".into(),
            fleet: OrchestrationFleetSpec {
                id: "fleet_a".into(),
                targets: vec![
                    OrchestrationTarget {
                        id: "local_node".into(),
                        kind: OrchestrationTargetKind::Node,
                        node_id: local_node(),
                        workspace_id: None,
                        task_id: None,
                    },
                    OrchestrationTarget {
                        id: "remote_workspace".into(),
                        kind: OrchestrationTargetKind::Workspace,
                        node_id: remote_node(),
                        workspace_id: Some(workspace_id()),
                        task_id: None,
                    },
                ],
            },
            steps: vec![
                OrchestrationStepSpec {
                    id: "node".into(),
                    target_id: "local_node".into(),
                    operation: OrchestrationReadOperation::NodeCapabilities,
                    depends_on: vec![],
                },
                OrchestrationStepSpec {
                    id: "workspace".into(),
                    target_id: "remote_workspace".into(),
                    operation: OrchestrationReadOperation::WorkspaceStatus,
                    depends_on: vec!["node".into()],
                },
            ],
        }
    }

    #[test]
    fn planner_builds_deterministic_read_only_waves_and_sources() {
        let plan = plan_workflow_for_node(&base_spec(), &local_node()).expect("plan");
        assert!(plan.read_only);
        assert_eq!(plan.state_authority, ORCHESTRATION_STATE_AUTHORITY);
        assert_eq!(plan.waves, vec![vec!["node"], vec!["workspace"]]);
        assert_eq!(
            plan.steps[0].source,
            OrchestrationReadSource::RuntimeCapabilityProvider
        );
        assert!(!plan.steps[0].remote);
        assert_eq!(
            plan.steps[1].source,
            OrchestrationReadSource::FederationRead
        );
        assert!(plan.steps[1].remote);
        let request = plan.steps[1]
            .federation_request
            .as_ref()
            .expect("remote request");
        assert_eq!(request.operation, FederationReadOperation::WorkspaceStatus);
        assert_eq!(
            request.context.scope,
            FederationContextScope::WorkspaceLocal
        );
    }

    #[test]
    fn planner_rejects_cycles_unknown_dependencies_and_duplicate_targets() {
        let mut cycle = base_spec();
        cycle.steps[0].depends_on = vec!["workspace".into()];
        assert!(plan_workflow_for_node(&cycle, &local_node())
            .expect_err("cycle")
            .to_string()
            .contains("ORCHESTRATION_DAG_CYCLE"));

        let mut unknown = base_spec();
        unknown.steps[1].depends_on = vec!["missing".into()];
        assert!(plan_workflow_for_node(&unknown, &local_node())
            .expect_err("unknown dependency")
            .to_string()
            .contains("ORCHESTRATION_DEPENDENCY_UNKNOWN"));

        let mut duplicate = base_spec();
        duplicate
            .fleet
            .targets
            .push(duplicate.fleet.targets[0].clone());
        assert!(plan_workflow_for_node(&duplicate, &local_node())
            .expect_err("duplicate target")
            .to_string()
            .contains("ORCHESTRATION_TARGET_IDENTITY_DUPLICATE"));

        let mut alias_duplicate = base_spec();
        let mut alias = alias_duplicate.fleet.targets[0].clone();
        alias.id = "same_node_alias".into();
        alias_duplicate.fleet.targets.push(alias);
        assert!(plan_workflow_for_node(&alias_duplicate, &local_node())
            .expect_err("duplicate target identity")
            .to_string()
            .contains("ORCHESTRATION_TARGET_IDENTITY_DUPLICATE"));
    }

    #[test]
    fn planner_rejects_remote_harness_state_and_target_shape_confusion() {
        let mut remote_task = base_spec();
        remote_task.fleet.targets = vec![OrchestrationTarget {
            id: "remote_task".into(),
            kind: OrchestrationTargetKind::HarnessTask,
            node_id: remote_node(),
            workspace_id: Some(workspace_id()),
            task_id: Some("task_a".into()),
        }];
        remote_task.steps = vec![OrchestrationStepSpec {
            id: "task".into(),
            target_id: "remote_task".into(),
            operation: OrchestrationReadOperation::HarnessTaskStatus,
            depends_on: vec![],
        }];
        assert!(plan_workflow_for_node(&remote_task, &local_node())
            .expect_err("remote Harness must fail closed")
            .to_string()
            .contains("ORCHESTRATION_REMOTE_HARNESS_UNAVAILABLE"));

        let mut confused = base_spec();
        confused.fleet.targets[0].workspace_id = Some(workspace_id());
        assert!(plan_workflow_for_node(&confused, &local_node())
            .expect_err("node target cannot carry workspace")
            .to_string()
            .contains("ORCHESTRATION_TARGET_BINDING_INVALID"));
    }

    #[test]
    fn workflow_wire_contract_contains_no_workspace_paths_or_write_capabilities() {
        let value = serde_json::to_value(base_spec()).expect("serialize");
        let encoded = serde_json::to_string(&value).expect("json");
        assert!(!encoded.contains("path"));
        assert!(!encoded.contains("write"));
        assert!(!encoded.contains("exec"));
        assert!(!encoded.contains("git"));
    }

    #[test]
    fn provider_failure_is_reported_as_unavailable_observation() {
        let plan = plan_workflow_for_node(&base_spec(), &local_node()).expect("plan");
        let record = observation_record(
            plan.steps[0].clone(),
            Err(AppError::Message(
                "FEDERATION_REMOTE_TRANSPORT_UNAVAILABLE: peer is offline".into(),
            )),
        );
        assert_eq!(record.state, OrchestrationObservationState::Unavailable);
        assert!(record.observation.is_none());
        assert!(record
            .error
            .as_deref()
            .is_some_and(|error| error.contains("FEDERATION_REMOTE_TRANSPORT_UNAVAILABLE")));
    }

    #[tokio::test]
    async fn inspection_execution_honors_dependency_waves() {
        use std::sync::{Arc, Mutex};

        let plan = plan_workflow_for_node(&base_spec(), &local_node()).expect("plan");
        let events = Arc::new(Mutex::new(Vec::<String>::new()));
        let results = execute_plan_waves(&plan, {
            let events = events.clone();
            move |step| {
                let events = events.clone();
                async move {
                    events
                        .lock()
                        .expect("events")
                        .push(format!("start:{}", step.id));
                    if step.id == "node" {
                        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                    }
                    events
                        .lock()
                        .expect("events")
                        .push(format!("end:{}", step.id));
                    step.id
                }
            }
        })
        .await
        .expect("execute waves");

        assert_eq!(results.len(), 2);
        let events = events.lock().expect("events").clone();
        let node_end = events
            .iter()
            .position(|event| event == "end:node")
            .expect("node end");
        let workspace_start = events
            .iter()
            .position(|event| event == "start:workspace")
            .expect("workspace start");
        assert!(node_end < workspace_start, "events={events:?}");
    }
}
