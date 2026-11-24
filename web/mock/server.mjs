// Mock of the nekorisu-server HTTP API (docs/api.md) for developing and checking the panel.
// Dev tooling only: no packages, all data is invented. Run: `pnpm mock` (port 8080).
//
// Switches (environment variables):
//   MOCK_INITIALIZED=1   start already set up; the password is "mock-password-1"
//   MOCK_CONFIG_ERROR=1  config source.last_error is non-null
//   MOCK_CONFIG_NULL=1   nothing was ever published (config.current is null)
//   MOCK_RANGE=false|null  storage range_read value (default true)
//   MOCK_STORAGE_ERROR=1 storage is down
//   PORT                 default 8080

import { createHash, randomBytes } from "node:crypto";
import { createServer } from "node:http";

const PORT = Number(process.env.PORT ?? 8080);
const SETUP_CODE = "MOCKCODE";
const NOW = Date.now();
const MIN = 60_000;
const HOUR = 60 * MIN;
const DAY = 24 * HOUR;

// ---------------------------------------------------------------- helpers

function mulberry32(seed) {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

function hashSeed(text) {
  return createHash("sha256").update(text).digest().readUInt32BE(0);
}

const sha = (text) => createHash("sha256").update(text).digest("hex");

function json(res, status, body, headers = {}) {
  const text = JSON.stringify(body);
  res.writeHead(status, { "Content-Type": "application/json; charset=utf-8", ...headers });
  res.end(text);
}

function apiError(res, status, code, message, extra = {}) {
  json(res, status, { error: { code, message, ...extra } });
}

// ---------------------------------------------------------------- synthetic data

const DEVICES = [
  { id: "d1", name: "mac-mini", first_seen_ms: NOW - 40 * DAY },
  { id: "d2", name: "work-laptop", first_seen_ms: NOW - 25 * DAY },
  { id: "d3", name: "old-desktop", first_seen_ms: NOW - 90 * DAY },
];
const deviceRef = (id) => {
  const d = DEVICES.find((x) => x.id === id);
  return { id, name: d ? d.name : null };
};

const PROJECTS = ["billing-api", "notes-app", "weekly-report", "game-prototype", "infra-scripts", "docs-site"];
const TITLES = [
  "重构登录模块的错误处理",
  "给日期解析加单元测试",
  "整理周报脚本",
  "排查构建变慢的原因",
  "把配置文件换成 TOML",
  "为什么列表页会闪一下",
  "写一个导出 CSV 的小工具",
  "迁移数据库的计划",
  "修复时区导致的统计偏差",
  "梳理接口文档",
  "给命令行加 --dry-run",
  "清理没用的依赖",
];

const USER_TEXTS = [
  "帮我看看登录失败的时候为什么没有任何提示。",
  "给 parse_date 加几个边界情况的测试，比如闰年和空字符串。",
  "把这个函数拆成两个，名字起得直白一点。",
  "构建从 40 秒变成 3 分钟了，帮我找找原因。",
  "不要改公开接口，只改内部实现。",
  "再跑一遍测试，确认没有别的地方坏掉。",
  "这个报错是什么意思？\n\n    thread 'main' panicked at src/lib.rs:42:9\n    called `Option::unwrap()` on a `None` value",
  "好，就按第二个方案做。",
];

const ASSISTANT_TEXTS = [
  "好的，我先看一下 `auth.rs` 里的错误分支，再决定怎么改。",
  "找到了：错误在 `login()` 里被 `?` 直接吞掉了，调用方拿不到原因。\n\n```rust\nfn login(&self, pw: &str) -> Result<Session, AuthError> {\n    let hash = self.store.hash()?;\n    verify(pw, &hash).map_err(AuthError::BadCredentials)\n}\n```\n\n这样调用方可以按错误类型分支。",
  "测试都通过了，一共 38 个，耗时 1.2 秒。",
  "有两个方案：\n1. 在入口处统一转换错误；\n2. 每个调用点各自处理。\n\n我建议第一个，改动小，也不会漏。",
  "已经改好了，改动在 3 个文件里，没有动公开接口。",
];

const REASONING_TEXTS = [
  "用户想要的是提示信息，先定位错误是在哪一层被吞掉的。",
  "先不要动公开接口，只改内部实现；改完要跑测试确认。",
  "两个方案各有利弊：统一转换更简单，但会丢失调用点的上下文。",
];

const TOOL_TEXTS = [
  "Read src/auth.rs (120 lines)",
  "Bash: cargo test -q → exit 0",
  "Edit src/auth.rs: 1 replacement",
  "Grep \"unwrap()\" in src/ → 7 matches",
  "Bash: ls -la → exit 0",
];

const META_TEXTS = ["file-history-snapshot", "summary", "system: context compacted", "turn_context"];

function makeVersions(ref, deviceIds, count, baseTs, lines) {
  const rand = mulberry32(hashSeed(ref));
  const out = [];
  for (let i = 1; i <= count; i++) {
    const dev = deviceIds[(i - 1) % deviceIds.length];
    out.push({
      id: `${dev}-${i}`,
      parent: i === 1 ? null : out[i - 2].id,
      device: dev,
      created_ms: baseTs + i * 20 * MIN,
      rewrite: false,
      bytes: Math.floor((20_000 + rand() * 200_000) * (0.4 + (0.6 * i) / count)),
      lines: Math.max(8, Math.floor((lines * i) / count)),
    });
  }
  return out;
}

function addSession(list, def) {
  const ref = `${def.agent}:${def.id}`;
  const projectDir = (def.cwd ?? "/Users/demo/scratch").replaceAll("/", "-");
  list.push({
    ref,
    agent: def.agent,
    id: def.id,
    title: def.title ?? null,
    cwd: def.cwd ?? null,
    agent_version: def.agent_version ?? (def.agent === "codex" ? "0.58.0" : "2.1.322"),
    parent: def.parent ?? null,
    fork_from: def.fork_from ?? null,
    digests: def.digests ?? [],
    first_ts_ms: def.first_ts_ms,
    versions: def.versions,
    mainFile:
      def.agent === "codex"
        ? `sessions/2026/11/rollout-${def.id}.jsonl`
        : `projects/${projectDir}/${def.id}.jsonl`,
    extraFiles: def.extraFiles ?? [],
  });
}

function buildSessions() {
  const list = [];

  // 1. A Claude Code session with a fork (2 heads), 4 versions, one of them a rewrite.
  {
    const base = NOW - 6 * HOUR;
    const v = makeVersions("claude-code:73ffc2c4", ["d1"], 3, base, 345);
    v.push({
      id: "d2-1",
      parent: "d1-2",
      device: "d2",
      created_ms: base + 3 * HOUR,
      rewrite: true,
      bytes: 98_000,
      lines: 330,
    });
    addSession(list, {
      agent: "claude-code",
      id: "73ffc2c4",
      title: "重构登录模块的错误处理",
      cwd: "/Users/demo/projects/billing-api",
      first_ts_ms: base - 2 * HOUR,
      versions: v,
      digests: [
        { id: "dg1", updated_ms: base + 4 * HOUR },
        { id: "dg2", updated_ms: base + 5 * HOUR },
      ],
      extraFiles: [
        { path: "projects/-Users-demo-projects-billing-api/73ffc2c4/subagents/agent-1.jsonl", role: "subagent", lines: 42 },
        { path: "projects/-Users-demo-projects-billing-api/73ffc2c4/tool-results/t1.txt", role: "tool_result", lines: 12 },
        { path: "projects/-Users-demo-projects-billing-api/73ffc2c4/assets/screenshot.png", role: "asset", lines: null },
      ],
    });
  }

  // 2. A session forked out of the first one.
  addSession(list, {
    agent: "claude-code",
    id: "c0ffee01",
    title: "登录模块：另一种方案（从上面的会话分出来）",
    cwd: "/Users/demo/projects/billing-api",
    first_ts_ms: NOW - 5 * HOUR,
    versions: makeVersions("claude-code:c0ffee01", ["d2"], 2, NOW - 5 * HOUR, 120),
    fork_from: { session: "claude-code:73ffc2c4", byte: 5000 },
  });

  // 3. A Codex parent with a child thread.
  addSession(list, {
    agent: "codex",
    id: "019a0001-aaaa",
    title: "整理周报脚本",
    cwd: "/Users/demo/projects/weekly-report",
    first_ts_ms: NOW - 30 * HOUR,
    versions: makeVersions("codex:019a0001-aaaa", ["d1"], 2, NOW - 30 * HOUR, 90),
  });
  addSession(list, {
    agent: "codex",
    id: "019a0001-bbbb",
    title: "子线程：解析日期",
    cwd: "/Users/demo/projects/weekly-report",
    first_ts_ms: NOW - 29 * HOUR,
    parent: "codex:019a0001-aaaa",
    versions: makeVersions("codex:019a0001-bbbb", ["d1"], 1, NOW - 29 * HOUR, 40),
  });

  // 4. No title; and one whose title and text try to look like HTML (must be shown as text).
  addSession(list, {
    agent: "claude-code",
    id: "5d5d5d5d",
    title: null,
    cwd: null,
    first_ts_ms: NOW - 3 * DAY,
    versions: makeVersions("claude-code:5d5d5d5d", ["d3"], 1, NOW - 3 * DAY, 25),
  });
  addSession(list, {
    agent: "claude-code",
    id: "ab12cd34",
    title: "测试：<b>粗体</b> 和 <img src=x onerror=alert(1)> 要原样显示",
    cwd: "/Users/demo/projects/docs-site",
    first_ts_ms: NOW - 2 * DAY,
    versions: makeVersions("claude-code:ab12cd34", ["d2"], 1, NOW - 2 * DAY, 60),
  });

  // 5. Plenty of ordinary sessions, to exercise paging and filters.
  const rand = mulberry32(42);
  for (let i = 0; i < 118; i++) {
    const agent = rand() < 0.2 ? "codex" : "claude-code";
    const id = agent === "codex" ? `019a${String(1000 + i)}-${Math.floor(rand() * 0xffff).toString(16).padStart(4, "0")}` : Math.floor(rand() * 0xffffffff).toString(16).padStart(8, "0");
    const project = PROJECTS[Math.floor(rand() * PROJECTS.length)];
    const devs = rand() < 0.2 ? ["d1", "d2"] : [DEVICES[Math.floor(rand() * 3)].id];
    const first = NOW - (4 + i * 5) * HOUR - Math.floor(rand() * HOUR);
    const title = rand() < 0.1 ? null : `${TITLES[Math.floor(rand() * TITLES.length)]} #${i + 1}`;
    addSession(list, {
      agent,
      id,
      title,
      cwd: `/Users/demo/projects/${project}`,
      first_ts_ms: first,
      versions: makeVersions(`${agent}:${id}`, devs, 1 + Math.floor(rand() * 3), first, 30 + Math.floor(rand() * 250)),
    });
  }
  return list;
}

let sessions = buildSessions();
const byRef = new Map(sessions.map((s) => [s.ref, s]));

function headsOf(s) {
  const parents = new Set(s.versions.map((v) => v.parent).filter(Boolean));
  return s.versions.filter((v) => !parents.has(v.id));
}
function latestHead(s) {
  return [...headsOf(s)].sort((a, b) => b.created_ms - a.created_ms)[0];
}

function summarize(s) {
  const head = latestHead(s);
  const deviceIds = [...new Set(s.versions.map((v) => v.device))];
  return {
    ref: s.ref,
    agent: s.agent,
    id: s.id,
    title: s.title,
    cwd: s.cwd,
    agent_version: s.agent_version,
    devices: deviceIds.map(deviceRef),
    first_ts_ms: s.first_ts_ms,
    last_ts_ms: head.created_ms - 3 * MIN,
    uploaded_ms: Math.max(...s.versions.map((v) => v.created_ms)),
    bytes: head.bytes,
    files: 1 + s.extraFiles.length,
    versions: s.versions.length,
    heads: headsOf(s).length,
    parent: s.parent,
    children: sessions.filter((c) => c.parent === s.ref).length,
  };
}

function filesOf(s, version) {
  const head = s.versions.find((v) => v.id === version) ?? latestHead(s);
  return [
    { path: s.mainFile, role: "transcript", bytes: head.bytes, lines: head.lines, segments: 1 + Math.floor(head.bytes / 150_000) },
    ...s.extraFiles.map((f) => ({
      path: f.path,
      role: f.role,
      bytes: f.role === "asset" ? 84_211 : 6_400,
      lines: f.lines,
      segments: 1,
    })),
  ];
}

// ---------------------------------------------------------------- transcript lines

function lineFor(s, versionId, filePath, n, firstTs) {
  const rand = mulberry32(hashSeed(`${s.ref}|${versionId}|${filePath}|${n}`));
  const pick = (arr) => arr[Math.floor(rand() * arr.length)];
  const ts_ms = firstTs + n * 45_000;
  const slot = n % 7;
  let kind;
  let raw_type;
  let text;
  if (n === 5) {
    kind = "user";
    raw_type = "user";
    text = `${"请把下面这些要点整理成一个清单，按重要程度排序。\n".repeat(1)}${Array.from({ length: 30 }, (_, i) => `  - 要点 ${i + 1}：这是合成的示例内容，用来测试长文本的折叠。`).join("\n")}`;
  } else if (n === 12) {
    kind = "user";
    raw_type = "user";
    text = '记录里可能有 HTML：<script>alert("x")</script> <a href="javascript:alert(1)">链接</a>\n缩进和空格   要   保留。';
  } else if (n % 53 === 0) {
    kind = "assistant";
    raw_type = "assistant";
    text = `${"这是一段很长的合成回复，用来测试截断提示。".repeat(260)}`;
  } else if (slot === 1) {
    kind = "user";
    raw_type = "user";
    text = pick(USER_TEXTS);
  } else if (slot === 2) {
    kind = "reasoning";
    raw_type = "thinking";
    text = pick(REASONING_TEXTS);
  } else if (slot === 3 || slot === 4) {
    kind = "tool";
    raw_type = slot === 3 ? "tool_use" : "tool_result";
    text = pick(TOOL_TEXTS);
  } else if (slot === 6) {
    kind = "meta";
    raw_type = pick(META_TEXTS);
    text = rand() < 0.5 ? "" : pick(META_TEXTS);
  } else {
    kind = "assistant";
    raw_type = "assistant";
    text = pick(ASSISTANT_TEXTS);
  }
  return { line: n, kind, raw_type, ts_ms, text, raw_len: text.length + 120 };
}

// ---------------------------------------------------------------- config

const CONFIG_NULL = process.env.MOCK_CONFIG_NULL === "1";
const CONFIG_SERIAL = 7;
const PUBLIC_KEY = randomBytes(32).toString("base64");
const KEY_ID = sha("mock-key").slice(0, 16);

function buildConfig() {
  const resources = [
    { name: "excerpt-default", version: "2026-11-23.1", sha256: sha("excerpt-default"), executable: false, size: 1204 },
    { name: "weekly-summary-script", version: "2026-11-20.3", sha256: sha("weekly-summary-script"), executable: true, size: 2048 },
  ];
  const payload = {
    schema: "1.0",
    serial: CONFIG_SERIAL,
    issued_at: new Date(NOW - 2 * HOUR).toISOString(),
    min_client_version: "0.1.0",
    poll_interval_secs: 3600,
    values: { "excerpt.max_chars": 2000, "local.copy_form": "summary" },
    resources: resources.map(({ name, version, sha256, executable }) => ({ name, version, sha256, executable })),
    force: [{ id: "f-001", changes: { "local.copy_form": "full" } }],
    requests: [
      { id: "x-001", kind: "exec", action: "allow" },
      { id: "x-002", kind: "exec", action: "revoke" },
    ],
  };
  const etag = sha(JSON.stringify(payload));
  return {
    current: CONFIG_NULL
      ? null
      : { serial: CONFIG_SERIAL, etag, issued_at_ms: NOW - 2 * HOUR, key_id: KEY_ID, payload, resources },
    public: { enabled: false, serial: CONFIG_NULL ? 0 : CONFIG_SERIAL - 1 },
    signing: {
      key_id: KEY_ID,
      public_key: PUBLIC_KEY,
      connect_command: `nekorisu source add home http://localhost:5173 --key ${PUBLIC_KEY}`,
    },
    history: CONFIG_NULL
      ? []
      : [7, 6, 5, 4, 3, 2, 1].map((serial) => ({
          serial,
          view: serial % 2 === 1 ? "account" : "public",
          issued_at_ms: NOW - (2 + (7 - serial) * 9) * HOUR,
          etag: sha(`etag-${serial}`),
        })),
    requests: CONFIG_NULL
      ? []
      : [
          {
            kind: "force",
            id: "f-001",
            action: null,
            changes: [{ key: "local.copy_form", value_json: '"full"' }],
            devices: [
              { id: "d1", name: "mac-mini", state: "pending" },
              { id: "d2", name: "work-laptop", state: "accepted" },
              { id: "d9", name: null, state: "unknown" },
            ],
          },
          {
            kind: "exec",
            id: "x-001",
            action: "allow",
            changes: [],
            devices: [
              { id: "d1", name: "mac-mini", state: "rejected" },
              { id: "d2", name: "work-laptop", state: "accepted" },
            ],
          },
          {
            kind: "exec",
            id: "x-002",
            action: "revoke",
            changes: [],
            devices: [{ id: "d2", name: "work-laptop", state: "pending" }],
          },
        ],
    source: {
      dir: "/config",
      checked_ms: NOW - 20_000,
      last_error: process.env.MOCK_CONFIG_ERROR === "1" ? "distribution.toml 第 12 行：键 local.nope 不存在，这个版本没有发布" : null,
    },
    device_token_set: true,
  };
}

function buildDevices() {
  const behind = CONFIG_NULL ? null : Math.max(0, CONFIG_SERIAL - 6);
  const out = DEVICES.map((d) => {
    const own = sessions.filter((s) => s.versions.some((v) => v.device === d.id));
    const lastUpload = own.length ? Math.max(...own.flatMap((s) => s.versions.filter((v) => v.device === d.id).map((v) => v.created_ms))) : null;
    return {
      id: d.id,
      name: d.name,
      first_seen_ms: d.first_seen_ms,
      sessions: own.length,
      bytes: own.reduce((n, s) => n + (latestHead(s)?.bytes ?? 0), 0),
      last_upload_ms: lastUpload,
      state: null,
    };
  });
  out[0].state = {
    verified: false,
    received_ms: NOW - 5 * MIN,
    client_version: "0.1.0",
    applied_serial: 6,
    applied_etag: sha("etag-6"),
    behind,
    exec_allowed: false,
    local_values: [
      { key: "local.copy_form", value_json: '"full"', locked: true },
      { key: "excerpt.max_chars", value_json: "2000", locked: false },
    ],
    requests: [{ id: "f-001", state: "pending" }],
    last_failure: { serial: 7, reason: "bad_signature" },
  };
  out[1].state = {
    verified: false,
    received_ms: NOW - 40 * MIN,
    client_version: "0.1.1",
    applied_serial: CONFIG_NULL ? 0 : CONFIG_SERIAL,
    applied_etag: sha("etag-7"),
    behind: CONFIG_NULL ? null : 0,
    exec_allowed: true,
    local_values: [],
    requests: [
      { id: "f-001", state: "accepted" },
      { id: "x-001", state: "accepted" },
    ],
    last_failure: null,
  };
  // d3: only in storage, no state. d9: only in reports.
  out.push({
    id: "d9",
    name: null,
    first_seen_ms: null,
    sessions: 0,
    bytes: 0,
    last_upload_ms: null,
    state: {
      verified: false,
      received_ms: NOW - 3 * HOUR,
      client_version: "0.0.9",
      applied_serial: 2,
      applied_etag: sha("etag-2"),
      behind: CONFIG_NULL ? null : CONFIG_SERIAL - 2,
      exec_allowed: false,
      local_values: [],
      requests: [],
      last_failure: { serial: 5, reason: "unsupported_schema" },
    },
  });
  return out;
}

function buildActivity() {
  const rand = mulberry32(7);
  const ops = ["archive", "append", "tag", "archive", "rename"];
  const items = [];
  for (let i = 0; i < 60; i++) {
    const s = sessions[Math.floor(rand() * sessions.length)];
    const dev = DEVICES[Math.floor(rand() * 3)].id;
    items.push({
      at_ms: NOW - i * 37 * MIN - Math.floor(rand() * MIN),
      seq: 60 - i,
      device: deviceRef(dev),
      op: ops[Math.floor(rand() * ops.length)],
      sessions: rand() < 0.15 ? [] : rand() < 0.2 ? [s.ref, sessions[(i * 7) % sessions.length].ref] : [s.ref],
      tags: rand() < 0.3 ? ["manual"] : [],
      note: rand() < 0.2 ? "合成的备注" : null,
      bytes: rand() < 0.1 ? null : Math.floor(rand() * 900_000),
    });
  }
  return items;
}

const EXPORT_FIELDS = [
  ["session", "会话（agent:ID）"],
  ["agent", "Agent"],
  ["device", "来源设备"],
  ["cwd", "工作目录"],
  ["turn", "这是会话里第几轮，从 1 数"],
  ["at", "这条原话的时间"],
  ["user", "用户原话，原样"],
  ["prev_reply", "它回应的那条 Agent 回复"],
  ["gap_since_reply", "距离上一条回复的毫秒数"],
  ["gap_since_user", "距离上一条原话的毫秒数"],
  ["model", "这一轮用的模型"],
  ["tokens", "这一轮所有模型请求的 token 合计（input、output、cache_read、cache_write）"],
  ["requests", "模型请求次数"],
  ["subagents", "派生的子 Agent 数"],
  ["reasoning_chars", "思考的长度"],
  ["agent_version", "这一轮涉及的程序版本"],
  ["parse", "ok，或 partial：有字段没取到"],
].map(([name, description]) => ({ name, description }));

// ---------------------------------------------------------------- state

let password = process.env.MOCK_INITIALIZED === "1" ? "mock-password-1" : null;
const authed = new Set();
let failures = 0;
let blockedUntil = 0;
let refreshedAt = NOW - 2 * MIN;

function parseCookies(req) {
  const out = {};
  for (const part of (req.headers.cookie ?? "").split(";")) {
    const i = part.indexOf("=");
    if (i > 0) out[part.slice(0, i).trim()] = part.slice(i + 1).trim();
  }
  return out;
}
const isAuthed = (req) => authed.has(parseCookies(req).nk_session);

async function readBody(req) {
  const chunks = [];
  for await (const c of req) chunks.push(c);
  const text = Buffer.concat(chunks).toString("utf8");
  if (!text) return {};
  try {
    return JSON.parse(text);
  } catch {
    return null;
  }
}

function startSession(res) {
  const token = randomBytes(24).toString("hex");
  authed.add(token);
  res.writeHead(204, { "Set-Cookie": `nk_session=${token}; HttpOnly; SameSite=Strict; Path=/` });
  res.end();
}

function rateLimited(res) {
  if (Date.now() < blockedUntil) {
    apiError(res, 429, "too_many_attempts", "尝试次数太多", { retry_after_secs: Math.ceil((blockedUntil - Date.now()) / 1000) });
    return true;
  }
  return false;
}
function recordFailure() {
  failures += 1;
  if (failures >= 5) {
    failures = 0;
    blockedUntil = Date.now() + 20_000;
  }
}

// ---------------------------------------------------------------- handlers

function listSessions(url, res) {
  const q = url.searchParams;
  const agent = q.get("agent");
  const device = q.get("device");
  const text = (q.get("q") ?? "").toLowerCase();
  const children = q.get("children") === "true";
  const limit = Math.min(500, Math.max(1, Number(q.get("limit") ?? 50) || 50));
  const offset = Math.max(0, Number(q.get("offset") ?? 0) || 0);

  let list = sessions.map(summarize);
  if (agent) list = list.filter((s) => s.agent === agent);
  if (device) list = list.filter((s) => s.devices.some((d) => d.id === device));
  if (!children) list = list.filter((s) => s.parent === null);
  if (text) {
    list = list.filter((s) => [s.title ?? "", s.cwd ?? "", s.id].some((f) => f.toLowerCase().includes(text)));
  }
  list.sort((a, b) => b.last_ts_ms - a.last_ts_ms);
  json(res, 200, { total: list.length, items: list.slice(offset, offset + limit) });
}

function sessionDetail(s, url, res) {
  const wanted = url.searchParams.get("version");
  if (wanted && !s.versions.some((v) => v.id === wanted)) {
    return apiError(res, 404, "version_not_found", "没有这个版本");
  }
  const selected = wanted ?? latestHead(s).id;
  const heads = new Set(headsOf(s).map((v) => v.id));
  json(res, 200, {
    summary: summarize(s),
    selected_version: selected,
    versions: s.versions.map((v) => ({
      id: v.id,
      parent: v.parent,
      device: deviceRef(v.device),
      created_ms: v.created_ms,
      rewrite: v.rewrite,
      bytes: v.bytes,
      is_head: heads.has(v.id),
    })),
    files: filesOf(s, selected),
    children: sessions.filter((c) => c.parent === s.ref).map(summarize),
    fork_from: s.fork_from,
    digests: s.digests,
  });
}

function sessionLines(s, url, res) {
  const q = url.searchParams;
  const versionId = q.get("version") ?? latestHead(s).id;
  const version = s.versions.find((v) => v.id === versionId);
  if (!version) return apiError(res, 404, "version_not_found", "没有这个版本");
  const files = filesOf(s, versionId);
  const filePath = q.get("file") ?? s.mainFile;
  const file = files.find((f) => f.path === filePath);
  if (!file) return apiError(res, 404, "file_not_found", "这个版本里没有这个文件");
  if (file.role === "asset") return apiError(res, 400, "not_text", "这个文件不是文本记录");

  const kinds = new Set((q.get("kinds") ?? "user,assistant").split(",").filter(Boolean));
  const from = Math.max(1, Number(q.get("from") ?? 1) || 1);
  const total = file.lines ?? 0;
  const to = Math.min(total, Number(q.get("to") ?? total) || total);
  const limit = Math.min(500, Math.max(1, Number(q.get("limit") ?? 100) || 100));
  const offset = Math.max(0, Number(q.get("offset") ?? 0) || 0);
  const maxChars = Math.max(1, Number(q.get("max_field_chars") ?? 4000) || 4000);
  const raw = q.get("raw") === "true";

  const matched = [];
  for (let n = from; n <= to; n++) {
    const item = lineFor(s, versionId, filePath, n, s.first_ts_ms);
    if (kinds.has(item.kind)) matched.push(item);
  }
  const page = matched.slice(offset, offset + limit).map((item) => {
    const cut = item.text.length > maxChars;
    return {
      ...item,
      text: cut ? item.text.slice(0, maxChars) : item.text,
      text_truncated: cut,
      raw: raw ? JSON.stringify({ type: item.raw_type, text: item.text }) : null,
    };
  });
  json(res, 200, {
    file: filePath,
    version: versionId,
    total_lines: total,
    matched: matched.length,
    truncated: offset + limit < matched.length,
    items: page,
  });
}

function csvCell(value) {
  const text = value === null || value === undefined ? "" : String(value);
  return /[",\n\r]/.test(text) ? `"${text.replaceAll('"', '""')}"` : text;
}

function exportRows(url) {
  const q = url.searchParams;
  const agent = q.get("agent");
  const device = q.get("device");
  const refs = q.getAll("session");
  const since = q.get("since_ms") ? Number(q.get("since_ms")) : null;
  const until = q.get("until_ms") ? Number(q.get("until_ms")) : null;
  const rows = [];
  for (const s of sessions) {
    if (agent && s.agent !== agent) continue;
    if (refs.length && !refs.includes(s.ref)) continue;
    const sum = summarize(s);
    if (device && !sum.devices.some((d) => d.id === device)) continue;
    for (let turn = 1; turn <= 3; turn++) {
      const at = s.first_ts_ms + turn * 10 * MIN;
      if (since !== null && at < since) continue;
      if (until !== null && at > until) continue;
      rows.push({
        session: s.ref,
        agent: s.agent,
        device: sum.devices[0].id,
        cwd: s.cwd,
        turn,
        at,
        user: USER_TEXTS[(turn + s.id.length) % USER_TEXTS.length],
        prev_reply: turn === 1 ? null : ASSISTANT_TEXTS[turn % ASSISTANT_TEXTS.length],
        gap_since_reply: turn === 1 ? null : 90_000 * turn,
        gap_since_user: turn === 1 ? null : 600_000,
        model: "synthetic-model-1",
        tokens: { input: 1200 * turn, output: 300 * turn, cache_read: 800, cache_write: 0 },
        requests: turn,
        subagents: 0,
        reasoning_chars: 120 * turn,
        agent_version: s.agent_version,
        parse: "ok",
      });
    }
  }
  return rows;
}

function exportDownload(url, res) {
  const format = url.searchParams.get("format") === "csv" ? "csv" : "jsonl";
  const all = EXPORT_FIELDS.map((f) => f.name);
  const fields = url.searchParams.get("fields") ? url.searchParams.get("fields").split(",").filter((f) => all.includes(f)) : all;
  res.writeHead(200, {
    "Content-Type": format === "csv" ? "text/csv; charset=utf-8" : "application/x-ndjson; charset=utf-8",
    "Content-Disposition": `attachment; filename="nekorisu-export.${format}"`,
  });
  const rows = exportRows(url);
  if (format === "jsonl") {
    for (const row of rows) res.write(`${JSON.stringify(Object.fromEntries(fields.map((f) => [f, row[f]])))}\n`);
  } else {
    const columns = fields.flatMap((f) =>
      f === "tokens" ? ["tokens_input", "tokens_output", "tokens_cache_read", "tokens_cache_write"] : [f],
    );
    res.write(`${columns.join(",")}\n`);
    for (const row of rows) {
      const cells = fields.flatMap((f) =>
        f === "tokens" ? [row.tokens.input, row.tokens.output, row.tokens.cache_read, row.tokens.cache_write] : [row[f]],
      );
      res.write(`${cells.map(csvCell).join(",")}\n`);
    }
  }
  res.end();
}

async function handle(req, res) {
  const url = new URL(req.url ?? "/", `http://${req.headers.host ?? "localhost"}`);
  const path = url.pathname.replace(/\/+$/, "") || "/";
  const method = req.method ?? "GET";

  if (path === "/healthz") {
    res.writeHead(200, { "Content-Type": "text/plain" });
    return res.end("ok");
  }
  if (path === "/mcp") return apiError(res, 404, "not_found", "MCP 没有开启");
  if (!path.startsWith("/api/")) {
    res.writeHead(404, { "Content-Type": "text/plain; charset=utf-8" });
    return res.end("mock server: only /api, /healthz and /mcp are served; use `pnpm dev` for the panel");
  }

  // ---- public: auth
  if (path === "/api/auth/state" && method === "GET") {
    return json(res, 200, { initialized: password !== null, authenticated: password !== null && isAuthed(req) });
  }
  if (path === "/api/auth/setup" && method === "POST") {
    if (password !== null) return apiError(res, 409, "already_initialized", "已经初始化过了");
    if (rateLimited(res)) return;
    const body = await readBody(req);
    if (!body || typeof body.password !== "string" || typeof body.setup_code !== "string") {
      return apiError(res, 400, "bad_request", "请求体不对");
    }
    if (body.setup_code !== SETUP_CODE) {
      recordFailure();
      return apiError(res, 403, "bad_setup_code", "设置码不对");
    }
    if ([...body.password].length < 10) return apiError(res, 400, "weak_password", "密码至少 10 个字符");
    password = body.password;
    failures = 0;
    return startSession(res);
  }
  if (path === "/api/auth/login" && method === "POST") {
    if (rateLimited(res)) return;
    const body = await readBody(req);
    if (password === null || !body || body.password !== password) {
      recordFailure();
      if (rateLimited(res)) return;
      return apiError(res, 401, "bad_credentials", "密码不对");
    }
    failures = 0;
    return startSession(res);
  }
  if (path === "/api/auth/logout" && method === "POST") {
    authed.delete(parseCookies(req).nk_session);
    res.writeHead(204, { "Set-Cookie": "nk_session=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0" });
    return res.end();
  }

  // ---- everything else needs a session
  if (password === null || !isAuthed(req)) return apiError(res, 401, "unauthorized", "没有登录");

  if (path === "/api/overview" && method === "GET") {
    const roots = sessions.filter((s) => s.parent === null);
    const config = buildConfig().current;
    const storageDown = process.env.MOCK_STORAGE_ERROR === "1";
    return json(res, 200, {
      version: "0.1.0-mock",
      commit: "mock000",
      source_url: "https://github.com/NEKO-CwC/nekorisu-server",
      storage: {
        description: "webdav https://dav.example/nekorisu",
        ok: !storageDown,
        error: storageDown ? "PROPFIND 返回 503" : null,
      },
      counts: {
        sessions: sessions.length,
        root_sessions: roots.length,
        devices: 3,
        bytes: sessions.reduce((n, s) => n + latestHead(s).bytes, 0),
        by_agent: {
          "claude-code": sessions.filter((s) => s.agent === "claude-code").length,
          codex: sessions.filter((s) => s.agent === "codex").length,
        },
      },
      last_upload_ms: Math.max(...sessions.flatMap((s) => s.versions.map((v) => v.created_ms))),
      config: config ? { serial: config.serial, issued_at_ms: config.issued_at_ms, etag: config.etag } : null,
      index: { refreshed_at_ms: refreshedAt, skipped_files: 1 },
    });
  }
  if (path === "/api/refresh" && method === "POST") {
    refreshedAt = Date.now();
    return json(res, 200, { refreshed_at_ms: refreshedAt, sessions: sessions.length });
  }
  if (path === "/api/sessions" && method === "GET") return listSessions(url, res);

  const m = path.match(/^\/api\/sessions\/([^/]+)\/([^/]+)(\/lines)?$/);
  if (m && method === "GET") {
    const s = byRef.get(`${decodeURIComponent(m[1])}:${decodeURIComponent(m[2])}`);
    if (!s) return apiError(res, 404, "session_not_found", "找不到这个会话");
    return m[3] ? sessionLines(s, url, res) : sessionDetail(s, url, res);
  }

  if (path === "/api/devices" && method === "GET") return json(res, 200, { items: buildDevices() });
  if (path === "/api/config" && method === "GET") return json(res, 200, buildConfig());
  if (path === "/api/storage" && method === "GET") {
    const down = process.env.MOCK_STORAGE_ERROR === "1";
    const range = process.env.MOCK_RANGE;
    return json(res, 200, {
      description: "webdav https://dav.example/nekorisu",
      ok: !down,
      error: down ? "PROPFIND 返回 503" : null,
      latency_ms: down ? null : 42,
      range_read: range === "null" ? null : range === "false" ? false : true,
      layout: down ? { found: false, entries: [] } : { found: true, entries: ["devices", "logs", "sessions", "digests"] },
    });
  }
  if (path === "/api/activity" && method === "GET") {
    const limit = Math.min(1000, Math.max(1, Number(url.searchParams.get("limit") ?? 100) || 100));
    return json(res, 200, { items: buildActivity().slice(0, limit), skipped_lines: 2 });
  }
  if (path === "/api/export/fields" && method === "GET") {
    return json(res, 200, { formats: ["jsonl", "csv"], fields: EXPORT_FIELDS });
  }
  if (path === "/api/export" && method === "GET") return exportDownload(url, res);

  return apiError(res, 404, "not_found", "没有这个接口");
}

createServer((req, res) => {
  handle(req, res).catch((err) => {
    console.error(err);
    if (!res.headersSent) apiError(res, 500, "internal", "mock server error");
    else res.end();
  });
}).listen(PORT, "127.0.0.1", () => {
  console.log(`mock API on http://127.0.0.1:${PORT}  (setup code ${SETUP_CODE}${password ? ", already initialized, password mock-password-1" : ""})`);
});
