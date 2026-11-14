# 0028 — Bounded MCP server health probe

## Status

Proposed (E17-06). Requires owner sign-off because it adds a process
capability to a `bounded-local` crate.

## Context

A coding client starts `compresso mcp serve` from the launcher path stored in
its configuration. When that path goes stale after an upgrade or move, when the
server crashes during startup, or when it stops answering, the client only
shows a generic connection failure and agents silently fall back to native
tools. `compresso doctor` and `compresso integrations status` could validate
the configured path, but a static check cannot distinguish a server that
starts from one that exits or hangs.

The capability matrix in `docs/plan/01-stack-and-workspace.md` §3.1 denied all
process execution in `compresso-cli`. Detecting a crash or a hang requires
starting the configured server once.

## Decision

`compresso-cli` gains one narrowly scoped process capability, confined to
`rust/compresso-cli/src/commands/mcp_probe.rs`:

- **What runs.** Only the Compresso launcher recorded in a client
  configuration, and only when the entry still has the exact shape a Compresso
  adapter writes: an absolute `command`, the fixed arguments `mcp serve`
  (plus `--root <canonical root>` for project scope), and no extra fields. A
  hand-edited entry is reported as `entry_modified` and is never started.
- **How it runs.** Direct `std::process::Command` with a fixed argument
  vector. No shell, no argument or environment taken from repository content,
  no `tokio` process feature, and no new dependency.
- **What it does.** One MCP `initialize`, `notifications/initialized`, and
  `tools/list` exchange over stdio. No tool is called and no repository
  content is read. The server records one ordinary local session row for the
  client name `compresso-doctor`.
- **Bounds.** One deadline (5 s in the real CLI) covers the whole exchange;
  server output is capped at 1 MiB; the child gets 500 ms to exit after its
  input closes and is then killed and reaped. `doctor` starts each distinct
  launcher at most once.
- **Injected boundaries stay process-free.** `IntegrationEnvironment::new`
  and a `DiagnosticEnvironment` without `mcp_probe_timeout` start nothing and
  report `probe_skipped`. Only the real CLI entry points enable the probe.
- **Verdicts.** `binary_missing`, `binary_not_executable`, `spawn_failed`,
  `exited_early`, `handshake_timeout`, `malformed_response`,
  `entry_modified`, `config_unreadable`, `not_configured`, `probe_skipped`,
  and `healthy`, each with its own fix text.

`scripts/check-crate-capabilities.ts` allows process APIs in that one file of
that one crate. Fixtures prove the allowance does not extend to another
module of the CLI or to the same path in another crate.

`compresso integrations verify` is unchanged: `live_client_evidence` still
means evidence from the coding client itself, which this probe does not
provide.

## Consequences

- `doctor` and `integrations status` can report a server that fails to start
  or stops responding, at the cost of up to one deadline per distinct
  launcher.
- The CLI can now execute a path read from a user-writable client
  configuration. That file already determines what the coding client executes
  on every start, so the probe adds no new authority; the exact-shape gate
  keeps the probe from running anything other than `<absolute path> mcp serve`.
- A hung server spawned through a wrapper that ignores termination of its
  parent may outlive the probe; the probe kills only its direct child.
- The probe checks that the server answers a handshake. It does not prove the
  coding client has loaded the server or that an already-running server
  process inside a client session is still responsive.
