# nekorisu-server

[nekorisu](https://github.com/NEKO-CwC/nekorisu) 的服务端：一个只读的 panel，让你在一个地方看各台设备上传的会话；
给设备下发签名的配置；给 Agent 一个 MCP 入口；按字段导出每轮的数据。存储是 provider，现在只有 WebDAV。

- **panel**：会话、设备、配置、存储、活动、导出。这个版本只展示，不改任何东西。
- **设备协议**：`nekorisu.cloud.v1`（gRPC），配置整份签名；设备上报自己的配置状态。
- **MCP**：`/mcp`，只读，让 Agent 读各设备的会话，配套 skill 在 [`skills/nekorisu-server/`](skills/nekorisu-server/SKILL.md)。
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

## 许可

[AGPL-3.0](LICENSE)。
