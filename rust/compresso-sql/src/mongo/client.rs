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
//! 3. `listCollections` (names only) for every collection the call reads,
//!    the action's own and each `$lookup` source: a view is refused, since
//!    reading it runs a stored pipeline the allow-list never saw;
//! 4. the action, with `maxTimeMS` set to the call's time limit, a row
//!    limit one past `max_rows` (to tell a full result from a cut one), and
//!    `allowDiskUse: false`. Documents are read as raw BSON (never through
//!    an extended-JSON reading) and reading stops at the row cap, the 256 KiB
//!    result cap, or [`MAX_READ_BYTES`] of raw documents, whichever comes
//!    first; `describe` stops at [`MAX_SAMPLE_BYTES`] and tracks at most
//!    [`MAX_TRACKED_PATHS`] field paths;
//! 5. an immediate shutdown, waited on for at most 2 s.
//!
//! The client deadline is the time limit plus a grace period; when it passes
//! the call is abandoned (the server's own limit stops the command). Errors
//! carry a fixed message, the server's numeric code and code name, and at
//! most a cut, escaped copy of the server's own message: never the driver's
//! rendering of a reply document.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use bson::{Bson, Document, RawDocumentBuf, Utf8Lossy, doc};
use futures_util::TryStreamExt;
use mongodb::Client;
use mongodb::error::ErrorKind;
use mongodb::options::{
    AggregateOptions, AuthMechanism, ClientOptions, Credential, FindOptions, ReadConcern,
    ReadPreference, SelectionCriteria, ServerAddress, ServerMonitoringMode, Tls, TlsOptions,
};
use tokio::runtime::Runtime;

use super::output::{self, ValueCollector};
use super::target::{Mechanism, MongoTarget, MongoTls};
use super::{
    CollectionEntry, CollectionsResult, CountResult, DESCRIBE_SAMPLE, DocumentsResult, FieldEntry,
    MAX_DESCRIBED_FIELDS, MongoDescribeResult, MongoIndexEntry, MongoOutcome, MongoRequest,
    Prepared, account, timeout,
};
use crate::cells::cut;
use crate::{MAX_TABLES, SqlError, Truncation};

/// Time the client waits past the server-side time limit.
const CLIENT_GRACE: Duration = Duration::from_secs(2);
/// Longest wait for the account check once connected.
const ACCOUNT_TIMEOUT: Duration = Duration::from_secs(5);
/// Longest wait for the client to shut down; a stalled peer is not awaited.
const FINISH_TIMEOUT: Duration = Duration::from_secs(2);
/// Raw document bytes one `find`, `aggregate`, or `distinct` reads at most.
/// The server sends at most 16 MiB per reply, so this admits one
/// maximum-size document and a full reply besides, while 1,000 rows of
/// 16 MiB each (16 GiB) can never be held: reading stops at the first
/// document past it, marked `max_bytes`.
pub(crate) const MAX_READ_BYTES: usize = 32 * 1024 * 1024;
/// Raw document bytes `describe` samples at most (100 documents of about
/// 80 KiB each); the rest of the sample is not read.
pub(crate) const MAX_SAMPLE_BYTES: usize = 8 * 1024 * 1024;
/// Distinct field paths `describe` tracks; a path first seen past it is
/// counted in `fields_omitted` instead.
pub(crate) const MAX_TRACKED_PATHS: usize = 4 * MAX_DESCRIBED_FIELDS;

/// The longest one call can take, end to end.
pub(crate) fn call_limit(connect_timeout: Duration, timeout: Duration) -> Duration {
    connect_timeout + ACCOUNT_TIMEOUT * 2 + timeout + CLIENT_GRACE + FINISH_TIMEOUT
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

/// The client options for one call, set field by field.
pub(crate) fn client_options(target: &MongoTarget, connect_timeout: Duration) -> ClientOptions {
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
    options.connect_timeout = Some(connect_timeout);
    options.server_selection_timeout = Some(connect_timeout);
    options.server_monitoring_mode = Some(ServerMonitoringMode::Poll);
    options.read_concern = Some(ReadConcern::local());
    options.selection_criteria = Some(SelectionCriteria::ReadPreference(
        ReadPreference::PrimaryPreferred { options: None },
    ));
    options.default_database = Some(target.database.clone());
    options.tls = Some(tls);
    options
}

/// `find` options: the row limit, batch size, skip, time limit, and no disk
/// use, plus the checked projection and sort.
pub(crate) fn find_options(
    max_rows: usize,
    skip: u64,
    timeout: Duration,
    projection: Option<Document>,
    sort: Option<Document>,
) -> FindOptions {
    let mut options = FindOptions::default();
    options.limit = Some(server_limit(max_rows));
    options.batch_size = Some(batch_size(max_rows));
    options.skip = Some(skip);
    options.max_time = Some(timeout);
    options.allow_disk_use = Some(false);
    options.projection = projection;
    options.sort = sort;
    options
}

/// `aggregate` options: time limit, batch size, and no disk use.
pub(crate) fn aggregate_options(max_rows: usize, timeout: Duration) -> AggregateOptions {
    let mut options = AggregateOptions::default();
    options.max_time = Some(timeout);
    options.allow_disk_use = Some(false);
    options.batch_size = Some(batch_size(max_rows));
    options
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

    async fn call(
        &self,
        request: &MongoRequest,
        prepared: &Prepared,
    ) -> Result<MongoOutcome, SqlError> {
        let client = Client::with_options(client_options(self.target, self.connect_timeout))
            .map_err(map_error)?;
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
        let read = prepared.collections_read();
        if !read.is_empty() {
            let reply = tokio::time::timeout(
                ACCOUNT_TIMEOUT,
                database.run_command(doc! {
                    "listCollections": 1,
                    "filter": {"name": {"$in": read.clone()}},
                    "nameOnly": true,
                    "authorizedCollections": true,
                }),
            )
            .await
            .map_err(|_| {
                SqlError::new(
                    "mongo_connection",
                    "the server did not list the collections in time",
                )
            })?
            .map_err(map_error)?;
            check_no_views(&reply)?;
        }
        tokio::time::timeout(
            request.timeout + CLIENT_GRACE,
            action(&database, request, prepared),
        )
        .await
        .unwrap_or_else(|_| Err(timeout(request.timeout)))
    }
}

/// Refuses the call if the `listCollections` reply names a view; an
/// unreadable reply refuses too.
pub(crate) fn check_no_views(reply: &Document) -> Result<(), SqlError> {
    let unreadable = || {
        SqlError::new(
            "mongo_refused",
            "the server's collection list could not be read, so the call is refused (fail closed).",
        )
    };
    let batch = reply
        .get_document("cursor")
        .and_then(|cursor| cursor.get_array("firstBatch"))
        .map_err(|_| unreadable())?;
    for entry in batch {
        let Bson::Document(entry) = entry else {
            return Err(unreadable());
        };
        match entry.get_str("type") {
            Ok("view") => {
                return Err(SqlError::new(
                    "mongo_view_refused",
                    "the call reads a view; reading a view runs a pipeline stored on the server that the allow-list never checks, so views are refused. Read the collections it is built on.",
                ));
            }
            Ok(_) => {}
            Err(_) => return Err(unreadable()),
        }
    }
    Ok(())
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

/// A raw document as a `Document`, read element by element: no
/// extended-JSON reading, so a stored `$oid` key stays a key.
fn decode(raw: &RawDocumentBuf) -> Result<Document, SqlError> {
    Utf8Lossy::<Document>::try_from(raw.as_ref())
        .map(|lossy| lossy.0)
        .map_err(|_| SqlError::new("mongo_error", "a document in the result is not valid BSON"))
}

/// Counts raw bytes read against a budget.
pub(crate) struct ReadBudget {
    used: usize,
    limit: usize,
}

impl ReadBudget {
    pub(crate) fn new(limit: usize) -> Self {
        Self { used: 0, limit }
    }

    /// Whether a document of `bytes` may still be read.
    pub(crate) fn admit(&mut self, bytes: usize) -> bool {
        if self.used.saturating_add(bytes) > self.limit {
            return false;
        }
        self.used += bytes;
        true
    }
}

async fn collect(
    mut cursor: mongodb::Cursor<RawDocumentBuf>,
    max_rows: usize,
    started: Instant,
) -> Result<DocumentsResult, SqlError> {
    let mut collector = ValueCollector::new(max_rows);
    let mut budget = ReadBudget::new(MAX_READ_BYTES);
    let mut over_budget = false;
    while let Some(raw) = cursor.try_next().await.map_err(map_error)? {
        if !budget.admit(raw.as_bytes().len()) {
            over_budget = true;
            break;
        }
        if !collector.push(output::render_document(&decode(&raw)?)) {
            break;
        }
    }
    let mut result = finish(collector, started);
    if over_budget && result.truncation_reason.is_none() {
        result.truncated = true;
        result.truncation_reason = Some(Truncation::MaxBytes);
    }
    Ok(result)
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
            let cursor = database
                .collection::<RawDocumentBuf>(&collection)
                .find(filter)
                .with_options(find_options(
                    max_rows,
                    skip,
                    request.timeout,
                    projection,
                    sort,
                ))
                .await
                .map_err(map_error)?;
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
                .collection::<RawDocumentBuf>(&collection)
                .aggregate(pipeline)
                .with_options(aggregate_options(max_rows, request.timeout))
                .with_type::<RawDocumentBuf>()
                .await
                .map_err(map_error)?;
            collect(cursor, max_rows, started)
                .await
                .map(MongoOutcome::Documents)
        }
        Prepared::Count { collection, filter } => {
            let count = database
                .collection::<RawDocumentBuf>(&collection)
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
            // One reply document of at most 16 MiB, bounded by the server.
            let values = database
                .collection::<RawDocumentBuf>(&collection)
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
            let handle = database.collection::<RawDocumentBuf>(&collection);
            let sample = i64::try_from(DESCRIBE_SAMPLE).unwrap_or(100);
            let mut cursor = handle
                .aggregate(vec![doc! {"$sample": {"size": sample}}])
                .with_options(aggregate_options(DESCRIBE_SAMPLE, request.timeout))
                .with_type::<RawDocumentBuf>()
                .await
                .map_err(map_error)?;
            let mut summary = FieldSummary::default();
            let mut budget = ReadBudget::new(MAX_SAMPLE_BYTES);
            let mut sampled = 0;
            while let Some(raw) = cursor.try_next().await.map_err(map_error)? {
                if !budget.admit(raw.as_bytes().len()) {
                    break;
                }
                sampled += 1;
                summary.add(&decode(&raw)?);
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

/// Field paths (nested documents to three levels) and the types seen. At
/// most [`MAX_TRACKED_PATHS`] distinct paths are kept; a path first seen
/// after that is counted in `untracked` and never stored, so memory does not
/// grow with the number of keys in the sample.
#[derive(Default)]
pub(crate) struct FieldSummary {
    fields: BTreeMap<String, (BTreeSet<&'static str>, usize)>,
    untracked: usize,
}

impl FieldSummary {
    pub(crate) fn add(&mut self, document: &Document) {
        self.walk(document, "", 0);
    }

    fn walk(&mut self, document: &Document, prefix: &str, depth: usize) {
        for (key, value) in document {
            let path = if prefix.is_empty() {
                output::stored_key(key)
            } else {
                format!("{prefix}.{}", output::stored_key(key))
            };
            if !self.fields.contains_key(&path) && self.fields.len() >= MAX_TRACKED_PATHS {
                self.untracked += 1;
                continue;
            }
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

    /// The first [`MAX_DESCRIBED_FIELDS`] paths in order, and how many more
    /// were seen: tracked paths not shown plus occurrences of untracked
    /// paths (an untracked path seen in several documents counts once per
    /// document).
    pub(crate) fn finish(self) -> (Vec<FieldEntry>, usize) {
        let tracked = self.fields.len();
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
        let omitted = tracked - fields.len() + self.untracked;
        (fields, omitted)
    }

    #[cfg(test)]
    fn tracked(&self) -> usize {
        self.fields.len()
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
/// Characters of a server error message kept.
const MAX_SERVER_MESSAGE_CHARS: usize = 200;

/// A server code name as shown: letters and digits only, else nothing.
fn code_name(name: &str) -> String {
    if !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|character| character.is_ascii_alphanumeric())
    {
        format!(" {name}")
    } else {
        String::new()
    }
}

/// One driver error as a fixed code and message. The driver's own rendering
/// of an error is never used: it can embed the server's reply document as
/// hex, where a URL part cannot be scrubbed. A command error keeps the
/// server's numeric code, its code name, and its message cut to 200
/// characters with hidden characters escaped (it can quote a stored value).
pub(crate) fn map_error(error: mongodb::error::Error) -> SqlError {
    match error.kind.as_ref() {
        ErrorKind::Command(command) if command.code == MAX_TIME_MS_EXPIRED => SqlError::new(
            "mongo_timeout",
            "the command did not finish within timeout_ms and was stopped (MaxTimeMSExpired); narrow it or raise timeout_ms.",
        ),
        ErrorKind::Command(command) if command.code == UNAUTHORIZED => SqlError::new(
            "mongo_permission",
            "MongoDB denied this to the configured account (code 13 Unauthorized).",
        ),
        ErrorKind::Command(command) => SqlError::new(
            "mongo_error",
            format!(
                "MongoDB refused the command (code {}{}): {}",
                command.code,
                code_name(&command.code_name),
                cut(&command.message, MAX_SERVER_MESSAGE_CHARS)
            ),
        ),
        ErrorKind::Authentication { .. } => SqlError::new(
            "mongo_connection",
            "authentication failed; check the user, password, authSource, and authMechanism in the URL.",
        ),
        ErrorKind::ServerSelection { message, .. } => {
            let reason = if message.contains("certificate") || message.contains("Tls") {
                "TLS refused the server's certificate (wrong host name or an untrusted root)"
            } else if message.contains("refused") {
                "the server refused the connection"
            } else {
                "no usable server answered within the connect deadline"
            };
            SqlError::new(
                "mongo_connection",
                format!("could not reach the MongoDB server: {reason}."),
            )
        }
        ErrorKind::Io(_) | ErrorKind::ConnectionPoolCleared { .. } => SqlError::new(
            "mongo_connection",
            "the connection to the MongoDB server failed.",
        ),
        ErrorKind::InvalidTlsConfig { .. } => {
            super::not_configured("the driver rejected the TLS settings")
        }
        ErrorKind::InvalidArgument { .. } => {
            SqlError::new("mongo_error", "the driver refused the request's arguments.")
        }
        _ => SqlError::new(
            "mongo_error",
            "the MongoDB driver reported an error it could not classify.",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mongo::target::MongoTarget;

    #[test]
    fn test_field_summary_counts_paths_and_types() {
        let mut summary = FieldSummary::default();
        summary.add(&doc! {"a": 1, "b": {"c": "x", "d": {"e": {"f": 1}}}});
        summary.add(&doc! {"a": "one", "$oid": 1});
        let (fields, omitted) = summary.finish();
        assert_eq!(omitted, 0);
        let a = fields.iter().find(|field| field.path == "a").unwrap();
        assert_eq!(a.types, vec!["int", "string"]);
        assert_eq!(a.present, 2);
        assert!(fields.iter().any(|field| field.path == "b.d.e"));
        assert!(!fields.iter().any(|field| field.path == "b.d.e.f"));
        assert!(fields.iter().any(|field| field.path == "\\$oid"));
    }

    /// The review's case, scaled: 100 documents of 10,000 distinct keys. Memory
    /// must stay at the tracked-path bound, not grow with every key.
    #[test]
    fn test_field_summary_memory_is_bounded_by_tracked_paths() {
        let mut summary = FieldSummary::default();
        for document in 0..100 {
            let mut wide = Document::new();
            for key in 0..10_000 {
                wide.insert(format!("d{document}k{key}"), 1_i32);
            }
            summary.add(&wide);
            assert!(summary.tracked() <= MAX_TRACKED_PATHS);
        }
        assert_eq!(summary.tracked(), MAX_TRACKED_PATHS);
        let (fields, omitted) = summary.finish();
        assert_eq!(fields.len(), MAX_DESCRIBED_FIELDS);
        assert_eq!(omitted, 100 * 10_000 - MAX_DESCRIBED_FIELDS);
    }

    #[test]
    fn test_the_read_budget_stops_before_holding_too_much() {
        let mut budget = ReadBudget::new(MAX_READ_BYTES);
        let sixteen = 16 * 1024 * 1024;
        assert!(budget.admit(sixteen));
        assert!(budget.admit(sixteen));
        assert!(!budget.admit(1));
        let mut budget = ReadBudget::new(MAX_SAMPLE_BYTES);
        assert!(!budget.admit(MAX_SAMPLE_BYTES + 1));
        assert!(MAX_READ_BYTES <= 32 * 1024 * 1024);
        assert!(MAX_SAMPLE_BYTES <= 8 * 1024 * 1024);
    }

    #[test]
    fn test_the_call_limit_covers_every_phase() {
        let limit = call_limit(Duration::from_secs(10), Duration::from_secs(5));
        assert_eq!(limit, Duration::from_secs(10 + 5 + 5 + 5 + 2 + 2));
        assert_eq!(server_limit(100), 101);
        assert_eq!(batch_size(1_000), 1_001);
    }

    #[test]
    fn test_client_and_action_options_carry_every_bound() {
        let target = MongoTarget::parse("mongodb://u:pw@db.example.com/app", &[]).expect("target");
        let options = client_options(&target, Duration::from_secs(3));
        assert_eq!(options.hosts.len(), 1);
        assert_eq!(options.direct_connection, Some(true));
        assert_eq!(options.retry_reads, Some(false));
        assert_eq!(options.retry_writes, Some(false));
        assert_eq!(options.max_pool_size, Some(1));
        assert_eq!(
            options.server_selection_timeout,
            Some(Duration::from_secs(3))
        );
        assert!(matches!(options.tls, Some(Tls::Enabled(_))));
        let Some(Tls::Enabled(tls)) = &options.tls else {
            unreachable!()
        };
        assert_eq!(tls.allow_invalid_certificates, Some(false));
        let credential = options.credential.as_ref().expect("credential");
        assert_eq!(credential.source.as_deref(), Some("app"));

        let find = find_options(10, 3, Duration::from_millis(700), None, None);
        assert_eq!(find.allow_disk_use, Some(false));
        assert_eq!(find.max_time, Some(Duration::from_millis(700)));
        assert_eq!(find.limit, Some(11));
        assert_eq!(find.skip, Some(3));
        let aggregate = aggregate_options(10, Duration::from_millis(700));
        assert_eq!(aggregate.allow_disk_use, Some(false));
        assert_eq!(aggregate.max_time, Some(Duration::from_millis(700)));
    }

    #[test]
    fn test_views_are_refused_and_unreadable_lists_fail_closed() {
        let collections = doc! {"cursor": {"firstBatch": [{"name": "a", "type": "collection"}, {"name": "t", "type": "timeseries"}]}, "ok": 1};
        check_no_views(&collections).expect("no view");
        let view = doc! {"cursor": {"firstBatch": [{"name": "a", "type": "collection"}, {"name": "v", "type": "view"}]}, "ok": 1};
        assert_eq!(
            check_no_views(&view).unwrap_err().code(),
            "mongo_view_refused"
        );
        for unreadable in [
            doc! {"ok": 1},
            doc! {"cursor": {"firstBatch": [{"name": "a"}]}},
            doc! {"cursor": {"firstBatch": ["a"]}},
        ] {
            assert_eq!(
                check_no_views(&unreadable).unwrap_err().code(),
                "mongo_refused"
            );
        }
    }

    #[test]
    fn test_code_names_are_shown_only_when_plain() {
        assert_eq!(code_name("TypeMismatch"), " TypeMismatch");
        assert_eq!(code_name("bad name"), "");
        assert_eq!(code_name("x\u{202e}"), "");
    }
}
