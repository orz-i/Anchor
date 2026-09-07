export interface OpenOptions {
  directory?: boolean;
  multiple?: boolean;
  defaultPath?: string;
}

export async function open(options: OpenOptions = {}): Promise<string | string[] | null> {
  if (typeof window === "undefined") return null;
  const subject = options.directory ? "服务器目录" : "服务器路径";
  const selected = window.prompt(`请输入${subject}：`, options.defaultPath ?? "");
  const value = selected?.trim();
  return value ? value : null;
}
