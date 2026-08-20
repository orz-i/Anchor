import { useEffect, useState } from "react";
import { toast } from "sonner";

import { CopyField } from "@/components/admin/CopyField";
import { RuntimeBadge } from "@/components/admin/RuntimeBadge";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import type { McpActivity, RuntimeRecovery, RuntimeState } from "@/lib/types";

const EMPTY_RECOVERY: RuntimeRecovery = { enabled: false, attempt: 0, maxAttempts: 5, retryInMs: null, recoveredCount: 0, lastError: "" };

function duration(ms: number | null): string {
  if (ms === null) return "-";
  if (ms < 1000) return `${ms}ms`;
  const seconds = Math.floor(ms / 1000);
  return seconds < 60 ? `${seconds}s` : `${Math.floor(seconds / 60)}m ${seconds % 60}s`;
}

export function ServicePanel({ title, subtitle, status, statusMessage = "", recovery = EMPTY_RECOVERY, activity = null, port, portEditable = false, busy = false, tunnelType = "none", localEndpoint, publicEndpoint = "", publicLabel = "公网", onToggle, onPortChange }: { title: string; subtitle: string; status: RuntimeState; statusMessage?: string; recovery?: RuntimeRecovery; activity?: McpActivity | null; port: number; portEditable?: boolean; busy?: boolean; tunnelType?: string; localEndpoint: string; publicEndpoint?: string; publicLabel?: string; onToggle: () => void | Promise<void>; onPortChange?: (port: number) => void | Promise<void> }) {
  const [draftPort, setDraftPort] = useState(port);
  useEffect(() => setDraftPort(port), [port]);
  const running = status === "running";
  const recovering = status === "recovering";
  const canEditPort = portEditable && !running && !recovering && status !== "starting" && status !== "stopping";
  const tunnelLabel = tunnelType === "cloudflare" ? "Cloudflare" : tunnelType === "frp" ? "FRP" : "";

  const commitPort = async () => {
    if (!onPortChange || draftPort === port) return;
    if (draftPort < 1024 || draftPort > 65535) { setDraftPort(port); toast.warning("端口必须在 1024–65535 之间"); return; }
    await onPortChange(draftPort);
  };

  return (
    <Card>
      <CardHeader className="flex-row items-start justify-between gap-4">
        <div className="min-w-0">
          <div className="flex items-center gap-2"><CardTitle>{title}</CardTitle><RuntimeBadge state={status} /></div>
          <CardDescription className="mt-1">{subtitle}</CardDescription>
          {tunnelLabel && <p className="mt-1 text-xs text-muted-foreground">{tunnelLabel} 隧道独立保活；配置重载不会更换公网链接，手动停止服务时才断开。</p>}
        </div>
        <Button type="button" variant={running ? "destructive" : "default"} disabled={busy || status === "starting" || status === "stopping"} onClick={() => void onToggle()}>{busy ? "处理中…" : running ? "停止" : recovering ? "立即重试" : "启动"}</Button>
      </CardHeader>
      <CardContent className="flex flex-col gap-4">
        {status === "error" && statusMessage && <Alert variant="destructive"><AlertTitle>服务错误</AlertTitle><AlertDescription>{statusMessage}</AlertDescription></Alert>}
        {recovering && <Alert><AlertTitle>正在自动恢复</AlertTitle><AlertDescription>{statusMessage || "连接中断，后台正在重试"}{recovery.retryInMs !== null ? ` · ${Math.max(1, Math.ceil(recovery.retryInMs / 1000))}s 后重试` : ` · 第 ${Math.min(recovery.attempt + 1, recovery.maxAttempts)}/${recovery.maxAttempts} 次`}</AlertDescription></Alert>}

        <div className="grid gap-3 md:grid-cols-2">
          <div className="rounded-xl border bg-muted/30 p-3">
            <p className="text-xs font-medium text-muted-foreground">端口</p>
            {canEditPort ? <Input type="number" min={1024} max={65535} className="mt-2 w-36 font-mono" value={draftPort} onChange={(event) => setDraftPort(Number(event.target.value))} onBlur={() => void commitPort()} /> : <p className="mt-2 font-mono text-sm">{port}</p>}
          </div>
          {activity && <div className="rounded-xl border bg-muted/30 p-3"><p className="text-xs font-medium text-muted-foreground">MCP 工具活动</p><p className="mt-2 text-sm font-medium">{activity.message}</p><p className="mt-1 text-xs text-muted-foreground">在途 {activity.inFlightRequests} · 最久 {duration(activity.oldestInFlightMs)} · 最近工具活动 {activity.lastActivityAgeMs === null ? "尚无记录" : `${duration(activity.lastActivityAgeMs)} 前`}</p>{activity.currentTool || activity.currentMethod ? <p className="mt-1 truncate font-mono text-xs text-muted-foreground">{activity.currentTool || activity.currentMethod}</p> : null}</div>}
        </div>
        <CopyField label="本地地址" value={localEndpoint} />
        <CopyField label={publicLabel} value={publicEndpoint} hint={publicEndpoint ? undefined : "未配置隧道"} />
      </CardContent>
    </Card>
  );
}
