# Apply Patch terminal-response investigation

## Reported symptom

An older plugin build was applying a multi-file patch during Slice 8.4. The `apply_patch` call did not return a result, and the upstream agent session moved directly to its final response.

The reported plugin was not the latest build, so the historical incident cannot be replayed from the current binary. There is also no terminal tool result identifying the exact phase where the old call stopped.

## Confirmed risk in the pre-fix implementation

The local `apply_patch` implementation was a synchronous `spawn_blocking` task. It had:

- a 200 KB patch-input policy limit;
- transactional post-image validation and atomic file replacement;
- an outer MCP request timeout of 90 seconds;
- no patch-specific timeout;
- no cooperative cancellation checks;
- no deadline checks inside exact, fuzzy, or nearest-context hunk searches.

This meant a patch worker could remain busy after the upstream client had already abandoned the request. The global 90-second MCP timeout was also long enough for an upstream host with a shorter turn/tool timeout to end the session first.

The available evidence cannot distinguish among these possible triggers in the old incident:

1. expensive mismatch or fuzzy-context search;
2. a slow or blocked filesystem read/write/rename;
3. transport interruption before the terminal response was delivered;
4. an upstream tool-call timeout shorter than Anchor's global timeout.

## Catalog v23 behavior

`apply_patch` and `patch_check` now have two bounded termination layers.

### Patch execution deadline

- default: 20,000 ms;
- configurable with `timeout_ms`;
- allowed range: 1,000–60,000 ms;
- cancellation/deadline checks run during parsing boundaries, per-file preparation, exact/fuzzy hunk search, nearest-context diagnostics, validation, temporary-file staging, and atomic commit;
- cancellation or timeout before commit leaves the workspace unchanged;
- cancellation or timeout during the commit loop triggers temporary-file cleanup and backup restoration.

Terminal errors use:

- `REQUEST_CANCELLED` for client/session cancellation;
- `PATCH_TIMEOUT` for the patch deadline or worker watchdog.

Both errors include the execution phase, elapsed time, retryability, and workspace-state guidance.

### MCP worker watchdog

The MCP layer waits for the configured patch timeout plus two seconds. If the worker has not returned, it cancels the request and allows a further two-second cleanup window.

Therefore the default MCP response boundary is approximately 24 seconds, below the global 90-second request timeout. A caller that explicitly requests the 60-second maximum receives a maximum normal boundary of approximately 64 seconds.

If a platform filesystem call remains blocked beyond the cleanup window, Anchor returns `PATCH_TIMEOUT` with:

- `worker_stopped=false`;
- `workspace_modified=null`;
- a requirement to inspect `git_status` and the target files before retrying.

The server no longer waits indefinitely for that blocking worker before returning a response to the upstream client.

## Successful response contract

Successful `apply_patch` and `patch_check` results now include:

- `terminal_status`;
- `duration_ms`;
- `timeout_ms`.

The presence of these required fields distinguishes a completed patch result from a dropped or incomplete call.

## Regression coverage

The test suite verifies:

- pre-cancelled patches return `REQUEST_CANCELLED` and do not modify files;
- expired patch deadlines return `PATCH_TIMEOUT` and do not modify files;
- successful patches return terminal timing metadata;
- hunk matching remains exact/fuzzy compatible;
- multi-file validation failure remains atomic;
- a deliberately stuck worker produces a terminal watchdog response before the global MCP timeout;
- unified tool dispatch preserves the no-write cancellation guarantee;
- published output schemas accept and require terminal metadata.
