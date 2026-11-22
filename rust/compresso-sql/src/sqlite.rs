//! SQLite: one read-only connection per call to a local file.
//!
//! The file is opened read-only without URI parsing and without following a
//! final symbolic link, then hardened before any statement runs: defensive
//! mode on, untrusted schema (views and triggers may call only innocuous
//! functions), `query_only`, no attached databases, an authorizer that allows
//! only reads, a few metadata pragmas, and non-denied functions, and a
//! progress handler that stops the statement at its deadline or once
//! SQLite's memory rises 256 MiB above the call's start (temporary tables
//! and sorts are kept in memory, so they count). Extension
//! loading is never enabled. A path that is, or resolves into, Compresso's
//! own data is refused.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
use rusqlite::limits::Limit;
use rusqlite::types::ValueRef;
use rusqlite::{Connection, ErrorCode, OpenFlags, config::DbConfig};
use serde_json::Value;

use crate::cells::{self, RowCollector};
use crate::redact::Secrets;
use crate::{
    ColumnEntry, Connector, DescribeResult, ForeignKeyEntry, IndexEntry, MAX_TABLES, SqlAction,
    SqlEngine, SqlError, SqlOutcome, SqlRequest, TableEntry, TablesResult, gate,
};

/// Metadata pragmas a statement (including this crate's own `describe`
/// queries) may use; every other pragma is denied.
const ALLOWED_PRAGMAS: &[&str] = &[
    "table_info",
    "table_xinfo",
    "index_list",
    "index_info",
    "index_xinfo",
    "foreign_key_list",
];

/// Largest string, blob, or statement one connection makes (16 MiB).
const MAX_VALUE_BYTES: i32 = 16 * 1024 * 1024;

/// Progress-handler interval, in virtual machine instructions: small enough
/// that a statement copying 1 MB values stops within about 100 ms of its
/// deadline or memory budget.
const PROGRESS_STEPS: i32 = 100;

/// How far SQLite's memory may rise above its level at the start of a call
/// before the statement is stopped (`sql_memory_limit`): 256 MiB.
const MEMORY_BUDGET: i64 = 256 * 1024 * 1024;

/// A local SQLite database file.
pub(crate) struct SqliteTarget {
    path: PathBuf,
    protected: Vec<PathBuf>,
    secrets: Secrets,
}

impl SqliteTarget {
    pub(crate) fn new(path: PathBuf, protected: Vec<PathBuf>) -> Result<Self, String> {
        let mut secrets = Secrets::default();
        secrets.add(&path.to_string_lossy());
        if let Ok(canonical) = fs::canonicalize(&path) {
            secrets.add(&canonical.to_string_lossy());
        }
        Ok(Self {
            path,
            protected,
            secrets,
        })
    }

    pub(crate) fn scrub(&self, message: &str) -> String {
        self.secrets.scrub(message)
    }

    /// The file to open, after refusing anything that is or resolves into a
    /// protected path (by canonical path and, on Unix, by file identity, so
    /// a hard link is refused too).
    fn checked_path(&self) -> Result<PathBuf, SqlError> {
        let canonical = fs::canonicalize(&self.path).map_err(|_| {
            SqlError::not_configured("the SQLite file does not exist or cannot be resolved")
        })?;
        let metadata = fs::metadata(&canonical)
            .map_err(|_| SqlError::not_configured("the SQLite file cannot be read"))?;
        if !metadata.is_file() {
            return Err(SqlError::not_configured(
                "the SQLite path is not a regular file",
            ));
        }
        for protected in &self.protected {
            let Ok(protected_canonical) = fs::canonicalize(protected) else {
                continue;
            };
            let inside =
                protected_canonical.is_dir() && canonical.starts_with(&protected_canonical);
            if inside
                || canonical == protected_canonical
                || same_file(&metadata, &protected_canonical)
            {
                return Err(SqlError::refused(
                    "the SQLite target is Compresso's own data; point the variable at a different database",
                ));
            }
        }
        Ok(canonical)
    }

    fn open(&self, timeout: Duration) -> Result<Connection, SqlError> {
        self.open_bounded(timeout).map(|(connection, _)| connection)
    }

    /// Opens and hardens the connection; the flag turns true when the
    /// progress handler stopped a statement for memory, not time.
    fn open_bounded(&self, timeout: Duration) -> Result<(Connection, Arc<AtomicBool>), SqlError> {
        let path = self.checked_path()?;
        // No SQLITE_OPEN_URI: the path is a path, never `file:...?mode=rwc`.
        let flags = OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW;
        let mut connection = Connection::open_with_flags(&path, flags).map_err(map_error)?;
        let memory_stop = harden(&mut connection, timeout).map_err(map_error)?;
        Ok((connection, memory_stop))
    }
}

#[cfg(unix)]
fn same_file(metadata: &fs::Metadata, other: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    fs::metadata(other)
        .is_ok_and(|other| other.dev() == metadata.dev() && other.ino() == metadata.ino())
}

#[cfg(not(unix))]
fn same_file(_metadata: &fs::Metadata, _other: &Path) -> bool {
    false
}

/// SQLite's memory in use, process-wide (the library keeps the count).
fn memory_used() -> i64 {
    // SAFETY: sqlite3_memory_used takes no arguments, only reads a counter
    // the library maintains under its own mutex, and is safe to call from
    // any thread at any time.
    unsafe { rusqlite::ffi::sqlite3_memory_used() }
}

fn harden(connection: &mut Connection, timeout: Duration) -> rusqlite::Result<Arc<AtomicBool>> {
    connection.set_db_config(DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true)?;
    connection.set_db_config(DbConfig::SQLITE_DBCONFIG_TRUSTED_SCHEMA, false)?;
    connection.set_db_config(DbConfig::SQLITE_DBCONFIG_ENABLE_FTS3_TOKENIZER, false)?;
    connection.set_limit(Limit::SQLITE_LIMIT_ATTACHED, 0)?;
    // One value or statement at most 16 MiB: a single huge allocation
    // (randomblob, printf padding, doubling strings) fails at once instead of
    // running past the deadline, which the progress handler only checks
    // between instructions. A process-wide heap limit is not set: the same
    // SQLite library also serves Compresso's store.
    connection.set_limit(Limit::SQLITE_LIMIT_LENGTH, MAX_VALUE_BYTES)?;
    connection.set_limit(Limit::SQLITE_LIMIT_SQL_LENGTH, MAX_VALUE_BYTES)?;
    // Page cache for this connection at most 16 MiB.
    connection.pragma_update(None, "cache_size", -16_384)?;
    // Temporary tables, sorts, and recursive queues stay in memory, where the
    // progress handler below can bound them; on disk they grew 300-450 MB a
    // second with no limit (E17-50 confirmation review).
    connection.pragma_update(None, "temp_store", "MEMORY")?;
    connection.pragma_update(None, "query_only", true)?;
    connection.busy_timeout(timeout)?;
    connection.authorizer(Some(authorize))?;
    let deadline = Instant::now() + timeout;
    let baseline = memory_used();
    let memory_stop = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&memory_stop);
    connection.progress_handler(
        PROGRESS_STEPS,
        Some(move || {
            if memory_used().saturating_sub(baseline) > MEMORY_BUDGET {
                flag.store(true, Ordering::Relaxed);
                return true;
            }
            Instant::now() >= deadline
        }),
    )?;
    Ok(memory_stop)
}

fn authorize(context: AuthContext<'_>) -> Authorization {
    match context.action {
        AuthAction::Select | AuthAction::Read { .. } | AuthAction::Recursive => {
            Authorization::Allow
        }
        AuthAction::Function { function_name } => {
            if gate::check_statement(&format!("SELECT {function_name}()"), SqlEngine::Sqlite)
                .is_ok()
            {
                Authorization::Allow
            } else {
                Authorization::Deny
            }
        }
        AuthAction::Pragma { pragma_name, .. }
            if ALLOWED_PRAGMAS.contains(&pragma_name.to_ascii_lowercase().as_str()) =>
        {
            Authorization::Allow
        }
        _ => Authorization::Deny,
    }
}

fn map_error(error: rusqlite::Error) -> SqlError {
    if let rusqlite::Error::SqliteFailure(failure, message) = &error {
        match failure.code {
            ErrorCode::OperationInterrupted => {
                return SqlError::new(
                    "sql_timeout",
                    "the statement did not finish within timeout_ms and was stopped; narrow it or raise timeout_ms.",
                );
            }
            ErrorCode::AuthorizationForStatementDenied => {
                return SqlError::refused(format!(
                    "SQLite refused the statement in this read-only session: {}",
                    message.as_deref().unwrap_or("not authorized")
                ));
            }
            // A WAL database whose shared-memory index or log cannot be
            // created or opened (a read-only directory, a recovery needed):
            // the statement wrote nothing.
            ErrorCode::ReadOnly if failure.extended_code != rusqlite::ffi::SQLITE_READONLY => {
                return SqlError::new(
                    "sql_wal_unavailable",
                    "this WAL-mode database cannot be read read-only here: SQLite needs to create or update its -wal and -shm files beside it (a read-only directory, or a log that needs recovery). Checkpoint it (PRAGMA wal_checkpoint(TRUNCATE)) or switch it to journal_mode=DELETE, or make the directory writable.",
                );
            }
            ErrorCode::ReadOnly => {
                return SqlError::new(
                    "sql_read_only",
                    "the database is opened read-only; the statement tried to write",
                );
            }
            ErrorCode::CannotOpen | ErrorCode::NotADatabase | ErrorCode::PermissionDenied => {
                return SqlError::not_configured("the SQLite file cannot be opened as a database");
            }
            _ => {}
        }
    }
    if matches!(error, rusqlite::Error::MultipleStatement) {
        return SqlError::refused("exactly one statement is allowed");
    }
    SqlError::new("sql_error", format!("SQLite: {error}"))
}

fn cell(value: ValueRef<'_>) -> Value {
    match value {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(integer) => Value::from(integer),
        ValueRef::Real(real) => serde_json::Number::from_f64(real)
            .map_or_else(|| Value::from(real.to_string()), Value::Number),
        ValueRef::Text(bytes) => cells::text(&String::from_utf8_lossy(bytes)),
        ValueRef::Blob(bytes) => cells::binary(bytes.len()),
    }
}

impl Connector for SqliteTarget {
    fn run(&self, request: &SqlRequest, gated: bool) -> Result<SqlOutcome, SqlError> {
        let (connection, memory_stop) = self.open_bounded(request.timeout)?;
        let outcome = self.run_on(&connection, request, gated);
        outcome.map_err(|error| {
            if error.code() == "sql_timeout" && memory_stop.load(Ordering::Relaxed) {
                SqlError::new(
                    "sql_memory_limit",
                    "the statement needed more than 256 MiB of memory and was stopped; narrow it (filters, LIMIT, smaller values)",
                )
            } else {
                error
            }
        })
    }
}

impl SqliteTarget {
    fn run_on(
        &self,
        connection: &Connection,
        request: &SqlRequest,
        gated: bool,
    ) -> Result<SqlOutcome, SqlError> {
        match &request.action {
            SqlAction::Query(sql) => {
                if gated {
                    gate::check_statement(sql, SqlEngine::Sqlite).map_err(SqlError::refused)?;
                }
                query(connection, sql, request.max_rows).map(SqlOutcome::Query)
            }
            SqlAction::Tables => tables(connection).map(SqlOutcome::Tables),
            SqlAction::Describe(table) => describe(connection, table).map(SqlOutcome::Describe),
        }
    }
}

fn query(
    connection: &Connection,
    sql: &str,
    max_rows: usize,
) -> Result<crate::QueryResult, SqlError> {
    let started = Instant::now();
    let mut statement = connection.prepare(sql).map_err(map_error)?;
    if !statement.readonly() {
        return Err(SqlError::refused(
            "SQLite reports that this statement writes; only read statements run",
        ));
    }
    let columns = statement
        .column_names()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let width = columns.len();
    let mut rows = statement.query([]).map_err(map_error)?;
    let mut collector = RowCollector::new(max_rows, columns);
    while let Some(row) = rows.next().map_err(map_error)? {
        let cells = (0..width)
            .map(|index| row.get_ref(index).map(cell))
            .collect::<Result<Vec<_>, _>>()
            .map_err(map_error)?;
        if !collector.push(cells) {
            break;
        }
    }
    let elapsed = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    Ok(collector.finish(elapsed))
}

fn tables(connection: &Connection) -> Result<TablesResult, SqlError> {
    let mut statement = connection
        .prepare(
            "SELECT name, type FROM sqlite_schema WHERE type IN ('table', 'view') AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\' ORDER BY name LIMIT ?1",
        )
        .map_err(map_error)?;
    let limit = i64::try_from(MAX_TABLES + 1).unwrap_or(i64::MAX);
    let mut tables = statement
        .query_map([limit], |row| {
            Ok(TableEntry {
                schema: None,
                name: row.get(0)?,
                kind: row.get(1)?,
                rows_estimate: None,
            })
        })
        .map_err(map_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_error)?;
    let truncated = tables.len() > MAX_TABLES;
    tables.truncate(MAX_TABLES);
    Ok(TablesResult { tables, truncated })
}

fn describe(connection: &Connection, table: &str) -> Result<DescribeResult, SqlError> {
    let exists = connection
        .query_row(
            "SELECT count(*) FROM sqlite_schema WHERE type IN ('table', 'view') AND name = ?1",
            [table],
            |row| row.get::<_, i64>(0),
        )
        .map_err(map_error)?;
    if exists == 0 {
        return Err(SqlError::new(
            "sql_not_found",
            "no table or view of that name; call compresso_sql with action tables",
        ));
    }
    let mut statement = connection
        .prepare("SELECT name, type, \"notnull\", dflt_value, pk FROM pragma_table_xinfo(?1) ORDER BY cid")
        .map_err(map_error)?;
    let columns = statement
        .query_map([table], |row| {
            Ok(ColumnEntry {
                name: row.get(0)?,
                data_type: row.get(1)?,
                nullable: row.get::<_, i64>(2)? == 0,
                default: row.get(3)?,
                primary_key: row.get::<_, i64>(4)? > 0,
            })
        })
        .map_err(map_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_error)?;
    let mut statement = connection
        .prepare("SELECT name, \"unique\" FROM pragma_index_list(?1) ORDER BY seq")
        .map_err(map_error)?;
    let index_names = statement
        .query_map([table], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? != 0))
        })
        .map_err(map_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_error)?;
    let mut indexes = Vec::new();
    let mut statement = connection
        .prepare("SELECT name FROM pragma_index_info(?1) ORDER BY seqno")
        .map_err(map_error)?;
    for (name, unique) in index_names {
        let columns = statement
            .query_map([&name], |row| row.get::<_, Option<String>>(0))
            .map_err(map_error)?
            .map(|column| column.map(|column| column.unwrap_or_else(|| "<expression>".to_owned())))
            .collect::<Result<Vec<_>, _>>()
            .map_err(map_error)?;
        indexes.push(IndexEntry {
            name,
            unique,
            columns,
        });
    }
    let mut statement = connection
        .prepare("SELECT id, \"table\", \"from\", \"to\" FROM pragma_foreign_key_list(?1) ORDER BY id, seq")
        .map_err(map_error)?;
    let rows = statement
        .query_map([table], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })
        .map_err(map_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_error)?;
    let mut foreign_keys: Vec<(i64, ForeignKeyEntry)> = Vec::new();
    for (id, references_table, from, to) in rows {
        let to = to.unwrap_or_else(|| "<primary key>".to_owned());
        match foreign_keys.last_mut() {
            Some((last, entry)) if *last == id => {
                entry.columns.push(from);
                entry.references_columns.push(to);
            }
            _ => foreign_keys.push((
                id,
                ForeignKeyEntry {
                    columns: vec![from],
                    references_table,
                    references_columns: vec![to],
                },
            )),
        }
    }
    Ok(DescribeResult {
        table: table.to_owned(),
        columns,
        indexes,
        foreign_keys: foreign_keys.into_iter().map(|(_, entry)| entry).collect(),
    })
}

#[cfg(test)]
mod tests {
    //! The database-level controls alone, with the statement gate bypassed:
    //! each must hold even if the gate let a statement through.
    use super::*;

    fn database() -> (tempfile::TempDir, SqliteTarget, Vec<u8>) {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("app.db");
        let setup = Connection::open(&path).expect("create");
        setup
            .execute_batch(
                "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT);
                 INSERT INTO t (name) VALUES ('a'), ('b');
                 CREATE TRIGGER grow AFTER INSERT ON t BEGIN INSERT INTO t (name) VALUES ('x'); END;",
            )
            .expect("schema");
        drop(setup);
        let bytes = fs::read(&path).expect("bytes");
        let target = SqliteTarget::new(path, Vec::new()).expect("target");
        (directory, target, bytes)
    }

    fn ungated(target: &SqliteTarget, sql: &str) -> Result<crate::QueryResult, SqlError> {
        let connection = target.open(Duration::from_secs(5))?;
        query(&connection, sql, 100)
    }

    #[test]
    fn test_database_level_controls_hold_without_the_gate() {
        let (_directory, target, before) = database();
        for sql in [
            "INSERT INTO t (name) VALUES ('c')",
            "UPDATE t SET name = 'z'",
            "DELETE FROM t",
            "CREATE TABLE u (a)",
            "DROP TABLE t",
            "ATTACH DATABASE ':memory:' AS other",
            "PRAGMA query_only = 0",
            "PRAGMA writable_schema = 1",
            "PRAGMA journal_mode = DELETE",
            "PRAGMA database_list",
            "SELECT load_extension('x')",
            "VACUUM",
            "BEGIN IMMEDIATE",
        ] {
            let error = ungated(&target, sql).expect_err(sql);
            assert!(
                matches!(error.code(), "sql_refused" | "sql_read_only" | "sql_error"),
                "{sql}: {error}"
            );
        }
        assert_eq!(fs::read(&target.path).expect("bytes"), before);
        let rows = ungated(&target, "SELECT name FROM t ORDER BY id").expect("read");
        assert_eq!(
            rows.rows,
            vec![vec![Value::from("a")], vec![Value::from("b")]]
        );
    }

    #[test]
    fn test_temporary_tables_and_sorts_stay_in_memory() {
        let (_directory, target, _) = database();
        let (mut connection, _) = target.open_bounded(Duration::from_secs(5)).expect("open");
        connection
            .authorizer(None::<fn(AuthContext<'_>) -> Authorization>)
            .expect("authorizer off for the check");
        let store: i64 = connection
            .pragma_query_value(None, "temp_store", |row| row.get(0))
            .expect("temp_store");
        assert_eq!(store, 2, "temp_store must be MEMORY");
    }

    #[test]
    fn test_the_connection_is_opened_read_only() {
        let (_directory, target, _) = database();
        let connection = target.open(Duration::from_secs(5)).expect("open");
        assert!(connection.is_readonly(rusqlite::MAIN_DB).expect("main"));
    }

    #[test]
    fn test_metadata_pragmas_used_by_describe_are_allowed() {
        let (_directory, target, _) = database();
        let rows = ungated(&target, "SELECT name FROM pragma_table_info('t')").expect("pragma");
        assert_eq!(rows.row_count, 2);
    }
}
