use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, mpsc};
use std::thread;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use compresso_core::TokenCounter;
use compresso_store::{EdgeKind, EdgeRecord, EdgeResolution, FileRecord, Store, SymbolRecord};

use crate::concurrency::{Concurrency, ReadAhead};
use crate::parsers::{ImportRecord, ImportResolution, ParseError, ParserRegistry};
use crate::scanner::{AtlasError, DEFAULT_MAX_FILE_BYTES, ScanOptions, scan_with_concurrency};

const INDEX_SCHEMA_VERSION: u32 = 1;
const DEFAULT_PARSER_VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), "+atlas-shell-text-v1");
/// Below this many candidates worker startup costs more than it saves.
const PARALLEL_INDEX_MIN_CANDIDATES: usize = 8;
/// Prepared files a worker may run ahead of the ordered commit.
const READ_AHEAD_PER_WORKER: usize = 4;

/// A deterministic progress event emitted only after its corresponding action starts or commits.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum IndexProgress {
    Parsing { path: PathBuf },
    Removed { path: String },
}

/// Work completed by one incremental index pass.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IndexReport {
    pub files_indexed: usize,
    pub files_removed: usize,
    pub files_skipped: usize,
    pub symbols: usize,
    pub imports_resolved: usize,
    pub imports_unresolved: usize,
    pub duration_ms: u64,
    pub stale: bool,
}

/// Typed indexing failures that leave the current file at its previous complete state.
#[derive(Debug, thiserror::Error)]
pub enum IndexError {
    #[error("index parser version must be non-empty and contain no NUL byte")]
    InvalidParserVersion,
    #[error("failed to canonicalize index root {path:?}")]
    CanonicalizeRoot {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to canonicalize index candidate {path:?}")]
    CanonicalizeFile {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("index candidate changed type during the scan: {path:?}")]
    EntryChanged { path: PathBuf },
    #[error("index candidate path is not valid UTF-8: {path:?}")]
    NonUtf8Path { path: PathBuf },
    #[error("failed to read index candidate {path:?}")]
    ReadSource {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to parse index candidate {path:?}")]
    Parse {
        path: PathBuf,
        #[source]
        source: ParseError,
    },
    #[error("parser returned an invalid line range for {path:?}: {start_line}..={end_line}")]
    InvalidSymbolRange {
        path: PathBuf,
        start_line: u32,
        end_line: u32,
    },
    #[error("system clock predates the Unix epoch")]
    InvalidSystemTime(#[from] std::time::SystemTimeError),
    #[error("Unix timestamp exceeds SQLite INTEGER range")]
    TimestampOutOfRange,
    #[cfg(test)]
    #[error("injected indexing failure before {path:?}")]
    InjectedFailure { path: PathBuf },
}

/// Incremental repository indexer over the local Store, parser registry, and token counter.
pub struct Indexer<'a> {
    store: &'a Store,
    counter: &'a dyn TokenCounter,
    parser_version: String,
    schema_version: u32,
    concurrency: Concurrency,
}

impl<'a> Indexer<'a> {
    pub fn new(store: &'a Store, counter: &'a dyn TokenCounter) -> Self {
        Self {
            store,
            counter,
            parser_version: DEFAULT_PARSER_VERSION.to_owned(),
            schema_version: INDEX_SCHEMA_VERSION,
            concurrency: Concurrency::default(),
        }
    }

    /// Bounds the workers and buffered source bytes used to scan, read, and parse.
    ///
    /// Store writes always stay on the calling thread in candidate order, so
    /// the resulting index is identical for every setting.
    pub fn with_concurrency(mut self, concurrency: Concurrency) -> Self {
        self.concurrency = concurrency;
        self
    }

    /// Overrides the parser version used for staleness checks and version stamps.
    pub fn with_parser_version(mut self, parser_version: impl Into<String>) -> Self {
        self.parser_version = parser_version.into();
        self
    }

    pub fn index(
        &self,
        root: &Path,
        progress: impl FnMut(IndexProgress),
    ) -> Result<IndexReport, AtlasError> {
        self.index_with_hook(root, progress, |_, _| Ok(()))
    }

    fn index_with_hook<P, H>(
        &self,
        root: &Path,
        mut progress: P,
        mut before_file: H,
    ) -> Result<IndexReport, AtlasError>
    where
        P: FnMut(IndexProgress),
        H: FnMut(usize, &Path) -> Result<(), IndexError>,
    {
        let started = Instant::now();
        if self.parser_version.is_empty() || self.parser_version.contains('\0') {
            return Err(IndexError::InvalidParserVersion.into());
        }
        let canonical_root =
            fs::canonicalize(root).map_err(|source| IndexError::CanonicalizeRoot {
                path: root.to_path_buf(),
                source,
            })?;
        let repo = self.store.approve_repo(&canonical_root)?;
        let previous_hashes = self.store.file_hashes(repo)?;
        let previous_version = self.store.latest_index_version(repo)?;
        let stale = previous_version.as_ref().is_some_and(|version| {
            version.parser_version != self.parser_version
                || version.schema_version != self.schema_version
        });
        let scan = scan_with_concurrency(
            self.store,
            repo,
            &canonical_root,
            ScanOptions::default(),
            self.concurrency,
        )?;
        let removed_set = scan.removed.iter().cloned().collect::<HashSet<_>>();
        let known_paths = known_repository_paths(&previous_hashes, &scan, &removed_set)?;
        let paths_expanded = !scan.added.is_empty();
        let mut candidates = scan
            .added
            .into_iter()
            .chain(scan.changed)
            .collect::<BTreeSet<_>>();
        if paths_expanded {
            candidates.extend(
                self.store
                    .unresolved_edge_sources(repo)?
                    .into_iter()
                    .filter(|path| !removed_set.contains(path))
                    .map(PathBuf::from),
            );
        }
        if !removed_set.is_empty() {
            let mut affected_sources = self.store.unresolved_edge_sources(repo)?;
            affected_sources.extend(
                self.store
                    .resolved_edges(repo)?
                    .into_iter()
                    .filter(|edge| removed_set.contains(&edge.to_path))
                    .map(|edge| edge.from_path),
            );
            candidates.extend(
                affected_sources
                    .into_iter()
                    .filter(|path| !removed_set.contains(path))
                    .map(PathBuf::from),
            );
        }
        if stale {
            candidates.extend(
                previous_hashes
                    .keys()
                    .filter(|path| !removed_set.contains(*path))
                    .map(PathBuf::from),
            );
        }

        let indexed_at = unix_timestamp()?;
        let mut files_indexed = 0_usize;
        let mut files_skipped = scan.skipped.len();
        let mut symbols = 0_usize;
        let mut imports_resolved = 0_usize;
        let mut imports_unresolved = 0_usize;

        let candidates = candidates.into_iter().collect::<Vec<_>>();
        let context = PrepareContext {
            root: &canonical_root,
            known_paths: &known_paths,
            counter: self.counter,
            indexed_at,
        };
        // Every Store write happens here, on the calling thread, in candidate
        // order: one writer, one transaction per file, at any concurrency.
        let mut commit = |relative: &Path,
                          outcome: Result<PreparedFile, PrepareFailure>|
         -> Result<(), AtlasError> {
            let prepared = match outcome {
                Ok(prepared) => prepared,
                Err(failure) => {
                    if failure.parsing_started {
                        progress(IndexProgress::Parsing {
                            path: relative.to_path_buf(),
                        });
                    }
                    return Err(failure.error);
                }
            };
            if prepared.parsed {
                progress(IndexProgress::Parsing {
                    path: relative.to_path_buf(),
                });
                files_indexed += 1;
                symbols += prepared.symbols.len();
                imports_resolved += prepared.imports_resolved;
                imports_unresolved += prepared.imports_unresolved;
            } else {
                files_skipped += 1;
            }
            self.store.replace_indexed_file(
                repo,
                &prepared.record,
                &prepared.symbols,
                &prepared.edges,
            )?;
            Ok(())
        };

        let workers = if candidates.len() < PARALLEL_INDEX_MIN_CANDIDATES {
            1
        } else {
            self.concurrency
                .workers_for(candidates.len(), DEFAULT_MAX_FILE_BYTES)
        };
        if workers <= 1 {
            for (index, relative) in candidates.iter().enumerate() {
                before_file(index, relative)?;
                commit(relative, prepare_file(&context, relative))?;
            }
        } else {
            prepare_in_parallel(
                &context,
                &candidates,
                workers,
                self.concurrency.memory_budget_bytes(),
                |index, outcome| {
                    before_file(index, &candidates[index])?;
                    commit(&candidates[index], outcome)
                },
            )?;
        }

        self.store.delete_files(repo, &scan.removed)?;
        for path in &scan.removed {
            progress(IndexProgress::Removed { path: path.clone() });
        }
        self.store
            .stamp_index_version(repo, &self.parser_version, self.schema_version)?;

        Ok(IndexReport {
            files_indexed,
            files_removed: scan.removed.len(),
            files_skipped,
            symbols,
            imports_resolved,
            imports_unresolved,
            duration_ms: elapsed_millis(started),
            stale,
        })
    }
}

/// Read-only inputs shared by every file preparation, on any thread.
struct PrepareContext<'a> {
    root: &'a Path,
    known_paths: &'a BTreeSet<String>,
    counter: &'a dyn TokenCounter,
    indexed_at: i64,
}

/// Everything one file contributes to the index, computed without the Store.
struct PreparedFile {
    record: FileRecord,
    symbols: Vec<SymbolRecord>,
    edges: Vec<EdgeRecord>,
    parsed: bool,
    imports_resolved: usize,
    imports_unresolved: usize,
}

/// A preparation failure plus whether parsing had begun, so the committing
/// thread can replay the same progress events a serial run would emit.
struct PrepareFailure {
    error: AtlasError,
    parsing_started: bool,
}

impl PrepareFailure {
    fn before_parse(error: impl Into<AtlasError>) -> Self {
        Self {
            error: error.into(),
            parsing_started: false,
        }
    }

    fn during_parse(error: impl Into<AtlasError>) -> Self {
        Self {
            error: error.into(),
            parsing_started: true,
        }
    }
}

/// Reads, hashes, parses, and resolves one candidate. Pure with respect to
/// the Store, so it is safe to run on a worker thread.
fn prepare_file(
    context: &PrepareContext<'_>,
    relative: &Path,
) -> Result<PreparedFile, PrepareFailure> {
    let relative_text = relative
        .to_str()
        .ok_or_else(|| {
            PrepareFailure::before_parse(IndexError::NonUtf8Path {
                path: relative.to_path_buf(),
            })
        })?
        .to_owned();
    let absolute =
        resolve_candidate(context.root, relative).map_err(PrepareFailure::before_parse)?;
    let source = fs::read_to_string(&absolute).map_err(|source| {
        PrepareFailure::before_parse(IndexError::ReadSource {
            path: absolute.clone(),
            source,
        })
    })?;
    let hash = blake3::hash(source.as_bytes()).to_hex().to_string();
    let language = ParserRegistry::for_path(relative);
    let mut symbols = Vec::new();
    let mut edges = Vec::new();
    let mut imports_resolved = 0_usize;
    let mut imports_unresolved = 0_usize;

    if let Some(language) = language {
        let mut extraction = ParserRegistry::extract(language, &source).map_err(|source| {
            PrepareFailure::during_parse(IndexError::Parse {
                path: relative.to_path_buf(),
                source,
            })
        })?;
        resolve_repository_imports(&relative_text, &mut extraction.imports, context.known_paths);
        symbols = extraction.symbols;
        estimate_symbol_tokens(&source, relative, &mut symbols, context.counter)
            .map_err(PrepareFailure::during_parse)?;
        for import in extraction.imports {
            match &import.resolution {
                ImportResolution::Resolved => imports_resolved += 1,
                ImportResolution::Ambiguous(_) | ImportResolution::Unresolved(_) => {
                    imports_unresolved += 1;
                }
            }
            edges.push(edge_from_import(import));
        }
    }

    Ok(PreparedFile {
        record: FileRecord {
            path: relative_text,
            hash,
            language,
            size_bytes: source.len() as u64,
            token_estimate: context.counter.count(&source),
            indexed_at: context.indexed_at,
        },
        symbols,
        edges,
        parsed: language.is_some(),
        imports_resolved,
        imports_unresolved,
    })
}

/// Prepares candidates on `workers` threads and commits them in candidate order.
///
/// `commit` runs only on the calling thread, strictly in index order, and the
/// first failure it returns stops the run with nothing later committed. Source
/// bytes are charged against `memory_budget_bytes` from dispatch until commit.
fn prepare_in_parallel<C>(
    context: &PrepareContext<'_>,
    candidates: &[PathBuf],
    workers: usize,
    memory_budget_bytes: u64,
    mut commit: C,
) -> Result<(), AtlasError>
where
    C: FnMut(usize, Result<PreparedFile, PrepareFailure>) -> Result<(), AtlasError>,
{
    let (job_sender, job_receiver) = mpsc::channel::<usize>();
    let job_receiver = Mutex::new(job_receiver);
    let (result_sender, result_receiver) = mpsc::channel();
    let cancelled = AtomicBool::new(false);

    thread::scope(|scope| {
        for _ in 0..workers {
            let result_sender = result_sender.clone();
            let job_receiver = &job_receiver;
            let cancelled = &cancelled;
            scope.spawn(move || {
                loop {
                    let job = match job_receiver.lock() {
                        Ok(receiver) => receiver.recv(),
                        Err(_) => break,
                    };
                    let Ok(index) = job else { break };
                    if cancelled.load(Ordering::Relaxed) {
                        break;
                    }
                    let outcome = prepare_file(context, &candidates[index]);
                    if result_sender.send((index, outcome)).is_err() {
                        break;
                    }
                }
            });
        }
        drop(result_sender);

        let mut read_ahead = ReadAhead::new(
            memory_budget_bytes,
            workers.saturating_mul(READ_AHEAD_PER_WORKER),
        );
        let mut ready = BTreeMap::new();
        let mut next_dispatch = 0_usize;
        let mut next_commit = 0_usize;
        let outcome = (|| {
            while next_commit < candidates.len() {
                while next_dispatch < candidates.len() {
                    let cost_bytes = fs::metadata(context.root.join(&candidates[next_dispatch]))
                        .map_or(0, |metadata| metadata.len());
                    if !read_ahead.try_admit(cost_bytes) {
                        break;
                    }
                    if job_sender.send(next_dispatch).is_err() {
                        read_ahead.release_oldest();
                        break;
                    }
                    next_dispatch += 1;
                }
                let Ok((index, prepared)) = result_receiver.recv() else {
                    // Every worker stopped early. Finish on this thread so the
                    // run still completes with the serial result.
                    for (index, candidate) in candidates.iter().enumerate().skip(next_commit) {
                        let prepared = ready
                            .remove(&index)
                            .unwrap_or_else(|| prepare_file(context, candidate));
                        commit(index, prepared)?;
                    }
                    return Ok(());
                };
                ready.insert(index, prepared);
                while let Some(prepared) = ready.remove(&next_commit) {
                    commit(next_commit, prepared)?;
                    read_ahead.release_oldest();
                    next_commit += 1;
                }
            }
            Ok(())
        })();
        cancelled.store(true, Ordering::Relaxed);
        drop(job_sender);
        outcome
    })
}

fn known_repository_paths(
    previous_hashes: &std::collections::HashMap<String, String>,
    scan: &crate::scanner::ScanResult,
    removed: &HashSet<String>,
) -> Result<BTreeSet<String>, IndexError> {
    let mut paths = previous_hashes
        .keys()
        .filter(|path| !removed.contains(*path))
        .cloned()
        .collect::<BTreeSet<_>>();
    for path in scan.added.iter().chain(&scan.changed) {
        let text = path
            .to_str()
            .ok_or_else(|| IndexError::NonUtf8Path { path: path.clone() })?;
        paths.insert(text.to_owned());
    }
    Ok(paths)
}

fn resolve_repository_imports(
    importer: &str,
    imports: &mut [ImportRecord],
    known_paths: &BTreeSet<String>,
) {
    for import in imports {
        let candidates = match importer.rsplit_once('.').map(|(_, extension)| extension) {
            Some("rs") => rust_candidates(importer, &import.raw),
            Some("py") => python_candidates(importer, &import.raw),
            Some("ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs") => {
                script_candidates(importer, &import.raw)
            }
            _ => BTreeSet::new(),
        };
        let matches = candidates
            .into_iter()
            .filter(|candidate| known_paths.contains(candidate))
            .collect::<Vec<_>>();
        match matches.as_slice() {
            [path] => {
                import.resolved_path = Some(path.clone());
                import.resolution = ImportResolution::Resolved;
            }
            [] => {
                import.resolved_path = None;
                import.resolution = ImportResolution::Unresolved("not_indexed".to_owned());
            }
            _ => {
                import.resolved_path = None;
                import.resolution = ImportResolution::Ambiguous(matches);
            }
        }
    }
}

fn script_candidates(importer: &str, raw: &str) -> BTreeSet<String> {
    if !raw.starts_with('.') {
        return BTreeSet::new();
    }
    let Some(stem) = join_repo_path(parent_path(importer), raw) else {
        return BTreeSet::new();
    };
    extension_candidates(&stem, &["ts", "tsx", "js", "jsx", "mjs", "cjs"], "index")
}

fn rust_candidates(importer: &str, raw: &str) -> BTreeSet<String> {
    let mut parts = raw
        .split("::")
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    let importer_dir = parent_path(importer);
    let source_root = rust_source_root(importer);
    let mut bases = BTreeSet::new();
    match parts.first().copied() {
        Some("crate") => {
            parts.remove(0);
            bases.insert(source_root);
        }
        Some("self") => {
            parts.remove(0);
            bases.insert(importer_dir.to_owned());
        }
        Some("super") => {
            let mut base = importer_dir.to_owned();
            while parts.first() == Some(&"super") {
                parts.remove(0);
                base = parent_path(&base).to_owned();
            }
            bases.insert(base);
        }
        Some(_) => {
            bases.insert(importer_dir.to_owned());
            bases.insert(source_root);
        }
        None => return BTreeSet::new(),
    }
    if parts.last() == Some(&"self") {
        parts.pop();
    }

    let mut candidates = BTreeSet::new();
    for base in bases {
        for length in (1..=parts.len()).rev() {
            let module = parts[..length].join("/");
            let Some(stem) = join_repo_path(&base, &module) else {
                continue;
            };
            candidates.insert(format!("{stem}.rs"));
            candidates.insert(format!("{stem}/mod.rs"));
        }
    }
    candidates
}

fn python_candidates(importer: &str, raw: &str) -> BTreeSet<String> {
    let leading_dots = raw
        .chars()
        .take_while(|character| *character == '.')
        .count();
    let module = &raw[leading_dots..];
    if module.is_empty() {
        return BTreeSet::new();
    }
    let mut bases = BTreeSet::new();
    if leading_dots > 0 {
        let mut base = parent_path(importer).to_owned();
        for _ in 1..leading_dots {
            base = parent_path(&base).to_owned();
        }
        bases.insert(base);
    } else {
        let mut base = parent_path(importer).to_owned();
        loop {
            bases.insert(base.clone());
            if base.is_empty() {
                break;
            }
            base = parent_path(&base).to_owned();
        }
    }

    let parts = module.split('.').collect::<Vec<_>>();
    let mut candidates = BTreeSet::new();
    for base in bases {
        for length in (1..=parts.len()).rev() {
            let module = parts[..length].join("/");
            let Some(stem) = join_repo_path(&base, &module) else {
                continue;
            };
            candidates.insert(format!("{stem}.py"));
            candidates.insert(format!("{stem}/__init__.py"));
        }
    }
    candidates
}

fn extension_candidates(stem: &str, extensions: &[&str], index: &str) -> BTreeSet<String> {
    let mut candidates = BTreeSet::from([stem.to_owned()]);
    for extension in extensions {
        candidates.insert(format!("{stem}.{extension}"));
        candidates.insert(format!("{stem}/{index}.{extension}"));
    }
    candidates
}

fn join_repo_path(base: &str, relative: &str) -> Option<String> {
    let mut components = base
        .split('/')
        .filter(|component| !component.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    for component in relative.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                components.pop()?;
            }
            component => components.push(component.to_owned()),
        }
    }
    Some(components.join("/"))
}

fn parent_path(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(parent, _)| parent)
}

fn rust_source_root(importer: &str) -> String {
    let components = importer.split('/').collect::<Vec<_>>();
    components
        .iter()
        .rposition(|component| *component == "src")
        .map_or_else(
            || parent_path(importer).to_owned(),
            |index| components[..=index].join("/"),
        )
}

fn resolve_candidate(root: &Path, relative: &Path) -> Result<PathBuf, AtlasError> {
    let joined = root.join(relative);
    let resolved = fs::canonicalize(&joined).map_err(|source| IndexError::CanonicalizeFile {
        path: joined.clone(),
        source,
    })?;
    if !resolved.starts_with(root) {
        return Err(AtlasError::OutsideRoot {
            path: resolved,
            root: root.to_path_buf(),
        });
    }
    if resolved != joined {
        return Err(IndexError::EntryChanged { path: joined }.into());
    }
    Ok(resolved)
}

fn estimate_symbol_tokens(
    source: &str,
    path: &Path,
    symbols: &mut [SymbolRecord],
    counter: &dyn TokenCounter,
) -> Result<(), IndexError> {
    let mut line_starts = vec![0_usize];
    for (index, byte) in source.bytes().enumerate() {
        if byte == b'\n' {
            line_starts.push(index + 1);
        }
    }
    for symbol in symbols {
        let start_index = usize::try_from(symbol.start_line.saturating_sub(1))
            .map_err(|_| invalid_range(path, symbol))?;
        let end_index =
            usize::try_from(symbol.end_line).map_err(|_| invalid_range(path, symbol))?;
        let Some(&start_byte) = line_starts.get(start_index) else {
            return Err(invalid_range(path, symbol));
        };
        let end_byte = line_starts.get(end_index).copied().unwrap_or(source.len());
        if start_byte > end_byte || end_byte > source.len() {
            return Err(invalid_range(path, symbol));
        }
        symbol.token_estimate = counter.count(&source[start_byte..end_byte]);
    }
    Ok(())
}

fn invalid_range(path: &Path, symbol: &SymbolRecord) -> IndexError {
    IndexError::InvalidSymbolRange {
        path: path.to_path_buf(),
        start_line: symbol.start_line,
        end_line: symbol.end_line,
    }
}

fn edge_from_import(import: ImportRecord) -> EdgeRecord {
    let to_path = import.resolved_path.unwrap_or(import.raw);
    let resolution = match import.resolution {
        ImportResolution::Resolved => EdgeResolution::Resolved,
        ImportResolution::Ambiguous(candidates) => EdgeResolution::Ambiguous(candidates),
        ImportResolution::Unresolved(reason) => EdgeResolution::Unresolved(reason),
    };
    EdgeRecord {
        to_path,
        kind: EdgeKind::Imports,
        resolution,
    }
}

fn unix_timestamp() -> Result<i64, IndexError> {
    let seconds = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    i64::try_from(seconds).map_err(|_| IndexError::TimestampOutOfRange)
}

fn elapsed_millis(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis())
        .unwrap_or(u64::MAX)
        .max(1)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use compresso_core::{TokenProvider, counter_for};
    use compresso_store::Store;

    use crate::parsers::ImportKind;

    use super::*;

    #[test]
    fn test_per_file_transaction_isolation() {
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let root = directory.path().join("repo");
        fs::create_dir_all(root.join("src")).expect("source directory should be created");
        for name in ['a', 'b', 'c', 'd', 'e'] {
            fs::write(
                root.join(format!("src/{name}.rs")),
                format!("pub fn old_{name}() -> u8 {{ 1 }}\n"),
            )
            .expect("initial source should be written");
        }
        let store = Store::open(&directory.path().join("compresso.db")).expect("store should open");
        let indexer = Indexer::new(&store, counter_for(TokenProvider::Heuristic));
        indexer
            .index(&root, |_| {})
            .expect("initial index should succeed");
        let canonical_root = fs::canonicalize(&root).expect("root should canonicalize");
        let repo = store
            .repo_by_root(&canonical_root)
            .expect("repository lookup should succeed")
            .expect("repository should exist")
            .id;
        let old_hashes = store.file_hashes(repo).expect("old hashes should load");

        for name in ['a', 'b', 'c', 'd', 'e'] {
            fs::write(
                root.join(format!("src/{name}.rs")),
                format!("pub fn new_{name}() -> u8 {{ 2 }}\n"),
            )
            .expect("changed source should be written");
        }
        let mut progress = Vec::new();
        let error = indexer
            .index_with_hook(
                &root,
                |event| progress.push(event),
                |index, path| {
                    if index == 2 {
                        return Err(IndexError::InjectedFailure {
                            path: path.to_path_buf(),
                        });
                    }
                    Ok(())
                },
            )
            .expect_err("third-file injection should stop the run");
        assert!(matches!(
            error,
            AtlasError::Index(IndexError::InjectedFailure { path })
                if path.as_path() == std::path::Path::new("src/c.rs")
        ));
        assert_eq!(
            progress,
            [
                IndexProgress::Parsing {
                    path: PathBuf::from("src/a.rs")
                },
                IndexProgress::Parsing {
                    path: PathBuf::from("src/b.rs")
                }
            ]
        );

        let current_hashes = store.file_hashes(repo).expect("current hashes should load");
        for name in ['a', 'b'] {
            let path = format!("src/{name}.rs");
            let source = fs::read(root.join(&path)).expect("changed source should read");
            assert_eq!(
                current_hashes.get(&path),
                Some(&blake3::hash(&source).to_hex().to_string())
            );
            assert_eq!(
                store
                    .symbols_in_file(repo, &path)
                    .expect("updated symbols should load")[0]
                    .name,
                format!("new_{name}")
            );
        }
        for name in ['c', 'd', 'e'] {
            let path = format!("src/{name}.rs");
            assert_eq!(current_hashes.get(&path), old_hashes.get(&path));
            assert_eq!(
                store
                    .symbols_in_file(repo, &path)
                    .expect("old symbols should load")[0]
                    .name,
                format!("old_{name}")
            );
        }
    }

    #[test]
    fn test_parallel_index_matches_serial_index() {
        let serial = index_generated_repository(Concurrency::serial());
        assert!(serial.files.len() >= 60);
        assert!(serial.edges.iter().any(|edge| edge.contains("resolved")));
        for (jobs, memory_budget_bytes) in [(2, 1 << 30), (4, 1 << 30), (8, 1 << 30), (8, 512)] {
            let parallel =
                index_generated_repository(Concurrency::exact(jobs, memory_budget_bytes));
            assert_eq!(
                parallel, serial,
                "jobs = {jobs}, budget = {memory_budget_bytes}"
            );
        }
    }

    #[test]
    fn test_parallel_preparation_commits_in_order_under_a_starved_memory_budget() {
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let root = directory.path().join("repo");
        write_generated_repository(&root, 48);
        let root = fs::canonicalize(&root).expect("root should canonicalize");
        let mut paths = generated_paths(48);
        paths.sort_unstable();
        let candidates = paths.iter().map(PathBuf::from).collect::<Vec<_>>();
        let known_paths = paths.iter().cloned().collect::<BTreeSet<_>>();
        let context = PrepareContext {
            root: &root,
            known_paths: &known_paths,
            counter: counter_for(TokenProvider::Heuristic),
            indexed_at: 1,
        };

        // One byte admits a single file at a time, so every commit must
        // release the budget before the next file can be dispatched.
        let mut committed = Vec::new();
        prepare_in_parallel(&context, &candidates, 4, 1, |index, outcome| {
            let prepared = outcome.map_err(|failure| failure.error)?;
            committed.push((index, prepared.record.path));
            Ok(())
        })
        .expect("a starved budget must still finish");

        let expected = paths.into_iter().enumerate().collect::<Vec<_>>();
        assert_eq!(committed, expected);
    }

    #[test]
    fn test_parallel_index_stops_at_lowest_ordered_failure() {
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let root = directory.path().join("repo");
        write_generated_repository(&root, 40);
        let store = Store::open(&directory.path().join("compresso.db")).expect("store should open");
        let indexer = Indexer::new(&store, counter_for(TokenProvider::Heuristic))
            .with_concurrency(Concurrency::exact(8, 1 << 30));
        let mut progress = Vec::new();
        let error = indexer
            .index_with_hook(
                &root,
                |event| progress.push(event),
                |index, path| {
                    if index == 5 {
                        return Err(IndexError::InjectedFailure {
                            path: path.to_path_buf(),
                        });
                    }
                    Ok(())
                },
            )
            .expect_err("sixth-file injection should stop the run");
        assert!(matches!(
            error,
            AtlasError::Index(IndexError::InjectedFailure { .. })
        ));
        assert_eq!(progress.len(), 5);

        let canonical_root = fs::canonicalize(&root).expect("root should canonicalize");
        let repo = store
            .repo_by_root(&canonical_root)
            .expect("repository lookup should succeed")
            .expect("repository should exist")
            .id;
        let mut committed = store
            .file_hashes(repo)
            .expect("hashes should load")
            .into_keys()
            .collect::<Vec<_>>();
        committed.sort_unstable();
        let mut expected = generated_paths(40);
        expected.sort_unstable();
        expected.truncate(5);
        assert_eq!(committed, expected);
    }

    #[derive(Debug, Eq, PartialEq)]
    struct IndexSnapshot {
        report: (usize, usize, usize, usize, usize, usize, bool),
        progress: Vec<IndexProgress>,
        files: Vec<(String, String, Option<String>, u64, usize)>,
        symbols: Vec<String>,
        edges: Vec<String>,
    }

    fn generated_paths(files: usize) -> Vec<String> {
        (0..files)
            .map(|index| {
                let extension = ["rs", "ts", "py", "md"][index % 4];
                format!("src/m{:02}/file_{index:03}.{extension}", index % 7)
            })
            .collect()
    }

    fn write_generated_repository(root: &Path, files: usize) {
        for (index, path) in generated_paths(files).into_iter().enumerate() {
            let absolute = root.join(&path);
            fs::create_dir_all(
                absolute
                    .parent()
                    .expect("generated path should have a parent"),
            )
            .expect("generated directory should be created");
            let body = match index % 4 {
                0 => format!(
                    "use crate::m00::file_000;\npub fn rust_{index}() -> usize {{ {index} }}\npub struct Shape{index};\n"
                ),
                1 => format!(
                    "import {{ helper }} from './file_{:03}';\nexport function ts_{index}(): number {{ return {index}; }}\n",
                    index + 4
                ),
                2 => format!("import os\n\ndef py_{index}():\n    return {index}\n"),
                _ => format!("# Notes {index}\n\nPlain text body {index}.\n"),
            };
            fs::write(absolute, body).expect("generated source should be written");
        }
    }

    fn index_generated_repository(concurrency: Concurrency) -> IndexSnapshot {
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let root = directory.path().join("repo");
        write_generated_repository(&root, 64);
        let store = Store::open(&directory.path().join("compresso.db")).expect("store should open");
        let mut progress = Vec::new();
        let report = Indexer::new(&store, counter_for(TokenProvider::Heuristic))
            .with_concurrency(concurrency)
            .index(&root, |event| progress.push(event))
            .expect("index should succeed");
        let canonical_root = fs::canonicalize(&root).expect("root should canonicalize");
        let repo = store
            .repo_by_root(&canonical_root)
            .expect("repository lookup should succeed")
            .expect("repository should exist")
            .id;
        let mut paths = store
            .file_hashes(repo)
            .expect("hashes should load")
            .into_iter()
            .collect::<Vec<_>>();
        paths.sort_unstable();
        let mut files = Vec::new();
        let mut symbols = Vec::new();
        for (path, hash) in paths {
            let record = store
                .indexed_file(repo, &path)
                .expect("file should load")
                .expect("file should exist");
            files.push((
                path.clone(),
                hash,
                record.language.map(|language| format!("{language:?}")),
                record.size_bytes,
                record.token_estimate,
            ));
            symbols.extend(
                store
                    .symbols_in_file(repo, &path)
                    .expect("symbols should load")
                    .into_iter()
                    .map(|symbol| format!("{path}: {symbol:?}")),
            );
        }
        let mut edges = store
            .resolved_edges(repo)
            .expect("resolved edges should load")
            .into_iter()
            .map(|edge| format!("resolved {edge:?}"))
            .collect::<Vec<_>>();
        edges.extend(
            store
                .unresolved_edge_sources(repo)
                .expect("unresolved sources should load")
                .into_iter()
                .map(|path| format!("unresolved {path}")),
        );
        IndexSnapshot {
            report: (
                report.files_indexed,
                report.files_removed,
                report.files_skipped,
                report.symbols,
                report.imports_resolved,
                report.imports_unresolved,
                report.stale,
            ),
            progress,
            files,
            symbols,
            edges,
        }
    }

    #[test]
    fn test_local_import_resolution_preserves_ambiguity_and_unresolved_state() {
        let known = BTreeSet::from(["src/choice.js".to_owned(), "src/choice.ts".to_owned()]);
        let mut imports = [
            ImportRecord {
                raw: "./choice".to_owned(),
                kind: ImportKind::Import,
                resolved_path: None,
                resolution: ImportResolution::Unresolved("not_indexed".to_owned()),
            },
            ImportRecord {
                raw: "external-package".to_owned(),
                kind: ImportKind::Import,
                resolved_path: None,
                resolution: ImportResolution::Unresolved("not_indexed".to_owned()),
            },
        ];

        resolve_repository_imports("src/app.ts", &mut imports, &known);

        assert_eq!(
            imports[0].resolution,
            ImportResolution::Ambiguous(vec![
                "src/choice.js".to_owned(),
                "src/choice.ts".to_owned(),
            ])
        );
        assert!(imports[0].resolved_path.is_none());
        assert_eq!(
            imports[1].resolution,
            ImportResolution::Unresolved("not_indexed".to_owned())
        );
        assert!(imports[1].resolved_path.is_none());
    }
}
