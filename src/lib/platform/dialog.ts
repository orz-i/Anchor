import { isTauriRuntime } from "$lib/platform/runtime";

export type DialogKind = "info" | "warning" | "error";

export interface MessageOptions {
  title?: string;
  kind?: DialogKind;
}

export interface ConfirmOptions extends MessageOptions {
  okLabel?: string;
  cancelLabel?: string;
}

export interface OpenOptions {
  directory?: boolean;
  multiple?: boolean;
  defaultPath?: string;
}

function browserText(message: string, title?: string): string {
  return title?.trim() ? `${title}\n\n${message}` : message;
}

export async function message(text: string, options: MessageOptions = {}): Promise<void> {
  if (isTauriRuntime()) {
    const dialog = await import("@tauri-apps/plugin-dialog");
    await dialog.message(text, options);
    return;
  }
  if (typeof window === "undefined") return;
  window.alert(browserText(text, options.title));
}

export async function confirm(text: string, options: ConfirmOptions = {}): Promise<boolean> {
  if (isTauriRuntime()) {
    const dialog = await import("@tauri-apps/plugin-dialog");
    return dialog.confirm(text, options);
  }
  if (typeof window === "undefined") return false;
  return window.confirm(browserText(text, options.title));
}

export async function open(options: OpenOptions = {}): Promise<string | string[] | null> {
  if (isTauriRuntime()) {
    const dialog = await import("@tauri-apps/plugin-dialog");
    return dialog.open(options);
  }
  if (typeof window === "undefined") return null;
  const subject = options.directory ? "服务器目录" : "服务器路径";
  const selected = window.prompt(`请输入${subject}：`, options.defaultPath ?? "");
  const value = selected?.trim();
  return value ? value : null;
}
