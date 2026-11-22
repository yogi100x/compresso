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
`docs/runbooks/install-local.md`). Postgres and MySQL/MariaDB support has not
yet been run against a live server; SQLite has.

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
