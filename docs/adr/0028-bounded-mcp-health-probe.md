# ADR-0028: Bounded MCP server health probe

## Status

Accepted 2026-11-14 by the owner (E17-06, D71). It adds a process capability
to a `bounded-local` crate, limited to the `mcp_probe` module.

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

- **Client configuration is untrusted input.** A project-scope config such as
  `.mcp.json` or `.cursor/mcp.json` lives inside a repository and can name any
  executable. Reading a configuration never implies permission to run what it
  names.
- **What runs.** A configured command is started only when both hold:
  1. the entry has the exact shape a Compresso adapter writes: an absolute
     `command`, the fixed arguments `mcp serve` (plus `--root <canonical
     root>` for project scope), and no extra fields; otherwise it is
     `config_modified`;
  2. the command is a trusted Compresso launcher: its canonical path equals
     the canonical path of the running Compresso executable, or an active,
     authenticated install receipt for that exact configuration file records
     an owned-value hash equal to the entry as it exists now (so Compresso
     itself wrote that launcher path through an approved plan); otherwise it
     is `not_probed`.
  Paths are compared after canonicalization, so a symbolic link to another
  binary is not trusted. A launcher trusted by path is started through its
  canonical path.
- **How it runs.** Direct `std::process::Command` with a fixed argument
  vector and no shell. The environment is cleared except for an allowlist
  (`PATH`, `HOME`, temp and locale variables, and the XDG and `COMPRESSO_*`
  variables Compresso reads). The working directory is the filesystem root
  on Unix, never the inspected repository. The server runs in its own
  process group. No `tokio` process feature and no new dependency.
- **What it does.** One MCP `initialize`, `notifications/initialized`, and
  `tools/list` exchange over stdio. No tool is called and no repository
  content is read. The server records one ordinary local session row for the
  client name `compresso-doctor`.
- **Bounds.** One deadline (5 s in the real CLI) covers the whole exchange;
  server output is capped at 1 MiB; the child gets 500 ms to exit after its
  input closes; otherwise its whole process group is killed (fixed
  `/bin/kill -KILL -- -<pgid>`) and the child is reaped. `doctor` starts each
  distinct trusted launcher at most once.
- **Injected boundaries stay process-free.** `IntegrationEnvironment::new`
  and a `DiagnosticEnvironment` without `mcp_probe_timeout` start nothing and
  report `probe_skipped`. Only the real CLI entry points enable the probe.
- **Verdicts.** `binary_missing`, `binary_not_executable`, `spawn_failed`,
  `exited_early`, `handshake_timeout`, `malformed_response`,
  `config_modified`, `not_probed`, `config_unreadable`, `not_configured`,
  `probe_skipped`, and `healthy`, each with its own fix text. `not_probed`
  and `config_modified` point at `compresso integrations install <client>`.

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
- Running `compresso doctor` or `compresso integrations status` in or against
  a hostile repository cannot execute a repository-chosen command: such a
  command is neither the running executable nor covered by a receipt in the
  user's private data directory.
- A hand-written entry that points at a valid Compresso build other than the
  running one is reported as `not_probed` until the user runs
  `compresso integrations install <client>`.
- Receipt trust covers the recorded launcher path, not the bytes at that
  path. Whoever can replace the user's installed launcher already controls
  what the coding client executes.
- A descendant that moves itself into a different process group can outlive
  the probe.
- The probe checks that the server answers a handshake. It does not prove the
  coding client has loaded the server or that an already-running server
  process inside a client session is still responsive.
