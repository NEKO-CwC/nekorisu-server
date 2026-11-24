# nekorisu panel

只读管理界面：Vite + React + TypeScript + React Router + TanStack Query，接口见 `../docs/api.md`。

- 安装：`pnpm install`；检查类型：`pnpm typecheck`
- 开发：`pnpm dev`（端口 5173，`/api`、`/healthz`、`/mcp`、`/oauth`、`/.well-known` 代理到 `127.0.0.1:8080`，可接真的服务端）
- 模拟后端：另开终端 `pnpm mock`（端口 8080，设置码 `MOCKCODE`，数据全是合成的）
- mock 开关：`MOCK_INITIALIZED=1`（密码 `mock-password-1`）、`MOCK_CONFIG_ERROR=1`、`MOCK_CONFIG_NULL=1`、`MOCK_RANGE=false|null`、`MOCK_STORAGE_ERROR=1`、`MOCK_MCP_OFF=1`
- 构建：`pnpm build`，输出 `dist/`，由 Rust 二进制嵌入
- 界面文字都在 `src/messages.ts`；样式只有 `src/styles.css`，可以整个换掉
