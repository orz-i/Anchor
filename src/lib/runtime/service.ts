import { toast } from "sonner";
import type { RuntimeStatus } from "@/lib/types";

function serviceErrorMessage(status: RuntimeStatus): string {
  return status.localMessage || status.publicMessage || "服务未能启动";
}

export async function runServiceToggle(
  running: boolean,
  start: () => Promise<RuntimeStatus>,
  stop: () => Promise<RuntimeStatus>,
  serviceLabel = "服务",
): Promise<RuntimeStatus | null> {
  try {
    return running ? await stop() : await start();
  } catch (error) {
    const text = error instanceof Error ? error.message : String(error);
    toast.error(running ? `${serviceLabel}停止失败` : `${serviceLabel}启动失败`, {
      description: text,
      duration: 8000,
    });
    return null;
  }
}

export function notifyStartFailure(
  serviceLabel: string,
  status: RuntimeStatus,
): void {
  toast.error(`${serviceLabel}启动失败`, {
    description: serviceErrorMessage(status),
    duration: 8000,
  });
}
