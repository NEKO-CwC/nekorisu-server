import type { UseQueryResult } from "@tanstack/react-query";
import type { ReactNode } from "react";
import { ApiError } from "../api/client";
import { errorText, t } from "../messages";

export function describeError(error: unknown): string {
  if (error instanceof ApiError) {
    const secs = error.extra.retry_after_secs;
    if (error.code === "too_many_attempts" && typeof secs === "number") return t.tooManyRetry(secs);
    return errorText(error.code, error.message);
  }
  return error instanceof Error ? error.message : String(error);
}

export function ErrorMessage({ error, onRetry }: { error: unknown; onRetry?: () => void }) {
  return (
    <p role="alert" className="error">
      <strong>{t.common.loadFailed}：</strong>
      {describeError(error)}{" "}
      {onRetry && (
        <button type="button" onClick={onRetry}>
          {t.common.retry}
        </button>
      )}
    </p>
  );
}

/** Renders loading / error / data for a query. */
export function QueryView<T>({
  query,
  children,
}: {
  query: UseQueryResult<T>;
  children: (data: T) => ReactNode;
}) {
  if (query.isPending) return <p role="status">{t.common.loading}</p>;
  if (query.isError) return <ErrorMessage error={query.error} onRetry={() => void query.refetch()} />;
  return <>{children(query.data)}</>;
}
