# Orchestration：只读 Workflow / Fleet 规划与观测

Anchor 的 Orchestration 当前是一个 **read-only planning and inspection foundation**，不是远程执行器或持久调度器。

当前 contract：

```text
anchor-orchestration-v1
```

它把一个瞬时 Workflow spec 解析成 DAG plan，然后从现有状态权威读取 Node、Workspace、Harness Task 状态：

```text
Workflow spec
    ↓
validate + plan DAG
    ↓
dependency waves
    ↓
read-only observations
    ├─ Runtime capability provider
    ├─ Existing control plane
    ├─ Harness store / Canvs projection
    └─ Trusted Federation read
```

Orchestration **没有自己的 workflow database、scheduler state 或 task authority**。

返回的 plan 会明确：

```text
readOnly       = true
stateAuthority = existing_harness_federation_control_plane
```

## 当前产品入口

当前实现有两个 Web Admin read-only management contracts：

```text
plan_orchestration_workflow
inspect_orchestration_workflow
```

TypeScript wrapper 位于 `src/lib/api/workspaces.ts`：

```text
planOrchestrationWorkflow(...)
inspectOrchestrationWorkflow(...)
```

当前版本**没有 Orchestration 专用可视化页面、没有 `anchor orchestration ...` CLI command，也没有一级 MCP orchestration tool**。因此本页主要面向 Web Admin/平台集成开发者和需要理解 P3 contract 的管理员。

## Workflow spec

基本结构：

```json
{
  "schemaVersion": 1,
  "contract": "anchor-orchestration-v1",
  "id": "fleet-health",
  "fleet": {
    "id": "prod-fleet",
    "targets": []
  },
  "steps": []
}
```

当前边界：

- 最多 32 个 target；
- 最多 64 个 step；
- 每个 step 最多 16 个 dependency；
- reference ID 最多 128 bytes；
- 同一 dependency wave 内 observation 最大并发 4。

## Target

支持三类：

```text
node
workspace
harness_task
```

### Node target

```json
{
  "id": "local-node",
  "kind": "node",
  "nodeId": "node_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
}
```

Node target 不能携带 `workspaceId` / `taskId`。

### Workspace target

```json
{
  "id": "remote-workspace",
  "kind": "workspace",
  "nodeId": "node_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
  "workspaceId": "0123456789abcdef0123456789abcdef"
}
```

### Harness Task target

```json
{
  "id": "local-task",
  "kind": "harness_task",
  "nodeId": "node_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
  "workspaceId": "0123456789abcdef0123456789abcdef",
  "taskId": "task-id"
}
```

当前 Federation v2 不导出远端 Harness Task state，所以 **remote `harness_task` target fail-closed**。

即使给两个 target 不同 alias，只要它们实际指向相同 Node/Workspace/Task identity，也会作为重复 target 拒绝，避免一个底层对象被伪装成多个逻辑执行目标。

## Read operation

当前只有：

```text
node_capabilities
node_control_status
workspace_status
harness_task_status
```

Operation 必须与 target kind 匹配；不存在“用 Node target 读取 Workspace”之类的隐式转换。

读取来源：

| 场景 | source |
| --- | --- |
| 本地 Node capability | `runtime_capability_provider` |
| 本地 Node/Workspace status | `existing_control_plane` |
| 本地 Harness Task | `harness_store` |
| 远端 Node/Workspace | `federation_read` |

远端 Node/Workspace 读取必须先满足 Federation trusted-peer gate；Orchestration 不自行绕过 trust，也不保存额外 remote credential。

## DAG 与 dependency waves

Step 示例：

```json
{
  "id": "workspace",
  "targetId": "remote-workspace",
  "operation": "workspace_status",
  "dependsOn": ["node"]
}
```

Planner 会拒绝：

- unknown dependency；
- self dependency；
- duplicate dependency；
- DAG cycle；
- duplicate step ID；
- incompatible target/operation；
- remote Harness state。

一个简单 Workflow：

```json
{
  "schemaVersion": 1,
  "contract": "anchor-orchestration-v1",
  "id": "remote-workspace-health",
  "fleet": {
    "id": "fleet-a",
    "targets": [
      {
        "id": "remote-node",
        "kind": "node",
        "nodeId": "node_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
      },
      {
        "id": "remote-workspace",
        "kind": "workspace",
        "nodeId": "node_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        "workspaceId": "0123456789abcdef0123456789abcdef"
      }
    ]
  },
  "steps": [
    {
      "id": "node",
      "targetId": "remote-node",
      "operation": "node_capabilities",
      "dependsOn": []
    },
    {
      "id": "workspace",
      "targetId": "remote-workspace",
      "operation": "workspace_status",
      "dependsOn": ["node"]
    }
  ]
}
```

Plan 会产生：

```json
{
  "waves": [
    ["node"],
    ["workspace"]
  ]
}
```

当前实现不仅生成 waves，`inspect` 也会**真实遵守 dependency barrier**：前一 wave 的 observation 全部结束后才开始下一 wave；同一 wave 内最多并发 4 个 observation。

注意：dependency 控制的是**观测顺序**，不是写操作提交顺序，因为当前所有 operation 都是 read-only。

## Plan 与 Inspect

### Plan

`plan_orchestration_workflow` 只做 validation 与 source resolution，不执行 remote read。

Plan step 会包含：

- target；
- operation；
- `dependsOn`；
- source；
- `remote`；
- 需要时生成 `federationRequest`。

适合在真正 inspection 前做安全预览。

### Inspect

`inspect_orchestration_workflow` 先生成 plan，再按 waves 读取状态。

每个 step 返回：

```text
observed
unavailable
```

底层 provider/peer 无法读取时，step 会变成 `unavailable` 并带 bounded error；Orchestration 不会伪造 success，也不会把整个 Workflow 状态写入新数据库。

Inspection summary 只统计：

```text
observed
unavailable
```

它不是 workflow execution state machine。

## Harness Task observation

本地 `harness_task_status` 读取既有 Harness/Canvs 投影，例如：

```text
workspaceId
taskId
status
progressPercent
current
active
updatedAt
```

Orchestration 不修改 Task phase、Slice、verification、Recovery 或 Git worktree。

远端 Harness Task 目前明确返回：

```text
ORCHESTRATION_REMOTE_HARNESS_UNAVAILABLE
```

这是安全边界，不应通过“找不到远端时读本地同名 Task”之类 fallback 绕过。

## 与 Federation 的关系

Orchestration 复用 Federation 的 trusted remote read：

```text
remote Node target
    → node_capabilities / node_control_status

remote Workspace target
    → workspace_status
```

因此：

- 未注册 peer 不可读；
- `untrusted` 不可读；
- `drifted` / `revoked` 不可读；
- credential 缺失不可读；
- signer drift 会由 Federation 先 fail-closed。

Orchestration 本身不维护 peer registry、credential 或 trust pin。

## 当前不会执行什么

`anchor-orchestration-v1` 当前没有：

- remote shell；
- remote file mutation；
- Git mutation；
- Harness Task mutation；
- persistent workflow scheduler；
- retry/compensation state machine；
- distributed lease/lock；
- automatic fleet scheduling；
- write capability negotiation。

如果未来增加 dispatch/write，必须建立新的明确授权和执行 contract；不能直接把当前 read-only inspector 当作 executor。

## 推荐使用方式

集成方应把 Orchestration 当作一个“安全的多目标观测计划器”：

```text
1. 构造 bounded Workflow spec
2. plan，检查 waves/source/remote target
3. 确认所有 remote target 已通过 Federation trust
4. inspect
5. 根据 observed/unavailable 做 UI 或诊断展示
```

不要把 inspection 结果持久化成第二套 Anchor 状态权威；如果需要历史趋势，应作为外部 observability 数据处理，并明确它只是 snapshot/cache。

## 常见错误

| 错误 | 含义 |
| --- | --- |
| `ORCHESTRATION_DAG_CYCLE` | dependency 存在环 |
| `ORCHESTRATION_DEPENDENCY_UNKNOWN` | dependsOn 指向不存在 step |
| `ORCHESTRATION_TARGET_IDENTITY_DUPLICATE` | 多个 alias 指向同一底层 target |
| `ORCHESTRATION_TARGET_BINDING_INVALID` | target kind 与 node/workspace/task 字段组合不合法 |
| `ORCHESTRATION_OPERATION_TARGET_MISMATCH` | operation 与 target kind 不匹配 |
| `ORCHESTRATION_REMOTE_HARNESS_UNAVAILABLE` | 当前 Federation 不导出远端 Harness Task state |
| observation=`unavailable` | 对应 runtime/control/federation provider 本次无法读取 |

## 状态权威

整个 P3 contract 最重要的不变量是：

```text
stateAuthority = existing_harness_federation_control_plane
```

这意味着：

- Harness Task 仍由 Harness store 管理；
- Workspace/daemon 状态仍由现有 control plane 管理；
- remote Node/Workspace 仍由 Federation trust/read transport 管理；
- Workflow spec、plan、inspection 都只是瞬时派生数据。
