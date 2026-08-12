# Windows SCM owner command context verification — 2026-08-12

## Reproduction

- Live Anchor build at investigation start: `4859deedad1733b34b914d75dee826907503c620`.
- Git repository owner is the configured desktop user, while the live MCP/command process reports the current Git identity as `NT AUTHORITY\\SYSTEM`; Git therefore emitted `detected dubious ownership` for `D:/anchor`.
- Runtime environment diagnostics found system Git/Node/Go, but direct `pnpm`, `cargo`, `rustup`, `rustc`, Clippy and rustfmt were not available from the daemon environment. `corepack pnpm` remained available because Corepack is installed with the system Node distribution.
- Attempts to temporarily inject the desktop user's `PATH`/`CARGO_HOME`, or directly execute an absolute binary under the user's profile, were correctly rejected by command policy before execution. This task did not add a wrapper or Git trust bypass.
- A later read-only audit found an existing `safe.directory=.` entry in `C:/Windows/system32/config/systemprofile/.gitconfig`. The source fix does not rely on it, and this workspace-scoped task does not mutate the SYSTEM profile outside the repository. Remove that legacy/runtime workaround separately after the updated user-context daemon is deployed.

## Root cause

The Windows SCM supervisor runs as LocalSystem. Its reconcile path called the ordinary `daemon::spawn_with_tunnels` / `gateway_daemon::spawn` helpers, both of which used `std::process::Command` and therefore inherited the supervisor token and environment. The persisted owner SID/username was previously used only for Named Pipe identity/ACL compatibility; it did not control the child process token.

This made the privilege boundary incorrect for developer commands: Workspace commands and downstream MCP processes were hosted by a SYSTEM daemon rather than by the configuration owner.

The first owner-token live test also exposed a release-only dispatch gap: owner children were intentionally launched as `--config-dir <path> daemon-run ...`, but `anchor-desktop.exe` only bypassed Tauri when the first argument itself was an internal daemon command. The owner-token process therefore entered the desktop GUI path and timed out before publishing daemon state. The desktop pre-dispatch now skips the supported global `--config-dir <path>` and `--json` options before checking for the four internal entrypoints.

## Fix

- Keep the SCM supervisor as LocalSystem so it can participate in boot recovery and machine-scoped service secret handling.
- When the caller is the SCM service context, route Workspace/Gateway child creation through an owner-token launcher instead of `std::process::Command`.
- At Service install/update time, capture the configuration owner's SID/username before the GUI UAC hop and persist them in the administrator-protected SCM `ImagePath`. Direct elevated CLI install uses its current Windows identity. The user-writable `windows-service.json` is not authoritative for impersonation.
- Read the current owner SID directly from the Windows process token rather than resolving `whoami.exe` through a user-controlled `PATH`, so the privilege identity captured before UAC is not command-resolution dependent.
- On `service-run`, require the owner SID/username from the SCM registration, validate them, and publish them only into the LocalSystem supervisor environment. Legacy registrations without these fields fail closed with an explicit install/update requirement.
- Reject well-known Windows service-account SIDs (`LocalSystem`, `LocalService`, `NetworkService`) as configuration owners so a direct CLI install performed from a service context cannot silently recreate the same privilege bug.
- Enumerate Active Windows sessions, obtain their primary tokens, read each token SID and require an exact match with the SCM-registration owner SID.
- Create the child environment from that session-bound user token and start the daemon with `CreateProcessAsUserW` under the owner's primary token. The daemon is headless (`CREATE_NO_WINDOW`); the internal child command receives an explicit `--config-dir` so it remains bound to the intended configuration domain.
- Windows daemon children redirect stdout/stderr to their canonical daemon log at the internal child entrypoint, before config loading. Managed spawn paths always pass the canonical workspace id, so startup/config failures remain observable without passing LocalSystem-owned inheritable log handles across the user-token boundary.
- The desktop binary recognizes internal daemon commands after supported global options, so an explicit owner-context `--config-dir` still reaches the CLI daemon loop before Tauri initialization. Ordinary desktop/CLI commands are not added to this bypass.
- If no Active session matches the configured owner, fail closed and let the SCM reconcile loop retry later. Never fall back to a LocalSystem Workspace/Gateway child.
- On every SCM reconcile, inspect any already-running Workspace/Gateway daemon process token before adopting it. If its SID differs from the trusted SCM-registration owner SID (for example, a legacy SYSTEM child surviving a supervisor restart), coordinate a restart first and only then enter the owner-token spawn path.
- Normal GUI/CLI daemon starts outside service context keep the existing current-user `std::process::Command` path.

## Security properties

- Git ownership now aligns with the account that owns the checkout once the updated daemon is running.
- User-scoped tool resolution is restored by the user's environment rather than by model-controlled `PATH` injection or wrapper scripts.
- SCM retains only the narrow supervisor privileges it needs; remote developer command execution no longer intentionally inherits LocalSystem.

## Verification status

- `corepack pnpm desktop:build` passes from the desktop-user toolchain and produces the release EXE, MSI, NSIS installer and `anchor-build-manifest.json`.
- Windows Service/CLI regression target: 11 passed, 0 failed. The desktop internal-dispatch regression target also passes.
- Full `cargo test`: library 554 passed, 1 ignored; all CLI, integration, contract, security, Harness, History, output-schema and doctest targets pass.
- `cargo clippy --all-targets --all-features -- -D warnings` passes.
- Task-scoped rustfmt check passes. The repository-wide `cargo fmt --all -- --check` still reports pre-existing formatting drift in unrelated clean files; those files were not reformatted into this security fix.
- `corepack pnpm check` passes with `svelte-check found 0 errors and 0 warnings`.
- The updated SCM registration is installed as automatic `LocalSystem` service `AnchorControlPlane-7a4577d0aa99`. Its protected `ImagePath` contains the config directory plus owner SID `S-1-5-21-1541436298-886280848-1540978344-1001` and username `mouta`.
- Live parent/child evidence after the final update: SCM supervisor PID 14948 runs as LocalSystem; direct Workspace child PID 22480 runs as `PC\\mouta`, uses the final `anchor-desktop.exe`, publishes state schema 2, and owns MCP port 28766. The configured owner SID exactly matches the desktop user's process-token SID.
- The live MCP `server_info` call succeeds through that child. Its `environment check` resolves user-level `pnpm` from `C:\\Users\\mouta\\AppData\\Roaming\\npm`, `rustup` from `C:\\Users\\mouta\\.cargo\\bin`, and cargo/rustc/Clippy/rustfmt from the user's stable Rust toolchain; all probes are healthy. MCP Git status also succeeds without `dubious ownership`.
- A direct MCP `whoami` probe remains correctly blocked because `whoami` is not in the workspace command allowlist. A permitted `exec_command` probe is currently gated by an existing Harness baseline mismatch across four retained tasks; this validation did not rewrite those unrelated task baselines merely to bypass governance.
- The SYSTEM-profile historical `safe.directory=.` entry was not used or modified.
