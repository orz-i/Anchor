# Anchor FRP HTTPS → HTTP 验证

日期：2026-08-06  
工作区：Anchor  
FRPS：`43.157.17.95:17001`

## 实现

Anchor 的 MCP 与 Actions FRP 配置新增两种公网协议：

- `http`：生成 FRP `type = "http"`，使用 `vhostHTTPPort`；
- `https2http`：生成 FRP `type = "https"` 与 `[proxies.plugin] type = "https2http"`，使用 `vhostHTTPSPort`，在 frpc 所在机器终止 TLS 后转发到本地 HTTP listener。

HTTPS → HTTP 模式生成的核心配置：

```toml
[[proxies]]
type = "https"
subdomain = "anchor"

[proxies.plugin]
type = "https2http"
localAddr = "127.0.0.1:28766"
crtPath = "D:\\anchor\\.anchor\\cert\\taoyan.icu.pem"
keyPath = "D:\\anchor\\.anchor\\cert\\taoyan.icu.key"
```

服务端配置了 `subDomainHost = "taoyan.icu"`，因此该根域下必须使用 `subdomain`。实机尝试把完整域名放入 `customDomains` 时，FRPS 明确拒绝：该域名属于已配置的 subdomain host。

## 证书发现与安全

- `.anchor/cert/` 已加入 `.gitignore`，防止私钥被 Git 意外提交。
- 证书和私钥路径必须位于当前工作区内。
- 最终文件不能是符号链接，canonical path 也不能逃逸工作区。
- 文件必须存在、为普通文件且非空。
- 两个路径留空时，从 `.anchor/cert` 中选择唯一的同名 `.pem/.crt/.cer` 与 `.key` 文件。
- 只填写一侧时，Anchor 尝试寻找同名配对文件。
- 存在多个配对时拒绝猜测，要求用户明确选择。
- 私钥内容不会进入 Workspace profile、日志或 Git；frpc 配置只保存路径。

当前证书公开元数据：

- 颁发者：Cloudflare Origin SSL Certificate Authority；
- SAN：`*.taoyan.icu`、`taoyan.icu`；
- 有效期：2026-07-24 至 2041-07-20。

## 服务端实机诊断

使用实际证书与私钥启动 frpc v0.61.2：

- 控制连接登录成功；
- `https2http` proxy 使用 `subdomain` 注册成功；
- 证书和私钥可被插件正常加载；
- 对 `43.157.17.95:443` 使用目标 SNI 连接时仍立即 EOF。

用户给出的服务端字段为：

```toml
vhostHttpsPort = 443
```

frp 官方 TOML 字段为：

```toml
vhostHTTPSPort = 443
```

`HTTPS` 必须全部大写。当前重复诊断证明 frpc 代理本身已注册，但 443 没有进入对应 SNI 路由；服务端应更正字段并重启 frps，同时确认 443 没有被其他进程占用。

Anchor 的“测试连接”也已加强：frpc 注册成功后会继续请求实际公网 URL。若公网仍不可达，会返回包含 `vhostHTTPSPort` 的可操作诊断，而不再误报测试成功。

## 持久化兼容

新增字段：

- `frp_proxy_type`
- `frp_cert_path`
- `frp_key_path`

旧 profile 缺少这些字段时自动迁移为 `http` 模式和空路径，不改变现有 FRP 行为。

## 配置方式

工作区隧道中选择：

```text
隧道类型：FRP
公网协议：HTTPS → 本地 HTTP
子域名：anchor
公网 URL：https://anchor.taoyan.icu
证书路径：留空自动发现，或 .anchor/cert/taoyan.icu.pem
私钥路径：留空自动发现，或 .anchor/cert/taoyan.icu.key
```

Cloudflare 可使用 Full (Strict)，因为源站 HTTPS 由 `https2http` 插件使用 Cloudflare Origin CA 证书终止。
