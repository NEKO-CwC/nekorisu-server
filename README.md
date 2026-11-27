# nekorisu-server

[nekorisu](https://github.com/NEKO-CwC/nekorisu) 的服务端：一个 panel，让你在一个地方看各台设备上传的会话；
给设备下发签名的配置；给 Agent 一个 MCP 入口；按字段导出每轮的数据。存储是 provider，现在只有 WebDAV。

- **panel**：会话、设备、配置、路径映射、存储、活动、导出。大多只展示；路径映射可以在这里改，改完马上下发。
- **连接说明**：`/connect`，不用登录。写着这台服务端开了什么、还缺哪些设置、电脑和 Agent 怎么接，还有术语解释；给 Agent 一个地址，它 `curl` 一下就知道怎么做。上面没有密码和令牌，存储地址默认也不放（登录后才给）。
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
`NEKORISU_WEBDAV_URL` 和客户端填同一个存储地址：归档在它下面的 `nekorisu/v1/`，所以填 `nekorisu/` 的上一层。服务端经内部地址读存储时（比如集群里的服务名），把电脑访问同一个文件夹用的地址写进 `NEKORISU_DEVICE_WEBDAV_URL`，给电脑看的就是它。
下发的配置放在 `/config`，样例在 [`config-example/`](config-example/)。
第一次启动，日志里会打印一个设置码；打开 panel，用它设置管理员密码。
`NEKORISU_PUBLIC_URL` 填服务端对外的地址（例如 `https://nekorisu.example.com`，端口不是 443 就带上），MCP 才会开。
设备协议（gRPC）不和 panel 在同一个地址时（比如 compose 样例里单独的 7070 端口），把设备连的地址写进 `NEKORISU_DEVICE_URL`。

**启动以后先打开 `https://<你的地址>/connect`**（还没设 `NEKORISU_PUBLIC_URL` 时是 `http://127.0.0.1:8080/connect`）。它列出还缺哪些设置、各有什么后果、怎么补；都补齐了，它就是给电脑和 Agent 的连接说明。不想公开这一页，设 `NEKORISU_CONNECT_PAGE=false`；服务端只在内网能访问时，可以设 `NEKORISU_CONNECT_SHOW_STORAGE=true`，把存储地址和用户名也写上去。
放在反向代理后面时，把代理的网段写进 `NEKORISU_TRUSTED_PROXIES`，输错密码才按真实的来源计数，不然一个人输错就挡住所有人（见 [`docs/api.md`](docs/api.md) 的认证一节）。
机器的 DNS 是 fake-IP 代理（sing-box、Clash 这类）时，设 `NEKORISU_DNS_OVER_HTTPS`（例如 `https://1.1.1.1/dns-query`），不然 Claude Code 这类用网址当 `client_id` 的 Agent 登录不了。

## 接 Agent

最省事的办法：把 `https://<host>/connect` 发给 Agent，让它照着做。它会用到客户端的[安装手册](https://github.com/NEKO-CwC/nekorisu/blob/main/docs/agent-install.md)。

也可以自己在 Agent 所在的电脑上运行（`<host>` 换成你的地址）：

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
登录后 Agent 可以调用 `setup_info`，拿到电脑上要填的存储地址和用户名（没有密码）。
不要在 Agent 的配置里手写 `Authorization` 头：写了它就不走 OAuth。

## 许可

[AGPL-3.0](LICENSE)。
