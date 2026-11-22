//! Live Postgres and MySQL/MariaDB checks (E17-50). Each test returns at
//! once unless its variables are set, so the default run needs no server:
//!
//! - `COMPRESSO_SQL_TEST_POSTGRES_URL`: a role with no special privileges;
//! - `COMPRESSO_SQL_TEST_POSTGRES_PRIVILEGED_URL`: a superuser on the same
//!   database, used to set up fixtures and expected to be refused;
//! - `COMPRESSO_SQL_TEST_MYSQL_URL` and `COMPRESSO_SQL_TEST_MYSQL_PRIVILEGED_URL`
//!   likewise (an account without FILE/SUPER/PROCESS, and root);
//! - optionally `COMPRESSO_SQL_TEST_POSTGRES_WRONG_HOST_URL`: a URL whose
//!   host name does not match the server's certificate.
//!
//! The CI job `live-sql` runs them against throwaway Postgres and MariaDB
//! containers. Database-level controls are checked with the statement gate
//! skipped (`test-hooks`), so each test proves that control alone.

use std::time::{Duration, Instant};

use compresso_sql::{
    SqlAction, SqlEngine, SqlError, SqlOutcome, SqlRequest, SqlService, SqlSettings,
};
use serde_json::Value;
use sqlx::mysql::MySqlConnection;
use sqlx::postgres::PgConnection;
use sqlx::{AssertSqlSafe, Connection, Executor, raw_sql};

const ENV: &str = "COMPRESSO_SQL_TEST";

fn variable(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

fn service(url: &str) -> SqlService {
    SqlService::new(&SqlSettings::with_value(ENV, Some(url)), Vec::new())
}

fn request(sql: &str, timeout_ms: u64) -> SqlRequest {
    SqlRequest {
        action: SqlAction::Query(sql.to_owned()),
        max_rows: 50,
        timeout: Duration::from_millis(timeout_ms),
    }
}

fn gated(service: &SqlService, sql: &str) -> Result<Vec<Vec<Value>>, SqlError> {
    match service.execute(&request(sql, 5_000))? {
        SqlOutcome::Query(result) => Ok(result.rows),
        other => panic!("unexpected outcome {other:?}"),
    }
}

fn ungated(service: &SqlService, sql: &str) -> Result<Vec<Vec<Value>>, SqlError> {
    match service.execute_ungated_for_tests(&request(sql, 5_000))? {
        SqlOutcome::Query(result) => Ok(result.rows),
        other => panic!("unexpected outcome {other:?}"),
    }
}

fn user_of(url: &str) -> String {
    url::Url::parse(url).expect("url").username().to_owned()
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
}

async fn pg_admin(admin: &str, statements: &[String]) {
    let mut connection = PgConnection::connect(admin).await.expect("admin connects");
    for statement in statements {
        connection
            .execute(raw_sql(AssertSqlSafe(statement.clone())))
            .await
            .unwrap_or_else(|error| panic!("{statement}: {error}"));
    }
    connection.close().await.expect("close");
}

fn postgres() -> Option<(SqlService, String, String)> {
    let reader = variable("COMPRESSO_SQL_TEST_POSTGRES_URL")?;
    let admin = variable("COMPRESSO_SQL_TEST_POSTGRES_PRIVILEGED_URL")
        .expect("COMPRESSO_SQL_TEST_POSTGRES_PRIVILEGED_URL sets up the fixture");
    let role = user_of(&reader);
    runtime().block_on(pg_admin(
        &admin,
        &[
            format!(
                "DO $$ BEGIN CREATE ROLE \"{role}\" LOGIN; EXCEPTION WHEN duplicate_object THEN NULL; END $$"
            ),
            "CREATE SCHEMA IF NOT EXISTS compresso_probe".to_owned(),
            "CREATE TABLE IF NOT EXISTS compresso_probe.items (id int PRIMARY KEY, name text)"
                .to_owned(),
            "INSERT INTO compresso_probe.items VALUES (1, 'a'), (2, 'b') ON CONFLICT DO NOTHING"
                .to_owned(),
            "CREATE SEQUENCE IF NOT EXISTS compresso_probe.seq".to_owned(),
            format!("GRANT USAGE, CREATE ON SCHEMA compresso_probe TO \"{role}\""),
            // Write privileges too, so a refusal can only come from the
            // read-only transaction.
            format!("GRANT SELECT, INSERT, UPDATE ON compresso_probe.items TO \"{role}\""),
            format!("GRANT USAGE, SELECT, UPDATE ON SEQUENCE compresso_probe.seq TO \"{role}\""),
            format!(
                "GRANT TEMPORARY ON DATABASE \"{}\" TO \"{role}\"",
                url::Url::parse(&reader)
                    .expect("url")
                    .path()
                    .trim_start_matches('/')
            ),
        ],
    ));
    Some((service(&reader), reader, admin))
}

#[test]
fn test_live_postgres_reads_and_each_write_gets_the_read_only_code() {
    let Some((service, _, _)) = postgres() else {
        return;
    };
    assert_eq!(service.engine(), Some(SqlEngine::Postgres));
    assert_eq!(
        gated(
            &service,
            "SELECT id, name FROM compresso_probe.items ORDER BY id"
        )
        .expect("read")[0],
        vec![Value::from(1), Value::from("a")]
    );
    for sql in [
        "INSERT INTO compresso_probe.items VALUES (3, 'c')",
        "UPDATE compresso_probe.items SET name = 'z'",
        "SELECT nextval('compresso_probe.seq')",
        "SELECT setval('compresso_probe.seq', 10)",
        "CREATE TEMP TABLE compresso_probe_temp (a int)",
        "SELECT * FROM compresso_probe.items FOR UPDATE",
        "SELECT * FROM compresso_probe.items FOR SHARE",
        "SELECT 1 INTO compresso_probe.copy",
    ] {
        let error = ungated(&service, sql).expect_err(sql);
        assert_eq!(error.code(), "sql_read_only", "{sql}: {error}");
        // The gate refuses each of them first.
        assert!(gated(&service, sql).is_err(), "{sql}");
    }
    assert!(matches!(
        service
            .execute(&SqlRequest::new(SqlAction::Tables))
            .expect("tables"),
        SqlOutcome::Tables(_)
    ));
    assert!(matches!(
        service
            .execute(&SqlRequest::new(SqlAction::Describe(
                "compresso_probe.items".to_owned()
            )))
            .expect("describe"),
        SqlOutcome::Describe(_)
    ));
}

#[test]
fn test_live_postgres_timeout_leaves_no_session_behind() {
    let Some((service, _, admin)) = postgres() else {
        return;
    };
    let started = Instant::now();
    let error = service
        .execute(&request("SELECT pg_sleep(5)", 500))
        .expect_err("timeout");
    assert_eq!(error.code(), "sql_timeout", "{error}");
    assert!(started.elapsed() < service.call_limit(Duration::from_millis(500)));
    runtime().block_on(async {
        let mut connection = PgConnection::connect(&admin).await.expect("admin");
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let left: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM pg_stat_activity WHERE application_name = 'compresso' AND state <> 'idle'",
            )
            .fetch_one(&mut connection)
            .await
            .expect("activity");
            if left == 0 {
                break;
            }
            assert!(Instant::now() < deadline, "a compresso session is still running");
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    });
}

#[test]
fn test_live_postgres_known_gap_a_session_of_the_same_role_can_be_terminated() {
    let Some((service, reader, _)) = postgres() else {
        return;
    };
    let victim_url = format!(
        "{reader}{}application_name=compresso_victim",
        if reader.contains('?') { "&" } else { "?" }
    );
    // The victim connection lives on its own runtime; the calls below are
    // made from plain test code, as the MCP server makes them.
    let victim_runtime = runtime();
    let victim = victim_runtime
        .block_on(PgConnection::connect(&victim_url))
        .expect("victim");
    let sql = "SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE application_name = 'compresso_victim'";
    // The gate refuses the call by name...
    assert_eq!(
        gated(&service, sql).expect_err("gate").code(),
        "sql_refused"
    );
    // ...but the read-only session does not stop it (ADR-0036, "What a model
    // can still do"): a wrapper function the gate cannot see would reach it.
    let rows = ungated(&service, sql).expect("the server allows it");
    eprintln!("known gap, server answered: {rows:?}");
    assert_eq!(rows, vec![vec![Value::from(true)]]);
    let alive = victim_runtime.block_on(async {
        let mut victim = victim;
        sqlx::query_scalar::<_, i32>("SELECT 1")
            .fetch_one(&mut victim)
            .await
            .is_ok()
    });
    assert!(!alive, "the terminated session still answered");
}

#[test]
fn test_live_postgres_privileged_role_and_role_settings() {
    let Some((service, reader, admin)) = postgres() else {
        return;
    };
    assert_eq!(
        gated(&self::service(&admin), "SELECT 1")
            .expect_err("superuser")
            .code(),
        "sql_privileged_role"
    );
    let role = user_of(&reader);
    runtime().block_on(pg_admin(
        &admin,
        &[format!(
            "ALTER ROLE \"{role}\" SET standard_conforming_strings = off"
        )],
    ));
    let setting = gated(
        &service,
        "SELECT current_setting('standard_conforming_strings')",
    );
    runtime().block_on(pg_admin(
        &admin,
        &[format!(
            "ALTER ROLE \"{role}\" RESET standard_conforming_strings"
        )],
    ));
    assert_eq!(setting.expect("setting"), vec![vec![Value::from("on")]]);
    runtime().block_on(pg_admin(
        &admin,
        &[format!("GRANT pg_signal_backend TO \"{role}\"")],
    ));
    let refused = gated(&service, "SELECT 1");
    runtime().block_on(pg_admin(
        &admin,
        &[format!("REVOKE pg_signal_backend FROM \"{role}\"")],
    ));
    assert_eq!(
        refused.expect_err("signal role").code(),
        "sql_privileged_role"
    );
}

#[test]
fn test_live_postgres_tls_to_the_wrong_host_name_is_refused() {
    let Some(url) = variable("COMPRESSO_SQL_TEST_POSTGRES_WRONG_HOST_URL") else {
        return;
    };
    let error = gated(&service(&url), "SELECT 1").expect_err("wrong host");
    assert_eq!(error.code(), "sql_connection", "{error}");
}

async fn mysql_admin(admin: &str, statements: &[String]) {
    let mut connection = MySqlConnection::connect(admin)
        .await
        .expect("admin connects");
    for statement in statements {
        connection
            .execute(raw_sql(AssertSqlSafe(statement.clone())))
            .await
            .unwrap_or_else(|error| panic!("{statement}: {error}"));
    }
    connection.close().await.expect("close");
}

fn mysql() -> Option<(SqlService, String, String)> {
    let reader = variable("COMPRESSO_SQL_TEST_MYSQL_URL")?;
    let admin = variable("COMPRESSO_SQL_TEST_MYSQL_PRIVILEGED_URL")
        .expect("COMPRESSO_SQL_TEST_MYSQL_PRIVILEGED_URL sets up the fixture");
    let role = user_of(&reader);
    runtime().block_on(mysql_admin(
        &admin,
        &[
            format!("CREATE USER IF NOT EXISTS '{role}'@'%'"),
            "CREATE DATABASE IF NOT EXISTS compresso_probe".to_owned(),
            "CREATE TABLE IF NOT EXISTS compresso_probe.items (id int PRIMARY KEY, name varchar(20))"
                .to_owned(),
            "INSERT IGNORE INTO compresso_probe.items VALUES (1, 'a'), (2, 'b')".to_owned(),
            format!("GRANT SELECT, INSERT, UPDATE ON compresso_probe.* TO '{role}'@'%'"),
        ],
    ));
    Some((service(&reader), reader, admin))
}

#[test]
fn test_live_mysql_reads_and_writes_get_the_read_only_code() {
    let Some((service, _, _)) = mysql() else {
        return;
    };
    assert_eq!(service.engine(), Some(SqlEngine::MySql));
    assert_eq!(
        gated(
            &service,
            "SELECT id, name FROM compresso_probe.items ORDER BY id"
        )
        .expect("read")[0],
        vec![Value::from(1), Value::from("a")]
    );
    for sql in [
        "INSERT INTO compresso_probe.items VALUES (3, 'c')",
        "UPDATE compresso_probe.items SET name = 'z'",
    ] {
        let error = ungated(&service, sql).expect_err(sql);
        assert_eq!(error.code(), "sql_read_only", "{sql}: {error}");
        assert!(gated(&service, sql).is_err(), "{sql}");
    }
    assert!(matches!(
        service
            .execute(&SqlRequest::new(SqlAction::Tables))
            .expect("tables"),
        SqlOutcome::Tables(_)
    ));
}

#[test]
fn test_live_mysql_a_statement_cut_by_the_time_limit_is_a_timeout() {
    let Some((service, _, _)) = mysql() else {
        return;
    };
    // MariaDB stops BENCHMARK at max_statement_time and returns 0 with no
    // error (seen in CI); a table scan is stopped with an error. Both must
    // answer sql_timeout, never a result.
    for sql in [
        "SELECT BENCHMARK(10000000000, MD5('a'))",
        "SELECT SLEEP(5)",
        "SELECT COUNT(*) FROM information_schema.COLUMNS a, information_schema.COLUMNS b, information_schema.COLUMNS c",
    ] {
        let started = Instant::now();
        let result = service.execute(&request(sql, 500));
        let error = result.expect_err(sql);
        assert_eq!(error.code(), "sql_timeout", "{sql}: {error}");
        assert!(started.elapsed() < service.call_limit(Duration::from_millis(500)));
    }
}

#[test]
fn test_live_mysql_privileged_account_and_server_modes() {
    let Some((service, _, admin)) = mysql() else {
        return;
    };
    assert_eq!(
        gated(&self::service(&admin), "SELECT 1")
            .expect_err("root")
            .code(),
        "sql_privileged_role"
    );
    let saved = runtime().block_on(async {
        let mut connection = MySqlConnection::connect(&admin).await.expect("admin");
        let mode: String = sqlx::query_scalar("SELECT CAST(@@GLOBAL.sql_mode AS CHAR)")
            .fetch_one(&mut connection)
            .await
            .expect("mode");
        mode
    });
    runtime().block_on(mysql_admin(
        &admin,
        &["SET GLOBAL sql_mode = 'ANSI_QUOTES,NO_BACKSLASH_ESCAPES'".to_owned()],
    ));
    let mode = gated(&service, "SELECT @@SESSION.sql_mode");
    runtime().block_on(mysql_admin(
        &admin,
        &[format!("SET GLOBAL sql_mode = '{saved}'")],
    ));
    let mode = mode.expect("mode")[0][0]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    assert!(!mode.contains("ANSI_QUOTES"), "{mode}");
    assert!(!mode.contains("NO_BACKSLASH_ESCAPES"), "{mode}");
}
