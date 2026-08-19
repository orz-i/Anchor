import { invokeAdmin, invokeRead } from "$lib/api/invoke";

export interface PreparedPrivilegedAction {
  confirmationId: string;
  action: string;
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
): Promise<PreparedPrivilegedAction> {
  return invokeAdmin<PreparedPrivilegedAction>("prepare_privileged_action", { action });
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
