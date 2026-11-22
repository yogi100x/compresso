# Local slice installer

`installers/install-local.sh` installs the checksum-verified D42 slice release at
`~/.local/bin/compresso`. The checksum manifest is signed by the main-branch CI workflow and the
installer verifies that signature with `cosign` (ADR-0030); the binary itself carries no native
platform signature until E12. It does not sign in, collect telemetry, start services, or modify
Compresso, Claude, Codex, Cursor, or other assistant configuration.

## Supported platforms

The POSIX installer maps the host to one of the E1-06 artifacts:

| Host | Release target |
|---|---|
| Apple Silicon macOS | `aarch64-apple-darwin` |
| Intel macOS | `x86_64-apple-darwin` |
| x86-64 Linux | `x86_64-unknown-linux-gnu` |

Windows uses the packaged `.exe` directly until the signed PowerShell installer arrives in E12.

## Install from local release artifacts

Place the platform tarball and the aggregated `SHA256SUMS` in one directory, then run:

```sh
installers/install-local.sh /path/to/release-artifacts
```

A direct tarball path is also accepted:

```sh
installers/install-local.sh /path/to/compresso-0.1.0-aarch64-apple-darwin.tar.gz
```

The sibling `SHA256SUMS` remains required. A directory must contain exactly one tarball matching
the current platform.

`SHA256SUMS.sigstore.json` must sit beside `SHA256SUMS`. The installer verifies it and stops if
the file is absent, `cosign` is missing, or `cosign` rejects it.

For artifacts you built on this machine there is no CI signature. Pass `--allow-unsigned` to
install them on the checksum alone; the installer prints a warning that the publisher signature was
not verified. The flag is refused for a URL source.

```sh
installers/install-local.sh --allow-unsigned /path/to/locally-built-artifacts
```

## Install from a URL

Pass an immutable, versioned `https://` tarball URL. Plain HTTP is rejected, redirects may not
leave HTTPS with `curl`, and the `wget` fallback follows no redirect at all. The installer fetches
`SHA256SUMS` and `SHA256SUMS.sigstore.json` from the same directory. The checksum guards against
corruption; the signature guards against a substituted release, because it must come from the
Compresso `ci.yml` workflow on `main` and that identity is fixed inside the installer.

Signature verification requires [`cosign`](https://docs.sigstore.dev/cosign/system_config/installation/)
3.0 or newer on `PATH`; CI signs with cosign 3, whose bundle format older releases do not read by
default. A missing signature file, a missing `cosign`, or a rejected signature stops the install
before anything is written. `cosign` contacts Sigstore's public infrastructure to refresh its
trust root unless it is configured for offline verification.

```sh
installers/install-local.sh \
  https://releases.example/compresso/0.1.0/compresso-0.1.0-aarch64-apple-darwin.tar.gz
```

URL directory listing and mutable “latest” resolution are intentionally unsupported. Native
platform signing, notarization, and package managers arrive in E12.

## Verification and replacement behavior

The installer downloads or copies both files to a temporary directory, finds the exact tarball
entry in `SHA256SUMS`, and compares the expected and actual SHA-256 digests. A mismatch reports
both values and stops before creating or changing anything under `~/.local/bin`.

Only a verified archive containing one regular file named `compresso` is accepted. The binary is
copied to a temporary file inside `~/.local/bin`, changed to mode `0755`, and atomically renamed to
`compresso`. When an installation already exists, its bytes are first preserved atomically as
`compresso.previous`. Re-running the installer is safe and makes the immediately prior binary the
backup.

To restore that backup manually:

```sh
cp "$HOME/.local/bin/compresso.previous" "$HOME/.local/bin/compresso"
chmod 0755 "$HOME/.local/bin/compresso"
```

A binary released before store migration 0008 (D89) refuses a store that a newer build has opened,
and fails with `InvalidMigrationHistory`. To go back to such a binary, reinstall it with a fresh
store and run `compresso index` in each repository.

If `~/.local/bin` is absent from `PATH`, the installer prints the shell export to add it. It never
edits shell startup files.

## Post-install checks

```sh
compresso --version
compresso doctor
```

If verification fails, confirm that the tarball and `SHA256SUMS` came from the same release. If the
platform is unsupported, use a matching E1-06 artifact or build locally; do not rename an artifact
for a different target.

## Optional: read-only live SQL queries

`compresso_sql` (E17-50, ADR-0036) is off unless you turn it on.
**Postgres and MariaDB support has been run only by CI tests against
throwaway containers, and MySQL not at all**; only SQLite is fully tested.
Enable it for those engines knowing that. Put the
database URL in an environment variable whose name starts with
`COMPRESSO_SQL_`, in the environment your coding client starts from (your
shell profile or the client's own launch environment; Compresso never reads
`.env` files and never stores the URL):

```sh
export COMPRESSO_SQL_APP='postgres://reader@db.internal:5432/app'
compresso integrations install claude-code --sql-env COMPRESSO_SQL_APP
```

The entry stores only the name. Other clients can start
`compresso mcp serve --sql-env COMPRESSO_SQL_APP` from their own
configuration. Accepted values: `sqlite:/absolute/path.db` (no
parameters), `postgres://user[:password]@host[:port]/database`, and
`mysql://user[:password]@host[:port]/database`; user, host, and database are
required, and only the TLS parameters (and MySQL `socket`) are accepted. The
connection comes only from that URL: while any `PG*` variable such as
`PGUSER`, `PGPASSWORD`, or `PGHOST` is set in the client's environment, a
Postgres target is refused with an error naming the variable, and
`~/.pgpass` is never read. Such variables are common (Postgres installers,
some CI images set `PGUSER`); unset them in the environment the client
starts from. Use a role that can only
read: Compresso refuses a Postgres role that is, or is a member of, a
superuser, BYPASSRLS, CREATEROLE, or REPLICATION role or any predefined
`pg_` role (including `pg_read_all_data`, which can read password hashes),
and a MySQL/MariaDB account with FILE, SUPER, PROCESS, CONNECTION_ADMIN,
PROXY, or ALL PRIVILEGES on `*.*`, directly or through roles. It cannot know
which of your tables hold secrets: an account that can read the `mysql`
schema or secret tables exposes them. A least-privilege account:

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

`SSL_CERT_FILE` and `SSL_CERT_DIR` must not be set where the client
starts, as for `PG*` variables. On macOS and Linux a SQLite file with more
than one hard link is refused. On Windows only the path checks apply: a
hard link to a file in Compresso's data directory is not detected there, so
do not point the variable at a hard link. Statements must be plain SQL: no comments,
`$`, backslashes, or non-ASCII outside `'...'` strings. Compresso answers one
tool call at a time, so a slow query delays its other tools until it ends
(at most the statement's `timeout_ms` plus about 16 s). Reading a WAL-mode
SQLite database may create its `-wal` and `-shm` files beside it. Any host other than loopback or a local socket needs
TLS with certificate verification (`sslmode=verify-full`,
`ssl-mode=VERIFY_IDENTITY`, or no mode at all). Results go to your coding
client and may be sent to its model provider.

## Test the installer

The test suite builds deterministic stub artifacts through the E1-06 packager and exercises local
and URL resolution, checksum failure, atomic replacement, mode `0755`, PATH guidance, and config
immutability:

```sh
installers/tests/install_local_test.sh
```
