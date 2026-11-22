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
use sqlx::mysql::{MySqlConnection, MySqlDatabaseError, MySqlRow};
use sqlx::postgres::{PgConnection, PgRow};
use sqlx::{
    AssertSqlSafe, Column, ConnectOptions, Connection, Executor, Row, SqlSafeStr, Statement,
    TypeInfo, ValueRef, raw_sql,
};
use tokio::runtime::Runtime;

use crate::cells::{self, RowCollector};
use crate::target::{DriverOptions, NetworkTarget};
use crate::{
    ColumnEntry, Connector, DescribeResult, ForeignKeyEntry, IndexEntry, MAX_TABLES, SqlAction,
    SqlEngine, SqlError, SqlOutcome, SqlRequest, TableEntry, TablesResult, gate,
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
    pub(crate) connect_timeout: Duration,
}

/// Longest wait for `ROLLBACK` and a clean close before the connection is
/// dropped instead; a stalled peer is never awaited.
const FINISH_TIMEOUT: Duration = Duration::from_secs(2);

/// The longest one call can take, end to end: connect, setup, statement
/// (with its client grace), and finish. Past it the whole call is dropped.
pub(crate) fn call_limit(connect_timeout: Duration, timeout: Duration) -> Duration {
    connect_timeout + timeout + CLIENT_GRACE + FINISH_TIMEOUT * 2
}

impl Connector for Bound<'_> {
    fn run(&self, request: &SqlRequest, gated: bool) -> Result<SqlOutcome, SqlError> {
        let limit = call_limit(self.connect_timeout, request.timeout);
        self.runtime.block_on(async {
            tokio::time::timeout(limit, self.call(request, gated))
                .await
                .unwrap_or_else(|_| Err(SqlError::timeout(request.timeout)))
        })
    }
}

impl Bound<'_> {
    async fn call(&self, request: &SqlRequest, gated: bool) -> Result<SqlOutcome, SqlError> {
        let deadline = request.timeout + CLIENT_GRACE;
        match &self.network.options {
            DriverOptions::Postgres(options) => {
                let options = options.clone().disable_statement_logging();
                let mut connection = timed_connect(
                    PgConnection::connect_with(&options),
                    SqlEngine::Postgres,
                    self.connect_timeout,
                )
                .await?;
                let result =
                    tokio::time::timeout(deadline, postgres_call(&mut connection, request, gated))
                        .await;
                settle(connection, result, request.timeout).await
            }
            DriverOptions::MySql(options) => {
                let options = options.clone().disable_statement_logging();
                let mut connection = timed_connect(
                    MySqlConnection::connect_with(&options),
                    SqlEngine::MySql,
                    self.connect_timeout,
                )
                .await?;
                let result =
                    tokio::time::timeout(deadline, mysql_call(&mut connection, request, gated))
                        .await;
                settle(connection, result, request.timeout).await
            }
        }
    }
}

/// Ends a call: roll back and close a connection whose last statement was
/// read to the end; drop any other. Never waits longer than
/// [`FINISH_TIMEOUT`] for the peer.
async fn settle<C: Connection>(
    mut connection: C,
    result: Result<Result<(SqlOutcome, bool), SqlError>, tokio::time::error::Elapsed>,
    timeout: Duration,
) -> Result<SqlOutcome, SqlError>
where
    for<'c> &'c mut C: Executor<'c>,
{
    match result {
        Ok(Ok((outcome, true))) => {
            let clean = tokio::time::timeout(FINISH_TIMEOUT, async {
                (&mut connection).execute(raw_sql("ROLLBACK")).await.is_ok()
            })
            .await
            .unwrap_or(false);
            if clean {
                let _ = tokio::time::timeout(FINISH_TIMEOUT, connection.close()).await;
            } else {
                drop_connection(connection).await;
            }
            Ok(outcome)
        }
        Ok(Ok((outcome, false))) => {
            drop_connection(connection).await;
            Ok(outcome)
        }
        Ok(Err(error)) => {
            drop_connection(connection).await;
            Err(error)
        }
        Err(_) => {
            drop_connection(connection).await;
            Err(SqlError::timeout(timeout))
        }
    }
}

async fn drop_connection<C: Connection>(connection: C) {
    let _ = tokio::time::timeout(FINISH_TIMEOUT, connection.close_hard()).await;
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

async fn timed_connect<C, F>(
    future: F,
    engine: SqlEngine,
    connect_timeout: Duration,
) -> Result<C, SqlError>
where
    F: Future<Output = Result<C, sqlx::Error>>,
{
    match tokio::time::timeout(connect_timeout, future).await {
        Ok(Ok(connection)) => Ok(connection),
        Ok(Err(error)) => Err(map_error(error, engine)),
        Err(_) => Err(SqlError::new(
            "sql_connection",
            format!(
                "could not connect within {} ms",
                connect_timeout.as_millis()
            ),
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
const POSTGRES_ROLE_CHECK: &str = "SELECT (r.rolsuper OR EXISTS (SELECT 1 FROM pg_catalog.pg_roles g WHERE g.rolname IN ('pg_read_server_files', 'pg_write_server_files', 'pg_execute_server_program', 'pg_signal_backend') AND pg_catalog.pg_has_role(r.oid, g.oid, 'MEMBER')))::text FROM pg_catalog.pg_roles r WHERE r.rolname = current_user";

const POSTGRES_TABLES: &str = "SELECT n.nspname::text, c.relname::text, CASE c.relkind WHEN 'r' THEN 'table' WHEN 'p' THEN 'table' WHEN 'v' THEN 'view' WHEN 'm' THEN 'materialized_view' ELSE 'foreign_table' END, CASE WHEN c.reltuples >= 0 THEN c.reltuples::bigint::text END FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace WHERE c.relkind IN ('r', 'p', 'v', 'm', 'f') AND NOT c.relispartition AND n.nspname <> 'information_schema' AND n.nspname NOT LIKE 'pg\\_%' ORDER BY 1, 2 LIMIT $1";

const POSTGRES_RELATION: &str = "SELECT c.oid::text, n.nspname::text, c.relname::text FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace WHERE c.relkind IN ('r', 'p', 'v', 'm', 'f') AND c.relname = $2 AND (($1::text IS NULL AND pg_catalog.pg_table_is_visible(c.oid)) OR n.nspname = $1::text) LIMIT 1";

const POSTGRES_COLUMNS: &str = "SELECT a.attname::text, pg_catalog.format_type(a.atttypid, a.atttypmod), (NOT a.attnotnull)::text, pg_catalog.pg_get_expr(d.adbin, d.adrelid), (EXISTS (SELECT 1 FROM pg_catalog.pg_index i WHERE i.indrelid = a.attrelid AND i.indisprimary AND a.attnum = ANY (i.indkey)))::text FROM pg_catalog.pg_attribute a LEFT JOIN pg_catalog.pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum WHERE a.attrelid = $1::text::oid AND a.attnum > 0 AND NOT a.attisdropped ORDER BY a.attnum";

const POSTGRES_INDEXES: &str = "SELECT ic.relname::text, i.indisunique::text, pg_catalog.array_to_string(ARRAY(SELECT pg_catalog.pg_get_indexdef(i.indexrelid, k + 1, true) FROM pg_catalog.generate_series(0, i.indnatts - 1) AS k ORDER BY k), chr(31)) FROM pg_catalog.pg_index i JOIN pg_catalog.pg_class ic ON ic.oid = i.indexrelid WHERE i.indrelid = $1::text::oid ORDER BY 1";

const POSTGRES_FOREIGN_KEYS: &str = "SELECT pg_catalog.array_to_string(ARRAY(SELECT a.attname::text FROM pg_catalog.unnest(con.conkey) WITH ORDINALITY AS k(num, ord) JOIN pg_catalog.pg_attribute a ON a.attrelid = con.conrelid AND a.attnum = k.num ORDER BY k.ord), chr(31)), con.confrelid::regclass::text, pg_catalog.array_to_string(ARRAY(SELECT a.attname::text FROM pg_catalog.unnest(con.confkey) WITH ORDINALITY AS k(num, ord) JOIN pg_catalog.pg_attribute a ON a.attrelid = con.confrelid AND a.attnum = k.num ORDER BY k.ord), chr(31)) FROM pg_catalog.pg_constraint con WHERE con.conrelid = $1::text::oid AND con.contype = 'f' ORDER BY con.conname";

/// Runs one call in a read-only transaction. The flag says whether the
/// connection read everything it was sent (and may be rolled back cleanly).
async fn postgres_call(
    connection: &mut PgConnection,
    request: &SqlRequest,
    gated: bool,
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
    // standard_conforming_strings is forced on so the server reads `\` in
    // a plain string literally, as the gate's lexer does (a role or database
    // may turn it off).
    connection
        .execute(raw_sql("BEGIN READ ONLY"))
        .await
        .map_err(error)?;
    connection
        .execute(raw_sql(AssertSqlSafe(format!(
            "SET LOCAL statement_timeout = {millis}; SET LOCAL lock_timeout = {millis}; SET LOCAL idle_in_transaction_session_timeout = {}; SET LOCAL standard_conforming_strings = on",
            millis + CLIENT_GRACE.as_millis() * 2
        ))))
        .await
        .map_err(error)?;
    match &request.action {
        SqlAction::Query(sql) => {
            if gated {
                gate::check_statement(sql, SqlEngine::Postgres).map_err(SqlError::refused)?;
            }
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

/// Global privileges that reach outside a read-only session: server files,
/// other sessions, or every privilege (`_` and space spellings both, for
/// MySQL and MariaDB).
const PRIVILEGED_GRANTS: &[&str] = &[
    "ALL",
    "ALL PRIVILEGES",
    "FILE",
    "SUPER",
    "PROCESS",
    "CONNECTION ADMIN",
];

/// One line of `SHOW GRANTS` output, classified.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum GrantLine {
    /// A privilege the account must not hold, by name.
    Privileged(String),
    /// Roles granted by this line (`GRANT role[, role] TO account`).
    Roles(Vec<String>),
    /// Harmless: ordinary privileges, `SET DEFAULT ROLE`.
    Ordinary,
    /// Not understood; the account is refused.
    Unknown,
}

/// Classifies one `SHOW GRANTS` line (MySQL or MariaDB syntax).
pub(crate) fn classify_grant(line: &str) -> GrantLine {
    let line = line.trim();
    let upper = line.to_ascii_uppercase();
    if upper.starts_with("SET DEFAULT ROLE") {
        return GrantLine::Ordinary;
    }
    let Some(rest) = line.get(6..).filter(|_| upper.starts_with("GRANT ")) else {
        return GrantLine::Unknown;
    };
    let upper_rest = &upper[6..];
    if upper_rest.starts_with("PROXY ON ") {
        return GrantLine::Privileged("PROXY".to_owned());
    }
    if let Some(on) = upper_rest.find(" ON ") {
        let privileges = &upper_rest[..on];
        let global = upper_rest[on + 4..].trim_start().starts_with("*.*");
        for privilege in privileges.split(',') {
            let name = privilege.trim().trim_matches('`').replace('_', " ");
            let name = name.split('(').next().unwrap_or_default().trim().to_owned();
            let everything = name.starts_with("ALL");
            if PRIVILEGED_GRANTS.contains(&name.as_str()) && (global || !everything) {
                return GrantLine::Privileged(name);
            }
        }
        return GrantLine::Ordinary;
    }
    let Some(to) = upper_rest.find(" TO ") else {
        return GrantLine::Unknown;
    };
    let mut roles = Vec::new();
    for role in rest[..to].split(',').map(str::trim) {
        let plain = role.chars().all(|character| {
            character.is_ascii_alphanumeric()
                || matches!(character, '`' | '\'' | '@' | '%' | '_' | '-' | '.')
        });
        if role.is_empty() || !plain {
            return GrantLine::Unknown;
        }
        roles.push(role.to_owned());
    }
    GrantLine::Roles(roles)
}

/// Checks a set of grant lines: the first privileged or unknown line fails
/// the account; otherwise returns the roles they grant.
pub(crate) fn check_grants(lines: &[String]) -> Result<Vec<String>, SqlError> {
    let mut roles = Vec::new();
    for line in lines {
        match classify_grant(line) {
            GrantLine::Privileged(name) => {
                return Err(SqlError::new(
                    "sql_privileged_role",
                    format!(
                        "the configured MySQL account (directly or through a role) holds {name}; connect with an account that has only SELECT on the schemas it needs"
                    ),
                ));
            }
            GrantLine::Unknown => return Err(undetermined_grants()),
            GrantLine::Roles(granted) => roles.extend(granted),
            GrantLine::Ordinary => {}
        }
    }
    Ok(roles)
}

fn undetermined_grants() -> SqlError {
    SqlError::new(
        "sql_privileged_role",
        "could not fully determine the account's privileges (including those of its roles and roles granted to them); connect with an account without roles, or whose roles hold only SELECT",
    )
}

/// The most role lookups one privilege check makes.
const MAX_ROLE_LOOKUPS: usize = 32;

/// Refuses an account that holds FILE, SUPER, PROCESS, CONNECTION_ADMIN,
/// ALL on `*.*`, or PROXY, directly, through a role, or through a role
/// granted to a role; refuses when that cannot be fully determined.
async fn mysql_privilege_check(
    connection: &mut MySqlConnection,
    mariadb: bool,
) -> Result<(), SqlError> {
    let undetermined = |_| undetermined_grants();
    let grants: Vec<String> = sqlx::query_scalar("SHOW GRANTS")
        .fetch_all(&mut *connection)
        .await
        .map_err(undetermined)?;
    let direct = check_grants(&grants)?;
    if direct.is_empty() {
        return Ok(());
    }
    if mariadb {
        // MariaDB lists a granted role's own grants, including the roles
        // granted to it, so the role graph is walked.
        let mut queue = direct;
        let mut seen: Vec<String> = Vec::new();
        while let Some(role) = queue.pop() {
            if seen.contains(&role) {
                continue;
            }
            if seen.len() == MAX_ROLE_LOOKUPS {
                return Err(undetermined_grants());
            }
            let lines: Vec<String> =
                sqlx::query_scalar(AssertSqlSafe(format!("SHOW GRANTS FOR {role}")))
                    .fetch_all(&mut *connection)
                    .await
                    .map_err(undetermined)?;
            seen.push(role);
            queue.extend(check_grants(&lines)?);
        }
        return Ok(());
    }
    // MySQL shows a role's privileges only for roles granted to the account
    // itself (`USING`); a role granted to a role cannot be listed without
    // access to the `mysql` schema, so its presence refuses the account.
    let applicable: Vec<String> = sqlx::query_scalar(
        "SELECT CONCAT('`', ROLE_NAME, '`@`', ROLE_HOST, '`') FROM information_schema.APPLICABLE_ROLES",
    )
    .fetch_all(&mut *connection)
    .await
    .map_err(undetermined)?;
    let normalize = |role: &str| role.replace('\'', "`").to_ascii_lowercase();
    let direct_normalized = direct
        .iter()
        .map(|role| normalize(role))
        .collect::<Vec<_>>();
    if applicable
        .iter()
        .any(|role| !direct_normalized.contains(&normalize(role)))
    {
        return Err(undetermined_grants());
    }
    let lines: Vec<String> = sqlx::query_scalar(AssertSqlSafe(format!(
        "SHOW GRANTS FOR CURRENT_USER() USING {}",
        direct.join(", ")
    )))
    .fetch_all(&mut *connection)
    .await
    .map_err(undetermined)?;
    check_grants(&lines).map(|_| ())
}

/// `sql_mode` entries that change how quotes and backslashes are read, so the
/// gate's lexer would disagree with the server (`ANSI` implies
/// `ANSI_QUOTES`).
const UNLEXED_SQL_MODES: &[&str] = &["ANSI_QUOTES", "NO_BACKSLASH_ESCAPES", "ANSI"];

/// The session's `sql_mode` without the modes the gate does not lex. Fails
/// on anything that is not a plain mode list, so the result is safe to embed.
pub(crate) fn lexable_sql_mode(current: &str) -> Result<String, SqlError> {
    let mut kept = Vec::new();
    for mode in current
        .split(',')
        .map(str::trim)
        .filter(|mode| !mode.is_empty())
    {
        if !mode
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
        {
            return Err(SqlError::refused(
                "the server reported an sql_mode this tool cannot read safely",
            ));
        }
        if !UNLEXED_SQL_MODES.contains(&mode) {
            kept.push(mode);
        }
    }
    Ok(kept.join(","))
}

/// Whether a reported `sql_mode` holds none of the modes the gate does not
/// lex.
pub(crate) fn mode_is_lexable(mode: &str) -> bool {
    mode.split(',')
        .map(str::trim)
        .all(|mode| !UNLEXED_SQL_MODES.contains(&mode))
}

async fn mysql_call(
    connection: &mut MySqlConnection,
    request: &SqlRequest,
    gated: bool,
) -> Result<(SqlOutcome, bool), SqlError> {
    let error = |error| map_error(error, SqlEngine::MySql);
    let version: String = sqlx::query_scalar("SELECT CAST(VERSION() AS CHAR)")
        .fetch_one(&mut *connection)
        .await
        .map_err(error)?;
    let mariadb = version.to_ascii_lowercase().contains("mariadb");
    mysql_privilege_check(connection, mariadb).await?;
    // utf8mb4 is pinned in the options; a multibyte charset such as gbk
    // could otherwise swallow a quote's escape. Confirm the server agrees.
    let charsets = sqlx::query(
        "SELECT CAST(@@character_set_client AS CHAR), CAST(@@character_set_connection AS CHAR), CAST(@@character_set_results AS CHAR)",
    )
    .fetch_one(&mut *connection)
    .await
    .map_err(error)?;
    if (0..3).any(|index| text_at(&charsets, index).as_deref() != Some("utf8mb4")) {
        return Err(SqlError::refused(
            "the server did not accept the utf8mb4 character set for this session; the statement gate reads only utf8mb4",
        ));
    }
    // The gate lexes quotes and backslashes the default way; take out the
    // modes that change that, then confirm the server agrees.
    let mode: String = sqlx::query_scalar("SELECT CAST(@@SESSION.sql_mode AS CHAR)")
        .fetch_one(&mut *connection)
        .await
        .map_err(error)?;
    if !mode_is_lexable(&mode) {
        let lexable = lexable_sql_mode(&mode)?;
        connection
            .execute(raw_sql(AssertSqlSafe(format!(
                "SET SESSION sql_mode = '{lexable}'"
            ))))
            .await
            .map_err(error)?;
        let confirmed: String = sqlx::query_scalar("SELECT CAST(@@SESSION.sql_mode AS CHAR)")
            .fetch_one(&mut *connection)
            .await
            .map_err(error)?;
        if !mode_is_lexable(&confirmed) {
            return Err(SqlError::refused(
                "the server keeps ANSI_QUOTES or NO_BACKSLASH_ESCAPES on for this session; the statement gate cannot read statements safely under it",
            ));
        }
    }
    let millis = request.timeout.as_millis();
    let seconds = request.timeout.as_secs().max(1);
    let deadline = if mariadb {
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
            if gated {
                gate::check_statement(sql, SqlEngine::MySql).map_err(SqlError::refused)?;
            }
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
    fn test_privileged_mysql_and_mariadb_grants_are_recognized() {
        for (grant, name) in [
            (
                "GRANT ALL PRIVILEGES ON *.* TO `root`@`localhost` WITH GRANT OPTION",
                "ALL PRIVILEGES",
            ),
            ("GRANT SELECT, FILE ON *.* TO `u`@`%`", "FILE"),
            ("GRANT SUPER ON *.* TO 'u'@'%'", "SUPER"),
            ("grant select, super on *.* to u", "SUPER"),
            ("GRANT PROCESS ON *.* TO `u`@`%`", "PROCESS"),
            (
                "GRANT CONNECTION_ADMIN ON *.* TO `u`@`%`",
                "CONNECTION ADMIN",
            ),
            (
                "GRANT CONNECTION ADMIN ON *.* TO `u`@`%`",
                "CONNECTION ADMIN",
            ),
            (
                "GRANT `CONNECTION_ADMIN`,SELECT ON *.* TO `u`@`%`",
                "CONNECTION ADMIN",
            ),
            (
                "GRANT PROXY ON ''@'%' TO 'root'@'localhost' WITH GRANT OPTION",
                "PROXY",
            ),
            ("GRANT PROXY ON `app`@`%` TO `u`@`%`", "PROXY"),
        ] {
            assert_eq!(
                classify_grant(grant),
                GrantLine::Privileged(name.to_owned()),
                "{grant}"
            );
        }
        for grant in [
            "GRANT USAGE ON *.* TO `u`@`%`",
            "GRANT SELECT ON `app`.* TO `u`@`%`",
            "GRANT ALL PRIVILEGES ON `app`.* TO `u`@`%`",
            "GRANT SELECT (`file`) ON `app`.`t` TO `u`@`%`",
            "GRANT USAGE ON *.* TO `u`@`%` IDENTIFIED BY PASSWORD '*AB'",
            "SET DEFAULT ROLE `r` FOR `u`@`%`",
        ] {
            assert_eq!(classify_grant(grant), GrantLine::Ordinary, "{grant}");
        }
        assert_eq!(classify_grant("REVOKE x"), GrantLine::Unknown);
        assert_eq!(
            classify_grant("GRANT `r`;DROP TABLE x TO `u`@`%`"),
            GrantLine::Unknown
        );
    }

    #[test]
    fn test_roles_from_either_engine_are_collected_and_privileges_through_them_refused() {
        assert_eq!(
            check_grants(&[
                "GRANT USAGE ON *.* TO `u`@`%`".to_owned(),
                "GRANT `reader`@`%`,`ops`@`localhost` TO `u`@`%`".to_owned(),
                "GRANT `analyst` TO `u`@`%`".to_owned(),
            ]),
            Ok(vec![
                "`reader`@`%`".to_owned(),
                "`ops`@`localhost`".to_owned(),
                "`analyst`".to_owned(),
            ])
        );
        // A role's own grants (MySQL USING output, MariaDB SHOW GRANTS FOR
        // role) holding a privileged grant refuse the account.
        let error = check_grants(&[
            "GRANT SELECT ON `app`.* TO `reader`@`%`".to_owned(),
            "GRANT PROCESS ON *.* TO `reader`@`%`".to_owned(),
        ])
        .expect_err("role privilege");
        assert_eq!(error.code(), "sql_privileged_role");
        assert!(error.message().contains("PROCESS"));
        assert_eq!(
            check_grants(&["something unexpected".to_owned()])
                .expect_err("unknown")
                .code(),
            "sql_privileged_role"
        );
    }

    #[test]
    fn test_sql_mode_drops_the_modes_the_gate_does_not_lex() {
        assert_eq!(
            lexable_sql_mode("STRICT_TRANS_TABLES,ANSI_QUOTES,NO_BACKSLASH_ESCAPES"),
            Ok("STRICT_TRANS_TABLES".to_owned())
        );
        // ANSI re-enables ANSI_QUOTES, so it goes too.
        assert_eq!(
            lexable_sql_mode("REAL_AS_FLOAT,PIPES_AS_CONCAT,ANSI_QUOTES,IGNORE_SPACE,ANSI"),
            Ok("REAL_AS_FLOAT,PIPES_AS_CONCAT,IGNORE_SPACE".to_owned())
        );
        assert_eq!(lexable_sql_mode(""), Ok(String::new()));
        assert!(lexable_sql_mode("STRICT'; DROP TABLE t; --").is_err());
        assert!(mode_is_lexable(
            "STRICT_TRANS_TABLES,NO_ENGINE_SUBSTITUTION"
        ));
        assert!(!mode_is_lexable("ANSI_QUOTES"));
        assert!(!mode_is_lexable("ONLY_FULL_GROUP_BY,NO_BACKSLASH_ESCAPES"));
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
