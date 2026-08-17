# CLI / daemon zero-downtime rolling upgrade design (2026-08-17)

## Goal

Upgrade a running Workspace/Gateway daemon generation without creating a listener gap on its stable business port. The existing rollback-safe `anchor upgrade` remains the compatibility fallback; zero-downtime is an additional handoff path used only when the running generation advertises handoff support.

## Non-goals

- This does not download or replace the Anchor CLI binary.
- This does not silently weaken daemon ownership, control-plane authentication, or build-identity checks.
- A pre-handoff daemon that only understands `prepare_restart` continues to use the existing bounded-outage rollout for the one bootstrap upgrade into a handoff-capable build.

## Required invariants

1. **No business-listener gap.** At least one generation owns every selected MCP/Actions/Gateway listener from handoff start until the successor is accepting traffic.
2. **Single canonical owner.** Only one generation owns daemon state, the singleton lock, and the canonical control endpoint at a time.
3. **Old generation remains the rollback authority before cutover.** If the successor cannot import listeners or initialize runtime state, the old generation keeps serving and the upgrade fails without outage.
4. **Successor becomes canonical before predecessor exits.** After listener readiness, the predecessor stops accepting new work, releases control/lock ownership, and only drains already accepted work while the successor acquires canonical ownership.
5. **Build identity is mandatory.** The successor must report the exact expected `BuildIdentity` before the rollout is considered successful.
6. **Protocol is additive and capability-gated.** Handoff is never sent to a daemon that did not advertise the handoff capability.
7. **Legacy rollback remains available.** If failure happens after canonical cutover, the existing saved rollback executable is still used to restore the previous generation.

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
  listener A'' ----------> starts accepting before cutover
        |
        +---- ready(build identity, pid)

old: stop accepting -> release control + singleton lock -> drain accepted work
new: acquire singleton lock -> publish state -> bind canonical control -> canonical ready
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
- successor PID once spawned;
- expected `BuildIdentity`;
- selected services/routes;
- transferred listener descriptors or Windows socket protocol records;
- stage: `prepared | successor_started | listener_ready | ownership_released | canonical_ready | failed`;
- bounded error text.

Atomic replacement is required for every stage transition. Stale manifests are ignored unless both nonce and predecessor identity match the active handoff.

## Control protocol

The daemon version response gains a handoff capability field. A capable CLI uses a new asynchronous control operation:

```text
prepare_handoff(workspace_id, executable_path, expected_build, timeout)
  -> operation_id
operation_status(operation_id)
  -> pending/running/succeeded/failed + successor_pid
```

The daemon validates the requested executable before spawning it. The control response is flushed before command dispatch, preserving the current no-half-response rule.

For a daemon that does not advertise handoff, `anchor upgrade` automatically uses the current `prepare_restart -> exit -> spawn -> readiness` path and reports the mode as `bounded_outage`.

## Runtime changes

MCP, Actions, and Gateway listener constructors are split into two steps:

1. create/bind listener or accept an imported listener;
2. build state and spawn the serving task.

The runtime supervisor retains one handoff duplicate for each Running service and drops it whenever that runtime is stopped or replaced. Imported listeners bypass normal port-conflict probing because the socket itself proves continuity with the predecessor.

The successor starts imported business listeners before acquiring canonical daemon ownership. It does not bind the normal daemon control endpoint until the predecessor explicitly releases ownership.

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
