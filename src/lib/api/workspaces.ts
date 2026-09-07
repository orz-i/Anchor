import { invokeAdmin, invokeRead } from "@/lib/api/invoke";
import type {
  ControlPlaneEventBatch,
  ControlPlaneEventCursor,
  ControlPlaneStatus,
  FederationHandshakeValidation,
  FederationBootstrapBundle,
  FederationDiscoveryDocument,
  FederationDiscoveryInspection,
  FederationLocalSigningStatus,
  FederationPeerCandidate,
  FederationPeerCredentialStatus,
  FederationPeerDescriptor,
  FederationPeerRegistration,
  FederationPeerUpdate,
  FederationPeerView,
  FederationReadPlan,
  FederationReadRequest,
  FederationReadResult,
  FederationRemoteTarget,
  GatewayEventBatch,
  GatewayEventCursor,
  GatewayLogChunk,
  OrchestrationInspection,
  OrchestrationPlan,
  OrchestrationWorkflowSpec,
  RuntimeStatus,
  SkillChannel,
  SkillInspection,
  SkillPackage,
  WorkspaceProfile,
} from "@/lib/types";

export async function listWorkspaces(): Promise<WorkspaceProfile[]> {
  return invokeRead<WorkspaceProfile[]>("list_workspaces");
}

export async function planOrchestrationWorkflow(
  workflow: OrchestrationWorkflowSpec,
): Promise<OrchestrationPlan> {
  return invokeRead<OrchestrationPlan>("plan_orchestration_workflow", { workflow });
}

export async function inspectOrchestrationWorkflow(
  workflow: OrchestrationWorkflowSpec,
): Promise<OrchestrationInspection> {
  return invokeRead<OrchestrationInspection>("inspect_orchestration_workflow", { workflow });
}

export async function acceptFederationPeerRebootstrap(
  nodeId: string,
  discovery: FederationDiscoveryDocument,
  grantId: string,
): Promise<FederationPeerView> {
  return invokeAdmin<FederationPeerView>("accept_federation_peer_rebootstrap", {
    nodeId,
    discovery,
    grantId,
  });
}

export async function getFederationSigningStatus(): Promise<FederationLocalSigningStatus> {
  return invokeRead<FederationLocalSigningStatus>("get_federation_signing_status");
}

export async function getFederationBootstrapBundle(): Promise<FederationBootstrapBundle> {
  return invokeRead<FederationBootstrapBundle>("get_federation_bootstrap_bundle");
}

export async function getFederationDiscoveryDocument(): Promise<FederationDiscoveryDocument> {
  return invokeRead<FederationDiscoveryDocument>("get_federation_discovery_document");
}

export async function rotateFederationSigningKey(
  grantId: string,
): Promise<FederationLocalSigningStatus> {
  return invokeAdmin<FederationLocalSigningStatus>("rotate_federation_signing_key", { grantId });
}

export async function revokeFederationPeer(
  nodeId: string,
  grantId: string,
): Promise<FederationPeerView> {
  return invokeAdmin<FederationPeerView>("revoke_federation_peer", { nodeId, grantId });
}

export async function rotateFederationPeerCredential(
  target: FederationRemoteTarget,
  token: string,
  grantId: string,
): Promise<FederationPeerCredentialStatus> {
  return invokeAdmin<FederationPeerCredentialStatus>("rotate_federation_peer_credential", {
    target,
    token,
    grantId,
  });
}

export async function listFederationPeers(): Promise<FederationPeerView[]> {
  return invokeRead<FederationPeerView[]>("list_federation_peers");
}

export async function getFederationPeer(nodeId: string): Promise<FederationPeerView> {
  return invokeRead<FederationPeerView>("get_federation_peer", { nodeId });
}

export async function inspectFederationPeerDiscovery(
  nodeId: string,
): Promise<FederationDiscoveryInspection> {
  return invokeRead<FederationDiscoveryInspection>("inspect_federation_peer_discovery", {
    nodeId,
  });
}

export async function inspectFederationCandidate(
  endpoint: string,
  displayName: string,
  bundle: FederationBootstrapBundle,
): Promise<FederationPeerCandidate> {
  return invokeRead<FederationPeerCandidate>("inspect_federation_candidate", {
    endpoint,
    displayName,
    bundle,
  });
}

export async function registerFederationPeer(
  registration: FederationPeerRegistration,
  grantId: string,
): Promise<FederationPeerView> {
  return invokeAdmin<FederationPeerView>("register_federation_peer", {
    ...registration,
    grantId,
  });
}

export async function updateFederationPeer(
  update: FederationPeerUpdate,
  grantId: string,
): Promise<FederationPeerView> {
  return invokeAdmin<FederationPeerView>("update_federation_peer", {
    ...update,
    grantId,
  });
}

export async function trustFederationPeer(
  nodeId: string,
  grantId: string,
): Promise<FederationPeerView> {
  return invokeAdmin<FederationPeerView>("trust_federation_peer", { nodeId, grantId });
}

export async function removeFederationPeer(nodeId: string, grantId: string): Promise<void> {
  await invokeAdmin<null>("remove_federation_peer", { nodeId, grantId });
}

export async function getFederationPeerCredentialStatus(
  target: FederationRemoteTarget,
): Promise<FederationPeerCredentialStatus> {
  return invokeRead<FederationPeerCredentialStatus>("get_federation_peer_credential_status", {
    target,
  });
}

export async function setFederationPeerCredential(
  target: FederationRemoteTarget,
  token: string,
  grantId: string,
): Promise<FederationPeerCredentialStatus> {
  return invokeAdmin<FederationPeerCredentialStatus>("set_federation_peer_credential", {
    target,
    token,
    grantId,
  });
}

export async function clearFederationPeerCredential(
  target: FederationRemoteTarget,
  grantId: string,
): Promise<FederationPeerCredentialStatus> {
  return invokeAdmin<FederationPeerCredentialStatus>("clear_federation_peer_credential", {
    target,
    grantId,
  });
}

export async function probeFederationPeer(
  target: FederationRemoteTarget,
): Promise<FederationPeerView> {
  return invokeAdmin<FederationPeerView>("probe_federation_peer", { target });
}

export async function readFederationRemote(
  target: FederationRemoteTarget,
  request: FederationReadRequest,
): Promise<FederationReadResult> {
  return invokeRead<FederationReadResult>("read_federation_remote", { target, request });
}

export async function getControlPlaneStatus(): Promise<ControlPlaneStatus> {
  return invokeRead<ControlPlaneStatus>("get_control_plane_status");
}

export async function getFederationCatalog(): Promise<FederationPeerDescriptor> {
  return invokeRead<FederationPeerDescriptor>("get_federation_catalog");
}

export async function validateFederationPeer(
  peer: FederationPeerDescriptor,
): Promise<FederationHandshakeValidation> {
  return invokeRead<FederationHandshakeValidation>("validate_federation_peer", { peer });
}

export async function resolveFederationRead(
  request: FederationReadRequest,
): Promise<FederationReadPlan> {
  return invokeRead<FederationReadPlan>("resolve_federation_read", {
    nodeId: request.nodeId,
    workspaceId: request.workspaceId,
    operation: request.operation,
    context: request.context,
  });
}

export async function getControlPlaneEvents(
  cursor: ControlPlaneEventCursor | null,
  waitMs = 15_000,
): Promise<ControlPlaneEventBatch> {
  return invokeRead<ControlPlaneEventBatch>("get_control_plane_events", {
    cursor,
    waitMs,
  });
}

export async function getGatewayControlEvents(
  cursor: GatewayEventCursor | null,
  waitMs = 15_000,
): Promise<GatewayEventBatch | null> {
  return invokeRead<GatewayEventBatch | null>("get_gateway_control_events", {
    cursor,
    waitMs,
  });
}

export async function readGatewayLogs(lines = 100): Promise<GatewayLogChunk> {
  return invokeRead<GatewayLogChunk>("read_gateway_logs", { lines });
}

export async function createWorkspace(
  path: string,
  name?: string,
): Promise<WorkspaceProfile> {
  return invokeAdmin<WorkspaceProfile>("create_workspace", { path, name });
}

export async function updateWorkspace(
  profile: WorkspaceProfile,
  baseProfile: WorkspaceProfile,
): Promise<void> {
  await invokeAdmin("stage_workspace_config", { baseProfile, profile });
  await invokeAdmin("apply_workspace_config", { id: profile.id, waitSeconds: 20 });
}

export async function inspectWorkspaceSkills(
  id: string,
  enabled: boolean,
): Promise<SkillInspection> {
  return invokeRead<SkillInspection>("inspect_workspace_skills", { id, enabled });
}

export async function installWorkspaceSkillPackage(
  id: string,
  path: string,
  channel: SkillChannel,
  activate = false,
): Promise<SkillPackage> {
  return invokeAdmin<SkillPackage>("install_workspace_skill_package", { id, path, channel, activate });
}

export async function setWorkspaceSkillChannel(
  id: string,
  name: string,
  channel: SkillChannel,
  version: string,
): Promise<SkillPackage> {
  return invokeAdmin<SkillPackage>("set_workspace_skill_channel", { id, name, channel, version });
}

export async function activateWorkspaceSkillPackage(
  id: string,
  name: string,
  channel: SkillChannel,
): Promise<SkillPackage> {
  return invokeAdmin<SkillPackage>("activate_workspace_skill_package", { id, name, channel });
}

export async function rollbackWorkspaceSkillPackage(
  id: string,
  name: string,
): Promise<SkillPackage> {
  return invokeAdmin<SkillPackage>("rollback_workspace_skill_package", { id, name });
}

export async function removeWorkspaceSkillPackage(
  id: string,
  name: string,
  version: string,
): Promise<SkillPackage> {
  return invokeAdmin<SkillPackage>("remove_workspace_skill_package", { id, name, version });
}

export async function openWorkspaceDirectory(path: string): Promise<void> {
  return invokeAdmin("open_workspace_directory", { path });
}

export async function deleteWorkspace(id: string): Promise<void> {
  return invokeAdmin("delete_workspace", { id });
}

export async function startRuntime(id: string): Promise<RuntimeStatus> {
  return invokeAdmin<RuntimeStatus>("start_runtime", { id });
}

export async function stopRuntime(id: string): Promise<RuntimeStatus> {
  return invokeAdmin<RuntimeStatus>("stop_runtime", { id });
}

export async function getRuntimeStatus(id: string): Promise<RuntimeStatus> {
  return invokeRead<RuntimeStatus>("get_runtime_status", { id });
}
