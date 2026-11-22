//! The SQLite memory budget, in a test binary of its own: SQLite counts
//! memory per process, so a concurrent test would share the budget.
#![cfg(unix)]
// SQLite targets are refused off Unix (no file identity in std).

use std::path::Path;
use std::time::{Duration, Instant};

use compresso_sql::{SqlAction, SqlRequest, SqlService, SqlSettings};
use rusqlite::Connection;

fn service(path: &Path) -> SqlService {
    let value = format!("sqlite:{}", path.display());
    SqlService::new(
        &SqlSettings::with_value("COMPRESSO_SQL_TEST", Some(&value)),
        Vec::new(),
    )
}

#[test]
fn test_sqlite_fan_out_over_large_values_is_bounded_in_memory_and_time() {
    // The confirmation review's probe: each round doubles the rows, each row
    // a 1 MB value. Before the fix SQLite spilled 300-450 MB a second into
    // temporary files and overshot a 1 s deadline by 0.86 s.
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("fan.db");
    Connection::open(&path)
        .expect("create")
        .execute_batch("CREATE TABLE t (a);")
        .expect("schema");
    let service = service(&path);
    for timeout_ms in [1_000, 3_000] {
        let request = SqlRequest {
            action: SqlAction::Query(
                "WITH RECURSIVE r(x) AS (SELECT randomblob(1000000) UNION ALL SELECT r.x FROM r, (SELECT 1 UNION ALL SELECT 2)) SELECT count(*) FROM r"
                    .to_owned(),
            ),
            max_rows: 5,
            timeout: Duration::from_millis(timeout_ms),
        };
        let started = Instant::now();
        let error = service.execute(&request).expect_err("fan-out");
        let elapsed = started.elapsed();
        eprintln!(
            "fan-out at {timeout_ms} ms: {} after {elapsed:?}",
            error.code()
        );
        assert!(
            matches!(error.code(), "sql_memory_limit" | "sql_timeout"),
            "{error}"
        );
        assert!(
            elapsed < Duration::from_millis(timeout_ms + 250),
            "{timeout_ms} ms deadline took {elapsed:?}"
        );
    }
}
