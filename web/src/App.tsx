import { Route, Routes } from "react-router";
import { AuthGate } from "./components/AuthGate";
import { Layout } from "./components/Layout";
import { ActivityPage } from "./pages/ActivityPage";
import { ConfigPage } from "./pages/ConfigPage";
import { DevicesPage } from "./pages/DevicesPage";
import { ExportPage } from "./pages/ExportPage";
import { LoginPage } from "./pages/LoginPage";
import { NotFoundPage } from "./pages/NotFoundPage";
import { OverviewPage } from "./pages/OverviewPage";
import { SessionDetailPage } from "./pages/SessionDetailPage";
import { SessionsPage } from "./pages/SessionsPage";
import { SetupPage } from "./pages/SetupPage";
import { StoragePage } from "./pages/StoragePage";

export function App() {
  return (
    <Routes>
      <Route path="/setup" element={<SetupPage />} />
      <Route path="/login" element={<LoginPage />} />
      <Route element={<AuthGate />}>
        <Route element={<Layout />}>
          <Route index element={<OverviewPage />} />
          <Route path="sessions" element={<SessionsPage />} />
          <Route path="sessions/:agent/:id" element={<SessionDetailPage />} />
          <Route path="devices" element={<DevicesPage />} />
          <Route path="config" element={<ConfigPage />} />
          <Route path="storage" element={<StoragePage />} />
          <Route path="activity" element={<ActivityPage />} />
          <Route path="export" element={<ExportPage />} />
          <Route path="*" element={<NotFoundPage />} />
        </Route>
      </Route>
    </Routes>
  );
}
