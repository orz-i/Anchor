import { invoke } from "@tauri-apps/api/core";
import { invokeRead } from "$lib/api/invoke";
import type {
  ControlPlaneEventBatch,
  ControlPlaneEventCursor,
  ControlPlaneStatus,
  ControlEventBatch,
  ControlEventCursor,
  GatewayEventBatch,
  GatewayEventCursor,
  GatewayLogChunk,
  RuntimeStatus,
  SkillInspection,
  WorkspaceControlStatus,
  WorkspaceProfile,
} from "$lib/types";

export async function listWorkspaces(): Promise<WorkspaceProfile[]> {
  return invokeRead<WorkspaceProfile[]>("list_workspaces");
}

export async function getControlPlaneStatus(): Promise<ControlPlaneStatus> {
  return invokeRead<ControlPlaneStatus>("get_control_plane_status");
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
  return invoke<WorkspaceProfile>("create_workspace", { path, name });
}

export async function updateWorkspace(profile: WorkspaceProfile): Promise<void> {
  return invoke("update_workspace", { profile });
}

export async function inspectWorkspaceSkills(
  id: string,
  enabled: boolean,
  roots: string,
): Promise<SkillInspection> {
  return invokeRead<SkillInspection>("inspect_workspace_skills", { id, enabled, roots });
}

export async function openWorkspaceDirectory(path: string): Promise<void> {
  return invoke("open_workspace_directory", { path });
}

export async function deleteWorkspace(id: string): Promise<void> {
  return invoke("delete_workspace", { id });
}

export async function startRuntime(id: string): Promise<RuntimeStatus> {
  return invoke<RuntimeStatus>("start_runtime", { id });
}

export async function stopRuntime(id: string): Promise<RuntimeStatus> {
  return invoke<RuntimeStatus>("stop_runtime", { id });
}

export async function getRuntimeStatus(id: string): Promise<RuntimeStatus> {
  return invokeRead<RuntimeStatus>("get_runtime_status", { id });
}

export async function getWorkspaceControlStatus(id: string): Promise<WorkspaceControlStatus> {
  return invokeRead<WorkspaceControlStatus>("get_workspace_control_status", { id });
}

export async function getWorkspaceControlEvents(
  id: string,
  cursor: ControlEventCursor | null,
  waitMs = 15_000,
): Promise<ControlEventBatch | null> {
  return invokeRead<ControlEventBatch | null>("get_workspace_control_events", {
    id,
    cursor,
    waitMs,
  });
}

export async function startActionsRuntime(id: string): Promise<RuntimeStatus> {
  return invoke<RuntimeStatus>("start_actions_runtime", { id });
}

export async function stopActionsRuntime(id: string): Promise<RuntimeStatus> {
  return invoke<RuntimeStatus>("stop_actions_runtime", { id });
}

export async function getActionsRuntimeStatus(id: string): Promise<RuntimeStatus> {
  return invokeRead<RuntimeStatus>("get_actions_runtime_status", { id });
}

export async function restartRuntime(id: string): Promise<RuntimeStatus> {
  return invoke<RuntimeStatus>("restart_runtime", { id });
}

export async function restartActionsRuntime(id: string): Promise<RuntimeStatus> {
  return invoke<RuntimeStatus>("restart_actions_runtime", { id });
}
