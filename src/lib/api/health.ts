import { invokeAdmin } from "$lib/api/invoke";

export interface HealthItem {
  label: string;
  ok: boolean;
  detail: string;
  hint: string;
}

export async function runHealthChecks(workspaceId: string): Promise<HealthItem[]> {
  return invokeAdmin<HealthItem[]>("run_health_checks", { id: workspaceId });
}
