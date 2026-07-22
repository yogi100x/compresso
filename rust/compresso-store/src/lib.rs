//! Local SQLite persistence boundaries for repositories, files, symbols, and operations.

mod files;
mod ops;
mod receipts;
mod repos;
mod symbols;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use thiserror::Error;

pub use files::{FileLanguage, FileRecord, RelativePathError};
pub use ops::{
    DetailedStatsSummary, ModeledCostSaved, OperationId, OperationRecord, SessionId, SessionRecord,
    StatsFilter, StatsSummary, ToolStatsSummary,
};
pub use receipts::{ReceiptId, ReceiptRecord};
pub use repos::{FileId, IndexVersionRecord, RepoId, RepoRecord, RepoRemovalPreview};
pub use symbols::{
    EdgeKind, EdgeRecord, EdgeResolution, EdgeResolutionCounts, ResolvedEdgeRecord, SymbolHit,
    SymbolKind, SymbolRecord, SymbolSelector,
};

const BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const WAL_RETRY_INITIAL_DELAY: Duration = Duration::from_millis(1);
const WAL_RETRY_MAX_DELAY: Duration = Duration::from_millis(25);
const SECONDS_PER_DAY: u64 = 24 * 60 * 60;

/// Default retention window for operation metadata.
pub const DEFAULT_OPERATION_RETENTION: Duration = Duration::from_secs(90 * SECONDS_PER_DAY);

const MIGRATIONS: &[Migration] = &[Migration {
    version: 1,
    sql: include_str!("../migrations/0001_init.sql"),
}];

#[derive(Clone, Copy)]
struct Migration {
    version: i64,
    sql: &'static str,
}

/// Options applied when opening a store.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StoreOptions {
    /// Operation rows older than this window are removed at startup.
    pub operation_retention: Duration,
}

impl Default for StoreOptions {
    fn default() -> Self {
        Self {
            operation_retention: DEFAULT_OPERATION_RETENTION,
        }
    }
}

/// A blocking, cheaply cloneable handle to the local SQLite database.
#[derive(Clone)]
pub struct Store {
    connection: Arc<Mutex<Connection>>,
}

/// Errors returned while opening or migrating the local store.
#[derive(Debug, Error)]
pub enum StoreError {
    /// The database's parent directory could not be created.
    #[error("failed to create store directory {path}")]
    CreateDirectory {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// A repository root could not be resolved to an existing filesystem path.
    #[error("failed to canonicalize repository root {path}")]
    CanonicalizeRepository {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// A repository root resolves to something other than a directory.
    #[error("repository root is not a directory: {path}")]
    RepositoryRootNotDirectory { path: PathBuf },
    /// Repository removal requires the exact absolute root persisted at approval time.
    #[error("repository root must be an absolute stored path: {path}")]
    RepositoryRootNotAbsolute { path: PathBuf },
    /// A filesystem path cannot be represented by SQLite's TEXT storage.
    #[error("path is not valid UTF-8: {path}")]
    NonUtf8Path { path: PathBuf },
    /// SQLite could not open the database file.
    #[error("failed to open store database {path}")]
    Open {
        path: PathBuf,
        #[source]
        source: rusqlite::Error,
    },
    /// A SQLite operation failed outside a migration.
    #[error("store database error")]
    Database(#[from] rusqlite::Error),
    /// SQLite could not enable the WAL journal mode required by the store.
    #[error("store requires WAL journal mode, but SQLite returned {actual}")]
    JournalModeMismatch { actual: String },
    /// A repository-relative path failed lexical validation.
    #[error("invalid repository-relative path {path:?}: {reason}")]
    InvalidRelativePath {
        path: String,
        reason: RelativePathError,
    },
    /// A requested repository does not exist.
    #[error("repository {0} does not exist")]
    RepositoryNotFound(RepoId),
    /// A repository approval changed after a destructive removal was previewed.
    #[error("repository approval changed after removal preview: {path}")]
    RepositoryApprovalChanged { path: PathBuf },
    /// A requested file does not exist.
    #[error("file {0} does not exist")]
    FileNotFound(FileId),
    /// A required record field was semantically invalid.
    #[error("invalid {field}: {reason}")]
    InvalidField {
        field: &'static str,
        reason: &'static str,
    },
    /// An unsigned Rust value cannot fit SQLite's signed INTEGER storage.
    #[error("{field} value {value} exceeds SQLite INTEGER range")]
    NumericOutOfRange { field: &'static str, value: u128 },
    /// A symbol line interval is not one-based and inclusive.
    #[error("invalid symbol line range {start_line}..={end_line}")]
    InvalidSymbolLineRange { start_line: u32, end_line: u32 },
    /// An edge marked resolved does not identify a stored file in the same repository.
    #[error("resolved edge target does not exist in repository {repo}: {path}")]
    ResolvedEdgeTargetNotFound { repo: RepoId, path: String },
    /// An edge's resolution payload is inconsistent with its status.
    #[error("invalid edge resolution for {path:?}: {reason}")]
    InvalidEdgeResolution { path: String, reason: &'static str },
    /// A caller supplied malformed full-text search input.
    #[error("invalid symbol search query: {reason}")]
    InvalidSearchQuery { reason: &'static str },
    /// A selector signature hash was not a 32-byte lowercase/uppercase hexadecimal digest.
    #[error("invalid BLAKE3 signature hash: {0}")]
    InvalidSignatureHash(String),
    /// Persisted data does not match the typed store contract.
    #[error("invalid stored {field} value: {value}")]
    InvalidStoredValue { field: &'static str, value: String },
    /// A caller named an operation tool outside the fixed first-slice ledger vocabulary.
    #[error("unknown operation tool {0:?}")]
    UnknownOperationTool(String),
    /// A compression operation omitted its required root trace document.
    #[error("compression tool {tool:?} requires trace_json")]
    MissingTrace { tool: String },
    /// A retrieval operation supplied a trace forbidden by D47.
    #[error("retrieval tool {tool:?} must not carry trace_json")]
    UnexpectedTrace { tool: String },
    /// A retrieval operation omitted its required served-token count.
    #[error("retrieval tool {tool:?} requires tokens_served")]
    MissingTokensServed { tool: String },
    /// A compression operation supplied retrieval-only served-token accounting.
    #[error("compression tool {tool:?} must not carry tokens_served")]
    UnexpectedTokensServed { tool: String },
    /// A compression trace failed JSON parsing or root-schema validation.
    #[error("invalid compression trace: {reason}")]
    InvalidTrace { reason: String },
    /// The operation row and its trace disagree on their primary identity.
    #[error(
        "operation id {operation_id:?} does not match trace operation id {trace_operation_id:?}"
    )]
    OperationTraceIdMismatch {
        operation_id: String,
        trace_operation_id: String,
    },
    /// A referenced session does not exist.
    #[error("session {0} does not exist")]
    SessionNotFound(String),
    /// A requested integration receipt does not exist.
    #[error("integration receipt {0} does not exist")]
    ReceiptNotFound(ReceiptId),
    /// JSON serialization failed at a Store persistence boundary.
    #[error("failed to encode {field} as JSON")]
    JsonEncoding {
        field: &'static str,
        #[source]
        source: serde_json::Error,
    },
    /// Persisted JSON is not the strict shape required by its typed Store DTO.
    #[error("malformed stored JSON in {field}: {reason}")]
    MalformedStoredJson { field: &'static str, reason: String },
    /// A migration failed and was rolled back.
    #[error("store migration {version} failed")]
    Migration {
        version: i64,
        #[source]
        source: rusqlite::Error,
    },
    /// Embedded migrations are not strictly increasing.
    #[error("embedded store migrations are not strictly increasing at version {version}")]
    InvalidMigrationOrder { version: i64 },
    /// The migration ledger is not a prefix of the embedded migrations.
    #[error("invalid store migration history: found {found:?}, expected {expected:?}")]
    InvalidMigrationHistory { found: Vec<i64>, expected: Vec<i64> },
    /// No platform data directory could be resolved.
    #[error("could not resolve the platform data directory")]
    DataDirectoryUnavailable,
    /// The system clock predates the Unix epoch.
    #[error("system clock predates the Unix epoch")]
    InvalidSystemTime(#[from] std::time::SystemTimeError),
    /// A prior panic poisoned the in-process connection lock.
    #[error("store connection lock is poisoned")]
    ConnectionPoisoned,
}

impl Store {
    /// Opens a database and applies the default store options.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        Self::open_with_options(path, StoreOptions::default())
    }

    /// Opens the database at the platform-specific Compresso data path.
    pub fn open_default() -> Result<Self, StoreError> {
        let path = default_database_path()?;
        Self::open(&path)
    }

    /// Opens a database with an explicit operation-retention policy.
    pub fn open_with_options(path: &Path, options: StoreOptions) -> Result<Self, StoreError> {
        Self::open_with_migrations(path, options, MIGRATIONS)
    }

    /// Returns SQLite's connection-local cumulative write count.
    ///
    /// Query acceptance tests use this value before and after read-only operations.
    pub fn total_changes(&self) -> Result<u64, StoreError> {
        Ok(self.connection()?.total_changes())
    }

    fn open_with_migrations(
        path: &Path,
        options: StoreOptions,
        migrations: &[Migration],
    ) -> Result<Self, StoreError> {
        create_parent_directory(path)?;
        let mut connection = Connection::open(path).map_err(|source| StoreError::Open {
            path: path.to_path_buf(),
            source,
        })?;
        configure_connection(&connection)?;
        migrate(&mut connection, migrations)?;
        prune_operations(&connection, options.operation_retention)?;

        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
        })
    }

    #[allow(dead_code)] // Shared by the DAO methods introduced in E3-02.
    fn connection(&self) -> Result<MutexGuard<'_, Connection>, StoreError> {
        self.connection
            .lock()
            .map_err(|_| StoreError::ConnectionPoisoned)
    }
}

fn default_database_path() -> Result<PathBuf, StoreError> {
    #[cfg(target_os = "windows")]
    let data_dir = dirs::data_local_dir();

    #[cfg(not(target_os = "windows"))]
    let data_dir = dirs::home_dir().map(|home| home.join(".local/share"));

    data_dir
        .map(|directory| directory.join("compresso/compresso.db"))
        .ok_or(StoreError::DataDirectoryUnavailable)
}

fn create_parent_directory(path: &Path) -> Result<(), StoreError> {
    let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    else {
        return Ok(());
    };
    fs::create_dir_all(parent).map_err(|source| StoreError::CreateDirectory {
        path: parent.to_path_buf(),
        source,
    })
}

fn configure_connection(connection: &Connection) -> Result<(), StoreError> {
    connection.pragma_update(None, "foreign_keys", "ON")?;
    let journal_mode = enable_wal_journal_mode(connection)?;
    if journal_mode != "wal" {
        return Err(StoreError::JournalModeMismatch {
            actual: journal_mode,
        });
    }
    Ok(())
}

fn enable_wal_journal_mode(connection: &Connection) -> Result<String, StoreError> {
    // Changing journal mode upgrades a read lock to a write lock. SQLite may
    // bypass the busy handler and return SQLITE_BUSY immediately to break a
    // simultaneous first-open deadlock, so retry the complete idempotent
    // PRAGMA under one bounded deadline and restore the normal timeout after.
    let deadline = Instant::now() + BUSY_TIMEOUT;
    let mut retry_delay = WAL_RETRY_INITIAL_DELAY;

    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        connection.busy_timeout(remaining.min(WAL_RETRY_MAX_DELAY))?;

        match connection.pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get(0)) {
            Ok(journal_mode) => {
                connection.busy_timeout(BUSY_TIMEOUT)?;
                return Ok(journal_mode);
            }
            Err(error) if error.sqlite_error_code() == Some(rusqlite::ErrorCode::DatabaseBusy) => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err(error.into());
                }
                std::thread::sleep(retry_delay.min(remaining));
                retry_delay = retry_delay.saturating_mul(2).min(WAL_RETRY_MAX_DELAY);
            }
            Err(error) => return Err(error.into()),
        }
    }
}

fn migrate(connection: &mut Connection, migrations: &[Migration]) -> Result<(), StoreError> {
    validate_migration_order(migrations)?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let applied = applied_migration_versions(&transaction)?;
    validate_migration_history(&applied, migrations)?;

    for migration in migrations
        .iter()
        .filter(|migration| !applied.contains(&migration.version))
    {
        apply_migration_in_transaction(&transaction, *migration)?;
    }
    transaction.commit()?;
    Ok(())
}

fn validate_migration_order(migrations: &[Migration]) -> Result<(), StoreError> {
    for pair in migrations.windows(2) {
        if pair[0].version >= pair[1].version {
            return Err(StoreError::InvalidMigrationOrder {
                version: pair[1].version,
            });
        }
    }
    Ok(())
}

fn applied_migration_versions(connection: &Connection) -> Result<Vec<i64>, StoreError> {
    let table_exists = connection
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'schema_migrations'",
            [],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if !table_exists {
        return Ok(Vec::new());
    }

    let mut statement =
        connection.prepare("SELECT version FROM schema_migrations ORDER BY version")?;
    let versions = statement
        .query_map([], |row| row.get(0))?
        .collect::<Result<Vec<i64>, _>>()?;
    Ok(versions)
}

fn validate_migration_history(applied: &[i64], migrations: &[Migration]) -> Result<(), StoreError> {
    let expected = migrations
        .iter()
        .take(applied.len())
        .map(|migration| migration.version)
        .collect::<Vec<_>>();
    if applied != expected {
        return Err(StoreError::InvalidMigrationHistory {
            found: applied.to_vec(),
            expected,
        });
    }
    Ok(())
}

fn apply_migration_in_transaction(
    transaction: &Transaction<'_>,
    migration: Migration,
) -> Result<(), StoreError> {
    transaction
        .execute_batch(migration.sql)
        .map_err(|source| StoreError::Migration {
            version: migration.version,
            source,
        })?;
    transaction
        .execute(
            "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, ?2)",
            params![migration.version, unix_timestamp()?],
        )
        .map_err(|source| StoreError::Migration {
            version: migration.version,
            source,
        })?;
    Ok(())
}

fn prune_operations(connection: &Connection, retention: Duration) -> Result<(), StoreError> {
    let retention_seconds = i64::try_from(retention.as_secs()).unwrap_or(i64::MAX);
    let cutoff = unix_timestamp()?.saturating_sub(retention_seconds);
    connection.execute(
        "DELETE FROM operations WHERE created_at < ?1",
        params![cutoff],
    )?;
    Ok(())
}

fn unix_timestamp() -> Result<i64, StoreError> {
    let seconds = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    Ok(i64::try_from(seconds).unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests {
    use std::sync::Barrier;
    use std::thread;

    use super::*;
    use tempfile::tempdir;

    fn test_database_path() -> (tempfile::TempDir, PathBuf) {
        let directory = tempdir().expect("temp directory should be created");
        let path = directory.path().join("compresso.db");
        (directory, path)
    }

    #[test]
    fn test_failed_migration_rolls_back() {
        const BAD_MIGRATION: Migration = Migration {
            version: 2,
            sql: "CREATE TABLE test_table (id INTEGER PRIMARY KEY); THIS IS INVALID SQL;",
        };
        let (_directory, path) = test_database_path();
        drop(Store::open(&path).expect("initial migration should succeed"));

        let error = match Store::open_with_migrations(
            &path,
            StoreOptions::default(),
            &[MIGRATIONS[0], BAD_MIGRATION],
        ) {
            Ok(_) => panic!("bad migration should fail"),
            Err(error) => error,
        };
        assert!(matches!(error, StoreError::Migration { version: 2, .. }));

        let connection = Connection::open(path).expect("database should reopen");
        let version: i64 = connection
            .query_row("SELECT MAX(version) FROM schema_migrations", [], |row| {
                row.get(0)
            })
            .expect("migration version should be readable");
        assert_eq!(version, 1);
        let test_table_exists: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'test_table')",
                [],
                |row| row.get(0),
            )
            .expect("sqlite_master should be readable");
        assert!(!test_table_exists);
    }

    #[test]
    fn test_wal_and_fk_enabled() {
        let (_directory, path) = test_database_path();
        let store = Store::open(&path).expect("store should open");
        let connection = store.connection().expect("connection lock should succeed");

        let journal_mode: String = connection
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .expect("journal mode should be readable");
        let foreign_keys: i64 = connection
            .pragma_query_value(None, "foreign_keys", |row| row.get(0))
            .expect("foreign key setting should be readable");
        let busy_timeout: i64 = connection
            .pragma_query_value(None, "busy_timeout", |row| row.get(0))
            .expect("busy timeout should be readable");

        assert_eq!(journal_mode, "wal");
        assert_eq!(foreign_keys, 1);
        assert_eq!(busy_timeout, 5_000);

        let orphan_error = connection
            .execute(
                "INSERT INTO files (repo_id, path, hash, size_bytes, token_estimate, indexed_at) VALUES (999, 'src/lib.rs', 'aa', 1, 1, 1)",
                [],
            )
            .expect_err("foreign key violations must fail");
        assert!(matches!(orphan_error, rusqlite::Error::SqliteFailure(_, _)));
    }

    #[test]
    fn test_in_memory_store_rejects_non_wal_journal_mode() {
        let error = match Store::open(Path::new(":memory:")) {
            Ok(_) => panic!("in-memory store should reject its effective journal mode"),
            Err(error) => error,
        };

        assert!(matches!(
            error,
            StoreError::JournalModeMismatch { actual } if actual == "memory"
        ));
    }

    #[test]
    fn test_simultaneous_first_open_applies_migration_once() {
        const OPENERS: usize = 8;

        let (_directory, path) = test_database_path();
        let barrier = Arc::new(Barrier::new(OPENERS));
        let mut handles = Vec::new();

        for _ in 0..OPENERS {
            let path = path.clone();
            let barrier = Arc::clone(&barrier);
            handles.push(thread::spawn(move || {
                barrier.wait();
                Store::open(&path)
            }));
        }

        let stores = handles
            .into_iter()
            .map(|handle| {
                handle
                    .join()
                    .expect("store opener should not panic")
                    .expect("simultaneous first open should succeed")
            })
            .collect::<Vec<_>>();

        for store in &stores {
            let connection = store.connection().expect("connection lock should succeed");
            let journal_mode: String = connection
                .pragma_query_value(None, "journal_mode", |row| row.get(0))
                .expect("journal mode should be readable");
            let foreign_keys: i64 = connection
                .pragma_query_value(None, "foreign_keys", |row| row.get(0))
                .expect("foreign key setting should be readable");
            let busy_timeout: i64 = connection
                .pragma_query_value(None, "busy_timeout", |row| row.get(0))
                .expect("busy timeout should be readable");
            assert_eq!(journal_mode, "wal");
            assert_eq!(foreign_keys, 1);
            assert_eq!(busy_timeout, 5_000);
        }

        let connection = Connection::open(path).expect("database should be inspectable");
        let rows: Vec<i64> = connection
            .prepare("SELECT version FROM schema_migrations ORDER BY version")
            .expect("migration query should prepare")
            .query_map([], |row| row.get(0))
            .expect("migration rows should be queryable")
            .collect::<Result<_, _>>()
            .expect("migration rows should decode");
        assert_eq!(rows, [1]);
    }

    #[test]
    fn test_unknown_migration_history_rejected() {
        let (_directory, path) = test_database_path();
        drop(Store::open(&path).expect("initial migration should succeed"));
        let connection = Connection::open(&path).expect("database should be inspectable");
        connection
            .execute(
                "INSERT INTO schema_migrations (version, applied_at) VALUES (99, 1)",
                [],
            )
            .expect("unknown migration row should insert");
        drop(connection);

        let error = match Store::open(&path) {
            Ok(_) => panic!("unknown migration history should be rejected"),
            Err(error) => error,
        };
        assert!(matches!(
            error,
            StoreError::InvalidMigrationHistory { found, expected }
                if found == [1, 99] && expected == [1]
        ));
    }

    #[test]
    fn test_gap_in_migration_history_rejected() {
        const MIGRATION_2: Migration = Migration {
            version: 2,
            sql: "SELECT 1;",
        };
        const MIGRATION_3: Migration = Migration {
            version: 3,
            sql: "SELECT 1;",
        };
        let (_directory, path) = test_database_path();
        drop(Store::open(&path).expect("initial migration should succeed"));
        let connection = Connection::open(&path).expect("database should be inspectable");
        connection
            .execute(
                "INSERT INTO schema_migrations (version, applied_at) VALUES (3, 1)",
                [],
            )
            .expect("gapped migration row should insert");
        drop(connection);

        let error = match Store::open_with_migrations(
            &path,
            StoreOptions::default(),
            &[MIGRATIONS[0], MIGRATION_2, MIGRATION_3],
        ) {
            Ok(_) => panic!("gapped migration history should be rejected"),
            Err(error) => error,
        };
        assert!(matches!(
            error,
            StoreError::InvalidMigrationHistory { found, expected }
                if found == [1, 3] && expected == [1, 2]
        ));
    }

    #[test]
    fn test_concurrent_writes_serialized() {
        let (_directory, path) = test_database_path();
        let store = Store::open(&path).expect("store should open");
        let barrier = Arc::new(Barrier::new(2));
        let mut handles = Vec::new();

        for worker in 0..2 {
            let store = store.clone();
            let barrier = Arc::clone(&barrier);
            handles.push(thread::spawn(move || {
                barrier.wait();
                for operation in 0..50 {
                    let id = format!("worker-{worker}-operation-{operation}");
                    store
                        .connection()
                        .expect("connection lock should succeed")
                        .execute(
                            "INSERT INTO operations (id, tool, mode, observed_tokens, applied_tokens, created_at) VALUES (?1, 'compresso_read', 'safe', 1, 1, ?2)",
                            params![id, unix_timestamp().expect("time should be valid")],
                        )
                        .expect("serialized write should succeed");
                }
            }));
        }

        for handle in handles {
            handle.join().expect("worker should not panic");
        }

        let operation_count: i64 = store
            .connection()
            .expect("connection lock should succeed")
            .query_row("SELECT COUNT(*) FROM operations", [], |row| row.get(0))
            .expect("operation count should be readable");
        assert_eq!(operation_count, 100);
    }

    #[test]
    fn test_store_send_sync_clone() {
        fn assert_bounds<T: Send + Sync + Clone>() {}
        assert_bounds::<Store>();
    }
}
