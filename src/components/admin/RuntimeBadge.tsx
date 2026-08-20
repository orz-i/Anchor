import { Badge } from "@/components/ui/badge";
import type { RuntimeState } from "@/lib/types";

const LABELS: Record<RuntimeState, string> = {
  stopped: "已停止",
  starting: "启动中",
  running: "运行中",
  recovering: "恢复中",
  stopping: "停止中",
  error: "错误",
};

export function RuntimeDot({ state }: { state: RuntimeState }) {
  const className =
    state === "running"
      ? "bg-emerald-500"
      : state === "error"
        ? "bg-destructive"
        : state === "starting" || state === "recovering" || state === "stopping"
          ? "bg-amber-500 animate-pulse"
          : "bg-muted-foreground/50";
  return <span aria-hidden="true" className={`size-2 rounded-full ${className}`} />;
}

export function RuntimeBadge({ state }: { state: RuntimeState }) {
  return (
    <Badge variant="outline" className="gap-1.5 font-normal">
      <RuntimeDot state={state} />
      {LABELS[state]}
    </Badge>
  );
}
