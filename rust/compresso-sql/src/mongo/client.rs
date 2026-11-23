//! MongoDB through the official driver (E17-52, D117).
//!
//! Every call builds a fresh client with options set field by field (never
//! from the URL text), runs the checks, runs one command the tool built, and
//! shuts the client down. Per call:
//!
//! 1. connect: one host, a direct connection, a pool of one, no retries, no
//!    compression, read concern `local`, read preference `primaryPreferred`
//!    (so a direct connection to a secondary member still reads), TLS per
//!    [`super::target::MongoTls`] (10 s for server selection and connect);
//! 2. [`preflight`]: `connectionStatus` with `showPrivileges`, checked by
//!    [`super::account::check`] (fail closed); `hostInfo` on `admin`, which
//!    must be refused as `Unauthorized` (the server enforces access
//!    control); and a names-only `listCollections` for every collection the
//!    call reads (its own and each `$lookup` source), refusing a view;
//! 3. the action, with `maxTimeMS` set to the call's time limit, a row
//!    limit one past `max_rows`, `allowDiskUse: false`, and batches of at
//!    most [`BATCH_DOCUMENTS`] documents. Replies are read as raw bytes
//!    ([`super::raw`]): no `Document` tree of a result document is built.
//!    Reading stops at the row cap, the 256 KiB result cap, or
//!    [`MAX_READ_BYTES`] of raw documents; `distinct` runs as a `$group`
//!    aggregation through the same path; `describe` stops at
//!    [`MAX_SAMPLE_BYTES`] and tracks at most [`MAX_TRACKED_PATHS`] paths.
//!    What remains: the driver holds one server reply in memory while it is
//!    read (at most 16 MiB of documents, a 48 MiB message at the protocol
//!    limit);
//! 4. an immediate shutdown, waited on for at most 2 s.
//!
//! The client deadline is the time limit plus a grace period; when it passes
//! the call is abandoned (the server's own limit stops the command). Errors
//! carry fixed text, the server's numeric code and code name, and the
//! server's own message; the service scrubs URL parts from it and then cuts
//! it, never the reverse. The driver's rendering of a reply is never used.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::pin::Pin;
use std::time::{Duration, Instant};

use bson::{Bson, Document, doc};
use futures_util::TryStreamExt;
use mongodb::Client;
use mongodb::error::ErrorKind;
use mongodb::options::{
    AggregateOptions, AuthMechanism, ClientOptions, Credential, FindOptions, ReadConcern,
    ReadPreference, SelectionCriteria, ServerAddress, ServerMonitoringMode, Tls, TlsOptions,
};
use serde_json::Value;
use tokio::runtime::Runtime;

use super::output::ValueCollector;
use super::raw;
use super::target::{Mechanism, MongoTarget, MongoTls};
use super::{
    CollectionEntry, CollectionsResult, CountResult, DESCRIBE_SAMPLE, DocumentsResult, FieldEntry,
    MAX_DESCRIBED_FIELDS, MongoDescribeResult, MongoIndexEntry, MongoOutcome, MongoRequest,
    Prepared, account, timeout,
};
use crate::{MAX_TABLES, SqlError, Truncation};

/// Time the client waits past the server-side time limit.
const CLIENT_GRACE: Duration = Duration::from_secs(2);
/// Longest wait for each check once connected.
const ACCOUNT_TIMEOUT: Duration = Duration::from_secs(5);
/// Longest wait for the client to shut down; a stalled peer is not awaited.
const FINISH_TIMEOUT: Duration = Duration::from_secs(2);
/// Raw document bytes one `find`, `aggregate`, or `distinct` reads at most.
/// The server sends at most 16 MiB of documents per reply, so this admits a
/// full reply and a maximum-size document besides; past it reading stops,
/// marked `max_bytes`.
pub(crate) const MAX_READ_BYTES: usize = 32 * 1024 * 1024;
/// Raw document bytes `describe` samples at most.
pub(crate) const MAX_SAMPLE_BYTES: usize = 8 * 1024 * 1024;
/// Distinct field paths `describe` tracks; a path first seen past it is
/// counted in `fields_omitted` instead.
pub(crate) const MAX_TRACKED_PATHS: usize = 4 * MAX_DESCRIBED_FIELDS;
/// Documents asked of the server per reply.
pub(crate) const BATCH_DOCUMENTS: u32 = 16;

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

/// `find` options: the row limit, small batches, skip, time limit, and no
/// disk use, plus the checked projection and sort.
pub(crate) fn find_options(
    max_rows: usize,
    skip: u64,
    timeout: Duration,
    projection: Option<Document>,
    sort: Option<Document>,
) -> FindOptions {
    let mut options = FindOptions::default();
    options.limit = Some(server_limit(max_rows));
    options.batch_size = Some(BATCH_DOCUMENTS);
    options.skip = Some(skip);
    options.max_time = Some(timeout);
    options.allow_disk_use = Some(false);
    options.projection = projection;
    options.sort = sort;
    options
}

/// `aggregate` options: time limit, small batches, and no disk use.
pub(crate) fn aggregate_options(timeout: Duration) -> AggregateOptions {
    let mut options = AggregateOptions::default();
    options.max_time = Some(timeout);
    options.allow_disk_use = Some(false);
    options.batch_size = Some(BATCH_DOCUMENTS);
    options
}

/// The pipeline `distinct` runs: documents that have `field` (an explicit
/// `null` included), its value as an array (a scalar becomes a one-element
/// array), unwound, grouped, and limited, unordered as `distinct` is. The
/// values come back as `_id` of raw documents through the same bounded path
/// as `find`. It matches the server's `distinct` for scalars, `null`, and an
/// array at the end of the path; it differs where the path crosses an array
/// of subdocuments (`b.c` over `[{c: 5}, {c: [6, 7]}]` gives `5` and
/// `[6, 7]`, where `distinct` gives 5, 6, 7) and for `undefined`, which
/// aggregation turns into `null` (ADR-0038).
pub(crate) fn distinct_pipeline(field: &str, filter: Document, max_rows: usize) -> Vec<Document> {
    let path = format!("${field}");
    let mut exists = Document::new();
    exists.insert(field, doc! {"$exists": true});
    vec![
        doc! {"$match": {"$and": [filter, exists]}},
        doc! {"$project": {"_id": 0, "value": {"$cond": [{"$isArray": path.clone()}, path.clone(), [path]]}}},
        doc! {"$unwind": "$value"},
        doc! {"$group": {"_id": "$value"}},
        doc! {"$limit": server_limit(max_rows)},
    ]
}

/// A command failure as the checks see it: a server error code, or an error
/// already mapped.
pub(crate) enum CommandFailure {
    Code(i32),
    Other(SqlError),
}

type CommandFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Document, CommandFailure>> + Send + 'a>>;

/// Runs one command on one database; the seam [`preflight`] is tested
/// through.
pub(crate) trait Commands: Sync {
    fn run<'a>(&'a self, database: &'a str, command: Document, wait: Duration)
    -> CommandFuture<'a>;
}

struct DriverCommands<'a> {
    client: &'a Client,
}

impl Commands for DriverCommands<'_> {
    fn run<'a>(
        &'a self,
        database: &'a str,
        command: Document,
        wait: Duration,
    ) -> CommandFuture<'a> {
        Box::pin(async move {
            match tokio::time::timeout(wait, self.client.database(database).run_command(command))
                .await
            {
                Err(_) => Err(CommandFailure::Other(SqlError::new(
                    "mongo_connection",
                    format!("the server did not answer within {} ms", wait.as_millis()),
                ))),
                Ok(Ok(reply)) => Ok(reply),
                Ok(Err(error)) => match error.kind.as_ref() {
                    ErrorKind::Command(command) => Err(CommandFailure::Code(command.code)),
                    _ => Err(CommandFailure::Other(map_error(error))),
                },
            }
        })
    }
}

fn command_error(failure: CommandFailure) -> SqlError {
    match failure {
        CommandFailure::Code(code) => SqlError::new(
            "mongo_error",
            format!("MongoDB refused a check command (code {code})."),
        ),
        CommandFailure::Other(error) => error,
    }
}

/// The checks before any action: the account, access control, and views.
pub(crate) async fn preflight(
    commands: &dyn Commands,
    database: &str,
    prepared: &Prepared,
    connect_timeout: Duration,
) -> Result<(), SqlError> {
    let status = commands
        .run(
            database,
            doc! {"connectionStatus": 1, "showPrivileges": true},
            connect_timeout + ACCOUNT_TIMEOUT,
        )
        .await
        .map_err(command_error)?;
    account::check(&status, database)
        .map_err(|rule| SqlError::new("mongo_account_refused", rule.message()))?;
    let probe = match commands
        .run(
            "admin",
            doc! {account::ENFORCEMENT_PROBE: 1},
            ACCOUNT_TIMEOUT,
        )
        .await
    {
        Ok(_) => Ok(()),
        Err(CommandFailure::Code(code)) => Err(Some(code)),
        Err(CommandFailure::Other(error)) => return Err(error),
    };
    account::check_enforcement(probe)
        .map_err(|rule| SqlError::new("mongo_account_refused", rule.message()))?;
    let read = prepared.collections_read();
    if !read.is_empty() {
        let reply = commands
            .run(
                database,
                doc! {
                    "listCollections": 1,
                    "filter": {"name": {"$in": read}},
                    "nameOnly": true,
                    "authorizedCollections": true,
                },
                ACCOUNT_TIMEOUT,
            )
            .await
            .map_err(command_error)?;
        check_no_views(&reply)?;
    }
    Ok(())
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
        let result = async {
            preflight(
                &DriverCommands { client: &client },
                &self.target.database,
                prepared,
                self.connect_timeout,
            )
            .await?;
            let database = client.database(&self.target.database);
            tokio::time::timeout(
                request.timeout + CLIENT_GRACE,
                action(&database, request, prepared),
            )
            .await
            .unwrap_or_else(|_| Err(timeout(request.timeout)))
        }
        .await;
        let _ = tokio::time::timeout(FINISH_TIMEOUT, client.shutdown().immediate(true)).await;
        result
    }
}

/// Refuses the call if the `listCollections` reply names a view; an
/// unreadable reply refuses too. A time-series collection is allowed: the
/// server unpacks its buckets itself, with no pipeline anyone stored.
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

/// Reads raw result documents into a bounded result: the read budget first,
/// then the per-document render, then the row and byte caps.
pub(crate) struct DocumentReader {
    collector: ValueCollector,
    budget: ReadBudget,
    over_budget: bool,
    /// `distinct`: show only this top-level field of each document.
    field: Option<&'static [u8]>,
}

impl DocumentReader {
    pub(crate) fn new(max_rows: usize, field: Option<&'static [u8]>) -> Self {
        Self {
            collector: ValueCollector::new(max_rows),
            budget: ReadBudget::new(MAX_READ_BYTES),
            over_budget: false,
            field,
        }
    }

    /// Adds one raw document; `false` once no more are wanted.
    pub(crate) fn push(&mut self, document: &[u8]) -> bool {
        if !self.budget.admit(document.len()) {
            self.over_budget = true;
            return false;
        }
        // Render within what the result may still hold (three quarters of
        // it: the output estimate is approximate), so a large document is cut
        // and marked `$truncated` instead of dropped.
        let limit = self.collector.remaining() / 4 * 3;
        let value = match self.field {
            Some(field) => raw::render_field(document, field, limit).unwrap_or(Value::Null),
            None => raw::render_document(document, limit),
        };
        self.collector.push(value)
    }

    pub(crate) fn finish(self, started: Instant) -> DocumentsResult {
        let (documents, mut truncation) = self.collector.finish();
        if self.over_budget && truncation.is_none() {
            truncation = Some(Truncation::MaxBytes);
        }
        DocumentsResult {
            count: documents.len(),
            documents,
            truncated: truncation.is_some(),
            truncation_reason: truncation,
            elapsed_ms: elapsed_ms(started),
        }
    }
}

/// Reads one raw-batch cursor into `reader` until it is done or full.
async fn read_batches(
    mut cursor: mongodb::raw_batch_cursor::RawBatchCursor,
    reader: &mut DocumentReader,
) -> Result<(), SqlError> {
    while let Some(batch) = cursor.try_next().await.map_err(map_error)? {
        let documents = raw::batch_documents(batch.as_raw_document().as_bytes())
            .map_err(|_| SqlError::new("mongo_error", "a server reply could not be read"))?;
        for document in documents {
            if !reader.push(document) {
                return Ok(());
            }
        }
    }
    Ok(())
}

/// Reads sampled raw documents into a field summary, under the sample byte
/// budget and the sample size.
pub(crate) struct SampleReader {
    summary: FieldSummary,
    budget: ReadBudget,
    sampled: usize,
}

impl SampleReader {
    pub(crate) fn new() -> Self {
        Self {
            summary: FieldSummary::default(),
            budget: ReadBudget::new(MAX_SAMPLE_BYTES),
            sampled: 0,
        }
    }

    /// Adds one raw document; `false` once no more are wanted.
    pub(crate) fn push(&mut self, document: &[u8]) -> bool {
        if self.sampled == DESCRIBE_SAMPLE || !self.budget.admit(document.len()) {
            return false;
        }
        self.sampled += 1;
        self.summary.add(document);
        self.sampled < DESCRIBE_SAMPLE
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
                .collection::<Document>(&collection)
                .find(filter)
                .with_options(find_options(
                    max_rows,
                    skip,
                    request.timeout,
                    projection,
                    sort,
                ))
                .batch()
                .await
                .map_err(map_error)?;
            let mut reader = DocumentReader::new(max_rows, None);
            read_batches(cursor, &mut reader).await?;
            Ok(MongoOutcome::Documents(reader.finish(started)))
        }
        Prepared::Aggregate {
            collection,
            mut pipeline,
        } => {
            pipeline.push(doc! {"$limit": server_limit(max_rows)});
            let cursor = database
                .collection::<Document>(&collection)
                .aggregate(pipeline)
                .with_options(aggregate_options(request.timeout))
                .batch()
                .await
                .map_err(map_error)?;
            let mut reader = DocumentReader::new(max_rows, None);
            read_batches(cursor, &mut reader).await?;
            Ok(MongoOutcome::Documents(reader.finish(started)))
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
            let cursor = database
                .collection::<Document>(&collection)
                .aggregate(distinct_pipeline(&field, filter, max_rows))
                .with_options(aggregate_options(request.timeout))
                .batch()
                .await
                .map_err(map_error)?;
            let mut reader = DocumentReader::new(max_rows, Some(b"_id"));
            read_batches(cursor, &mut reader).await?;
            Ok(MongoOutcome::Documents(reader.finish(started)))
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
                .with_options(aggregate_options(request.timeout))
                .batch()
                .await
                .map_err(map_error)?;
            let mut reader = SampleReader::new();
            'batches: while let Some(batch) = cursor.try_next().await.map_err(map_error)? {
                let documents =
                    raw::batch_documents(batch.as_raw_document().as_bytes()).map_err(|_| {
                        SqlError::new("mongo_error", "a server reply could not be read")
                    })?;
                for document in documents {
                    if !reader.push(document) {
                        break 'batches;
                    }
                }
            }
            drop(cursor);
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
            let sampled = reader.sampled;
            let (fields, fields_omitted) = reader.summary.finish();
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

/// Field paths (nested documents to three levels) and the types seen, read
/// from raw documents. At most [`MAX_TRACKED_PATHS`] distinct paths are
/// kept; a path first seen after that is counted in `untracked` and never
/// stored, so memory does not grow with the number of keys in the sample.
#[derive(Default)]
pub(crate) struct FieldSummary {
    fields: BTreeMap<String, (BTreeSet<&'static str>, usize)>,
    untracked: usize,
}

impl FieldSummary {
    pub(crate) fn add(&mut self, document: &[u8]) {
        self.walk(document, "", 0);
    }

    fn walk(&mut self, document: &[u8], prefix: &str, depth: usize) {
        let Ok(elements) = raw::elements(document) else {
            return;
        };
        for element in elements.flatten() {
            let name = raw::key_name(element.key);
            let path = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}.{name}")
            };
            if !self.fields.contains_key(&path) && self.fields.len() >= MAX_TRACKED_PATHS {
                self.untracked += 1;
                continue;
            }
            let entry = self.fields.entry(path.clone()).or_default();
            entry.0.insert(raw::type_name(element.kind));
            entry.1 += 1;
            if element.kind == 0x03 && depth < 2 {
                self.walk(element.value, &path, depth + 1);
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

/// Server error codes the tool names.
const MAX_TIME_MS_EXPIRED: i32 = 50;
const UNAUTHORIZED: i32 = 13;

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
/// server's numeric code, its code name, and its message; the service then
/// scrubs URL parts, escapes hidden characters, and cuts the length, in that
/// order (a cut before the scrub could leave part of a secret).
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
                command.message
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
    use std::sync::Mutex;

    fn raw(document: &Document) -> Vec<u8> {
        document.to_vec().expect("raw")
    }

    /// A document of about `bytes` raw bytes that renders to a few bytes.
    fn binary_document(bytes: usize) -> Vec<u8> {
        raw(
            &doc! {"b": bson::Binary { subtype: bson::spec::BinarySubtype::Generic, bytes: vec![0; bytes] }},
        )
    }

    #[test]
    fn test_field_summary_counts_paths_and_types() {
        let mut summary = FieldSummary::default();
        summary.add(&raw(&doc! {"a": 1, "b": {"c": "x", "d": {"e": {"f": 1}}}}));
        summary.add(&raw(&doc! {"a": "one", "$oid": 1}));
        let (fields, omitted) = summary.finish();
        assert_eq!(omitted, 0);
        let a = fields.iter().find(|field| field.path == "a").unwrap();
        assert_eq!(a.types, vec!["int", "string"]);
        assert_eq!(a.present, 2);
        assert!(fields.iter().any(|field| field.path == "b.d.e"));
        assert!(!fields.iter().any(|field| field.path == "b.d.e.f"));
        assert!(fields.iter().any(|field| field.path == "\\$oid"));
    }

    /// The review's case, scaled: memory stays at the tracked-path bound,
    /// which is 800 (a literal, so raising the constant fails this test).
    #[test]
    fn test_field_summary_memory_is_bounded_by_tracked_paths() {
        let mut summary = FieldSummary::default();
        for document in 0..100 {
            let mut wide = Document::new();
            for key in 0..10_000 {
                wide.insert(format!("d{document}k{key}"), 1_i32);
            }
            summary.add(&raw(&wide));
            assert!(summary.tracked() <= 800);
        }
        assert_eq!(summary.tracked(), 800);
        let (fields, omitted) = summary.finish();
        assert_eq!(fields.len(), 200);
        assert_eq!(omitted, 100 * 10_000 - 200);
    }

    /// 70 documents of 512 KiB each: the reader stops at the 32 MiB read
    /// budget (64 documents), marked max_bytes.
    #[test]
    fn test_the_document_reader_stops_at_the_read_budget() {
        let document = binary_document(512 * 1024 - 64);
        let size = document.len();
        let mut reader = DocumentReader::new(1_000, None);
        let mut read = 0;
        for _ in 0..70 {
            if !reader.push(&document) {
                break;
            }
            read += 1;
        }
        assert_eq!(read, (32 * 1024 * 1024) / size);
        let result = reader.finish(Instant::now());
        assert_eq!(result.count, read);
        assert_eq!(result.truncation_reason, Some(Truncation::MaxBytes));
    }

    /// A document that renders larger than the result can hold is cut and
    /// marked, not dropped, and the result stays within its byte cap.
    #[test]
    fn test_a_large_document_is_cut_into_the_result_not_dropped() {
        let mut wide = Document::new();
        for index in 0..9 {
            let items = (0..100)
                .map(|_| Bson::String("y".repeat(1_000)))
                .collect::<Vec<_>>();
            wide.insert(format!("k{index}"), items);
        }
        let document = raw(&wide);
        assert!(document.len() <= raw::MAX_DOCUMENT_BYTES);
        let mut reader = DocumentReader::new(10, None);
        assert!(reader.push(&document));
        reader.push(&document);
        let result = reader.finish(Instant::now());
        assert!(result.count >= 1);
        assert_eq!(result.documents[0]["$truncated"], true);
        assert!(serde_json::to_string(&result.documents).unwrap().len() <= crate::MAX_RESULT_BYTES);
    }

    /// The sample reader stops at 8 MiB and at 100 documents.
    #[test]
    fn test_the_sample_reader_stops_at_its_byte_budget_and_size() {
        let document = binary_document(512 * 1024 - 64);
        let mut reader = SampleReader::new();
        while reader.push(&document) {}
        assert_eq!(reader.sampled, (8 * 1024 * 1024) / document.len());
        let small = raw(&doc! {"a": 1});
        let mut reader = SampleReader::new();
        while reader.push(&small) {}
        assert_eq!(reader.sampled, 100);
    }

    #[test]
    fn test_distinct_values_render_raw_and_stored_wrappers_stay_escaped() {
        let mut reader = DocumentReader::new(10, Some(b"_id"));
        for value in [
            doc! {"_id": {"$oid": "65a1b2c3d4e5f60718293a4b"}},
            doc! {"_id": [{"$date": "2020-01-01T00:00:00Z"}]},
            doc! {"_id": {"x": {"$numberLong": "5"}}},
            doc! {"_id": bson::oid::ObjectId::parse_str("65a1b2c3d4e5f60718293a4b").unwrap()},
        ] {
            assert!(reader.push(&raw(&value)));
        }
        let result = reader.finish(Instant::now());
        assert_eq!(
            result.documents[0],
            serde_json::json!({"\\$oid": "65a1b2c3d4e5f60718293a4b"})
        );
        assert_eq!(
            result.documents[1],
            serde_json::json!([{"\\$date": "2020-01-01T00:00:00Z"}])
        );
        assert_eq!(
            result.documents[2],
            serde_json::json!({"x": {"\\$numberLong": "5"}})
        );
        assert_eq!(
            result.documents[3],
            serde_json::json!({"$oid": "65a1b2c3d4e5f60718293a4b"})
        );
        let pipeline = distinct_pipeline("a.b", doc! {"k": 1}, 10);
        assert_eq!(pipeline[2], doc! {"$unwind": "$value"});
        assert_eq!(pipeline[4], doc! {"$limit": 11_i64});
    }

    #[test]
    fn test_the_call_limit_covers_every_phase() {
        let limit = call_limit(Duration::from_secs(10), Duration::from_secs(5));
        assert_eq!(limit, Duration::from_secs(10 + 5 + 5 + 5 + 2 + 2));
        assert_eq!(server_limit(100), 101);
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
        let Some(Tls::Enabled(tls)) = &options.tls else {
            panic!("tls")
        };
        assert_eq!(tls.allow_invalid_certificates, Some(false));
        let credential = options.credential.as_ref().expect("credential");
        assert_eq!(credential.source.as_deref(), Some("app"));

        let find = find_options(10, 3, Duration::from_millis(700), None, None);
        assert_eq!(find.allow_disk_use, Some(false));
        assert_eq!(find.max_time, Some(Duration::from_millis(700)));
        assert_eq!(find.limit, Some(11));
        assert_eq!(find.skip, Some(3));
        assert_eq!(find.batch_size, Some(16));
        let aggregate = aggregate_options(Duration::from_millis(700));
        assert_eq!(aggregate.allow_disk_use, Some(false));
        assert_eq!(aggregate.max_time, Some(Duration::from_millis(700)));
        assert_eq!(aggregate.batch_size, Some(16));
    }

    /// Canned replies for the preflight checks.
    struct Fake {
        enforced: bool,
        listing: Document,
        seen: Mutex<Vec<String>>,
    }

    impl Commands for Fake {
        fn run<'a>(
            &'a self,
            database: &'a str,
            command: Document,
            _wait: Duration,
        ) -> CommandFuture<'a> {
            let name = command.keys().next().cloned().unwrap_or_default();
            self.seen.lock().unwrap().push(format!("{database}.{name}"));
            Box::pin(async move {
                match name.as_str() {
                    "connectionStatus" => Ok(doc! {"authInfo": {
                        "authenticatedUsers": [{"user": "r", "db": "probe"}],
                        "authenticatedUserRoles": [{"role": "read", "db": "probe"}],
                        "authenticatedUserPrivileges": [{"resource": {"db": "probe", "collection": ""}, "actions": ["find"]}],
                    }, "ok": 1}),
                    "hostInfo" if self.enforced => Err(CommandFailure::Code(13)),
                    "hostInfo" => Ok(doc! {"ok": 1}),
                    "listCollections" => Ok(self.listing.clone()),
                    _ => Err(CommandFailure::Code(59)),
                }
            })
        }
    }

    fn find_prepared(collection: &str) -> Prepared {
        Prepared::Find {
            collection: collection.to_owned(),
            filter: doc! {},
            projection: None,
            sort: None,
            skip: 0,
        }
    }

    #[test]
    fn test_preflight_refuses_views_and_servers_without_access_control() {
        let runtime = runtime().expect("runtime");
        let view = doc! {"cursor": {"firstBatch": [{"name": "v", "type": "view"}]}, "ok": 1};
        let plain = doc! {"cursor": {"firstBatch": [{"name": "c", "type": "collection"}]}, "ok": 1};
        let fake = Fake {
            enforced: true,
            listing: view,
            seen: Mutex::default(),
        };
        let error = runtime
            .block_on(preflight(
                &fake,
                "probe",
                &find_prepared("v"),
                Duration::from_secs(1),
            ))
            .expect_err("view refused");
        assert_eq!(error.code(), "mongo_view_refused");
        assert_eq!(
            *fake.seen.lock().unwrap(),
            [
                "probe.connectionStatus",
                "admin.hostInfo",
                "probe.listCollections"
            ]
        );
        let fake = Fake {
            enforced: true,
            listing: plain.clone(),
            seen: Mutex::default(),
        };
        runtime
            .block_on(preflight(
                &fake,
                "probe",
                &find_prepared("c"),
                Duration::from_secs(1),
            ))
            .expect("plain collection");
        let fake = Fake {
            enforced: false,
            listing: plain,
            seen: Mutex::default(),
        };
        let error = runtime
            .block_on(preflight(
                &fake,
                "probe",
                &find_prepared("c"),
                Duration::from_secs(1),
            ))
            .expect_err("no access control");
        assert_eq!(error.code(), "mongo_account_refused");
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
