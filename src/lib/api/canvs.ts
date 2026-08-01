import { invokeRead } from "$lib/api/invoke";

export type CanvsTaskStatus =
  | "active"
  | "paused"
  | "verifying"
  | "failed"
  | "completed"
  | "completed_unverified"
  | "rolled_back"
  | "unknown";

export interface CanvsTask {
  id: string;
  objective: string;
  status: CanvsTaskStatus;
  current: boolean;
  completedSteps: string[];
  pendingSteps: string[];
  progressPercent: number;
  branch: string | null;
  head: string | null;
  expectedHead: string | null;
  latestChangeId: string | null;
  latestVerificationId: string | null;
  createdAt: string;
  updatedAt: string;
}

export interface CanvsTaskList {
  workspaceId: string;
  tasks: CanvsTask[];
  refreshedAt: string;
}

export interface CanvsEvent {
  id: string;
  kind: string;
  toolName: string | null;
  ok: boolean | null;
  affectedFiles: number;
  createdAt: string;
}

export interface CanvsOperation {
  id: string;
  tool: string;
  kind: string;
  status: string;
  ok: boolean | null;
  affectedFiles: number;
  durationMs: number | null;
  createdAt: string;
}

export interface CanvsChange {
  id: string;
  commitSha: string | null;
  committedFiles: string[];
  verificationCount: number;
  createdAt: string;
}

export interface CanvsVerification {
  id: string;
  kind: string;
  command: string;
  status: string;
  level: string;
  passed: boolean;
  exitCode: number | null;
  durationMs: number | null;
  disposition: string;
  createdAt: string;
}

export interface CanvsSnapshot {
  workspaceId: string;
  task: CanvsTask | null;
  recentEvents: CanvsEvent[];
  recentOperations: CanvsOperation[];
  changes: CanvsChange[];
  verifications: CanvsVerification[];
  refreshedAt: string;
}

export function getCanvsSnapshot(workspaceId: string): Promise<CanvsSnapshot> {
  return invokeRead<CanvsSnapshot>("get_canvs_snapshot", { id: workspaceId }, { attempts: 1 });
}

export function listCanvsTasks(workspaceId: string): Promise<CanvsTaskList> {
  return invokeRead<CanvsTaskList>("list_canvs_tasks", { id: workspaceId }, { attempts: 1 });
}

export function getCanvsTaskSnapshot(
  workspaceId: string,
  taskId: string,
): Promise<CanvsSnapshot> {
  return invokeRead<CanvsSnapshot>(
    "get_canvs_task_snapshot",
    { id: workspaceId, taskId },
    { attempts: 1 },
  );
}
