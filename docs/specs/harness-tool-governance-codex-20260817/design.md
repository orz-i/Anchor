# Harness Tool Governance — Codex-aligned round (2026-08-17)

## Goal

Tighten Anchor's public tool contracts and Harness failure lifecycle without
growing the tool surface. The round keeps Catalog 37's domain-facade model,
but removes two sources of avoidable agent friction found during live use:

1. facade schemas expose the flattened union of every operation argument, so
   callers can select one operation and still be shown arguments that the
   delegated canonical tool rejects;
2. a command that never starts and never mutates the workspace can still open
   a durable Task Recovery, turning environment probes and command discovery
   into task-level recovery work.

## Codex principles used as reference

OpenAI's public Codex material describes a harness where shell/file tools,
MCP, and skills participate under one policy model; sandboxing defines the
technical execution boundary, while approval policy decides when an action
must stop for review. The App Server description also keeps thread lifecycle,
tool execution, extensions, and persistence inside one harness runtime.

For Anchor, the directly transferable principles are:

- public tool contracts should describe the action the agent is actually
  allowed to take, rather than depending on a later hidden rejection;
- preflight/control-plane failures must be distinct from failures of actions
  that actually executed or mutated state;
- recovery should protect uncertain or failed work, not become a generic error
  log for harmless discovery attempts;
- audit metadata must remain available even when a failure is intentionally
  excluded from durable Recovery.

OS-enforced command sandboxing is intentionally not implemented in this
slice. Anchor currently reports `execution_boundary=policy_only` and
`sandbox_enforced=false`; replacing that with Landlock/seccomp/Windows sandbox
enforcement is a separate cross-platform security project and must not be
simulated by metadata changes.

## Design

### 1. Operation-aware facade contracts without adding tools

Keep the ChatGPT-compatible top-level object schema (no `oneOf`, `anyOf`, or
`$ref`) and the existing flattened call shape:

```json
{"operation":"show","rev":"HEAD"}
```

Improve it in two compatible ways:

- every flattened property records which facade operations accept it;
- delegated validation failures are wrapped as a facade-level error that
  returns `allowed_arguments`, `required_arguments`, and the canonical leaf
  validation error.

This preserves the low tool count while making the operation boundary visible
to the model and actionable when a cached/ambiguous schema still causes a bad
call.

### 2. Recovery eligibility is execution-aware

Durable Task Recovery is for a failed logical action, not every tool error.
For tracked tools, a failed attempt is not recovery-eligible when all of the
following are true:

- no workspace mutation was attributed;
- no verification failure was recorded;
- the caller did not provide an explicit retry identity;
- execution is explicitly known not to have started.

`exec_command` errors that occur before a process starts must therefore be
normalized with `execution_started=false` before recovery classification.
Commands that start and fail, mutate before failing, or produce blocking
verification evidence remain recovery-eligible.

### 3. Catalog boundary

The public names and operation call shapes do not change. Schema descriptions
and facade error semantics do change, so the catalog version must advance to
force clients that cache tool definitions to refresh.

## Acceptance checks

- `git status` schema identifies only the operations that accept each optional
  property, including `include_ignored`.
- a mismatched facade argument returns an operation-scoped error containing
  allowed and required argument lists.
- `exec_command` command discovery for a missing program reports
  `execution_started=false` and does not create Task Recovery.
- a command that actually starts and exits non-zero remains recovery-eligible.
- profile-specific facade operation filtering remains unchanged.
- Catalog count does not increase.
