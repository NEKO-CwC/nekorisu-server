# 管理接口（草案）

> 2026-11-23。panel 用的 HTTP 接口。第一个版本**只读**，只有登录和初始化会写。

## 约定

- 路径都在 `/api` 下，JSON，字段名 `snake_case`，时间都是毫秒时间戳（字段名以 `_ms` 结尾），拿不到的值是 `null`。
- 认证：`nk_session` cookie（`HttpOnly`、`SameSite=Strict`）。没登录返回 `401`；公开的只有 `/api/auth/state`、`/api/auth/setup`、`/api/auth/login`、`/healthz`。
- 错误：HTTP 状态码加 `{"error": {"code": "...", "message": "..."}}`。`code` 是固定的短词，前端按它分支；`message` 给人看。
- 会话的写法：`<agent>:<会话ID>`，`agent` 是 `claude-code` 或 `codex`。
- 列表都是 `{"items": [...]}`，有分页的再加 `total`。
- 接口不返回存储密码、令牌、签名私钥。

## 认证

`GET /api/auth/state` →
```json
{ "initialized": false, "authenticated": false }
```

`POST /api/auth/setup`，`{"password": "...", "setup_code": "..."}`：只在 `initialized` 为 false 时可用。成功返回 `204`，同时登录。
错误：`409 already_initialized`、`403 bad_setup_code`、`400 weak_password`（少于 10 个字符）、`429 too_many_attempts`。

`POST /api/auth/login`，`{"password": "..."}`：`204` 加 cookie；`401 bad_credentials`；`429 too_many_attempts`（`error` 里多一个 `retry_after_secs`）。

`POST /api/auth/logout`：`204`。

## 概览

`GET /api/overview`：
```json
{
  "version": "0.1.0",
  "commit": "abc1234",
  "source_url": "https://github.com/NEKO-CwC/nekorisu-server",
  "storage": { "description": "webdav https://dav.example/nekorisu", "ok": true, "error": null },
  "counts": { "sessions": 12, "root_sessions": 10, "devices": 2, "bytes": 123456789,
              "by_agent": { "claude-code": 8, "codex": 4 } },
  "last_upload_ms": 1794700800000,
  "config": { "serial": 7, "issued_at_ms": 1794700800000, "etag": "..." },
  "index": { "refreshed_at_ms": 1794700800000, "skipped_files": 0 }
}
```
`config` 没有发布过就是 `null`。

`POST /api/refresh`：重新列一遍存储，更新索引（不改存储里的任何东西）。返回 `{"refreshed_at_ms": 1, "sessions": 12}`。

## 会话

`GET /api/sessions?agent=&device=&q=&limit=50&offset=0&children=false`：
- `device` 是设备 ID；`q` 在标题、目录、会话 ID 里搜；`children=false` 时不列子会话（Codex 的子线程）。
- 按最后活动时间倒排。

```json
{ "total": 120, "items": [ SessionSummary ] }
```

`SessionSummary`：
```json
{
  "ref": "claude-code:73ffc2c4",
  "agent": "claude-code",
  "id": "73ffc2c4",
  "title": "…",
  "cwd": "/Users/me/project",
  "agent_version": "2.1.322",
  "devices": [ { "id": "d1", "name": "mac-mini" } ],
  "first_ts_ms": 1794700000000,
  "last_ts_ms": 1794700800000,
  "uploaded_ms": 1794700900000,
  "bytes": 123456,
  "files": 3,
  "versions": 2,
  "heads": 1,
  "parent": null,
  "children": 0
}
```
- `versions` 是 manifest 的个数；`heads` 大于 1 就是有分叉。`bytes` 和 `files` 是最新版本的。
- `devices` 是上传过这个会话的设备。

`GET /api/sessions/{agent}/{id}?version=`：
```json
{
  "summary": SessionSummary,
  "selected_version": "d1-3",
  "versions": [
    { "id": "d1-3", "parent": "d1-2", "device": { "id": "d1", "name": "mac-mini" },
      "created_ms": 1794700900000, "rewrite": false, "bytes": 123456, "is_head": true }
  ],
  "files": [
    { "path": "projects/-Users-me-project/73ffc2c4.jsonl", "role": "transcript",
      "bytes": 120000, "lines": 345, "segments": 2 }
  ],
  "children": [ SessionSummary ],
  "fork_from": { "session": "claude-code:aaaa", "byte": 5000 },
  "digests": [ { "id": "dg1", "updated_ms": 1794700900000 } ]
}
```
`version` 不给就是最新的头；有分叉时是上传时间最晚的那个。`fork_from` 没有就是 `null`。`lines` 不知道时是 `null`。`role` 是 `transcript`、`subagent`、`tool_result`、`file_history`、`asset`、`other` 之一。找不到会话：`404 session_not_found`。

`GET /api/sessions/{agent}/{id}/lines?version=&file=&from=&to=&kinds=user,assistant&limit=100&offset=0&max_field_chars=4000&raw=false`：
- `file` 默认是主记录；`from`、`to` 是行号（从 1 数，含两端）；`kinds` 是 `user`、`assistant`、`reasoning`、`tool`、`meta` 的逗号分隔；`offset` 对匹配上的行算；`limit` 最大 500。
- `text` 是从那一行里取出来的可读文本（用户原话、Agent 的文字回复、思考；工具和其他种类给一行摘要）。`raw=true` 时再带上原始的一行。

```json
{
  "file": "projects/-Users-me-project/73ffc2c4.jsonl",
  "version": "d1-3",
  "total_lines": 345,
  "matched": 40,
  "truncated": false,
  "items": [
    { "line": 12, "kind": "user", "raw_type": "user", "ts_ms": 1794700000000,
      "text": "…", "text_truncated": false, "raw_len": 345, "raw": null }
  ]
}
```
`truncated` 为 true 表示匹配的行比 `limit` 多。

## 设备

`GET /api/devices`：
```json
{ "items": [ {
  "id": "d1", "name": "mac-mini", "first_seen_ms": 1794600000000,
  "sessions": 8, "bytes": 123456, "last_upload_ms": 1794700900000,
  "state": {
    "verified": false,
    "received_ms": 1794700900000,
    "client_version": "0.1.0",
    "applied_serial": 6,
    "applied_etag": "...",
    "behind": 1,
    "exec_allowed": false,
    "local_values": [ { "key": "local.copy_form", "value_json": "\"full\"", "locked": true } ],
    "requests": [ { "id": "f-001", "state": "pending" } ],
    "last_failure": { "serial": 7, "reason": "bad_signature" }
  }
} ] }
```
- 只在存储里出现的设备，`state` 是 `null`；只在上报里出现的设备，`name` 和 `first_seen_ms` 是 `null`。
- `verified` 现在永远是 false：设备 ID 是设备自己报的。
- `behind` 是当前账号版配置的序号减去 `applied_serial`，不会小于 0；没有配置时是 `null`。
- `requests[].state`：`pending`、`accepted`、`rejected`。`last_failure.reason`：`bad_signature`、`unsupported_schema`、`resource_unavailable`、`resource_hash_mismatch`、`other`；没有是 `null`。

## 配置

`GET /api/config`：
```json
{
  "current": {
    "serial": 7, "etag": "...", "issued_at_ms": 1794700800000, "key_id": "0123456789abcdef",
    "payload": { "schema": "1.0", "serial": 7 },
    "resources": [ { "name": "excerpt-default", "version": "2026-11-23.1", "sha256": "...",
                     "executable": false, "size": 120 } ]
  },
  "public": { "enabled": false, "serial": 6 },
  "signing": { "key_id": "0123456789abcdef", "public_key": "base64...",
               "connect_command": "nekorisu source add home https://nekorisu.example --key base64..." },
  "history": [ { "serial": 7, "view": "account", "issued_at_ms": 1794700800000, "etag": "..." } ],
  "requests": [ {
    "kind": "force", "id": "f-001", "action": null,
    "changes": [ { "key": "local.copy_form", "value_json": "\"full\"" } ],
    "devices": [ { "id": "d1", "name": "mac-mini", "state": "pending" } ]
  } ],
  "source": { "dir": "/config", "checked_ms": 1794700800000, "last_error": null },
  "device_token_set": true
}
```
- `current` 是账号版；没发布过是 `null`。`payload` 是签名的那份 JSON，原样的内容。
- `requests[].kind` 是 `force` 或 `exec`；`exec` 的 `action` 是 `allow` 或 `revoke`，`force` 的是 `null`；`devices` 是这条请求针对的设备，`state` 多一个 `unknown`（设备还没上报过）。
- `history` 最多 50 条，新的在前。`last_error` 是配置文件出错时的原因，正常是 `null`。

## 存储

`GET /api/storage`：
```json
{
  "description": "webdav https://dav.example/nekorisu",
  "ok": true,
  "error": null,
  "latency_ms": 42,
  "range_read": true,
  "layout": { "found": true, "entries": ["devices", "logs", "sessions", "digests"] }
}
```
`range_read` 是 `null` 表示还没测过；`false` 表示服务器不认 `Range`，按行读会退化成整份下载。

## 活动

`GET /api/activity?limit=100`：合并所有设备的操作日志，新的在前。
```json
{ "items": [ { "at_ms": 1794700900000, "seq": 12, "device": { "id": "d1", "name": "mac-mini" },
               "op": "archive", "sessions": ["claude-code:73ffc2c4"], "tags": [], "note": null, "bytes": 123456 } ],
  "skipped_lines": 0 }
```

## 导出

`GET /api/export/fields`：
```json
{ "formats": ["jsonl", "csv"],
  "fields": [ { "name": "user", "description": "用户原话，原样" } ] }
```

`GET /api/export?format=jsonl&fields=session,user,tokens&agent=&device=&session=&since_ms=&until_ms=`：
- 响应是下载：`Content-Disposition: attachment`，流式输出，每行对应一条用户原话；`fields` 不给就是全部；`session` 可以重复。

## MCP

`POST /mcp`：Streamable HTTP，JSON-RPC 2.0，只有 `initialize`、`ping`、`tools/list`、`tools/call`。认证：`Authorization: Bearer <NEKORISU_MCP_TOKEN>`；没设令牌，这个路径返回 `404`。工具见 `tools/list`。

## 其他

- `GET /healthz`：`200 ok`，不认证。
- 其他路径：`/api` 以外的都返回前端的 `index.html`（单页应用的路由）。
