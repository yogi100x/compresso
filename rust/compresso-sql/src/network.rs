//! PostgreSQL and MySQL/MariaDB through `sqlx`.
//!
//! Every call opens one fresh connection and closes it, so nothing a
//! statement leaves in the session (advisory or named locks, `set_config`,
//! user variables, temporary objects) outlives the call. Per call:
//!
//! 1. connect (10 s deadline; TLS per [`crate::target::tls_policy`]);
//! 2. refuse a role or account that can reach server files or programs;
//! 3. open a read-only transaction with server-side statement deadlines;
//! 4. for a query: pass the gate, prepare the statement with the extended
//!    protocol (which refuses more than one statement), then run the same
//!    text with the simple protocol so every column arrives as the server's
//!    own text rendering; stop reading at the row or byte cap;
//! 5. roll back and close, or drop the connection if the result was cut
//!    short or the client deadline passed.
//!
//! The client deadline is the statement deadline plus a grace period; when it
//! passes the connection is dropped (the server's own deadline stops the
//! statement). No cancel request is sent: `sqlx` 0.9 exposes none.

use std::time::{Duration, Instant};

use futures_util::TryStreamExt;
use serde_json::Value;
use sqlx::mysql::{
    MySqlConnectOptions, MySqlConnection, MySqlDatabaseError, MySqlRow, MySqlSslMode,
};
use sqlx::postgres::{PgConnectOptions, PgConnection, PgRow, PgSslMode};
use sqlx::{
    AssertSqlSafe, Column, ConnectOptions, Connection, Executor, Row, SqlSafeStr, Statement,
    TypeInfo, ValueRef, raw_sql,
};
use tokio::runtime::Runtime;

use crate::cells::{self, RowCollector};
use crate::target::{LocalTls, NetworkTarget, Tls};
use crate::{
    CONNECT_TIMEOUT, ColumnEntry, Connector, DescribeResult, ForeignKeyEntry, IndexEntry,
    MAX_TABLES, SqlAction, SqlEngine, SqlError, SqlOutcome, SqlRequest, TableEntry, TablesResult,
    gate,
};

/// Time the client waits past the server-side statement deadline.
const CLIENT_GRACE: Duration = Duration::from_secs(2);
/// Separator for lists aggregated into one text column.
const LIST_SEPARATOR: char = '\u{1f}';

/// The runtime that drives network calls, separate from the MCP server's.
pub(crate) fn runtime() -> Result<Runtime, SqlError> {
    tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
        .map_err(|_| SqlError::new("internal", "could not start the SQL runtime"))
}

/// A network target with the runtime that runs its calls.
pub(crate) struct Bound<'a> {
    pub(crate) network: &'a NetworkTarget,
    pub(crate) runtime: &'a Runtime,
}

impl Connector for Bound<'_> {
    fn run(&self, request: &SqlRequest) -> Result<SqlOutcome, SqlError> {
        let deadline = request.timeout + CLIENT_GRACE;
        self.runtime.block_on(async {
            match self.network.engine {
                SqlEngine::Postgres => {
                    let mut connection = postgres_connect(self.network).await?;
                    match tokio::time::timeout(deadline, postgres_call(&mut connection, request))
                        .await
                    {
                        Ok(Ok((outcome, clean))) => {
                            finish(connection, clean).await;
                            Ok(outcome)
                        }
                        Ok(Err(error)) => {
                            let _ = connection.close_hard().await;
                            Err(error)
                        }
                        Err(_) => {
                            let _ = connection.close_hard().await;
                            Err(SqlError::timeout(request.timeout))
                        }
                    }
                }
                _ => {
                    let mut connection = mysql_connect(self.network).await?;
                    match tokio::time::timeout(deadline, mysql_call(&mut connection, request)).await
                    {
                        Ok(Ok((outcome, clean))) => {
                            finish(connection, clean).await;
                            Ok(outcome)
                        }
                        Ok(Err(error)) => {
                            let _ = connection.close_hard().await;
                            Err(error)
                        }
                        Err(_) => {
                            let _ = connection.close_hard().await;
                            Err(SqlError::timeout(request.timeout))
                        }
                    }
                }
            }
        })
    }
}

/// Rolls back and closes a connection whose last statement was read to the
/// end; drops one that still has unread rows.
async fn finish<C: Connection>(mut connection: C, clean: bool)
where
    for<'c> &'c mut C: Executor<'c>,
{
    if clean && (&mut connection).execute(raw_sql("ROLLBACK")).await.is_ok() {
        let _ = connection.close().await;
    } else {
        let _ = connection.close_hard().await;
    }
}

fn map_error(error: sqlx::Error, engine: SqlEngine) -> SqlError {
    let label = match engine {
        SqlEngine::Postgres => "Postgres",
        _ => "MySQL",
    };
    match error {
        sqlx::Error::Database(database) => {
            let message = database.message().to_owned();
            let state = database.code().map(|code| code.into_owned());
            let number = database
                .try_downcast_ref::<MySqlDatabaseError>()
                .map(MySqlDatabaseError::number);
            match (state.as_deref(), number) {
                // query_canceled (statement_timeout); MySQL max_execution_time;
                // MariaDB max_statement_time.
                (Some("57014"), _) | (_, Some(3024 | 1969 | 1317)) => SqlError::new(
                    "sql_timeout",
                    "the statement did not finish within timeout_ms and was stopped; narrow it or raise timeout_ms.",
                ),
                (Some("25006"), _) | (_, Some(1792)) => SqlError::new(
                    "sql_read_only",
                    format!("{label} refused a write in this read-only transaction: {message}"),
                ),
                (Some("42501"), _) | (_, Some(1142 | 1143 | 1044 | 1227)) => SqlError::new(
                    "sql_permission",
                    format!("{label} denied this to the configured role: {message}"),
                ),
                _ => SqlError::new("sql_error", format!("{label}: {message}")),
            }
        }
        sqlx::Error::Configuration(_) => {
            SqlError::not_configured("the driver rejected the connection settings")
        }
        sqlx::Error::Io(_) | sqlx::Error::Tls(_) | sqlx::Error::Protocol(_) => SqlError::new(
            "sql_connection",
            format!("could not talk to the {label} server: {error}"),
        ),
        other => SqlError::new("sql_error", format!("{label}: {other}")),
    }
}

async fn timed_connect<C, F>(future: F, engine: SqlEngine) -> Result<C, SqlError>
where
    F: Future<Output = Result<C, sqlx::Error>>,
{
    match tokio::time::timeout(CONNECT_TIMEOUT, future).await {
        Ok(Ok(connection)) => Ok(connection),
        Ok(Err(error)) => Err(map_error(error, engine)),
        Err(_) => Err(SqlError::new(
            "sql_connection",
            format!("could not connect within {} s", CONNECT_TIMEOUT.as_secs()),
        )),
    }
}

fn owned_sql(sql: &str) -> sqlx::SqlStr {
    AssertSqlSafe(sql.to_owned()).into_sql_str()
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// A text cell from the server's own text rendering, with integers and
/// booleans as JSON values and binary types as a size placeholder.
fn text_cell(text: Option<&str>, bytes: Option<usize>, type_name: &str, binary: bool) -> Value {
    if binary {
        return cells::binary(bytes.unwrap_or_default());
    }
    let Some(text) = text else {
        return Value::from(format!("<{} value>", type_name.to_ascii_lowercase()));
    };
    let upper = type_name.to_ascii_uppercase();
    let integer = matches!(
        upper.split_whitespace().next().unwrap_or_default(),
        "INT2"
            | "INT4"
            | "INT8"
            | "TINYINT"
            | "SMALLINT"
            | "MEDIUMINT"
            | "INT"
            | "INTEGER"
            | "BIGINT"
    );
    if integer && let Ok(number) = text.parse::<i64>() {
        return Value::from(number);
    }
    if upper == "BOOL" {
        match text {
            "t" => return Value::Bool(true),
            "f" => return Value::Bool(false),
            _ => {}
        }
    }
    cells::text(text)
}

// ---------------------------------------------------------------- Postgres

/// Refuses a superuser or a member of a role that reads or writes server
/// files or runs server programs.
const POSTGRES_ROLE_CHECK: &str = "SELECT (r.rolsuper OR EXISTS (SELECT 1 FROM pg_catalog.pg_roles g WHERE g.rolname IN ('pg_read_server_files', 'pg_write_server_files', 'pg_execute_server_program') AND pg_catalog.pg_has_role(r.oid, g.oid, 'MEMBER')))::text FROM pg_catalog.pg_roles r WHERE r.rolname = current_user";

const POSTGRES_TABLES: &str = "SELECT n.nspname::text, c.relname::text, CASE c.relkind WHEN 'r' THEN 'table' WHEN 'p' THEN 'table' WHEN 'v' THEN 'view' WHEN 'm' THEN 'materialized_view' ELSE 'foreign_table' END, CASE WHEN c.reltuples >= 0 THEN c.reltuples::bigint::text END FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace WHERE c.relkind IN ('r', 'p', 'v', 'm', 'f') AND NOT c.relispartition AND n.nspname <> 'information_schema' AND n.nspname NOT LIKE 'pg\\_%' ORDER BY 1, 2 LIMIT $1";

const POSTGRES_RELATION: &str = "SELECT c.oid::text, n.nspname::text, c.relname::text FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace WHERE c.relkind IN ('r', 'p', 'v', 'm', 'f') AND c.relname = $2 AND (($1::text IS NULL AND pg_catalog.pg_table_is_visible(c.oid)) OR n.nspname = $1::text) LIMIT 1";

const POSTGRES_COLUMNS: &str = "SELECT a.attname::text, pg_catalog.format_type(a.atttypid, a.atttypmod), (NOT a.attnotnull)::text, pg_catalog.pg_get_expr(d.adbin, d.adrelid), (EXISTS (SELECT 1 FROM pg_catalog.pg_index i WHERE i.indrelid = a.attrelid AND i.indisprimary AND a.attnum = ANY (i.indkey)))::text FROM pg_catalog.pg_attribute a LEFT JOIN pg_catalog.pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum WHERE a.attrelid = $1::text::oid AND a.attnum > 0 AND NOT a.attisdropped ORDER BY a.attnum";

const POSTGRES_INDEXES: &str = "SELECT ic.relname::text, i.indisunique::text, pg_catalog.array_to_string(ARRAY(SELECT pg_catalog.pg_get_indexdef(i.indexrelid, k + 1, true) FROM pg_catalog.generate_series(0, i.indnatts - 1) AS k ORDER BY k), chr(31)) FROM pg_catalog.pg_index i JOIN pg_catalog.pg_class ic ON ic.oid = i.indexrelid WHERE i.indrelid = $1::text::oid ORDER BY 1";

const POSTGRES_FOREIGN_KEYS: &str = "SELECT pg_catalog.array_to_string(ARRAY(SELECT a.attname::text FROM pg_catalog.unnest(con.conkey) WITH ORDINALITY AS k(num, ord) JOIN pg_catalog.pg_attribute a ON a.attrelid = con.conrelid AND a.attnum = k.num ORDER BY k.ord), chr(31)), con.confrelid::regclass::text, pg_catalog.array_to_string(ARRAY(SELECT a.attname::text FROM pg_catalog.unnest(con.confkey) WITH ORDINALITY AS k(num, ord) JOIN pg_catalog.pg_attribute a ON a.attrelid = con.confrelid AND a.attnum = k.num ORDER BY k.ord), chr(31)) FROM pg_catalog.pg_constraint con WHERE con.conrelid = $1::text::oid AND con.contype = 'f' ORDER BY con.conname";

async fn postgres_connect(target: &NetworkTarget) -> Result<PgConnection, SqlError> {
    let mode = match target.tls {
        Tls::VerifyFull | Tls::Local(LocalTls::VerifyFull) => PgSslMode::VerifyFull,
        Tls::Local(LocalTls::VerifyCa) => PgSslMode::VerifyCa,
        Tls::Local(LocalTls::Require) => PgSslMode::Require,
        Tls::Local(LocalTls::Prefer) => PgSslMode::Prefer,
        Tls::Local(LocalTls::Disable) => PgSslMode::Disable,
    };
    let options = PgConnectOptions::from_url(&target.url)
        .map_err(|error| map_error(error, SqlEngine::Postgres))?
        .ssl_mode(mode)
        .application_name("compresso")
        .disable_statement_logging();
    timed_connect(PgConnection::connect_with(&options), SqlEngine::Postgres).await
}

/// Runs one call in a read-only transaction. The flag says whether the
/// connection read everything it was sent (and may be rolled back cleanly).
async fn postgres_call(
    connection: &mut PgConnection,
    request: &SqlRequest,
) -> Result<(SqlOutcome, bool), SqlError> {
    let error = |error| map_error(error, SqlEngine::Postgres);
    let privileged: String = sqlx::query_scalar(POSTGRES_ROLE_CHECK)
        .fetch_one(&mut *connection)
        .await
        .map_err(error)?;
    if privileged != "false" {
        return Err(SqlError::new(
            "sql_privileged_role",
            "the configured Postgres role is a superuser or can read or write server files or run server programs; connect with a read-only role that has only SELECT on the tables it needs",
        ));
    }
    let millis = request.timeout.as_millis();
    connection
        .execute(raw_sql("BEGIN READ ONLY"))
        .await
        .map_err(error)?;
    connection
        .execute(raw_sql(AssertSqlSafe(format!(
            "SET LOCAL statement_timeout = {millis}; SET LOCAL lock_timeout = {millis}; SET LOCAL idle_in_transaction_session_timeout = {}",
            millis + CLIENT_GRACE.as_millis() * 2
        ))))
        .await
        .map_err(error)?;
    match &request.action {
        SqlAction::Query(sql) => {
            gate::check_statement(sql, SqlEngine::Postgres).map_err(SqlError::refused)?;
            let statement = (&mut *connection)
                .prepare(owned_sql(sql))
                .await
                .map_err(error)?;
            let columns = statement
                .columns()
                .iter()
                .map(|column| column.name().to_owned())
                .collect::<Vec<_>>();
            let started = Instant::now();
            let mut rows = RowCollector::new(request.max_rows);
            {
                let mut stream = raw_sql(owned_sql(sql)).fetch(&mut *connection);
                while let Some(row) = stream.try_next().await.map_err(error)? {
                    if !rows.push(postgres_row(&row)) {
                        break;
                    }
                }
            }
            let clean = !rows.is_truncated();
            Ok((
                SqlOutcome::Query(rows.finish(columns, elapsed_ms(started))),
                clean,
            ))
        }
        SqlAction::Tables => {
            let limit = i64::try_from(MAX_TABLES + 1).unwrap_or(i64::MAX);
            let rows = sqlx::query(POSTGRES_TABLES)
                .bind(limit)
                .fetch_all(&mut *connection)
                .await
                .map_err(error)?;
            let mut tables = rows
                .iter()
                .map(|row| TableEntry {
                    schema: text_at(row, 0),
                    name: text_at(row, 1).unwrap_or_default(),
                    kind: text_at(row, 2).unwrap_or_default(),
                    rows_estimate: text_at(row, 3).and_then(|value| value.parse().ok()),
                })
                .collect::<Vec<_>>();
            let truncated = tables.len() > MAX_TABLES;
            tables.truncate(MAX_TABLES);
            Ok((SqlOutcome::Tables(TablesResult { tables, truncated }), true))
        }
        SqlAction::Describe(table) => {
            let (schema, name) = split_qualified(table);
            let relation = sqlx::query(POSTGRES_RELATION)
                .bind(schema)
                .bind(name)
                .fetch_optional(&mut *connection)
                .await
                .map_err(error)?
                .ok_or_else(not_found)?;
            let oid = text_at(&relation, 0).unwrap_or_default();
            let qualified = format!(
                "{}.{}",
                text_at(&relation, 1).unwrap_or_default(),
                text_at(&relation, 2).unwrap_or_default()
            );
            let columns = sqlx::query(POSTGRES_COLUMNS)
                .bind(&oid)
                .fetch_all(&mut *connection)
                .await
                .map_err(error)?
                .iter()
                .map(|row| ColumnEntry {
                    name: text_at(row, 0).unwrap_or_default(),
                    data_type: text_at(row, 1).unwrap_or_default(),
                    nullable: text_at(row, 2).as_deref() == Some("true"),
                    default: text_at(row, 3),
                    primary_key: text_at(row, 4).as_deref() == Some("true"),
                })
                .collect();
            let indexes = sqlx::query(POSTGRES_INDEXES)
                .bind(&oid)
                .fetch_all(&mut *connection)
                .await
                .map_err(error)?
                .iter()
                .map(|row| IndexEntry {
                    name: text_at(row, 0).unwrap_or_default(),
                    unique: text_at(row, 1).as_deref() == Some("true"),
                    columns: split_list(text_at(row, 2)),
                })
                .collect();
            let foreign_keys = sqlx::query(POSTGRES_FOREIGN_KEYS)
                .bind(&oid)
                .fetch_all(&mut *connection)
                .await
                .map_err(error)?
                .iter()
                .map(|row| ForeignKeyEntry {
                    columns: split_list(text_at(row, 0)),
                    references_table: text_at(row, 1).unwrap_or_default(),
                    references_columns: split_list(text_at(row, 2)),
                })
                .collect();
            Ok((
                SqlOutcome::Describe(DescribeResult {
                    table: qualified,
                    columns,
                    indexes,
                    foreign_keys,
                }),
                true,
            ))
        }
    }
}

fn postgres_row(row: &PgRow) -> Vec<Value> {
    (0..row.len())
        .map(|index| {
            let Ok(value) = row.try_get_raw(index) else {
                return Value::Null;
            };
            if value.is_null() {
                return Value::Null;
            }
            let type_name = value.type_info().name().to_owned();
            let binary = type_name.eq_ignore_ascii_case("BYTEA");
            // Text format renders bytea as `\x` and two hex digits a byte.
            let bytes = value
                .as_bytes()
                .ok()
                .map(|bytes| bytes.len().saturating_sub(2) / 2);
            text_cell(value.as_str().ok(), bytes, &type_name, binary)
        })
        .collect()
}

// ------------------------------------------------------------------- MySQL

const MYSQL_TABLES: &str = "SELECT CAST(TABLE_SCHEMA AS CHAR), CAST(TABLE_NAME AS CHAR), CAST(TABLE_TYPE AS CHAR), CAST(TABLE_ROWS AS CHAR) FROM information_schema.TABLES WHERE TABLE_SCHEMA NOT IN ('mysql', 'information_schema', 'performance_schema', 'sys') ORDER BY TABLE_SCHEMA, TABLE_NAME LIMIT ?";

const MYSQL_RELATION: &str = "SELECT CAST(TABLE_SCHEMA AS CHAR), CAST(TABLE_NAME AS CHAR) FROM information_schema.TABLES WHERE TABLE_SCHEMA = COALESCE(?, DATABASE()) AND TABLE_NAME = ? LIMIT 1";

const MYSQL_COLUMNS: &str = "SELECT CAST(COLUMN_NAME AS CHAR), CAST(COLUMN_TYPE AS CHAR), CAST(IS_NULLABLE AS CHAR), CAST(COLUMN_DEFAULT AS CHAR), CAST(COLUMN_KEY AS CHAR) FROM information_schema.COLUMNS WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? ORDER BY ORDINAL_POSITION";

const MYSQL_INDEXES: &str = "SELECT CAST(INDEX_NAME AS CHAR), CAST(NON_UNIQUE AS CHAR), CAST(COLUMN_NAME AS CHAR) FROM information_schema.STATISTICS WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? ORDER BY INDEX_NAME, SEQ_IN_INDEX";

const MYSQL_FOREIGN_KEYS: &str = "SELECT CAST(CONSTRAINT_NAME AS CHAR), CAST(COLUMN_NAME AS CHAR), CAST(REFERENCED_TABLE_SCHEMA AS CHAR), CAST(REFERENCED_TABLE_NAME AS CHAR), CAST(REFERENCED_COLUMN_NAME AS CHAR) FROM information_schema.KEY_COLUMN_USAGE WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? AND REFERENCED_TABLE_NAME IS NOT NULL ORDER BY CONSTRAINT_NAME, ORDINAL_POSITION";

async fn mysql_connect(target: &NetworkTarget) -> Result<MySqlConnection, SqlError> {
    let mode = match target.tls {
        Tls::VerifyFull | Tls::Local(LocalTls::VerifyFull) => MySqlSslMode::VerifyIdentity,
        Tls::Local(LocalTls::VerifyCa) => MySqlSslMode::VerifyCa,
        Tls::Local(LocalTls::Require) => MySqlSslMode::Required,
        Tls::Local(LocalTls::Prefer) => MySqlSslMode::Preferred,
        Tls::Local(LocalTls::Disable) => MySqlSslMode::Disabled,
    };
    let options = MySqlConnectOptions::from_url(&target.url)
        .map_err(|error| map_error(error, SqlEngine::MySql))?
        .ssl_mode(mode)
        // Keep the server's own `||` and time zone, so results match what
        // the operator's own client shows.
        .pipes_as_concat(false)
        .timezone(None)
        .disable_statement_logging();
    timed_connect(MySqlConnection::connect_with(&options), SqlEngine::MySql).await
}

/// Whether one `SHOW GRANTS` line gives FILE, SUPER, or every privilege on
/// all databases.
pub(crate) fn grant_is_privileged(grant: &str) -> bool {
    let upper = grant.to_ascii_uppercase();
    let Some(rest) = upper.strip_prefix("GRANT ") else {
        return false;
    };
    let Some((privileges, scope)) = rest.split_once(" ON ") else {
        return false;
    };
    if !scope.trim_start().starts_with("*.*") {
        return false;
    }
    privileges.contains("ALL PRIVILEGES")
        || privileges
            .split(',')
            .map(str::trim)
            .any(|privilege| matches!(privilege, "FILE" | "SUPER"))
}

async fn mysql_call(
    connection: &mut MySqlConnection,
    request: &SqlRequest,
) -> Result<(SqlOutcome, bool), SqlError> {
    let error = |error| map_error(error, SqlEngine::MySql);
    let grants: Vec<String> = sqlx::query_scalar("SHOW GRANTS")
        .fetch_all(&mut *connection)
        .await
        .map_err(error)?;
    if grants.iter().any(|grant| grant_is_privileged(grant)) {
        return Err(SqlError::new(
            "sql_privileged_role",
            "the configured MySQL account holds FILE, SUPER, or ALL PRIVILEGES on *.*; connect with an account that has only SELECT on the schemas it needs",
        ));
    }
    let version: String = sqlx::query_scalar("SELECT CAST(VERSION() AS CHAR)")
        .fetch_one(&mut *connection)
        .await
        .map_err(error)?;
    let millis = request.timeout.as_millis();
    let seconds = request.timeout.as_secs().max(1);
    let deadline = if version.to_ascii_lowercase().contains("mariadb") {
        format!(
            "SET SESSION max_statement_time = {}.{:03}, SESSION lock_wait_timeout = {seconds}",
            millis / 1_000,
            millis % 1_000
        )
    } else {
        format!(
            "SET SESSION max_execution_time = {millis}, SESSION lock_wait_timeout = {seconds}, SESSION innodb_lock_wait_timeout = {seconds}"
        )
    };
    connection
        .execute(raw_sql(AssertSqlSafe(deadline)))
        .await
        .map_err(error)?;
    connection
        .execute(raw_sql("START TRANSACTION READ ONLY"))
        .await
        .map_err(error)?;
    match &request.action {
        SqlAction::Query(sql) => {
            gate::check_statement(sql, SqlEngine::MySql).map_err(SqlError::refused)?;
            let statement = (&mut *connection)
                .prepare(owned_sql(sql))
                .await
                .map_err(error)?;
            let columns = statement
                .columns()
                .iter()
                .map(|column| column.name().to_owned())
                .collect::<Vec<_>>();
            let started = Instant::now();
            let mut rows = RowCollector::new(request.max_rows);
            {
                let mut stream = raw_sql(owned_sql(sql)).fetch(&mut *connection);
                while let Some(row) = stream.try_next().await.map_err(error)? {
                    if !rows.push(mysql_row(&row)) {
                        break;
                    }
                }
            }
            let clean = !rows.is_truncated();
            Ok((
                SqlOutcome::Query(rows.finish(columns, elapsed_ms(started))),
                clean,
            ))
        }
        SqlAction::Tables => {
            let limit = i64::try_from(MAX_TABLES + 1).unwrap_or(i64::MAX);
            let rows = sqlx::query(MYSQL_TABLES)
                .bind(limit)
                .fetch_all(&mut *connection)
                .await
                .map_err(error)?;
            let mut tables = rows
                .iter()
                .map(|row| TableEntry {
                    schema: text_at(row, 0),
                    name: text_at(row, 1).unwrap_or_default(),
                    kind: match text_at(row, 2).as_deref() {
                        Some("BASE TABLE") => "table".to_owned(),
                        Some("VIEW" | "SYSTEM VIEW") => "view".to_owned(),
                        other => other.unwrap_or_default().to_ascii_lowercase(),
                    },
                    rows_estimate: text_at(row, 3).and_then(|value| value.parse().ok()),
                })
                .collect::<Vec<_>>();
            let truncated = tables.len() > MAX_TABLES;
            tables.truncate(MAX_TABLES);
            Ok((SqlOutcome::Tables(TablesResult { tables, truncated }), true))
        }
        SqlAction::Describe(table) => {
            let (schema, name) = split_qualified(table);
            let relation = sqlx::query(MYSQL_RELATION)
                .bind(schema)
                .bind(name)
                .fetch_optional(&mut *connection)
                .await
                .map_err(error)?
                .ok_or_else(not_found)?;
            let schema = text_at(&relation, 0).unwrap_or_default();
            let name = text_at(&relation, 1).unwrap_or_default();
            let columns = sqlx::query(MYSQL_COLUMNS)
                .bind(&schema)
                .bind(&name)
                .fetch_all(&mut *connection)
                .await
                .map_err(error)?
                .iter()
                .map(|row| ColumnEntry {
                    name: text_at(row, 0).unwrap_or_default(),
                    data_type: text_at(row, 1).unwrap_or_default(),
                    nullable: text_at(row, 2).as_deref() == Some("YES"),
                    default: text_at(row, 3),
                    primary_key: text_at(row, 4).as_deref() == Some("PRI"),
                })
                .collect();
            let mut indexes: Vec<IndexEntry> = Vec::new();
            for row in sqlx::query(MYSQL_INDEXES)
                .bind(&schema)
                .bind(&name)
                .fetch_all(&mut *connection)
                .await
                .map_err(error)?
            {
                let index = text_at(&row, 0).unwrap_or_default();
                let column = text_at(&row, 2).unwrap_or_else(|| "<expression>".to_owned());
                match indexes.last_mut() {
                    Some(last) if last.name == index => last.columns.push(column),
                    _ => indexes.push(IndexEntry {
                        name: index,
                        unique: text_at(&row, 1).as_deref() == Some("0"),
                        columns: vec![column],
                    }),
                }
            }
            let mut keys: Vec<(String, ForeignKeyEntry)> = Vec::new();
            for row in sqlx::query(MYSQL_FOREIGN_KEYS)
                .bind(&schema)
                .bind(&name)
                .fetch_all(&mut *connection)
                .await
                .map_err(error)?
            {
                let constraint = text_at(&row, 0).unwrap_or_default();
                let column = text_at(&row, 1).unwrap_or_default();
                let referenced = text_at(&row, 4).unwrap_or_default();
                match keys.last_mut() {
                    Some((last, entry)) if *last == constraint => {
                        entry.columns.push(column);
                        entry.references_columns.push(referenced);
                    }
                    _ => keys.push((
                        constraint,
                        ForeignKeyEntry {
                            columns: vec![column],
                            references_table: format!(
                                "{}.{}",
                                text_at(&row, 2).unwrap_or_default(),
                                text_at(&row, 3).unwrap_or_default()
                            ),
                            references_columns: vec![referenced],
                        },
                    )),
                }
            }
            Ok((
                SqlOutcome::Describe(DescribeResult {
                    table: format!("{schema}.{name}"),
                    columns,
                    indexes,
                    foreign_keys: keys.into_iter().map(|(_, entry)| entry).collect(),
                }),
                true,
            ))
        }
    }
}

fn mysql_row(row: &MySqlRow) -> Vec<Value> {
    (0..row.len())
        .map(|index| {
            let Ok(value) = row.try_get_raw(index) else {
                return Value::Null;
            };
            if value.is_null() {
                return Value::Null;
            }
            let type_name = value.type_info().name().to_owned();
            let upper = type_name.to_ascii_uppercase();
            let binary = upper.contains("BLOB") || upper.contains("BINARY") || upper == "GEOMETRY";
            let bytes = row
                .try_get_unchecked::<&[u8], _>(index)
                .ok()
                .map(<[u8]>::len);
            let text = row.try_get_unchecked::<&str, _>(index).ok();
            text_cell(text, bytes, &type_name, binary)
        })
        .collect()
}

// ------------------------------------------------------------------ shared

/// Column `index` of a metadata row as text (every metadata query casts its
/// columns to text, so no type check is needed).
fn text_at<R>(row: &R, index: usize) -> Option<String>
where
    R: Row,
    usize: sqlx::ColumnIndex<R>,
    for<'r> String: sqlx::Decode<'r, R::Database>,
{
    row.try_get_unchecked::<Option<String>, _>(index)
        .ok()
        .flatten()
}

fn split_list(value: Option<String>) -> Vec<String> {
    value
        .filter(|value| !value.is_empty())
        .map(|value| value.split(LIST_SEPARATOR).map(str::to_owned).collect())
        .unwrap_or_default()
}

/// `schema.table` or `table` (a name with more dots keeps the rest in the
/// table part).
fn split_qualified(table: &str) -> (Option<String>, String) {
    match table.trim().split_once('.') {
        Some((schema, name)) if !schema.is_empty() && !name.is_empty() => {
            (Some(schema.to_owned()), name.to_owned())
        }
        _ => (None, table.trim().to_owned()),
    }
}

fn not_found() -> SqlError {
    SqlError::new(
        "sql_not_found",
        "no table or view of that name is visible to the configured role; call compresso_sql with action tables",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_privileged_mysql_grants_are_recognized() {
        for grant in [
            "GRANT ALL PRIVILEGES ON *.* TO `root`@`localhost` WITH GRANT OPTION",
            "GRANT SELECT, FILE ON *.* TO `u`@`%`",
            "GRANT SUPER ON *.* TO 'u'@'%'",
            "grant select, super on *.* to u",
        ] {
            assert!(grant_is_privileged(grant), "{grant}");
        }
        for grant in [
            "GRANT USAGE ON *.* TO `u`@`%`",
            "GRANT SELECT ON `app`.* TO `u`@`%`",
            "GRANT ALL PRIVILEGES ON `app`.* TO `u`@`%`",
            "GRANT SELECT (`file`) ON `app`.`t` TO `u`@`%`",
            "GRANT `reader`@`%` TO `u`@`%`",
        ] {
            assert!(!grant_is_privileged(grant), "{grant}");
        }
    }

    #[test]
    fn test_text_cells_keep_integers_booleans_and_hide_binary() {
        assert_eq!(text_cell(Some("42"), None, "INT8", false), Value::from(42));
        assert_eq!(
            text_cell(Some("42"), None, "BIGINT UNSIGNED", false),
            Value::from(42)
        );
        assert_eq!(text_cell(Some("t"), None, "BOOL", false), Value::Bool(true));
        assert_eq!(
            text_cell(Some("2026-11-22 10:00:00+00"), None, "TIMESTAMPTZ", false),
            Value::from("2026-11-22 10:00:00+00")
        );
        assert_eq!(
            text_cell(Some("12.50"), None, "NUMERIC", false),
            Value::from("12.50")
        );
        assert_eq!(
            text_cell(None, Some(16), "BYTEA", true),
            Value::from("<binary 16 bytes>")
        );
        assert_eq!(
            text_cell(None, None, "JSONB", false),
            Value::from("<jsonb value>")
        );
    }

    #[test]
    fn test_qualified_names_split_on_the_first_dot() {
        assert_eq!(split_qualified("orders"), (None, "orders".to_owned()));
        assert_eq!(
            split_qualified("sales.orders"),
            (Some("sales".to_owned()), "orders".to_owned())
        );
        assert_eq!(split_qualified(".x"), (None, ".x".to_owned()));
    }
}
