# Compresso

Compresso is a local-first context intelligence layer for AI coding tools. The
current delivery target is the offline vertical slice covering the compression
core, repository indexing, CLI, MCP server, and supported client
integrations. The MCP server is read-only unless the operator starts it with
`--edits`, which adds one write tool, `compresso_edit`, for verified literal
replacements, identifier renames, and anchored inserts in indexed files
(ADR-0035); `compresso edit` runs the same edits from the command line.
Compresso makes no network request of its own unless the operator starts the
server with `--sql-env COMPRESSO_SQL_NAME`, which adds one read-only tool,
`compresso_sql`, that queries the one Postgres, MySQL/MariaDB, or SQLite
database whose URL that environment variable holds (ADR-0036; see
`docs/runbooks/install-local.md`). Its live tests pass in CI against
Postgres 16.4, MariaDB 11.4.3, and MySQL 8.4 containers (read-only codes,
time limits, privilege refusals, TLS host checks); other versions, managed
cloud variants, and Windows clients have not been tried. SQLite is fully
tested (on Windows a hard link to a file in Compresso's data
directory is not detected as one). Give it an account that can read only what the model should
see (see the runbook for a least-privilege example): the tool refuses
powerful roles, but a role that can read secret tables exposes them. A
private CA goes in the URL (`sslrootcert=` / `ssl-ca=`), never in
`SSL_CERT_FILE`. A Postgres target is refused while any
`PG*` variable (such as `PGUSER`) is set where the client starts.

## Connecting a coding client

`compresso setup <claude-code|codex|cursor> <ROOT>` indexes and approves one
repository and writes the Compresso MCP server entry into that client's
configuration. `compresso integrations install <client>` writes only the
server entry, except that a Claude Code install with `--project DIR` also
writes the `/compresso` session command skill
(`.claude/skills/compresso/SKILL.md`: `/compresso stats`, `status`, `recall`,
`update`, `bug`, `feedback`, `share`; `--no-session-commands` leaves it out).
The `bug` and `feedback` reports are saved locally; Compresso sends them
nowhere (like every tool result, their text reaches the client's model
provider). Both show the exact plan first (`--dry-run` previews without
writing), keep a backup and a receipt, and are undone with
`compresso integrations uninstall <client>`.

For a Claude Code project (`setup claude-code ROOT`, or
`integrations install claude-code --project DIR`) the install also makes
Compresso the session's main agent by default (ADR-0037):

- `.claude/agents/compresso-main.md`: a short main-session prompt that routes
  repository search, reads, and edits through Compresso. It replaces the
  client's long default system prompt; the client keeps all its own tools.
- `"agent": "compresso-main"` in your personal
  `.claude/settings.local.json`. Only that one key is added; every other byte
  of the file is kept. An `agent` value you already set is never
  overwritten: the install skips this step and says so. If the project is a
  git repository that does not ignore this file, the install prints one
  warning and changes nothing.
- The project's server entry runs
  `mcp serve --tools lean --session-tools --edits`: the lean tools with
  compact text results, the edit tool `compresso_edit`, and the two tools the
  `/compresso` commands need. With `--sql-env` the entry keeps the full
  profile, because the lean profile never serves `compresso_sql`.

No permission rule is added for the edit tool, so the client still asks
before each edit. Pass `--no-edits` to keep the agent without the edit tool,
or `--no-main-agent` to install the entry as before (full profile) with no
agent file and no settings change. A plain project uninstall removes the
key (only while it still names `compresso-main`), the agent file (only while
unchanged), and the settings file only if Compresso created it and nothing
else is left in it. User-scope installs, Codex, and Cursor are unchanged.

The saving behind this default was measured on one model and one
edit-heavy suite, two runs: about $3.07 a run against $5.60 for the client
alone, with the first request about 2,800 tokens smaller. Other models and
task mixes are unmeasured, and the release that added the default
re-measured only the first-request size, not the suite.

Session recall is on by default for Claude Code and Codex: the server entry
ends in `--recall-source <that client>`, which serves the `compresso_recall`
tool over that client's own local session transcripts. Recall is scoped to the
session's repository, read-only, secret-redacted, builds no index, and uploads
nothing; a client never gets another client's transcripts, and Cursor gets
none because its transcript format is unsupported. Pass `--no-recall` to
either command to leave it off, or re-run the command with `--no-recall` to
turn it off later. Compresso itself makes no network request (unless the operator
enables `compresso_sql` with `--sql-env`), but anything an
MCP tool returns, recall snippets included, goes to the coding client, which
may send it to its model provider.

`compresso setup` for Claude Code and Codex also installs one hook: a
`SessionStart` hook with matcher `compact` that runs
`compresso mcp routing-reminder --session-start-hook`, so the client re-reads
Compresso's tool routing after it compacts a conversation. The hook goes in
your user-level settings (`~/.claude/settings.json` or `~/.codex/hooks.json`),
never in a repository file, so it is not committed and one hook covers all
your repositories. It is previewed with the rest of the plan and has its own
receipt. If you already define
`SessionStart` hooks, Compresso does not touch them: setup skips the hook,
prints a warning with the entry to add by hand, and still succeeds. Pass
`--no-compaction-hook` to leave it out, and remove it later with
`compresso integrations uninstall <client> --compaction-hook`; because the
hook is user-wide, uninstalling the MCP entry of one repository leaves it in
place.
`compresso integrations install` adds the hook only when given
`--compaction-hook`. Details:
[Post-compaction routing reminder](docs/integrations/compaction-reminder.md).

## Development

Development standards are documented in
[Repository conventions](docs/architecture/conventions.md).
