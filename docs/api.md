# 管理接口（草案）

> 2026-11-23。panel 用的 HTTP 接口。第一个版本**只读**，只有登录和初始化会写。

## 约定

- 路径都在 `/api` 下，JSON，字段名 `snake_case`，时间都是毫秒时间戳（字段名以 `_ms` 结尾），拿不到的值是 `null`。
- 认证：`nk_session` cookie（`HttpOnly`、`SameSite=Strict`）。没登录返回 `401`；公开的只有 `/api/auth/state`、`/api/auth/setup`、`/api/auth/login`、`/healthz`。`/mcp` 用 OAuth 的访问令牌，见最后的 [MCP](#mcp) 一节。
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
一分钟内最多 5 次错误的密码，之后要等；对了的那次不算。MCP 授权页上输错的密码也算在这里面。设置码另外计数，规则一样。

**按来源计数**：IPv4 按单个地址，IPv6 按 /64，一个来源输错不会把别人挡在外面。所有来源加起来一分钟最多 50 次错误，超了谁都要等到这一分钟过去（已经登录的 panel 照常用）。来源默认是连到服务端的那个地址；放在反向代理后面时，设 `NEKORISU_TRUSTED_PROXIES`（逗号分隔的网段，如 `10.42.0.0/16`），对端在这些网段里才相信它的 `X-Real-IP`，没有或不合法就取 `X-Forwarded-For` 从右数第一个不是可信代理的地址，再不行就算对端自己。不设的话，代理后面的所有人都算同一个来源。`::ffff:` 写法的 IPv4 当作 IPv4。网段写错了服务端起不来。

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

## 授权的应用

用 OAuth 登录过 MCP 的 Agent。

`GET /api/oauth/apps`：
```json
{
  "mcp_url": "https://nekorisu.example.com/mcp",
  "items": [ {
    "id": "3f2a…", "client_name": "Claude Code",
    "client_id": "https://agent.example/oauth/client.json", "metadata_document": true,
    "redirect_host": "localhost:51234", "loopback": true, "scope": "nekorisu:read",
    "created_ms": 1794700800000, "last_used_ms": 1794700900000
  } ]
}
```
- `mcp_url` 是 `null` 表示 MCP 没开（没设 `NEKORISU_PUBLIC_URL`）。
- 每次批准是一条；只列还能用的（刷新令牌没过期、没撤销）。`metadata_document` 为 true 时名字取自 `client_id` 那个网址上的说明文档，为 false 时是应用注册时自己填的。`last_used_ms` 最多每分钟更新一次。不返回任何令牌。

`POST /api/oauth/apps/{id}/revoke`：`204`，它的访问令牌和刷新令牌立刻失效；没有这一条 `404 app_not_found`。

## MCP

`POST /mcp`：Streamable HTTP，JSON-RPC 2.0，只有 `initialize`、`ping`、`tools/list`、`tools/call`。工具见 `tools/list`。没设 `NEKORISU_PUBLIC_URL` 时 MCP 关着，下面的路径都返回 `404`。

**认证**是 OAuth 2.1，照 MCP 的授权规范：`Authorization: Bearer <访问令牌>`，令牌只能是这台服务端为 `<公网地址>/mcp` 签发的。没有令牌或令牌不对（过期、撤销、发给别的地址的）返回 `401`：
```
WWW-Authenticate: Bearer resource_metadata="https://nekorisu.example.com/.well-known/oauth-protected-resource/mcp", scope="nekorisu:read"
```
带了令牌但不对时，后面再加 `, error="invalid_token"`。Agent 照这个头自己去登录，人只要在浏览器里批准：

```bash
claude mcp add --transport http -s user nekorisu-server https://nekorisu.example.com/mcp
claude mcp login nekorisu-server

codex mcp add nekorisu-server --url https://nekorisu.example.com/mcp
codex mcp login nekorisu-server
```

不要在 Agent 的配置里写死 `Authorization` 头：写了它就不走 OAuth。

| 路径 | 做什么 |
|---|---|
| `GET /.well-known/oauth-protected-resource/mcp`（和不带 `/mcp` 的） | 受保护资源元数据（RFC 9728）：`resource`、`authorization_servers`（就是这台服务端）、`scopes_supported`、`bearer_methods_supported` |
| `GET /.well-known/oauth-authorization-server` | 授权服务器元数据（RFC 8414）：只有 `code`、PKCE 只认 `S256`、只有公开客户端（`none`）、`client_id_metadata_document_supported`、`authorization_response_iss_parameter_supported` |
| `POST /oauth/register` | 动态注册（RFC 7591），只收公开客户端：`redirect_uris`（1 到 10 个）、`client_name`。返回 `201` 和 `client_id`。同一个来源（同上）10 分钟内最多注册 10 个，所有来源加起来 100 个，超了返回 `429` 和 `Retry-After`；没人批准的注册一天后清掉，最多留 500 个 |
| `GET /oauth/authorize` | 检查请求，给人看授权页（见下） |
| `POST /oauth/authorize` | 授权页提交：批准要管理员密码；成功 `302` 回到 `redirect_uri`，带 `code`、`state`、`iss` |
| `POST /oauth/token` | `authorization_code` 和 `refresh_token` 两种 |

- **范围**只有一个：`nekorisu:read`（读会话、设备、导出）。请求里带 `offline_access` 也行，不影响什么；别的范围返回 `invalid_scope`。
- **客户端**：`client_id` 是 https 网址时，服务端去取那个网址上的说明文档（Client ID Metadata Document），要求里面的 `client_id` 就是这个网址、有 `client_name` 和 `redirect_uris`，缓存 10 分钟；只连公网地址，5 秒超时，最大 64 KiB。198.18.0.0/15 不算公网：fake-IP 的 DNS 代理（sing-box、Clash 这类）把所有外网域名都解析到这个段，看不出请求真正去哪。服务端所在机器的 DNS 是这种代理时，设 `NEKORISU_DNS_OVER_HTTPS`（如 `https://1.1.1.1/dns-query`，主机必须写 IP），说明文档的域名（每次跳转都一样）改走 DNS over HTTPS（RFC 8484）解析；它出错就取不到文档。没设时用系统的 DNS，解析出来全是 fake-IP 的地址时日志里会这样提示。别的 `client_id` 要先在 `/oauth/register` 注册过。`NEKORISU_OAUTH_CLIENT_METADATA_DOCUMENTS=false` 关掉网址形式，Agent 会改用注册。
- **返回地址**要和登记的完全一样；只有本机的 `http://localhost`、`http://127.0.0.1`、`http://[::1]` 端口可以不同（RFC 8252）。其他的只能是 https。`client_id` 或返回地址不对时不跳转，直接显示错误页；之后的错误（`invalid_request`、`invalid_target`、`invalid_scope`、`unsupported_response_type`、`access_denied`）跳回去，带 `state` 和 `iss`。
- **授权请求**：`response_type=code`、`client_id`、`redirect_uri`、`code_challenge` 加 `code_challenge_method=S256`（必须）、`state`、`resource`（不给就是 `<公网地址>/mcp`，给了必须是它）、`scope`。
- **授权页**：中文，没有脚本；写着哪个应用、授权后回到哪里（本机的就说是这台电脑上的程序）、能做什么，一个管理员密码框，批准、拒绝。表单里带一个一次性的请求 ID 和 CSRF 令牌，10 分钟有效。还没设管理员密码时只提示先去 panel 设置。页面不能被嵌进框架（`X-Frame-Options: DENY`、`frame-ancestors 'none'`），`Cache-Control: no-store`。
- **令牌**：授权码 5 分钟、只能用一次（第二次用会撤销它换来的令牌），和 `client_id`、`redirect_uri`、`code_challenge`、`resource`、`scope` 绑定；访问令牌 1 小时；刷新令牌 30 天，每次刷新都换新的，旧的再出现就撤销整个授权。都是随机的不透明字符串，库里只存 SHA-256。错误按 RFC 6749：`{"error": "invalid_grant", "error_description": "..."}`。

## 其他

- `GET /healthz`：`200 ok`，不认证。
- 其他路径：`/api` 以外的都返回前端的 `index.html`（单页应用的路由）。
