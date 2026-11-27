// Types mirroring docs/api.md. Field names are exactly what the API sends (snake_case).
// Times are millisecond timestamps (`*_ms`); unavailable values are `null`.

export type Agent = "claude-code" | "codex";

/** Error body: `{"error": {"code", "message", ...extra}}` (e.g. `retry_after_secs`). */
export interface ApiErrorBody {
  error: {
    code: string;
    message: string;
    retry_after_secs?: number;
    [key: string]: unknown;
  };
}

// ---- auth ----

export interface AuthState {
  initialized: boolean;
  authenticated: boolean;
}

export interface SetupRequest {
  password: string;
  setup_code: string;
}

export interface LoginRequest {
  password: string;
}

// ---- overview ----

export interface Overview {
  version: string;
  commit: string;
  source_url: string;
  storage: { description: string; ok: boolean; error: string | null };
  counts: {
    sessions: number;
    root_sessions: number;
    devices: number;
    bytes: number;
    by_agent: Record<string, number>;
  };
  last_upload_ms: number | null;
  /** null when nothing was ever published. */
  config: { serial: number; issued_at_ms: number; etag: string } | null;
  index: { refreshed_at_ms: number | null; skipped_files: number };
}

export interface RefreshResult {
  refreshed_at_ms: number;
  sessions: number;
}

// ---- sessions ----

/** `name` can be missing for devices that only appear in logs. */
export interface DeviceRef {
  id: string;
  name: string | null;
}

export interface SessionSummary {
  /** `<agent>:<session id>` */
  ref: string;
  agent: Agent;
  id: string;
  title: string | null;
  cwd: string | null;
  agent_version: string | null;
  devices: DeviceRef[];
  first_ts_ms: number | null;
  last_ts_ms: number | null;
  uploaded_ms: number | null;
  /** bytes/files of the latest version */
  bytes: number;
  files: number;
  /** number of manifests */
  versions: number;
  /** more than 1 means a fork */
  heads: number;
  /** parent session ref (Codex child thread) */
  parent: string | null;
  children: number;
}

export interface SessionList {
  total: number;
  items: SessionSummary[];
}

export interface SessionListParams {
  agent?: string;
  device?: string;
  q?: string;
  limit?: number;
  offset?: number;
  children?: boolean;
}

export interface SessionVersion {
  id: string;
  parent: string | null;
  device: DeviceRef;
  created_ms: number;
  rewrite: boolean;
  bytes: number;
  is_head: boolean;
}

export type FileRole =
  | "transcript"
  | "subagent"
  | "tool_result"
  | "file_history"
  | "asset"
  | "other";

export interface SessionFile {
  path: string;
  role: FileRole;
  bytes: number;
  lines: number | null;
  segments: number;
}

export interface SessionDetail {
  summary: SessionSummary;
  selected_version: string;
  versions: SessionVersion[];
  files: SessionFile[];
  children: SessionSummary[];
  fork_from: { session: string; byte: number } | null;
  digests: { id: string; updated_ms: number }[];
}

export type LineKind = "user" | "assistant" | "reasoning" | "tool" | "meta";

export interface SessionLine {
  /** 1-based line number in the file */
  line: number;
  kind: LineKind;
  raw_type: string | null;
  ts_ms: number | null;
  text: string;
  text_truncated: boolean;
  raw_len: number;
  /** the original line; only when `raw=true` */
  raw: string | null;
}

export interface SessionLines {
  file: string;
  version: string;
  total_lines: number;
  matched: number;
  /** true when more lines matched than `limit` */
  truncated: boolean;
  items: SessionLine[];
}

export interface SessionLinesParams {
  version?: string;
  file?: string;
  from?: number;
  to?: number;
  kinds: LineKind[];
  limit?: number;
  offset?: number;
  max_field_chars?: number;
}

// ---- devices ----

export type RequestState = "pending" | "accepted" | "rejected";

export type FailureReason =
  | "bad_signature"
  | "unsupported_schema"
  | "resource_unavailable"
  | "resource_hash_mismatch"
  | "other";

export interface DeviceState {
  /** always false for now: the device ID is self-reported */
  verified: boolean;
  received_ms: number;
  client_version: string;
  applied_serial: number;
  applied_etag: string;
  /** current account config serial minus applied_serial, >= 0; null when no config */
  behind: number | null;
  exec_allowed: boolean;
  local_values: { key: string; value_json: string; locked: boolean }[];
  requests: { id: string; state: RequestState }[];
  last_failure: { serial: number; reason: FailureReason } | null;
}

export interface Device {
  id: string;
  /** null for devices that only appear in reports */
  name: string | null;
  first_seen_ms: number | null;
  sessions: number;
  bytes: number;
  last_upload_ms: number | null;
  /** null for devices that only appear in storage */
  state: DeviceState | null;
}

export interface DeviceList {
  items: Device[];
}

// ---- config ----

export interface ConfigResource {
  name: string;
  version: string;
  sha256: string;
  executable: boolean;
  size: number;
}

export interface ConfigCurrent {
  serial: number;
  etag: string;
  issued_at_ms: number;
  key_id: string;
  /** the signed JSON, as is */
  payload: unknown;
  resources: ConfigResource[];
}

export type ConfigRequestDeviceState = RequestState | "unknown";

export interface ConfigRequest {
  kind: "force" | "exec";
  id: string;
  /** `allow`/`revoke` for exec, null for force */
  action: "allow" | "revoke" | null;
  changes: { key: string; value_json: string }[];
  devices: { id: string; name: string | null; state: ConfigRequestDeviceState }[];
}

export interface ConfigInfo {
  /** account view; null when nothing was ever published */
  current: ConfigCurrent | null;
  public: { enabled: boolean; serial: number };
  signing: { key_id: string; public_key: string; connect_command: string };
  /** at most 50, newest first */
  history: { serial: number; view: string; issued_at_ms: number; etag: string }[];
  requests: ConfigRequest[];
  source: { dir: string; checked_ms: number | null; last_error: string | null };
  device_token_set: boolean;
}

// ---- storage ----

export interface StorageInfo {
  description: string;
  /** What a device fills in with `nekorisu storage add`; null when no storage is set. */
  address: string | null;
  username: string | null;
  ok: boolean;
  error: string | null;
  latency_ms: number | null;
  /** null: not tested yet; false: server ignores Range */
  range_read: boolean | null;
  layout: { found: boolean; entries: string[] };
}

// ---- activity ----

export interface ActivityItem {
  at_ms: number;
  seq: number;
  device: DeviceRef;
  op: string;
  sessions: string[];
  tags: string[];
  note: string | null;
  bytes: number | null;
}

export interface ActivityList {
  items: ActivityItem[];
  skipped_lines: number;
}

// ---- authorised apps (OAuth) ----

/** An app (an agent) that signed in to the MCP endpoint with OAuth. */
export interface OauthApp {
  id: string;
  /** What the app calls itself; may be empty. */
  client_name: string;
  client_id: string;
  /** true: client_id is an https URL whose document named the app; false: it registered itself. */
  metadata_document: boolean;
  /** Where the sign-in returned to: `host[:port]`. */
  redirect_host: string;
  /** true: a program on the user's own computer (localhost). */
  loopback: boolean;
  scope: string;
  created_ms: number;
  last_used_ms: number | null;
}

export interface OauthApps {
  /** null when MCP is off (NEKORISU_PUBLIC_URL not set). */
  mcp_url: string | null;
  items: OauthApp[];
}

// ---- export ----

export type ExportFormat = "jsonl" | "csv";

export interface ExportFields {
  formats: ExportFormat[];
  fields: { name: string; description: string }[];
}

// ---- path maps ----

/** A rule devices get: a working directory that starts with `from` becomes `to` plus the rest. */
export interface PathMap {
  from: string;
  /** Empty: the device drops its own rule with this `from`. */
  to: string;
  /** Device IDs; empty = every device. */
  devices: string[];
}

/** A `[[paths.map]]` rule written on a device, as it last reported it. */
export interface LocalPathMap {
  from: string;
  to: string;
  locked: boolean;
}

export interface PathMapDevice {
  id: string;
  name: string | null;
  /** null: the device has not reported yet */
  reported_ms: number | null;
  applied_serial: number | null;
  local: LocalPathMap[] | null;
}

export interface PathMaps {
  items: PathMap[];
  /** null: never saved */
  updated_ms: number | null;
  published_serial: number | null;
  /** whether the newest signed config carries exactly `items` */
  in_force: boolean;
  /** why the config directory could not be published, if it could not */
  publish_error: string | null;
  devices: PathMapDevice[];
  /** force requests made on this page */
  force: PanelForce[];
}

/** Asks one device to replace (or, with an empty `to`, drop) a path rule it locked. */
export interface PanelForce {
  id: string;
  device: string;
  from: string;
  to: string;
  created_ms: number;
  /** what the device last reported; "unknown" before it has */
  state: "pending" | "accepted" | "rejected" | "unknown";
}

export interface SavePathMapsResult {
  published: boolean;
  serial: number | null;
  error: string | null;
  /** set when a force request was made */
  id?: string;
}

export interface ForceRequestBody {
  device: string;
  from: string;
  to: string;
}
