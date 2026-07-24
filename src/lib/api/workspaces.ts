import { invoke } from "@tauri-apps/api/core";
import type { RuntimeStatus, SkillInspection, WorkspaceProfile } from "$lib/types";

export async function listWorkspaces(): Promise<WorkspaceProfile[]> {
  return invoke<WorkspaceProfile[]>("list_workspaces");
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
  return invoke<SkillInspection>("inspect_workspace_skills", { id, enabled, roots });
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
  return invoke<RuntimeStatus>("get_runtime_status", { id });
}

export async function startActionsRuntime(id: string): Promise<RuntimeStatus> {
  return invoke<RuntimeStatus>("start_actions_runtime", { id });
}

export async function stopActionsRuntime(id: string): Promise<RuntimeStatus> {
  return invoke<RuntimeStatus>("stop_actions_runtime", { id });
}

export async function getActionsRuntimeStatus(id: string): Promise<RuntimeStatus> {
  return invoke<RuntimeStatus>("get_actions_runtime_status", { id });
}

export async function restartRuntime(id: string): Promise<RuntimeStatus> {
  return invoke<RuntimeStatus>("restart_runtime", { id });
}

export async function restartActionsRuntime(id: string): Promise<RuntimeStatus> {
  return invoke<RuntimeStatus>("restart_actions_runtime", { id });
}
