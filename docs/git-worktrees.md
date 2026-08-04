# Optional Git worktree tasks

Anchor uses the configured workspace checkout by default. Git worktrees are an optional Harness task isolation mode for cases where multiple tasks need independent branches, indexes, files, commands, and commits.

## Start modes

The default remains the shared checkout:

```json
{
  "objective": "Fix the parser",
  "workspace_mode": "shared"
}
```

To create an isolated linked worktree for a new task:

```json
{
  "objective": "Fix the parser",
  "workspace_mode": "worktree",
  "worktree_base_ref": "HEAD",
  "worktree_branch": "anchor/parser-fix",
  "worktree_remove_on_close": false
}
```

`workspace_mode` is accepted by `start_task` and `begin_work_session`. Omitting it is equivalent to `shared`.

## Managed layout

Anchor-managed worktrees are created under:

```text
.anchor/worktrees/<task-id>/
```

The directory is ignored by the primary checkout. Task metadata stores the canonical execution path, branch, base ref, creation time, and cleanup preference.

The worktree starts from the committed `worktree_base_ref`. Uncommitted files from the primary checkout are not copied into it.

## Routing and concurrency

For a task bound to a worktree, Anchor routes these operations to the linked checkout automatically:

- file reads, searches, patches, and removals;
- command execution and retained command sessions;
- Git status, diff, stage, commit, restore, reset, revert, and clean;
- stage-commit verification and task baseline checks;
- Browser file paths, artifacts, uploads, and frontend build information;
- Canvs task branch, HEAD, and workspace-mode display.

History Session files and portable handoff exports remain in the primary configured workspace.

Shared-checkout tasks still use a single writer lease. Separate linked worktrees are independent write domains, so tasks in different worktrees may remain active and run commands concurrently.

## Worktree management tools

- `git_worktree_list`: list main, external, and Anchor-managed worktrees.
- `git_worktree_create`: create a managed worktree under `.anchor/worktrees`.
- `git_worktree_remove`: remove a clean managed worktree.
- `git_worktree_prune`: prune stale Git administrative records.

Only worktrees under `.anchor/worktrees` can be removed by Anchor. A worktree attached to an active, paused, verifying, or failed task cannot be removed manually. Force removal requires operator-enabled dangerous mode.

## Task completion and cleanup

By default, completing a task keeps its worktree and local branch for inspection or later integration.

Set `worktree_remove_on_close=true` to remove the linked checkout after all of these conditions are met:

1. required verification passed;
2. the task worktree is clean;
3. the task was closed successfully;
4. the matching History checkpoint was persisted.

Automatic cleanup removes the linked checkout only. It does not delete the task branch.

## Recovery behavior

Task-to-worktree binding is persisted in the Harness task record. A reconnected MCP session can resume the task and recover the same execution root. Explicit `task_id`, History identity, and retained-command ownership take precedence over ambient session binding.

If the linked checkout is missing or inaccessible, task operations fail with `TASK_WORKTREE_UNAVAILABLE` instead of silently falling back to the primary workspace.
