import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { toast } from "sonner";

import {
  getControlPlaneEvents,
  getControlPlaneStatus,
  listWorkspaces,
} from "@/lib/api/workspaces";
import type {
  ControlPlaneEventCursor,
  RuntimeState,
  WorkspaceProfile,
} from "@/lib/types";

interface AdminContextValue {
  workspaces: WorkspaceProfile[];
  mcpRuntimeStates: Record<string, RuntimeState>;
  actionsRuntimeStates: Record<string, RuntimeState>;
  loading: boolean;
  refreshWorkspaces: () => Promise<WorkspaceProfile[]>;
  setWorkspaces: React.Dispatch<React.SetStateAction<WorkspaceProfile[]>>;
  setMcpRuntimeState: (workspaceId: string, state: RuntimeState) => void;
  setActionsRuntimeState: (workspaceId: string, state: RuntimeState) => void;
}

const AdminContext = createContext<AdminContextValue | null>(null);

function delay(ms: number): Promise<void> {
  return new Promise((resolve) => window.setTimeout(resolve, ms));
}

export function AdminProvider({ children }: { children: ReactNode }) {
  const [workspaces, setWorkspaces] = useState<WorkspaceProfile[]>([]);
  const [mcpRuntimeStates, setMcpRuntimeStates] = useState<Record<string, RuntimeState>>({});
  const [actionsRuntimeStates, setActionsRuntimeStates] = useState<Record<string, RuntimeState>>({});
  const [loading, setLoading] = useState(true);
  const mountedRef = useRef(true);

  const applyControlPlaneStatus = useCallback(
    (status: Awaited<ReturnType<typeof getControlPlaneStatus>>) => {
      const mcpStates: Record<string, RuntimeState> = {};
      const actionsStates: Record<string, RuntimeState> = {};
      for (const item of status.workspaces) {
        mcpStates[item.id] = item.mcpState;
        actionsStates[item.id] = item.actionsState;
      }
      setMcpRuntimeStates(mcpStates);
      setActionsRuntimeStates(actionsStates);
    },
    [],
  );

  const refreshWorkspaces = useCallback(async () => {
    const [items, status] = await Promise.all([listWorkspaces(), getControlPlaneStatus()]);
    if (mountedRef.current) {
      setWorkspaces(items);
      applyControlPlaneStatus(status);
    }
    return items;
  }, [applyControlPlaneStatus]);

  useEffect(() => {
    mountedRef.current = true;
    let cancelled = false;
    let cursor: ControlPlaneEventCursor | null = null;

    const observe = async () => {
      let lastFault = "";
      while (!cancelled) {
        try {
          const batch = await getControlPlaneEvents(cursor, 15_000);
          if (cancelled) return;
          cursor = batch.nextCursor;
          lastFault = "";
          if (batch.events.length > 0 || batch.resetSources.length > 0) {
            applyControlPlaneStatus(await getControlPlaneStatus());
          } else {
            const items = await listWorkspaces();
            if (cancelled) return;
            setWorkspaces((current) => {
              const currentIds = new Set(current.map((item) => item.id));
              const changed =
                items.length !== currentIds.size || items.some((item) => !currentIds.has(item.id));
              return changed ? items : current;
            });
          }
        } catch (error) {
          if (cancelled) return;
          const detail = String(error);
          if (detail !== lastFault) {
            lastFault = detail;
            toast.error("控制面事件异常", { description: detail });
          }
          await delay(3_000);
        }
      }
    };

    void refreshWorkspaces()
      .catch((error) => toast.error("加载控制面失败", { description: String(error) }))
      .finally(() => {
        if (!cancelled) {
          setLoading(false);
          void observe();
        }
      });

    return () => {
      cancelled = true;
      mountedRef.current = false;
    };
  }, [applyControlPlaneStatus, refreshWorkspaces]);

  const value = useMemo<AdminContextValue>(
    () => ({
      workspaces,
      mcpRuntimeStates,
      actionsRuntimeStates,
      loading,
      refreshWorkspaces,
      setWorkspaces,
      setMcpRuntimeState: (workspaceId, state) =>
        setMcpRuntimeStates((current) => ({ ...current, [workspaceId]: state })),
      setActionsRuntimeState: (workspaceId, state) =>
        setActionsRuntimeStates((current) => ({ ...current, [workspaceId]: state })),
    }),
    [actionsRuntimeStates, loading, mcpRuntimeStates, refreshWorkspaces, workspaces],
  );

  return <AdminContext.Provider value={value}>{children}</AdminContext.Provider>;
}

export function useAdmin() {
  const context = useContext(AdminContext);
  if (!context) throw new Error("useAdmin must be used inside AdminProvider");
  return context;
}
