<h1 align="center">Anchor</h1>

<p align="center">
  Turn a local project into a persistent AI development workspace that carries context across conversations.
</p>

<p align="center">
  <a href="https://github.com/orz-i/Anchor/releases/latest"><img src="https://img.shields.io/github/v/release/orz-i/Anchor?label=Release" alt="Latest release"></a>
  <img src="https://img.shields.io/badge/Windows-x64-0078D4?logo=windows" alt="Windows x64">
  <img src="https://img.shields.io/badge/macOS-Apple%20Silicon-000000?logo=apple" alt="macOS Apple Silicon">
  <a href="https://www.apache.org/licenses/LICENSE-2.0"><img src="https://img.shields.io/badge/license-Apache--2.0-blue" alt="Apache-2.0"></a>
</p>

<p align="center">
  <a href="README.md">中文</a> · <a href="README.en.md">English</a> · <a href="https://github.com/orz-i/Anchor/releases/latest">Download latest</a>
</p>

Anchor is a **Rust CLI/daemon + browser Web Admin** workspace gateway. The management frontend uses pnpm, Vite, React, React Router, shadcn/ui, and Tailwind CSS; runtime ownership stays in the `anchor` CLI/daemons. After registering a project and starting its service, an AI agent can read files, edit code, run commands and tests, inspect Git, and preserve development progress through MCP Sessions.

Anchor reads only the current configuration directory and current configuration format. It no longer imports earlier product directories or compatibility formats automatically. Back up existing configuration and re-register any workspaces that must be retained.

## Understand the workflow in 30 seconds

```text
Build or install the anchor CLI
  → start anchor admin
  → add a project directory in the browser Web Admin
  → start MCP and a public tunnel
  → copy the Public MCP URL
  → enable ChatGPT developer mode
  → create an MCP plugin and paste the URL
  → authorize it and start developing in a new conversation
```

For a first connection, remember only this: **the Anchor CLI/daemons own runtime services, the browser Web Admin manages them, and ChatGPT connects through the public `/mcp` URL.**

- [See the complete Web Admin setup](#get-started-in-five-minutes)
- [Go directly to the ChatGPT plugin setup](#mcp-connector)
- [Open the Anchor documentation center](docs/README.md)

## Get started in five minutes

### 1. Build and start Anchor

The current product no longer ships a Tauri desktop installer. Build from source with pnpm and Rust stable:

```bash
pnpm install --frozen-lockfile
pnpm cli:build
./crates/anchor/target/release/anchor admin serve
```

On Windows the executable is `crates/anchor/target/release/anchor.exe`. `anchor admin serve` binds only to loopback and hosts both the same-origin Web Admin and management API.

### 2. Add a project workspace

1. Open the local management URL printed by `anchor admin serve`, then click **Add workspace** in the sidebar.
2. Select the project root directory.
3. Configure the workspace name, MCP port, and authentication mode.
4. Save it. The workspace remains available in the sidebar across conversations and restarts.

### 3. Configure a public tunnel

When the AI client is not running on the same machine, expose MCP through HTTPS:

- Install or detect `frpc` / `cloudflared` from **Software management**.
- Save the server, port, and token under **FRP settings**, or select Cloudflare in the workspace.
- Give each workspace a distinct subdomain. The app manages the FRP process and aggregates multiple proxy routes.

If you do not have an FRPS server yet, follow this [FRPS server installation guide (Chinese, WeChat)](https://mp.weixin.qq.com/s/kmpQhHsvmHlaLfj4rw3A0Q). After deployment, enter the server address, port, and token under **FRP settings** in the Web Admin.

### 4. Start MCP

Open the workspace and click **Start** in the MCP panel. The Web Admin shows:

- a local MCP URL such as `http://127.0.0.1:28766/mcp`;
- the public HTTPS MCP URL;
- authentication details for ChatGPT;
- live logs and health-check results.

The Web Admin can verify the local and public endpoints, OAuth metadata, and the MCP protected-resource document.

When a connection fails, inspect recent MCP requests without leaving the Web Admin.

Health checks report connectivity and authentication metadata separately; logs can confirm whether tool discovery and `session open/checkpoint` calls actually reached the server.

### 5. Connect an AI client

Use the public MCP URL shown by the Web Admin. With OAuth enabled, the client follows the server metadata into the authorization flow; authorization codes, Client IDs, and secrets can be generated and managed from the Web Admin. This release uses preconfigured OAuth clients, so select static/manual OAuth credentials when creating a ChatGPT plugin; CIMD is not required.

For a first connection, open the current Session before inspecting the workspace:

```text
session { operation: "open" }
server_info
cwd { operation: "get" }
git { operation: "status" }
environment { operation: "check" }
```

This gives the agent explicit project and capability state instead of guessing from the current chat window.

## Connect ChatGPT

| Mode | Best for | Use this endpoint |
| --- | --- | --- |
| MCP Connector | Direct access to files, commands, and Git | the workspace's public `/mcp` URL |

### MCP Connector

Before configuring ChatGPT, make sure that:

1. The workspace MCP service and public tunnel are both running.
2. The public MCP endpoint passes the Web Admin health check. If OAuth is enabled, also verify the protected-resource document and authorization metadata.
3. You have copied the **Public MCP URL** from the Web Admin **GPT configuration** card. For OAuth, also have the OAuth Client ID, OAuth Client Secret, and authorization password ready.

> ChatGPT must use the public HTTPS `/mcp` URL. A local address such as `http://127.0.0.1:28766/mcp` is not reachable from ChatGPT. Menu names may vary slightly by ChatGPT version and language.

#### 1. Enable ChatGPT developer mode

Open ChatGPT settings, go to **Account security and sign-in**, and enable **Developer mode**. This allows unverified MCP connectors to be added.

![Enable developer mode in ChatGPT](docs/images/gpt-config-1.png)

*Developer mode grants powerful access. Only connect MCP servers that you operate or explicitly trust.*

#### 2. Create the MCP plugin

Open **Plugins** from the ChatGPT sidebar, click the `+` button, select the MCP beta option, and enter:

| ChatGPT field | Value |
| --- | --- |
| Name | A recognizable name such as `Anchor` |
| Description | A short description of the connected project or purpose |
| Connection | The public MCP URL from the Web Admin **GPT configuration** card; it should end in `/mcp` |
| Authentication | The same mode configured in the Web Admin; the screenshot uses OAuth |

![Create an MCP plugin and enter its connection details](docs/images/gpt-config-2-detail.png)

For OAuth, open the advanced OAuth settings, select static/manual OAuth credentials, and enter the Client ID and Client Secret shown by the Web Admin. CIMD is not required. When ChatGPT opens the authorization page, enter the authorization password from the Web Admin **GPT configuration** card.

> Client Secrets, authorization passwords, and Bearer tokens are sensitive. Never paste them into chats, issues, or public screenshots. If the Web Admin is configured for Bearer or no authentication, select the matching option currently offered by ChatGPT.

#### 3. Verify the connection

Start a new conversation with the plugin enabled and ask:

```text
Use Anchor to call server_info, cwd(operation=get), and git(operation=status).
Tell me which workspace is connected, its default directory, and its Git status.
```

If ChatGPT returns information from the current project, the Anchor runtime, public tunnel, authentication, ChatGPT, and MCP tool chain are connected end to end. Before real development, use the `session` facade with `operation=open` to initialize or restore the current project Session.

If ChatGPT still shows an old tool list, disconnect and reconnect the plugin or verify again in a new conversation.

#### Troubleshooting

| Symptom | Check first |
| --- | --- |
| ChatGPT cannot connect | Confirm that the URL is the public HTTPS `/mcp` endpoint rather than `127.0.0.1`, and that the public MCP health check passes |
| OAuth authorization fails | Confirm that the Client ID, Client Secret, and authorization password come from the same workspace, and check the OAuth metadata results |
| New tools are missing | Disconnect and reconnect the plugin, then start a new conversation |
| A tool call fails | Open **Logs** and **Health checks** in the Web Admin and confirm that the request reached the MCP service |

## Why use it

- **Built for real development**: files, commands, Git, tests, and retained processes live in one Workspace.
- **Cross-conversation continuity**: a new conversation can resume the current Session through the `session` facade and read the latest structured checkpoint without relying on chat history as project state.
- **Auditable progress**: structured checkpoints preserve decisions, changed files, test results, remaining issues, and next steps inside the project.
- **Multiple workspaces**: one browser Web Admin stores multiple projects and manages their MCP and public endpoints.
- **Direct ChatGPT connectivity**: Streamable HTTP, OAuth, Bearer tokens, FRP, and Cloudflare are built in.
- **A focused default tool surface**: stable core tools are available by default; advanced Harness capabilities are opt-in.

## Let the project remember every development Session

Chat transcripts are useful for rereading a discussion, but they are a poor long-term development handoff. Anchor now stores persistent Sessions under `docs/session/`, so context follows the project instead of staying trapped in one chat window. The old `docs/history-session/` directory is a frozen archive; the current Session store does not scan, migrate, or write to it.

In a new conversation, ask the agent to start with `session { operation: "open" }`, read the current Session when needed, and save structured progress with `checkpoint` after a completed unit of work.

The current API is one `session` facade:

| Operation | Purpose |
| --- | --- |
| `open` | Create, resume, or continue the current Session and return a stable `session_id` / `session_path` |
| `checkpoint` | Persist decisions, changes, tests, and next actions to the exact Session target returned by `open`; mismatched targets are rejected |
| `list` | Page through Session metadata from the current `docs/session` store |
| `get` | Read one explicit `session_id` |
| `validate` | Validate the current Session store/index and optionally repair it; the legacy archive is not scanned |

Session documents use readable Markdown and a derived `docs/session/index.json`. Harness treats `docs/session/` as local Session metadata and excludes it from the business Git baseline by default; teams that want to version these records should make that a deliberate repository policy. A checkpoint is only considered saved when its `session_id` and `expected_path` match the active Session target.

> History persistence is performed when the AI calls the MCP tools; the Web Admin does not record chat content in the background. If the client does not invoke a tool, the server cannot infer that a new conversation or task has happened.

## What an agent can do

The default `core` profile provides a stable, composable development tool set:

| Category | Main tools |
| --- | --- |
| File reading | `read_file`, `list_dir`, `list_files`, `search`, `view_image` |
| File modification | `apply_patch` |
| Command execution | `exec_command`, `write_stdin`, `read_output`, `kill_session` |
| Git | the `git` facade: `status`, `diff`, `log`, `show`, `blame`, plus profile-gated mutation/worktree operations |
| Environment | `server_info`, the `environment` facade, and the `cwd` facade |
| Persistent Sessions | the `session` facade: `open`, `checkpoint`, `list`, `get`, `validate` |
| Downstream MCP | the `mcp` facade for lazy discovery; core may call tools and advanced may manage server lifecycle |
| Agent Skills | the `skill` facade for package reads/validation; advanced may install, activate, roll back, and remove packages |

A typical development loop is:

```text
Open Workspace
  → understand project and Git state
  → search and read code
  → apply a transactional patch
  → run commands and tests
  → inspect the diff and commit
```

The advanced profile retains project-state and operation-history Harness capabilities, but normal edits and command execution do not require a Task.

Harness tasks continue to use the configured Workspace by default. When independent branches, indexes, and files are useful, a task can explicitly opt into Git worktree isolation. See [Optional Git worktree tasks](docs/git-worktrees.md).

### Code search and structural analysis

`search` is the unified repository-search entry point with `auto`, `text`, `symbol`, `callers`, `callees`, `impact`, and `explore` modes. `auto` uses deterministic routing rather than an extra LLM classifier: explicit path/regex/glob/context controls select text search, identifier-shaped queries prefer symbol search, and natural-language queries select structural exploration.

The text backend prefers an available ripgrep (`rg`) binary to prefilter candidate files, while Anchor remains responsible for path boundaries, decoding, ordering, context, pagination, and scan budgets. If `rg` is unavailable or prefiltering fails, it falls back to the built-in scanner. The semantic backend invokes CodeGraph internally, lazily initializes a Workspace-local index, and syncs it before querying; index or query failures are surfaced as `degraded` results and fall back to text search. `grep` and the older `search_text` name are retired from the tool protocol and are not hidden aliases in the catalog, OpenAPI schema, or dispatcher; use `search` instead.

Anchor's `software` manager also supports ripgrep and CodeGraph:

```bash
anchor software install ripgrep
anchor software install codegraph
anchor software list
```

CodeGraph is an internal `search` runtime rather than an Agent-facing `exec_command` or `codegraph_*` capability. Anchor disables its telemetry/update checks, serializes index operations, and isolates `.codegraph` as a local runtime artifact. `environment check` reports only `search.text` and `search.semantic` capability state instead of exposing the CodeGraph executable path. Operators can still install, inspect, or uninstall the managed runtime through `anchor software`.

## Permission and recovery model

The project uses a Workspace-first permission model:

- Normal files inside the Workspace can be read, created, modified, deleted, and executed.
- Explicit paths outside the Workspace are rejected by default; read-only access outside the Workspace requires an explicit trusted operator override.
- Writes, deletes, and command execution outside the Workspace are blocked.
- `.git` and `.github` cannot be damaged through ordinary file tools, Patch, or interpreter commands.
- Patch performs preflight validation and operation-local recovery; long-term recovery uses Git instead of full Workspace snapshots.

> Windows child-process execution currently uses a `policy_only` boundary. The honest runtime value is `sandbox_enforced: false`; static command policy is not a complete OS filesystem sandbox.

## Local development

The default development path requires Node.js 20+ and Rust stable. Tauri has been removed from the current product and build dependencies.

```bash
pnpm install --frozen-lockfile
pnpm start
```

`pnpm start` builds the Vite + React Web Admin and launches local `anchor admin serve` through the default CLI target. Release builds use `pnpm release:build` / `pnpm cli:build` and no longer require a desktop installer.

The repository uses `pnpm@11.18.0` as its only Node package manager and lockfile authority. Do not run `npm install` / `npm ci` to create a second dependency state; development, checks, tests, and builds use `pnpm ...` consistently.

Useful verification commands:

```bash
pnpm check
pnpm build
cd crates/anchor && cargo test
cd crates/anchor && cargo clippy --all-targets -- -D warnings
```

The desktop/Tauri shell, installer scripts, and Tauri dependencies have been physically removed. Browser Web Admin is the management UI and `anchor` CLI/daemon owns runtime operations. See [Desktop / Tauri retirement record](docs/desktop-retirement.md).

### Headless Linux CLI

Linux servers can build `anchor` directly. It reads the unified workspace/profile configuration and runs MCP in the foreground:

```bash
pnpm cli:build
./crates/anchor/target/release/anchor list
./crates/anchor/target/release/anchor serve <workspace> --service mcp
```

The Linux CLI also provides built-in daemon operations including `start`, `stop`, `restart`, `status`, `logs`, `doctor`, and `upgrade`. It will not take over a port already owned by another Anchor runtime or external process. For boot-time recovery, prefer Anchor's native `anchor service install` systemd-user control plane instead of putting repeated `restart` commands into shell profiles. See the [Linux CLI guide](docs/linux-cli.md) and [CLI daemon operations guide](docs/cli-daemon.md).

Workspace-level commands include `register`, `unregister`, `show`, `start`, `stop`, `gpt-config`, and `test`, covering profile registration, redacted GPT connection settings, and MCP protocol checks. See the [Workspace CLI guide](docs/workspace-cli.md).

For Windows/Linux migration, do not copy `secrets.json` directly. `anchor export` / `anchor import` (also available as `anchor config export/import`) decrypt on the source platform into a passphrase-encrypted migration bundle, then re-protect secrets with the target platform's native mechanism while preserving Workspace IDs, OAuth client IDs, and authentication secrets. `import` supports `--workspace-path WORKSPACE=ABSOLUTE_PATH` and `--dry-run` for path mapping and validation. See [cross-platform configuration migration](docs/config-migration.md).

Multiple workspaces can share one local Gateway and one public tunnel while remaining separate logical MCP servers at `/w/<workspace-id>/mcp`. Configure it under **Settings → General → Single MCP Gateway**, or use `gateway configure/show/serve` on Linux. See [Single MCP Gateway and multiple workspaces](docs/mcp-gateway.md).

### Request Agent Skills through MCP

Each workspace/profile uses immutable Agent Skill packages stored under `.anchor/skills`. Source directories are only inputs to explicit `validate/install`; the runtime no longer auto-scans `.agents/skills`, `.codex/skills`, or `skills`. Installed versions are assigned to stable/development/canary/pinned channels and become usable only after explicit activation; rollback and protected version removal are supported.

MCP still publishes one `skill` facade: read-only/core can list/get/read_resource/packages/validate, while advanced alone can install/set_channel/activate/rollback/remove. `anchor skill ...` and the Web Admin Agent Skills panel use the same canonical package store. See the [Agent Skill package lifecycle guide](docs/skill-service.md) for lifecycle, protocol, and security boundaries.

### Dynamic MCP, Federation, and Orchestration

- [Dynamic MCP](docs/dynamic-mcp.md) documents the single `mcp` facade, lazy downstream tool discovery, and transactional server lifecycle.
- [Federation](docs/federation.md) documents authenticated + signed read-only connectivity between Anchor Nodes through the existing Gateway; discovery never implies trust and remote write/exec/Harness access remains unavailable.
- [Orchestration](docs/orchestration.md) documents the `anchor-orchestration-v1` read-only DAG planner/inspector for Node, Workspace, and local Harness Task observations, including dependency-wave barriers.

See the [Anchor documentation center](docs/README.md) for the complete documentation map. Topic guides are currently canonical in Chinese when an English translation has not yet been added.

### Reconnection and OAuth renewal

Anchor daemons and CLI detect MCP and tunnel disconnects and recover them with bounded exponential backoff. OAuth now supports one-hour access tokens and rotating 90-day refresh tokens. Read-only status calls may retry automatically; writes and potentially side-effecting tool calls are never replayed blindly.

See [Connection recovery, retries, and OAuth renewal](docs/reliability.md) for behavior and current limitations.

## Project layout

| Path | Purpose |
| --- | --- |
| `crates/anchor/src/tools/` | Shared file, Patch, Exec, and Git tool kernel |
| `crates/anchor/src/mcp/` | MCP Streamable HTTP server |
| `crates/anchor/src/tunnel/` | FRP / Cloudflare tunnel and process management |
| `crates/anchor/tests/` | Rust contract, security, output-schema, and integration tests |
| `src/` | Vite + React + React Router + shadcn/ui + Tailwind CSS Web Admin |
| `docs/` | Product, protocol, architecture, and verification documentation |

## License

[Apache-2.0](https://www.apache.org/licenses/LICENSE-2.0)
