import { invokeRead } from "@/lib/api/invoke";

export type TaskStatus =
  | "active"
  | "paused"
  | "verifying"
  | "failed"
  | "incomplete"
  | "completed"
  | "completed_unverified"
  | "rolled_back"
  | "unknown";

export interface TaskSummary {
  id: string;
  objective: string;
  status: TaskStatus;
  workspaceMode: "shared" | "worktree";
  current: boolean;
  active: boolean;
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
  lastActivityAt: string | null;
}

export interface AdminTaskEntry {
  workspaceId: string;
  workspaceName: string;
  task: TaskSummary;
}

export interface TaskWorkspaceError {
  workspaceId: string;
  workspaceName: string;
  message: string;
}

export interface TaskListResult {
  tasks: AdminTaskEntry[];
  workspaceErrors: TaskWorkspaceError[];
  refreshedAt: string;
}

export interface TaskEvent {
  id: string;
  kind: string;
  toolName: string | null;
  ok: boolean | null;
  affectedFiles: number;
  createdAt: string;
}

export interface TaskOperation {
  id: string;
  tool: string;
  kind: string;
  status: string;
  ok: boolean | null;
  affectedFiles: number;
  durationMs: number | null;
  createdAt: string;
}

export interface TaskChange {
  id: string;
  commitSha: string | null;
  committedFiles: string[];
  verificationCount: number;
  createdAt: string;
}

export interface TaskVerification {
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

export interface TaskSnapshot {
  workspaceId: string;
  task: TaskSummary | null;
  recentEvents: TaskEvent[];
  recentOperations: TaskOperation[];
  changes: TaskChange[];
  verifications: TaskVerification[];
  refreshedAt: string;
}

export function listTasks(): Promise<TaskListResult> {
  return invokeRead<TaskListResult>("list_tasks", {}, { attempts: 1 });
}

export function getTaskSnapshot(workspaceId: string, taskId: string): Promise<TaskSnapshot> {
  return invokeRead<TaskSnapshot>(
    "get_task_snapshot",
    { id: workspaceId, taskId },
    { attempts: 1 },
  );
}
