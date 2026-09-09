import { invokeAdmin, invokeRead } from "@/lib/api/invoke";

export interface ILinkWorkerStatus {
  running: boolean;
  pid?: number | null;
  state: string;
  loggedIn: boolean;
  bound: boolean;
  reauthorizationRequired: boolean;
  lastError: string;
}

export interface NotificationChannelStatus {
  id: "ilink";
  label: string;
  description: string;
  worker: ILinkWorkerStatus;
}

export interface ILinkLoginChallenge {
  qrId: string;
  qrUrl: string;
  qrImageDataUrl: string;
  baseUrl: string;
}

export type ILinkLoginState =
  | "wait"
  | "scanned"
  | "need_verify_code"
  | "redirect"
  | "expired"
  | "verify_code_blocked"
  | "confirmed"
  | "already_bound";

export interface ILinkLoginPollResult {
  state: ILinkLoginState;
  baseUrl: string;
  worker?: ILinkWorkerStatus;
}

export async function listNotificationChannels(): Promise<NotificationChannelStatus[]> {
  return invokeRead<NotificationChannelStatus[]>("get_notification_channels");
}

export async function beginNotificationILinkLogin(): Promise<ILinkLoginChallenge> {
  return invokeAdmin<ILinkLoginChallenge>("begin_notification_ilink_login");
}

export async function pollNotificationILinkLogin(input: {
  qrId: string;
  baseUrl: string;
  verifyCode?: string;
}): Promise<ILinkLoginPollResult> {
  return invokeAdmin<ILinkLoginPollResult>("poll_notification_ilink_login", input);
}

export async function startNotificationILink(): Promise<NotificationChannelStatus> {
  return invokeAdmin<NotificationChannelStatus>("start_notification_ilink");
}

export async function stopNotificationILink(): Promise<NotificationChannelStatus> {
  return invokeAdmin<NotificationChannelStatus>("stop_notification_ilink");
}
