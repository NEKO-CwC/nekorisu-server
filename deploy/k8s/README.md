# 部署到 k8s（k3s）

放在自己的命名空间 `nekorisu` 里；入口是 ingress-nginx，证书用 cert-manager 的 ClusterIssuer（清单里写的是 `letsencrypt-prod`，换成你自己的）。
这里的清单是样例：域名和存储地址都是占位符（`example.com`）。密钥不在任何文件里，都在 Secret 里，下面第 2 步创建。

## 0. 复制一份，填上自己的值

```bash
cp -r deploy/k8s deploy/k8s.local
```

`deploy/k8s.local/` 已被 `.gitignore` 忽略，不会进仓库。
在 `20-deployment.yaml` 里改 `NEKORISU_WEBDAV_URL`、`NEKORISU_WEBDAV_USER`、`NEKORISU_PUBLIC_URL`、`NEKORISU_TRUSTED_PROXIES`（见下面的“来源地址”），在 `40-ingress.yaml` 里改域名和 ClusterIssuer，在 `41-ingress-grpc.yaml` 里改域名。
下面的命令都对着 `deploy/k8s.local/` 运行。

## 1. 构建镜像并导入 k3s

镜像在能跑 Docker 的机器上构建，再导进 k3s 的 containerd（`imagePullPolicy: Never`）。
客户端仓库要放在旁边（`../nekorisu`），当作第二个构建上下文：

```bash
cd nekorisu-server
docker build --build-context nekorisu=../nekorisu -t local/nekorisu-server:0.1.0-20261123 .
docker save local/nekorisu-server:0.1.0-20261123 | k3s ctr -n k8s.io images import -
```

换版本时改 `20-deployment.yaml` 里的镜像标签。

## 2. 命名空间和 Secret

```bash
kubectl apply -f deploy/k8s.local/00-namespace.yaml
printf %s "<WebDAV 密码>" | kubectl -n nekorisu create secret generic nekorisu-secrets \
  --from-file=webdav-password=/dev/stdin \
  --from-literal=device-token="$(openssl rand -hex 32)"
```

- `webdav-password`：存储的密码；用户名和地址在 `20-deployment.yaml` 里。`NEKORISU_WEBDAV_URL` 和客户端填同一个地址：归档在它下面的 `nekorisu/v1/`，所以填 `nekorisu/` 的上一层。
  OpenList / Alist 的 `/dav` 根是虚拟的，只能写在挂载的存储里：加一个挂载路径为 `/nekorisu` 的存储，地址填 `…/dav` 即可。
  服务端用集群里的服务名读存储时（`http://openlist.<命名空间>.svc.cluster.local:5244/dav` 这类），在 `20-deployment.yaml` 里再设 `NEKORISU_DEVICE_WEBDAV_URL`，写电脑访问同一个文件夹用的地址；`setup_info` 和 panel 给电脑的就是它。
- `device-token`：设备拉配置、上报时用。
- MCP 没有令牌要配：Agent 用 OAuth 登录（第 5 步）。

## 3. 部署

```bash
kubectl apply -f deploy/k8s.local/10-pvc.yaml -f deploy/k8s.local/20-deployment.yaml -f deploy/k8s.local/30-service.yaml -f deploy/k8s.local/40-ingress.yaml -f deploy/k8s.local/41-ingress-grpc.yaml
kubectl -n nekorisu rollout status deploy/nekorisu-server
kubectl -n nekorisu get certificate,ingress
```

## 4. 第一次打开 panel

日志里有一次性的设置码，打开你的域名（`https://nekorisu.example.com`），用它设置管理员密码：

```bash
kubectl -n nekorisu logs deploy/nekorisu-server | grep "Setup code"
```

## 5. 接 Agent（MCP）

`20-deployment.yaml` 里的 `NEKORISU_PUBLIC_URL` 要是 Agent 访问的地址（端口不是 443 就带上），MCP 才开。
在 Agent 所在的电脑上：

```bash
claude mcp add --transport http -s user nekorisu-server https://nekorisu.example.com/mcp
claude mcp login nekorisu-server
# Codex
codex mcp add nekorisu-server --url https://nekorisu.example.com/mcp
codex mcp login nekorisu-server
```

`login` 打开浏览器，输入 panel 的管理员密码批准（所以先做完第 4 步）。不用复制令牌；panel 的“应用”页可以撤销。
OAuth 的路径（`/.well-known/…`、`/oauth/…`）和 `/mcp` 一样走这个 ingress，不用另外配。

Agent 用网址当 `client_id` 时（Claude Code 就是），服务端要能连到那个网址去取应用的说明文档；Pod 出不了公网的话，设 `NEKORISU_OAUTH_CLIENT_METADATA_DOCUMENTS=false`，Agent 会改用动态注册。

节点的 DNS 是 fake-IP 代理（sing-box、Clash 这类，外网域名都解析成 198.18.x.x；CoreDNS 转发给节点，所以 Pod 里也一样）时，服务端看不出说明文档真正在哪，不会去取，日志里会提示。这时在 `20-deployment.yaml` 里打开 `NEKORISU_DNS_OVER_HTTPS`（例如 `https://1.1.1.1/dns-query` 或 `https://223.5.5.5/dns-query`，主机必须写 IP），服务端就通过 DNS over HTTPS 解析这些域名。

## 6. 检查

打开 `https://nekorisu.example.com/connect`（不用登录）。上面列着这台服务端开了什么、还缺哪些设置；“还差什么”一节没有了，就都配齐了。
这一页也是给电脑和 Agent 的连接说明：把地址发给 Agent，它照着就能接。存储地址和用户名默认不在上面，Agent 登录 MCP 后用 `setup_info` 拿。
不想公开这一页，在 `20-deployment.yaml` 里设 `NEKORISU_CONNECT_PAGE=false`。

## 来源地址

panel 的登录、设置码、MCP 授权页上的密码和 OAuth 注册都按来源地址限速。Pod 看到的对端是 ingress-nginx，所以 `NEKORISU_TRUSTED_PROXIES` 要包含 ingress-nginx 控制器 Pod 的网段（样例是 k3s 默认的 `10.42.0.0/16`，换成你集群的）：对端在这些网段里，服务端才相信它给的 `X-Real-IP`、`X-Forwarded-For`。网段里的其他 Pod 也能自称任何来源，在意的话写窄一些。不设的话，所有请求都算 ingress 一个来源，一个人输错几次密码就把所有人挡在外面一分钟。

ingress-nginx 自己看到的也得是客户端的真实地址：比如它的 Service 用 `externalTrafficPolicy: Local`；前面还有一层只做 TLS 透传的代理时，那一层和 ingress-nginx 都要开 PROXY protocol。不然所有人还是同一个来源。

## 备份

`/data`（PVC `nekorisu-data`）里有 SQLite 和配置签名钥 `signing-key.json`。签名钥丢了，所有设备都得重新指向这个服务端。

## 说明

- 设备协议（gRPC，7070）由 `41-ingress-grpc.yaml` 经 ingress 对外，和 panel 同一个主机名，路径 `/nekorisu.cloud.v1.Cloud`。设备连的就是 panel 的地址：配置页上给出 `nekorisu source add` 的完整命令，设备上再运行 `nekorisu source login`，输入 `device-token`。ingress 前面还有一层代理的话，它要原样透传 TLS（或者自己支持 HTTP/2），gRPC 才过得去。
- `/config` 是空的时候，设备拿到的是全默认的配置。
