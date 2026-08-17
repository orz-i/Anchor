# CLI / daemon zero-downtime rolling upgrade design (2026-08-17)

## Goal

Upgrade a running Workspace/Gateway daemon generation without creating a listener gap on its stable business port. The existing rollback-safe `anchor upgrade` remains the compatibility fallback; zero-downtime is an additional handoff path used only when the running generation advertises handoff support.

## Non-goals

- This does not download or replace the Anchor CLI binary.
- This does not silently weaken daemon ownership, control-plane authentication, or build-identity checks.
- A pre-handoff daemon that only understands `prepare_restart` continues to use the existing bounded-outage rollout for the one bootstrap upgrade into a handoff-capable build.

## V1 safety boundary

The first Unix Workspace handoff implementation supports either no active Streamable HTTP transport session, or exactly one active transport session when that session owns the still-blocking `exec_command` process that invoked `anchor upgrade`. In the latter case the predecessor snapshots and transfers the MCP session id/protocol/initialization/request-id reservations plus session-scoped cwd, task binding, principal/cursor scope, and command-output cursor state before exporting the listener.

The initiating blocking `exec_command` itself is not migrated: it remains on the predecessor until the upgrade CLI verifies the successor and returns, after which the old request completes and the predecessor can drain. Any unrelated active MCP transport session, concurrent non-initiator request, or already-exposed retained/process-bound command session blocks handoff **before cutover**. This boundary prevents TCP continuity from masking application-state loss while still allowing the normal ChatGPT plugin session to upgrade its own daemon generation.

The predecessor snapshots state before spawning the successor and performs the same preflight plus a full structural snapshot comparison again after the successor reports `successor_prepared`. If any request/session/tool-context mutation occurred during that preparation window, the successor is terminated and the predecessor remains canonical. This closes the race between initial preflight and ownership release without rejecting or freezing normal requests on the old listener.

V1 does not transfer the transport identity of downstream stdio MCP proxy processes. The successor rebuilds its proxy registry using the normal configured startup path, and the handoff preflight rejects non-initiator concurrent Anchor MCP requests so no proxy tool call is deliberately cut over in flight. Durable downstream/browser-process identity, when required by a specific proxy, is a separate proxy-handoff problem and must not be inferred from listener continuity.

## Required invariants

1. **No business-listener gap.** At least one generation owns every selected MCP/Actions/Gateway listener from handoff start until the successor is accepting traffic.
2. **Single canonical owner.** Only one generation owns daemon state, the singleton lock, and the canonical control endpoint at a time.
3. **Old generation remains the rollback authority before cutover.** If the successor cannot import listeners or initialize runtime state, the old generation keeps serving and the upgrade fails without outage.
4. **No silent process-local state loss.** A generation is not allowed to enter cutover while it owns MCP transport/command session state that the successor cannot reconstruct.
5. **Successor becomes canonical before predecessor exits.** After listener readiness, the predecessor stops accepting new work, releases control/lock ownership, and only drains already accepted work while the successor acquires canonical ownership.
6. **Build identity is mandatory.** The successor must report the exact expected `BuildIdentity` before the rollout is considered successful.
7. **Protocol is additive and capability-gated.** Handoff is never sent to a daemon that did not advertise the handoff capability.
8. **Legacy rollback remains available.** If failure happens after canonical cutover, the existing saved rollback executable is still used to restore the previous generation.

## Generation handoff model

The daemon keeps a duplicate of every live TCP listener specifically for upgrade handoff. The serving task owns one socket handle; the supervisor owns a duplicate referring to the same kernel listener.

```text
old generation
  serving listener A
  handoff duplicate A'
        |
        | duplicate/inherit
        v
new generation
  listener A'' ----------> inherited and validated, accept loop parked
        |
        +---- successor-prepared(build identity, pid)

old: publish cutover -> stop accepting -> release control + singleton lock
new: acquire singleton lock -> publish state -> bind canonical control -> activate A''
new: canonical ready; queued/new connections are served by successor
old: exit after bounded drain
```

Because the socket is shared at the kernel level, there is no bind/rebind window and therefore no business-port outage during a successful handoff.

## Platform transport

### Linux / Unix

- Duplicate the live socket with `dup`.
- Clear `FD_CLOEXEC` on the child copy immediately before spawning the successor.
- Pass only the explicit descriptor numbers and a random handoff nonce to the child.
- The child reconstructs `std::net::TcpListener` from those owned descriptors, restores nonblocking mode, and starts the normal Axum servers from the inherited listeners.
- Parent-owned duplicates remain close-on-exec outside the narrow spawn window.

### Windows

- Do not depend on ambient inheritable handles.
- Spawn the successor in handoff-wait mode, obtain its PID, and call `WSADuplicateSocketW` for each live listener.
- Serialize the resulting `WSAPROTOCOL_INFOW` records into the private handoff manifest identified by the nonce.
- The child reconstructs sockets with `WSASocketW` and then starts the same listener-import path.

This keeps the logical state machine identical across platforms while using the OS-supported socket transfer primitive on each platform.

## Private handoff manifest

Handoff coordination lives under the existing private runtime directory and is keyed by an unguessable UUID. The file is mode 0600 on Unix and uses the existing private runtime ACL boundary on Windows.

The manifest contains only process-local coordination data:

- schema version;
- nonce;
- target kind and workspace id / Gateway scope;
- predecessor PID;
- upgrade initiator PID;
- successor PID once spawned;
- expected `BuildIdentity`;
- selected services/routes;
- transferable MCP transport + ToolContext snapshot;
- Windows socket protocol records when the Windows transport is implemented (Unix descriptors are inherited directly by the child process);
- stage: `prepared | successor_started | listener_ready | ownership_released | canonical_ready | failed`;
- bounded error text.

Atomic replacement is required for every stage transition. Stale manifests are ignored unless both nonce and predecessor identity match the active handoff.

## Control protocol

The daemon version response gains a handoff capability field. A capable CLI uses a new asynchronous control operation:

```text
prepare_handoff(workspace_id, initiator_pid, executable_path, expected_build)
  -> operation_id
```

The daemon validates the initiator/session shape and requested executable before cutover. The control response is flushed before command dispatch, preserving the current no-half-response rule. Because the canonical control socket itself changes generations, the invoking CLI follows the operation through the private handoff manifest and then verifies the successor using canonical daemon state plus the new control endpoint/build identity.

For a daemon that does not advertise handoff, `anchor upgrade` automatically uses the current `prepare_restart -> exit -> spawn -> readiness` path and reports the mode as `bounded_outage`.

## Runtime changes

MCP, Actions, and Gateway listener constructors are split into two steps:

1. create/bind listener or accept an imported listener;
2. build state and spawn the serving task.

The runtime supervisor retains one handoff duplicate for each Running service and drops it whenever that runtime is stopped or replaced. Imported listeners bypass normal port-conflict probing because the socket itself proves continuity with the predecessor.

The successor imports and validates business listener descriptors before cutover, but deliberately does not start their accept loops yet. After the predecessor publishes the cutover stage, triggers graceful shutdown of its accept loops, and releases canonical control/lock ownership, the successor acquires that ownership and activates the inherited listeners. The kernel listener itself remains continuously bound/listening throughout this parked interval, so new TCP connections queue rather than observing an unbound port.

## Drain semantics

Cutover stops the predecessor from accepting new requests by triggering normal Axum graceful shutdown. Already accepted HTTP/SSE requests may finish on the predecessor while new requests are accepted by the successor.

The predecessor has a bounded drain deadline. At deadline it aborts remaining server tasks and exits; this bounds upgrade duration without creating a listener outage because the successor is already canonical and accepting traffic.

## Failure matrix

### Before listener readiness

- successor spawn/import/config failure: terminate successor, keep predecessor canonical and serving, report failure with `outageMs=0`;
- manifest corruption/nonce mismatch: fail closed and keep predecessor serving;
- build identity mismatch: terminate successor before cutover and keep predecessor serving.

### After listener readiness but before canonical ownership

- predecessor fails to release control/lock: terminate successor and keep predecessor serving if possible; otherwise fall back to saved rollback executable.

### After canonical ownership

- successor fails readiness/control/build verification: use the existing rollback executable and result model (`rolled_back` / `failed`).

## Observability

`RuntimeRolloutResult` gains:

- `mode`: `zero_downtime_handoff | bounded_outage`;
- `handoff_supported`;
- `handoff_operation_id` when used;
- `listener_ready_ms`;
- `drain_ms`;
- `outage_ms`, which must be exactly `0` for a successful handoff.

Text output explicitly states whether a target used zero-downtime handoff or legacy bounded replacement.

## Delivery slices

1. listener import/export primitives and runtime-held handoff duplicates;
2. Workspace daemon handoff coordination and capability-gated CLI rollout;
3. Gateway daemon handoff using the same primitive;
4. Windows `WSADuplicateSocketW` transport and SCM ownership integration;
5. live regression tests that continuously probe the business endpoint during a generation switch and fail on any refused/failed connection.

## Acceptance

A platform is considered zero-downtime-qualified only when a live test proves all of the following for a real generation replacement:

- predecessor PID differs from successor PID;
- successor build identity matches the invoking CLI;
- at least one request is successfully served before, during, and after cutover;
- no probe observes connection refused / reset caused by listener replacement;
- reported `outageMs` is `0`;
- pre-cutover successor failure leaves the predecessor serving without restart;
- post-cutover failure exercises and verifies the existing rollback path.
