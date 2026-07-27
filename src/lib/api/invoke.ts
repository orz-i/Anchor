import { invoke } from "@tauri-apps/api/core";

interface ReadRetryOptions {
  attempts?: number;
  baseDelayMs?: number;
  maxDelayMs?: number;
}

const TERMINAL_ERROR_MARKERS = [
  "not found",
  "unknown",
  "invalid",
  "不能为空",
  "不存在",
  "不允许",
  "拒绝",
  "已被占用",
  "permission",
];

const TRANSIENT_ERROR_MARKERS = [
  "failed to invoke",
  "ipc",
  "channel",
  "transport",
  "connection",
  "disconnected",
  "temporarily unavailable",
  "timeout",
  "timed out",
  "poisoned",
  "webview",
  "后台连接",
];

function errorText(error: unknown): string {
  if (error instanceof Error) return error.message;
  if (typeof error === "string") return error;
  if (error && typeof error === "object" && "message" in error) {
    return String((error as { message: unknown }).message);
  }
  return String(error);
}

function isTransientInvokeError(error: unknown): boolean {
  const text = errorText(error).toLowerCase();
  if (TERMINAL_ERROR_MARKERS.some((marker) => text.includes(marker))) return false;
  return TRANSIENT_ERROR_MARKERS.some((marker) => text.includes(marker));
}

export async function invokeRead<T>(
  command: string,
  args?: Record<string, unknown>,
  options: ReadRetryOptions = {},
): Promise<T> {
  const attempts = Math.max(1, options.attempts ?? 3);
  const baseDelayMs = Math.max(0, options.baseDelayMs ?? 200);
  const maxDelayMs = Math.max(baseDelayMs, options.maxDelayMs ?? 1_200);
  let lastError: unknown;

  for (let attempt = 1; attempt <= attempts; attempt += 1) {
    try {
      return await invoke<T>(command, args);
    } catch (error) {
      lastError = error;
      if (attempt >= attempts || !isTransientInvokeError(error)) throw error;
      const delay = Math.min(maxDelayMs, baseDelayMs * 2 ** (attempt - 1));
      await new Promise((resolve) => globalThis.setTimeout(resolve, delay));
    }
  }

  throw lastError;
}
