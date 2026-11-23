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

For a Claude Code project (`setup claude-code ROOT`, or
`integrations install claude-code --project DIR`) the install also makes
Compresso the session's main agent by default (ADR-0037):

- `.claude/agents/compresso-main.md`: a short main-session prompt that routes
  repository search, reads, and edits through Compresso. It replaces the
  client's long default system prompt; the client keeps all its own tools.
- `"agent": "compresso-main"` in your personal
  `.claude/settings.local.json`. Only that one key is added; every other byte
  of the file is kept, and no backup copy of this file is ever written (it
  may hold secrets; the change is exactly reversible).
- The project's server entry runs
  `mcp serve --tools lean --session-tools --edits`: the lean tools with
  compact text results, the edit tool `compresso_edit`, and the two tools the
  `/compresso` commands need.

The three always agree. If the agent cannot be installed in full (an
`agent` value you already set, which is never overwritten, or a
`compresso-main.md` that Compresso did not write), the entry is written as
`--no-main-agent` writes it and a warning says what was skipped and how to
get the default. The agent file and the key are written before the
entry; if either cannot be written (a file changed after the preview), the
full-profile entry the preview also shows is written instead. `--sql-env` keeps the full profile, because the lean profile
never serves `compresso_sql`, so it installs no agent either. If the project
is a git repository that does not ignore `.claude/settings.local.json`, the
install warns; it never edits `.gitignore` itself. If the project is not yet
indexed, the install warns too (run `compresso index DIR`; `setup` indexes).

No permission rule is added for the edit tool, so the client still asks
before each edit. Pass `--no-edits` to keep the agent without the edit tool,
or `--no-main-agent` to install the entry as before (full profile) with no
agent file and no settings change; over a project that has the agent, it
also removes the agent file and the key. A plain project uninstall removes
the key (only while it still names `compresso-main`), the agent file (only
while unchanged, and never while the key could not be removed), and the
settings file only if Compresso created it and nothing else is left in it.
User-scope installs, Codex, and Cursor are unchanged.

The saving behind this default was measured on one model and one
edit-heavy suite, two runs: about $3.07 a run against $5.60 for the client
alone, with the first request about 2,800 tokens smaller. The shipped prompt
differs from the measured one by a few lines (untrusted content, scoped and
non-destructive changes, a fallback when the repository is not indexed, a
general rule for finding where behaviour is decided, and Read before Edit
without the edit tool); the suite was not re-run, only the first-request
size was re-measured. Other models and task mixes are unmeasured.

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
