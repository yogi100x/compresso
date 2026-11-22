//! Postgres and MySQL/MariaDB paths.
//!
//! These tests need no database server: they check that connection
//! failures, including a server that echoes the user name back, never carry
//! any part of the configured URL, and that a stalled peer cannot hold a
//! call. The live tests are in `tests/live.rs`.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::Duration;

use compresso_sql::{
    SqlAction, SqlEngine, SqlError, SqlOutcome, SqlRequest, SqlService, SqlSettings,
};

const ENV: &str = "COMPRESSO_SQL_TEST";
const USER: &str = concat!("canary", "_", "role");
const PASSWORD: &str = concat!("canary", "-", "Pw", "-", "5521");
const DATABASE: &str = concat!("canary", "_", "db");

fn service(url: &str) -> SqlService {
    SqlService::new(&SqlSettings::with_value(ENV, Some(url)), Vec::new())
}

fn query(service: &SqlService, sql: &str) -> Result<SqlOutcome, SqlError> {
    service.execute(&SqlRequest::new(SqlAction::Query(sql.to_owned())))
}

fn assert_no_canary(error: &SqlError) {
    let text = format!("{error} {error:?}");
    for canary in [USER, PASSWORD, DATABASE] {
        assert!(!text.contains(canary), "{canary} in {text}");
    }
}

/// A port with nothing listening on it.
fn closed_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("address").port();
    drop(listener);
    port
}

#[test]
fn test_refused_connections_carry_no_url_part() {
    let port = closed_port();
    for url in [
        format!("postgres://{USER}:{PASSWORD}@127.0.0.1:{port}/{DATABASE}?sslmode=disable"),
        format!("mysql://{USER}:{PASSWORD}@127.0.0.1:{port}/{DATABASE}?ssl-mode=DISABLED"),
    ] {
        let service = service(&url);
        assert!(service.engine().is_some_and(SqlEngine::uses_network));
        let error = query(&service, "SELECT 1").expect_err("refused");
        assert_eq!(error.code(), "sql_connection", "{error}");
        assert_no_canary(&error);
        assert!(!format!("{service:?}").contains(PASSWORD));
    }
}

/// A one-shot Postgres server that rejects the login with the message a
/// real server sends, which names the user.
fn echoing_postgres_server() -> (u16, std::thread::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("address").port();
    let handle = std::thread::spawn(move || {
        let Ok((mut stream, _)) = listener.accept() else {
            return String::new();
        };
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("timeout");
        let mut length = [0u8; 4];
        if stream.read_exact(&mut length).is_err() {
            return String::new();
        }
        let mut startup = vec![0u8; u32::from_be_bytes(length) as usize - 4];
        if stream.read_exact(&mut startup).is_err() {
            return String::new();
        }
        // The user the client sent, echoed back as a real server does.
        let user = startup[4..]
            .split(|byte| *byte == 0)
            .map(|field| String::from_utf8_lossy(field).into_owned())
            .collect::<Vec<_>>()
            .windows(2)
            .find(|pair| pair[0] == "user")
            .map(|pair| pair[1].clone())
            .unwrap_or_default();
        let mut fields = Vec::new();
        for (tag, value) in [
            (b'S', "FATAL".to_owned()),
            (b'V', "FATAL".to_owned()),
            (b'C', "28P01".to_owned()),
            (
                b'M',
                format!(
                    "password authentication failed for user \"{user}\" on database \"{DATABASE}\""
                ),
            ),
        ] {
            fields.push(tag);
            fields.extend_from_slice(value.as_bytes());
            fields.push(0);
        }
        fields.push(0);
        let mut message = vec![b'E'];
        message
            .extend_from_slice(&(u32::try_from(fields.len() + 4).expect("length")).to_be_bytes());
        message.extend_from_slice(&fields);
        let _ = stream.write_all(&message);
        let _ = stream.flush();
        user
    });
    (port, handle)
}

#[test]
fn test_a_server_error_that_names_the_user_is_scrubbed() {
    let (port, server) = echoing_postgres_server();
    let service = service(&format!(
        "postgres://{USER}:{PASSWORD}@127.0.0.1:{port}/{DATABASE}?sslmode=disable"
    ));
    let error = query(&service, "SELECT 1").expect_err("rejected login");
    assert_no_canary(&error);
    assert!(
        error.message().contains("password authentication failed"),
        "{error}"
    );
    assert_eq!(server.join().expect("server thread"), USER);
}

#[test]
fn test_a_url_without_user_host_or_database_is_not_configured() {
    // The driver would fill them from PGUSER, the operating-system user,
    // PGHOST, or PGDATABASE; the connection must come only from the URL.
    for url in [
        format!("postgres://127.0.0.1:5432/{DATABASE}?sslmode=disable"),
        format!("postgres://{USER}@127.0.0.1:5432?sslmode=disable"),
        format!("postgres://{USER}@:5432/{DATABASE}"),
    ] {
        let error = query(&service(&url), "SELECT 1").expect_err("unusable");
        assert_eq!(error.code(), "sql_not_configured", "{error}");
        assert_no_canary(&error);
    }
}

#[test]
fn test_remote_hosts_without_verified_tls_are_not_configured() {
    let service = service(&format!(
        "postgres://{USER}:{PASSWORD}@db.example.com/{DATABASE}?sslmode=require"
    ));
    assert_eq!(service.engine(), None);
    let error = query(&service, "SELECT 1").expect_err("weak tls");
    assert_eq!(error.code(), "sql_not_configured");
    assert!(error.message().contains("verify-full"), "{error}");
    assert_no_canary(&error);
}

#[test]
fn test_a_stalled_peer_cannot_hold_a_call_past_its_limit() {
    // Accepts the connection and never answers.
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("address").port();
    let server = std::thread::spawn(move || {
        let accepted = listener.accept();
        std::thread::sleep(Duration::from_secs(3));
        drop(accepted);
    });
    {
        let url =
            format!("postgres://{USER}:{PASSWORD}@127.0.0.1:{port}/{DATABASE}?sslmode=disable");
        let service = service(&url).with_connect_timeout(Duration::from_millis(300));
        let request = SqlRequest {
            action: SqlAction::Query("SELECT 1".to_owned()),
            max_rows: 1,
            timeout: Duration::from_millis(200),
        };
        let started = std::time::Instant::now();
        let error = service.execute(&request).expect_err("stalled");
        let elapsed = started.elapsed();
        assert!(
            matches!(error.code(), "sql_connection" | "sql_timeout"),
            "{error}"
        );
        assert!(elapsed < service.call_limit(request.timeout), "{elapsed:?}");
        assert!(elapsed < Duration::from_secs(2), "{elapsed:?}");
        assert_no_canary(&error);
    }
    server.join().expect("server thread");
}

#[test]
fn test_a_set_pg_variable_refuses_postgres_and_the_ambient_environment_is_not_read() {
    let url = format!("postgres://{USER}:{PASSWORD}@127.0.0.1:5432/{DATABASE}?sslmode=disable");
    // Settings built with an explicit value see only the environment they
    // are given, whatever this process has set (GitHub's Windows image sets
    // PGUSER).
    assert_eq!(service(&url).engine(), Some(SqlEngine::Postgres));
    let settings = SqlSettings::with_value(ENV, Some(&url)).with_postgres_environment(&["PGUSER"]);
    let service = SqlService::new(&settings, Vec::new());
    assert_eq!(service.engine(), None);
    let error = query(&service, "SELECT 1").expect_err("PGUSER set");
    assert_eq!(error.code(), "sql_not_configured");
    assert!(error.message().contains("PGUSER is set"), "{error}");
    assert_no_canary(&error);
    // MySQL reads no PG* variable.
    let mysql = format!("mysql://{USER}@127.0.0.1:3306/{DATABASE}");
    let settings =
        SqlSettings::with_value(ENV, Some(&mysql)).with_postgres_environment(&["PGUSER"]);
    assert_eq!(
        SqlService::new(&settings, Vec::new()).engine(),
        Some(SqlEngine::MySql)
    );
}

#[test]
fn test_a_call_from_inside_an_async_context_does_not_panic() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let port = closed_port();
    let service = service(&format!(
        "postgres://{USER}:{PASSWORD}@127.0.0.1:{port}/{DATABASE}?sslmode=disable"
    ));
    let error = runtime
        .block_on(async { query(&service, "SELECT 1") })
        .expect_err("refused");
    assert_eq!(error.code(), "sql_connection", "{error}");
}

#[test]
fn test_ssl_cert_file_in_the_environment_matters_only_for_verified_tls() {
    // A CI-shaped environment: SSL_CERT_FILE set (as on GitHub's Linux
    // runners). SQLite and a loopback target without TLS still work.
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("plain.db");
    std::fs::write(&path, b"").expect("empty database");
    let sqlite = format!("sqlite:{}", path.display());
    let loopback =
        format!("postgres://{USER}:{PASSWORD}@127.0.0.1:5432/{DATABASE}?sslmode=disable");
    let remote = format!("postgres://{USER}:{PASSWORD}@db.example.com/{DATABASE}");
    let with = |url: &str| {
        SqlService::new(
            &SqlSettings::with_value(ENV, Some(url)).with_environment(&["SSL_CERT_FILE"]),
            Vec::new(),
        )
    };
    assert_eq!(with(&sqlite).engine(), Some(SqlEngine::Sqlite));
    assert!(query(&with(&sqlite), "SELECT 1").is_ok());
    assert_eq!(with(&loopback).engine(), Some(SqlEngine::Postgres));
    let refused = with(&remote);
    let reason = refused
        .configuration_problem()
        .expect("verified TLS refused");
    assert!(reason.starts_with("SSL_CERT_FILE is set"), "{reason}");
    assert!(reason.contains("sslrootcert"), "{reason}");
    // Settings with an explicit value never read the process environment.
    assert_eq!(service(&remote).engine(), Some(SqlEngine::Postgres));
}
