const ADMIN_API_PREFIX = "/api/v1/commands";

interface AdminApiSuccess<T> {
  ok: true;
  data: T;
}

interface AdminApiFailure {
  ok: false;
  error: {
    code?: string;
    message: string;
  };
}

type AdminApiResponse<T> = AdminApiSuccess<T> | AdminApiFailure;

interface AdminSessionBootstrap {
  csrfToken: string;
  idleTimeoutSeconds: number;
  supportedCommands?: string[];
  unavailableCommands?: string[];
}

let webAdminSession: AdminSessionBootstrap | null = null;
let webAdminSessionPromise: Promise<AdminSessionBootstrap> | null = null;

async function createWebAdminSession(): Promise<AdminSessionBootstrap> {
  const response = await fetch("/api/v1/session", {
    method: "POST",
    credentials: "same-origin",
    headers: {
      "x-anchor-admin-request": "1",
    },
  });
  const payload = (await response.json().catch(() => null)) as
    | AdminApiResponse<AdminSessionBootstrap>
    | null;
  if (!response.ok || !payload || payload.ok === false) {
    const detail = payload && payload.ok === false ? payload.error.message : response.statusText;
    throw new Error(detail || `创建管理会话失败：HTTP ${response.status}`);
  }
  return payload.data;
}

async function ensureWebAdminSession(): Promise<AdminSessionBootstrap> {
  if (webAdminSession) return webAdminSession;
  if (!webAdminSessionPromise) {
    webAdminSessionPromise = createWebAdminSession()
      .then((session) => {
        webAdminSession = session;
        return session;
      })
      .finally(() => {
        webAdminSessionPromise = null;
      });
  }
  return webAdminSessionPromise;
}

async function invokeWebOnce<T>(
  command: string,
  args: Record<string, unknown> | undefined,
  session: AdminSessionBootstrap,
): Promise<{ response: Response; payload: AdminApiResponse<T> | null }> {
  const response = await fetch(`${ADMIN_API_PREFIX}/${encodeURIComponent(command)}`, {
    method: "POST",
    credentials: "same-origin",
    headers: {
      "content-type": "application/json",
      "x-anchor-admin-request": "1",
      "x-anchor-admin-csrf": session.csrfToken,
    },
    body: JSON.stringify({ args: args ?? {} }),
  });
  const payload = (await response.json().catch(() => null)) as AdminApiResponse<T> | null;
  return { response, payload };
}

async function invokeWeb<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  let session = await ensureWebAdminSession();
  if (session.supportedCommands?.includes(command) !== true) {
    const privileged = session.unavailableCommands?.includes(command) ?? false;
    throw new Error(
      privileged
        ? `当前 Web 管理面尚未开放高权限操作：${command}`
        : `当前 Web 管理面不支持操作：${command}`,
    );
  }
  let { response, payload } = await invokeWebOnce<T>(command, args, session);
  if (response.status === 401) {
    webAdminSession = null;
    session = await ensureWebAdminSession();
    ({ response, payload } = await invokeWebOnce<T>(command, args, session));
  }
  if (!response.ok || !payload || payload.ok === false) {
    const detail = payload && payload.ok === false ? payload.error.message : response.statusText;
    throw new Error(detail || `管理 API 请求失败：HTTP ${response.status}`);
  }
  return payload.data;
}

export async function invokeAdmin<T>(
  command: string,
  args?: Record<string, unknown>,
): Promise<T> {
  return invokeWeb<T>(command, args);
}

export async function supportsAdminCommand(command: string): Promise<boolean> {
  const session = await ensureWebAdminSession();
  return session.supportedCommands?.includes(command) === true;
}

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
      return await invokeAdmin<T>(command, args);
    } catch (error) {
      lastError = error;
      if (attempt >= attempts || !isTransientInvokeError(error)) throw error;
      const delay = Math.min(maxDelayMs, baseDelayMs * 2 ** (attempt - 1));
      await new Promise((resolve) => globalThis.setTimeout(resolve, delay));
    }
  }

  throw lastError;
}
