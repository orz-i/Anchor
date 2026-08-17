# Anchor CLI/Daemon Zero-Downtime Upgrade Verification — 2026-08-17

## Qualified scope

This verification closes the first production-safe zero-downtime handoff scope:

- Linux/Unix Workspace daemon generations.
- MCP/Actions business listeners are transferred by inherited listener descriptors rather than unbinding and rebinding the service port.
- The daemon advertises `zero_downtime_handoff_v1`; callers use the handoff path only when that capability is present.
- A Workspace with managed tunnels continues to use the existing rollback-safe bounded-outage path because tunnel ownership transfer is not part of V1.

Gateway daemon handoff and Windows/SCM `WSADuplicateSocketW` transport are **not** zero-downtime-qualified by this slice. Those targets keep their existing bounded-outage upgrade semantics and are not advertised as V1 zero-downtime handoff targets. This is an intentional capability boundary, not an implicit claim of cross-platform zero downtime.

## Safety model verified

The predecessor retains canonical ownership until the successor has imported and validated the inherited listeners. Before ownership release, the predecessor revalidates the transferable MCP snapshot. Unrelated active transport sessions, concurrent non-initiator requests, or process-bound retained command state block cutover rather than silently losing process-local state.

After cutover, the successor must acquire the canonical daemon lock/control endpoint and report the expected build identity. Failure before cutover leaves the predecessor canonical and serving. Failure after cutover exercises rollback to the previous generation. Existing bounded-outage fallback remains available when the V1 capability or prerequisites are absent.

## Regression evidence

Final verification on the feature worktree completed with:

- `cargo fmt --all -- --check` — passed.
- `cargo clippy --no-default-features --features cli --lib -- -D warnings` — passed with zero warnings.
- `cargo test --no-default-features --features cli --test cli_upgrade -- --test-threads=1` — **9 passed, 0 failed**.
- `git diff --check` — passed.

The CLI upgrade suite covers:

- real Linux generation replacement with continuous business-endpoint probes and no failed probe during handoff;
- failure before cutover preserving predecessor PID/canonical ownership and service continuity;
- failure after cutover restoring the previous generation;
- active MCP transport-session quiescence rejection before cutover with `outageMs = 0`;
- explicit target/preflight and JSON error-contract behavior;
- stopped Gateway dry-run safety.

Earlier handoff-core verification also passed listener duplication/import tests, synchronous port-conflict regression, RuntimeSupervisor regression, MCP Gateway listener regression, Clippy, formatting, and diff checks.

## Delivery commits

- `0d7784bf74a19d04b57c4f248a5ad52aea0beabb` — zero-downtime design and invariants.
- `1ecfdad725dc9f537ad29a581798689a08d20d4a` — listener handoff core.
- `406f52086fa09aa2597973752c4d90562cda6034` — Workspace daemon/CLI zero-downtime handoff integration and regression coverage.

## Operational note

The managed feature worktree accumulated roughly 7.4 GB, almost entirely under `src-tauri/target`. Once this branch is integrated and the Harness task is closed, removing the managed worktree is the preferred safe cleanup because it reclaims the build artifacts together with the isolated checkout without deleting tracked source files from the primary checkout.
