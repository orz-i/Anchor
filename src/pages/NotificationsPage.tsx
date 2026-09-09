import { useEffect, useState } from "react";
import { Bell, Play, QrCode as QrCodeIcon, RefreshCw, Square } from "lucide-react";
import { toast } from "sonner";

import { PageLayout } from "@/components/admin/PageLayout";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Field, FieldDescription, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import {
  beginNotificationILinkLogin,
  listNotificationChannels,
  pollNotificationILinkLogin,
  startNotificationILink,
  stopNotificationILink,
  type ILinkLoginChallenge,
  type ILinkLoginState,
  type NotificationChannelStatus,
} from "@/lib/api/notifications";

const POLL_DELAY_MS = 1_500;

function workerStateLabel(state: string): string {
  switch (state) {
    case "running":
      return "运行中";
    case "retrying":
      return "重试中";
    case "starting":
      return "启动中";
    case "not_logged_in":
      return "未登录";
    case "reauthorization_required":
      return "需要重新登录";
    case "stopped_with_error":
      return "异常停止";
    default:
      return "已停止";
  }
}

function loginStateLabel(state: ILinkLoginState): string {
  switch (state) {
    case "scanned":
      return "已扫码，等待微信确认…";
    case "need_verify_code":
      return "请输入手机微信显示的配对数字。";
    case "redirect":
      return "正在切换 iLink 登录节点…";
    case "expired":
      return "二维码已过期，请重新生成。";
    case "verify_code_blocked":
      return "配对验证已被阻止，请重新生成二维码。";
    case "confirmed":
      return "登录成功。";
    case "already_bound":
      return "已复用现有登录身份。";
    default:
      return "请使用微信扫描二维码。";
  }
}

export function NotificationsPage() {
  const [channels, setChannels] = useState<NotificationChannelStatus[]>([]);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [challenge, setChallenge] = useState<ILinkLoginChallenge | null>(null);
  const [loginState, setLoginState] = useState<ILinkLoginState>("wait");
  const [verifyCode, setVerifyCode] = useState("");

  const ilink = channels.find((channel) => channel.id === "ilink");

  const refresh = async () => {
    setChannels(await listNotificationChannels());
  };

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const next = await listNotificationChannels();
        if (!cancelled) setChannels(next);
      } catch (error) {
        if (!cancelled) toast.error("加载通知渠道失败", { description: String(error) });
      } finally {
        if (!cancelled) setLoading(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  useEffect(() => {
    if (
      !challenge ||
      loginState === "need_verify_code" ||
      loginState === "expired" ||
      loginState === "verify_code_blocked"
    ) {
      return;
    }
    let cancelled = false;
    let timer: number | undefined;
    const qrId = challenge.qrId;
    let baseUrl = challenge.baseUrl;
    const poll = async () => {
      try {
        const result = await pollNotificationILinkLogin({ qrId, baseUrl });
        if (cancelled) return;
        setLoginState(result.state);
        if (result.baseUrl !== baseUrl) {
          baseUrl = result.baseUrl;
          setChallenge((current) =>
            current ? { ...current, baseUrl: result.baseUrl } : current,
          );
        }
        if (result.state === "confirmed" || result.state === "already_bound") {
          setChallenge(null);
          setVerifyCode("");
          await refresh();
          if (!cancelled) {
            toast.success(result.state === "confirmed" ? "iLink 登录成功" : "iLink 已恢复登录");
          }
          return;
        }
        if (result.state === "need_verify_code" || result.state === "expired" || result.state === "verify_code_blocked") {
          return;
        }
        timer = window.setTimeout(() => void poll(), POLL_DELAY_MS);
      } catch (error) {
        if (!cancelled) toast.error("iLink 登录状态检查失败", { description: String(error) });
      }
    };
    timer = window.setTimeout(() => void poll(), POLL_DELAY_MS);
    return () => {
      cancelled = true;
      if (timer !== undefined) window.clearTimeout(timer);
    };
  }, [challenge, loginState]);

  const beginLogin = async () => {
    setBusy(true);
    try {
      const next = await beginNotificationILinkLogin();
      setChallenge(next);
      setLoginState("wait");
      setVerifyCode("");
      await refresh();
    } catch (error) {
      toast.error("生成 iLink 登录二维码失败", { description: String(error) });
    } finally {
      setBusy(false);
    }
  };

  const submitVerifyCode = async () => {
    if (!challenge) return;
    const code = verifyCode.trim();
    if (!code || !/^\d{1,16}$/.test(code)) {
      toast.warning("请输入有效的配对数字");
      return;
    }
    setBusy(true);
    try {
      const result = await pollNotificationILinkLogin({
        qrId: challenge.qrId,
        baseUrl: challenge.baseUrl,
        verifyCode: code,
      });
      setLoginState(result.state);
      if (result.baseUrl !== challenge.baseUrl) {
        setChallenge({ ...challenge, baseUrl: result.baseUrl });
      }
      if (result.state === "confirmed" || result.state === "already_bound") {
        setChallenge(null);
        setVerifyCode("");
        await refresh();
        toast.success("iLink 登录成功");
      }
    } catch (error) {
      toast.error("iLink 配对验证失败", { description: String(error) });
    } finally {
      setBusy(false);
    }
  };

  const startWorker = async () => {
    setBusy(true);
    try {
      await startNotificationILink();
      await refresh();
      toast.success("iLink 通知 worker 已启动");
    } catch (error) {
      toast.error("启动 iLink 失败", { description: String(error) });
    } finally {
      setBusy(false);
    }
  };

  const stopWorker = async () => {
    setBusy(true);
    try {
      await stopNotificationILink();
      await refresh();
      toast.success("iLink 通知 worker 已停止");
    } catch (error) {
      toast.error("停止 iLink 失败", { description: String(error) });
    } finally {
      setBusy(false);
    }
  };

  return (
    <PageLayout
      kicker="Notification"
      title="通知"
      description="统一管理应用级通知渠道。渠道只配置一次，不再绑定单个 Workspace；Workspace 仅作为通知事件来源。"
      actions={
        <Button type="button" variant="outline" size="sm" disabled={loading || busy} onClick={() => void refresh()}>
          <RefreshCw data-icon="inline-start" />
          刷新
        </Button>
      }
    >
      <div className="grid gap-5 xl:grid-cols-[minmax(22rem,1fr)_minmax(22rem,0.9fr)]">
        <Card>
          <CardHeader>
            <div className="flex items-start justify-between gap-3">
              <div>
                <CardTitle className="flex items-center gap-2">
                  <Bell className="size-4" />
                  iLink
                </CardTitle>
                <CardDescription className="mt-1">微信 iLink 渠道，用于接收 Harness 任务完成通知。</CardDescription>
              </div>
              {ilink && (
                <Badge variant={ilink.worker.running ? "default" : "secondary"}>
                  {workerStateLabel(ilink.worker.state)}
                </Badge>
              )}
            </div>
          </CardHeader>
          <CardContent>
            {loading || !ilink ? (
              <p className="text-sm text-muted-foreground">加载中…</p>
            ) : (
              <div className="flex flex-col gap-5">
                <div className="grid gap-3 sm:grid-cols-3">
                  <div className="rounded-lg border p-3">
                    <p className="text-xs text-muted-foreground">登录</p>
                    <p className="mt-1 text-sm font-medium">{ilink.worker.loggedIn ? "已登录" : "未登录"}</p>
                  </div>
                  <div className="rounded-lg border p-3">
                    <p className="text-xs text-muted-foreground">微信会话</p>
                    <p className="mt-1 text-sm font-medium">{ilink.worker.bound ? "已绑定" : "未绑定"}</p>
                  </div>
                  <div className="rounded-lg border p-3">
                    <p className="text-xs text-muted-foreground">Worker</p>
                    <p className="mt-1 text-sm font-medium">{ilink.worker.running ? `PID ${ilink.worker.pid ?? "-"}` : "已停止"}</p>
                  </div>
                </div>

                {ilink.worker.loggedIn && !ilink.worker.bound && (
                  <div className="rounded-lg border bg-muted/40 p-3 text-sm leading-6">
                    登录完成后，在微信中向 ClawBot 发送 <code className="rounded bg-muted px-1.5 py-0.5 font-mono text-xs">/bind</code>，即可将该会话设为全局通知目标。
                  </div>
                )}

                {ilink.worker.lastError && (
                  <div className="rounded-lg border border-destructive/40 bg-destructive/5 p-3 text-sm text-destructive">
                    {ilink.worker.lastError}
                  </div>
                )}

                <div className="flex flex-wrap gap-2">
                  <Button type="button" disabled={busy} onClick={() => void beginLogin()}>
                    <QrCodeIcon data-icon="inline-start" />
                    {ilink.worker.loggedIn ? "重新扫码登录" : "扫码登录"}
                  </Button>
                  <Button type="button" variant="outline" disabled={busy || !ilink.worker.loggedIn || ilink.worker.running} onClick={() => void startWorker()}>
                    <Play data-icon="inline-start" />
                    启动 Worker
                  </Button>
                  <Button type="button" variant="outline" disabled={busy || !ilink.worker.running} onClick={() => void stopWorker()}>
                    <Square data-icon="inline-start" />
                    停止 Worker
                  </Button>
                </div>
              </div>
            )}
          </CardContent>
        </Card>

        <Card>
          <CardHeader>
            <CardTitle>渠道登录</CardTitle>
            <CardDescription>登录凭据由 Anchor 在本机受保护的应用级 secret scope 中保存，不写入 Workspace。</CardDescription>
          </CardHeader>
          <CardContent>
            {challenge ? (
              <div className="flex flex-col items-center gap-4">
                <div className="rounded-xl border bg-white p-3 shadow-xs">
                  <img className="size-64 max-w-full" src={challenge.qrImageDataUrl} alt="iLink 微信登录二维码" />
                </div>
                <p className="text-center text-sm text-muted-foreground">{loginStateLabel(loginState)}</p>

                {loginState === "need_verify_code" && (
                  <Field className="w-full max-w-sm">
                    <FieldLabel htmlFor="ilink-verify-code">配对数字</FieldLabel>
                    <div className="flex gap-2">
                      <Input
                        id="ilink-verify-code"
                        inputMode="numeric"
                        autoComplete="one-time-code"
                        value={verifyCode}
                        onChange={(event) => setVerifyCode(event.target.value.replace(/\D/g, "").slice(0, 16))}
                        onKeyDown={(event) => {
                          if (event.key === "Enter") void submitVerifyCode();
                        }}
                      />
                      <Button type="button" disabled={busy} onClick={() => void submitVerifyCode()}>确认</Button>
                    </div>
                    <FieldDescription>数字仅用于本次二维码登录确认。</FieldDescription>
                  </Field>
                )}

                {(loginState === "expired" || loginState === "verify_code_blocked") && (
                  <Button type="button" disabled={busy} onClick={() => void beginLogin()}>
                    <RefreshCw data-icon="inline-start" />
                    重新生成二维码
                  </Button>
                )}
              </div>
            ) : (
              <div className="flex min-h-64 flex-col items-center justify-center gap-3 rounded-xl border border-dashed p-6 text-center">
                <QrCodeIcon className="size-8 text-muted-foreground" />
                <div>
                  <p className="text-sm font-medium">没有进行中的登录</p>
                  <p className="mt-1 text-sm text-muted-foreground">点击左侧“扫码登录”生成一次性二维码。</p>
                </div>
              </div>
            )}
          </CardContent>
        </Card>
      </div>
    </PageLayout>
  );
}
