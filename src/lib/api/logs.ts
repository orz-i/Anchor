import { invokeAdmin } from "$lib/api/invoke";

export interface LogChunk {
  name: string;
  content: string;
}

export type LogService = "mcp" | "actions";

export async function readWorkspaceLogs(
  workspaceId: string,
  service: LogService,
): Promise<LogChunk[]> {
  return invokeAdmin<LogChunk[]>("read_workspace_logs", { id: workspaceId, service });
}
