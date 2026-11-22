# ADR-0036: One opt-in, read-only live SQL tool is the only network path in the MCP server

## Status

Proposed 2026-11-22 (E17-50, D115), revised the same day after an
independent security review. The owner decided on 2026-11-22 to add a live
SQL query tool for Postgres, MySQL/MariaDB, and SQLite, with database drivers
linked into the shipped binary, approved the gate amendments in decision 3
and the guarantee-sentence rule in decision 4, and set the rule that the
connection comes only from the one named variable (decision 6). Every other
choice here was made by the orchestrating and implementing sessions and is
**awaiting owner confirmation** (listed in D115).

**Live verification is partial.** The CI job `live-sql` runs `tests/live.rs`
against Postgres 16.4, MariaDB 11.4.3, MySQL 8.4, and a TLS Postgres with a
certificate made in the job; `COMPRESSO_SQL_TEST_REQUIRE_LIVE=1` makes a
missing variable a failure. Its first run (Postgres and MariaDB only) passed
five checks: Postgres read-only codes, timeout with no session left behind,
privileged role and `standard_conforming_strings`, and MariaDB read-only
codes and privileged account with `sql_mode`. The wrong-host TLS check did
not run then (no variable; it passed vacuously). That run exposed three
faults, all fixed: MariaDB returning a value for a statement cut at its time
limit, a panic when called from inside a Tokio runtime, and in-process tests
refused on machines with `PGUSER` set; separately, build output in the
checkout tripped the secret scan. Everything added since (MySQL 8.4, TLS,
the role and cut-rule tests) is unproven until that job runs again;
statements below not covered by a named live test are reasoned.

## Context

Until E17-50 the server opened no network connection: `AGENTS.md` said Core,
Atlas, Store, and MCP code must not gain network-I/O dependencies, the
capability check enforced it on the workspace-unified dependency graph, and
the server instructions said every tool "makes no independent network
request". The E15 plan (E15-09..E15-13, `09-advanced-capabilities.md` §6)
puts SQL intelligence behind connection profiles, keychain credentials, a
parser and classifier, and a mutation plan. This ADR lands a narrower part
of E15-09 and E15-12 now, by owner request: bounded read-only queries against
one database named by an environment variable. Schema graphs, profiles,
keychain credentials, a SQL parser, and mutation stay in E15.

## Decision

1. **One crate owns every database connection.** `rust/compresso-sql`
   (capability class `database-network`) holds the URL parsing, the transport
   policy, the statement gate, the engines, and the scrubbing. Postgres and
   MySQL/MariaDB use `sqlx` 0.9.1 with `default-features = false` and the
   features `postgres`, `mysql`, `runtime-tokio`, and
   `tls-rustls-ring-native-roots` (rustls, the ring provider, the operating
   system's certificate store); no macros, migrations, bundled roots,
   `mysql-rsa`, native TLS, or C build dependency. SQLite reuses the bundled
   `rusqlite` with `hooks` and `limits`. `cargo deny check licenses bans
   sources` passes with no `deny.toml` change. An internal `Connector` trait
   is the seam for a later non-SQL store (E17-51). A `test-hooks` feature,
   enabled only by the crate's own tests, adds a call that skips the
   statement gate so each database-level control is tested alone.
2. **Opt-in, fixed at startup.** `compresso mcp serve --sql-env NAME`
   (`COMPRESSO_SQL_[A-Z0-9_]+`) reads that one variable once: `sqlite:/abs/path`
   (no parameters), `postgres://` / `postgresql://`, or `mysql://` /
   `mariadb://`. Nothing reads `.env` files, discovers databases, accepts a
   connection string as a tool argument, or stores the value. The tool is
   served only in the full profile; the lean profile and the exploration
   agent never get it, and `--tools lean --sql-env` is refused. A missing or
   invalid value still advertises the tool, every call returns
   `sql_not_configured`, and stderr names the variable once, never its value.
   `integrations install claude-code --sql-env NAME` writes only the name.
3. **Gate amendments (owner-approved).** (a) The capability check judges
   each member on its own resolved tree (`cargo tree -p MEMBER`). (b)
   `compresso-mcp` and `compresso-cli` may reach network-capable packages
   only through `compresso-sql`, checked two ways: the member is resolved
   again in a temporary copy of the workspace with `compresso-sql` removed
   and must reach no network package there (this accounts for feature
   unification: Tokio's `net`, turned on only by `sqlx`); and in the real
   tree no path to a network package may pass through a workspace crate
   other than `compresso-sql` and the two exempt consumers (so
   `compresso-sql` cannot turn on a network feature of a pure-local crate).
   It fails closed if `compresso-sql` is not exactly one workspace member or
   is declared in a form it cannot remove, and tells the operator to run
   `cargo fetch --locked` if the offline copy misses a crate. Every other
   crate keeps its class; negative controls cover a direct network feature in
   `compresso-mcp`, other crates depending on `compresso-sql`, an impostor
   crate, and a pure-local feature turned on by `compresso-sql`. (c)
   `AGENTS.md` carries exactly this exception. (d) Decision 4.
4. **The guarantee sentence states the truth.** With the flag off the full
   instructions are byte-identical to before. With it on, "Every tool bounds
   output, executes no shell command, and makes no independent network
   request." is replaced, for the engine known at startup, by one that names
   `compresso_sql`'s one database (network), one local file with no network
   (SQLite), or no usable database (unconfigured).
5. **The statement gate is a strict subset of SQL, and defence in depth
   only.** It also refuses `INTO` and `SHARE` anywhere, quoted EXPLAIN
   options (`EXPLAIN ("analyze")`), and MySQL `SHOW ... WHERE` (its
   expressions run outside `max_execution_time`); refusals say exactly what
   to do (remove the comment; quote a reserved-looking name). A first gate
   that lexed each dialect faithfully was bypassed six
   times in review (a non-ASCII dollar-quote tag, Unicode whitespace after
   MySQL `--`, optimizer hints that override the server deadline, prefixed
   and escaped strings, a word glued to a number). The gate now refuses,
   and names, anything whose reading differs between engines or settings:
   every comment form (`--`, `/* */` including `/*+ */` hints and `/*! */`,
   MySQL `#`); `$`; backslashes; prefixed strings and names (`E''`, `N''`,
   `X''`, `B''`, `U&''`, `U&""`, `_charset''`, a quote glued to a word or
   number) and `UESCAPE`; hex and binary literals; a letter, digit, `_`, `.`,
   or quote directly after a number; control characters and NUL; non-ASCII
   characters outside a plain `'...'` string. Whitespace is ASCII only.
   Within that subset it requires one statement, a leading `SELECT`, `WITH`,
   `VALUES`, `TABLE`, `SHOW`, `EXPLAIN`, or `DESCRIBE`, no `ANALYZE` in an
   explain, no write, DDL, or session keyword, and no function on a
   per-engine deny-list. The deny-list is a text list, incomplete by nature:
   a view, a wrapper function, or an unlisted function reaches past it. The
   controls that hold are the read-only session, the refusal of privileged
   roles, and the deadlines.
   - *SQLite*: opened read-only (`SQLITE_OPEN_READ_ONLY`, no URI, no final
     symlink, canonical path); defensive mode, untrusted schema, FTS3
     tokenizer off, attached-database limit 0, `query_only`, values and
     statements at most 16 MiB (`SQLITE_LIMIT_LENGTH`, `SQL_LENGTH`, so one
     huge allocation fails instead of overrunning the deadline), a 16 MiB
     page cache, an authorizer allowing only reads, six metadata pragmas, and
     allowed functions, `stmt.readonly()` required, and a busy timeout.
     `temp_store = MEMORY` (set by the connection; queries still cannot
     change pragmas), so temporary tables, sorts, and recursive queues never
     spill to disk (a fan-out over 1 MB values grew temporary files 300-450
     MB a second before); a progress handler every 100 instructions stops the
     statement at its deadline or once SQLite's memory (process-wide, read
     with `sqlite3_memory_used`) rises 256 MiB above the call's start
     (`sql_memory_limit`). Measured: the review's fan-out probe stops in
     about 0.15-0.2 s at both 1 s and 3 s deadlines, peak resident memory
     about 290 MB. No hard heap limit: it would be process-wide and the
     store shares the library; because MCP calls run one at a time, the
     process-wide count is the call's own. The store is refused by path and
     directory; on Unix any target with more than one hard link is refused
     outright, and a walk of Compresso's data directory (at most 10,000
     entries, 3 levels) refuses whatever it cannot fully check. On Windows
     (owner decision 2026-11-23) SQLite targets are allowed with the path
     checks only: the standard library gives neither a link count nor a
     file identity there, so a hard link to a file in Compresso's data
     directory is not detected on Windows. Backslashes are
     allowed inside plain SQLite strings (SQLite never reads them as
     escapes), so `LIKE ... ESCAPE '\'` works there.
   - *WAL databases*: a reader may create `-wal` and `-shm` beside the
     database (SQLite needs its shared-memory index); the database's own
     bytes are never written. In a read-only directory SQLite cannot, and
     the call answers `sql_wal_unavailable` with what to do. `immutable=1`
     was rejected deliberately: on a database another process is writing it
     ignores the log and locks, so it can return stale or inconsistent rows
     or report corruption.
   - *Postgres*: one fresh connection per call; refuse a role that is, or
     is a member of (direct, inherited, or NOINHERIT and usable with SET
     ROLE), a superuser, BYPASSRLS, CREATEROLE, or REPLICATION role, or any
     predefined `pg_` role: none is allowed, not even `pg_read_all_data`,
     which reads protected catalogs such as `pg_authid` password hashes,
     `pg_user_mapping`, and `pg_subscription`; `pg_read_all_stats`,
     `pg_read_all_settings`, and `pg_monitor` expose other sessions'
     statements, like MySQL's PROCESS. CREATEDB is allowed. The filter runs
     on the server and returns one row, so no cap on memberships can hide a
     refused role; any error refuses. `BEGIN READ ONLY`;
     `SET LOCAL statement_timeout`, `lock_timeout`,
     `idle_in_transaction_session_timeout`, and `standard_conforming_strings
     = on`; prepare with the extended protocol (refuses several statements),
     then run the same text with the simple protocol so every column is the
     server's text; `ROLLBACK` and close, or drop the connection.
   - *MySQL/MariaDB*: one fresh connection per call; utf8mb4 pinned and
     confirmed (`character_set_client`, `_connection`, `_results`); refuse
     FILE, SUPER, PROCESS, CONNECTION_ADMIN (MariaDB `CONNECTION ADMIN`),
     global ALL, or PROXY, held directly or through a role (grant lines are
     tokenized with quoted names honoured, so a role named `read ON all` is
     a role; any line that does not parse refuses): MariaDB walks
     the role graph with `SHOW GRANTS FOR role`; MySQL lists granted roles'
     privileges with `SHOW GRANTS … USING`, and an account with a role
     granted to a role (from `information_schema.APPLICABLE_ROLES`) is
     refused because those privileges cannot be listed without the `mysql`
     schema; any grant line it cannot read refuses the account. `sql_mode`
     loses `ANSI_QUOTES`, `NO_BACKSLASH_ESCAPES`, and `ANSI` and is
     confirmed; `max_execution_time` (MariaDB `max_statement_time`) and lock
     wait timeouts; `START TRANSACTION READ ONLY`; prepare, then text
     protocol; roll back and close, or drop. MariaDB stops `BENCHMARK` (and
     `SLEEP`) at its limit and returns a value with no error (seen in CI), so
     a query that leaves an interruption warning (1317, 1028, 1969, 3024 in
     `SHOW WARNINGS`), or whose run reached its deadline (within 2% or
     10 ms), answers `sql_timeout` instead of a result: a cut result is never
     presented as complete, at the cost of refusing a query that finishes
     just at its limit.
   - One connection per call means nothing a statement leaves in the session
     (advisory or named locks, `set_config`, user variables, temporary
     objects) survives the call.
6. **The connection comes only from the URL (owner intent, stated as a
   rule).** User, host, and database must be in the URL. Driver options are
   built once from the URL's own parts with every field set explicitly; the
   TLS policy is decided from that same options object, so a second parse
   cannot disagree with it. Only these parameters are accepted, each at most
   once (duplicates and alias pairs refused): Postgres `sslmode`,
   `sslrootcert`, `sslcert`, `sslkey`; MySQL `ssl-mode`, `ssl-ca`,
   `ssl-cert`, `ssl-key`, `socket`. `charset`, `options`, `host`,
   `hostaddr`, `application_name`, and everything else are refused. If any
   of `PGHOST`, `PGHOSTADDR`, `PGPORT`, `PGUSER`, `PGPASSWORD`,
   `PGPASSFILE`, `PGDATABASE`, `PGOPTIONS`, `PGSSLMODE`, `PGSSLROOTCERT`,
   `PGSSLCERT`, `PGSSLKEY`, or `PGAPPNAME` is set in the server's
   environment, a Postgres target answers `sql_not_configured` naming that
   variable (never its value); the password file is never read. The
   variables the driver reads whatever the settings say (`PGHOSTADDR`,
   `PGOPTIONS`, `PGSSL*`, and `PGPASSWORD` when the URL has no password) are
   also checked in the real process environment for settings built with an
   explicit value, and a built options object carrying `PGOPTIONS` is
   refused. `SSL_CERT_FILE` and `SSL_CERT_DIR`, which the native certificate
   loader reads, refuse a target only where certificates are verified
   (a remote host, or loopback with `verify-ca`/`verify-full`): they
   change nothing for SQLite, plain-text, or unverified prefer/require
   local connections, and stock environments such as GitHub's Ubuntu
   runner image set `SSL_CERT_FILE`. With a private CA named in the URL the
   variable is still refused, because sqlx loads the system roots (which
   the variable replaces) in addition to the named file and offers no way
   to skip them; the message names the variable and the parameter. All
   environment reads happen once, when the settings are made from the
   process environment; settings built with an explicit value read none. A private
   CA is configured through the one variable instead: `sslrootcert`
   (Postgres) or `ssl-ca` (MySQL/MariaDB) names an absolute path to a
   regular, readable PEM file of at most 1 MiB holding a certificate, not a
   symbolic link (checked at startup; the error names only "the configured
   root certificate"). It is added to the operating system's roots, and
   verification stays on (no option disables it); `sslcert`/`sslkey`
   client files get the same checks. Such
   variables are common (Postgres installers; CI images set `PGUSER`).
7. **Transport.** Plain text only to a loopback address or a local socket;
   any other host gets TLS with certificate and host-name verification
   (Postgres `verify-full`, MySQL `VERIFY_IDENTITY`), a weaker mode is a
   configuration error, and there is no invalid-certificate option.
8. **Bounds.** `max_rows` (default 100, at most 1,000); at most 100 columns
   (`columns_omitted` counts the rest); names cut to 64 characters, metadata
   values to 256; text cells cut at 1,000 characters; binary cells shown as
   their size; C0 controls (except tab, LF, CR), C1 controls, every Unicode
   15.1 Cf character (zero-width and bidirectional marks, U+FEFF, the ZWJ
   inside emoji sequences too), U+2028/U+2029, and the tag block written as
   `\uXXXX` text in values and names, in every engine; a result (names and
   rows) at most 256 KiB; then the JSON is
   fitted to `max_tokens` (300–8,000) by dropping trailing rows and, if
   needed, columns. `timeout_ms` 100–30,000. The network engines run on a
   thread of their own, so a call from any thread, including one already
   inside a Tokio runtime, cannot panic. One call is bounded end to end:
   connect (10 s), setup and statement (`timeout_ms` plus 2 s), and closing
   (`ROLLBACK` and close at most 2 s each, after which the connection is
   dropped, never awaited); `SqlService::call_limit` states it and a test
   holds a stalled peer to it. MCP tool calls run one at a time per server,
   so a slow SQL call delays every other tool until it ends.
9. **Secrets.** The URL and its parts never leave the crate except into the
   driver. Errors carry fixed codes and scrub every URL component (raw,
   decoded, encoded, any case), any `scheme://` URL, and any `password=`
   value. No public type prints the value. The report shows `sql <engine>`
   or `sql not configured`, never the variable's name. Driver statement
   logging is off.
10. **No ledger row** (D113's reason): `compresso_stats` does not count SQL
    calls and `compresso_explain` has no trace for them.

## What a model can still do (reasoned; not observed against a live server)

- **Postgres**, as a role without the refused roles, in `BEGIN READ ONLY`:
  read every table, view, and catalog the role may read; terminate or cancel
  other sessions of the same role (`pg_terminate_backend` and
  `pg_cancel_backend` are deny-listed, but a wrapper or `SECURITY DEFINER`
  function reaches them, and the read-only transaction does not stop them;
  a live test documents it); take advisory locks, held until the call's
  connection closes; call `dblink` or `postgres_fdw` if installed and granted
  (the remote side is a separate transaction that can write; deny-listed by
  name only); call a `SECURITY DEFINER` function owned by a privileged role
  (it runs with its owner's rights inside the same read-only transaction,
  which stops its writes to this database but not file or program access
  its owner has); load the server until the deadline (`pg_sleep`, heavy
  joins). `statement_timeout` stops a statement at the server only when the
  server checks for interrupts; the client drops the connection at the
  deadline plus 2 s, and the backend may keep running until it notices.
  Writes, sequences, large-object writes, and `NOTIFY` are refused by the
  read-only transaction (`25006`).
- **MySQL/MariaDB**, as an account without the refused privileges, in a read
  only transaction: read what the account may read; take named locks
  (`GET_LOCK`, deny-listed by name) and row locks with locking reads, held
  until the connection closes; run `SLEEP` and `BENCHMARK` until the
  deadline (`max_execution_time` covers `SELECT` only); write temporary
  tables through a stored routine; call a UDF with external effects if one is
  installed and granted.
- **SQLite**: read everything in that one file; make a query slow until the
  deadline; nothing else.
- **All engines**: the gate is not the server's parser; anything it admits
  that the server reads otherwise must still pass the read-only session.

## Least-privilege accounts (the operator's part)

The tool refuses powerful roles, but it cannot know which tables hold
secrets: a MySQL account with SELECT on the `mysql` schema, or a Postgres
role that can read catalog secrets, exposes them to the model. Supply an
account that can read only what the model should see, for example:

```sql
-- Postgres
CREATE ROLE sql_reader LOGIN PASSWORD '...';
GRANT CONNECT ON DATABASE app TO sql_reader;
GRANT USAGE ON SCHEMA public TO sql_reader;
GRANT SELECT ON ALL TABLES IN SCHEMA public TO sql_reader;
ALTER DEFAULT PRIVILEGES IN SCHEMA public GRANT SELECT ON TABLES TO sql_reader;

-- MySQL / MariaDB
CREATE USER 'sql_reader'@'%' IDENTIFIED BY '...';
GRANT SELECT ON app.* TO 'sql_reader'@'%';
```

## Known gaps in the build gates

- The capability check's shadow resolve removes `compresso-sql` and the
  real-tree path rule looks at workspace crates; a non-workspace local
  path crate that `compresso-sql` feature-unifies into a network capability
  is not caught.

## Consequences

- The default build links `sqlx`, rustls, and the native-certificate loader
  (67 new third-party packages and 6 version bumps in `Cargo.lock`; the
  binary size is in D115); a server started without `--sql-env` never opens
  a connection.
- The capability check runs `cargo tree` per member, once more in a
  temporary copy for the two exempt members, and inverted trees for their
  network packages; it takes a few seconds longer.
- The full surface grows by about 370 o200k tokens only when the flag is
  set; default surfaces and goldens are unchanged.
- The gate refuses some valid SQL (comments, `$`, non-ASCII names, glued
  numbers, columns named like denied words unless quoted, MySQL `LOCK IN
  SHARE MODE`); every refusal says what to rewrite.
