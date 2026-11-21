use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs::{self, File, Metadata};
use std::io::{self, BufReader, Read};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use compresso_store::{RepoId, Store, StoreError};
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use ignore::{DirEntry, WalkBuilder, WalkState};

use crate::concurrency::Concurrency;

/// Safe scanner limit used by [`scan`] and by the slice configuration default.
pub const DEFAULT_MAX_FILE_BYTES: u64 = 1_000_000;
const BINARY_SNIFF_BYTES: u64 = 8 * 1024;
const HASH_BUFFER_BYTES: usize = 64 * 1024;
/// Upper bound on the buffers one inspection worker holds while hashing a file.
const INSPECTION_WORKER_BYTES: u64 = BINARY_SNIFF_BYTES * 2 + HASH_BUFFER_BYTES as u64;
/// Below this many files a parallel inspection costs more than it saves.
const PARALLEL_INSPECTION_MIN_FILES: usize = 16;

/// Per-scan limits that can be supplied by configuration and tests.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScanOptions {
    pub max_file_bytes: u64,
    /// Whether a language's own larger ceiling (SQL, E17-42) applies. A
    /// ceiling the user configured is [`ScanOptions::configured`]: it caps
    /// every language.
    pub language_ceilings: bool,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            max_file_bytes: DEFAULT_MAX_FILE_BYTES,
            language_ceilings: true,
        }
    }
}

impl ScanOptions {
    /// A ceiling the user configured: it applies to every file, SQL too.
    pub const fn configured(max_file_bytes: u64) -> Self {
        Self {
            max_file_bytes,
            language_ceilings: false,
        }
    }

    /// The size ceiling for one repository path: a language's own larger
    /// ceiling (SQL, E17-42) unless the ceiling was configured.
    pub fn max_bytes_for(&self, relative: &str) -> u64 {
        if !self.language_ceilings {
            return self.max_file_bytes;
        }
        relative
            .rsplit_once('.')
            .and_then(|(_, extension)| crate::parsers::languages::for_extension(extension))
            .map_or(self.max_file_bytes, |spec| {
                spec.max_file_bytes.max(self.max_file_bytes)
            })
    }
}

/// A deterministic difference between the filesystem and the stored file ledger.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScanResult {
    pub added: Vec<PathBuf>,
    pub changed: Vec<PathBuf>,
    pub removed: Vec<String>,
    pub skipped: Vec<(PathBuf, SkipReason)>,
    /// The repository's own `info/exclude` exists, or its `.git` pointer
    /// names one, but it was not applied in full (unsafe or unreadable path,
    /// over the size bound, or an unparsable pattern). A flag only: no path or
    /// content from the pointer is carried.
    pub git_exclude_unusable: bool,
}

/// Why an on-disk entry was intentionally excluded from candidate hashing.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum SkipReason {
    Ignored,
    SecretPattern,
    Binary,
    TooLarge,
    Symlink,
    UnsupportedFileType,
}

/// Atlas-level failures. Future Atlas modules share the path-boundary variant.
#[derive(Debug, thiserror::Error)]
pub enum AtlasError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Scan(#[from] ScanError),
    #[error(transparent)]
    Index(#[from] crate::indexer::IndexError),
    #[error(transparent)]
    Query(#[from] crate::queries::QueryError),
    #[error(transparent)]
    Grep(#[from] crate::grep::GrepError),
    #[error("file {path:?} is not indexed in repository {repo}")]
    FileNotIndexed { repo: RepoId, path: String },
    #[error("path {path:?} resolves outside approved repository root {root:?}")]
    OutsideRoot { path: PathBuf, root: PathBuf },
}

/// Typed repository traversal and content-inspection failures.
#[derive(Debug, thiserror::Error)]
pub enum ScanError {
    #[error("failed to canonicalize repository scan root {path:?}")]
    CanonicalizeRoot {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("repository scan root {path:?} is not a directory")]
    RootNotDirectory { path: PathBuf },
    #[error("repository {repo} has not approved scan root {path:?}")]
    RepositoryNotApproved { repo: RepoId, path: PathBuf },
    #[error(
        "scan root {path:?} belongs to repository {actual}, not requested repository {requested}"
    )]
    RepositoryMismatch {
        requested: RepoId,
        actual: RepoId,
        path: PathBuf,
    },
    #[error("repository walk failed at {path:?}")]
    Walk {
        path: Option<PathBuf>,
        #[source]
        source: ignore::Error,
    },
    #[error("repository ignore matcher became unavailable")]
    IgnoreMatcherUnavailable,
    #[error("failed to stat repository entry {path:?}")]
    Metadata {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("repository path is not valid UTF-8: {path:?}")]
    NonUtf8Path { path: PathBuf },
    #[error("repository path is not a safe relative forward-slash path: {path:?}")]
    UnsafeRelativePath { path: PathBuf },
    #[error("repository entry changed type while it was being scanned: {path:?}")]
    EntryChanged { path: PathBuf },
    #[error("failed to read repository file {path:?}")]
    ReadFile {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// Scans with the documented one-million-byte safe default.
pub fn scan(store: &Store, repo: RepoId, root: &Path) -> Result<ScanResult, AtlasError> {
    scan_with_options(store, repo, root, ScanOptions::default())
}

/// Scans an approved repository using caller-supplied file-size limits.
pub fn scan_with_options(
    store: &Store,
    repo: RepoId,
    root: &Path,
    options: ScanOptions,
) -> Result<ScanResult, AtlasError> {
    scan_with_concurrency(store, repo, root, options, Concurrency::default())
}

/// Scans with explicit worker and memory bounds.
///
/// The result is identical for every [`Concurrency`]: the walk and file
/// inspection fan out, while classification, error selection, and output
/// ordering stay keyed by repository-relative path.
pub fn scan_with_concurrency(
    store: &Store,
    repo: RepoId,
    root: &Path,
    options: ScanOptions,
    concurrency: Concurrency,
) -> Result<ScanResult, AtlasError> {
    scan_impl(store, repo, root, options, concurrency, None, &inspect_file)
}

fn scan_impl<I>(
    store: &Store,
    repo: RepoId,
    root: &Path,
    options: ScanOptions,
    concurrency: Concurrency,
    instrumentation: Option<WalkInstrumentation>,
    inspector: &I,
) -> Result<ScanResult, AtlasError>
where
    I: Fn(&Path, u64) -> io::Result<FileInspection> + Sync,
{
    let canonical_root = fs::canonicalize(root).map_err(|source| ScanError::CanonicalizeRoot {
        path: root.to_path_buf(),
        source,
    })?;
    if !canonical_root.is_dir() {
        return Err(ScanError::RootNotDirectory {
            path: canonical_root,
        }
        .into());
    }
    let Some(approved) = store.repo_by_root(&canonical_root)? else {
        return Err(ScanError::RepositoryNotApproved {
            repo,
            path: canonical_root,
        }
        .into());
    };
    if approved.id != repo {
        return Err(ScanError::RepositoryMismatch {
            requested: repo,
            actual: approved.id,
            path: canonical_root,
        }
        .into());
    }

    let stored_hashes = store.file_hashes(repo)?;
    let classification =
        metadata_classification_walk(&canonical_root, concurrency.jobs(), instrumentation)?;
    let git_exclude_unusable = classification.git_exclude_unusable;
    let mut candidate_hashes = BTreeMap::new();
    let mut skipped = classification
        .ignored
        .into_iter()
        .map(|path| (path, SkipReason::Ignored))
        .collect::<Vec<_>>();

    // Path, type, and size exclusions are decided here, before any worker can
    // open a file, so excluded entries are never read at any concurrency.
    let mut pending = Vec::new();
    for (relative, entry) in classification.discovered {
        let relative_path = PathBuf::from(&relative);
        let reason = if is_secret_pattern(&relative) {
            Some(SkipReason::SecretPattern)
        } else if entry.metadata.file_type().is_symlink() {
            Some(SkipReason::Symlink)
        } else if !entry.metadata.is_file() || is_edit_temporary(&relative) {
            // A crash can leave an edit's temporary file behind; it is
            // never repository content.
            Some(SkipReason::UnsupportedFileType)
        } else if entry.metadata.len() > options.max_bytes_for(&relative) {
            Some(SkipReason::TooLarge)
        } else {
            None
        };
        if let Some(reason) = reason {
            skipped.push((relative_path, reason));
            continue;
        }

        pending.push((relative, entry.absolute));
    }

    let workers = if pending.len() < PARALLEL_INSPECTION_MIN_FILES {
        1
    } else {
        concurrency.workers_for(pending.len(), INSPECTION_WORKER_BYTES)
    };
    let outcomes = inspect_candidates(&canonical_root, &pending, options, workers, inspector);
    for ((relative, absolute), outcome) in pending.into_iter().zip(outcomes) {
        let inspection = match outcome {
            Some(inspection) => inspection,
            None => inspect_candidate(
                &canonical_root,
                &absolute,
                options.max_bytes_for(&relative),
                inspector,
            ),
        }?;
        match inspection {
            FileInspection::Binary => skipped.push((PathBuf::from(relative), SkipReason::Binary)),
            FileInspection::TooLarge => {
                skipped.push((PathBuf::from(relative), SkipReason::TooLarge));
            }
            FileInspection::Hash(hash) => {
                candidate_hashes.insert(relative, hash);
            }
        }
    }

    let mut added = Vec::new();
    let mut changed = Vec::new();
    for (path, hash) in &candidate_hashes {
        match stored_hashes.get(path) {
            None => added.push(PathBuf::from(path)),
            Some(stored) if stored != hash => changed.push(PathBuf::from(path)),
            Some(_) => {}
        }
    }
    let mut removed = stored_hashes
        .keys()
        .filter(|path| !candidate_hashes.contains_key(*path))
        .cloned()
        .collect::<Vec<_>>();
    removed.sort_unstable();
    removed.dedup();
    skipped.sort_by(|left, right| left.0.cmp(&right.0).then(left.1.cmp(&right.1)));
    skipped.dedup();

    Ok(ScanResult {
        added,
        changed,
        removed,
        skipped,
        git_exclude_unusable,
    })
}

/// Confirms one candidate still resolves to itself inside the root, then inspects it.
fn inspect_candidate<I>(
    canonical_root: &Path,
    absolute: &Path,
    max_file_bytes: u64,
    inspector: &I,
) -> Result<FileInspection, AtlasError>
where
    I: Fn(&Path, u64) -> io::Result<FileInspection>,
{
    let resolved = fs::canonicalize(absolute).map_err(|source| ScanError::ReadFile {
        path: absolute.to_path_buf(),
        source,
    })?;
    if !resolved.starts_with(canonical_root) {
        return Err(AtlasError::OutsideRoot {
            path: resolved,
            root: canonical_root.to_path_buf(),
        });
    }
    if resolved != absolute {
        return Err(ScanError::EntryChanged {
            path: absolute.to_path_buf(),
        }
        .into());
    }
    inspector(&resolved, max_file_bytes).map_err(|source| {
        ScanError::ReadFile {
            path: resolved.clone(),
            source,
        }
        .into()
    })
}

/// Inspects candidates in path order, returning one slot per candidate.
///
/// Every slot up to and including the lowest-ordered failure is filled, so a
/// caller that consumes the slots in order observes exactly the serial outcome.
/// Slots after a failure may be `None` because that work is abandoned.
fn inspect_candidates<I>(
    canonical_root: &Path,
    pending: &[(String, PathBuf)],
    options: ScanOptions,
    workers: usize,
    inspector: &I,
) -> Vec<Option<Result<FileInspection, AtlasError>>>
where
    I: Fn(&Path, u64) -> io::Result<FileInspection> + Sync,
{
    let mut outcomes = Vec::new();
    outcomes.resize_with(pending.len(), || None);
    if workers <= 1 {
        for (slot, (relative, absolute)) in outcomes.iter_mut().zip(pending) {
            let outcome = inspect_candidate(
                canonical_root,
                absolute,
                options.max_bytes_for(relative),
                inspector,
            );
            let failed = outcome.is_err();
            *slot = Some(outcome);
            if failed {
                break;
            }
        }
        return outcomes;
    }

    let next = AtomicUsize::new(0);
    let first_failure = AtomicUsize::new(usize::MAX);
    let batches = thread::scope(|scope| {
        let handles = (0..workers)
            .map(|_| {
                scope.spawn(|| {
                    let mut batch = Vec::new();
                    loop {
                        let index = next.fetch_add(1, Ordering::Relaxed);
                        // Indices are claimed in order, so everything below a
                        // recorded failure is already owned by some worker.
                        if index >= pending.len() || index > first_failure.load(Ordering::Relaxed) {
                            break;
                        }
                        let outcome = inspect_candidate(
                            canonical_root,
                            &pending[index].1,
                            options.max_bytes_for(&pending[index].0),
                            inspector,
                        );
                        if outcome.is_err() {
                            first_failure.fetch_min(index, Ordering::Relaxed);
                        }
                        batch.push((index, outcome));
                    }
                    batch
                })
            })
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .map(|handle| {
                handle
                    .join()
                    .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
            })
            .collect::<Vec<_>>()
    });
    for (index, outcome) in batches.into_iter().flatten() {
        outcomes[index] = Some(outcome);
    }
    outcomes
}

struct ClassifiedEntry {
    absolute: PathBuf,
    metadata: Metadata,
}

struct ClassificationWalk {
    discovered: BTreeMap<String, ClassifiedEntry>,
    ignored: Vec<PathBuf>,
    git_exclude_unusable: bool,
}

fn metadata_classification_walk(
    root: &Path,
    workers: usize,
    instrumentation: Option<WalkInstrumentation>,
) -> Result<ClassificationWalk, ScanError> {
    let mut matcher_builder = WalkBuilder::new(root);
    configure_ignore_filters(&mut matcher_builder, true);
    let Some(matcher) = matcher_builder.build_matchers().into_iter().next() else {
        return Err(ScanError::IgnoreMatcherUnavailable);
    };
    let exclusions = ScannerExclusions::load(root);
    let git_exclude_unusable = exclusions.git_exclude_unusable();
    let matcher = Arc::new(Mutex::new(matcher));
    let ignored = Arc::new(Mutex::new(BTreeSet::<PathBuf>::new()));
    let matcher_errors = Arc::new(Mutex::new(Vec::<ignore::Error>::new()));
    let matcher_unavailable = Arc::new(AtomicBool::new(false));

    let mut builder = WalkBuilder::new(root);
    configure_ignore_filters(&mut builder, false);
    let root_for_filter = root.to_path_buf();
    let matcher_for_filter = Arc::clone(&matcher);
    let ignored_for_filter = Arc::clone(&ignored);
    let errors_for_filter = Arc::clone(&matcher_errors);
    let unavailable_for_filter = Arc::clone(&matcher_unavailable);
    builder.filter_entry(move |entry| {
        if let Some(instrumentation) = &instrumentation {
            instrumentation.record(entry.path());
        }
        if entry.depth() == 0 {
            return true;
        }
        if is_builtin_excluded_directory(entry) {
            if let Ok(mut ignored) = ignored_for_filter.lock() {
                ignored.insert(entry.path().to_path_buf());
            } else {
                unavailable_for_filter.store(true, Ordering::Relaxed);
            }
            return false;
        }
        let Some(file_type) = entry.file_type() else {
            return true;
        };
        let Ok(relative) = entry.path().strip_prefix(&root_for_filter) else {
            unavailable_for_filter.store(true, Ordering::Relaxed);
            return false;
        };
        if !file_type.is_dir() && (file_type.is_symlink() || is_secret_path(relative)) {
            return true;
        }
        let matched = match matcher_for_filter.lock() {
            Ok(mut matcher) => {
                let (matched, error) = matcher.matched_with_errors(relative, file_type.is_dir());
                if let Some(error) = error {
                    if let Ok(mut errors) = errors_for_filter.lock() {
                        errors.push(error);
                    } else {
                        unavailable_for_filter.store(true, Ordering::Relaxed);
                    }
                }
                matched
            }
            Err(_) => {
                unavailable_for_filter.store(true, Ordering::Relaxed);
                return false;
            }
        };
        // `.gitignore` files outrank the repository's `info/exclude`, as in git.
        let excluded = matched.is_ignore()
            || (matched.is_none() && exclusions.exclude_matches(relative, file_type.is_dir()))
            || (file_type.is_dir() && is_nested_linked_worktree(entry.path()));
        if excluded {
            if let Ok(mut ignored) = ignored_for_filter.lock() {
                ignored.insert(entry.path().to_path_buf());
            } else {
                unavailable_for_filter.store(true, Ordering::Relaxed);
            }
            return false;
        }
        true
    });

    let discovered = if workers <= 1 {
        let mut discovered = BTreeMap::new();
        for result in builder.build() {
            if let Some((relative, entry)) = classify_walk_entry(root, result)? {
                discovered.insert(relative, entry);
            }
        }
        discovered
    } else {
        parallel_classification_walk(root, &mut builder, workers)?
    };
    if matcher_unavailable.load(Ordering::Relaxed) {
        return Err(ScanError::IgnoreMatcherUnavailable);
    }
    if let Some(error) = matcher_errors
        .lock()
        .map_err(|_| ScanError::IgnoreMatcherUnavailable)?
        .pop()
    {
        return Err(walk_error(error));
    }
    let ignored = ignored
        .lock()
        .map_err(|_| ScanError::IgnoreMatcherUnavailable)?
        .iter()
        .map(|path| relative_path(root, path).map(PathBuf::from))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ClassificationWalk {
        discovered,
        ignored,
        git_exclude_unusable,
    })
}

/// Converts one walk result into a discovered non-directory entry, if any.
fn classify_walk_entry(
    root: &Path,
    result: Result<DirEntry, ignore::Error>,
) -> Result<Option<(String, ClassifiedEntry)>, ScanError> {
    let entry = result.map_err(walk_error)?;
    if entry.depth() == 0 || entry.file_type().is_some_and(|kind| kind.is_dir()) {
        return Ok(None);
    }
    let relative = relative_path(root, entry.path())?;
    let metadata = fs::symlink_metadata(entry.path()).map_err(|source| ScanError::Metadata {
        path: entry.path().to_path_buf(),
        source,
    })?;
    Ok(Some((
        relative,
        ClassifiedEntry {
            absolute: entry.path().to_path_buf(),
            metadata,
        },
    )))
}

/// Walks with several threads and merges entries into path order.
///
/// Workers finish the whole walk before a failure is chosen, and the failure
/// with the lowest path wins, so the reported error does not depend on timing.
fn parallel_classification_walk(
    root: &Path,
    builder: &mut WalkBuilder,
    workers: usize,
) -> Result<BTreeMap<String, ClassifiedEntry>, ScanError> {
    let discovered = Mutex::new(BTreeMap::new());
    let failures = Mutex::new(Vec::<(Option<PathBuf>, ScanError)>::new());
    let poisoned = AtomicBool::new(false);
    builder.threads(workers).build_parallel().run(|| {
        Box::new(|result| {
            let entry_path = result.as_ref().ok().map(|entry| entry.path().to_path_buf());
            match classify_walk_entry(root, result) {
                Ok(Some((relative, entry))) => match discovered.lock() {
                    Ok(mut discovered) => {
                        discovered.insert(relative, entry);
                    }
                    Err(_) => poisoned.store(true, Ordering::Relaxed),
                },
                Ok(None) => {}
                Err(error) => {
                    let path = match &error {
                        ScanError::Walk { path, .. } => path.clone(),
                        _ => entry_path,
                    };
                    match failures.lock() {
                        Ok(mut failures) => failures.push((path, error)),
                        Err(_) => poisoned.store(true, Ordering::Relaxed),
                    }
                }
            }
            WalkState::Continue
        })
    });
    if poisoned.load(Ordering::Relaxed) {
        return Err(ScanError::IgnoreMatcherUnavailable);
    }
    let mut failures = failures
        .into_inner()
        .map_err(|_| ScanError::IgnoreMatcherUnavailable)?;
    failures.sort_by(|left, right| left.0.cmp(&right.0));
    if let Some((_, error)) = failures.into_iter().next() {
        return Err(error);
    }
    discovered
        .into_inner()
        .map_err(|_| ScanError::IgnoreMatcherUnavailable)
}

fn configure_ignore_filters(builder: &mut WalkBuilder, respect_gitignore: bool) {
    builder
        .standard_filters(false)
        .hidden(false)
        .parents(false)
        .ignore(false)
        .git_ignore(respect_gitignore)
        .git_global(false)
        .git_exclude(false)
        .require_git(false)
        .follow_links(false)
        .sort_by_file_path(Path::cmp);
}

/// Whether the scanner's own exclusion rules skip `relative` under `root`.
///
/// This applies the same policy a scan does to one path, without walking the
/// tree: a secret-pattern name, a built-in excluded directory or `.git`
/// entry (any case), a match in a `.gitignore` inside the repository or in
/// the repository's own `info/exclude`, or a directory that is a nested
/// linked worktree. It judges current rules, not the index. The edit tool
/// refuses such a path even while a stale index row still names it; the read
/// tools serve any file that has an index row (see `compresso_read`). Like
/// the scanner, it ignores global and user ignore rules and ignore files
/// above the root. A caller judging many paths loads [`ScannerExclusions`]
/// once instead.
pub fn is_scanner_excluded(root: &Path, relative: &Path) -> bool {
    ScannerExclusions::load(root).excludes(root, relative)
}

/// The scanner's exclusion policy for one repository, loaded once and shared
/// by every path an operation judges.
pub struct ScannerExclusions {
    exclude: Gitignore,
    git_exclude_unusable: bool,
}

impl ScannerExclusions {
    /// Loads the repository's `info/exclude` rules for `root`, judged from
    /// its canonical form (the form a scan uses).
    pub fn load(root: &Path) -> Self {
        let real_root = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        let (exclude, git_exclude_unusable) = repository_exclude(&real_root);
        Self {
            exclude,
            git_exclude_unusable,
        }
    }

    /// Whether the scanner skips `relative` under `root` (see
    /// [`is_scanner_excluded`]).
    pub fn excludes(&self, root: &Path, relative: &Path) -> bool {
        if self.excluded_by_policy(root, relative) {
            return true;
        }
        // The request may reach an excluded file through a symbolic link or,
        // on a case-insensitive file system, different casing. The resolved
        // path is the one the scanner would have judged.
        let (Ok(real_root), Ok(real_path)) = (
            fs::canonicalize(root),
            fs::canonicalize(root.join(relative)),
        ) else {
            return false;
        };
        real_path
            .strip_prefix(&real_root)
            .is_ok_and(|real_relative| self.excluded_by_policy(&real_root, real_relative))
    }

    /// The repository's `info/exclude` exists, or its `.git` pointer names
    /// one, but it was not applied in full.
    pub fn git_exclude_unusable(&self) -> bool {
        self.git_exclude_unusable
    }

    fn exclude_matches(&self, relative: &Path, is_directory: bool) -> bool {
        self.exclude.matched(relative, is_directory).is_ignore()
    }

    fn excluded_by_policy(&self, root: &Path, relative: &Path) -> bool {
        if is_secret_path(relative) || relative.file_name().is_some_and(is_git_name) {
            return true;
        }
        let Some(parent) = relative.parent() else {
            return false;
        };
        if parent
            .components()
            .any(|component| is_builtin_excluded_name(component.as_os_str()))
        {
            return true;
        }
        // Git never re-includes a path below an excluded directory, so every
        // containing directory is judged first, outermost first.
        let mut directories = parent
            .ancestors()
            .filter(|directory| !directory.as_os_str().is_empty())
            .collect::<Vec<_>>();
        directories.reverse();
        directories.into_iter().any(|directory| {
            self.is_ignored(root, directory, true)
                || is_nested_linked_worktree(&root.join(directory))
        }) || self.is_ignored(root, relative, false)
    }

    /// Whether the `.gitignore` files above `relative` or, when none of them
    /// decides, the repository's `info/exclude` ignore it.
    fn is_ignored(&self, root: &Path, relative: &Path, is_directory: bool) -> bool {
        gitignore_decision(root, relative, is_directory)
            .unwrap_or_else(|| self.exclude_matches(relative, is_directory))
    }
}

#[cfg(test)]
thread_local! {
    static EXCLUDE_LOADS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn exclude_loads_on_this_thread() -> usize {
    EXCLUDE_LOADS.with(std::cell::Cell::get)
}

/// The `.gitignore` decision for `relative`: `Some(true)` ignored,
/// `Some(false)` re-included, `None` when no rule matches. The deepest file
/// with a matching rule decides, so a nested file can re-include a path an
/// outer one ignores.
fn gitignore_decision(root: &Path, relative: &Path, is_directory: bool) -> Option<bool> {
    for directory in relative.ancestors().skip(1) {
        let ignore_file = root.join(directory).join(".gitignore");
        if !ignore_file.is_file() {
            continue;
        }
        let (matcher, _) = Gitignore::new(&ignore_file);
        let Ok(within) = relative.strip_prefix(directory) else {
            continue;
        };
        let matched = matcher.matched(within, is_directory);
        if matched.is_ignore() {
            return Some(true);
        }
        if matched.is_whitelist() {
            return Some(false);
        }
    }
    None
}

/// `.git` in any case: a `.GIT` entry is git metadata on a case-insensitive
/// file system and never repository content anywhere.
fn is_git_name(name: &OsStr) -> bool {
    name.to_str()
        .is_some_and(|name| name.eq_ignore_ascii_case(".git"))
}

fn is_builtin_excluded_name(name: &OsStr) -> bool {
    name == OsStr::new("node_modules")
        || name == OsStr::new("target")
        || name == OsStr::new("dist")
        || is_git_name(name)
}

/// A built-in excluded directory, or a `.git` entry of any type: git never
/// tracks a path named `.git`, and a `.git` file only points at git metadata.
fn is_builtin_excluded_directory(entry: &DirEntry) -> bool {
    is_git_name(entry.file_name())
        || (entry.file_type().is_some_and(|kind| kind.is_dir())
            && is_builtin_excluded_name(entry.file_name()))
}

/// Largest `.git` pointer or `commondir` file the scanner reads.
const GIT_POINTER_MAX_BYTES: u64 = 4 * 1024;
/// Largest `info/exclude` file the scanner reads; a larger one is not applied.
const GIT_EXCLUDE_MAX_BYTES: u64 = 1024 * 1024;

/// The rules of the indexed repository's own `info/exclude`, rooted at the
/// canonical `root`, and whether an exclude file git would read was not
/// applied in full.
///
/// For a `.git` directory the file is `<root>/.git/info/exclude`. For a
/// `.git` file (a linked worktree or a submodule) it follows the `gitdir:`
/// pointer and, when present, that directory's `commondir` pointer, as git
/// finds its common directory, with three limits: a pointer is resolved
/// lexically and must not name a UNC, verbatim, or device path; the
/// directory it names must lie at `<A>/.git/...` for `A` the root or one of
/// its ancestors, and is reached by joining those names onto `A`, never by
/// opening the pointer's own text; and no component from `A` down to the file
/// may be a symbolic link. Only the pointer files and the exclude file are
/// read, each opened without following links or blocking and checked on the
/// open handle to be a regular file within its bound. No global or user
/// configuration is read. Anything else means no rules.
fn repository_exclude(root: &Path) -> (Gitignore, bool) {
    #[cfg(test)]
    EXCLUDE_LOADS.with(|loads| loads.set(loads.get() + 1));
    let (anchor, mut components) = match exclude_location(root) {
        Location::NotARepository => return (Gitignore::empty(), false),
        Location::Unusable => return (Gitignore::empty(), true),
        Location::At { anchor, components } => (anchor, components),
    };
    components.extend(["info".to_owned(), "exclude".to_owned()]);
    let bytes = match read_contained(&anchor, &components, GIT_EXCLUDE_MAX_BYTES) {
        Contained::Missing => return (Gitignore::empty(), false),
        Contained::Unusable => return (Gitignore::empty(), true),
        Contained::Read(bytes) => bytes,
    };
    let mut builder = GitignoreBuilder::new(root);
    let mut unusable = false;
    let text = String::from_utf8_lossy(&bytes);
    for line in text.trim_start_matches('\u{feff}').lines() {
        // Git skips a pattern it cannot parse; the scanner does too, and
        // reports that it did.
        unusable |= builder.add_line(None, line).is_err();
    }
    match builder.build() {
        Ok(matcher) => (matcher, unusable),
        Err(_) => (Gitignore::empty(), true),
    }
}

enum Location {
    NotARepository,
    Unusable,
    /// The git directory holding `info/exclude` is `anchor` joined with
    /// `components` (the first of which is `.git`).
    At {
        anchor: PathBuf,
        components: Vec<String>,
    },
}

fn exclude_location(root: &Path) -> Location {
    match fs::symlink_metadata(root.join(".git")) {
        Err(_) => return Location::NotARepository,
        Ok(metadata) if metadata.is_dir() => {
            return Location::At {
                anchor: root.to_path_buf(),
                components: vec![".git".to_owned()],
            };
        }
        Ok(_) => {}
    }
    common_dir_from_pointer(root).map_or(Location::Unusable, |(anchor, components)| Location::At {
        anchor,
        components,
    })
}

/// The common git directory a root `.git` file leads to, as an anchor (the
/// root or one of its ancestors) and the names below it.
fn common_dir_from_pointer(root: &Path) -> Option<(PathBuf, Vec<String>)> {
    let root_key = path_key(root)?;
    let Contained::Read(bytes) = read_contained(root, &[".git"], GIT_POINTER_MAX_BYTES) else {
        return None;
    };
    let pointer = classify_pointer(&pointer_text(&bytes, Some("gitdir: "))?)?;
    let git_dir = resolve_key(&root_key, &pointer)?;
    let (anchor, components) = anchor_below_root_ancestor(root, &root_key, &git_dir)?;
    let mut commondir = components.clone();
    commondir.push("commondir".to_owned());
    match read_contained(&anchor, &commondir, GIT_POINTER_MAX_BYTES) {
        // A submodule or a separate git directory is its own common directory.
        Contained::Missing => Some((anchor, components)),
        Contained::Unusable => None,
        Contained::Read(bytes) => {
            let pointer = classify_pointer(&pointer_text(&bytes, None)?)?;
            let common = resolve_key(&git_dir, &pointer)?;
            anchor_below_root_ancestor(root, &root_key, &common)
        }
    }
}

/// Whether `directory` is the working tree of a linked worktree: its `.git`
/// is a regular file whose `gitdir:` pointer names `<repo>/.git/worktrees/<name>`.
///
/// The decision is lexical: the named directory is never opened. A missing,
/// oversized, or malformed pointer, and a submodule (whose git directory
/// lives under `.git/modules/`), are not linked worktrees.
fn is_nested_linked_worktree(directory: &Path) -> bool {
    let Contained::Read(bytes) = read_contained(directory, &[".git"], GIT_POINTER_MAX_BYTES) else {
        return false;
    };
    pointer_text(&bytes, Some("gitdir: ")).is_some_and(|target| is_worktree_pointer(&target))
}

fn is_worktree_pointer(target: &str) -> bool {
    let components = target
        .split(['/', '\\'])
        .filter(|component| !component.is_empty() && *component != ".")
        .collect::<Vec<_>>();
    let [.., dot_git, worktrees, name] = components.as_slice() else {
        return false;
    };
    *dot_git == ".git"
        && *worktrees == "worktrees"
        && *name != ".."
        && !components
            .windows(2)
            .any(|pair| pair[0] == ".git" && pair[1] == "modules")
}

/// The path a pointer file holds: git strips only trailing line endings, so
/// the scanner does too. More than one line, a NUL, or an empty path is no
/// pointer. With `prefix`, the text must start with it and it is removed.
fn pointer_text(bytes: &[u8], prefix: Option<&str>) -> Option<String> {
    let text = std::str::from_utf8(bytes)
        .ok()?
        .trim_end_matches(['\r', '\n']);
    if text.contains(['\r', '\n', '\0']) {
        return None;
    }
    let text = match prefix {
        Some(prefix) => text.strip_prefix(prefix)?,
        None => text,
    };
    (!text.is_empty()).then(|| text.to_owned())
}

/// A pointer target split lexically: an optional root (`/` or a drive such
/// as `C:`) and its names, `..` kept for resolution.
#[derive(Debug, Eq, PartialEq)]
struct Pointer {
    root: Option<String>,
    components: Vec<String>,
}

/// Splits a pointer target without touching the file system, on every
/// platform alike. A UNC (`//host/share`, `\\host\share`), verbatim (`\\?\`),
/// or device (`\\.\`) path, a drive-relative path (`C:x`), and a name with a
/// `:` are refused, so no pointer can lead to a network share or a device.
fn classify_pointer(target: &str) -> Option<Pointer> {
    let target = target.replace('\\', "/");
    if target.starts_with("//") {
        return None;
    }
    let bytes = target.as_bytes();
    let (root, rest) = if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        if bytes.get(2) != Some(&b'/') {
            return None;
        }
        (
            Some(format!("{}:", char::from(bytes[0].to_ascii_uppercase()))),
            &target[3..],
        )
    } else if let Some(rest) = target.strip_prefix('/') {
        (Some("/".to_owned()), rest)
    } else {
        (None, target.as_str())
    };
    let components = rest
        .split('/')
        .filter(|component| !component.is_empty() && *component != ".")
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if components.iter().any(|component| component.contains(':')) {
        return None;
    }
    Some(Pointer { root, components })
}

/// The lexical key of a real absolute path: its root (`/` or `C:`, for a
/// verbatim drive path too) and its names. Any other prefix has no key.
fn path_key(path: &Path) -> Option<Vec<String>> {
    use std::path::Prefix;

    let mut key = Vec::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => match prefix.kind() {
                Prefix::Disk(drive) | Prefix::VerbatimDisk(drive) => {
                    key.push(format!("{}:", char::from(drive.to_ascii_uppercase())));
                }
                _ => return None,
            },
            Component::RootDir => {
                if key.is_empty() {
                    key.push("/".to_owned());
                }
            }
            Component::CurDir => {}
            Component::ParentDir => return None,
            Component::Normal(name) => key.push(name.to_str()?.to_owned()),
        }
    }
    (!key.is_empty()).then_some(key)
}

/// Resolves a pointer against the key of the directory holding it.
fn resolve_key(base: &[String], pointer: &Pointer) -> Option<Vec<String>> {
    let mut resolved = match &pointer.root {
        Some(root) => vec![root.clone()],
        None => base.to_vec(),
    };
    for component in &pointer.components {
        if component == ".." {
            if resolved.len() <= 1 {
                return None;
            }
            resolved.pop();
        } else {
            resolved.push(component.clone());
        }
    }
    Some(resolved)
}

/// Places a resolved git directory at `<A>/.git/...` for `A` the root or the
/// nearest of its ancestors that fits, returning `A` as a real path and the
/// names below it. Anything else (another drive, a sibling tree) is refused.
fn anchor_below_root_ancestor(
    root: &Path,
    root_key: &[String],
    target: &[String],
) -> Option<(PathBuf, Vec<String>)> {
    for depth in (1..=root_key.len()).rev() {
        if target.len() > depth && target[..depth] == root_key[..depth] && target[depth] == ".git" {
            let mut anchor = root.to_path_buf();
            for _ in depth..root_key.len() {
                anchor.pop();
            }
            return Some((anchor, target[depth..].to_vec()));
        }
    }
    None
}

/// The outcome of reading one small git file below an anchor directory.
enum Contained {
    Missing,
    Unusable,
    Read(Vec<u8>),
}

/// Reads `anchor/<components...>` when no component is a symbolic link,
/// every directory is a directory, and the file is a regular file of at most
/// `max_bytes`. The file is opened without following a link or blocking (a
/// FIFO or device is refused on the open handle).
fn read_contained<S: AsRef<str>>(anchor: &Path, components: &[S], max_bytes: u64) -> Contained {
    let Some((file_name, directories)) = components.split_last() else {
        return Contained::Unusable;
    };
    let not_found = |error: &io::Error| error.kind() == io::ErrorKind::NotFound;
    let mut path = anchor.to_path_buf();
    for directory in directories {
        path.push(directory.as_ref());
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_dir() => {}
            Err(error) if not_found(&error) => return Contained::Missing,
            _ => return Contained::Unusable,
        }
    }
    path.push(file_name.as_ref());
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.is_file() && metadata.len() <= max_bytes => {}
        Err(error) if not_found(&error) => return Contained::Missing,
        _ => return Contained::Unusable,
    }
    let file = match open_no_follow(&path) {
        Ok(file) => file,
        Err(error) if not_found(&error) => return Contained::Missing,
        Err(_) => return Contained::Unusable,
    };
    match file.metadata() {
        Ok(metadata) if metadata.is_file() && metadata.len() <= max_bytes => {}
        _ => return Contained::Unusable,
    }
    let mut bytes = Vec::new();
    if file.take(max_bytes + 1).read_to_end(&mut bytes).is_err() || bytes.len() as u64 > max_bytes {
        return Contained::Unusable;
    }
    Contained::Read(bytes)
}

/// Opens for reading without following a final symbolic link (or reparse
/// point) and, on Unix, without blocking on a FIFO.
fn open_no_follow(path: &Path) -> io::Result<File> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // FILE_FLAG_OPEN_REPARSE_POINT
        options.custom_flags(0x0020_0000);
    }
    options.open(path)
}

/// Whether `relative` names a temporary file the edit tool writes beside an
/// edited file (`.<name>.compresso-edit-<role>-<pid>-<n>.tmp`).
pub(crate) fn is_edit_temporary(relative: &str) -> bool {
    let name = relative.rsplit('/').next().unwrap_or(relative);
    name.starts_with('.') && name.contains(".compresso-edit-") && name.ends_with(".tmp")
}

fn is_secret_pattern(relative: &str) -> bool {
    is_secret_path(Path::new(relative))
}

pub(crate) fn is_secret_path(relative: &Path) -> bool {
    if relative
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(name) => Some(name),
            _ => None,
        })
        .any(is_secret_name)
    {
        return true;
    }

    let normalized = relative
        .to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase();
    normalized.ends_with(".cargo/credentials")
        || normalized.ends_with(".cargo/credentials.toml")
        || normalized.ends_with(".aws/credentials")
        || normalized.ends_with(".docker/config.json")
        || normalized.ends_with(".kube/config")
}

fn is_secret_name(name: &OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return true;
    };
    let lower = name.to_ascii_lowercase();

    lower.starts_with(".env")
        || lower.ends_with(".pem")
        || lower.starts_with("id_rsa")
        || matches!(
            lower.as_str(),
            ".npmrc"
                | ".pypirc"
                | ".netrc"
                | ".git-credentials"
                | "id_ed25519"
                | "id_ecdsa"
                | "id_dsa"
                | "kubeconfig"
                | "application_default_credentials.json"
        )
        || lower.ends_with(".key")
        || lower.ends_with(".p12")
        || lower.ends_with(".pfx")
        || is_terraform_state_name(&lower)
        || is_service_account_json_name(&lower)
}

fn is_terraform_state_name(name: &str) -> bool {
    name.ends_with(".tfstate")
        || name.ends_with(".tfstate.backup")
        || (name.contains(".tfstate.") && name.ends_with(".backup"))
}

fn is_service_account_json_name(name: &str) -> bool {
    let Some(stem) = name.strip_suffix(".json") else {
        return false;
    };
    matches!(
        stem,
        "service-account"
            | "service_account"
            | "service-account-key"
            | "service_account_key"
            | "service-account-credentials"
            | "service_account_credentials"
    ) || stem.ends_with("-service-account")
        || stem.ends_with("_service_account")
        || stem.ends_with("-service-account-key")
        || stem.ends_with("_service_account_key")
        || stem.ends_with("-service-account-credentials")
        || stem.ends_with("_service_account_credentials")
}

fn relative_path(root: &Path, path: &Path) -> Result<String, ScanError> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| ScanError::UnsafeRelativePath {
            path: path.to_path_buf(),
        })?;
    let mut components = Vec::new();
    for component in relative.components() {
        match component {
            Component::Normal(value) => {
                let value = value.to_str().ok_or_else(|| ScanError::NonUtf8Path {
                    path: path.to_path_buf(),
                })?;
                if value.is_empty() || value.contains(['/', '\\', '\0']) {
                    return Err(ScanError::UnsafeRelativePath {
                        path: path.to_path_buf(),
                    });
                }
                components.push(value);
            }
            Component::CurDir
            | Component::ParentDir
            | Component::RootDir
            | Component::Prefix(_) => {
                return Err(ScanError::UnsafeRelativePath {
                    path: path.to_path_buf(),
                });
            }
        }
    }
    if components.is_empty() {
        return Err(ScanError::UnsafeRelativePath {
            path: path.to_path_buf(),
        });
    }
    Ok(components.join("/"))
}

fn walk_error(source: ignore::Error) -> ScanError {
    let path = ignore_error_path(&source);
    ScanError::Walk { path, source }
}

fn ignore_error_path(error: &ignore::Error) -> Option<PathBuf> {
    match error {
        ignore::Error::Partial(errors) => errors.iter().find_map(ignore_error_path),
        ignore::Error::WithLineNumber { err, .. } | ignore::Error::WithDepth { err, .. } => {
            ignore_error_path(err)
        }
        ignore::Error::WithPath { path, .. } => Some(path.clone()),
        ignore::Error::Loop { child, .. } => Some(child.clone()),
        ignore::Error::Io(_)
        | ignore::Error::Glob { .. }
        | ignore::Error::UnrecognizedFileType(_)
        | ignore::Error::InvalidDefinition => None,
    }
}

enum FileInspection {
    Binary,
    TooLarge,
    Hash(String),
}

fn inspect_file(path: &Path, max_file_bytes: u64) -> io::Result<FileInspection> {
    let file = File::open(path)?;
    if file.metadata()?.len() > max_file_bytes {
        return Ok(FileInspection::TooLarge);
    }
    let mut reader = BufReader::new(file);
    let mut prefix = Vec::with_capacity(BINARY_SNIFF_BYTES as usize);
    reader
        .by_ref()
        .take(BINARY_SNIFF_BYTES)
        .read_to_end(&mut prefix)?;
    if prefix.contains(&0) {
        return Ok(FileInspection::Binary);
    }

    let mut hasher = blake3::Hasher::new();
    hasher.update(&prefix);
    let mut total = prefix.len() as u64;
    let mut buffer = [0_u8; HASH_BUFFER_BYTES];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        total = total.saturating_add(read as u64);
        if total > max_file_bytes {
            return Ok(FileInspection::TooLarge);
        }
        hasher.update(&buffer[..read]);
    }
    Ok(FileInspection::Hash(hasher.finalize().to_hex().to_string()))
}

#[derive(Clone, Default)]
struct WalkInstrumentation {
    paths: Arc<Mutex<Vec<PathBuf>>>,
}

impl WalkInstrumentation {
    fn record(&self, path: &Path) {
        if let Ok(mut paths) = self.paths.lock() {
            paths.push(path.to_path_buf());
        }
    }

    #[cfg(test)]
    fn paths(&self) -> Vec<PathBuf> {
        self.paths
            .lock()
            .map(|paths| paths.clone())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs::{self, File};
    use std::io;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};

    use compresso_store::{FileRecord, RepoId, Store};
    use tempfile::TempDir;

    use super::*;

    const EMPTY_BLAKE3: &str = "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262";

    #[test]
    fn test_single_path_exclusion_matches_the_scan_policy() {
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let root = directory.path();
        fs::create_dir_all(root.join("nested/deep")).expect("directories should be created");
        fs::write(root.join(".gitignore"), "*.log\nprivate/\n").expect("ignore file written");
        fs::write(root.join("nested/.gitignore"), "!keep.log\n*.tmp\n")
            .expect("nested ignore file written");
        let excluded = |path: &str| is_scanner_excluded(root, Path::new(path));

        assert!(excluded("build.log"));
        assert!(excluded("private/notes.txt"));
        assert!(excluded("nested/deep/trace.log"));
        assert!(excluded("nested/deep/scratch.tmp"));
        assert!(excluded("node_modules/pkg/index.js"));
        assert!(excluded("crates/target/debug/out.rs"));
        assert!(excluded(".env"));
        assert!(!excluded("nested/keep.log"));
        assert!(!excluded("scratch.tmp"));
        assert!(!excluded("src/lib.rs"));
        assert!(!excluded("target"));
    }

    #[test]
    fn test_worktree_pointer_classification_is_lexical() {
        for linked in [
            "/home/u/repo/.git/worktrees/x",
            "../../.git/worktrees/x",
            "C:\\work\\repo\\.git\\worktrees\\x",
            "C:/work/repo/.git/worktrees/x/",
        ] {
            assert!(is_worktree_pointer(linked), "{linked}");
        }
        for other in [
            "../.git/modules/sub",
            "../.git/modules/a/.git/worktrees/b",
            "/repo/.git/worktrees",
            "/repo/.git/worktrees/..",
            "/repo/.git",
            "worktrees/x",
        ] {
            assert!(!is_worktree_pointer(other), "{other}");
        }
    }

    #[test]
    fn test_pointer_classification_refuses_network_and_device_paths() {
        let pointer = |root: Option<&str>, components: &[&str]| Pointer {
            root: root.map(str::to_owned),
            components: components.iter().map(|name| (*name).to_owned()).collect(),
        };
        for refused in [
            "//host/share/repo/.git",
            r"\\host\share\repo\.git",
            r"\/host/share",
            r"\\?\UNC\host\share\repo\.git",
            r"\\?\C:\repo\.git",
            r"\\.\pipe\repo",
            r"\\.\C:\repo\.git",
            "C:relative/.git",
            "c:",
            "/repo/name:stream",
        ] {
            assert_eq!(classify_pointer(refused), None, "{refused}");
        }
        assert_eq!(
            classify_pointer("/home/u/repo/.git/worktrees/x"),
            Some(pointer(
                Some("/"),
                &["home", "u", "repo", ".git", "worktrees", "x"]
            ))
        );
        assert_eq!(
            classify_pointer(r"c:\Users\u\repo\.git"),
            Some(pointer(Some("C:"), &["Users", "u", "repo", ".git"]))
        );
        assert_eq!(
            classify_pointer("C:/Users/u/repo/./.git/"),
            Some(pointer(Some("C:"), &["Users", "u", "repo", ".git"]))
        );
        assert_eq!(
            classify_pointer(r"..\..\.git\worktrees\x"),
            Some(pointer(None, &["..", "..", ".git", "worktrees", "x"]))
        );
    }

    #[test]
    fn test_git_dirs_are_anchored_below_an_ancestor_of_the_root_only() {
        let key = |text: &str| text.split('/').map(str::to_owned).collect::<Vec<_>>();
        let rooted = |relative: &str| {
            let mut target = vec!["/".to_owned()];
            target.extend(key(relative));
            Some(target)
        };
        let root = Path::new("/w/main/trees/wt");
        let root_key = rooted("w/main/trees/wt").expect("key");
        let anchored = |target: Option<Vec<String>>| {
            target.and_then(|target| anchor_below_root_ancestor(root, &root_key, &target))
        };
        // A linked worktree inside the main checkout and a submodule.
        assert_eq!(
            anchored(rooted("w/main/.git/worktrees/wt")),
            Some((PathBuf::from("/w/main"), key(".git/worktrees/wt")))
        );
        assert_eq!(
            anchored(rooted("w/main/trees/wt/.git")),
            Some((PathBuf::from("/w/main/trees/wt"), key(".git")))
        );
        // A sibling checkout, another tree, another drive, and a `.git`
        // that is not the first name below an ancestor are refused.
        assert_eq!(anchored(rooted("w/other/.git/worktrees/wt")), None);
        assert_eq!(anchored(rooted("srv/repo/.git")), None);
        assert_eq!(anchored(rooted("w/main/sub/.git")), None);
        assert_eq!(
            anchored(Some(vec![
                "C:".to_owned(),
                "w".to_owned(),
                ".git".to_owned()
            ])),
            None
        );
        // `..` resolves lexically and cannot climb above the root marker.
        assert_eq!(
            resolve_key(
                &root_key,
                &classify_pointer("../../.git/worktrees/wt").expect("pointer")
            ),
            rooted("w/main/.git/worktrees/wt")
        );
        assert_eq!(
            resolve_key(
                &root_key,
                &classify_pointer("../../../../../x").expect("pointer")
            ),
            None
        );
    }

    #[test]
    fn test_oversized_or_unreadable_exclude_sources_fail_open() {
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let root = directory.path();
        fs::create_dir_all(root.join(".git/info")).expect("git dir");
        let mut oversized = "a.log\n".repeat(200_000);
        oversized.push_str("*.rs\n");
        fs::write(root.join(".git/info/exclude"), oversized).expect("exclude written");
        assert!(!is_scanner_excluded(root, Path::new("a.log")));

        // A worktree pointer whose commondir is oversized reads no exclude.
        let worktree = root.join("main/wt");
        fs::create_dir_all(root.join("main/.git/worktrees/wt")).expect("git dir");
        fs::create_dir_all(root.join("main/.git/info")).expect("info");
        fs::write(root.join("main/.git/info/exclude"), "*.log\n").expect("exclude");
        fs::write(
            root.join("main/.git/worktrees/wt/commondir"),
            "../".repeat(2_000),
        )
        .expect("commondir");
        fs::create_dir_all(&worktree).expect("worktree");
        fs::write(worktree.join(".git"), "gitdir: ../.git/worktrees/wt\n").expect("pointer");
        assert!(!is_scanner_excluded(&worktree, Path::new("a.log")));
        fs::write(root.join("main/.git/worktrees/wt/commondir"), "../..\n").expect("commondir");
        assert!(is_scanner_excluded(&worktree, Path::new("a.log")));
    }

    #[test]
    fn test_single_path_exclusion_cannot_be_bypassed() {
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let root = directory.path();
        fs::create_dir_all(root.join("private/inner")).expect("directories should be created");
        fs::create_dir_all(root.join("node_modules/pkg")).expect("directories should be created");
        fs::write(root.join(".gitignore"), "private/\n").expect("ignore file written");
        // Git never re-includes a file whose parent directory is excluded.
        fs::write(root.join("private/inner/.gitignore"), "!notes.txt\n")
            .expect("nested ignore file written");
        fs::write(root.join("private/inner/notes.txt"), "x").expect("file written");
        fs::write(root.join("node_modules/pkg/index.js"), "x").expect("file written");
        let excluded = |path: &str| is_scanner_excluded(root, Path::new(path));

        assert!(excluded("private/inner/notes.txt"));
        // A case-insensitive file system resolves a differently-cased request
        // to the excluded file; a case-sensitive one has no such file.
        for path in ["Node_Modules/pkg/index.js", "PRIVATE/inner/notes.txt"] {
            assert!(!root.join(path).exists() || excluded(path));
        }
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(root.join("private"), root.join("alias"))
                .expect("symlink should be created");
            assert!(excluded("alias/inner/notes.txt"));
        }
    }

    #[test]
    fn test_gitignore_respected() {
        let fixture = Fixture::new();
        let result =
            scan(&fixture.store, fixture.repo, &fixture.root).expect("scan should succeed");

        assert!(!result.added.contains(&PathBuf::from("ignored.log")));
        assert!(!result.added.contains(&PathBuf::from("nested/ignored.tmp")));
        let skipped = skipped_map(&result);
        assert_eq!(skipped.get("ignored.log"), Some(&SkipReason::Ignored));
        assert_eq!(
            skipped.get("nested/ignored.tmp"),
            Some(&SkipReason::Ignored)
        );
        assert!(result.added.contains(&PathBuf::from("nested/kept.rs")));
    }

    #[test]
    fn test_secret_patterns_skipped_without_opening() {
        let fixture = Fixture::new();
        let secret_paths = [
            ".env.local",
            ".ENV.production",
            ".env.vault/nested.rs",
            "server.pem",
            "id_rsa",
            "ID_RSA.backup",
            "ID_RSA.archive/nested.rs",
            ".npmrc",
            ".pypirc",
            ".netrc",
            ".git-credentials",
            ".ssh/id_ed25519",
            ".ssh/id_ecdsa",
            ".ssh/id_dsa",
            "certificates/client.key",
            "certificates/client.p12",
            "certificates/client.pfx",
            "infra/terraform.tfstate",
            "infra/production.tfstate.backup",
            ".cargo/credentials",
            ".cargo/credentials.toml",
            ".aws/credentials",
            ".docker/config.json",
            "deploy/kubeconfig",
            ".kube/config",
            ".config/gcloud/application_default_credentials.json",
            "deploy/production-service-account.json",
            "deploy/production_service_account_key.json",
        ];
        for relative in secret_paths {
            let absolute = fixture.root.join(relative);
            fs::create_dir_all(
                absolute
                    .parent()
                    .expect("secret fixture should have a parent directory"),
            )
            .expect("secret fixture parent should be created");
            fs::write(&absolute, "credential fixture\n").expect("secret fixture should be written");
        }
        let protected = secret_paths
            .iter()
            .map(|relative| {
                fs::canonicalize(fixture.root.join(relative))
                    .expect("secret fixture path should canonicalize")
            })
            .collect::<BTreeSet<_>>();
        let protected_for_scan = protected.clone();
        let inspected = Arc::new(Mutex::new(Vec::new()));
        let inspected_for_scan = Arc::clone(&inspected);
        let result = scan_impl(
            &fixture.store,
            fixture.repo,
            &fixture.root,
            ScanOptions::default(),
            Concurrency::default(),
            None,
            &move |path, max_file_bytes| {
                assert!(
                    !protected_for_scan.contains(path),
                    "secret artifact reached the file inspector: {path:?}"
                );
                inspected_for_scan
                    .lock()
                    .expect("inspection recorder should lock")
                    .push(path.to_path_buf());
                inspect_file(path, max_file_bytes)
            },
        )
        .expect("scan should succeed");

        let skipped = skipped_map(&result);
        for path in secret_paths {
            assert_eq!(skipped.get(path), Some(&SkipReason::SecretPattern));
            assert!(!result.added.contains(&PathBuf::from(path)));
        }
        let inspected = inspected.lock().expect("inspection recorder should lock");
        assert!(inspected.iter().all(|path| !protected.contains(path)));
    }

    #[test]
    fn test_secret_patterns_do_not_overmatch_source_like_names() {
        let fixture = Fixture::new();
        let allowed_paths = [
            "src/credentials.rs",
            "src/key.rs",
            "src/config.json",
            "docs/service-account-schema.json",
            "cargo/credentials.toml",
            "aws/credentials",
            "docker/config.json",
            ".docker/config.json.template",
        ];
        for relative in allowed_paths {
            let absolute = fixture.root.join(relative);
            fs::create_dir_all(
                absolute
                    .parent()
                    .expect("source-like fixture should have a parent directory"),
            )
            .expect("source-like fixture parent should be created");
            fs::write(&absolute, "non-secret source fixture\n")
                .expect("source-like fixture should be written");
        }

        let result =
            scan(&fixture.store, fixture.repo, &fixture.root).expect("scan should succeed");

        for relative in allowed_paths {
            assert!(
                result.added.contains(&PathBuf::from(relative)),
                "{relative}"
            );
            assert_ne!(
                skipped_map(&result).get(relative),
                Some(&SkipReason::SecretPattern),
                "{relative}"
            );
        }
    }

    #[test]
    fn test_binary_and_oversize_skipped_before_oversize_open() {
        let fixture = Fixture::new();
        let oversize = fixture.root.join("large.bin");
        File::create(&oversize)
            .expect("oversize fixture should be created")
            .set_len(3_000_000)
            .expect("oversize fixture length should be set");
        fs::write(
            fixture.root.join("image.png"),
            b"\x89PNG\r\n\x1a\n\0fixture",
        )
        .expect("binary fixture should be written");
        let inspected = Arc::new(Mutex::new(Vec::new()));
        let inspected_for_scan = Arc::clone(&inspected);

        let result = scan_impl(
            &fixture.store,
            fixture.repo,
            &fixture.root,
            ScanOptions::configured(1_000_000),
            Concurrency::default(),
            None,
            &move |path, max_file_bytes| {
                inspected_for_scan
                    .lock()
                    .expect("inspection recorder should lock")
                    .push(path.to_path_buf());
                inspect_file(path, max_file_bytes)
            },
        )
        .expect("scan should succeed");

        let skipped = skipped_map(&result);
        assert_eq!(skipped.get("large.bin"), Some(&SkipReason::TooLarge));
        assert_eq!(skipped.get("image.png"), Some(&SkipReason::Binary));
        let inspected = inspected.lock().expect("inspection recorder should lock");
        assert!(!inspected.iter().any(|path| path == &oversize));
    }

    #[test]
    fn test_changed_detected_by_hash() {
        let fixture = Fixture::new();
        seed_store(&fixture);
        fs::write(
            fixture.root.join("src/main.rs"),
            "fn main() { println!(\"one byte changed!\"); }\n",
        )
        .expect("source fixture should change");

        let result =
            scan(&fixture.store, fixture.repo, &fixture.root).expect("scan should succeed");
        assert_eq!(result.changed, [PathBuf::from("src/main.rs")]);
        assert!(result.added.is_empty());
        assert!(result.removed.is_empty());
    }

    #[test]
    fn test_removed_detected() {
        let fixture = Fixture::new();
        seed_store(&fixture);
        fs::remove_file(fixture.root.join("src/util.rs"))
            .expect("source fixture should be removed");

        let result =
            scan(&fixture.store, fixture.repo, &fixture.root).expect("scan should succeed");
        assert_eq!(result.removed, ["src/util.rs"]);
        assert!(result.added.is_empty());
        assert!(result.changed.is_empty());
    }

    #[test]
    fn test_previously_stored_skipped_path_is_removed_from_candidate_hashes() {
        let fixture = Fixture::new();
        fixture
            .store
            .upsert_file(
                fixture.repo,
                &FileRecord {
                    path: "ignored.log".to_owned(),
                    hash: "stale-hash".to_owned(),
                    language: None,
                    size_bytes: 1,
                    token_estimate: 0,
                    indexed_at: 1,
                },
            )
            .expect("legacy ignored path should be stored");

        let result =
            scan(&fixture.store, fixture.repo, &fixture.root).expect("scan should succeed");
        assert_eq!(result.removed, ["ignored.log"]);
        assert_eq!(
            skipped_map(&result).get("ignored.log"),
            Some(&SkipReason::Ignored)
        );
    }

    #[test]
    fn test_blake3_matches_reference() {
        let fixture = Fixture::new();
        fixture
            .store
            .upsert_file(
                fixture.repo,
                &FileRecord {
                    path: "src/empty.rs".to_owned(),
                    hash: EMPTY_BLAKE3.to_owned(),
                    language: None,
                    size_bytes: 0,
                    token_estimate: 0,
                    indexed_at: 1,
                },
            )
            .expect("reference hash should be stored");

        let result =
            scan(&fixture.store, fixture.repo, &fixture.root).expect("scan should succeed");
        assert!(!result.added.contains(&PathBuf::from("src/empty.rs")));
        assert!(!result.changed.contains(&PathBuf::from("src/empty.rs")));
    }

    #[test]
    fn test_builtin_directory_is_not_descended() {
        let fixture = Fixture::new();
        let excluded = fixture.root.join("node_modules/package");
        fs::create_dir_all(&excluded).expect("excluded directory should be created");
        fs::write(excluded.join("never-visit.js"), "module.exports = 1;\n")
            .expect("excluded fixture should be written");
        let visits = WalkInstrumentation::default();

        let result = scan_impl(
            &fixture.store,
            fixture.repo,
            &fixture.root,
            ScanOptions::default(),
            Concurrency::default(),
            Some(visits.clone()),
            &inspect_file,
        )
        .expect("scan should succeed");

        assert_eq!(
            skipped_map(&result).get("node_modules"),
            Some(&SkipReason::Ignored)
        );
        let visited = visits.paths();
        assert!(visited.iter().any(|path| path.ends_with("node_modules")));
        assert!(visited.iter().all(|path| !path.ends_with("never-visit.js")));
    }

    #[test]
    fn test_gitignored_directory_is_reported_but_not_descended() {
        let fixture = Fixture::new();
        let visits = WalkInstrumentation::default();

        let result = scan_impl(
            &fixture.store,
            fixture.repo,
            &fixture.root,
            ScanOptions::default(),
            Concurrency::default(),
            Some(visits.clone()),
            &inspect_file,
        )
        .expect("scan should succeed");

        assert_eq!(
            skipped_map(&result).get("ignored-dir"),
            Some(&SkipReason::Ignored)
        );
        let visited = visits.paths();
        assert!(visited.iter().any(|path| path.ends_with("ignored-dir")));
        assert!(visited.iter().all(|path| !path.ends_with("never-visit.rs")));
    }

    #[cfg(unix)]
    #[test]
    fn test_symlink_escape_is_never_opened() {
        use std::os::unix::fs::symlink;

        let fixture = Fixture::new();
        let outside = fixture.temp.path().join("outside.rs");
        fs::write(&outside, "pub const OUTSIDE: bool = true;\n")
            .expect("outside fixture should be written");
        symlink(&outside, fixture.root.join("escape.rs")).expect("symlink should be created");
        let inspected = Arc::new(Mutex::new(Vec::new()));
        let inspected_for_scan = Arc::clone(&inspected);

        let result = scan_impl(
            &fixture.store,
            fixture.repo,
            &fixture.root,
            ScanOptions::default(),
            Concurrency::default(),
            None,
            &move |path, max_file_bytes| {
                inspected_for_scan
                    .lock()
                    .expect("inspection recorder should lock")
                    .push(path.to_path_buf());
                inspect_file(path, max_file_bytes)
            },
        )
        .expect("scan should safely skip the symlink");

        assert_eq!(
            skipped_map(&result).get("escape.rs"),
            Some(&SkipReason::Symlink)
        );
        assert!(!result.added.contains(&PathBuf::from("escape.rs")));
        assert!(
            inspected
                .lock()
                .expect("inspection recorder should lock")
                .iter()
                .all(|path| path != &fixture.root.join("escape.rs"))
        );
    }

    #[test]
    fn test_read_error_fails_scan_with_typed_error() {
        let fixture = Fixture::new();
        let error = scan_impl(
            &fixture.store,
            fixture.repo,
            &fixture.root,
            ScanOptions::default(),
            Concurrency::default(),
            None,
            &|path, max_file_bytes| {
                if path.ends_with("src/main.rs") {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "injected read denial",
                    ));
                }
                inspect_file(path, max_file_bytes)
            },
        )
        .expect_err("read errors must fail safe");

        assert!(matches!(
            error,
            AtlasError::Scan(ScanError::ReadFile { path, .. })
                if path.ends_with("src/main.rs")
        ));
    }

    #[test]
    fn test_scan_rejects_unapproved_or_mismatched_root() {
        let fixture = Fixture::new();
        let other = fixture.temp.path().join("other");
        fs::create_dir(&other).expect("other root should be created");
        let error = scan(&fixture.store, fixture.repo, &other)
            .expect_err("an unapproved root must be rejected");
        assert!(matches!(
            error,
            AtlasError::Scan(ScanError::RepositoryNotApproved { .. })
        ));

        let other_repo = fixture
            .store
            .approve_repo(&other)
            .expect("other root should be approved");
        let error = scan(&fixture.store, other_repo, &fixture.root)
            .expect_err("a mismatched repo id must be rejected");
        assert!(matches!(
            error,
            AtlasError::Scan(ScanError::RepositoryMismatch { .. })
        ));
    }

    #[test]
    fn test_default_scan_limit_matches_config_contract() {
        assert_eq!(ScanOptions::default().max_file_bytes, 1_000_000);
    }

    #[test]
    fn test_outputs_are_sorted_and_deduplicated() {
        let fixture = Fixture::new();
        fs::write(fixture.root.join("z-last.rs"), "pub const Z: u8 = 1;\n")
            .expect("last fixture should be written");
        fs::write(fixture.root.join("a-first.rs"), "pub const A: u8 = 1;\n")
            .expect("first fixture should be written");

        let result =
            scan(&fixture.store, fixture.repo, &fixture.root).expect("scan should succeed");
        assert_sorted_unique(&result.added);
        assert_sorted_unique(&result.changed);
        assert_sorted_unique(&result.removed);
        assert_sorted_unique(&result.skipped);
    }

    #[test]
    fn test_parallel_scan_matches_serial_scan() {
        let fixture = Fixture::new();
        populate_wide_tree(&fixture.root, 12, 9);
        seed_store(&fixture);
        fs::write(
            fixture.root.join("wide/d03/f004.rs"),
            "pub fn changed() -> u8 { 9 }\n",
        )
        .expect("changed fixture should be written");
        fs::remove_file(fixture.root.join("wide/d07/f001.rs"))
            .expect("removed fixture should be deleted");
        fs::write(
            fixture.root.join("wide/d09/added.rs"),
            "pub fn added() {}\n",
        )
        .expect("added fixture should be written");
        fs::write(fixture.root.join("wide/d05/blob.bin"), b"\0binary")
            .expect("binary fixture should be written");
        fs::write(fixture.root.join("wide/d02/.env.local"), "TOKEN=fixture\n")
            .expect("secret fixture should be written");

        let serial = scan_with_concurrency(
            &fixture.store,
            fixture.repo,
            &fixture.root,
            ScanOptions::default(),
            Concurrency::serial(),
        )
        .expect("serial scan should succeed");
        assert_eq!(serial.changed, [PathBuf::from("wide/d03/f004.rs")]);
        assert_eq!(serial.removed, ["wide/d07/f001.rs"]);
        for jobs in [2, 4, 8] {
            for _ in 0..3 {
                let parallel = scan_with_concurrency(
                    &fixture.store,
                    fixture.repo,
                    &fixture.root,
                    ScanOptions::default(),
                    Concurrency::exact(jobs, 1 << 30),
                )
                .expect("parallel scan should succeed");
                assert_eq!(parallel, serial, "jobs = {jobs}");
            }
        }
    }

    #[test]
    fn test_parallel_scan_never_opens_excluded_entries() {
        let fixture = Fixture::new();
        populate_wide_tree(&fixture.root, 6, 8);
        let secret = fixture.root.join("wide/d01/.env.production");
        fs::write(&secret, "TOKEN=fixture\n").expect("secret fixture should be written");
        let oversize = fixture.root.join("wide/d02/large.rs");
        File::create(&oversize)
            .expect("oversize fixture should be created")
            .set_len(2_000_000)
            .expect("oversize fixture length should be set");
        let inspected = Arc::new(Mutex::new(Vec::new()));
        let inspected_for_scan = Arc::clone(&inspected);

        let result = scan_impl(
            &fixture.store,
            fixture.repo,
            &fixture.root,
            ScanOptions::default(),
            Concurrency::exact(4, 1 << 30),
            None,
            &move |path, max_file_bytes| {
                inspected_for_scan
                    .lock()
                    .expect("inspection recorder should lock")
                    .push(path.to_path_buf());
                inspect_file(path, max_file_bytes)
            },
        )
        .expect("scan should succeed");

        let skipped = skipped_map(&result);
        assert_eq!(
            skipped.get("wide/d01/.env.production"),
            Some(&SkipReason::SecretPattern)
        );
        assert_eq!(
            skipped.get("wide/d02/large.rs"),
            Some(&SkipReason::TooLarge)
        );
        let inspected = inspected.lock().expect("inspection recorder should lock");
        assert!(inspected.len() >= PARALLEL_INSPECTION_MIN_FILES);
        assert!(
            inspected
                .iter()
                .all(|path| !path.ends_with(".env.production") && !path.ends_with("large.rs"))
        );
    }

    #[test]
    fn test_parallel_scan_reports_lowest_ordered_read_error() {
        let fixture = Fixture::new();
        populate_wide_tree(&fixture.root, 6, 8);
        for _ in 0..5 {
            let error = scan_impl(
                &fixture.store,
                fixture.repo,
                &fixture.root,
                ScanOptions::default(),
                Concurrency::exact(8, 1 << 30),
                None,
                &|path, max_file_bytes| {
                    if path.ends_with("wide/d01/f002.rs") || path.ends_with("wide/d04/f000.rs") {
                        return Err(io::Error::new(
                            io::ErrorKind::PermissionDenied,
                            "injected read denial",
                        ));
                    }
                    inspect_file(path, max_file_bytes)
                },
            )
            .expect_err("read errors must fail safe");
            assert!(matches!(
                error,
                AtlasError::Scan(ScanError::ReadFile { path, .. })
                    if path.ends_with("wide/d01/f002.rs")
            ));
        }
    }

    #[test]
    fn test_memory_budget_limits_inspection_workers() {
        let tight = Concurrency::exact(8, INSPECTION_WORKER_BYTES * 2);
        assert_eq!(tight.workers_for(1_000, INSPECTION_WORKER_BYTES), 2);
        let starved = Concurrency::exact(8, 1);
        assert_eq!(starved.workers_for(1_000, INSPECTION_WORKER_BYTES), 1);
    }

    fn populate_wide_tree(root: &Path, directories: usize, files: usize) {
        for directory in 0..directories {
            let path = root.join(format!("wide/d{directory:02}"));
            fs::create_dir_all(&path).expect("wide fixture directory should be created");
            for file in 0..files {
                fs::write(
                    path.join(format!("f{file:03}.rs")),
                    format!("pub fn value_{directory}_{file}() -> usize {{ {file} }}\n"),
                )
                .expect("wide fixture file should be written");
            }
        }
    }

    struct Fixture {
        temp: TempDir,
        root: PathBuf,
        store: Store,
        repo: RepoId,
    }

    impl Fixture {
        fn new() -> Self {
            let temp = tempfile::tempdir().expect("temporary directory should be created");
            let root = temp.path().join("repo");
            copy_dir(fixture_root(), &root);
            let store = Store::open(&temp.path().join("compresso.db")).expect("store should open");
            let repo = store
                .approve_repo(&root)
                .expect("fixture should be approved");
            Self {
                temp,
                root,
                store,
                repo,
            }
        }
    }

    fn fixture_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/scan-repo")
    }

    fn copy_dir(source: PathBuf, destination: &Path) {
        fs::create_dir_all(destination).expect("fixture destination should be created");
        let mut entries = fs::read_dir(source)
            .expect("fixture directory should be readable")
            .collect::<Result<Vec<_>, _>>()
            .expect("fixture entries should be readable");
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let destination_path = destination.join(entry.file_name());
            if entry
                .file_type()
                .expect("fixture type should load")
                .is_dir()
            {
                copy_dir(entry.path(), &destination_path);
            } else {
                fs::copy(entry.path(), destination_path).expect("fixture file should be copied");
            }
        }
    }

    fn skipped_map(result: &ScanResult) -> BTreeMap<String, SkipReason> {
        result
            .skipped
            .iter()
            .map(|(path, reason)| {
                (
                    path.to_str()
                        .expect("fixture paths should be UTF-8")
                        .to_owned(),
                    *reason,
                )
            })
            .collect()
    }

    fn seed_store(fixture: &Fixture) {
        let result =
            scan(&fixture.store, fixture.repo, &fixture.root).expect("scan should succeed");
        for path in result.added {
            let absolute = fixture.root.join(&path);
            let bytes = fs::read(&absolute).expect("candidate should be readable");
            fixture
                .store
                .upsert_file(
                    fixture.repo,
                    &FileRecord {
                        path: path
                            .to_str()
                            .expect("fixture path should be UTF-8")
                            .to_owned(),
                        hash: blake3::hash(&bytes).to_hex().to_string(),
                        language: None,
                        size_bytes: bytes.len() as u64,
                        token_estimate: 0,
                        indexed_at: 1,
                    },
                )
                .expect("candidate hash should be stored");
        }
    }

    fn assert_sorted_unique<T: Ord + std::fmt::Debug>(values: &[T]) {
        assert!(
            values.windows(2).all(|pair| pair[0] < pair[1]),
            "{values:?}"
        );
    }
}
