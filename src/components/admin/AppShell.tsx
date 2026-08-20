import { FolderPlus, KeyRound, Moon, Network, Settings2, Sun, Wrench } from "lucide-react";
import { useTheme } from "next-themes";
import { NavLink, Outlet, useLocation, useNavigate } from "react-router-dom";
import { toast } from "sonner";

import { Button } from "@/components/ui/button";
import { ScrollArea } from "@/components/ui/scroll-area";
import { APP_VERSION } from "@/lib/app-version";
import { createWorkspace } from "@/lib/api/workspaces";
import { open } from "@/lib/platform/dialog";
import { cn } from "@/lib/utils";
import { RuntimeDot } from "@/components/admin/RuntimeBadge";
import { useAdmin } from "@/components/admin/AdminProvider";

const SETTINGS = [
  { to: "/settings/general", label: "通用", icon: Settings2 },
  { to: "/settings/keys", label: "共享密钥", icon: KeyRound },
  { to: "/settings/frp", label: "FRP 配置", icon: Network },
  { to: "/settings/software", label: "软件管理", icon: Wrench },
];

export function AppShell() {
  const { workspaces, mcpRuntimeStates, refreshWorkspaces } = useAdmin();
  const navigate = useNavigate();
  const location = useLocation();
  const { resolvedTheme, setTheme } = useTheme();

  const addWorkspace = async () => {
    try {
      const selected = await open({ directory: true, multiple: false });
      if (!selected || Array.isArray(selected)) return;
      const profile = await createWorkspace(selected);
      await refreshWorkspaces();
      navigate(`/workspace/${profile.id}`);
    } catch (error) {
      toast.error("添加工作区失败", { description: String(error) });
    }
  };

  return (
    <div className="grid h-dvh grid-cols-[15rem_minmax(0,1fr)] overflow-hidden bg-background text-foreground max-md:grid-cols-1">
      <aside className="flex min-h-0 flex-col border-r bg-sidebar text-sidebar-foreground max-md:hidden">
        <div className="border-b p-4">
          <div className="flex items-start justify-between gap-3">
            <div>
              <p className="text-[10px] font-semibold uppercase tracking-[0.18em] text-muted-foreground">Anchor</p>
              <h1 className="mt-1 text-base font-semibold">Web 管理控制台</h1>
            </div>
            <Button
              type="button"
              variant="ghost"
              size="icon"
              aria-label="切换主题"
              onClick={() => setTheme(resolvedTheme === "dark" ? "light" : "dark")}
            >
              {resolvedTheme === "dark" ? <Sun data-icon="inline-start" /> : <Moon data-icon="inline-start" />}
            </Button>
          </div>
          <Button type="button" className="mt-4 w-full" onClick={() => void addWorkspace()}>
            <FolderPlus data-icon="inline-start" />
            添加工作区
          </Button>
        </div>

        <ScrollArea className="min-h-0 flex-1 px-2 py-3">
          <p className="px-2 pb-2 text-[11px] font-medium uppercase tracking-[0.12em] text-muted-foreground">工作区</p>
          <div className="flex flex-col gap-1">
            {workspaces.map((workspace) => {
              const active = location.pathname === `/workspace/${workspace.id}`;
              return (
                <NavLink
                  key={workspace.id}
                  to={`/workspace/${workspace.id}`}
                  className={cn(
                    "flex items-center gap-2 rounded-lg px-3 py-2 text-sm transition-colors hover:bg-sidebar-accent",
                    active && "bg-sidebar-accent font-medium text-sidebar-accent-foreground",
                  )}
                >
                  <RuntimeDot state={mcpRuntimeStates[workspace.id] ?? "stopped"} />
                  <span className="truncate">{workspace.name}</span>
                </NavLink>
              );
            })}
            {workspaces.length === 0 && (
              <p className="px-3 py-4 text-xs text-muted-foreground">暂无工作区</p>
            )}
          </div>
        </ScrollArea>

        <div className="border-t p-2">
          <p className="px-2 pb-2 pt-1 text-[11px] font-medium uppercase tracking-[0.12em] text-muted-foreground">设置</p>
          <nav className="flex flex-col gap-1">
            {SETTINGS.map(({ to, label, icon: Icon }) => (
              <NavLink
                key={to}
                to={to}
                className={({ isActive }) =>
                  cn(
                    "flex items-center gap-2 rounded-lg px-3 py-2 text-sm transition-colors hover:bg-sidebar-accent",
                    isActive && "bg-sidebar-accent font-medium text-sidebar-accent-foreground",
                  )
                }
              >
                <Icon className="size-4" aria-hidden="true" />
                {label}
              </NavLink>
            ))}
          </nav>
          <p className="px-3 pb-1 pt-3 text-[11px] text-muted-foreground">v{APP_VERSION}</p>
        </div>
      </aside>

      <main className="min-w-0 overflow-hidden bg-muted/20">
        <Outlet />
      </main>
    </div>
  );
}
