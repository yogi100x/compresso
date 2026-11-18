use std::collections::BTreeSet;
use std::time::{SystemTime, UNIX_EPOCH};

use compresso_store::Store;
use rusqlite::{Connection, params};
use tempfile::tempdir;

const EXPECTED_TABLES: [&str; 14] = [
    "call_sites",
    "edges",
    "files",
    "index_versions",
    "integration_receipts",
    "memory_facts",
    "operations",
    "repositories",
    "schema_migrations",
    "sessions",
    "strategy_results",
    "symbols",
    "symbols_fts",
    "update_state",
];

fn unix_timestamp() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should follow the Unix epoch")
            .as_secs(),
    )
    .expect("current time should fit in i64")
}

#[test]
fn test_open_fresh_creates_all_tables() {
    let directory = tempdir().expect("temp directory should be created");
    let path = directory.path().join("nested/compresso.db");
    let store = Store::open(&path).expect("fresh store should open");

    let connection = Connection::open(&path).expect("database should be inspectable");
    let mut statement = connection
        .prepare("SELECT name FROM sqlite_master WHERE type IN ('table', 'view')")
        .expect("sqlite_master query should prepare");
    let tables = statement
        .query_map([], |row| row.get::<_, String>(0))
        .expect("table names should be queryable")
        .collect::<Result<BTreeSet<_>, _>>()
        .expect("table names should decode");

    for expected in EXPECTED_TABLES {
        assert!(tables.contains(expected), "missing table {expected}");
    }
    drop(store);
}

#[test]
fn test_open_idempotent() {
    let directory = tempdir().expect("temp directory should be created");
    let path = directory.path().join("compresso.db");
    drop(Store::open(&path).expect("first open should succeed"));

    let first_connection = Connection::open(&path).expect("database should be inspectable");
    let first_rows: Vec<(i64, i64)> = first_connection
        .prepare("SELECT version, applied_at FROM schema_migrations ORDER BY version")
        .expect("migration query should prepare")
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .expect("migration rows should be queryable")
        .collect::<Result<_, _>>()
        .expect("migration rows should decode");
    drop(first_connection);

    drop(Store::open(&path).expect("second open should succeed"));
    let connection = Connection::open(path).expect("database should be inspectable");
    let rows: Vec<(i64, i64)> = connection
        .prepare("SELECT version, applied_at FROM schema_migrations ORDER BY version")
        .expect("migration query should prepare")
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .expect("migration rows should be queryable")
        .collect::<Result<_, _>>()
        .expect("migration rows should decode");
    assert_eq!(rows, first_rows);
}

#[test]
fn test_retention_prunes_old_operations() {
    let directory = tempdir().expect("temp directory should be created");
    let path = directory.path().join("compresso.db");
    drop(Store::open(&path).expect("store should open"));

    let connection = Connection::open(&path).expect("database should be inspectable");
    let now = unix_timestamp();
    for (id, age_days) in [("old", 91), ("recent", 89)] {
        connection
            .execute(
                "INSERT INTO operations (id, tool, mode, observed_tokens, applied_tokens, created_at) VALUES (?1, 'compresso_read', 'safe', 1, 1, ?2)",
                params![id, now - age_days * 24 * 60 * 60],
            )
            .expect("operation should insert");
    }
    drop(connection);

    drop(Store::open(&path).expect("reopen should prune operations"));
    let connection = Connection::open(path).expect("database should be inspectable");
    let ids = connection
        .prepare("SELECT id FROM operations ORDER BY id")
        .expect("operation query should prepare")
        .query_map([], |row| row.get::<_, String>(0))
        .expect("operations should be queryable")
        .collect::<Result<Vec<_>, _>>()
        .expect("operation ids should decode");
    assert_eq!(ids, ["recent"]);
}

#[test]
fn test_foreign_keys_and_fts5_schema_work() {
    let directory = tempdir().expect("temp directory should be created");
    let path = directory.path().join("compresso.db");
    drop(Store::open(&path).expect("store should open"));

    let connection = Connection::open(path).expect("database should be inspectable");
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .expect("foreign keys should enable");
    connection
        .execute(
            "INSERT INTO repositories (id, root, approved_at, parser_version) VALUES (1, '/tmp/repo', 1, 'v1')",
            [],
        )
        .expect("repository should insert");
    connection
        .execute(
            "INSERT INTO files (id, repo_id, path, hash, size_bytes, token_estimate, indexed_at) VALUES (1, 1, 'src/lib.rs', 'aa', 10, 2, 1)",
            [],
        )
        .expect("file should insert");
    connection
        .execute(
            "INSERT INTO symbols (id, file_id, name, qualified_name, kind, start_line, end_line, token_estimate) VALUES (1, 1, 'start', 'SessionManager::start', 'method', 1, 3, 2)",
            [],
        )
        .expect("symbol should insert");

    let fts_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM symbols_fts WHERE symbols_fts MATCH 'essionMan'",
            [],
            |row| row.get(0),
        )
        .expect("trigram FTS query should succeed");
    assert_eq!(fts_count, 1);

    connection
        .execute("DELETE FROM files WHERE id = 1", [])
        .expect("file should delete");
    let symbol_count: i64 = connection
        .query_row("SELECT COUNT(*) FROM symbols", [], |row| row.get(0))
        .expect("symbol count should be readable");
    let fts_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM symbols_fts WHERE symbols_fts MATCH 'essionMan'",
            [],
            |row| row.get(0),
        )
        .expect("trigram FTS query should succeed after cascade");
    assert_eq!(symbol_count, 0);
    assert_eq!(fts_count, 0);
}
