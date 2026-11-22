//! `compresso_sql` against scratch SQLite files, through the public service.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use compresso_sql::{
    MAX_CELL_CHARS, MAX_RESULT_BYTES, SqlAction, SqlEngine, SqlOutcome, SqlRequest, SqlService,
    SqlSettings, Truncation,
};
use rusqlite::Connection;
use serde_json::Value;

const ENV: &str = "COMPRESSO_SQL_TEST";

fn create(path: &Path, sql: &str) {
    let connection = Connection::open(path).expect("create database");
    connection.execute_batch(sql).expect("schema");
}

fn service(path: &Path, protected: Vec<PathBuf>) -> SqlService {
    let value = format!("sqlite:{}", path.display());
    SqlService::new(&SqlSettings::with_value(ENV, Some(&value)), protected)
}

fn query(
    service: &SqlService,
    sql: &str,
) -> Result<compresso_sql::QueryResult, compresso_sql::SqlError> {
    match service.execute(&SqlRequest::new(SqlAction::Query(sql.to_owned())))? {
        SqlOutcome::Query(result) => Ok(result),
        other => panic!("unexpected outcome {other:?}"),
    }
}

fn shop(directory: &Path) -> PathBuf {
    let path = directory.join("shop.db");
    create(
        &path,
        "CREATE TABLE customers (id INTEGER PRIMARY KEY, name TEXT NOT NULL, note TEXT DEFAULT 'none');
         CREATE TABLE orders (id INTEGER PRIMARY KEY, customer_id INTEGER REFERENCES customers(id), total REAL, receipt BLOB);
         CREATE UNIQUE INDEX orders_receipt ON orders (receipt);
         CREATE VIEW big_orders AS SELECT * FROM orders WHERE total > 10;
         INSERT INTO customers (name) VALUES ('ada'), ('grace');
         INSERT INTO orders (customer_id, total, receipt) VALUES (1, 12.5, x'00010203'), (2, 3.0, NULL);",
    );
    path
}

#[test]
fn test_sqlite_query_returns_typed_bounded_rows() {
    let directory = tempfile::tempdir().expect("tempdir");
    let service = service(&shop(directory.path()), Vec::new());
    assert_eq!(service.engine(), Some(SqlEngine::Sqlite));
    let result = query(
        &service,
        "SELECT o.id, c.name, o.total, o.receipt, o.customer_id IS NULL AS orphan, NULL AS empty FROM orders o JOIN customers c ON c.id = o.customer_id ORDER BY o.id",
    )
    .expect("query");
    assert_eq!(
        result.columns,
        ["id", "name", "total", "receipt", "orphan", "empty"]
    );
    assert_eq!(
        result.rows[0],
        vec![
            Value::from(1),
            Value::from("ada"),
            Value::from(12.5),
            Value::from("<binary 4 bytes>"),
            Value::from(0),
            Value::Null,
        ]
    );
    assert_eq!(result.row_count, 2);
    assert!(!result.truncated);
}

#[test]
fn test_sqlite_writes_are_refused_and_the_file_is_unchanged() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = shop(directory.path());
    let before = fs::read(&path).expect("bytes");
    let service = service(&path, Vec::new());
    for sql in [
        "INSERT INTO customers (name) VALUES ('eve')",
        "UPDATE customers SET name = 'x'",
        "DELETE FROM orders",
        "DROP TABLE orders",
        "CREATE TABLE x (a)",
        "PRAGMA query_only = 0",
        "PRAGMA writable_schema = ON",
        "ATTACH DATABASE ':memory:' AS other",
        "VACUUM",
        "REPLACE INTO customers (id, name) VALUES (1, 'x')",
        "WITH x AS (SELECT 1) INSERT INTO customers (name) SELECT 'x' FROM x",
    ] {
        let error = query(&service, sql).expect_err(sql);
        assert_eq!(error.code(), "sql_refused", "{sql}: {error}");
    }
    assert_eq!(fs::read(&path).expect("bytes"), before);
    assert_eq!(
        query(&service, "SELECT count(*) FROM customers")
            .unwrap()
            .rows[0][0],
        Value::from(2)
    );
}

#[test]
fn test_sqlite_wal_database_is_read_without_changing_the_main_file() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("wal.db");
    create(
        &path,
        "PRAGMA journal_mode = WAL; CREATE TABLE t (a); INSERT INTO t VALUES (1), (2);",
    );
    // A writer still holds the database open with frames in the log.
    let writer = Connection::open(&path).expect("writer");
    writer
        .execute("INSERT INTO t VALUES (3)", [])
        .expect("insert");
    let before = fs::read(&path).expect("bytes");
    let wal_before = fs::read(directory.path().join("wal.db-wal")).expect("wal");
    let service = service(&path, Vec::new());
    assert_eq!(
        query(&service, "SELECT count(*) FROM t").unwrap().rows[0][0],
        Value::from(3)
    );
    assert!(query(&service, "INSERT INTO t VALUES (4)").is_err());
    assert_eq!(fs::read(&path).expect("bytes"), before);
    assert_eq!(
        fs::read(directory.path().join("wal.db-wal")).expect("wal"),
        wal_before
    );
    drop(writer);
}

#[test]
fn test_sqlite_stacked_statements_are_refused() {
    let directory = tempfile::tempdir().expect("tempdir");
    let service = service(&shop(directory.path()), Vec::new());
    for sql in [
        "SELECT 1; SELECT 2",
        "SELECT 1; DELETE FROM orders",
        "SELECT 1;/* x */;",
    ] {
        assert_eq!(
            query(&service, sql).expect_err(sql).code(),
            "sql_refused",
            "{sql}"
        );
    }
    assert!(query(&service, "SELECT 1; ").is_ok());
    // Comments are outside the strict subset (E17-50 review).
    assert_eq!(
        query(&service, "SELECT 1; -- trailing comment")
            .expect_err("comment")
            .code(),
        "sql_refused"
    );
}

#[test]
fn test_sqlite_runaway_query_stops_at_its_deadline() {
    let directory = tempfile::tempdir().expect("tempdir");
    let service = service(&shop(directory.path()), Vec::new());
    let request = SqlRequest {
        action: SqlAction::Query(
            "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM c) SELECT count(*) FROM c"
                .to_owned(),
        ),
        max_rows: 10,
        timeout: Duration::from_millis(200),
    };
    let started = Instant::now();
    let error = service.execute(&request).expect_err("runaway");
    // Its queue grows in memory, so the memory budget may stop it first.
    assert!(
        matches!(error.code(), "sql_timeout" | "sql_memory_limit"),
        "{error}"
    );
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[test]
fn test_sqlite_rows_cells_and_bytes_are_bounded() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("wide.db");
    create(&path, "CREATE TABLE w (body TEXT);");
    let connection = Connection::open(&path).expect("open");
    let body = "x".repeat(MAX_CELL_CHARS * 3);
    for _ in 0..400 {
        connection
            .execute("INSERT INTO w VALUES (?1)", [&body])
            .expect("row");
    }
    drop(connection);
    let service = service(&path, Vec::new());

    let request = SqlRequest {
        action: SqlAction::Query("SELECT rowid FROM w".to_owned()),
        max_rows: 7,
        timeout: Duration::from_secs(5),
    };
    let SqlOutcome::Query(rows) = service.execute(&request).expect("rows") else {
        panic!("query");
    };
    assert_eq!(rows.row_count, 7);
    assert_eq!(rows.truncation_reason, Some(Truncation::MaxRows));

    let request = SqlRequest {
        action: SqlAction::Query("SELECT body, body, body FROM w".to_owned()),
        max_rows: 1_000,
        timeout: Duration::from_secs(5),
    };
    let SqlOutcome::Query(wide) = service.execute(&request).expect("wide") else {
        panic!("query");
    };
    let Value::String(cell) = &wide.rows[0][0] else {
        panic!("text cell");
    };
    assert!(cell.ends_with(&format!("... [+{} chars]", MAX_CELL_CHARS * 2)));
    assert_eq!(wide.truncation_reason, Some(Truncation::MaxBytes));
    assert!(wide.row_count < 400);
    assert!(serde_json::to_string(&wide.rows).unwrap().len() <= MAX_RESULT_BYTES);

    let exact = query(&service, "SELECT rowid FROM w LIMIT 100").expect("exact");
    assert!(!exact.truncated);
}

#[test]
fn test_sqlite_store_paths_are_refused_directly_and_through_links() {
    let directory = tempfile::tempdir().expect("tempdir");
    let data = directory.path().join("compresso");
    fs::create_dir(&data).expect("data dir");
    let store = data.join("compresso.db");
    create(&store, "CREATE TABLE operations (id TEXT);");
    let protected = vec![store.clone(), data.clone()];

    let error = query(&service(&store, protected.clone()), "SELECT 1").expect_err("store");
    assert_eq!(error.code(), "sql_refused");

    #[cfg(unix)]
    {
        let link = directory.path().join("innocent.db");
        std::os::unix::fs::symlink(&store, &link).expect("symlink");
        let error = query(&service(&link, protected.clone()), "SELECT 1").expect_err("symlink");
        assert_eq!(error.code(), "sql_refused");

        let hard = directory.path().join("hard.db");
        fs::hard_link(&store, &hard).expect("hard link");
        let error = query(&service(&hard, vec![store.clone()]), "SELECT 1").expect_err("hard link");
        assert_eq!(error.code(), "sql_refused");
    }

    let elsewhere = shop(directory.path());
    assert!(query(&service(&elsewhere, protected), "SELECT 1").is_ok());
}

#[test]
fn test_sqlite_hostile_schema_cannot_reach_outside_the_read() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("hostile.db");
    create(
        &path,
        "CREATE TABLE t (a);
         INSERT INTO t VALUES (1);
         CREATE TRIGGER spread AFTER INSERT ON t BEGIN INSERT INTO t VALUES (NEW.a + 1); END;",
    );
    // A view whose body calls a function the gate never sees in the query
    // text. It is written through the schema table, as a crafted file would.
    let connection = Connection::open(&path).expect("open");
    connection
        .execute_batch(
            "PRAGMA writable_schema = ON;
             INSERT INTO sqlite_schema (type, name, tbl_name, rootpage, sql) VALUES ('view', 'lure', 'lure', 0, 'CREATE VIEW lure AS SELECT load_extension(''/tmp/x'') AS a');
             PRAGMA writable_schema = OFF;",
        )
        .expect("crafted view");
    drop(connection);
    let before = fs::read(&path).expect("bytes");
    let service = service(&path, Vec::new());
    let error = query(&service, "SELECT * FROM lure").expect_err("view");
    assert!(
        matches!(error.code(), "sql_refused" | "sql_error"),
        "{error}"
    );
    assert!(query(&service, "INSERT INTO t VALUES (5)").is_err());
    assert_eq!(fs::read(&path).expect("bytes"), before);
    assert_eq!(
        query(&service, "SELECT count(*) FROM t").unwrap().rows[0][0],
        Value::from(1)
    );
}

#[test]
fn test_sqlite_tables_and_describe() {
    let directory = tempfile::tempdir().expect("tempdir");
    let service = service(&shop(directory.path()), Vec::new());
    let SqlOutcome::Tables(tables) = service
        .execute(&SqlRequest::new(SqlAction::Tables))
        .expect("tables")
    else {
        panic!("tables");
    };
    let names = tables
        .tables
        .iter()
        .map(|table| (table.name.as_str(), table.kind.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        [
            ("big_orders", "view"),
            ("customers", "table"),
            ("orders", "table")
        ]
    );

    let SqlOutcome::Describe(orders) = service
        .execute(&SqlRequest::new(SqlAction::Describe("orders".to_owned())))
        .expect("describe")
    else {
        panic!("describe");
    };
    assert_eq!(orders.columns.len(), 4);
    assert!(orders.columns[0].primary_key);
    assert_eq!(orders.indexes[0].name, "orders_receipt");
    assert!(orders.indexes[0].unique);
    assert_eq!(orders.indexes[0].columns, ["receipt"]);
    assert_eq!(orders.foreign_keys[0].references_table, "customers");
    assert_eq!(orders.foreign_keys[0].columns, ["customer_id"]);

    let SqlOutcome::Describe(customers) = service
        .execute(&SqlRequest::new(SqlAction::Describe(
            "customers".to_owned(),
        )))
        .expect("describe")
    else {
        panic!("describe");
    };
    assert!(!customers.columns[1].nullable);
    assert_eq!(customers.columns[2].default.as_deref(), Some("'none'"));

    let missing = service
        .execute(&SqlRequest::new(SqlAction::Describe("nope".to_owned())))
        .expect_err("missing");
    assert_eq!(missing.code(), "sql_not_found");
}

#[test]
fn test_unusable_settings_report_not_configured_without_the_value() {
    let canary = concat!("canary", "-", "secret", "-path");
    let directory = tempfile::tempdir().expect("tempdir");
    let missing = directory.path().join(format!("{canary}.db"));
    for settings in [
        SqlSettings::with_value(ENV, None),
        SqlSettings::with_value(ENV, Some(&format!("sqlite:relative/{canary}.db"))),
        SqlSettings::with_value(ENV, Some(&format!("sqlite:{}", missing.display()))),
        SqlSettings::with_value("DATABASE_URL", Some("sqlite:/tmp/x.db")),
        SqlSettings::with_value(ENV, Some(&format!("ftp://{canary}@host/x"))),
    ] {
        let service = SqlService::new(&settings, Vec::new());
        let error = query(&service, "SELECT 1").expect_err("unusable");
        assert_eq!(error.code(), "sql_not_configured", "{error}");
        assert!(!error.message().contains(canary), "{error}");
        assert!(!format!("{settings:?} {service:?}").contains(canary));
    }
}

#[test]
fn test_sqlite_column_names_and_counts_are_bounded() {
    let directory = tempfile::tempdir().expect("tempdir");
    let service = service(&shop(directory.path()), Vec::new());
    // An unaliased column is named after its expression.
    let long = format!("SELECT '{}'", "a".repeat(99_000));
    let result = query(&service, &long).expect("long name");
    assert!(result.columns[0].chars().count() <= compresso_sql::MAX_NAME_CHARS + 32);
    assert!(serde_json::to_string(&result).unwrap().len() < 4_000);

    let many = format!(
        "SELECT {}",
        (0..2_000)
            .map(|index| format!("{index} AS c{index:045}"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let result = query(&service, &many).expect("many columns");
    assert_eq!(result.columns.len(), compresso_sql::MAX_COLUMNS);
    assert_eq!(result.rows[0].len(), compresso_sql::MAX_COLUMNS);
    assert_eq!(result.columns_omitted, 2_000 - compresso_sql::MAX_COLUMNS);
    assert!(result.truncated);
    assert!(serde_json::to_string(&result).unwrap().len() <= compresso_sql::MAX_RESULT_BYTES);
}

#[test]
fn test_sqlite_huge_values_fail_fast_instead_of_overrunning_the_deadline() {
    let directory = tempfile::tempdir().expect("tempdir");
    let service = service(&shop(directory.path()), Vec::new());
    for sql in [
        "SELECT randomblob(1000000000)",
        "SELECT zeroblob(1000000000)",
        "SELECT printf('%.*c', 900000000, 'x')",
        "WITH RECURSIVE c(s) AS (SELECT 'x' UNION ALL SELECT s || s FROM c WHERE length(s) < 500000000) SELECT length(max(s)) FROM c",
    ] {
        let request = SqlRequest {
            action: SqlAction::Query(sql.to_owned()),
            max_rows: 5,
            timeout: Duration::from_millis(3_000),
        };
        let started = Instant::now();
        let error = service.execute(&request).expect_err(sql);
        assert!(
            matches!(error.code(), "sql_error" | "sql_timeout"),
            "{sql}: {error}"
        );
        assert!(
            started.elapsed() < Duration::from_millis(3_500),
            "{sql}: {:?}",
            started.elapsed()
        );
    }
}

#[cfg(unix)]
#[test]
fn test_sqlite_wal_database_in_a_read_only_directory_says_why() {
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::tempdir().expect("tempdir");
    let folder = directory.path().join("ro");
    fs::create_dir(&folder).expect("dir");
    let path = folder.join("wal.db");
    create(
        &path,
        "PRAGMA journal_mode = WAL; CREATE TABLE t (a); INSERT INTO t VALUES (1);",
    );
    fs::set_permissions(&folder, fs::Permissions::from_mode(0o555)).expect("read-only dir");
    let result = query(&service(&path, Vec::new()), "SELECT count(*) FROM t");
    fs::set_permissions(&folder, fs::Permissions::from_mode(0o755)).expect("restore");
    match result {
        // Some SQLite builds read a clean WAL database without its index.
        Ok(rows) => assert_eq!(rows.rows[0][0], Value::from(1)),
        Err(error) => assert_eq!(error.code(), "sql_wal_unavailable", "{error}"),
    }
}

#[test]
fn test_sqlite_c1_and_bidirectional_controls_are_escaped_not_returned() {
    let directory = tempfile::tempdir().expect("tempdir");
    let service = service(&shop(directory.path()), Vec::new());
    let result = query(
        &service,
        "SELECT 'a' || char(8238) || 'b' || char(133) || 'c' || char(8297) AS v, 1 AS 'x\u{202e}y'",
    )
    .expect("query");
    let text = serde_json::to_string(&result).expect("json");
    for raw in ['\u{202e}', '\u{85}', '\u{2069}'] {
        assert!(!text.contains(raw), "{raw:?} in {text}");
    }
    assert_eq!(result.rows[0][0], Value::from("a\\u202Eb\\u0085c\\u2069"));
    assert_eq!(result.columns[1], "x\\u202Ey");
}

#[cfg(unix)]
#[test]
fn test_sqlite_a_hard_link_to_any_file_in_the_data_directory_is_refused() {
    let directory = tempfile::tempdir().expect("tempdir");
    let data = directory.path().join("compresso");
    fs::create_dir_all(data.join("reports")).expect("data dir");
    let store = data.join("compresso.db");
    create(&store, "CREATE TABLE operations (id TEXT);");
    let other = data.join("reports").join("side.db");
    create(&other, "CREATE TABLE t (a);");
    let link = directory.path().join("innocent.db");
    fs::hard_link(&other, &link).expect("hard link");
    let error = query(&service(&link, vec![store, data]), "SELECT 1").expect_err("hard link");
    assert_eq!(error.code(), "sql_refused", "{error}");
}

#[cfg(unix)]
#[test]
fn test_sqlite_protection_of_the_data_directory_fails_closed() {
    use std::os::unix::fs::PermissionsExt;
    // Any hard-linked target is refused: a database has no reason to be.
    let directory = tempfile::tempdir().expect("tempdir");
    let data = directory.path().join("compresso");
    fs::create_dir_all(&data).expect("data");
    let store = data.join("compresso.db");
    create(&store, "CREATE TABLE operations (id TEXT);");
    let elsewhere = directory.path().join("plain.db");
    create(&elsewhere, "CREATE TABLE t (a);");
    let twin = directory.path().join("twin.db");
    fs::hard_link(&elsewhere, &twin).expect("hard link");
    for target in [&elsewhere, &twin] {
        let error = query(&service(target, vec![data.clone()]), "SELECT 1").expect_err("linked");
        assert_eq!(error.code(), "sql_refused", "{error}");
    }
    fs::remove_file(&twin).expect("unlink");
    assert!(query(&service(&elsewhere, vec![data.clone()]), "SELECT 1").is_ok());

    // A data directory the walk cannot fully account for refuses.
    let crowded = directory.path().join("crowded");
    fs::create_dir_all(&crowded).expect("crowded");
    for index in 0..10_050 {
        fs::write(crowded.join(format!("f{index}")), b"").expect("entry");
    }
    let error = query(&service(&elsewhere, vec![crowded]), "SELECT 1").expect_err("crowded");
    assert_eq!(error.code(), "sql_refused", "{error}");

    let closed = directory.path().join("closed");
    fs::create_dir_all(closed.join("locked")).expect("closed");
    fs::set_permissions(closed.join("locked"), fs::Permissions::from_mode(0o000)).expect("lock");
    let result = query(&service(&elsewhere, vec![closed.clone()]), "SELECT 1");
    fs::set_permissions(closed.join("locked"), fs::Permissions::from_mode(0o755)).expect("unlock");
    assert_eq!(result.expect_err("unreadable").code(), "sql_refused");
}

#[cfg(windows)]
#[test]
fn test_sqlite_targets_are_accepted_on_windows() {
    // Owner decision 2026-11-23 (D115): SQLite works on Windows with the
    // path checks only; a hard link into Compresso's data is not detected.
    let directory = tempfile::tempdir().expect("tempdir");
    let service = service(&shop(directory.path()), Vec::new());
    assert_eq!(service.engine(), Some(SqlEngine::Sqlite));
    assert_eq!(
        query(&service, "SELECT count(*) FROM customers")
            .unwrap()
            .rows[0][0],
        Value::from(2)
    );
}
