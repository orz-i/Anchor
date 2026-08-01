# Command cost governance

Anchor classifies workspace commands as `free`, `local_expensive`, or
`external_paid`. Commands classified as `external_paid` are blocked unless an
operator enables them in the trusted GUI or CLI control plane. A model-supplied
argument cannot grant this approval.

The workspace-level daily run input has no additional UI maximum. Runtime and
persisted counters use unsigned 64-bit integers, while project rules can still
set a lower `max_runs` value for individual commands.

The runtime setting also defines a maximum number of paid runs per day and a
maximum duration for each paid run. Run reservations are persisted under the
Anchor Harness data directory, so reconnecting the MCP client does not reset
the daily limit.

Projects can further restrict commands with `.anchor/command-policy.yml`:

```yaml
commands:
  story-live:
    match: "playwright.*story-live"
    cost_class: external_paid
    require_confirmation: true
    max_runs: 1
    max_duration_seconds: 1800
    max_retries: 0
    max_external_calls: 30
    max_tokens: 200000
    max_cost_usd: 10
```

`match` is a Rust regular expression. Rules are evaluated in deterministic key
order. A malformed YAML file or regular expression blocks command execution;
Anchor does not silently ignore an invalid cost policy.

Anchor also conservatively classifies commands containing common live-test
markers such as `REAL_MODEL=1`, `E2E_LIVE`, `story-live`, or provider API-key
names as `external_paid` when no project rule matches.

Anchor can directly enforce run count and wall-clock duration. External call,
token, and monetary limits are returned as declared budgets, but remain
advisory unless the tested program or provider integration reports usage back
to Anchor.
