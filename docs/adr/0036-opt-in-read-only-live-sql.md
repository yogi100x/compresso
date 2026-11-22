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

**Postgres and MySQL/MariaDB have not been run against a live server.** The
live tests exist and a CI job (`live-sql`) runs them against throwaway
containers; until that job has passed, every statement below about those
engines is reasoned from their documentation and `sqlx` 0.9.1's source.

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
   only.** A first gate that lexed each dialect faithfully was bypassed six
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
     allowed functions, `stmt.readonly()` required, a progress-handler
     deadline, and a busy timeout. No process-wide heap limit: the same
     library serves Compresso's store. The store is refused by path,
     directory, and (Unix) file identity.
   - *WAL databases*: a reader may create `-wal` and `-shm` beside the
     database (SQLite needs its shared-memory index); the database's own
     bytes are never written. In a read-only directory SQLite cannot, and
     the call answers `sql_wal_unavailable` with what to do. `immutable=1`
     was rejected deliberately: on a database another process is writing it
     ignores the log and locks, so it can return stale or inconsistent rows
     or report corruption.
   - *Postgres*: one fresh connection per call; refuse a superuser or a
     member of `pg_read_server_files`, `pg_write_server_files`,
     `pg_execute_server_program`, or `pg_signal_backend`; `BEGIN READ ONLY`;
     `SET LOCAL statement_timeout`, `lock_timeout`,
     `idle_in_transaction_session_timeout`, and `standard_conforming_strings
     = on`; prepare with the extended protocol (refuses several statements),
     then run the same text with the simple protocol so every column is the
     server's text; `ROLLBACK` and close, or drop the connection.
   - *MySQL/MariaDB*: one fresh connection per call; utf8mb4 pinned and
     confirmed (`character_set_client`, `_connection`, `_results`); refuse
     FILE, SUPER, PROCESS, CONNECTION_ADMIN (MariaDB `CONNECTION ADMIN`),
     global ALL, or PROXY, held directly or through a role: MariaDB walks
     the role graph with `SHOW GRANTS FOR role`; MySQL lists granted roles'
     privileges with `SHOW GRANTS … USING`, and an account with a role
     granted to a role (from `information_schema.APPLICABLE_ROLES`) is
     refused because those privileges cannot be listed without the `mysql`
     schema; any grant line it cannot read refuses the account. `sql_mode`
     loses `ANSI_QUOTES`, `NO_BACKSLASH_ESCAPES`, and `ANSI` and is
     confirmed; `max_execution_time` (MariaDB `max_statement_time`) and lock
     wait timeouts; `START TRANSACTION READ ONLY`; prepare, then text
     protocol; roll back and close, or drop.
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
   variable (never its value); the password file is never read.
7. **Transport.** Plain text only to a loopback address or a local socket;
   any other host gets TLS with certificate and host-name verification
   (Postgres `verify-full`, MySQL `VERIFY_IDENTITY`), a weaker mode is a
   configuration error, and there is no invalid-certificate option.
8. **Bounds.** `max_rows` (default 100, at most 1,000); at most 100 columns
   (`columns_omitted` counts the rest); names cut to 64 characters, metadata
   values to 256; text cells cut at 1,000 characters; binary cells shown as
   their size; a result (names and rows) at most 256 KiB; then the JSON is
   fitted to `max_tokens` (300–8,000) by dropping trailing rows and, if
   needed, columns. `timeout_ms` 100–30,000. One call is bounded end to end:
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
