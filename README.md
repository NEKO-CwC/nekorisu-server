# nekorisu-server

[nekorisu](https://github.com/NEKO-CwC/nekorisu) 的服务端：一个只读的 panel，让你在一个地方看各台设备上传的会话；
给设备下发签名的配置；给 Agent 一个 MCP 入口；按字段导出每轮的数据。存储是 provider，现在只有 WebDAV。

- **panel**：会话、设备、配置、存储、活动、导出。这个版本只展示，不改任何东西。
- **设备协议**：`nekorisu.cloud.v1`（gRPC），配置整份签名；设备上报自己的配置状态。
- **MCP**：`/mcp`，只读，让 Agent 读各设备的会话，配套 skill 在 [`skills/nekorisu-server/`](skills/nekorisu-server/SKILL.md)。
  Agent 用 OAuth 登录：浏览器里输入 panel 的管理员密码批准，不用复制令牌。
- **导出**：JSONL 或 CSV，全量或自选字段，现算现给，不往存储里写任何东西。

单用户。接口见 [`docs/api.md`](docs/api.md)。
还没有发布版本。

## 用容器跑

```bash
docker build --build-context nekorisu=../nekorisu -t nekorisu-server .
```

然后照 [`docker-compose.example.yml`](docker-compose.example.yml) 配环境变量（全部用环境变量设置，见该文件和 `deploy/k8s/README.md`）。
`NEKORISU_WEBDAV_URL` 和客户端填同一个存储地址：归档在它下面的 `nekorisu/v1/`，所以填 `nekorisu/` 的上一层。
下发的配置放在 `/config`，样例在 [`config-example/`](config-example/)。
第一次启动，日志里会打印一个设置码；打开 panel，用它设置管理员密码。
`NEKORISU_PUBLIC_URL` 填服务端对外的地址（例如 `https://nekorisu.example.com`，端口不是 443 就带上），MCP 才会开。
放在反向代理后面时，把代理的网段写进 `NEKORISU_TRUSTED_PROXIES`，输错密码才按真实的来源计数，不然一个人输错就挡住所有人（见 [`docs/api.md`](docs/api.md) 的认证一节）。
机器的 DNS 是 fake-IP 代理（sing-box、Clash 这类）时，设 `NEKORISU_DNS_OVER_HTTPS`（例如 `https://1.1.1.1/dns-query`），不然 Claude Code 这类用网址当 `client_id` 的 Agent 登录不了。

## 接 Agent

在 Agent 所在的电脑上运行（`<host>` 换成你的地址）：

```bash
# Claude Code
claude mcp add --transport http -s user nekorisu-server https://<host>/mcp
claude mcp login nekorisu-server

# Codex
codex mcp add nekorisu-server --url https://<host>/mcp
codex mcp login nekorisu-server
```

`login` 会打开浏览器，页面上写着是哪个应用、授权后回到哪里、能做什么；输入 panel 的管理员密码批准，Agent 自动拿到令牌。
只读，令牌一小时过期，Agent 自己续期；panel 的“应用”页能看到谁登录过，随时撤销。
不要在 Agent 的配置里手写 `Authorization` 头：写了它就不走 OAuth。

## 许可

[AGPL-3.0](LICENSE)。
