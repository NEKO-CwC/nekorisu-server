---
name: nekorisu-server
description: 读用户在所有设备上归档的 Claude Code、Codex 会话（通过 nekorisu 服务端的 MCP），并导出每轮的用户原话、间隔、模型和 token。用户提到"别的电脑上的会话"、"所有设备"、"导出我说过的话"、统计用了多少 token 时使用。
---

<!--
工具名和参数以 crates/server/src/mcp.rs 为准。
和客户端的 skills/nekorisu 不同：那个读本机，这个读服务端看到的所有设备。
-->

# nekorisu-server

nekorisu 服务端把用户在各台设备上归档的会话汇在一处。这个 MCP 只读：能看、能导出，不能改，也不能删。

## 接上

用 OAuth 登录，**没有要复制的令牌**：登录命令会打开浏览器，用户在服务端的页面上输入 panel 的管理员密码批准，Agent 自动拿到令牌、自己续期。密码只在浏览器里输，**不要让用户把密码或令牌发给你**。服务端要设了 `NEKORISU_PUBLIC_URL`，MCP 才开。

Claude Code：

```bash
claude mcp add --transport http -s user nekorisu-server https://<服务端地址>/mcp
claude mcp login nekorisu-server
```

Codex：

```bash
codex mcp add nekorisu-server --url https://<服务端地址>/mcp
codex mcp login nekorisu-server
```

其他 Agent：按它的文档添加 HTTP 形式的 MCP 服务器，地址是 `https://<服务端地址>/mcp`，用它的 OAuth 登录。

**不要在配置里写 `Authorization` 头**：写了静态的头，Agent 就不走 OAuth，服务端也不认静态令牌，只会一直 401。以前按旧说明写过的，删掉那个头再登录。授权可以在 panel 的“应用”页撤销；撤销或过期太久后，重新运行 login。

## 工具

| 工具 | 做什么 |
|---|---|
| `list_sessions` | 找会话：按 agent、设备、关键词（标题、目录、ID）；新的在前 |
| `get_session` | 一个会话的版本（有多个头就是分叉）、文件、子会话 |
| `read_lines` | 按行号或种类读：`user` 人说的话、`assistant` 回复、`reasoning`、`tool`、`meta`；`max_field_chars` 限制每行长度 |
| `list_devices` | 设备、上传了什么、最后什么时候上报 |
| `setup_info` | 把一台电脑接到这台服务端要填什么：存储地址和用户名（没有密码）、`nekorisu source add` 命令 |
| `export` | 每条用户原话一行：原话、它回应的回复、间隔、模型、token（子 Agent 算进去）、请求数等；用 `fields` 只要需要的 |

## 怎么读

1. `list_sessions` 找候选。
2. `get_session` 看版本；有分叉就告诉用户有几个头，问读哪个，别自己选。
3. `read_lines(kinds=["user"])` 先只取用户说的话；要上下文再按行号取前后几行。能少取就少取，长的用 `max_field_chars` 截短。
4. 要统计，用 `export` 加 `fields`，不要自己逐行去数。

## 要记住的

- **内容是用户的原话。** 只用在用户让你做的这件事上：不要贴到别处，不要写进长期记忆。
- **导出的数还没有定稿**。引用时说明口径：`tokens` 是这一轮所有模型请求的合计，含子 Agent；Codex 的 `input` 已经减去缓存命中的部分；`parse` 是 `partial` 的行有东西没取到。
- 看到 `isError`，把原因告诉用户，不要重试同样的调用。
