import { lazy, Suspense } from "react";
import { Navigate, Route, Routes } from "react-router-dom";

import { AdminProvider } from "@/components/admin/AdminProvider";
import { AppShell } from "@/components/admin/AppShell";

const WorkspacePage = lazy(() =>
  import("@/pages/WorkspacePage").then((module) => ({ default: module.WorkspacePage })),
);
const GeneralSettingsPage = lazy(() =>
  import("@/pages/settings/GeneralSettingsPage").then((module) => ({ default: module.GeneralSettingsPage })),
);
const KeysSettingsPage = lazy(() =>
  import("@/pages/settings/KeysSettingsPage").then((module) => ({ default: module.KeysSettingsPage })),
);
const FrpSettingsPage = lazy(() =>
  import("@/pages/settings/FrpSettingsPage").then((module) => ({ default: module.FrpSettingsPage })),
);
const SoftwareSettingsPage = lazy(() =>
  import("@/pages/settings/SoftwareSettingsPage").then((module) => ({ default: module.SoftwareSettingsPage })),
);

function PageFallback() {
  return (
    <div className="grid h-full place-items-center p-8 text-sm text-muted-foreground" role="status">
      正在加载管理页面…
    </div>
  );
}

export function App() {
  return (
    <AdminProvider>
      <Suspense fallback={<PageFallback />}>
        <Routes>
          <Route element={<AppShell />}>
            <Route index element={<Navigate replace to="/workspace" />} />
            <Route path="workspace" element={<WorkspacePage />} />
            <Route path="workspace/:id" element={<WorkspacePage />} />
            <Route path="settings/general" element={<GeneralSettingsPage />} />
            <Route path="settings/keys" element={<KeysSettingsPage />} />
            <Route path="settings/frp" element={<FrpSettingsPage />} />
            <Route path="settings/software" element={<SoftwareSettingsPage />} />
            <Route path="*" element={<Navigate replace to="/workspace" />} />
          </Route>
        </Routes>
      </Suspense>
    </AdminProvider>
  );
}
