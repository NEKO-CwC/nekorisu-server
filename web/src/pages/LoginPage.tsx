import { useEffect, useState, type FormEvent } from "react";
import { Navigate, useLocation } from "react-router";
import { ApiError } from "../api/client";
import { useAuthState, useLogin } from "../api/hooks";
import { describeError, ErrorMessage } from "../components/QueryView";
import { t } from "../messages";
import { useTitle } from "../useTitle";

/** Only same-site paths are valid redirect targets. */
function safeFrom(state: unknown): string {
  const from = (state as { from?: unknown } | null)?.from;
  return typeof from === "string" && from.startsWith("/") && !from.startsWith("//") ? from : "/";
}

export function LoginPage() {
  useTitle(t.login.title);
  const auth = useAuthState();
  const login = useLogin();
  const location = useLocation();
  const [password, setPassword] = useState("");
  const [waitSecs, setWaitSecs] = useState(0);

  useEffect(() => {
    if (waitSecs <= 0) return;
    const id = setTimeout(() => setWaitSecs((n) => n - 1), 1000);
    return () => clearTimeout(id);
  }, [waitSecs]);

  if (auth.isPending) return <p role="status">{t.common.loading}</p>;
  if (auth.isError) return <ErrorMessage error={auth.error} onRetry={() => void auth.refetch()} />;
  if (!auth.data.initialized) return <Navigate to="/setup" replace />;
  if (auth.data.authenticated) return <Navigate to={safeFrom(location.state)} replace />;

  function onSubmit(e: FormEvent) {
    e.preventDefault();
    login.mutate(
      { password },
      {
        onError: (err) => {
          const secs = err instanceof ApiError ? err.extra.retry_after_secs : undefined;
          if (err instanceof ApiError && err.code === "too_many_attempts" && typeof secs === "number") {
            setWaitSecs(Math.ceil(secs));
          }
        },
      },
    );
  }

  return (
    <div className="auth-page">
      <h1>{t.login.title}</h1>
      <form className="form-narrow" onSubmit={onSubmit}>
        <label htmlFor="login-password">{t.login.password}</label>
        <input
          id="login-password"
          type="password"
          autoComplete="current-password"
          required
          autoFocus
          value={password}
          onChange={(e) => setPassword(e.target.value)}
        />
        {login.isError && (
          <p role="alert" className="error">
            {describeError(login.error)}
          </p>
        )}
        <p>
          <button type="submit" disabled={login.isPending || waitSecs > 0}>
            {login.isPending ? t.login.submitting : waitSecs > 0 ? t.login.waitSecs(waitSecs) : t.login.submit}
          </button>
        </p>
      </form>
    </div>
  );
}
