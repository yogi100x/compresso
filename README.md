# Compresso

Compresso is a local-first context intelligence layer for AI coding tools. The
current delivery target is the offline vertical slice covering the compression
core, repository indexing, CLI, MCP server, and supported client
integrations. The MCP server is read-only unless the operator starts it with
`--edits`, which adds one write tool, `compresso_edit`, for verified literal
replacements, identifier renames, and anchored inserts in indexed files
(ADR-0035); `compresso edit` runs the same edits from the command line.
Compresso makes no network request of its own unless the operator starts the
server with `--sql-env COMPRESSO_SQL_NAME` (or `--mongo-env`, below), which adds one read-only tool,
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

### Read-only MongoDB (`compresso_mongo`)

Starting the server with `--mongo-env COMPRESSO_MONGO_NAME` adds a second
read-only tool, `compresso_mongo`, for the one MongoDB database whose
`mongodb://` URL that variable holds (E17-52, ADR-0038). It is off unless
you turn it on, full profile only, and never given to the exploration
agent; `compresso integrations install claude-code --mongo-env NAME` writes
only the variable's name into the Claude Code entry.

**MongoDB has no read-only session, so two checks are the whole control,
not a backstop:** an allow-list that the tool's arguments must pass before
anything is sent (only listed query operators and pipeline stages, at every
depth; `$where`, `$function`, `$accumulator`, `$out`, `$merge`, and every
unlisted `$` key are refused), and an account check on every connection
(exactly one user, only the `read` role on the configured database, only
read actions, and a server that enforces access control). The tool builds
`find`, `aggregate`, `count`, `distinct`, `collections`, and `describe`
itself and never forwards a command; every command carries a server time
limit and a row limit.

Create a user with only `read` on the data database, on a server with
access control on:

```javascript
use app
db.createUser({ user: "mongo_reader", pwd: passwordPrompt(),
                roles: [{ role: "read", db: "app" }] })
```

then set `COMPRESSO_MONGO_APP=mongodb://mongo_reader:...@db.example.com/app`.
One host only: `mongodb+srv://`, several hosts, `replicaSet`, and unlisted
URL options are refused, and the tool always connects directly to that host.
Any host but loopback gets verified TLS against the driver's bundled root
certificates, or only the file named by `tlsCAFile=/abs/ca.pem`; TLS cannot
be turned off. That user can read everything in that database, so keep what
the model should not see elsewhere. The live tests passed on the build
machine against MongoDB 8.0.38; the CI job that runs them has not run yet.

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
enables `compresso_sql` with `--sql-env` or `compresso_mongo` with `--mongo-env`), but anything an
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
