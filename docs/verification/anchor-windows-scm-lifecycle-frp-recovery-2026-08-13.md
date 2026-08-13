# Windows SCM lifecycle / FRP recovery verification — 2026-08-13

## Scope

This follow-up started from an apparent MCP-session termination observed while querying Windows SCM status after the owner-token security fix. The investigation separated transport symptoms from actual process lifecycle and then fixed the independent state/recovery defects exposed by the live system.

## Findings

### `service status` does not terminate the Workspace daemon

- Repeated `anchor --json service status` calls completed successfully while the SCM supervisor stayed on PID 21904 and the Workspace listener remained healthy.
- Long compile + status probes (roughly 24–39 seconds) also completed without changing the Workspace daemon PID.
- During `pnpm desktop:build`, one `wait_command` request returned a transient `Session terminated`/connector error, but the same Anchor server later reported the original build command as still running in its retained-session registry. The build subsequently completed successfully. This proves the process survived and the symptom was a transport/request interruption under build load, not an SCM-triggered daemon shutdown.

### SCM build identity was falsely reported as `unknown`

- `sc queryex` reported the LocalSystem service as RUNNING with PID 21904, but ordinary-user status code called `OpenProcess` through `is_process_alive`/`process_image_path` and treated access denial as a dead process.
- Status validation now treats the SCM `queryex` RUNNING/PID result as authoritative for liveness and uses the executable registered by `sc qc` to validate the service runtime record.
- A fallback process probe remains only for the unexpected case where SCM registration data cannot be parsed.
- Status now exposes an optional `runtimeIssue` reason instead of silently discarding invalid runtime evidence.
- Live patched CLI verification changed the same running service from `buildState=unknown` to `buildState=current` and returned the complete runtime build identity for b024610.

### Windows Named Pipe ERROR_NO_DATA / ERROR_BROKEN_PIPE noise

- Workspace and Gateway control listeners can receive Windows error 232 (`ERROR_NO_DATA`) or 109 (`ERROR_BROKEN_PIPE`) when a one-request client closes immediately after the exchange.
- These two peer-close errors are now suppressed only at log emission. Request failure semantics are unchanged: a failed response write still fails the request and does not become permission to dispatch a write command.
- Other I/O failures continue to be logged.

### Legacy LocalSystem `frpc` processes survived the owner-token migration

- Live process inspection found three `frpc` processes: one readable as the config owner and two protected from the ordinary user.
- Anchor and Gaoge FRP logs independently showed continuous `proxy ... already exists` errors for their stable proxy names. The two protected processes therefore matched the two SCM-managed workspaces and explained why the owner-token daemons could not re-establish their routes.
- The old FRP candidate path wrote `frpc.pid` immediately after spawn. If the candidate then failed readiness (for example because the old proxy already existed), it cleared the PID file and lost the only durable PID/image ownership record that could have been used for safe recovery.

## Fix

- Commit a managed FRP PID/image record only after all requested proxies become ready. Failed replacement candidates no longer overwrite or clear the previous durable ownership record.
- Add an exact-image process enumeration primitive to the platform layer. Windows reuses the existing process snapshot and executable-image validation logic.
- On SCM supervisor startup, LocalSystem performs a one-time migration cleanup for `frpc` processes that satisfy both conditions:
  1. the executable image exactly matches an Anchor-managed/resolvable FRP candidate; and
  2. the process SID differs from the trusted config-owner SID stored in the administrator-protected SCM registration.
- Correct-owner FRP processes are never killed by this migration cleanup. Arbitrary `frpc.exe` images outside the managed candidate set are also untouched.
- The existing owner-token boundary remains unchanged: Workspace/Gateway developer daemons run as the config owner; LocalSystem is used only for SCM supervision and the narrow legacy-child cleanup it is uniquely privileged to perform.

## Verification

- Windows Service targeted tests: passed (SCM parsing, owner validation, runtime registration validation, service build state).
- Workspace/Gateway Named Pipe peer-close regression: 2 passed, 0 failed.
- Tunnel/FRP targeted tests: 35 passed, 0 failed.
- `pnpm check`: 0 errors, 0 warnings.
- `cargo test --all-targets --all-features -- --test-threads=1`: passed.
- `cargo clippy --all-targets --all-features -- -D warnings`: passed.
- Task-scoped rustfmt check with `skip_children=true`: passed.
- `git diff --check`: passed; only the repository's existing LF/CRLF conversion warnings were emitted.
- `pnpm desktop:build`: passed and produced the release EXE, MSI, NSIS installer and `anchor-build-manifest.json`.
- Patched CLI live status against the still-installed b024610 SCM instance returns the complete runtime record (PID 21904, executable `D:\Program Files\Anchor\anchor-desktop.exe`) instead of `unknown`. Before the final source edits it matched as `current`; with this task's uncommitted source changes the debug CLI correctly reports `different` because its `currentBuild.gitDirty=true` while the installed service is the clean b024610 build.

## Deployment boundary

The current installed SCM service still runs the previously installed b024610 binary. The source/bundle fix is verified, but the new privileged legacy-FRP cleanup executes only after the newly built desktop package is installed and the SCM supervisor is restarted. This verification does not bypass Windows UAC or directly overwrite Program Files from the MCP workspace process.
