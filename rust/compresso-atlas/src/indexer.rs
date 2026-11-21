use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs;
use std::io;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, mpsc};
use std::thread;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use compresso_core::TokenCounter;
use compresso_store::{
    CallSiteRecord, EdgeKind, EdgeRecord, EdgeResolution, FileRecord, RepoId, Store, SymbolRecord,
};

use crate::concurrency::{Concurrency, ReadAhead};
use crate::declared_imports::DeclaredPackages;
use crate::go_imports::GoModules;
use crate::parsers::languages::{self, ImportStyle};
use crate::parsers::{ImportContext, ImportRecord, ImportResolution, ParseError, ParserRegistry};
use crate::rust_imports::{self, RustWorkspace};
use crate::scanner::{AtlasError, DEFAULT_MAX_FILE_BYTES, ScanOptions, scan_with_concurrency};

const INDEX_SCHEMA_VERSION: u32 = 1;
pub(crate) const DEFAULT_PARSER_VERSION: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    "+atlas-shell-text-v1+rust-imports-v2+java-csharp-v1+go-v1+calls-v1"
);
/// Marks an index whose parser recorded call sites (D89). Every version this
/// indexer stamps carries it, including one set with
/// [`Indexer::with_parser_version`], so `trace` can tell a call graph exists
/// without knowing the exact parser version.
pub(crate) const CALL_GRAPH_MARKER: &str = "+calls-v1";
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
    /// Call sites recorded inside the indexed functions and methods (D89).
    pub call_sites: usize,
    /// Files whose call extraction stopped at a per-file bound.
    pub call_site_files_truncated: usize,
    /// Files whose call sites the store would reject: the file is indexed
    /// with its symbols and imports, without call sites.
    pub call_site_files_rejected: usize,
    pub duration_ms: u64,
    pub stale: bool,
    /// The repository's own `info/exclude` was not applied in full (see
    /// [`crate::ScanResult::git_exclude_unusable`]).
    pub git_exclude_unusable: bool,
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

    /// Overrides the parser version used for staleness checks and version
    /// stamps. The call-graph marker (`+calls-v1`) is appended when the given
    /// version lacks it, because this indexer always records call sites.
    pub fn with_parser_version(mut self, parser_version: impl Into<String>) -> Self {
        let mut parser_version = parser_version.into();
        if !parser_version.contains(CALL_GRAPH_MARKER) {
            parser_version.push_str(CALL_GRAPH_MARKER);
        }
        self.parser_version = parser_version;
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
        let git_exclude_unusable = scan.git_exclude_unusable;
        let known_paths = known_repository_paths(&previous_hashes, &scan, &removed_set)?;
        let rust_workspace = RustWorkspace::discover(&canonical_root, &known_paths);
        let go_modules = GoModules::discover(&canonical_root, &known_paths);
        let paths_expanded = !scan.added.is_empty();
        // A `go.mod` added, changed, or removed can move any Go import
        // between a package directory and the outside world.
        let go_modules_changed = scan
            .added
            .iter()
            .chain(&scan.changed)
            .any(|path| path.file_name().is_some_and(|name| name == "go.mod"))
            || scan
                .removed
                .iter()
                .any(|path| path == "go.mod" || path.ends_with("/go.mod"));
        // A file added to a Go package directory belongs to every import of
        // that package, so files importing the package are re-resolved.
        let grown_packages = scan
            .added
            .iter()
            .filter_map(|path| path.to_str())
            .filter(|path| languages::package_is_directory(path))
            .map(|path| parent_path(path).to_owned())
            .collect::<HashSet<_>>();
        let added = scan
            .added
            .iter()
            .filter_map(|path| path.to_str())
            .map(str::to_owned)
            .collect::<HashSet<_>>();
        let mut candidates = scan
            .added
            .into_iter()
            .chain(scan.changed)
            .collect::<BTreeSet<_>>();
        // Java and C# importers are re-resolved by the packages a run
        // changes (`re_resolve_importers`), not because any file appeared:
        // nearly every one of them has an external import (D105).
        if paths_expanded {
            candidates.extend(
                self.store
                    .unresolved_edge_sources(repo)?
                    .into_iter()
                    .filter(|path| !removed_set.contains(path))
                    .filter(|path| declaring_language(path).is_none())
                    .map(PathBuf::from),
            );
        }
        if go_modules_changed {
            candidates.extend(
                known_paths
                    .iter()
                    .filter(|path| path.ends_with(".go") && !removed_set.contains(*path))
                    .map(PathBuf::from),
            );
        }
        if !grown_packages.is_empty() {
            candidates.extend(
                self.store
                    .resolved_edges(repo)?
                    .into_iter()
                    .filter(|edge| grown_packages.contains(parent_path(&edge.to_path)))
                    .filter(|edge| !removed_set.contains(&edge.from_path))
                    .map(|edge| PathBuf::from(edge.from_path)),
            );
        }
        if !removed_set.is_empty() {
            let mut affected_sources = self.store.unresolved_edge_sources(repo)?;
            affected_sources.retain(|path| declaring_language(path).is_none());
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
        let declared = self.declared_packages(
            repo,
            &canonical_root,
            &known_paths,
            &removed_set,
            &added,
            stale,
            &mut candidates,
        )?;

        let indexed_at = unix_timestamp()?;
        let mut files_indexed = 0_usize;
        let mut files_skipped = scan.skipped.len();
        let mut symbols = 0_usize;
        let mut imports_resolved = 0_usize;
        let mut imports_unresolved = 0_usize;
        let mut call_sites = 0_usize;
        let mut call_site_files_truncated = 0_usize;
        let mut call_site_files_rejected = 0_usize;

        let candidates = candidates.into_iter().collect::<Vec<_>>();
        let context = PrepareContext {
            root: &canonical_root,
            known_paths: &known_paths,
            rust_workspace: &rust_workspace,
            go_modules: &go_modules,
            declared: &declared,
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
                    return Err(*failure.error);
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
            if write_prepared(self.store, repo, &prepared)? {
                call_site_files_rejected += 1;
            } else if prepared.parsed {
                call_sites += prepared.calls.len();
                call_site_files_truncated += usize::from(prepared.calls_truncated);
            }
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
            call_sites,
            call_site_files_truncated,
            call_site_files_rejected,
            duration_ms: elapsed_millis(started),
            stale,
            git_exclude_unusable,
        })
    }
}

/// The language of a path whose files declare packages or namespaces (Java,
/// C#).
fn declaring_language(path: &str) -> Option<compresso_store::FileLanguage> {
    ParserRegistry::for_path(Path::new(path))
        .filter(|language| languages::spec(*language).declarations.is_some())
}

impl Indexer<'_> {
    /// The declared packages and namespaces of every scanned Java and C#
    /// file (D105): stored declarations for files this run does not re-parse,
    /// and, for the candidates it does, declarations read before the run, so
    /// an import resolves against what every file declares now. A candidate
    /// that is added, removed, or declares something else than before makes
    /// every file importing the packages it left or joined a candidate too.
    #[allow(clippy::too_many_arguments)]
    fn declared_packages(
        &self,
        repo: RepoId,
        root: &Path,
        known_paths: &BTreeSet<String>,
        removed: &HashSet<String>,
        added: &HashSet<String>,
        stale: bool,
        candidates: &mut BTreeSet<PathBuf>,
    ) -> Result<DeclaredPackages, AtlasError> {
        let any_declaring = known_paths
            .iter()
            .chain(removed)
            .any(|path| declaring_language(path).is_some());
        if !any_declaring {
            return Ok(DeclaredPackages::default());
        }
        let mut stored = BTreeMap::<String, Vec<String>>::new();
        for (path, name) in self
            .store
            .module_declarations(repo, &languages::declaring_languages())?
        {
            stored.entry(path).or_default().push(name);
        }
        let to_read = candidates
            .iter()
            .filter_map(|path| path.to_str())
            .filter(|path| !removed.contains(*path) && declaring_language(path).is_some())
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let workers = if to_read.len() < PARALLEL_INDEX_MIN_CANDIDATES {
            1
        } else {
            self.concurrency
                .workers_for(to_read.len(), DEFAULT_MAX_FILE_BYTES)
        };
        let read = read_declarations(root, &to_read, workers);

        let declarations_of = |path: &str| {
            read.get(path)
                .or_else(|| stored.get(path))
                .map_or(&[][..], Vec::as_slice)
        };
        let mut rows = Vec::new();
        for path in known_paths {
            if removed.contains(path) {
                continue;
            }
            if let Some(language) = declaring_language(path) {
                for name in declarations_of(path) {
                    rows.push((path.clone(), language, name.clone()));
                }
            }
        }
        if !stale {
            let after = rows
                .iter()
                .map(|(_, language, name)| (language.as_str(), name.clone()))
                .collect::<HashSet<_>>();
            self.re_resolve_importers(repo, &stored, &read, &after, removed, added, candidates)?;
        }
        Ok(DeclaredPackages::new(rows))
    }

    /// Adds to `candidates` the Java and C# files whose stored imports a
    /// run's changes can alter (D105): an import of one type whose file now
    /// declares another package or is gone; an import not resolved before
    /// (external or `not_indexed`) under a package a file joined or left;
    /// and a whole-package record whose package no file declares any more.
    /// A file joining a package its importers already import changes no
    /// stored row of theirs: the record is expanded when read.
    #[allow(clippy::too_many_arguments)]
    fn re_resolve_importers(
        &self,
        repo: RepoId,
        stored: &BTreeMap<String, Vec<String>>,
        read: &BTreeMap<String, Vec<String>>,
        after: &HashSet<(&'static str, String)>,
        removed: &HashSet<String>,
        added: &HashSet<String>,
        candidates: &mut BTreeSet<PathBuf>,
    ) -> Result<(), AtlasError> {
        let mut moved = HashSet::<(&'static str, String)>::new();
        let mut relocated = HashSet::<&str>::new();
        let mut note = |path: &str, names: &[String]| {
            if let Some(language) = declaring_language(path) {
                moved.extend(names.iter().map(|name| (language.as_str(), name.clone())));
            }
        };
        for (path, now) in read {
            let before = stored.get(path).map_or(&[][..], Vec::as_slice);
            let unchanged = {
                let mut left = before.iter().collect::<Vec<_>>();
                let mut right = now.iter().collect::<Vec<_>>();
                left.sort();
                right.sort();
                left == right
            };
            if !unchanged {
                relocated.insert(path.as_str());
            }
            if added.contains(path) || !unchanged {
                note(path, before);
                note(path, now);
            }
        }
        for path in removed {
            note(path, stored.get(path).map_or(&[][..], Vec::as_slice));
        }
        if moved.is_empty() {
            return Ok(());
        }
        let disappeared = moved
            .iter()
            .filter(|package| !after.contains(*package))
            .cloned()
            .collect::<HashSet<_>>();
        // A type import names every file of its package named after the
        // type (`Store.cs`, `Store.Part.cs`): a file added under, or moved to
        // or from, such a name re-resolves the importers of its namesakes.
        let stem = |path: &str| {
            let file = path.rsplit('/').next().unwrap_or(path);
            file.split('.').next().unwrap_or(file).to_owned()
        };
        let mut joined = HashSet::<(&'static str, String, String)>::new();
        for (path, now) in read {
            if (added.contains(path) || relocated.contains(path.as_str()))
                && let Some(language) = declaring_language(path)
            {
                let before = stored.get(path).map_or(&[][..], Vec::as_slice);
                for name in before.iter().chain(now) {
                    joined.insert((language.as_str(), name.clone(), stem(path)));
                }
            }
        }
        if !joined.is_empty() {
            for edge in self.store.resolved_edges(repo)? {
                if removed.contains(&edge.from_path)
                    || declaring_language(&edge.from_path).is_none()
                {
                    continue;
                }
                let Some(language) = declaring_language(&edge.to_path) else {
                    continue;
                };
                let target_stem = stem(&edge.to_path);
                let namesake = relocated.contains(edge.to_path.as_str())
                    || stored
                        .get(&edge.to_path)
                        .into_iter()
                        .chain(read.get(&edge.to_path))
                        .flatten()
                        .any(|name| {
                            joined.contains(&(language.as_str(), name.clone(), target_stem.clone()))
                        });
                if namesake {
                    candidates.insert(PathBuf::from(edge.from_path));
                }
            }
        }
        for (from, raw, detail) in self.store.unresolved_edge_targets(repo)? {
            if removed.contains(&from) {
                continue;
            }
            let Some(language) = declaring_language(&from) else {
                continue;
            };
            let raw = raw
                .strip_prefix(crate::declared_imports::GLOBAL_IMPORT_PREFIX)
                .unwrap_or(&raw);
            let raw = raw.strip_suffix(".*").unwrap_or(raw);
            if detail.as_deref() == Some(compresso_store::EDGE_DETAIL_PACKAGE) {
                if disappeared.contains(&(language.as_str(), raw.to_owned())) {
                    candidates.insert(PathBuf::from(from));
                }
                continue;
            }
            // Every dotted prefix of the import, `a.b.C` then `a.b` then `a`.
            let mut end = raw.len();
            let names_moved = loop {
                if moved.contains(&(language.as_str(), raw[..end].to_owned())) {
                    break true;
                }
                match raw[..end].rfind('.') {
                    Some(dot) => end = dot,
                    None => break false,
                }
            };
            // A C# `using` written inside a namespace may name a namespace
            // nested in an enclosing one: `Util` for `Acme.Util`.
            let names_nested = || {
                moved.iter().any(|(moved_language, name)| {
                    *moved_language == language.as_str()
                        && name
                            .strip_suffix(raw)
                            .is_some_and(|outer| outer.ends_with('.'))
                })
            };
            if names_moved || names_nested() {
                candidates.insert(PathBuf::from(from));
            }
        }
        Ok(())
    }
}

/// Reads the declared packages or namespaces of `paths` on `workers`
/// threads. A file that cannot be read or parsed declares nothing here; its
/// own preparation reports the failure.
fn read_declarations(
    root: &Path,
    paths: &[String],
    workers: usize,
) -> BTreeMap<String, Vec<String>> {
    let declarations_of = |path: &str| -> Vec<String> {
        let Some(language) = declaring_language(path) else {
            return Vec::new();
        };
        let Ok(absolute) = resolve_candidate(root, Path::new(path)) else {
            return Vec::new();
        };
        let Ok(source) = fs::read_to_string(&absolute) else {
            return Vec::new();
        };
        panic::catch_unwind(AssertUnwindSafe(|| {
            crate::parsers::declared_packages(language, &source)
        }))
        .unwrap_or_default()
    };
    if workers <= 1 {
        return paths
            .iter()
            .map(|path| (path.clone(), declarations_of(path)))
            .collect();
    }
    let next = std::sync::atomic::AtomicUsize::new(0);
    let found = Mutex::new(BTreeMap::new());
    thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(path) = paths.get(index) else {
                        break;
                    };
                    let declared = declarations_of(path);
                    if let Ok(mut found) = found.lock() {
                        found.insert(path.clone(), declared);
                    }
                }
            });
        }
    });
    found.into_inner().unwrap_or_default()
}

/// Writes one prepared file in one transaction and reports whether its call
/// sites were dropped.
///
/// The call extractor already drops every call the store would refuse; this
/// is the second line of defence. If one file's call sites still fail the
/// store's validation, the file is written with its symbols and imports and
/// without call sites, so a hostile or generated file can never abort the
/// repository index (the approach of the empty-import fix). Any other store
/// failure still stops the run.
fn write_prepared(
    store: &Store,
    repo: RepoId,
    prepared: &PreparedFile,
) -> Result<bool, AtlasError> {
    let rejected =
        compresso_store::validate_call_sites(&prepared.calls, prepared.symbols.len()).is_err();
    let calls: &[CallSiteRecord] = if rejected { &[] } else { &prepared.calls };
    store.replace_indexed_file_with_calls(
        repo,
        &prepared.record,
        &prepared.symbols,
        &prepared.edges,
        calls,
    )?;
    Ok(rejected)
}

/// Read-only inputs shared by every file preparation, on any thread.
struct PrepareContext<'a> {
    root: &'a Path,
    known_paths: &'a BTreeSet<String>,
    rust_workspace: &'a RustWorkspace,
    go_modules: &'a GoModules,
    declared: &'a DeclaredPackages,
    counter: &'a dyn TokenCounter,
    indexed_at: i64,
}

/// Everything one file contributes to the index, computed without the Store.
struct PreparedFile {
    record: FileRecord,
    symbols: Vec<SymbolRecord>,
    edges: Vec<EdgeRecord>,
    calls: Vec<CallSiteRecord>,
    calls_truncated: bool,
    parsed: bool,
    imports_resolved: usize,
    imports_unresolved: usize,
}

/// A preparation failure plus whether parsing had begun, so the committing
/// thread can replay the same progress events a serial run would emit.
struct PrepareFailure {
    // Boxed: `AtlasError` carries paths, which push the `Err` variant past
    // clippy's `result_large_err` threshold on Windows.
    error: Box<AtlasError>,
    parsing_started: bool,
}

impl PrepareFailure {
    fn before_parse(error: impl Into<AtlasError>) -> Self {
        Self {
            error: Box::new(error.into()),
            parsing_started: false,
        }
    }

    fn during_parse(error: impl Into<AtlasError>) -> Self {
        Self {
            error: Box::new(error.into()),
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
    let mut calls = Vec::new();
    let mut calls_truncated = false;
    let mut imports_resolved = 0_usize;
    let mut imports_unresolved = 0_usize;

    if let Some(language) = language {
        let mut extraction = ParserRegistry::extract(language, &source).map_err(|source| {
            PrepareFailure::during_parse(IndexError::Parse {
                path: relative.to_path_buf(),
                source,
            })
        })?;
        resolve_repository_imports(
            &relative_text,
            &mut extraction.imports,
            context.known_paths,
            context.rust_workspace,
            context.go_modules,
            context.declared,
        );
        symbols = extraction.symbols;
        calls = extraction.calls;
        calls_truncated = extraction.calls_truncated;
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
        calls,
        calls_truncated,
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
    commit: C,
) -> Result<(), AtlasError>
where
    C: FnMut(usize, Result<PreparedFile, PrepareFailure>) -> Result<(), AtlasError>,
{
    prepare_in_parallel_with(
        context.root,
        candidates,
        workers,
        memory_budget_bytes,
        |candidate| prepare_file(context, candidate),
        commit,
    )
}

/// [`prepare_in_parallel`] with the per-file preparation step supplied by the
/// caller.
fn prepare_in_parallel_with<P, C>(
    root: &Path,
    candidates: &[PathBuf],
    workers: usize,
    memory_budget_bytes: u64,
    prepare: P,
    mut commit: C,
) -> Result<(), AtlasError>
where
    P: Fn(&Path) -> Result<PreparedFile, PrepareFailure> + Sync,
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
            let prepare = &prepare;
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
                    // A panic is forwarded rather than left to end this worker
                    // silently: the committing thread would otherwise wait
                    // forever for a result that never arrives.
                    let outcome =
                        panic::catch_unwind(AssertUnwindSafe(|| prepare(&candidates[index])));
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
        let mut worker_panic = None;
        let outcome = (|| {
            while next_commit < candidates.len() {
                while next_dispatch < candidates.len() {
                    let cost_bytes = fs::metadata(root.join(&candidates[next_dispatch]))
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
                        let prepared = ready.remove(&index).unwrap_or_else(|| prepare(candidate));
                        commit(index, prepared)?;
                    }
                    return Ok(());
                };
                let prepared = match prepared {
                    Ok(prepared) => prepared,
                    Err(payload) => {
                        worker_panic = Some(payload);
                        return Ok(());
                    }
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
        // The workers must be released before the panic resumes, because the
        // scope joins them while unwinding.
        drop(job_sender);
        if let Some(payload) = worker_panic {
            panic::resume_unwind(payload);
        }
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
    imports: &mut Vec<ImportRecord>,
    known_paths: &BTreeSet<String>,
    rust_workspace: &RustWorkspace,
    go_modules: &GoModules,
    declared: &DeclaredPackages,
) {
    let language = ParserRegistry::for_path(Path::new(importer));
    let style = language.map_or(ImportStyle::None, |language| {
        languages::spec(language).imports
    });
    if style == ImportStyle::Go {
        *imports = resolve_go_imports(importer, std::mem::take(imports), go_modules);
        return;
    }
    if let (ImportStyle::Java | ImportStyle::CSharp, Some(language)) = (style, language) {
        *imports = resolve_declared_imports(importer, language, std::mem::take(imports), declared);
        return;
    }
    for import in imports.iter_mut() {
        let candidates = match style {
            ImportStyle::Rust => {
                (import.resolved_path, import.resolution) =
                    rust_imports::resolve(importer, &import.raw, known_paths, rust_workspace);
                continue;
            }
            ImportStyle::Python => python_candidates(importer, &import.raw),
            ImportStyle::Script => {
                // The `node:` scheme only ever names a runtime built-in.
                if import.raw.starts_with("node:") {
                    import.resolved_path = None;
                    import.resolution =
                        ImportResolution::Unresolved(rust_imports::EXTERNAL.to_owned());
                    continue;
                }
                script_candidates(importer, &import.raw)
            }
            ImportStyle::None | ImportStyle::Go | ImportStyle::Java | ImportStyle::CSharp => {
                BTreeSet::new()
            }
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

/// Resolved edges one Go file may hold. A file importing more package files
/// than this keeps the first ones and records one unresolved
/// [`GO_EDGE_LIMIT`] edge per import it cut, so `impact` reports it as
/// incomplete instead of silently missing dependents.
pub(crate) const MAX_GO_IMPORT_EDGES_PER_FILE: usize = 4_096;
/// Detail of the unresolved edge that marks an import cut by
/// [`MAX_GO_IMPORT_EDGES_PER_FILE`].
pub(crate) const GO_EDGE_LIMIT: &str = "edge_limit";

/// A Go import names a package directory: one edge per file in it. A path
/// imported twice (`import _ "p"` repeated) counts once.
fn resolve_go_imports(
    importer: &str,
    imports: Vec<ImportRecord>,
    go_modules: &GoModules,
) -> Vec<ImportRecord> {
    let mut seen = HashSet::new();
    let mut resolved_edges = 0_usize;
    let mut output = Vec::new();
    for import in imports {
        if !seen.insert(import.raw.clone()) {
            continue;
        }
        let mut cut = false;
        for (resolved_path, resolution) in go_modules.resolve(importer, &import.raw) {
            if resolution == ImportResolution::Resolved {
                if resolved_edges == MAX_GO_IMPORT_EDGES_PER_FILE {
                    cut = true;
                    continue;
                }
                resolved_edges += 1;
            }
            output.push(ImportRecord {
                raw: import.raw.clone(),
                kind: import.kind,
                resolved_path,
                resolution,
                context: ImportContext::None,
            });
        }
        if cut {
            output.push(ImportRecord {
                raw: import.raw,
                kind: import.kind,
                resolved_path: None,
                resolution: ImportResolution::Unresolved(GO_EDGE_LIMIT.to_owned()),
                context: ImportContext::None,
            });
        }
    }
    output
}

/// A Java import or C# `using` (D105, owner decision 2026-11-21): a whole
/// package or namespace is one record naming it, expanded to its files by
/// queries; a type import is an edge to the type's file. A repeated import
/// counts once and a file reached twice once. At most
/// [`MAX_GO_IMPORT_EDGES_PER_FILE`] records are kept per file, a bound on
/// hostile input only; one unresolved [`GO_EDGE_LIMIT`] record, naming the
/// first import cut, marks the cut.
fn resolve_declared_imports(
    importer: &str,
    language: compresso_store::FileLanguage,
    imports: Vec<ImportRecord>,
    declared: &DeclaredPackages,
) -> Vec<ImportRecord> {
    let mut seen = HashSet::new();
    let mut reached = HashSet::new();
    let mut output = Vec::new();
    for import in imports {
        let (raw, targets) = declared.resolve_in(importer, language, &import.raw, &import.context);
        if !seen.insert(raw.clone()) {
            continue;
        }
        let import = ImportRecord { raw, ..import };
        for (resolved_path, resolution) in targets {
            if let Some(path) = resolved_path.as_deref()
                && !reached.insert(path.to_owned())
            {
                continue;
            }
            if output.len() == MAX_GO_IMPORT_EDGES_PER_FILE {
                output.push(ImportRecord {
                    raw: import.raw,
                    kind: import.kind,
                    resolved_path: None,
                    resolution: ImportResolution::Unresolved(GO_EDGE_LIMIT.to_owned()),
                    context: ImportContext::None,
                });
                return output;
            }
            output.push(ImportRecord {
                raw: import.raw.clone(),
                kind: import.kind,
                resolved_path,
                resolution,
                context: ImportContext::None,
            });
        }
    }
    output
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
    fn test_every_stamped_parser_version_carries_the_call_graph_marker() {
        assert!(DEFAULT_PARSER_VERSION.ends_with(CALL_GRAPH_MARKER));
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let store = Store::open(&directory.path().join("compresso.db")).expect("store should open");
        let counter = counter_for(TokenProvider::Heuristic);
        assert_eq!(
            Indexer::new(&store, counter)
                .with_parser_version("custom-1")
                .parser_version,
            "custom-1+calls-v1"
        );
        assert_eq!(
            Indexer::new(&store, counter)
                .with_parser_version("custom-1+calls-v1")
                .parser_version,
            "custom-1+calls-v1"
        );
    }

    #[test]
    fn test_call_sites_the_store_rejects_are_dropped_without_failing_the_file() {
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let store = Store::open(&directory.path().join("compresso.db")).expect("store should open");
        let repo = store
            .approve_repo(directory.path())
            .expect("repository should be approved");
        let symbol = SymbolRecord {
            name: "run".to_owned(),
            qualified_name: "run".to_owned(),
            kind: compresso_store::SymbolKind::Function,
            signature: None,
            start_line: 1,
            end_line: 3,
            token_estimate: 1,
        };
        let call = |qualifier: Option<String>| CallSiteRecord {
            caller: 0,
            callee_name: "make".to_owned(),
            qualifier,
            form: compresso_store::CallForm::Path,
            line: 2,
        };
        let prepared = |calls| PreparedFile {
            record: FileRecord {
                path: "src/hostile.rs".to_owned(),
                hash: "aa".to_owned(),
                language: Some(compresso_store::FileLanguage::Rust),
                size_bytes: 10,
                token_estimate: 2,
                indexed_at: 1,
            },
            symbols: vec![symbol.clone()],
            edges: Vec::new(),
            calls,
            calls_truncated: false,
            parsed: true,
            imports_resolved: 0,
            imports_unresolved: 0,
        };

        let hostile = prepared(vec![
            call(Some("Store".to_owned())),
            call(Some("A".repeat(300))),
        ]);
        assert!(write_prepared(&store, repo, &hostile).expect("the file must still be written"));
        assert_eq!(
            store
                .symbols_in_file(repo, "src/hostile.rs")
                .expect("symbols")
                .len(),
            1
        );
        assert_eq!(store.call_site_count(repo).expect("count"), 0);

        let valid = prepared(vec![call(Some("Store".to_owned()))]);
        assert!(!write_prepared(&store, repo, &valid).expect("valid write"));
        assert_eq!(store.call_site_count(repo).expect("count"), 1);
    }

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
            rust_workspace: &RustWorkspace::default(),
            go_modules: &GoModules::default(),
            declared: &DeclaredPackages::default(),
            counter: counter_for(TokenProvider::Heuristic),
            indexed_at: 1,
        };

        // One byte admits a single file at a time, so every commit must
        // release the budget before the next file can be dispatched.
        let mut committed = Vec::new();
        prepare_in_parallel(&context, &candidates, 4, 1, |index, outcome| {
            let prepared = outcome.map_err(|failure| *failure.error)?;
            committed.push((index, prepared.record.path));
            Ok(())
        })
        .expect("a starved budget must still finish");

        let expected = paths.into_iter().enumerate().collect::<Vec<_>>();
        assert_eq!(committed, expected);
    }

    #[test]
    fn test_parallel_preparation_propagates_a_worker_panic_instead_of_hanging() {
        let candidates = (0..40)
            .map(|index| PathBuf::from(format!("src/file_{index:02}.rs")))
            .collect::<Vec<_>>();
        let (finished_sender, finished_receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                prepare_in_parallel_with(
                    Path::new("missing-root"),
                    &candidates,
                    4,
                    1 << 30,
                    |candidate| {
                        assert!(!candidate.ends_with("file_05.rs"), "injected worker panic");
                        Err(PrepareFailure::before_parse(IndexError::InjectedFailure {
                            path: candidate.to_path_buf(),
                        }))
                    },
                    |_, _| Ok(()),
                )
            }));
            let _ = finished_sender.send(run.is_err());
        });

        let panicked = finished_receiver
            .recv_timeout(std::time::Duration::from_secs(20))
            .expect("a worker panic must not hang the run");
        assert!(panicked, "the worker panic must reach the caller");
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
    fn test_go_imports_are_deduplicated_and_capped_per_file() {
        let mut known = BTreeSet::from(["main.go".to_owned()]);
        for index in 0..(MAX_GO_IMPORT_EDGES_PER_FILE + 10) {
            known.insert(format!("p/f{index}.go"));
        }
        known.insert("q/q.go".to_owned());
        let modules = GoModules::for_test(&[("m", "")], &known);
        let import = |raw: &str| ImportRecord {
            raw: raw.to_owned(),
            kind: ImportKind::Import,
            resolved_path: None,
            resolution: ImportResolution::Unresolved("not_indexed".to_owned()),
            context: ImportContext::None,
        };
        let mut imports = vec![import("m/q"), import("m/q"), import("m/q"), import("fmt")];
        resolve_repository_imports(
            "main.go",
            &mut imports,
            &known,
            &RustWorkspace::default(),
            &modules,
            &DeclaredPackages::default(),
        );
        assert_eq!(imports.len(), 2, "{imports:?}");
        assert_eq!(imports[0].resolved_path.as_deref(), Some("q/q.go"));

        let mut imports = vec![import("m/p"), import("m/q")];
        resolve_repository_imports(
            "main.go",
            &mut imports,
            &known,
            &RustWorkspace::default(),
            &modules,
            &DeclaredPackages::default(),
        );
        let resolved = imports
            .iter()
            .filter(|import| import.resolution == ImportResolution::Resolved)
            .count();
        assert_eq!(resolved, MAX_GO_IMPORT_EDGES_PER_FILE);
        let cut = imports
            .iter()
            .filter(|import| {
                import.resolution == ImportResolution::Unresolved(GO_EDGE_LIMIT.to_owned())
            })
            .map(|import| import.raw.as_str())
            .collect::<Vec<_>>();
        assert_eq!(cut, ["m/p", "m/q"]);
    }

    /// E17-40 (D105, owner decision 2026-11-21): a namespace import is one
    /// record however many files declare the namespace; a repeated import or
    /// a file reached twice counts once; a file keeps at most 4,096 import
    /// records, and one `edge_limit` record marks the cut.
    #[test]
    fn test_declared_imports_are_one_record_per_package_and_capped_per_file() {
        use compresso_store::{EDGE_DETAIL_PACKAGE, FileLanguage};
        let mut rows = Vec::new();
        for index in 0..5_000 {
            rows.push((
                format!("big/F{index}.cs"),
                FileLanguage::CSharp,
                "Big".to_owned(),
            ));
        }
        rows.push((
            "small/Only.cs".to_owned(),
            FileLanguage::CSharp,
            "Small".to_owned(),
        ));
        rows.push((
            "small/Only.cs".to_owned(),
            FileLanguage::CSharp,
            "Small.Inner".to_owned(),
        ));
        let declared = DeclaredPackages::new(rows);
        let import = |raw: &str| ImportRecord {
            raw: raw.to_owned(),
            kind: ImportKind::Import,
            resolved_path: None,
            resolution: ImportResolution::Unresolved("not_indexed".to_owned()),
            context: ImportContext::None,
        };
        let resolve = |imports: &mut Vec<ImportRecord>| {
            resolve_repository_imports(
                "app/Main.cs",
                imports,
                &BTreeSet::new(),
                &RustWorkspace::default(),
                &GoModules::default(),
                &declared,
            );
        };
        let rows_of = |imports: &[ImportRecord]| {
            imports
                .iter()
                .map(|import| {
                    let detail = match &import.resolution {
                        ImportResolution::Unresolved(detail) => detail.clone(),
                        ImportResolution::Resolved => "resolved".to_owned(),
                        ImportResolution::Ambiguous(_) => "ambiguous".to_owned(),
                    };
                    (import.raw.clone(), import.resolved_path.clone(), detail)
                })
                .collect::<Vec<_>>()
        };
        let mut imports = vec![
            import("Big"),
            import("Small.Only"),
            import("Big"),
            import("Small.Only"),
            import("System"),
        ];
        resolve(&mut imports);
        assert_eq!(
            rows_of(&imports),
            [
                ("Big".to_owned(), None, EDGE_DETAIL_PACKAGE.to_owned()),
                (
                    "Small.Only".to_owned(),
                    Some("small/Only.cs".to_owned()),
                    "resolved".to_owned()
                ),
                ("System".to_owned(), None, "external".to_owned()),
            ]
        );

        let mut imports = (0..5_000)
            .map(|index| import(&format!("Vendor.N{index}")))
            .collect::<Vec<_>>();
        resolve(&mut imports);
        assert_eq!(imports.len(), MAX_GO_IMPORT_EDGES_PER_FILE + 1);
        let last = rows_of(&imports).pop().expect("marker");
        assert_eq!(
            last,
            ("Vendor.N4096".to_owned(), None, GO_EDGE_LIMIT.to_owned())
        );
    }

    #[test]
    fn test_local_import_resolution_preserves_ambiguity_and_unresolved_state() {
        let known = BTreeSet::from(["src/choice.js".to_owned(), "src/choice.ts".to_owned()]);
        let mut imports = vec![
            ImportRecord {
                raw: "./choice".to_owned(),
                kind: ImportKind::Import,
                resolved_path: None,
                resolution: ImportResolution::Unresolved("not_indexed".to_owned()),
                context: ImportContext::None,
            },
            ImportRecord {
                raw: "external-package".to_owned(),
                kind: ImportKind::Import,
                resolved_path: None,
                resolution: ImportResolution::Unresolved("not_indexed".to_owned()),
                context: ImportContext::None,
            },
        ];

        resolve_repository_imports(
            "src/app.ts",
            &mut imports,
            &known,
            &RustWorkspace::default(),
            &GoModules::default(),
            &DeclaredPackages::default(),
        );

        let mut builtins = vec![ImportRecord {
            raw: "node:fs/promises".to_owned(),
            kind: ImportKind::Import,
            resolved_path: None,
            resolution: ImportResolution::Unresolved("not_indexed".to_owned()),
            context: ImportContext::None,
        }];
        resolve_repository_imports(
            "src/app.ts",
            &mut builtins,
            &known,
            &RustWorkspace::default(),
            &GoModules::default(),
            &DeclaredPackages::default(),
        );
        assert_eq!(
            builtins[0].resolution,
            ImportResolution::Unresolved("external".to_owned()),
            "a node: specifier always names a runtime built-in"
        );

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
