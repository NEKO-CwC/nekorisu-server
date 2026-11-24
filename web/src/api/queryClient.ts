import { MutationCache, QueryCache, QueryClient } from "@tanstack/react-query";
import { ApiError } from "./client";
import type { AuthState } from "./types";

export const AUTH_KEY = ["auth", "state"] as const;

declare module "@tanstack/react-query" {
  interface Register {
    defaultError: ApiError;
  }
}

/** Any 401 (other than a failed login attempt) means the session is gone: drop cached data, re-check auth. */
function handleError(error: unknown): void {
  if (!(error instanceof ApiError) || error.status !== 401 || error.code === "bad_credentials") return;
  const current = queryClient.getQueryData<AuthState>(AUTH_KEY);
  if (current && !current.authenticated) return;
  queryClient.setQueryData<AuthState | undefined>(AUTH_KEY, (old) => (old ? { ...old, authenticated: false } : old));
  queryClient.removeQueries({ predicate: (q) => q.queryKey[0] !== AUTH_KEY[0] });
  void queryClient.invalidateQueries({ queryKey: AUTH_KEY });
}

export const queryClient = new QueryClient({
  queryCache: new QueryCache({ onError: handleError }),
  mutationCache: new MutationCache({ onError: handleError }),
  defaultOptions: {
    queries: {
      staleTime: 30_000,
      gcTime: 5 * 60_000,
      refetchOnWindowFocus: false,
      retry: (failureCount, error) => {
        if (error instanceof ApiError && error.status >= 400 && error.status < 500) return false;
        return failureCount < 1;
      },
    },
  },
});

/** Forget everything except the auth state (used on logout). */
export function clearDataCache(): void {
  queryClient.removeQueries({ predicate: (q) => q.queryKey[0] !== AUTH_KEY[0] });
}
