//! Read-only MongoDB queries for the operator-enabled `compresso_mongo` MCP
//! tool (E17-52, D117, ADR-0038).
//!
//! MongoDB has no read-only session. The two controls are the argument
//! allow-list ([`gate`]), checked before any network call, and the account
//! check ([`account`]), run first on every connection; neither is defence in
//! depth for the other. The tool never forwards a command document: it
//! builds `find`, `aggregate`, `count`, `distinct`, `listCollections`, and
//! `listIndexes` itself from typed arguments, always with a server-side time
//! limit and a row limit, and the call has a client-side deadline too.
//!
//! The operator names one environment variable (`COMPRESSO_MONGO_*`) holding
//! one `mongodb://` URL; nothing here reads `.env` files, discovers servers,
//! or accepts a URL from a tool call. Results reuse the SQL tool's limits
//! and its escaping of hidden characters.

pub mod account;
mod client;
pub mod gate;
mod output;
mod target;

use std::fmt;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;

pub use target::{
    MONGO_ENV_PREFIX, MONGO_HANDSHAKE_ENVIRONMENT, URL_OPTIONS, is_valid_mongo_env_name,
};

use crate::{
    CONNECT_TIMEOUT, DEFAULT_MAX_ROWS, DEFAULT_TIMEOUT_MS, MAX_ROWS, MAX_TABLES, MAX_TIMEOUT_MS,
    SqlError, Truncation,
};

/// Documents `describe` samples at most.
pub const DESCRIBE_SAMPLE: usize = 100;
/// Field paths `describe` reports at most.
pub const MAX_DESCRIBED_FIELDS: usize = 200;
/// Largest `skip` a `find` accepts.
pub const MAX_SKIP: u64 = 1_000_000;

/// What one call asks the server. Every argument is checked by [`gate`]
/// before the call connects.
#[derive(Clone, Debug, PartialEq)]
pub enum MongoAction {
    /// `find` with a filter, optional projection and sort, and a skip.
    Find {
        collection: String,
        filter: Value,
        projection: Option<Value>,
        sort: Option<Value>,
        skip: u64,
    },
    /// `aggregate` with an allow-listed pipeline.
    Aggregate { collection: String, pipeline: Value },
    /// The number of documents matching a filter.
    Count { collection: String, filter: Value },
    /// Distinct values of one field among documents matching a filter.
    Distinct {
        collection: String,
        field: String,
        filter: Value,
    },
    /// The collections and views the account may list.
    Collections,
    /// Field paths and types from a bounded sample, and the index list.
    Describe { collection: String },
}

/// One bounded call.
#[derive(Clone, Debug, PartialEq)]
pub struct MongoRequest {
    pub action: MongoAction,
    /// Documents (or distinct values) returned at most (1..=[`MAX_ROWS`]).
    pub max_rows: usize,
    /// Server-side time limit (`maxTimeMS`); the client waits a grace past it.
    pub timeout: Duration,
}

impl MongoRequest {
    /// A request with the default row cap and time limit.
    pub fn new(action: MongoAction) -> Self {
        Self {
            action,
            max_rows: DEFAULT_MAX_ROWS,
            timeout: Duration::from_millis(DEFAULT_TIMEOUT_MS),
        }
    }
}

/// Documents from `find` or `aggregate`, or values from `distinct`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DocumentsResult {
    pub documents: Vec<Value>,
    pub count: usize,
    pub truncated: bool,
    pub truncation_reason: Option<Truncation>,
    pub elapsed_ms: u64,
}

impl DocumentsResult {
    /// Keeps the first `count` documents and records why.
    pub fn truncate_to(&mut self, count: usize, reason: Truncation) {
        if count < self.documents.len() {
            self.documents.truncate(count);
            self.count = count;
            self.truncated = true;
            self.truncation_reason = Some(reason);
        }
    }
}

/// The number of matching documents.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct CountResult {
    pub count: u64,
    pub elapsed_ms: u64,
}

/// One collection or view.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CollectionEntry {
    pub name: String,
    /// `collection`, `view`, or `timeseries`.
    #[serde(rename = "type")]
    pub kind: String,
}

/// The collections the account may list.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CollectionsResult {
    pub collections: Vec<CollectionEntry>,
    pub truncated: bool,
}

/// One field path seen in the sample.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct FieldEntry {
    pub path: String,
    /// BSON type names seen, sorted.
    pub types: Vec<String>,
    /// Sampled documents holding the path.
    pub present: usize,
}

/// One index.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct MongoIndexEntry {
    pub name: String,
    /// `field: direction-or-kind` pairs in key order.
    pub keys: Vec<String>,
    pub unique: bool,
}

/// A bounded sample's field summary and the index list.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct MongoDescribeResult {
    pub collection: String,
    pub sampled: usize,
    pub fields: Vec<FieldEntry>,
    #[serde(skip_serializing_if = "is_zero")]
    pub fields_omitted: usize,
    pub indexes: Vec<MongoIndexEntry>,
}

fn is_zero(value: &usize) -> bool {
    *value == 0
}

/// The result of one call.
#[derive(Clone, Debug, PartialEq)]
pub enum MongoOutcome {
    Documents(DocumentsResult),
    Count(CountResult),
    Collections(CollectionsResult),
    Describe(MongoDescribeResult),
}

/// The operator's choice for one server process: which variable names the
/// server, and its value as read once at startup. `Debug` never prints the
/// value.
#[derive(Clone, Eq, PartialEq)]
pub struct MongoSettings {
    env_name: String,
    value: Option<String>,
    /// [`MONGO_HANDSHAKE_ENVIRONMENT`] variables set as the driver tests
    /// them: names only.
    environment: Vec<String>,
}

impl MongoSettings {
    /// Reads `env_name`, and which handshake variables are set, once.
    pub fn from_env(env_name: &str) -> Self {
        Self {
            env_name: env_name.to_owned(),
            value: std::env::var(env_name).ok(),
            environment: MONGO_HANDSHAKE_ENVIRONMENT
                .iter()
                .filter(|name| {
                    std::env::var(name).is_ok_and(|value| target::marks_handshake(name, &value))
                })
                .map(|name| (*name).to_owned())
                .collect(),
        }
    }

    /// Settings with an explicit value and no handshake variable set; the
    /// process environment is not read.
    pub fn with_value(env_name: &str, value: Option<&str>) -> Self {
        Self {
            env_name: env_name.to_owned(),
            value: value.map(str::to_owned),
            environment: Vec::new(),
        }
    }

    /// The same settings as if the named handshake variables were set.
    pub fn with_environment(mut self, names: &[&str]) -> Self {
        self.environment = names.iter().map(|name| (*name).to_owned()).collect();
        self
    }

    /// The operator-chosen variable name (kept out of tool output).
    pub fn env_name(&self) -> &str {
        &self.env_name
    }
}

impl fmt::Debug for MongoSettings {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MongoSettings")
            .field("env_name", &self.env_name)
            .field("value", &self.value.as_ref().map(|_| "[redacted]"))
            .field("environment", &self.environment)
            .finish()
    }
}

/// The live MongoDB target for one server process, fixed at startup.
pub struct MongoService {
    target: Result<target::MongoTarget, String>,
    runtime: Mutex<Option<tokio::runtime::Runtime>>,
    connect_timeout: Duration,
}

pub(crate) fn not_configured(reason: &str) -> SqlError {
    SqlError::new(
        "mongo_not_configured",
        format!(
            "compresso_mongo has no usable server: {reason}. The operator sets the variable named by `--mongo-env` in the environment that starts the server, then restarts it."
        ),
    )
}

pub(crate) fn timeout(timeout: Duration) -> SqlError {
    SqlError::new(
        "mongo_timeout",
        format!(
            "the command did not finish within {} ms and was stopped; narrow it (filter, $limit) or raise timeout_ms (max {MAX_TIMEOUT_MS}).",
            timeout.as_millis()
        ),
    )
}

/// The arguments of one call as the BSON the driver will send, built and
/// checked before any connection.
#[derive(Clone, Debug)]
pub(crate) enum Prepared {
    Find {
        collection: String,
        filter: bson::Document,
        projection: Option<bson::Document>,
        sort: Option<bson::Document>,
        skip: u64,
    },
    Aggregate {
        collection: String,
        pipeline: Vec<bson::Document>,
    },
    Count {
        collection: String,
        filter: bson::Document,
    },
    Distinct {
        collection: String,
        field: String,
        filter: bson::Document,
    },
    Collections,
    Describe {
        collection: String,
    },
}

/// Runs the allow-list over one request. Nothing here touches the network.
pub(crate) fn prepare(request: &MongoRequest) -> Result<Prepared, SqlError> {
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
    let prepared = match &request.action {
        MongoAction::Find {
            collection,
            filter,
            projection,
            sort,
            skip,
        } => {
            gate::check_collection(collection)?;
            if *skip > MAX_SKIP {
                return Err(SqlError::new(
                    "invalid_arguments",
                    format!("skip must be at most {MAX_SKIP}"),
                ));
            }
            Prepared::Find {
                collection: collection.clone(),
                filter: gate::filter(filter)?,
                projection: projection.as_ref().map(gate::projection).transpose()?,
                sort: sort.as_ref().map(gate::sort).transpose()?,
                skip: *skip,
            }
        }
        MongoAction::Aggregate {
            collection,
            pipeline,
        } => {
            gate::check_collection(collection)?;
            Prepared::Aggregate {
                collection: collection.clone(),
                pipeline: gate::pipeline(pipeline)?,
            }
        }
        MongoAction::Count { collection, filter } => {
            gate::check_collection(collection)?;
            Prepared::Count {
                collection: collection.clone(),
                filter: gate::filter(filter)?,
            }
        }
        MongoAction::Distinct {
            collection,
            field,
            filter,
        } => {
            gate::check_collection(collection)?;
            gate::check_field(field)?;
            Prepared::Distinct {
                collection: collection.clone(),
                field: field.clone(),
                filter: gate::filter(filter)?,
            }
        }
        MongoAction::Collections => Prepared::Collections,
        MongoAction::Describe { collection } => {
            gate::check_collection(collection)?;
            Prepared::Describe {
                collection: collection.clone(),
            }
        }
    };
    prepared.assert_sendable()?;
    Ok(prepared)
}

impl Prepared {
    /// The final walk over every document this call will send.
    fn assert_sendable(&self) -> Result<(), SqlError> {
        let documents: Vec<&bson::Document> = match self {
            Self::Find {
                filter,
                projection,
                sort,
                ..
            } => [Some(filter), projection.as_ref(), sort.as_ref()]
                .into_iter()
                .flatten()
                .collect(),
            Self::Aggregate { pipeline, .. } => pipeline.iter().collect(),
            Self::Count { filter, .. } | Self::Distinct { filter, .. } => vec![filter],
            Self::Collections | Self::Describe { .. } => Vec::new(),
        };
        documents.into_iter().try_for_each(gate::assert_sendable)
    }
}

impl MongoService {
    /// Parses `settings` once. A missing or invalid value is kept as a
    /// reason (never containing the value), and every call then fails with
    /// `mongo_not_configured`.
    pub fn new(settings: &MongoSettings) -> Self {
        let target = if !is_valid_mongo_env_name(&settings.env_name) {
            Err(format!(
                "the variable name must match {MONGO_ENV_PREFIX}[A-Z0-9_]+"
            ))
        } else {
            match settings.value.as_deref() {
                None => Err("the variable is not set".to_owned()),
                Some(value) => target::MongoTarget::parse(value, &settings.environment),
            }
        };
        Self {
            target,
            runtime: Mutex::new(None),
            connect_timeout: CONNECT_TIMEOUT,
        }
    }

    /// The same service with a shorter connect deadline (tests of a stalled
    /// peer). Never longer than [`CONNECT_TIMEOUT`].
    #[doc(hidden)]
    pub fn with_connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = timeout.min(CONNECT_TIMEOUT);
        self
    }

    /// Whether the variable held a usable URL.
    pub fn is_configured(&self) -> bool {
        self.target.is_ok()
    }

    /// Why the target is unusable, if it is. Never contains the value.
    pub fn configuration_problem(&self) -> Option<&str> {
        self.target.as_ref().err().map(String::as_str)
    }

    /// The longest one call with `timeout` can take end to end.
    pub fn call_limit(&self, timeout: Duration) -> Duration {
        client::call_limit(self.connect_timeout, timeout)
    }

    /// Runs one call to completion on the calling thread. The arguments pass
    /// the allow-list before any connection is opened.
    pub fn execute(&self, request: &MongoRequest) -> Result<MongoOutcome, SqlError> {
        let target = self
            .target
            .as_ref()
            .map_err(|reason| not_configured(reason))?;
        let prepared = prepare(request)?;
        self.run(target, request, &prepared)
    }

    /// Runs one call without the allow-list, so a live test can prove what
    /// the server or the account check does alone (for example, that a
    /// `readWrite` account is refused before `$out` runs). Built only with
    /// the `test-hooks` feature, which nothing in the shipped binary enables.
    #[cfg(feature = "test-hooks")]
    #[doc(hidden)]
    pub fn execute_unchecked_pipeline_for_tests(
        &self,
        collection: &str,
        pipeline: Vec<bson::Document>,
        timeout: Duration,
    ) -> Result<MongoOutcome, SqlError> {
        let target = self
            .target
            .as_ref()
            .map_err(|reason| not_configured(reason))?;
        let request = MongoRequest {
            action: MongoAction::Collections,
            max_rows: DEFAULT_MAX_ROWS,
            timeout,
        };
        let prepared = Prepared::Aggregate {
            collection: collection.to_owned(),
            pipeline,
        };
        self.run(target, &request, &prepared)
    }

    fn run(
        &self,
        target: &target::MongoTarget,
        request: &MongoRequest,
        prepared: &Prepared,
    ) -> Result<MongoOutcome, SqlError> {
        let mut runtime = self.runtime.lock().unwrap_or_else(PoisonError::into_inner);
        if runtime.is_none() {
            *runtime = Some(client::runtime()?);
        }
        let Some(runtime) = runtime.as_ref() else {
            return Err(SqlError::new("internal", "MongoDB runtime unavailable"));
        };
        client::Bound {
            target,
            runtime,
            connect_timeout: self.connect_timeout,
        }
        .run(request, prepared)
        .map(bound_metadata)
        .map_err(|error| SqlError::new(error.code(), target.secrets.scrub(error.message())))
    }
}

impl Drop for MongoService {
    /// Shuts the runtime down without waiting, as the SQL service does.
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

impl fmt::Debug for MongoService {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MongoService")
            .field("configured", &self.target.is_ok())
            .finish_non_exhaustive()
    }
}

/// Cuts names in `collections` and `describe` results.
fn bound_metadata(outcome: MongoOutcome) -> MongoOutcome {
    use crate::cells::{MAX_METADATA_CHARS, MAX_NAME_CHARS, cut};
    match outcome {
        MongoOutcome::Collections(mut result) => {
            result.collections.truncate(MAX_TABLES);
            for entry in &mut result.collections {
                entry.name = cut(&entry.name, MAX_METADATA_CHARS);
                entry.kind = cut(&entry.kind, MAX_NAME_CHARS);
            }
            MongoOutcome::Collections(result)
        }
        MongoOutcome::Describe(mut result) => {
            result.collection = cut(&result.collection, MAX_METADATA_CHARS);
            for field in &mut result.fields {
                field.path = cut(&field.path, MAX_METADATA_CHARS);
            }
            for index in &mut result.indexes {
                index.name = cut(&index.name, MAX_METADATA_CHARS);
                index.keys = index
                    .keys
                    .iter()
                    .map(|key| cut(key, MAX_METADATA_CHARS))
                    .collect();
            }
            MongoOutcome::Describe(result)
        }
        other => other,
    }
}
