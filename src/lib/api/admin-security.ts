import { invokeAdmin, invokeRead } from "$lib/api/invoke";
import { isTauriRuntime } from "$lib/platform/runtime";

export interface PrivilegedActionBinding {
  id?: string;
  key?: string;
  kind?: string;
}

export interface PreparedPrivilegedAction {
  confirmationId: string;
  action: string;
  targetSummary: string;
  confirmationText: string;
  expiresInSeconds: number;
}

export interface ApprovedPrivilegedGrant {
  grantId: string;
  action: string;
  expiresInSeconds: number;
}

export interface AdminAuditEvent {
  timestampUnixMs: number;
  sessionFingerprint: string;
  action: string;
  phase: string;
  outcome: string;
}

export async function preparePrivilegedAction(
  action: string,
  binding: PrivilegedActionBinding,
): Promise<PreparedPrivilegedAction> {
  return invokeAdmin<PreparedPrivilegedAction>("prepare_privileged_action", { action, binding });
}

export async function confirmPrivilegedAction(
  confirmationId: string,
  confirmationText: string,
): Promise<ApprovedPrivilegedGrant> {
  return invokeAdmin<ApprovedPrivilegedGrant>("confirm_privileged_action", {
    confirmationId,
    confirmationText,
  });
}

export async function listAdminAuditEvents(limit = 50): Promise<AdminAuditEvent[]> {
  return invokeRead<AdminAuditEvent[]>("list_admin_audit_events", { limit });
}

export class PrivilegedActionCancelledError extends Error {
  constructor() {
    super("高权限操作已取消");
    this.name = "PrivilegedActionCancelledError";
  }
}

export function isPrivilegedActionCancelled(error: unknown): boolean {
  return error instanceof PrivilegedActionCancelledError;
}

async function requestPrivilegedGrant(
  action: string,
  binding: PrivilegedActionBinding,
): Promise<string> {
  const prepared = await preparePrivilegedAction(action, binding);
  if (typeof window === "undefined") {
    throw new Error("高权限确认只能在交互式 Web 管理页面执行");
  }
  const entered = window.prompt(
    [
      "此操作会修改本机高权限配置，需要再次确认。",
      `目标：${prepared.targetSummary}`,
      `请输入以下确认文本（${prepared.expiresInSeconds} 秒内有效）：`,
      prepared.confirmationText,
    ].join("\n\n"),
    "",
  );
  if (entered === null) throw new PrivilegedActionCancelledError();
  const grant = await confirmPrivilegedAction(
    prepared.confirmationId,
    entered.trim(),
  );
  return grant.grantId;
}

export async function invokePrivilegedAdmin<T>(
  action: string,
  args: Record<string, unknown>,
  binding: PrivilegedActionBinding,
): Promise<T> {
  if (isTauriRuntime()) {
    return invokeAdmin<T>(action, args);
  }
  const grantId = await requestPrivilegedGrant(action, binding);
  return invokeAdmin<T>(action, { ...args, grantId });
}
