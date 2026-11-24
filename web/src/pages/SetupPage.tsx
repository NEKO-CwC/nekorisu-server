import { useState, type FormEvent } from "react";
import { Navigate } from "react-router";
import { useAuthState, useSetup } from "../api/hooks";
import { ApiError } from "../api/client";
import { AUTH_KEY, queryClient } from "../api/queryClient";
import { describeError, ErrorMessage } from "../components/QueryView";
import { t } from "../messages";
import { useTitle } from "../useTitle";

const MIN_PASSWORD_CHARS = 10;

export function SetupPage() {
  useTitle(t.setup.title);
  const auth = useAuthState();
  const setup = useSetup();
  const [password, setPassword] = useState("");
  const [confirm, setConfirm] = useState("");
  const [code, setCode] = useState("");
  const [localError, setLocalError] = useState<string | null>(null);

  if (auth.isPending) return <p role="status">{t.common.loading}</p>;
  if (auth.isError) return <ErrorMessage error={auth.error} onRetry={() => void auth.refetch()} />;
  // Already initialized (or just finished setting up, which also logs in): leave this page.
  if (auth.data.initialized) return <Navigate to={auth.data.authenticated ? "/" : "/login"} replace />;

  function onSubmit(e: FormEvent) {
    e.preventDefault();
    setLocalError(null);
    if ([...password].length < MIN_PASSWORD_CHARS) {
      setLocalError(t.setup.tooShort);
      return;
    }
    if (password !== confirm) {
      setLocalError(t.setup.mismatch);
      return;
    }
    setup.mutate(
      { password, setup_code: code.trim() },
      {
        onError: (err) => {
          if (err instanceof ApiError && err.code === "already_initialized") {
            void queryClient.invalidateQueries({ queryKey: AUTH_KEY });
          }
        },
      },
    );
  }

  const error = localError ?? (setup.isError ? describeError(setup.error) : null);

  return (
    <div className="auth-page">
      <h1>{t.setup.title}</h1>
      <p>{t.setup.intro}</p>
      <p>{t.setup.codeHint}</p>
      <form className="form-narrow" onSubmit={onSubmit}>
        <label htmlFor="setup-password">{t.setup.password}</label>
        <input
          id="setup-password"
          type="password"
          autoComplete="new-password"
          minLength={MIN_PASSWORD_CHARS}
          required
          value={password}
          onChange={(e) => setPassword(e.target.value)}
        />
        <label htmlFor="setup-confirm">{t.setup.confirm}</label>
        <input
          id="setup-confirm"
          type="password"
          autoComplete="new-password"
          required
          value={confirm}
          onChange={(e) => setConfirm(e.target.value)}
        />
        <label htmlFor="setup-code">{t.setup.code}</label>
        <input
          id="setup-code"
          type="text"
          autoComplete="off"
          spellCheck={false}
          required
          value={code}
          onChange={(e) => setCode(e.target.value)}
        />
        {error && (
          <p role="alert" className="error">
            {error}
          </p>
        )}
        <p>
          <button type="submit" disabled={setup.isPending}>
            {setup.isPending ? t.setup.submitting : t.setup.submit}
          </button>
        </p>
      </form>
    </div>
  );
}
