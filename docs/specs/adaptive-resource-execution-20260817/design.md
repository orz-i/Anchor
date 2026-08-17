# Adaptive resource execution governance

## Problem

Anchor currently protects command retention, but not host CPU saturation. `CommandSessionStore`
allows up to 64 retained execution sessions and `exec_command` starts the child before the store
rejects an over-capacity session. MCP can also deliver several tool calls concurrently. A CPU-heavy
command such as `cargo build`, `cargo test`, `pnpm build`, `make`, or `ninja` may itself fan out to
all visible CPUs, so a small number of concurrent calls can make a low-resource host unusable.

Workspace daemons are independent processes. A per-daemon semaphore alone is therefore
insufficient when several workspaces are active on the same machine.

## Goals

- Derive a conservative execution budget from the resources visible to the daemon.
- Preserve at least one CPU for the operating system when more than one CPU is available.
- Bound the number of simultaneously running child commands before child creation.
- Serialize known CPU-intensive commands across Anchor workspace daemons in the same user data
  scope.
- Propagate parallelism caps to common build/test runtimes so one child cannot immediately consume
  every CPU.
- Lower the scheduling priority of arbitrary exec children as a final responsiveness guard.
- Expose the effective policy through environment diagnostics and command-session results.
- Keep command-result retention semantics separate from live execution capacity.

## Effective CPU budget

The governor uses `std::thread::available_parallelism()` as the portable baseline. On Linux it also
reads cgroup v2 `cpu.max` or cgroup v1 CFS quota when present and uses the lower effective value.

Default execution CPU target is 75%. For hosts with more than one effective CPU the result is also
clamped to `effective_cpus - 1`, guaranteeing one logical CPU of headroom. A one-CPU environment
uses one execution slot because no smaller integral CPU budget is possible.

The live command limit is derived from that CPU budget and, when detectable, total/cgroup memory.
It is intentionally small (1-4 by default). This is a running-process limit, not a retained-session
limit.

Operator environment overrides may make the defaults stricter; CPU percentage remains bounded so
the automatic headroom rule cannot be disabled accidentally.

## Cross-daemon heavy-command lease

Known CPU-intensive commands acquire an exclusive file lock under the shared Harness data root
before spawn. OS file locks automatically recover after daemon crashes, while custom Harness roots
keep tests isolated. Lightweight commands do not take this global lock.

The lease is held for the child lifetime and released as soon as terminal status is observed. A
background timeout monitor continues to refresh detached commands, so an exited child cannot retain
the heavy-command lock for the output-retention window.

## Child parallelism and priority

Every exec child receives conservative inherited caps for common runtimes, including Cargo/Rust
tests, Rayon, Go, CMake, libuv, OpenMP, OpenBLAS, MKL, and NumExpr. Explicit lower numeric limits are
preserved; higher values are clamped to the governor budget. Direct `make`/`gmake`/`ninja` job flags
are normalized for known heavy invocations so command-line flags cannot bypass the cap.

Exec children run below normal priority: Unix children receive a positive nice value before exec;
Windows children use `BELOW_NORMAL_PRIORITY_CLASS` together with the existing no-console flag.
Descendants normally inherit that scheduling priority.

## Observability

`environment check` reports detected CPUs, optional memory/cgroup limits, reserved CPUs, effective
execution CPU budget, maximum running commands, heavy-command parallelism, queue timeout, and the
cross-daemon serialization policy. Command session snapshots carry the allocation used for that
specific child.

Resource queue rejection is retryable and must occur before child spawn. It is not a workspace
mutation and must not create a recovery requirement.
