# MCP Tool 规范审计（2026-07-26）

## 结论

结论：**核心本地 Tool 的基础协议形态通过，但 Tool 安全语义、聚合代理信任边界和状态模型仍需整改；当前不建议把全部工具档位标记为规范完成。**

审计基线为 MCP 稳定规范 `2025-11-25`，并参考当前 draft 中的聚合命名、状态型 Tool 和 Tool schema 演进指南。

## 审计范围

- `src-tauri/src/tools/registry.rs`
- `src-tauri/src/tools/schema.rs`
- `src-tauri/src/tools/dispatch.rs`
- `src-tauri/src/tools/context.rs`
- `src-tauri/src/tools/workspace.rs`
- `src-tauri/src/tools/policy.rs`
- `src-tauri/src/tools/image_tool.rs`
- `src-tauri/src/mcp/server.rs`
- `src-tauri/src/mcp/proxy.rs`
- `src-tauri/src/mcp/protocol.rs`
- `src/lib/components/RuntimePolicyForm.svelte`

代码基线：`532586f462546f7110052939d7ca3248b6751074`。

## 已确认符合的基础项

- 当前主协议版本为 `2025-11-25`。
- 核心档位暴露 27 个名称唯一的本地 Tool。
- 本地 Tool 名称仅使用 ASCII 字母、数字、下划线或连字符，且均远小于 128 字符。
- 每个本地 Tool 都提供 `name`、`title`、`description`、根级 object `inputSchema` 和 `annotations`。
- 无参数 Tool 使用 object schema，并通过 `additionalProperties: false` 拒绝额外参数。
- 本地 input schema 覆盖 required、enum、类型、字符串长度和数值上下限。
- 本地 Tool 执行错误通常返回 `CallToolResult`，包含 `content`、`structuredContent` 和 `isError: true`。
- 未知 Tool、缺少 Tool 名称和本地 worker 异常使用 JSON-RPC error，层级选择正确。
- 普通结构化结果同时以 TextContent 序列化，具备旧客户端回退能力。
- `tools/list` 声明 `listChanged: false`，本地 catalog 在 listener 生命周期内保持固定。
- `read-only` 档位实际移除了 `apply_patch`、`exec_command`、`write_stdin`、`kill_session` 和 `set_default_cwd`。
- `write_stdin`、`read_output`、`kill_session` 使用显式 `session_id`；Task 和历史工具也使用显式 handle。

## P1 问题

### P1-1：`compat-readonly-all` 提供虚假的安全 annotations

位置：

- `src-tauri/src/tools/registry.rs:503-532`
- `src/lib/components/RuntimePolicyForm.svelte:20-25`

该档位暴露全部 P0 Tool，但统一改写为：

```json
{
  "readOnlyHint": true,
  "destructiveHint": false,
  "openWorldHint": false
}
```

实际 `apply_patch`、`exec_command`、`write_stdin`、`kill_session`、历史写入和 Task 状态工具仍可修改环境。客户端若信任该服务器的 annotations，可能自动批准本应要求确认的调用。

建议：删除该档位；或让它真正使用只读 Tool 集。不得通过伪造 annotations 兼容客户端。若暂时保留，GUI 必须明确标记“非只读、危险兼容模式”，但这仍不是最终修复。

### P1-2：聚合代理未经验证地提升下游 Tool metadata 的信任等级

位置：`src-tauri/src/mcp/proxy.rs:170-238`。

聚合流程只检查下游条目是 object 且存在字符串 `name`，之后原样保留：

- `inputSchema`
- `outputSchema`
- `annotations`
- `icons`
- `execution`
- `_meta`

没有验证 schema 是否存在、是否为合法 object、根类型是否符合当前稳定规范，也没有校验 title/annotations 类型。当前服务器是客户端直接信任的上游，因此不应把任意下游服务器的 annotations 当作自身承诺重新发布。

公共名称只做字符替换，不限制最终长度；超长 prefix/name 可以产生超过 128 字符的 Tool 名称。

建议：建立 `validate_and_normalize_proxy_tool()`：

1. 校验名称、长度、唯一性和合法 schema；
2. 对 annotations 重新分类或默认移除；
3. 只保留允许的 Tool 字段；
4. 对被拒绝的单个 Tool 记录 warning，而不是污染整个 catalog；
5. 对 catalog 设置 Tool 数量和总字节上限。

### P1-3：代理重连只比较名称，不比较完整 Tool 契约

位置：

- `src-tauri/src/mcp/proxy.rs:25-35`
- `src-tauri/src/mcp/proxy.rs:320-341`

`catalog_tool_names()` 只计算名称集合。重连后只要名称未变化，即使下游修改了 `inputSchema`、`outputSchema`、annotations 或语义，重连仍成功；上游继续向客户端发布旧 catalog，但调用进入新实现。

影响：客户端按旧 schema 生成参数，下游按新 schema 执行；安全 annotations 也可能与实际行为漂移。

建议：对规范化后的完整 Tool definition 生成稳定 digest，并在重连时比较 digest。任何契约变化都应拒绝重连并要求 listener 重启，或实现 `notifications/tools/list_changed`。

### P1-4：代理 Tool 执行失败被错误提升为协议级错误

位置：

- `src-tauri/src/mcp/proxy.rs:250-310`
- `src-tauri/src/mcp/server.rs:160-173`

下游 `tools/call` 返回 error、连接失败、超时或取消时，代理返回 `Err(proxy_call_error)`；`handle_tools_call` 将其直接作为 JSON-RPC error 返回。

MCP 建议 Tool 执行错误放入 `CallToolResult` 并设置 `isError: true`，这样模型能够读取错误内容并调整参数或重试。当前行为会让客户端把普通 Tool 失败当作服务器协议失败。

建议：未知公共 Tool 继续使用协议 error；已知代理 Tool 的下游执行失败统一转换为结构化 Tool error result。

### P1-5：`request_permissions` 的声明与实现不一致

位置：

- `src-tauri/src/tools/registry.rs:300-307`
- `src-tauri/src/tools/registry.rs:886-922`
- `src-tauri/src/tools/dispatch.rs:260-293`

Tool 描述承诺“创建 scoped permission grant”，schema 暴露 `scope` 和 `ttl_seconds`，但当前实现：

- safe/trusted 模式始终返回 `ELICITATION_UNSUPPORTED`；
- dangerous 模式返回固定的 `dangerously-skip-all-permissions`；
- 不创建 grant 状态；
- 不执行 once/session scope；
- 不执行 TTL；
- 后续 Tool 调用不消费任何 grant。

因此该 Tool 不是权限授予工具，只是能力探测/错误返回工具。

建议：未实现真实 grant 前，从 catalog 移除，或重命名为 `permission_support_status`。若保留原名，必须实现服务器侧 grant store、参数绑定、scope、TTL、单次消费及审计日志。

### P1-6：`confirm=true` 不是可信的人类批准凭证

位置：

- `src-tauri/src/tools/policy.rs:203-300`
- `src-tauri/src/tools/dispatch.rs:10-50`

危险命令和部分脚本只要求调用参数中出现 `confirm=true`。该字段由模型自己生成，没有客户端 elicitation 回执、用户批准 token 或服务器侧 grant 绑定。

影响：错误或恶意模型可自行补充 `confirm=true`，因此它只能表达“调用方声明确认”，不能表达“用户已经批准”。

建议：将 confirm 仅作为二次意图标志；真正高风险操作必须绑定客户端批准产生的不可伪造 grant。未支持 elicitation 的客户端应 fail closed。

### P1-7：取消可能在写入已完成后仍返回“操作已取消”

位置：`src-tauri/src/tools/dispatch.rs:120-335`。

除 `exec_command` 外，多数本地 Tool 不支持执行中取消。通用分发器在 Tool 返回后再次检查 cancellation；若此时 token 已取消且 Tool 原本成功，会用 `REQUEST_CANCELLED` 覆盖成功结果。

对于 `apply_patch`、history 写入或其他状态修改，可能出现：

1. 副作用已经提交；
2. 客户端收到 `isError: true / REQUEST_CANCELLED`；
3. 模型重试并产生重复或二次修改。

建议：只有具备事务回滚或原生取消语义的 Tool 才在完成后返回 cancelled。不可取消的写 Tool 一旦提交成功，应返回成功并附加 `cancellation_observed_after_commit` warning。

## P2 问题

### P2-1：read-only catalog 与初始化 instructions 冲突

位置：

- `src-tauri/src/tools/registry.rs:357-382`
- `src-tauri/src/mcp/server.rs:120-145`

初始化 instructions 无条件要求每个新会话调用 `history_session_bootstrap`，完成每个任务后调用 `history_session_checkpoint`。但 read-only 档位不暴露这些 Tool，客户端无法满足服务器的强制指令。

建议：根据实际 catalog 动态生成 instructions。只有 bootstrap/checkpoint 均暴露时才声明该工作流为 required。

### P2-2：GUI“完整工具”实际映射为 `core`

位置：

- `src/lib/components/RuntimePolicyForm.svelte:20-25`
- `src-tauri/src/tools/registry.rs:477-497`
- `src-tauri/src/tools/context.rs:31-80`

GUI 保存的值是 `full`，但 `normalize_tool_profile()` 未识别 `full`，会回退到 `core`。真正暴露全部 P0 Tool 的值是 `advanced`，但 GUI 不提供该选项。

影响：UI、持久化配置、`server_info.tool_profile` 和实际 catalog 语义不一致。

建议：统一为明确的 `core`、`advanced`、`read-only`；或让 `full` 正式映射到 `advanced`。迁移旧配置并增加 catalog 数量测试。

### P2-3：`view_image` 缺少结构化结果的 TextContent 回退

位置：`src-tauri/src/tools/workspace.rs:548-584`。

所有调用都返回 `structuredContent`。普通 Tool 同时返回序列化 JSON TextContent；但 `view_image(output=mcp_image)` 只返回 ImageContent。

稳定规范建议：返回 structuredContent 时，同时提供序列化 JSON TextContent 以兼容不读取 structuredContent 的客户端。

建议：content 同时包含：

1. ImageContent；
2. 仅含 path、mime、尺寸、resize、warning 的 TextContent。

### P2-4：`view_image` 三次复制二进制数据

位置：

- `src-tauri/src/tools/image_tool.rs:70-100`
- `src-tauri/src/tools/workspace.rs:551-584`

成功结果同时包含：

- `structuredContent.base64`
- `structuredContent.data_url`
- `content[].data`

三处均包含同一图像的 base64。最大输入允许 10 MiB，最终 JSON 响应可能膨胀到数十 MiB。

建议：MCP image 模式的 structuredContent 只返回元数据，不返回 base64/data_url；data URL 仅在显式 `output=data_url` 时返回。

### P2-5：本地 Tool 全部缺少 `outputSchema`

位置：`src-tauri/src/tools/registry.rs:503-532`。

`outputSchema` 是可选字段，因此这不是协议违规。但所有本地 Tool 都返回结构化对象，缺少 output schema 会导致客户端无法验证成功/失败 envelope、分页字段、session handle 和操作结果。

建议先定义通用宽松 envelope，再为稳定高频 Tool 增加专用 output schema：

- `server_info`
- `read_file`
- `list_dir` / `list_files`
- `search_text`
- `exec_command`
- `git_status`
- `view_image`

提供 output schema 后，服务器必须在单元测试中验证所有结果符合 schema。

### P2-6：本地 schema validator 不是完整 JSON Schema 2020-12

位置：`src-tauri/src/tools/schema.rs:15-118`。

当前只实现 type、enum、min/max length、minimum/maximum、items、required 和 additionalProperties=false。对当前本地 schema 基本足够，但存在两个契约风险：

- JSON Schema 中 `1.0` 是 integer，当前 validator 仅接受 serde_json i64/u64；
- 未来加入 pattern、format、minItems、oneOf、anyOf、allOf 或 `$ref` 时，会被静默忽略。

建议使用成熟 JSON Schema 2020-12 validator，或在 schema 构建测试中拒绝所有未实现关键字，避免“声明已验证、实际未验证”。

### P2-7：`server_info` 报告的 Tool catalog 不是实际 `tools/list`

位置：

- `src-tauri/src/tools/dispatch.rs:548-568`
- `src-tauri/src/mcp/server.rs:60-92`

`server_info` 只返回 profile 的本地 Tool 名称；实际 `tools/list` 还会：

- 在 Skill 服务关闭时移除 Skill Tool；
- 合并代理 Tool。

因此 `tools` 和 `tool_count` 可能同时多报和少报。

建议复用同一个 catalog builder，并区分 `local_tools`、`proxied_tools`、`effective_tools`。

### P2-8：`write_stdin` annotations 低估潜在副作用

位置：`src-tauri/src/tools/registry.rs:240-247`。

`write_stdin` 被标记为 non-read-only，但 `destructiveHint=false`、`openWorldHint=false`。它可向一个正在运行的任意命令发送输入，输入可能确认删除、发布或网络操作。

建议采用保守 annotations：`destructiveHint=true`、`openWorldHint=true`，或让 session 创建结果记录风险级别并由后续 Tool 继承。

### P2-9：`set_default_cwd` 使用 listener 全局隐式状态

位置：

- `src-tauri/src/tools/context.rs:10-113`
- `src-tauri/src/tools/dispatch.rs:355-421`

`default_cwd` 存储在共享 `ToolContext` 中，不属于 MCP Session。一个客户端调用 `set_default_cwd` 会改变同一 Workspace listener 上其他 Session 后续省略 path/workdir 的行为。

建议：

- 优先移除隐式 cwd，要求每次调用显式 path/workdir；或
- 返回 `cwd_handle`，后续 Tool 显式携带；或
- 至少将 cwd 绑定到 `MCP-Session-Id`。

当前 draft 的状态型 Tool 指南也建议使用显式 handle，避免依赖连接级隐式状态。

### P2-10：代理 catalog 缺少总量和字节预算

位置：`src-tauri/src/mcp/proxy.rs:540-567`。

虽然限制为最多 100 个分页，但每页 Tool 数量和定义大小不受限。恶意或错误的下游可让初始化占用大量内存并产生超大 `tools/list`。

建议设置：最大下游服务器数、每服务器 Tool 数、总 Tool 数、单 Tool definition 字节数和 catalog 总字节数。

## P3 / 前向兼容建议

- `execution.taskSupport` 缺省为 forbidden，当前符合规范；若未来将 `exec_command` 接入 MCP Tasks，再声明 optional/required。
- 不需要为了 draft 立即实现 `x-mcp-header`；敏感参数也不应通过该扩展放入 Header。
- 当前稳定规范的 structuredContent 根类型仍为 object；不要在 SEP-2106 正式进入稳定版本前让本地 Tool 返回数组或 primitive。
- 建议为 Tool metadata 建立单一 `ToolSpec` 结构，消除 `P0_TOOLS`、`CORE_TOOLS`、`READ_ONLY_TOOLS`、`MUTATING_TOOLS` 和 dispatch match 的多份事实源。
- 建议对实际 `tools/list` 生成稳定 digest，并在 CI 中做 snapshot/diff 审查。

## 验证结果

- `tools::registry::tests`：2 passed。
- `tools::schema::tests`：3 passed。
- `mcp::proxy::tests`：6 passed。
- 既有全量回归在上一轮为 220 passed、1 ignored，额外集成/安全/历史测试 72 passed。

现有测试确认了基础结构，但未覆盖以下审计问题：

- compat 档位 annotations 与实际副作用一致性；
- 代理 Tool definition 的 schema/annotation 验证；
- 同名但 schema 变化的代理重连；
- 代理 Tool error 的 `isError` 层级；
- 取消发生在副作用提交后的结果语义；
- 多 MCP Session 的 default cwd 隔离；
- `request_permissions` grant 的 scope/TTL/消费行为；
- view_image 响应体大小和 TextContent 回退。

## 推荐整改顺序

第一批安全阻断项：

1. 删除或实质化 `compat-readonly-all`；
2. 验证并重新分类代理 Tool metadata；
3. 代理重连改为完整 catalog digest；
4. 实现真实 permission grant，或移除 `request_permissions`；
5. 修复写 Tool 的取消后提交语义。

第二批协议与契约一致性：

1. 代理 Tool error 返回 `isError: true`；
2. 动态生成 initialize instructions；
3. 统一 full/core/advanced profile；
4. 修正 view_image content 和 payload；
5. 修正 write_stdin annotations。

第三批可维护性和前向兼容：

1. 引入 outputSchema；
2. 使用完整 JSON Schema validator；
3. effective catalog 单一构建入口；
4. 显式化 cwd 和其他跨调用状态；
5. 增加 catalog snapshot、schema conformance 和代理 fuzz 测试。

## 当前门禁判断

- **本地 core Tool 的 wire format：通过。**
- **read-only Tool 集的实际写入边界：通过。**
- **annotations 真实性：不通过。**
- **代理 Tool catalog 信任边界：不通过。**
- **代理 Tool error 层级：不通过。**
- **真实 permission grant：未实现。**
- **结构化输出强类型契约：部分通过。**
- **多 Session 状态隔离：部分通过。**
