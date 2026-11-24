# 部署到 k8s（k3s）

放在自己的命名空间 `nekorisu` 里；入口是 ingress-nginx，证书用 cert-manager 的 ClusterIssuer（清单里写的是 `letsencrypt-prod`，换成你自己的）。
这里的清单是样例：域名和存储地址都是占位符（`example.com`）。密钥不在任何文件里，都在 Secret 里，下面第 2 步创建。

## 0. 复制一份，填上自己的值

```bash
cp -r deploy/k8s deploy/k8s.local
```

`deploy/k8s.local/` 已被 `.gitignore` 忽略，不会进仓库。
在 `20-deployment.yaml` 里改 `NEKORISU_WEBDAV_URL`、`NEKORISU_WEBDAV_USER`、`NEKORISU_PUBLIC_URL`，在 `40-ingress.yaml` 里改域名和 ClusterIssuer。
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
  --from-literal=device-token="$(openssl rand -hex 32)" \
  --from-literal=mcp-token="$(openssl rand -hex 32)"
```

- `webdav-password`：存储的密码；用户名和地址在 `20-deployment.yaml` 里（`NEKORISU_WEBDAV_URL` 指向装着归档的那一层，里面是 `nekorisu/v1/`）。
  有的 WebDAV 服务（比如 OpenList）的 `/dav` 根是虚拟的，不能直接建文件夹，写入会返回 405，只能写在挂载的存储里。这时地址要指向挂载下的某个目录。
- `device-token`：设备拉配置、上报时用。
- `mcp-token`：Agent 连 `/mcp` 时用。

## 3. 部署

```bash
kubectl apply -f deploy/k8s.local/10-pvc.yaml -f deploy/k8s.local/20-deployment.yaml -f deploy/k8s.local/30-service.yaml -f deploy/k8s.local/40-ingress.yaml
kubectl -n nekorisu rollout status deploy/nekorisu-server
kubectl -n nekorisu get certificate,ingress
```

## 4. 第一次打开 panel

日志里有一次性的设置码，打开你的域名（`https://nekorisu.example.com`），用它设置管理员密码：

```bash
kubectl -n nekorisu logs deploy/nekorisu-server | grep "Setup code"
```

## 5. 取令牌（给 Agent 配 MCP 用）

```bash
kubectl -n nekorisu get secret nekorisu-secrets -o jsonpath="{.data.mcp-token}" | base64 -d
```

## 备份

`/data`（PVC `nekorisu-data`）里有 SQLite 和配置签名钥 `signing-key.json`。签名钥丢了，所有设备都得重新指向这个服务端。

## 说明

- 设备协议（gRPC，7070）没有经过 ingress 对外。
- `/config` 是空的时候，设备拿到的是全默认的配置。
