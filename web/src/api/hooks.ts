import {
  keepPreviousData,
  useInfiniteQuery,
  useMutation,
  useQuery,
  type UseMutationResult,
} from "@tanstack/react-query";
import { apiFetch, apiGet, apiPost, sessionPath } from "./client";
import { AUTH_KEY, clearDataCache, queryClient } from "./queryClient";
import type {
  ActivityList,
  AuthState,
  ConfigInfo,
  DeviceList,
  ExportFields,
  LoginRequest,
  Overview,
  RefreshResult,
  SessionDetail,
  SessionLines,
  SessionLinesParams,
  SessionList,
  SessionListParams,
  SetupRequest,
  StorageInfo,
} from "./types";

const MINUTE = 60_000;

// ---- auth ----

export function useAuthState() {
  return useQuery({
    queryKey: AUTH_KEY,
    queryFn: ({ signal }) => apiGet<AuthState>("/api/auth/state", {}, signal),
    staleTime: 30_000,
    refetchOnWindowFocus: true,
  });
}

function markAuthenticated(): void {
  queryClient.setQueryData<AuthState>(AUTH_KEY, { initialized: true, authenticated: true });
}

export function useSetup(): UseMutationResult<void, Error, SetupRequest> {
  return useMutation({
    mutationFn: (body: SetupRequest) => apiPost<void>("/api/auth/setup", body),
    onSuccess: markAuthenticated,
  });
}

export function useLogin(): UseMutationResult<void, Error, LoginRequest> {
  return useMutation({
    mutationFn: (body: LoginRequest) => apiPost<void>("/api/auth/login", body),
    onSuccess: markAuthenticated,
  });
}

export function useLogout(): UseMutationResult<void, Error, void> {
  return useMutation({
    mutationFn: () => apiPost<void>("/api/auth/logout"),
    onSettled: () => {
      clearDataCache();
      queryClient.setQueryData<AuthState>(AUTH_KEY, { initialized: true, authenticated: false });
    },
  });
}

// ---- overview / refresh ----

export function useOverview() {
  return useQuery({
    queryKey: ["overview"],
    queryFn: ({ signal }) => apiGet<Overview>("/api/overview", {}, signal),
    staleTime: 10_000,
    refetchOnWindowFocus: true,
  });
}

export function useRefresh() {
  return useMutation({
    mutationFn: () => apiFetch<RefreshResult>("/api/refresh", { method: "POST" }),
    onSuccess: () => {
      void queryClient.invalidateQueries({ predicate: (q) => q.queryKey[0] !== AUTH_KEY[0] });
    },
  });
}

// ---- sessions ----

export function useSessions(params: SessionListParams) {
  return useQuery({
    queryKey: ["sessions", params],
    queryFn: ({ signal }) =>
      apiGet<SessionList>(
        "/api/sessions",
        {
          agent: params.agent,
          device: params.device,
          q: params.q,
          limit: params.limit,
          offset: params.offset,
          children: params.children ?? false,
        },
        signal,
      ),
    placeholderData: keepPreviousData,
    staleTime: 30_000,
  });
}

export function useSession(agent: string, id: string, version?: string) {
  return useQuery({
    queryKey: ["session", agent, id, version ?? ""],
    queryFn: ({ signal }) => apiGet<SessionDetail>(sessionPath(agent, id), { version }, signal),
    // Keep showing the same session while another version of it loads (but never another session's data).
    placeholderData: (prev, prevQuery) =>
      prevQuery?.queryKey[1] === agent && prevQuery.queryKey[2] === id ? prev : undefined,
    staleTime: 5 * MINUTE,
  });
}

export const LINES_PAGE_SIZE = 100;

/** Reader: pages through `/lines` with `offset` (counted over matching lines). */
export function useSessionLines(agent: string, id: string, params: SessionLinesParams) {
  return useInfiniteQuery({
    queryKey: ["lines", agent, id, params],
    queryFn: ({ pageParam, signal }) =>
      apiGet<SessionLines>(
        `${sessionPath(agent, id)}/lines`,
        {
          version: params.version,
          file: params.file,
          from: params.from,
          to: params.to,
          kinds: params.kinds.join(","),
          limit: LINES_PAGE_SIZE,
          offset: pageParam,
          max_field_chars: params.max_field_chars,
        },
        signal,
      ),
    initialPageParam: 0,
    getNextPageParam: (last, all) => {
      if (last.items.length === 0) return undefined;
      const loaded = all.reduce((n, p) => n + p.items.length, 0);
      return last.truncated || loaded < last.matched ? loaded : undefined;
    },
    enabled: params.kinds.length > 0,
    // Stored originals never change, so a loaded transcript stays fresh for a long time.
    staleTime: 10 * MINUTE,
  });
}

// ---- devices / config / storage / activity / export ----

export function useDevices() {
  return useQuery({
    queryKey: ["devices"],
    queryFn: ({ signal }) => apiGet<DeviceList>("/api/devices", {}, signal),
    staleTime: 10_000,
    refetchOnWindowFocus: true,
  });
}

export function useConfig() {
  return useQuery({
    queryKey: ["config"],
    queryFn: ({ signal }) => apiGet<ConfigInfo>("/api/config", {}, signal),
    staleTime: 15_000,
  });
}

export function useStorage() {
  return useQuery({
    queryKey: ["storage"],
    queryFn: ({ signal }) => apiGet<StorageInfo>("/api/storage", {}, signal),
    staleTime: 15_000,
  });
}

export function useActivity(limit: number) {
  return useQuery({
    queryKey: ["activity", limit],
    queryFn: ({ signal }) => apiGet<ActivityList>("/api/activity", { limit }, signal),
    placeholderData: keepPreviousData,
    staleTime: 15_000,
  });
}

export function useExportFields() {
  return useQuery({
    queryKey: ["export", "fields"],
    queryFn: ({ signal }) => apiGet<ExportFields>("/api/export/fields", {}, signal),
    staleTime: 10 * MINUTE,
  });
}
