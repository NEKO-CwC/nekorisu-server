import type { ApiErrorBody } from "./types";

export class ApiError extends Error {
  readonly status: number;
  readonly code: string;
  /** Everything in the error object besides `code` and `message` (e.g. `retry_after_secs`). */
  readonly extra: Record<string, unknown>;

  constructor(status: number, code: string, message: string, extra: Record<string, unknown> = {}) {
    super(message);
    this.name = "ApiError";
    this.status = status;
    this.code = code;
    this.extra = extra;
  }
}

type QueryValue = string | number | boolean | null | undefined | readonly string[];

/** Builds a query string. Empty/undefined values are omitted; arrays repeat the key. */
export function buildQuery(params: Record<string, QueryValue>): string {
  const usp = new URLSearchParams();
  for (const [key, value] of Object.entries(params)) {
    if (value === undefined || value === null || value === "") continue;
    if (Array.isArray(value)) {
      for (const v of value) usp.append(key, v);
    } else {
      usp.append(key, String(value));
    }
  }
  const s = usp.toString();
  return s ? `?${s}` : "";
}

interface RequestOptions {
  method?: "GET" | "POST";
  body?: unknown;
  signal?: AbortSignal;
}

function parseErrorBody(status: number, text: string): ApiError {
  try {
    const parsed = JSON.parse(text) as Partial<ApiErrorBody>;
    const err = parsed.error;
    if (err && typeof err === "object" && typeof err.code === "string") {
      const { code, message, ...extra } = err;
      return new ApiError(status, code, typeof message === "string" ? message : code, extra);
    }
  } catch {
    // not JSON: fall through
  }
  return new ApiError(status, `http_${status}`, text.slice(0, 200) || `HTTP ${status}`);
}

/** Small typed fetch wrapper: same-origin cookies, JSON in and out, throws ApiError. */
export async function apiFetch<T>(path: string, options: RequestOptions = {}): Promise<T> {
  const { method = "GET", body, signal } = options;
  const headers: Record<string, string> = { Accept: "application/json" };
  if (body !== undefined) headers["Content-Type"] = "application/json";

  let res: Response;
  try {
    res = await fetch(path, {
      method,
      headers,
      credentials: "same-origin",
      body: body === undefined ? undefined : JSON.stringify(body),
      signal,
    });
  } catch (e) {
    if (e instanceof DOMException && e.name === "AbortError") throw e;
    throw new ApiError(0, "network_error", e instanceof Error ? e.message : "network error");
  }

  if (!res.ok) {
    throw parseErrorBody(res.status, await res.text());
  }
  if (res.status === 204) return undefined as T;

  const text = await res.text();
  if (text === "") return undefined as T;
  try {
    return JSON.parse(text) as T;
  } catch {
    throw new ApiError(res.status, "bad_response", "response is not JSON");
  }
}

export function apiGet<T>(path: string, params: Record<string, QueryValue> = {}, signal?: AbortSignal): Promise<T> {
  return apiFetch<T>(`${path}${buildQuery(params)}`, { signal });
}

export function apiPost<T>(path: string, body?: unknown): Promise<T> {
  return apiFetch<T>(path, { method: "POST", body });
}

/** Session path segments: `/api/sessions/{agent}/{id}`. */
export function sessionPath(agent: string, id: string): string {
  return `/api/sessions/${encodeURIComponent(agent)}/${encodeURIComponent(id)}`;
}

/** Splits `<agent>:<id>` at the first colon. */
export function splitRef(ref: string): { agent: string; id: string } | null {
  const i = ref.indexOf(":");
  if (i <= 0 || i === ref.length - 1) return null;
  return { agent: ref.slice(0, i), id: ref.slice(i + 1) };
}
