# Federation：Anchor Node 之间的受信任只读互联

Federation 让多个 Anchor Node 在**不复制 Workspace 状态、不共享 Harness store、不开放远程写入**的前提下，读取彼此的 Node/Workspace 运行状态。

当前 wire contract：

```text
anchor-federation-v2
```

当前安全模型是：

```text
out-of-band signed bootstrap / discovery
        ↓
local peer registry (Untrusted)
        ↓
pairwise bearer credential
        ↓
authenticated + signed probe
        ↓
explicit operator trust
        ↓
Trusted read-only peer
```

**发现不等于信任，签名有效也不等于信任。** 所有 trust/re-bootstrap/revoke 等状态改变都要求显式 operator 动作。

## 当前产品入口

Federation 已有完整 Rust management contract、Web Admin command contract 和 TypeScript API contract，但当前版本**没有专用可视化 Federation 页面，也没有 `anchor federation ...` CLI command group，更没有一级 MCP `federation` tool**。

因此本页同时服务两类读者：

- 管理员：理解部署、Gateway、配对、凭据和安全边界；
- Web Admin/集成开发者：了解当前已经存在的 management commands，避免重复实现第二套状态。

相关 typed API 位于 `src/lib/api/workspaces.ts`。这些 API 是当前 Web Admin 管理层的一部分；不要把内部 command 名称包装成新的 CLI/MCP surface。

## Node 与 Workspace

每台 Anchor 安装有稳定 machine-local Node identity：

```text
data/node.json
node_<32 hex>
```

Node ID 与 Workspace ID 是不同层级：

```text
Node A
 ├─ Workspace A1
 └─ Workspace A2

Node B
 └─ Workspace B1
```

Portable config export/import 不携带 Node identity；迁移到另一台机器后仍应获得新的 Node ID，而不是复制旧节点身份。

## Signing identity

Federation 使用 Ed25519 Node signing identity，独立于稳定 Node ID：

```text
data/federation-signing.json
```

当前内部存储 schema 为 v2。私钥不会再以明文 `privateKeyPkcs8Base64` 保存：

- Windows：使用 DPAPI Local Machine protection，兼容 interactive Anchor 与 service-managed control plane；
- Linux/其他非 Windows：沿用 private-file-permissions protection，并依赖私有文件权限；
- v1 明文 signing record 已 hard-cut。当前版本不会读取、迁移或重写旧 `privateKeyPkcs8Base64`；检测到 schema v1 会 fail closed。需要重新建立本机 signing identity 时，应先归档/移除旧 record，再通过 out-of-band discovery/bootstrap 对所有受影响 peer 重新确认 fingerprint 和 trust。

公钥身份形如：

```text
contract    anchor-node-signature-v1
algorithm   ed25519
keyEpoch    1, 2, 3, ...
fingerprint sha256:<hex>
```

显式 rotation 会增加 `keyEpoch` 并生成新 key，但 Node ID 保持不变。

## Gateway wire endpoints

Federation 不启动独立公网 listener。所有远端 HTTP surface 都挂在**现有 MCP Gateway** 上：

```text
GET  /federation/v2/discovery
POST /federation/v2/read
```

如果 Gateway 没有启用/运行，就没有可远端访问的 Federation HTTP ingress。

### `/federation/v2/discovery`

唯一 discovery endpoint，返回 `anchor-federation-discovery-v2`，其中包含当前 signed bootstrap bundle，并可携带 bounded `rotationChain`。客户端不会在 404 或协议失败后回退到旧 `/federation/v2/bootstrap`。

Discovery 是**无需 bearer credential 的 public metadata**，但不会导出 Workspace catalog。用于发现的 descriptor 会把 `workspaces` 清空，因此不能用 discovery endpoint 枚举私有 Workspace。

### `/federation/v2/read`

真正的远端读取 endpoint。要求：

- pairwise bearer credential；
- caller Node header 与 signed request envelope 一致；
- request timestamp/TTL 有效；
- nonce/request ID 未重放；
- target Node 与本机 Node 一致；
- runtime/federation contract 兼容；
- 返回值带 Node signing key 的 detached Ed25519 signature。

客户端会验证 responder Node、contract、result kind 和签名后才接受结果。

## Endpoint 约束

Peer endpoint 是**origin URL**，例如：

```text
https://node-a.example.com
http://127.0.0.1:28765
```

规则：

- 非 loopback 必须 HTTPS；
- HTTP 只允许 `localhost`、`127.0.0.1`、`::1`；
- 不能带 username/password；
- 不能带 path、query 或 fragment；
- origin 最长 256 bytes。

不要把 `/federation/v2/read` 自身拼进 peer endpoint；Anchor 会从 origin 构造固定协议路径。

## Pairwise credential

Federation bearer token 必须是 32–4096 个可打印字符，不能包含 control character。

对一个 target 设置 credential 时，本机同时保存：

- 绑定 `remote Node ID + canonical endpoint` 的 outbound token；
- 绑定 remote Node ID 的 inbound expected token。

因此双向通信时，应在 A 和 B 两端分别对“对方 Node”配置同一个 pairwise token：

```text
Node A: target = Node B, token = T
Node B: target = Node A, token = T
```

凭据保存在 machine-local SecretStore，不进入 portable config bundle。Endpoint 变化会使旧 endpoint-bound credential 失效并触发 re-bootstrap/reset 行为。

## Peer registry 与 trust state

Peer registry 位于 machine-local：

```text
data/federation-peers.json
```

状态只有：

```text
untrusted
trusted
drifted
revoked
```

### Untrusted

已注册但未完成显式信任。可能已经有 bootstrap pin、credential 或 probe 结果，但不能执行 trusted remote read。

### Trusted

必须满足：

1. 已注册 signed bootstrap；
2. pairwise credential ready；
3. 最近 signed probe 成功；
4. probe descriptor digest 与 signer 匹配 bootstrap pin；
5. operator 显式执行 trust。

### Drifted

已信任 peer 的安全 descriptor 或 signing identity 与 trusted pin 不一致时进入 Drifted。远端读取 fail-closed。

### Revoked

显式撤销后的状态。Revoked peer 不能通过普通 discovery 自动恢复信任。

## 推荐配对流程

当前没有专用可视化页面，因此下面使用 management contract 名称表达步骤；实际产品集成应复用 Web Admin 的 privileged confirmation/grant 流程，而不是绕过权限层直接改本地文件。

### 1. 在远端获取 discovery/bootstrap

远端网络发现统一使用：

```text
get_federation_discovery_document
```

如果当前集成只需要生成本机 signed bootstrap bundle，而不是访问旧网络 endpoint，可使用：

```text
get_federation_bootstrap_bundle
```

`get_federation_bootstrap_bundle` 是当前 bootstrap 子契约的本地管理能力，不代表 `/federation/v2/bootstrap` 兼容路由仍存在。把返回的 signed public bundle 通过可信的 out-of-band 渠道交给另一端。Bundle 不包含私钥或 bearer credential。

### 2. 只读检查 candidate

```text
inspect_federation_candidate
```

该操作验证 Node ID、descriptor、signer 和 endpoint，但：

```text
persisted      = false
credentialSent = false
trustStatus    = untrusted
```

### 3. 显式注册

```text
register_federation_peer
```

这是 privileged mutation，需要 Web Admin grant。注册后仍是 `untrusted`。

### 4. 在两端配置 pairwise credential

```text
set_federation_peer_credential
```

同样是 privileged mutation。可使用：

```text
get_federation_peer_credential_status
```

检查 inbound/outbound 是否都已配置。

### 5. Probe

```text
probe_federation_peer
```

Probe 实际通过 authenticated/signed federation read 获取远端 catalog descriptor。成功 probe 会记录最后 descriptor digest 与 signer，但不会自动 trust。

### 6. 显式 Trust

```text
trust_federation_peer
```

只有最近 probe 与 bootstrap pin 一致时才能成功。

### 7. 远端读取

```text
read_federation_remote
```

Registry gate 会先要求 target peer 为 `trusted`；response signer 发生变化时当前读取立即失败，并把 peer 标记为 Drifted。

## 允许的 Federation read

Wire contract 当前只有四种 operation：

```text
node_capabilities
node_control_status
workspace_catalog
workspace_status
```

没有 remote shell、文件读取、Git、Harness Task、Skill mutation、配置写入或任意 command execution。

### Context scope

请求 context 只有：

```text
global_shared
node_local
workspace_local
```

当前 `global_shared` 表示 Federation context scope 语义，不代表已经存在跨节点复制型 global database。

安全 policy 固定：

```text
exportWorkspacePaths = false
exportSecrets        = false
exportHarnessState   = false
remoteMutation       = false
```

## Discovery 与 key rotation

Discovery inspection 可能得到：

```text
current
descriptor_drift
rotation_available
identity_drift
```

### 合法 rotation

合法 rotation 不是“新 key 自己说自己是新 key”，而是旧 key 对新 key 签发 continuity notice：

```text
K1 --signed notice--> K2 --signed notice--> K3
```

当前 machine-local history：

```text
data/federation-rotation-history.json
```

最多保存 8 跳。Discovery v2 会发送仍有效、连续的 bounded chain；peer 可以从 retained chain 中自己已经 pin 的任意旧 key 验证到 current key。旧单条 `data/federation-rotation-notice.json` 不再写入，也不会在 history 缺失时回读。

即使 continuity 验证成功，也只得到 `rotation_available`，**不会自动接受新 signer**。

推荐流程：

```text
inspect_federation_peer_discovery
        ↓
rotation_available
        ↓
operator compare fingerprint / bootstrap
        ↓
accept_federation_peer_rebootstrap
        ↓
Untrusted
        ↓
probe
        ↓
explicit trust
```

如果 peer 离线超过 rotation history retention 范围，需要重新通过 out-of-band bootstrap 建立信任链。这是当前 bounded 安全设计，不会自动越过缺失的 key history。

## Trust health

Peer view 的 health 是 registry/probe/credential 的**派生状态**，不是第二套权威：

```text
healthy
probe_stale
credential_missing
untrusted
drifted
revoked
```

`healthy` 不意味着开放写能力，只表示当前 read-only trust material 完整且 probe 新鲜。

## 当前 management commands

### Read-only

当前 Web Admin backend 提供的主要只读 contract：

- `get_federation_catalog`
- `get_federation_signing_status`
- `get_federation_bootstrap_bundle`
- `get_federation_discovery_document`
- `validate_federation_peer`
- `resolve_federation_read`
- `list_federation_peers`
- `get_federation_peer`
- `inspect_federation_peer_discovery`
- `inspect_federation_candidate`
- `get_federation_peer_credential_status`
- `read_federation_remote`

### Stateful probe（不需要 privileged grant）

- `probe_federation_peer`

Probe 会发起 authenticated/signed read，并把 `last_probe_*`、最近 descriptor/signing observation 写回 peer registry；发现可信 peer 的 contract/bootstrap drift 时还可能把状态更新为 `drifted`。因此它不是只读 command，但当前实现**不属于 privileged action**，不消费 Web Admin privileged grant。

### Privileged / mutation

- `rotate_federation_signing_key`
- `register_federation_peer`
- `update_federation_peer`
- `accept_federation_peer_rebootstrap`
- `trust_federation_peer`
- `remove_federation_peer`
- `set_federation_peer_credential`
- `clear_federation_peer_credential`
- `rotate_federation_peer_credential`
- `revoke_federation_peer`

这些 mutation 必须经过 Web Admin privileged grant/binding；不要通过直接编辑 registry、secret 或 signing 文件模拟操作。

## 故障排查

| 状态/错误 | 处理方向 |
| --- | --- |
| `FEDERATION_CREDENTIAL_MISSING` | 检查 target Node+endpoint 的 outbound credential |
| `FEDERATION_AUTH_REQUIRED` / `...INVALID` | 检查两端是否使用同一个 pairwise token、caller Node ID 是否正确 |
| `FEDERATION_PEER_NOT_TRUSTED` | 远端没有为 caller Node 配置 inbound credential，或 peer 已被清理 |
| `drifted` | 检查 descriptor/signing fingerprint，必要时重新 bootstrap |
| `rotation_available` | 手工确认新 bootstrap 后 accept re-bootstrap，再 probe/trust |
| `identity_drift` | 不要自动接受；视为未知 signer 或攻击，重新走 out-of-band 验证 |
| Gateway stopped | Federation HTTP endpoints 不存在；先恢复 Gateway desired state |
| 非 HTTPS endpoint 被拒绝 | 只有 loopback HTTP 允许；远端必须使用 HTTPS origin |

## 明确不做的事情

当前 Federation 不提供：

- 自动 LAN/Internet 扫描；
- 自动 trust；
- 自动 token/key exchange；
- public federation-only listener；
- remote file/Git/Harness/Skill/config mutation；
- remote shell/exec；
- 跨节点状态复制；
- 自动接受 signing key rotation。

这些限制是当前安全边界，不应在客户端层通过 fallback 绕过。
