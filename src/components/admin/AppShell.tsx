import {
  Boxes,
  ChevronRight,
  FolderPlus,
  KeyRound,
  LayoutGrid,
  Moon,
  Network,
  Settings2,
  Sun,
  Wrench,
} from "lucide-react";
import { useTheme } from "next-themes";
import { NavLink, Outlet, useLocation, useNavigate } from "react-router-dom";
import { toast } from "sonner";

import { useAdmin } from "@/components/admin/AdminProvider";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { ScrollArea } from "@/components/ui/scroll-area";
import { createWorkspace } from "@/lib/api/workspaces";
import { APP_VERSION } from "@/lib/app-version";
import { open } from "@/lib/platform/dialog";
import { cn } from "@/lib/utils";

const SETTINGS_NAV = [
  { to: "/settings/general", label: "通用设置", icon: Settings2, description: "代理、单一 Gateway 与 Windows 服务" },
  { to: "/settings/keys", label: "共享密钥", icon: KeyRound, description: "MCP 与 Actions 共享认证凭据" },
  { to: "/settings/frp", label: "FRP 配置", icon: Network, description: "服务器端点与 Token 管理" },
  { to: "/settings/software", label: "软件管理", icon: Wrench, description: "隧道与代码分析工具下载、状态管理" },
];

export function AppShell() {
  const { workspaces, refreshWorkspaces } = useAdmin();
  const navigate = useNavigate();
  const location = useLocation();
  const { resolvedTheme, setTheme } = useTheme();

  const isWorkspaceRoute =
    location.pathname.startsWith("/workspaces") ||
    location.pathname.startsWith("/workspace");

  const addWorkspace = async () => {
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

  return (
    <div className="grid h-dvh grid-cols-[16rem_minmax(0,1fr)] overflow-hidden bg-background text-foreground max-md:grid-cols-1">
      {/* 左侧 Sidebar */}
      <aside className="flex min-h-0 flex-col border-r bg-sidebar text-sidebar-foreground max-md:hidden">
        {/* 顶部 Brand 与 主题切换 */}
        <div className="border-b p-4">
          <div className="flex items-center justify-between gap-3">
            <div className="flex items-center gap-2.5">
              <div className="flex size-8 items-center justify-center rounded-lg bg-primary text-primary-foreground shadow-xs">
                <Boxes className="size-4" />
              </div>
              <div>
                <p className="text-[10px] font-semibold uppercase tracking-[0.18em] text-muted-foreground">
                  Anchor
                </p>
                <h1 className="text-sm font-semibold tracking-tight">管理控制台</h1>
              </div>
            </div>

            <Button
              type="button"
              variant="ghost"
              size="icon-sm"
              aria-label="切换主题"
              className="text-muted-foreground hover:text-foreground"
              onClick={() => setTheme(resolvedTheme === "dark" ? "light" : "dark")}
            >
              {resolvedTheme === "dark" ? (
                <Sun className="size-4" />
              ) : (
                <Moon className="size-4" />
              )}
            </Button>
          </div>

          <Button
            type="button"
            className="mt-3.5 w-full justify-start shadow-xs"
            onClick={() => void addWorkspace()}
          >
            <FolderPlus data-icon="inline-start" />
            添加工作区
          </Button>
        </div>

        {/* 侧边栏导航内容 */}
        <ScrollArea className="min-h-0 flex-1 px-3 py-4">
          <div className="flex flex-col gap-6">
            {/* 分组 1: 工作区管理 */}
            <div className="flex flex-col gap-1">
              <p className="px-2 pb-1.5 text-[11px] font-semibold uppercase tracking-[0.14em] text-muted-foreground">
                工作区管理
              </p>
              <NavLink
                to="/workspaces"
                className={cn(
                  "flex items-center justify-between rounded-lg px-3 py-2 text-sm font-medium transition-colors hover:bg-sidebar-accent hover:text-sidebar-accent-foreground",
                  isWorkspaceRoute && "bg-sidebar-accent text-sidebar-accent-foreground font-semibold",
                )}
              >
                <div className="flex items-center gap-2.5">
                  <LayoutGrid className="size-4 shrink-0 opacity-80" />
                  <span>工作区概览</span>
                </div>
                <Badge
                  variant={isWorkspaceRoute ? "default" : "secondary"}
                  className="h-5 px-1.5 text-[11px]"
                >
                  {workspaces.length}
                </Badge>
              </NavLink>
            </div>

            {/* 分组 2: 系统设置 */}
            <div className="flex flex-col gap-1">
              <p className="px-2 pb-1.5 text-[11px] font-semibold uppercase tracking-[0.14em] text-muted-foreground">
                系统设置
              </p>
              <nav className="flex flex-col gap-1">
                {SETTINGS_NAV.map(({ to, label, icon: Icon }) => (
                  <NavLink
                    key={to}
                    to={to}
                    className={({ isActive }) =>
                      cn(
                        "flex items-center justify-between rounded-lg px-3 py-2 text-sm transition-colors hover:bg-sidebar-accent hover:text-sidebar-accent-foreground",
                        isActive
                          ? "bg-sidebar-accent font-semibold text-sidebar-accent-foreground"
                          : "text-muted-foreground hover:text-foreground",
                      )
                    }
                  >
                    <div className="flex items-center gap-2.5">
                      <Icon className="size-4 shrink-0 opacity-80" />
                      <span>{label}</span>
                    </div>
                    <ChevronRight className="size-3 opacity-40" />
                  </NavLink>
                ))}
              </nav>
            </div>
          </div>
        </ScrollArea>

        {/* 侧边栏底部版本号信息 */}
        <div className="border-t p-3 text-xs text-muted-foreground">
          <div className="flex items-center justify-between px-2">
            <span>版本</span>
            <span className="font-mono text-[11px]">v{APP_VERSION}</span>
          </div>
        </div>
      </aside>

      {/* 右侧主内容区域 */}
      <main className="min-w-0 overflow-hidden bg-muted/20">
        <Outlet />
      </main>
    </div>
  );
}
