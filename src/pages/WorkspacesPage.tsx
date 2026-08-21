import { useMemo, useState } from "react";
import {
  Boxes,
  Check,
  Copy,
  FolderPlus,
  Network,
  Play,
  Plus,
  RefreshCw,
  Search,
  Server,
  Square,
  Trash2,
  Zap,
} from "lucide-react";
import { Link, useNavigate } from "react-router-dom";
import { toast } from "sonner";

import { useAdmin } from "@/components/admin/AdminProvider";
import { PageLayout } from "@/components/admin/PageLayout";
import { RuntimeDot } from "@/components/admin/RuntimeBadge";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Card,
  CardContent,
  CardDescription,
  CardFooter,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import { Empty, EmptyDescription, EmptyHeader, EmptyTitle } from "@/components/ui/empty";
import { Input } from "@/components/ui/input";
import {
  Pagination,
  PaginationContent,
  PaginationEllipsis,
  PaginationItem,
  PaginationLink,
  PaginationNext,
  PaginationPrevious,
} from "@/components/ui/pagination";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { createWorkspace, deleteWorkspace, startActionsRuntime, startRuntime, stopActionsRuntime, stopRuntime } from "@/lib/api/workspaces";
import { open } from "@/lib/platform/dialog";
import { notifyStartFailure, runServiceToggle } from "@/lib/runtime/service";
import type { RuntimeState, WorkspaceProfile } from "@/lib/types";
import { actionsConfig } from "@/lib/types";

type StatusFilter = "all" | "mcp_running" | "actions_running" | "any_running" | "stopped";

export function WorkspacesPage() {
  const { workspaces, mcpRuntimeStates, actionsRuntimeStates, loading, refreshWorkspaces, setMcpRuntimeState, setActionsRuntimeState } = useAdmin();
  const navigate = useNavigate();

  const [searchQuery, setSearchQuery] = useState("");
  const [statusFilter, setStatusFilter] = useState<StatusFilter>("all");
  const [currentPage, setCurrentPage] = useState(1);
  const [pageSize, setPageSize] = useState(6);
  const [copiedId, setCopiedId] = useState<string | null>(null);
  const [busyMap, setBusyMap] = useState<Record<string, boolean>>({});
  const [refreshing, setRefreshing] = useState(false);

  // 快捷添加工作区
  const handleAddWorkspace = async () => {
    try {
      const selected = await open({ directory: true, multiple: false });
      if (!selected || Array.isArray(selected)) return;
      const profile = await createWorkspace(selected);
      await refreshWorkspaces();
      toast.success("工作区添加成功");
      navigate(`/workspaces/${profile.id}`);
    } catch (error) {
      toast.error("添加工作区失败", { description: String(error) });
    }
  };

  // 快捷刷新
  const handleRefresh = async () => {
    setRefreshing(true);
    try {
      await refreshWorkspaces();
      toast.success("工作区状态已刷新");
    } catch (error) {
      toast.error("刷新失败", { description: String(error) });
    } finally {
      setRefreshing(false);
    }
  };

  // 复制路径
  const handleCopyPath = (path: string, id: string) => {
    void navigator.clipboard.writeText(path);
    setCopiedId(id);
    toast.success("路径已复制到剪贴板");
    setTimeout(() => setCopiedId(null), 2000);
  };

  // 快捷服务启停
  const toggleService = async (workspace: WorkspaceProfile, service: "mcp" | "actions") => {
    const key = `${workspace.id}-${service}`;
    if (busyMap[key]) return;
    
    const currentState: RuntimeState =
      service === "mcp"
        ? (mcpRuntimeStates[workspace.id] ?? "stopped")
        : (actionsRuntimeStates[workspace.id] ?? "stopped");
    
    setBusyMap((prev) => ({ ...prev, [key]: true }));
    try {
      const isRunning = currentState === "running";
      const result = await runServiceToggle(
        isRunning,
        service === "mcp"
          ? () => startRuntime(workspace.id)
          : () => startActionsRuntime(workspace.id),
        service === "mcp"
          ? () => stopRuntime(workspace.id)
          : () => stopActionsRuntime(workspace.id),
        service === "mcp" ? "MCP" : "Actions",
      );

      if (result) {
        if (service === "mcp") {
          setMcpRuntimeState(workspace.id, result.state);
        } else {
          setActionsRuntimeState(workspace.id, result.state);
        }
        if (!isRunning && result.state === "error") {
          notifyStartFailure(service === "mcp" ? "MCP" : "Actions", result);
        }
      }
    } finally {
      setBusyMap((prev) => ({ ...prev, [key]: false }));
    }
  };

  // 删除工作区
  const handleDeleteWorkspace = async (workspace: WorkspaceProfile) => {
    if (!window.confirm(`确定删除工作区「${workspace.name}」？不会删除磁盘上的代码文件。`)) {
      return;
    }
    try {
      await deleteWorkspace(workspace.id);
      await refreshWorkspaces();
      toast.success("工作区已删除");
    } catch (error) {
      toast.error("删除失败", { description: String(error) });
    }
  };

  // 统计指标
  const stats = useMemo(() => {
    const total = workspaces.length;
    let mcpRunning = 0;
    let actionsRunning = 0;
    let tunnelsActive = 0;

    for (const ws of workspaces) {
      const mcpState = mcpRuntimeStates[ws.id] ?? "stopped";
      const actState = actionsRuntimeStates[ws.id] ?? "stopped";
      if (mcpState === "running") mcpRunning++;
      if (actState === "running") actionsRunning++;
      if (ws.tunnel.type !== "none" || actionsConfig(ws).tunnel_type !== "none") {
        tunnelsActive++;
      }
    }

    return { total, mcpRunning, actionsRunning, tunnelsActive };
  }, [workspaces, mcpRuntimeStates, actionsRuntimeStates]);

  // 过滤工作区
  const filteredWorkspaces = useMemo(() => {
    return workspaces.filter((ws) => {
      const query = searchQuery.trim().toLowerCase();
      const matchesSearch =
        !query ||
        ws.name.toLowerCase().includes(query) ||
        ws.path.toLowerCase().includes(query);

      if (!matchesSearch) return false;

      const mcpState = mcpRuntimeStates[ws.id] ?? "stopped";
      const actState = actionsRuntimeStates[ws.id] ?? "stopped";

      if (statusFilter === "mcp_running") return mcpState === "running";
      if (statusFilter === "actions_running") return actState === "running";
      if (statusFilter === "any_running") return mcpState === "running" || actState === "running";
      if (statusFilter === "stopped") return mcpState === "stopped" && actState === "stopped";

      return true;
    });
  }, [workspaces, searchQuery, statusFilter, mcpRuntimeStates, actionsRuntimeStates]);

  // 分页计算
  const totalItems = filteredWorkspaces.length;
  const totalPages = Math.max(1, Math.ceil(totalItems / pageSize));
  const currentPageClamped = Math.min(Math.max(1, currentPage), totalPages);

  const paginatedWorkspaces = useMemo(() => {
    const startIndex = (currentPageClamped - 1) * pageSize;
    return filteredWorkspaces.slice(startIndex, startIndex + pageSize);
  }, [filteredWorkspaces, currentPageClamped, pageSize]);

  const handleSearchChange = (value: string) => {
    setSearchQuery(value);
    setCurrentPage(1);
  };

  const handleFilterChange = (value: string | null) => {
    setStatusFilter((value ?? "all") as StatusFilter);
    setCurrentPage(1);
  };

  const handlePageSizeChange = (val: string | null) => {
    if (val) {
      setPageSize(Number(val));
      setCurrentPage(1);
    }
  };

  return (
    <PageLayout
      kicker="工作区管理"
      title="工作区概览"
      description="集中管理已配置的工作区实例，监控 MCP / Actions 服务运行状态与公网隧道连接。"
      actions={
        <div className="flex items-center gap-2">
          <Button
            type="button"
            variant="outline"
            size="sm"
            disabled={refreshing}
            onClick={() => void handleRefresh()}
          >
            <RefreshCw className={refreshing ? "animate-spin" : ""} data-icon="inline-start" />
            刷新
          </Button>
          <Button type="button" size="sm" onClick={() => void handleAddWorkspace()}>
            <FolderPlus data-icon="inline-start" />
            添加工作区
          </Button>
        </div>
      }
    >
      <div className="flex flex-col gap-6">
        {/* 统计指标卡片组 */}
        <div className="grid grid-cols-2 gap-4 sm:grid-cols-2 lg:grid-cols-4">
          <Card className="bg-card/50 backdrop-blur-xs">
            <CardHeader className="flex flex-row items-center justify-between pb-2">
              <CardTitle className="text-xs font-medium text-muted-foreground">总工作区数</CardTitle>
              <Boxes className="size-4 text-muted-foreground" />
            </CardHeader>
            <CardContent>
              <div className="text-2xl font-bold">{stats.total}</div>
              <p className="mt-1 text-xs text-muted-foreground">已注册的本地工程目录</p>
            </CardContent>
          </Card>

          <Card className="bg-card/50 backdrop-blur-xs">
            <CardHeader className="flex flex-row items-center justify-between pb-2">
              <CardTitle className="text-xs font-medium text-muted-foreground">MCP 运行中</CardTitle>
              <Network className="size-4 text-emerald-500" />
            </CardHeader>
            <CardContent>
              <div className="text-2xl font-bold text-emerald-600 dark:text-emerald-400">
                {stats.mcpRunning}
              </div>
              <p className="mt-1 text-xs text-muted-foreground">
                共 {stats.total} 个实例 ({stats.total > 0 ? Math.round((stats.mcpRunning / stats.total) * 100) : 0}%)
              </p>
            </CardContent>
          </Card>

          <Card className="bg-card/50 backdrop-blur-xs">
            <CardHeader className="flex flex-row items-center justify-between pb-2">
              <CardTitle className="text-xs font-medium text-muted-foreground">Actions 运行中</CardTitle>
              <Zap className="size-4 text-blue-500" />
            </CardHeader>
            <CardContent>
              <div className="text-2xl font-bold text-blue-600 dark:text-blue-400">
                {stats.actionsRunning}
              </div>
              <p className="mt-1 text-xs text-muted-foreground">GPT Actions 活跃服务</p>
            </CardContent>
          </Card>

          <Card className="bg-card/50 backdrop-blur-xs">
            <CardHeader className="flex flex-row items-center justify-between pb-2">
              <CardTitle className="text-xs font-medium text-muted-foreground">配置公网隧道</CardTitle>
              <Server className="size-4 text-purple-500" />
            </CardHeader>
            <CardContent>
              <div className="text-2xl font-bold">{stats.tunnelsActive}</div>
              <p className="mt-1 text-xs text-muted-foreground">Cloudflare / FRP 隧道接入</p>
            </CardContent>
          </Card>
        </div>

        {/* 筛选与搜索工具栏 */}
        <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
          <div className="flex flex-1 items-center gap-2">
            <div className="relative max-w-sm flex-1">
              <Search className="absolute left-2.5 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" />
              <Input
                placeholder="搜索工作区名称或路径…"
                value={searchQuery}
                onChange={(e) => handleSearchChange(e.target.value)}
                className="pl-8"
              />
            </div>
            <Select
              value={statusFilter}
              onValueChange={(val) => handleFilterChange((val ?? "all") as StatusFilter)}
            >
              <SelectTrigger className="w-[150px]">
                <SelectValue placeholder="状态筛选" />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="all">全部状态</SelectItem>
                <SelectItem value="mcp_running">MCP 运行中</SelectItem>
                <SelectItem value="actions_running">Actions 运行中</SelectItem>
                <SelectItem value="any_running">任一运行中</SelectItem>
                <SelectItem value="stopped">已全部停止</SelectItem>
              </SelectContent>
            </Select>
          </div>

          <div className="flex items-center gap-2 text-xs text-muted-foreground">
            <span>每页显示:</span>
            <Select value={String(pageSize)} onValueChange={handlePageSizeChange}>
              <SelectTrigger className="h-8 w-[70px]">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="6">6</SelectItem>
                <SelectItem value="9">9</SelectItem>
                <SelectItem value="12">12</SelectItem>
                <SelectItem value="24">24</SelectItem>
              </SelectContent>
            </Select>
          </div>
        </div>

        {/* 主内容区域：卡片列表 / 空状态 */}
        {loading && workspaces.length === 0 ? (
          <div className="grid grid-cols-1 gap-4 md:grid-cols-2 xl:grid-cols-3">
            {[1, 2, 3].map((i) => (
              <Card key={i} className="animate-pulse">
                <CardHeader>
                  <div className="h-5 w-1/3 rounded bg-muted"></div>
                  <div className="mt-2 h-4 w-2/3 rounded bg-muted/60"></div>
                </CardHeader>
                <CardContent>
                  <div className="h-16 rounded bg-muted/40"></div>
                </CardContent>
              </Card>
            ))}
          </div>
        ) : workspaces.length === 0 ? (
          <Empty className="rounded-xl border bg-card/40 p-12">
            <EmptyHeader>
              <EmptyTitle>暂无工作区</EmptyTitle>
              <EmptyDescription>
                通过添加本地代码项目目录，即可为其快速配置专属的 MCP 与 Actions 网关服务。
              </EmptyDescription>
            </EmptyHeader>
            <Button className="mt-4" onClick={() => void handleAddWorkspace()}>
              <Plus data-icon="inline-start" />
              立即添加第一个工作区
            </Button>
          </Empty>
        ) : filteredWorkspaces.length === 0 ? (
          <Empty className="rounded-xl border bg-card/40 p-12">
            <EmptyHeader>
              <EmptyTitle>未找到匹配的工作区</EmptyTitle>
              <EmptyDescription>请尝试调整搜索关键词或状态过滤条件。</EmptyDescription>
            </EmptyHeader>
            <Button
              variant="outline"
              className="mt-4"
              onClick={() => {
                setSearchQuery("");
                setStatusFilter("all");
                setCurrentPage(1);
              }}
            >
              清空搜索与筛选
            </Button>
          </Empty>
        ) : (
          <>
            {/* 卡片网格 */}
            <div className="grid grid-cols-1 gap-4 md:grid-cols-2 xl:grid-cols-3">
              {paginatedWorkspaces.map((workspace) => {
                const mcpState = mcpRuntimeStates[workspace.id] ?? "stopped";
                const actState = actionsRuntimeStates[workspace.id] ?? "stopped";
                const isMcpBusy = busyMap[`${workspace.id}-mcp`] ?? false;
                const isActBusy = busyMap[`${workspace.id}-actions`] ?? false;
                const actConfig = actionsConfig(workspace);

                return (
                  <Card
                    key={workspace.id}
                    className="group relative flex flex-col justify-between border-border/80 transition-all hover:border-border hover:shadow-sm"
                  >
                    <div>
                      <CardHeader className="pb-3">
                        <div className="flex items-start justify-between gap-2">
                          <div className="min-w-0 flex-1">
                            <Link
                              to={`/workspaces/${workspace.id}`}
                              className="font-semibold text-foreground transition-colors hover:text-primary"
                            >
                              <CardTitle className="truncate text-base">{workspace.name}</CardTitle>
                            </Link>
                            <CardDescription
                              className="mt-1 flex cursor-pointer items-center gap-1.5 font-mono text-xs text-muted-foreground hover:text-foreground"
                              title="点击复制路径"
                              onClick={() => handleCopyPath(workspace.path, workspace.id)}
                            >
                              <span className="truncate">{workspace.path}</span>
                              {copiedId === workspace.id ? (
                                <Check className="size-3 shrink-0 text-emerald-500" />
                              ) : (
                                <Copy className="size-3 shrink-0 opacity-60 group-hover:opacity-100" />
                              )}
                            </CardDescription>
                          </div>
                        </div>

                        {/* 状态徽章条 */}
                        <div className="mt-3 flex flex-wrap items-center gap-2">
                          <div className="flex items-center gap-1.5 rounded-md border bg-muted/30 px-2 py-1 text-xs">
                            <RuntimeDot state={mcpState} />
                            <span className="font-medium">MCP:</span>
                            <span className="capitalize text-muted-foreground">{mcpState}</span>
                          </div>
                          <div className="flex items-center gap-1.5 rounded-md border bg-muted/30 px-2 py-1 text-xs">
                            <RuntimeDot state={actState} />
                            <span className="font-medium">Actions:</span>
                            <span className="capitalize text-muted-foreground">{actState}</span>
                          </div>
                        </div>
                      </CardHeader>

                      <CardContent className="flex flex-col gap-2.5 pb-4 text-xs">
                        {/* 配置摘要 */}
                        <div className="rounded-lg border bg-muted/20 p-2.5">
                          <div className="flex items-center justify-between text-muted-foreground">
                            <span>本地端口</span>
                            <span className="font-mono text-foreground">
                              MCP:{workspace.runtime.local_port} / Act:{actConfig.local_port}
                            </span>
                          </div>
                          <div className="mt-1.5 flex items-center justify-between text-muted-foreground">
                            <span>公网隧道</span>
                            <Badge variant="outline" className="text-[10px]">
                              {workspace.tunnel.type !== "none"
                                ? workspace.tunnel.type.toUpperCase()
                                : "未启用"}
                            </Badge>
                          </div>
                        </div>

                        {/* 快捷服务启停按钮 */}
                        <div className="grid grid-cols-2 gap-2 pt-1">
                          <Button
                            type="button"
                            variant={mcpState === "running" ? "outline" : "secondary"}
                            size="sm"
                            className="w-full text-xs"
                            disabled={isMcpBusy}
                            onClick={() => void toggleService(workspace, "mcp")}
                          >
                            {mcpState === "running" ? (
                              <>
                                <Square data-icon="inline-start" className="size-3 text-destructive" />
                                停止 MCP
                              </>
                            ) : (
                              <>
                                <Play data-icon="inline-start" className="size-3 text-emerald-500" />
                                启动 MCP
                              </>
                            )}
                          </Button>

                          <Button
                            type="button"
                            variant={actState === "running" ? "outline" : "secondary"}
                            size="sm"
                            className="w-full text-xs"
                            disabled={isActBusy}
                            onClick={() => void toggleService(workspace, "actions")}
                          >
                            {actState === "running" ? (
                              <>
                                <Square data-icon="inline-start" className="size-3 text-destructive" />
                                停止 Actions
                              </>
                            ) : (
                              <>
                                <Play data-icon="inline-start" className="size-3 text-emerald-500" />
                                启动 Actions
                              </>
                            )}
                          </Button>
                        </div>
                      </CardContent>
                    </div>

                    <CardFooter className="flex items-center justify-between border-t bg-muted/10 p-3">
                      <Button
                        type="button"
                        variant="ghost"
                        size="xs"
                        className="text-muted-foreground hover:text-destructive"
                        onClick={() => void handleDeleteWorkspace(workspace)}
                      >
                        <Trash2 className="size-3.5" data-icon="inline-start" />
                        删除
                      </Button>

                      <Button
                        type="button"
                        variant="default"
                        size="sm"
                        onClick={() => navigate(`/workspaces/${workspace.id}`)}
                      >
                        进入详情
                      </Button>
                    </CardFooter>
                  </Card>
                );
              })}
            </div>

            {/* 分页控制栏 */}
            <div className="flex flex-col items-center justify-between gap-4 border-t pt-4 sm:flex-row">
              <div className="text-xs text-muted-foreground">
                显示第{" "}
                <span className="font-medium text-foreground">
                  {(currentPageClamped - 1) * pageSize + 1}
                </span>{" "}
                到{" "}
                <span className="font-medium text-foreground">
                  {Math.min(currentPageClamped * pageSize, totalItems)}
                </span>{" "}
                项，共 <span className="font-medium text-foreground">{totalItems}</span> 个工作区
              </div>

              {totalPages > 1 && (
                <Pagination className="mx-0 w-auto justify-end">
                  <PaginationContent>
                    <PaginationItem>
                      <PaginationPrevious
                        disabled={currentPageClamped <= 1}
                        onClick={() => setCurrentPage((p) => Math.max(1, p - 1))}
                      />
                    </PaginationItem>

                    {Array.from({ length: totalPages }, (_, i) => i + 1)
                      .filter((page) => {
                        return (
                          page === 1 ||
                          page === totalPages ||
                          Math.abs(page - currentPageClamped) <= 1
                        );
                      })
                      .map((page, idx, arr) => {
                        const prev = arr[idx - 1];
                        return (
                          <div key={page} className="flex items-center">
                            {prev && page - prev > 1 && <PaginationEllipsis />}
                            <PaginationItem>
                              <PaginationLink
                                isActive={page === currentPageClamped}
                                onClick={() => setCurrentPage(page)}
                              >
                                {page}
                              </PaginationLink>
                            </PaginationItem>
                          </div>
                        );
                      })}

                    <PaginationItem>
                      <PaginationNext
                        disabled={currentPageClamped >= totalPages}
                        onClick={() => setCurrentPage((p) => Math.min(totalPages, p + 1))}
                      />
                    </PaginationItem>
                  </PaginationContent>
                </Pagination>
              )}
            </div>
          </>
        )}
      </div>
    </PageLayout>
  );
}
