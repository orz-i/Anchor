# Anchor CLI / daemon rolling-upgrade verification — 2026-08-17

## Scope

This verification covers the bounded rolling-upgrade implementation introduced on
`feature/rolling-upgrade-20260817`.

The implementation deliberately does **not** claim zero downtime. Workspace and
Gateway listeners retain fixed ports, so the supported contract is:

1. preflight every requested target before mutating any runtime;
2. preserve a trustworthy rollback executable when possible;
3. send the existing cross-version `prepare_restart` lifecycle drain;
4. wait for the old generation to release its runtime ownership;
5. start the current Anchor executable and require readiness plus matching
   `BuildIdentity`;
6. if the new generation fails, restore the previous executable and previous
   Workspace service/tunnel or Gateway route selection;
7. report `planned`, `already_current`, `upgraded`, `rolled_back`, or `failed`
   explicitly. `rolled_back` is not reported as success by the CLI.

On Linux, the old running image is copied from `/proc/<pid>/exe` before drain, so
rollback remains possible even when the on-disk `anchor` binary has already been
replaced. On Windows, an installed/running SCM supervisor remains the single
runtime authority; the ordinary CLI must not race the supervisor to replace a
managed generation.

## Implemented surfaces

- `src-tauri/src/rollout.rs`
  - Workspace/Gateway rollout state machine;
  - preflight, BuildIdentity comparison and readiness verification;
  - Linux rollback-image preservation and cleanup;
  - explicit executable launch paths for rollback generations;
  - bounded outage duration and rollback result reporting.
- `src-tauri/src/daemon.rs` and `src-tauri/src/gateway_daemon.rs`
  - launch a daemon generation from an explicit executable while preserving the
    existing ownership/readiness checks.
- `src-tauri/src/windows_service.rs`
  - owner-token child launch accepts an explicit executable, keeping SCM-managed
    generations under the same identity boundary.
- `src-tauri/src/cli/upgrade.rs` and `src-tauri/src/cli/args.rs`
  - `anchor upgrade` target selection, `--all`, `--gateway`, `--dry-run`,
    timeout/force controls and optional `--allow-no-rollback` override;
  - all requested targets are preflighted before the first runtime is drained;
  - duplicate Workspace aliases are deduplicated before rollout.
- `docs/cli-daemon.md` and `docs/cli-daemon-roadmap.md`
  - document the bounded-downtime contract and remaining zero-downtime boundary.

## Verification evidence

### Live CLI rollout integration

Command:

```text
cargo test --no-default-features --features cli --test cli_upgrade -- --test-threads=1
```

Final result after the Clippy cleanup: **6 passed / 0 failed**.

Coverage includes:

- an actual temporary-config Workspace daemon generation on Linux;
- simulated stale BuildIdentity followed by `anchor upgrade`;
- PID generation replacement and current BuildIdentity readiness;
- cleanup of the temporary Linux rollback executable;
- Workspace selector alias deduplication;
- safe Gateway dry-run while stopped;
- invalid target combinations and JSON error shape.

### Adjacent integration regression

Command:

```text
cargo test --no-default-features --features cli \
  --test cli_upgrade \
  --test call_tool_contract \
  --test call_tool_security \
  --test config_migration_cli \
  --test harness_state \
  --test harness_tool_contract \
  --test session \
  --test tool_output_schema_contract \
  -- --test-threads=1
```

Result: **162 passed / 0 failed** across the eight integration targets.

### Rust format / Clippy

- `cargo fmt --all -- --check` — passed.
- `cargo clippy --no-default-features --features cli --lib` — passed.
- The first Clippy pass found three warnings introduced by `rollout.rs`
  (`too_many_arguments` and two `needless_return` findings). They were removed
  and the final Clippy pass contains no rolling-upgrade-specific warning.
- The final output still contains the repository's existing Linux conditional
  warnings (12 Clippy warnings, including the existing `src/tools/exec.rs`
  `needless_return`). These predate this slice and were not hidden or waived.

### Frontend / repository build checks

- `pnpm check` — passed; `svelte-check` reported 0 errors and 0 warnings.
- `pnpm build` — passed and generated the static site.
- Both commands still print the repository's existing transient Vite/esbuild
  warning about `.svelte-kit/tsconfig.json` before SvelteKit sync/build completes.

### Platform coverage

`rustup target list --installed` reported only:

```text
x86_64-unknown-linux-gnu
```

Therefore the Windows-specific SCM code path was reviewed and kept behind the
existing platform boundary, but **was not compiled or live-tested on Windows in
this Linux worktree**. Windows SCM update/owner-token rolling behavior remains a
Windows CI / real-machine release acceptance item.

## Release-build interruption after Anchor service restart

`pnpm cli:build` was started as the final release-build verification. During that
build the Anchor MCP transport/service restarted and the retained command session
disappeared before a terminal verification record was persisted. The build is
therefore **not counted as passed**.

After reconnect, `environment check` reported that the restarted Anchor daemon's
execution environment could no longer resolve `node`, `pnpm`, `cargo`, `rustc`,
or `rustup`, although the worktree and `node_modules` remained intact. A retry of
`pnpm cli:build` was rejected before execution with `Program not found on PATH`.
Model-supplied `PATH` is correctly protected by Harness policy, and Docker was not
available through the active command allowlist, so no policy bypass was used.

This is recorded as an execution-environment limitation rather than a code-build
failure. Production/headless compilation had already passed via
`cargo check --no-default-features --features cli --lib`, the real CLI integration
binary was built and exercised by `cli_upgrade`, and the adjacent 162-test suite
passed before the service restart.

## Remaining release acceptance

Before declaring package-level rolling-upgrade support fully release-qualified:

1. run `pnpm cli:build` again from a normal host/user toolchain environment and
   retain its terminal success evidence;
2. run Windows build/CI coverage for the explicit-executable SCM owner-token path;
3. execute a real Windows SCM update with active Workspace/Gateway desired state;
4. optionally add a stable external proxy/socket handoff layer in a future phase
   if true zero-downtime listener replacement is required.

The current feature is therefore a **rollback-safe, bounded-downtime controlled
upgrade**, not a zero-downtime socket handoff implementation.
