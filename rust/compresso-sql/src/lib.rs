//! Read-only live SQL queries for the operator-enabled `compresso_sql` MCP
//! tool (E17-50, D115, ADR-0036).
//!
//! One crate owns every database connection Compresso makes, so the
//! capability check can let `compresso-mcp` and `compresso-cli` reach network
//! packages only through it. The operator names one environment variable
//! (`COMPRESSO_SQL_*`) holding one database URL; nothing here reads `.env`
//! files, discovers databases, or accepts a connection string from a tool
//! call.
//!
//! Engines: SQLite (the workspace's bundled `rusqlite`, opened read-only),
//! PostgreSQL, and MySQL/MariaDB (`sqlx`, rustls with the operating system's
//! certificate store). Every call is read-only twice over: the database is
//! asked for a read-only session, and the statement must pass
//! [`gate::check_statement`] first. The [`Connector`] trait is the seam a
//! later non-SQL store plugs into without reshaping the crate.
//!
//! Secrets: the URL and its parts never leave this crate except inside the
//! driver. Errors carry a fixed code and an engine message with every URL
//! component scrubbed, and no type here prints the URL through `Debug`.

mod cells;
pub mod gate;
mod network;
mod redact;
mod sqlite;
mod target;

use std::fmt;
use std::path::PathBuf;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;

pub use cells::{
    MAX_CELL_CHARS, MAX_COLUMNS, MAX_METADATA_CHARS, MAX_NAME_CHARS, MAX_RESULT_BYTES,
};
pub use target::{
    POSTGRES_ENVIRONMENT, POSTGRES_UNOVERRIDABLE, SqlEngine, TRUST_ANCHOR_ENVIRONMENT,
    is_valid_env_name,
};

/// Prefix every operator-chosen variable name carries.
pub const ENV_PREFIX: &str = "COMPRESSO_SQL_";
/// Rows a query returns unless the call asks for fewer or more.
pub const DEFAULT_MAX_ROWS: usize = 100;
/// The most rows one query may return.
pub const MAX_ROWS: usize = 1_000;
/// Statement deadline unless the call sets one, in milliseconds.
pub const DEFAULT_TIMEOUT_MS: u64 = 5_000;
/// The longest statement deadline a call may set, in milliseconds.
pub const MAX_TIMEOUT_MS: u64 = 30_000;
/// Deadline for opening a connection to a network database.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Tables `tables` lists at most.
pub const MAX_TABLES: usize = 500;

/// A stable, content-free failure: a fixed code plus a message whose engine
/// text has every part of the configured URL scrubbed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SqlError {
    code: &'static str,
    message: String,
}

impl SqlError {
    pub(crate) fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// Stable error code (`sql_not_configured`, `sql_refused`, ...).
    pub const fn code(&self) -> &'static str {
        self.code
    }

    /// Actionable message; never contains the URL or any of its parts.
    pub fn message(&self) -> &str {
        &self.message
    }

    pub(crate) fn not_configured(reason: &str) -> Self {
        Self::new(
            "sql_not_configured",
            format!(
                "compresso_sql has no usable database: {reason}. The operator sets the variable named by `--sql-env` in the environment that starts the server, then restarts it."
            ),
        )
    }

    pub(crate) fn refused(message: impl Into<String>) -> Self {
        Self::new("sql_refused", message)
    }

    pub(crate) fn timeout(timeout: Duration) -> Self {
        Self::new(
            "sql_timeout",
            format!(
                "the statement did not finish within {} ms and was stopped; narrow it (filters, LIMIT) or raise timeout_ms (max {MAX_TIMEOUT_MS}).",
                timeout.as_millis()
            ),
        )
    }
}

impl fmt::Display for SqlError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "[{}] {}", self.code, self.message)
    }
}

impl std::error::Error for SqlError {}

/// What one call asks the database.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SqlAction {
    /// Run exactly one read-only statement.
    Query(String),
    /// List schemas and tables, with row estimates where they are cheap.
    Tables,
    /// Columns, keys, and indexes of one table (`table` or `schema.table`).
    Describe(String),
}

/// One bounded call.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SqlRequest {
    pub action: SqlAction,
    /// Rows returned at most (1..=[`MAX_ROWS`]).
    pub max_rows: usize,
    /// Statement deadline.
    pub timeout: Duration,
}

impl SqlRequest {
    /// A request with the default row cap and deadline.
    pub fn new(action: SqlAction) -> Self {
        Self {
            action,
            max_rows: DEFAULT_MAX_ROWS,
            timeout: Duration::from_millis(DEFAULT_TIMEOUT_MS),
        }
    }
}

/// Why a result holds fewer rows than the statement produced.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Truncation {
    /// `max_rows` was reached.
    MaxRows,
    /// The serialized rows reached [`MAX_RESULT_BYTES`].
    MaxBytes,
    /// The caller's token budget was reached (set by the MCP layer).
    MaxTokens,
    /// The statement returned more than [`MAX_COLUMNS`] columns.
    MaxColumns,
}

/// Rows of one query.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct QueryResult {
    /// At most [`MAX_COLUMNS`] names, each cut to [`MAX_NAME_CHARS`].
    pub columns: Vec<String>,
    /// Columns the statement returned beyond [`MAX_COLUMNS`].
    #[serde(skip_serializing_if = "is_zero")]
    pub columns_omitted: usize,
    pub rows: Vec<Vec<Value>>,
    /// Rows returned (after every cap).
    pub row_count: usize,
    pub truncated: bool,
    pub truncation_reason: Option<Truncation>,
    pub elapsed_ms: u64,
}

fn is_zero(value: &usize) -> bool {
    *value == 0
}

impl QueryResult {
    /// Keeps the first `rows` rows and records why.
    pub fn truncate_to(&mut self, rows: usize, reason: Truncation) {
        if rows < self.rows.len() {
            self.rows.truncate(rows);
            self.row_count = rows;
            self.truncated = true;
            self.truncation_reason = Some(reason);
        }
    }
}

/// One table or view.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct TableEntry {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    pub name: String,
    /// `table`, `view`, `materialized_view`, `foreign_table`, ...
    pub kind: String,
    /// The engine's own estimate, when it keeps one; never a `COUNT(*)`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rows_estimate: Option<i64>,
}

/// The tables a role can see.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct TablesResult {
    pub tables: Vec<TableEntry>,
    pub truncated: bool,
}

/// One column of a described table.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ColumnEntry {
    pub name: String,
    #[serde(rename = "type")]
    pub data_type: String,
    pub nullable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    pub primary_key: bool,
}

/// One index of a described table.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct IndexEntry {
    pub name: String,
    pub unique: bool,
    pub columns: Vec<String>,
}

/// One foreign key of a described table.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ForeignKeyEntry {
    pub columns: Vec<String>,
    pub references_table: String,
    pub references_columns: Vec<String>,
}

/// Columns, keys, and indexes of one table.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DescribeResult {
    pub table: String,
    pub columns: Vec<ColumnEntry>,
    pub indexes: Vec<IndexEntry>,
    pub foreign_keys: Vec<ForeignKeyEntry>,
}

/// The result of one call.
#[derive(Clone, Debug, PartialEq)]
pub enum SqlOutcome {
    Query(QueryResult),
    Tables(TablesResult),
    Describe(DescribeResult),
}

/// The seam every engine implements: a later non-SQL store (E17-51) adds a
/// connector and an engine kind without reshaping the request, result, or
/// error types.
pub(crate) trait Connector {
    /// Runs one call. `gated` is false only for the test hook that checks a
    /// database-level control alone.
    fn run(&self, request: &SqlRequest, gated: bool) -> Result<SqlOutcome, SqlError>;
}

/// The operator's choice for one server process: which variable names the
/// database, and its value as read once at startup.
///
/// `Debug` prints the variable's name and whether it was set, never the
/// value.
#[derive(Clone, Eq, PartialEq)]
pub struct SqlSettings {
    env_name: String,
    value: Option<String>,
    /// The connection-affecting variables ([`POSTGRES_ENVIRONMENT`],
    /// [`POSTGRES_UNOVERRIDABLE`], [`TRUST_ANCHOR_ENVIRONMENT`]) set where the
    /// value was read: their names only, never their values.
    postgres_environment: Vec<String>,
}

impl SqlSettings {
    /// Reads `env_name`, and which Postgres variables are set, from this
    /// process's environment once.
    pub fn from_env(env_name: &str) -> Self {
        Self {
            env_name: env_name.to_owned(),
            value: std::env::var(env_name).ok(),
            postgres_environment: POSTGRES_ENVIRONMENT
                .iter()
                .chain(POSTGRES_UNOVERRIDABLE)
                .chain(TRUST_ANCHOR_ENVIRONMENT)
                .filter(|name| match std::env::var_os(name) {
                    // The certificate loader drops empty SSL_CERT_DIR
                    // entries, so a directory list with none is no change.
                    Some(value) if **name == "SSL_CERT_DIR" => {
                        std::env::split_paths(&value).any(|path| !path.as_os_str().is_empty())
                    }
                    Some(_) => true,
                    None => false,
                })
                .map(|name| (*name).to_owned())
                .collect(),
        }
    }

    /// Settings with an explicit value and no Postgres variable set, for
    /// embedders and tests; the process environment is not read.
    pub fn with_value(env_name: &str, value: Option<&str>) -> Self {
        Self {
            env_name: env_name.to_owned(),
            value: value.map(str::to_owned),
            postgres_environment: Vec::new(),
        }
    }

    /// The same settings as if the named variables (Postgres or
    /// `SSL_CERT_*`) were set where the value was read. Settings never read
    /// the process environment after they are made, so tests are hermetic.
    pub fn with_environment(self, names: &[&str]) -> Self {
        self.with_postgres_environment(names)
    }

    /// The same settings as if the named Postgres variables were set.
    pub fn with_postgres_environment(mut self, names: &[&str]) -> Self {
        self.postgres_environment = names.iter().map(|name| (*name).to_owned()).collect();
        self
    }

    /// The operator-chosen variable name (not secret, but kept out of tool
    /// output; see D115).
    pub fn env_name(&self) -> &str {
        &self.env_name
    }
}

impl fmt::Debug for SqlSettings {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SqlSettings")
            .field("env_name", &self.env_name)
            .field("value", &self.value.as_ref().map(|_| "[redacted]"))
            .field("postgres_environment", &self.postgres_environment)
            .finish()
    }
}

/// The live connection target for one server process, fixed at startup.
pub struct SqlService {
    target: Result<target::SqlTarget, String>,
    runtime: Mutex<Option<tokio::runtime::Runtime>>,
    connect_timeout: Duration,
    cut_checks: network::CutChecks,
}

impl SqlService {
    /// Parses `settings` once. A missing or invalid value is kept as a
    /// reason (never containing the value), and every call then fails with
    /// `sql_not_configured`. `protected` lists files and directories a
    /// SQLite target may not be (Compresso's own store).
    pub fn new(settings: &SqlSettings, protected: Vec<PathBuf>) -> Self {
        let target = if !is_valid_env_name(&settings.env_name) {
            Err(format!(
                "the variable name must match {ENV_PREFIX}[A-Z0-9_]+"
            ))
        } else {
            match settings.value.as_deref() {
                None => Err("the variable is not set".to_owned()),
                Some(value) => target::SqlTarget::parse_with_environments(
                    value,
                    protected,
                    |name| {
                        settings
                            .postgres_environment
                            .iter()
                            .any(|set| set == name)
                            .then(String::new)
                    },
                    |name| {
                        settings
                            .postgres_environment
                            .iter()
                            .any(|set| set == name)
                            .then(String::new)
                    },
                ),
            }
        };
        Self {
            target,
            runtime: Mutex::new(None),
            connect_timeout: CONNECT_TIMEOUT,
            cut_checks: network::CutChecks::default(),
        }
    }

    /// The same service with a shorter connect deadline (tests of a stalled
    /// peer). Never longer than [`CONNECT_TIMEOUT`].
    #[doc(hidden)]
    pub fn with_connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = timeout.min(CONNECT_TIMEOUT);
        self
    }

    /// The same service with one of the two MySQL/MariaDB time-limit cut
    /// rules turned off (`warnings`: interruption warnings; `deadline`: a
    /// run that reached its deadline), so a live test proves the other
    /// alone. Built only with the `test-hooks` feature.
    #[cfg(feature = "test-hooks")]
    #[doc(hidden)]
    pub fn with_cut_checks_for_tests(mut self, warnings: bool, deadline: bool) -> Self {
        self.cut_checks = network::CutChecks { warnings, deadline };
        self
    }

    /// The longest one call with `timeout` can take end to end, for a
    /// network engine: connect, session setup, the statement with its client
    /// grace, and closing. A SQLite call is bounded by `timeout` itself.
    pub fn call_limit(&self, timeout: Duration) -> Duration {
        network::call_limit(self.connect_timeout, timeout)
    }

    /// The configured engine, if the variable held a usable URL.
    pub fn engine(&self) -> Option<SqlEngine> {
        self.target.as_ref().ok().map(target::SqlTarget::engine)
    }

    /// Why the target is unusable, if it is. Never contains the value.
    pub fn configuration_problem(&self) -> Option<&str> {
        self.target.as_ref().err().map(String::as_str)
    }

    /// Runs one call to completion on the calling thread. Calls are
    /// serialized: one statement at a time per server process.
    pub fn execute(&self, request: &SqlRequest) -> Result<SqlOutcome, SqlError> {
        self.execute_gated(request, true)
    }

    /// Runs one call without the statement gate, so a test can prove that a
    /// database-level control holds by itself. Built only with the
    /// `test-hooks` feature, which nothing in the shipped binary enables.
    #[cfg(feature = "test-hooks")]
    #[doc(hidden)]
    pub fn execute_ungated_for_tests(&self, request: &SqlRequest) -> Result<SqlOutcome, SqlError> {
        self.execute_gated(request, false)
    }

    fn execute_gated(&self, request: &SqlRequest, gated: bool) -> Result<SqlOutcome, SqlError> {
        let target = self
            .target
            .as_ref()
            .map_err(|reason| SqlError::not_configured(reason))?;
        validate_request(request)?;
        let mut runtime = self.runtime.lock().unwrap_or_else(PoisonError::into_inner);
        let outcome = match target {
            target::SqlTarget::Sqlite(sqlite) => sqlite.run(request, gated),
            target::SqlTarget::Network(network, _) => {
                if runtime.is_none() {
                    *runtime = Some(network::runtime()?);
                }
                let Some(runtime) = runtime.as_ref() else {
                    return Err(SqlError::new("internal", "SQL runtime unavailable"));
                };
                network::Bound {
                    network,
                    runtime,
                    connect_timeout: self.connect_timeout,
                    cut_checks: self.cut_checks,
                }
                .run(request, gated)
            }
        };
        outcome
            .map(bound_metadata)
            .map_err(|error| target.scrub(error))
    }
}

impl Drop for SqlService {
    /// The service may be dropped on an async task (the MCP server closing),
    /// where dropping a runtime would block; its runtime is shut down without
    /// waiting instead.
    fn drop(&mut self) {
        let runtime = self
            .runtime
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(runtime) = runtime {
            runtime.shutdown_background();
        }
    }
}

impl fmt::Debug for SqlService {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SqlService")
            .field("engine", &self.engine())
            .field("configured", &self.target.is_ok())
            .finish_non_exhaustive()
    }
}

fn validate_request(request: &SqlRequest) -> Result<(), SqlError> {
    if !(1..=MAX_ROWS).contains(&request.max_rows) {
        return Err(SqlError::new(
            "invalid_arguments",
            format!("max_rows must be between 1 and {MAX_ROWS}"),
        ));
    }
    let millis = request.timeout.as_millis();
    if millis == 0 || millis > u128::from(MAX_TIMEOUT_MS) {
        return Err(SqlError::new(
            "invalid_arguments",
            format!("timeout_ms must be between 1 and {MAX_TIMEOUT_MS}"),
        ));
    }
    if let SqlAction::Describe(table) = &request.action
        && (table.trim().is_empty() || table.len() > 256)
    {
        return Err(SqlError::new(
            "invalid_arguments",
            "table must name one table in 1 to 256 characters",
        ));
    }
    Ok(())
}

/// Cuts every name in a `tables` or `describe` result to [`MAX_NAME_CHARS`]
/// and every other metadata value to [`MAX_METADATA_CHARS`] (SQLite names
/// have no length limit).
fn bound_metadata(outcome: SqlOutcome) -> SqlOutcome {
    use cells::cut;
    let name = |value: &str| cut(value, MAX_NAME_CHARS);
    let names = |values: &[String]| values.iter().map(|value| name(value)).collect::<Vec<_>>();
    match outcome {
        SqlOutcome::Tables(mut tables) => {
            for table in &mut tables.tables {
                table.schema = table.schema.as_deref().map(name);
                table.name = name(&table.name);
                table.kind = name(&table.kind);
            }
            SqlOutcome::Tables(tables)
        }
        SqlOutcome::Describe(mut describe) => {
            describe.table = cut(&describe.table, MAX_METADATA_CHARS);
            for column in &mut describe.columns {
                column.name = name(&column.name);
                column.data_type = cut(&column.data_type, MAX_METADATA_CHARS);
                column.default = column
                    .default
                    .as_deref()
                    .map(|value| cut(value, MAX_METADATA_CHARS));
            }
            for index in &mut describe.indexes {
                index.name = name(&index.name);
                index.columns = names(&index.columns);
            }
            for key in &mut describe.foreign_keys {
                key.columns = names(&key.columns);
                key.references_table = cut(&key.references_table, MAX_METADATA_CHARS);
                key.references_columns = names(&key.references_columns);
            }
            SqlOutcome::Describe(describe)
        }
        query => query,
    }
}
