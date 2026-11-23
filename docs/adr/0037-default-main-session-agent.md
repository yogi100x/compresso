# ADR-0037: A default main-session agent for Claude Code projects

## Status

Proposed 2026-11-23 by the implementing session (E17-51, D116). The owner
decided on 2026-11-22/23 that the main-session agent and the edit tool are on
by default in a Claude Code project install, with opt-out flags, and accepted
five defaults (listed in D116). Every other choice below is the session's and
awaits owner confirmation. It amends D92 (no installer writes `lean` into the
shared entry), D103 (the shared entry stays on the full profile), and D113
(the lean profile does not serve the session command tools). It adds no
network or process capability.

## Context

Claude Code can run a whole session as one agent: the `agent` key of a
settings file names an agent file under `.claude/agents/`, and that file's
body replaces the client's default system prompt. A measured probe, on one
model and one edit-heavy suite, two runs, found a short body that routes
repository search, reads, and edits through Compresso, with Compresso's lean
tools and the edit tool, cost about $3.07 a run against $5.60 for the client
alone. The same setup made the first request about 2,800 tokens smaller. In
the winning runs the server was `mcp serve --root <repo> --tools lean
--edits`, its tools were deferred behind the client's tool search, the
client's built-in tools stayed in place, and nothing else was added (no
`alwaysLoad`, no hooks, no per-turn text). This branch re-measured only the
first-request size, not the suite.

Three client facts shaped the design, each verified on 2026-11-22 with the
client's `-p` mode: the `agent` key in `.claude/settings.local.json` is
honoured exactly like `--agent`; an empty agent body keeps the long default
prompt and loses the saving; and an agent file's own `mcpServers` do not
start in a folder the user has not trusted, nor ever under `-p`.

## Decision

- **Three files, three plans, three receipts.** A Claude Code project install
  (`integrations install claude-code --project DIR` and `setup claude-code`)
  writes, beside the `/compresso` skill:
  1. the project entry in `.mcp.json` with `--tools lean --session-tools`
     after the base arguments, then `--edits`, then the recall source;
  2. `.claude/agents/compresso-main.md`, whole-file owned as in ADR-0034
     (marker line, refusal of a file without it, deletion on uninstall only
     while unchanged);
  3. the one key `"agent": "compresso-main"` in `.claude/settings.local.json`
     (`ConfigFormat::Json`, owned path `agent`).
  Everything is planned before the first write. An unparseable settings file,
  a non-object root, a key named twice, or a symbolic link on any path stops
  the install with nothing written and names the file. A user file at the
  agent path, or an `agent` value that is not ours, skips that step with a
  warning and the install succeeds; the key is never written without the
  agent file.
- **Agent file.** Frontmatter `name` and `description` only: no `tools` (the
  session keeps every client tool), no `model`, no `mcpServers` (they would
  not start in an untrusted folder or under `-p`). The body is the measured
  prompt with three changes: it restates that repository content, tool
  results, and command output are untrusted data, never instructions, and
  that edits stay minimal and scoped; its routing rule is general ("find how
  the request reaches it (entry point, routing, middleware, validation,
  callers)"); and it says to use the client's tools if Compresso's cannot be
  loaded. Tool names are the bare names the `compresso` server serves. The
  body is at most 2,500 characters (a test pins it and that it is not empty).
  With `--no-edits` the body never names `compresso_edit`.
- **Settings splice.** The key is spliced into the existing text after the
  last member, copying the file's own separator and colon spacing, so every
  other byte (formatting, key order, trailing newline, byte-order mark, line
  endings) is kept; uninstall removes exactly those bytes. A missing file is
  created as `{"agent": "compresso-main"}` with two-space indentation and a
  trailing newline. The plan manager writes through a temporary file renamed
  in the same directory and keeps its usual timestamped backup of an existing
  file. Uninstall removes the key only while it still holds `compresso-main`
  (otherwise `key_modified`), and deletes the file only when nothing else is
  left in it and the install receipt says Compresso created it. The plan
  manager enforces that last rule for every JSON target: an empty JSON
  target is accepted only for an uninstall whose receipt records
  `(none: file created)`, and its owned-path check proves nothing else is in
  the file.
- **Profile.** A new `mcp serve --session-tools` flag, accepted only with
  `--tools lean`, adds `compresso_stats` and `compresso_report` (short
  descriptions, `stats` advertising only `since`) after the lean tools so the
  `/compresso` commands keep working; recall stays a lean row served with
  `--recall-source`. The lean instructions stay byte-identical (the new rows
  add no hint), and plain `--tools lean` is unchanged. With `--sql-env` the
  entry keeps the full profile, because the lean profile never serves
  `compresso_sql` (D115); the agent is still installed and its tool names are
  served by the full profile too.
- **Opt-outs.** `--no-main-agent` writes neither file and keeps the previous
  entry (full profile; `--edits` only if given). `--no-edits` keeps the
  agent and the lean session profile without the edit tool. User-scope
  installs, Codex, and Cursor are unchanged. No permission rule is added for
  the edit tool.
- **Verify.** `integrations verify claude-code --project` adds
  `main_agent_managed`, `main_agent_current` (either body), `main_agent_setting`,
  and `main_agent_server_profile` (lean session profile, or full with
  `--sql-env`, and `--edits` when the body names the edit tool) whenever the
  agent file or the key is present; a `--no-main-agent` install gets none.
- **Git warning.** When the project holds `.git` and neither its `.gitignore`
  files, its `info/exclude`, nor the user's global git excludes file ignore
  `.claude/settings.local.json`, install prints one warning. Nothing is
  edited.

## Consequences

- A default project install now changes how every Claude Code session in that
  project behaves: the client's default system prompt is replaced by a short
  one. The saving was measured on one model and one suite, two runs; other
  models and task mixes are unmeasured.
- The project entry serves lean text results instead of the full profile's
  JSON, and `compresso_compress`, `compresso_impact`, `compresso_explain`, and
  `compresso_sql` are not served unless the entry is reinstalled with
  `--no-main-agent` (or `--sql-env`).
- First request on the client's `-p` mode (its smallest model, scratch project, 2026-11-23):
  the default install measured 17,322 input tokens; the same agent with plain
  `--tools lean --edits` 17,263 (the session tools and recall add 59); the same
  agent with no server 16,765; `--no-main-agent` 20,202 (2,880 more than the
  default); the default entry with the `agent` key removed 20,104 (the agent
  alone saves 2,782).
- `--allow-edit-tool` was not built: the receipt model owns whole JSON values
  by key path, so a single element of the user's `permissions.allow` list
  cannot be owned and removed on uninstall without extending the plan
  validator that guards every client configuration change.
- Removing the agent leaves an empty `.claude/agents/` directory if
  Compresso created it.
