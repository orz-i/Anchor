# Anchor FRP / taoyan.icu 链路验证

日期：2026-08-06  
工作区：Anchor  
FRPS：`43.157.17.95:17001`  
公网根域名：`taoyan.icu`

## 结论

FRPS 服务端控制链路和 HTTP vhost 端口均可达；当前无法使用首先是因为 Anchor 工作区仍配置为 Cloudflare Quick Tunnel，尚未选择 FRP profile，也没有 FRP 子域名，因此运行态从未启动 `frpc`。

同时确认了一个 Anchor 缺陷：FRP 控制服务器地址被用于自动生成公网 URL。控制服务器为 `43.157.17.95`、公网根域名为 `taoyan.icu` 时，旧实现会生成无效地址 `https://<subdomain>.43.157.17.95`，并覆盖用户填写的实际公网 URL。

修复后，FRP 控制地址、控制端口与公网 URL 相互独立：

- 控制地址：`43.157.17.95`
- 控制端口：`17001`
- 子域名：例如 `anchor`
- 公网 URL：`https://anchor.taoyan.icu`

当控制地址是 IP 时，Anchor 现在强制要求填写实际公网 URL；切换隧道类型时也会清理上一种隧道遗留的公网地址。

## 实测结果

| 项目 | 结果 |
| --- | --- |
| `43.157.17.95:17001` | TCP 可连接，约 3–15 ms |
| FRPS 控制 TLS | TLS 1.3 握手成功 |
| `43.157.17.95:80` | TCP 可连接；带未注册测试 Host 返回 FRPS 404 |
| `43.157.17.95:443` | TCP 可连接；未注册 HTTPS proxy 时 TLS SNI 握手 EOF |
| `*.taoyan.icu` | 解析到 Cloudflare Anycast 地址，而非源站 IP |
| Cloudflare HTTP | 返回 Cloudflare 301 HTTPS 跳转 |
| Cloudflare HTTPS | Cloudflare 边缘 TLS 正常 |
| 本机 frpc | Anchor 缓存中存在 v0.61.2 客户端 |
| frpc 配置校验 | `frpc verify -c` 通过 |

服务端 `transport.tls.force = true` 与当前 frpc 兼容；frp 0.50 以后客户端控制连接默认启用 TLS。

## Cloudflare 注意事项

Cloudflare 橙云 DNS 记录只适合代理 HTTP/HTTPS。FRP 控制连接 `17001` 必须使用服务器 IP，或单独建立 DNS-only 控制域名；不能让 frpc 连接橙云 wildcard 域名的 `17001`。

Anchor 当前创建的是 FRP `http` proxy，实际由 FRPS 的 `vhostHTTPPort = 80` 接收。Cloudflare 的 Full / Full (Strict) 模式会让外部 HTTPS 请求连接源站 443，但 FRPS 443 只会路由已注册的 FRP `https` proxy，而 Anchor 本地服务不是 HTTPS。因此：

- 临时验证可对目标 hostname 使用 Cloudflare Configuration Rule，将 SSL 模式设为 Flexible，使 Cloudflare 到源站走 HTTP 80；不要无必要地对整个 zone 改为 Flexible。
- 更严格的长期方案需要一个真正支持源站 TLS 的反向代理/HTTPS FRP 路由，再使用 Full (Strict)。
- `transport.tls.certFile` / `keyFile` 保护的是 frpc ↔ frps 控制连接，不会替 FRP HTTP 服务终止公网 HTTPS。

## 修复后的配置步骤

1. 在“设置 → FRP 配置”中填写控制服务器 `43.157.17.95`、端口 `17001` 和 Token。
2. 在工作区隧道中把类型切换为 FRP，选择上述 profile。
3. 填写唯一子域名，例如 `anchor`。
4. 填写实际公网 URL：`https://anchor.taoyan.icu`。
5. 关闭“使用网络代理”，除非本机确实需要 HTTP/SOCKS 代理才能访问 FRPS。
6. 点击“测试连接”；成功后再启动 MCP 服务并保持隧道运行。
7. 若 wildcard 保持 Cloudflare 橙云，为该 hostname 配置合适的源站协议；当前 HTTP proxy 需要 Cloudflare 到源站走 HTTP 80。

## 回归验证

- FRP 模块单元测试 16/16 通过。
- 显式公网 URL 优先于控制服务器地址。
- IP 控制服务器缺少公网 URL 时返回可操作错误。
- Workspace 与前端 URL 计算均保留显式公网 URL。
- Svelte / TypeScript 检查通过。
