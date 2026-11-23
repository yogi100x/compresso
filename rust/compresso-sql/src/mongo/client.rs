//! MongoDB through the official driver (E17-52, D117).
//!
//! Every call builds a fresh client with options set field by field (never
//! from the URL text), runs the account check, runs one command the tool
//! built, and shuts the client down. Per call:
//!
//! 1. connect: one host, a direct connection, a pool of one, no retries, no
//!    compression, read concern `local`, read preference `primaryPreferred`
//!    (so a direct connection to a secondary member still reads), TLS per
//!    [`super::target::MongoTls`] (10 s for server selection and connect);
//! 2. `connectionStatus` with `showPrivileges`, checked by
//!    [`super::account::check`] (fail closed), then `hostInfo` on `admin`,
//!    which must be refused as `Unauthorized` (the server enforces access
//!    control);
//! 3. the action, with `maxTimeMS` set to the call's time limit, a row
//!    limit one past `max_rows` (to tell a full result from a cut one), and
//!    `allowDiskUse: false`; reading stops at the row or byte cap;
//! 4. an immediate shutdown, waited on for at most 2 s.
//!
//! The client deadline is the time limit plus a grace period; when it passes
//! the call is abandoned (the server's own limit stops the command).

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use bson::{Bson, Document, doc};
use futures_util::TryStreamExt;
use mongodb::Client;
use mongodb::error::ErrorKind;
use mongodb::options::{
    AuthMechanism, ClientOptions, Credential, ReadConcern, ReadPreference, SelectionCriteria,
    ServerAddress, ServerMonitoringMode, Tls, TlsOptions,
};
use tokio::runtime::Runtime;

use super::output::{self, ValueCollector};
use super::target::{Mechanism, MongoTarget, MongoTls};
use super::{
    CollectionEntry, CollectionsResult, CountResult, DESCRIBE_SAMPLE, DocumentsResult, FieldEntry,
    MAX_DESCRIBED_FIELDS, MongoDescribeResult, MongoIndexEntry, MongoOutcome, MongoRequest,
    Prepared, account, timeout,
};
use crate::{MAX_TABLES, SqlError};

/// Time the client waits past the server-side time limit.
const CLIENT_GRACE: Duration = Duration::from_secs(2);
/// Longest wait for the account check once connected.
const ACCOUNT_TIMEOUT: Duration = Duration::from_secs(5);
/// Longest wait for the client to shut down; a stalled peer is not awaited.
const FINISH_TIMEOUT: Duration = Duration::from_secs(2);

/// The longest one call can take, end to end.
pub(crate) fn call_limit(connect_timeout: Duration, timeout: Duration) -> Duration {
    connect_timeout + ACCOUNT_TIMEOUT + timeout + CLIENT_GRACE + FINISH_TIMEOUT
}

/// The runtime that drives MongoDB calls, separate from the MCP server's.
pub(crate) fn runtime() -> Result<Runtime, SqlError> {
    tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
        .map_err(|_| SqlError::new("internal", "could not start the MongoDB runtime"))
}

/// A target with the runtime that runs its calls.
pub(crate) struct Bound<'a> {
    pub(crate) target: &'a MongoTarget,
    pub(crate) runtime: &'a Runtime,
    pub(crate) connect_timeout: Duration,
}

impl Bound<'_> {
    pub(crate) fn run(
        &self,
        request: &MongoRequest,
        prepared: &Prepared,
    ) -> Result<MongoOutcome, SqlError> {
        let limit = call_limit(self.connect_timeout, request.timeout);
        // As for SQL (E17-50): the runtime is driven from a thread of its
        // own, so a caller already inside a Tokio runtime cannot panic.
        std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    self.runtime.block_on(async {
                        tokio::time::timeout(limit, self.call(request, prepared))
                            .await
                            .unwrap_or_else(|_| Err(timeout(request.timeout)))
                    })
                })
                .join()
                .unwrap_or_else(|_| Err(SqlError::new("internal", "the MongoDB call failed")))
        })
    }

    fn options(&self) -> ClientOptions {
        let target = self.target;
        let mut credential = Credential::default();
        credential.username = Some(target.username.clone());
        credential.password = Some(target.password.clone());
        credential.source = Some(
            target
                .auth_source
                .clone()
                .unwrap_or_else(|| target.database.clone()),
        );
        credential.mechanism = target.mechanism.map(|mechanism| match mechanism {
            Mechanism::ScramSha256 => AuthMechanism::ScramSha256,
            Mechanism::ScramSha1 => AuthMechanism::ScramSha1,
        });
        let tls = match &target.tls {
            MongoTls::Off => Tls::Disabled,
            MongoTls::Verified { ca_file } => {
                let mut options = TlsOptions::default();
                options.ca_file_path = ca_file.clone();
                options.allow_invalid_certificates = Some(false);
                Tls::Enabled(options)
            }
        };
        let mut options = ClientOptions::default();
        options.hosts = vec![ServerAddress::Tcp {
            host: target.host.clone(),
            port: Some(target.port),
        }];
        options.credential = Some(credential);
        options.direct_connection = Some(true);
        options.max_pool_size = Some(1);
        options.min_pool_size = Some(0);
        options.max_connecting = Some(1);
        options.retry_reads = Some(false);
        options.retry_writes = Some(false);
        options.connect_timeout = Some(self.connect_timeout);
        options.server_selection_timeout = Some(self.connect_timeout);
        options.server_monitoring_mode = Some(ServerMonitoringMode::Poll);
        options.read_concern = Some(ReadConcern::local());
        options.selection_criteria = Some(SelectionCriteria::ReadPreference(
            ReadPreference::PrimaryPreferred { options: None },
        ));
        options.default_database = Some(target.database.clone());
        options.tls = Some(tls);
        options
    }

    async fn call(
        &self,
        request: &MongoRequest,
        prepared: &Prepared,
    ) -> Result<MongoOutcome, SqlError> {
        let client = Client::with_options(self.options()).map_err(map_error)?;
        let result = self.session(&client, request, prepared).await;
        let _ = tokio::time::timeout(FINISH_TIMEOUT, client.shutdown().immediate(true)).await;
        result
    }

    async fn session(
        &self,
        client: &Client,
        request: &MongoRequest,
        prepared: &Prepared,
    ) -> Result<MongoOutcome, SqlError> {
        let database = client.database(&self.target.database);
        let check = tokio::time::timeout(
            self.connect_timeout + ACCOUNT_TIMEOUT,
            database.run_command(doc! {"connectionStatus": 1, "showPrivileges": true}),
        )
        .await
        .map_err(|_| {
            SqlError::new(
                "mongo_connection",
                format!(
                    "could not connect and check the account within {} ms",
                    (self.connect_timeout + ACCOUNT_TIMEOUT).as_millis()
                ),
            )
        })?
        .map_err(map_error)?;
        account::check(&check, &self.target.database)
            .map_err(|rule| SqlError::new("mongo_account_refused", rule.message()))?;
        let probe = tokio::time::timeout(
            ACCOUNT_TIMEOUT,
            client
                .database("admin")
                .run_command(doc! {account::ENFORCEMENT_PROBE: 1}),
        )
        .await
        .map_err(|_| {
            SqlError::new(
                "mongo_connection",
                "the server did not answer the access-control check in time",
            )
        })?;
        let probe = match probe {
            Ok(_) => Ok(()),
            Err(error) => match error.kind.as_ref() {
                ErrorKind::Command(command) => Err(Some(command.code)),
                _ => return Err(map_error(error)),
            },
        };
        account::check_enforcement(probe)
            .map_err(|rule| SqlError::new("mongo_account_refused", rule.message()))?;
        tokio::time::timeout(
            request.timeout + CLIENT_GRACE,
            action(&database, request, prepared),
        )
        .await
        .unwrap_or_else(|_| Err(timeout(request.timeout)))
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Rows asked of the server: one past the cap tells a cut result apart.
fn server_limit(max_rows: usize) -> i64 {
    i64::try_from(max_rows)
        .unwrap_or(i64::MAX)
        .saturating_add(1)
}

fn batch_size(max_rows: usize) -> u32 {
    u32::try_from(max_rows + 1).unwrap_or(u32::MAX).min(1_001)
}

async fn collect(
    mut cursor: mongodb::Cursor<Document>,
    max_rows: usize,
    started: Instant,
) -> Result<DocumentsResult, SqlError> {
    let mut collector = ValueCollector::new(max_rows);
    while let Some(document) = cursor.try_next().await.map_err(map_error)? {
        if !collector.push(output::render_document(&document)) {
            break;
        }
    }
    Ok(finish(collector, started))
}

fn finish(collector: ValueCollector, started: Instant) -> DocumentsResult {
    let (documents, truncation) = collector.finish();
    DocumentsResult {
        count: documents.len(),
        documents,
        truncated: truncation.is_some(),
        truncation_reason: truncation,
        elapsed_ms: elapsed_ms(started),
    }
}

async fn action(
    database: &mongodb::Database,
    request: &MongoRequest,
    prepared: &Prepared,
) -> Result<MongoOutcome, SqlError> {
    let started = Instant::now();
    let max_rows = request.max_rows;
    match prepared.clone() {
        Prepared::Find {
            collection,
            filter,
            projection,
            sort,
            skip,
        } => {
            let collection = database.collection::<Document>(&collection);
            let mut find = collection
                .find(filter)
                .limit(server_limit(max_rows))
                .batch_size(batch_size(max_rows))
                .skip(skip)
                .max_time(request.timeout)
                .allow_disk_use(false);
            if let Some(projection) = projection {
                find = find.projection(projection);
            }
            if let Some(sort) = sort {
                find = find.sort(sort);
            }
            let cursor = find.await.map_err(map_error)?;
            collect(cursor, max_rows, started)
                .await
                .map(MongoOutcome::Documents)
        }
        Prepared::Aggregate {
            collection,
            mut pipeline,
        } => {
            pipeline.push(doc! {"$limit": server_limit(max_rows)});
            let cursor = database
                .collection::<Document>(&collection)
                .aggregate(pipeline)
                .max_time(request.timeout)
                .allow_disk_use(false)
                .batch_size(batch_size(max_rows))
                .await
                .map_err(map_error)?;
            collect(cursor, max_rows, started)
                .await
                .map(MongoOutcome::Documents)
        }
        Prepared::Count { collection, filter } => {
            let count = database
                .collection::<Document>(&collection)
                .count_documents(filter)
                .max_time(request.timeout)
                .await
                .map_err(map_error)?;
            Ok(MongoOutcome::Count(CountResult {
                count,
                elapsed_ms: elapsed_ms(started),
            }))
        }
        Prepared::Distinct {
            collection,
            field,
            filter,
        } => {
            let values = database
                .collection::<Document>(&collection)
                .distinct(field, filter)
                .max_time(request.timeout)
                .await
                .map_err(map_error)?;
            let mut collector = ValueCollector::new(max_rows);
            for value in &values {
                if !collector.push(output::render(value)) {
                    break;
                }
            }
            Ok(MongoOutcome::Documents(finish(collector, started)))
        }
        Prepared::Collections => {
            let mut cursor = database
                .list_collections()
                .authorized_collections(true)
                .await
                .map_err(map_error)?;
            let mut collections = Vec::new();
            let mut truncated = false;
            while let Some(specification) = cursor.try_next().await.map_err(map_error)? {
                if collections.len() == MAX_TABLES {
                    truncated = true;
                    break;
                }
                let kind = match specification.collection_type {
                    mongodb::results::CollectionType::View => "view",
                    mongodb::results::CollectionType::Timeseries => "timeseries",
                    _ => "collection",
                };
                collections.push(CollectionEntry {
                    name: specification.name,
                    kind: kind.to_owned(),
                });
            }
            collections.sort_by(|left, right| left.name.cmp(&right.name));
            Ok(MongoOutcome::Collections(CollectionsResult {
                collections,
                truncated,
            }))
        }
        Prepared::Describe { collection } => {
            let handle = database.collection::<Document>(&collection);
            let sample = i64::try_from(DESCRIBE_SAMPLE).unwrap_or(100);
            let mut cursor = handle
                .aggregate(vec![doc! {"$sample": {"size": sample}}])
                .max_time(request.timeout)
                .allow_disk_use(false)
                .await
                .map_err(map_error)?;
            let mut summary = FieldSummary::default();
            let mut sampled = 0;
            while let Some(document) = cursor.try_next().await.map_err(map_error)? {
                sampled += 1;
                summary.add(&document);
                if sampled == DESCRIBE_SAMPLE {
                    break;
                }
            }
            let mut indexes = Vec::new();
            let mut cursor = handle.list_indexes().await.map_err(map_error)?;
            while let Some(index) = cursor.try_next().await.map_err(map_error)? {
                if indexes.len() == MAX_TABLES {
                    break;
                }
                let options = index.options.unwrap_or_default();
                indexes.push(MongoIndexEntry {
                    name: options.name.unwrap_or_default(),
                    keys: index
                        .keys
                        .iter()
                        .map(|(key, value)| format!("{key}: {}", key_kind(value)))
                        .collect(),
                    unique: options.unique.unwrap_or(false),
                });
            }
            let (fields, fields_omitted) = summary.finish();
            Ok(MongoOutcome::Describe(MongoDescribeResult {
                collection,
                sampled,
                fields,
                fields_omitted,
                indexes,
            }))
        }
    }
}

fn key_kind(value: &Bson) -> String {
    match value {
        Bson::String(text) => text.clone(),
        Bson::Int32(number) => number.to_string(),
        Bson::Int64(number) => number.to_string(),
        Bson::Double(number) => number.to_string(),
        _ => "?".to_owned(),
    }
}

/// Field paths (nested documents to three levels) and the types seen.
#[derive(Default)]
pub(crate) struct FieldSummary {
    fields: BTreeMap<String, (BTreeSet<&'static str>, usize)>,
}

impl FieldSummary {
    pub(crate) fn add(&mut self, document: &Document) {
        self.walk(document, "", 0);
    }

    fn walk(&mut self, document: &Document, prefix: &str, depth: usize) {
        for (key, value) in document {
            let path = if prefix.is_empty() {
                key.to_string()
            } else {
                format!("{prefix}.{key}")
            };
            let entry = self.fields.entry(path.clone()).or_default();
            entry.0.insert(type_name(value));
            entry.1 += 1;
            if let Bson::Document(inner) = value
                && depth < 2
            {
                self.walk(inner, &path, depth + 1);
            }
        }
    }

    pub(crate) fn finish(self) -> (Vec<FieldEntry>, usize) {
        let total = self.fields.len();
        let fields = self
            .fields
            .into_iter()
            .take(MAX_DESCRIBED_FIELDS)
            .map(|(path, (types, present))| FieldEntry {
                path,
                types: types.into_iter().map(str::to_owned).collect(),
                present,
            })
            .collect::<Vec<_>>();
        let omitted = total - fields.len();
        (fields, omitted)
    }
}

/// The BSON type name MongoDB's `$type` uses.
fn type_name(value: &Bson) -> &'static str {
    match value {
        Bson::Double(_) => "double",
        Bson::String(_) => "string",
        Bson::Array(_) => "array",
        Bson::Document(_) => "object",
        Bson::Boolean(_) => "bool",
        Bson::Null => "null",
        Bson::RegularExpression(_) => "regex",
        Bson::JavaScriptCode(_) => "javascript",
        Bson::JavaScriptCodeWithScope(_) => "javascriptWithScope",
        Bson::Int32(_) => "int",
        Bson::Int64(_) => "long",
        Bson::Timestamp(_) => "timestamp",
        Bson::Binary(_) => "binData",
        Bson::ObjectId(_) => "objectId",
        Bson::DateTime(_) => "date",
        Bson::Symbol(_) => "symbol",
        Bson::Decimal128(_) => "decimal",
        Bson::Undefined => "undefined",
        Bson::MaxKey => "maxKey",
        Bson::MinKey => "minKey",
        Bson::DbPointer(_) => "dbPointer",
    }
}

/// Server error codes the tool names.
const MAX_TIME_MS_EXPIRED: i32 = 50;
const UNAUTHORIZED: i32 = 13;

fn map_error(error: mongodb::error::Error) -> SqlError {
    let text = error.to_string();
    match error.kind.as_ref() {
        ErrorKind::Command(command) if command.code == MAX_TIME_MS_EXPIRED => SqlError::new(
            "mongo_timeout",
            "the command did not finish within timeout_ms and was stopped (MaxTimeMSExpired); narrow it or raise timeout_ms.",
        ),
        ErrorKind::Command(command) if command.code == UNAUTHORIZED => SqlError::new(
            "mongo_permission",
            format!("MongoDB denied this to the configured account: {text}"),
        ),
        ErrorKind::Command(_) => SqlError::new("mongo_error", format!("MongoDB: {text}")),
        ErrorKind::InvalidArgument { .. } => {
            SqlError::new("mongo_error", format!("MongoDB driver: {text}"))
        }
        ErrorKind::InvalidTlsConfig { .. } => {
            super::not_configured("the driver rejected the TLS settings")
        }
        _ => SqlError::new(
            "mongo_connection",
            format!("could not talk to the MongoDB server: {text}"),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_field_summary_counts_paths_and_types() {
        let mut summary = FieldSummary::default();
        summary.add(&doc! {"a": 1, "b": {"c": "x", "d": {"e": {"f": 1}}}});
        summary.add(&doc! {"a": "one"});
        let (fields, omitted) = summary.finish();
        assert_eq!(omitted, 0);
        let a = fields.iter().find(|field| field.path == "a").unwrap();
        assert_eq!(a.types, vec!["int", "string"]);
        assert_eq!(a.present, 2);
        assert!(fields.iter().any(|field| field.path == "b.d.e"));
        assert!(!fields.iter().any(|field| field.path == "b.d.e.f"));
    }

    #[test]
    fn test_the_call_limit_covers_every_phase() {
        let limit = call_limit(Duration::from_secs(10), Duration::from_secs(5));
        assert_eq!(limit, Duration::from_secs(10 + 5 + 5 + 2 + 2));
        assert_eq!(server_limit(100), 101);
        assert_eq!(batch_size(1_000), 1_001);
    }
}
