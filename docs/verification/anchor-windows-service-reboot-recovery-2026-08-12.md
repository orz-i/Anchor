# Windows SCM reboot recovery verification — 2026-08-12

## Reproduction

- Installed service: `AnchorControlPlane-7a4577d0aa99`, `state=running`, `autoStart=true`, observed SCM PID `15888`.
- Desired plan still contained both Anchor and Gaoge workspaces after reboot.
- `service status` reported `buildState=unknown` while the Service process itself was alive.
- `windows-service.log` reproduced the boot recovery failure every ~2 seconds. The supervisor aborted before reconciling workspaces because `DataStore::load()` tried to decrypt the user's `secrets.json` under LocalSystem and both primary/backup returned `Windows DPAPI decryption failed`.
- The on-disk secret envelope metadata was `version=1; protection=windows-dpapi-current-user-v1`. Secret payload contents were not printed or inspected.
- The user-visible stale daemon message (`daemon 状态已过期，PID ... 不存在`) was therefore a secondary symptom: the previous daemon state survived reboot, while the SCM reconcile loop never reached the normal stale-state cleanup/spawn path.

## Fix

1. Keep the canonical Windows secret payload protected by CurrentUser DPAPI for user processes.
2. Add a backward-compatible optional LocalMachine DPAPI service mirror to the same envelope. Existing binaries ignore the additional fields and continue reading the unchanged user payload.
3. User-side `DataStore::load()` upgrades a legacy envelope by adding the mirror without changing its existing user ciphertext. GUI UAC and direct CLI service install/start/restart paths provision the mirror before SCM startup.
4. Mark the SCM `service-run` process with `ANCHOR_WINDOWS_SERVICE_CONTEXT=1`; inherited Workspace/Gateway daemon children read and update only the service mirror, preserving the user ciphertext.
   User-side mirror refresh preserves the service-owned `oauth_refresh_replay` app-secret scope so a later GUI/CLI configuration save cannot roll back refresh-token replay state recorded by the service.
5. SCM desired-state reconciliation, plan synchronization, and shutdown enumeration use profiles-only loading and no longer require any secret decryption.
6. Windows service plan/runtime state replacement now uses `MoveFileExW(REPLACE_EXISTING | WRITE_THROUGH)` so a prior boot's state file cannot block publication of the new Service runtime identity.
7. Windows daemon PID ownership additionally checks process creation time against persisted `startedAtUnix`, preventing same-executable PID reuse after reboot from masquerading as the old daemon.

## Verification

- `cargo check --all-targets --all-features`: passed after correcting implementation compile errors; the passing run superseded the earlier failed check.
- `cargo test --all-features --lib -- --test-threads=1`: **545 passed, 0 failed, 1 ignored** before the final replay-state preservation assertion was added; the final full-suite result is recorded below.
- Targeted `service_secret_update_preserves_user_ciphertext`: passed after extending it to verify a later user-side mirror refresh preserves service-owned OAuth replay state.
- `cargo clippy --all-targets --all-features -- -D warnings`: passed.
- Final `cargo check --all-targets --all-features`: passed.
- Final `cargo clippy --all-targets --all-features -- -D warnings`: passed.
- Final `cargo test --all-targets --all-features -- --test-threads=1`: passed after the replay-state preservation change. Two `wait_command` polls encountered upstream HTTP 502 while the retained test process continued; the same command session was consumed to its successful terminal result and was not restarted.
- `pnpm check`: passed with **0 errors / 0 warnings**.
- `pnpm build`: passed.
- Repository-wide `cargo fmt --all -- --check` exposed pre-existing rustfmt drift in unrelated files outside this change set. No unrelated source was reformatted. Strict `rustfmt --check` over every Rust file modified by this fix passed and superseded that format verification failure.
- `git diff --check`: passed; Git only reported the repository's existing Windows `core.autocrlf` LF→CRLF warnings for touched Rust files.
- Added regression coverage for user/service secret round-trip, legacy envelope service-mirror migration, service writes preserving user ciphertext and service runtime replay state, Windows Service runtime-state replacement, and daemon PID creation-time matching.

## Deployment boundary

The live installed SCM instance used for reproduction is still the pre-fix build while this isolated worktree is being validated. Do not claim the reboot path is live-fixed until the new build is installed/updated through the SCM service update path and a real reboot/autostart smoke confirms Workspace ports and control pipes recover without manual startup.
