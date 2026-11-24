import { Navigate, Outlet, useLocation } from "react-router";
import { useAuthState } from "../api/hooks";
import { ErrorMessage } from "./QueryView";
import { t } from "../messages";

/** Everything below requires an initialized server and a logged-in session. */
export function AuthGate() {
  const auth = useAuthState();
  const location = useLocation();

  if (auth.isPending) return <p role="status">{t.common.loading}</p>;
  if (auth.isError) return <ErrorMessage error={auth.error} onRetry={() => void auth.refetch()} />;
  if (!auth.data.initialized) return <Navigate to="/setup" replace />;
  if (!auth.data.authenticated) {
    return <Navigate to="/login" replace state={{ from: `${location.pathname}${location.search}` }} />;
  }
  return <Outlet />;
}
