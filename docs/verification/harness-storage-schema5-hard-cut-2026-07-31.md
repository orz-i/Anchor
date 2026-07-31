# Harness Storage Schema 5 Hard Cut

Date: 2026-07-31

## Scope

Anchor now uses Harness Storage Schema 5 for durable Task, baseline, verification, event, operation, staged-commit, and work-session-close state.

This is an intentional hard cut. There is no compatibility reader, migration bridge, or automatic import for the previous path-hash-based Harness store, inline Task baselines, or bare JSONL journals.

## Storage boundary

The default system data root changes from:

```text
<local-data>/anchor/harness
```

to:

```text
<local-data>/anchor/harness-v5
```

The Schema 5 root contains a required `store.json` marker. A non-empty unmarked root or a marker with another schema version fails with `STORE_SCHEMA_INCOMPATIBLE`.

Old data remains untouched under the old root, but the new build does not read it.

## Stable workspace identity

Git workspaces receive a generated UUID in Git-private metadata:

```text
<git-dir>/anchor-workspace-id.json
```

Non-Git workspaces use the fallback:

```text
<workspace>/.anchor/workspace-id.json
```

The system store uses the UUID as its namespace:

```text
harness-v5/workspaces/<workspace-uuid>/
```

`identity.json` records the current canonical path and all observed aliases. Moving a Git workspace together with its `.git` directory preserves its Harness identity and Task history.

## Workspace transaction lock

All compound Harness mutations use:

```text
workspaces/<workspace-uuid>/workspace.lock
```

The lock is process-wide and cross-process, waits for at most five seconds, and treats Windows sharing violations 32 and 33 as normal contention. Task read-modify-write, workspace state, verification updates, and associated event journal appends execute under the same lock.

JSON objects use temp-file write, `sync_all`, atomic replacement, and parent-directory sync. Windows replacement uses `MoveFileExW` with replace-existing and write-through flags.

## Content-addressed baselines

Task files no longer embed every baseline entry. A Task stores only baseline metadata and an object reference:

```json
{
  "baseline": {
    "schema_version": 5,
    "object_id": "<sha256>",
    "file_count": 506,
    "worktree_fingerprint": "<sha256>",
    "branch": "main",
    "head": "<commit>"
  }
}
```

Entries are stored once at:

```text
workspaces/<workspace-uuid>/baselines/<object-id>.json
```

The object ID is derived from the ordered entry content. Existing objects are immutable; a path/content mismatch is treated as store corruption.

## Checksummed segmented journals

Events and operations use segmented journals:

```text
journals/events/<task-id>/00000001.jsonl
journals/operations/00000001.jsonl
```

Each line is an envelope:

```json
{
  "schema_version": 5,
  "sequence": 42,
  "checksum": "<sha256>",
  "record": {}
}
```

Properties:

- monotonically increasing sequence numbers;
- checksum over schema, sequence, and record content;
- malformed JSON, wrong schema, failed checksum, and duplicate/out-of-order records are skipped rather than terminating the scan;
- health counters are exposed by `harness_status.journal_health`;
- journals rotate at 4 MiB and retain eight segments;
- each append calls `sync_data` before returning.

Bare pre-Schema-5 JSONL records are not accepted.

## Durable close-work-session outbox

`close_work_session` persists intent before closing the Task:

```text
outbox/close-work-session/<task-id>.json
```

The state machine is:

```text
prepared -> task_closed -> checkpoint_pending -> completed
```

The outbox stores the exact finish and History checkpoint arguments, attempt count, phase, and last error. Recovery runs:

- when a ToolContext starts;
- before each Harness tool call other than `close_work_session`;
- when `close_work_session` is retried.

If the Task closes but History persistence is unavailable, Anchor returns `WORK_SESSION_CHECKPOINT_PENDING`; the durable outbox retries the checkpoint without repeating Task completion. Completed outboxes are retained as audit receipts.

## Operational impact

After installing this build:

- previously active Harness Tasks are not resumed;
- old operation/event journals are not displayed;
- old verification and stage-commit receipts are not imported;
- History Session Markdown under `docs/history-session` is unaffected because it is a separate storage system;
- Git commits and working-tree content are unaffected.

Rollback to an older build uses the old `anchor/harness` root. Rolling forward again uses `anchor/harness-v5`.

## Verification coverage

Automated coverage includes:

- old unmarked store rejection;
- stable workspace UUID across directory moves and alias recording;
- Task baseline object references without inline entries;
- concurrent operation writers with sequence and checksum validation;
- malformed journal line recovery;
- checksum tampering recovery;
- segment rotation and retention;
- staged-commit isolation from workspace identity metadata;
- Task-closed/History-unavailable outbox recovery to `checkpoint_pending`, followed by automatic completion when History storage returns;
- full Rust library, integration, security, History, Harness, and output-schema contracts;
- strict Clippy, frontend checks/build, and formal desktop packaging.
