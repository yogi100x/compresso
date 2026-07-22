use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs::{self, File, Metadata};
use std::io::{self, BufReader, Read};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use compresso_store::{RepoId, Store, StoreError};
use ignore::{DirEntry, WalkBuilder};

/// Safe scanner limit used by [`scan`] and by the slice configuration default.
pub const DEFAULT_MAX_FILE_BYTES: u64 = 1_000_000;
const BINARY_SNIFF_BYTES: u64 = 8 * 1024;
const HASH_BUFFER_BYTES: usize = 64 * 1024;

/// Per-scan limits that can be supplied by configuration and tests.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScanOptions {
    pub max_file_bytes: u64,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            max_file_bytes: DEFAULT_MAX_FILE_BYTES,
        }
    }
}

/// A deterministic difference between the filesystem and the stored file ledger.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScanResult {
    pub added: Vec<PathBuf>,
    pub changed: Vec<PathBuf>,
    pub removed: Vec<String>,
    pub skipped: Vec<(PathBuf, SkipReason)>,
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
    scan_impl(store, repo, root, options, None, &inspect_file)
}

fn scan_impl<I>(
    store: &Store,
    repo: RepoId,
    root: &Path,
    options: ScanOptions,
    instrumentation: Option<WalkInstrumentation>,
    inspector: &I,
) -> Result<ScanResult, AtlasError>
where
    I: Fn(&Path, u64) -> io::Result<FileInspection>,
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
    let classification = metadata_classification_walk(&canonical_root, instrumentation)?;
    let mut candidate_hashes = BTreeMap::new();
    let mut skipped = classification
        .ignored
        .into_iter()
        .map(|path| (path, SkipReason::Ignored))
        .collect::<Vec<_>>();

    for (relative, entry) in classification.discovered {
        let relative_path = PathBuf::from(&relative);
        let reason = if is_secret_pattern(&relative) {
            Some(SkipReason::SecretPattern)
        } else if entry.metadata.file_type().is_symlink() {
            Some(SkipReason::Symlink)
        } else if !entry.metadata.is_file() {
            Some(SkipReason::UnsupportedFileType)
        } else if entry.metadata.len() > options.max_file_bytes {
            Some(SkipReason::TooLarge)
        } else {
            None
        };
        if let Some(reason) = reason {
            skipped.push((relative_path, reason));
            continue;
        }

        let resolved = fs::canonicalize(&entry.absolute).map_err(|source| ScanError::ReadFile {
            path: entry.absolute.clone(),
            source,
        })?;
        if !resolved.starts_with(&canonical_root) {
            return Err(AtlasError::OutsideRoot {
                path: resolved,
                root: canonical_root,
            });
        }
        if resolved != entry.absolute {
            return Err(ScanError::EntryChanged {
                path: entry.absolute,
            }
            .into());
        }

        match inspector(&resolved, options.max_file_bytes).map_err(|source| {
            ScanError::ReadFile {
                path: resolved.clone(),
                source,
            }
        })? {
            FileInspection::Binary => skipped.push((relative_path, SkipReason::Binary)),
            FileInspection::TooLarge => skipped.push((relative_path, SkipReason::TooLarge)),
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
    })
}

struct ClassifiedEntry {
    absolute: PathBuf,
    metadata: Metadata,
}

struct ClassificationWalk {
    discovered: BTreeMap<String, ClassifiedEntry>,
    ignored: Vec<PathBuf>,
}

fn metadata_classification_walk(
    root: &Path,
    instrumentation: Option<WalkInstrumentation>,
) -> Result<ClassificationWalk, ScanError> {
    let mut matcher_builder = WalkBuilder::new(root);
    configure_ignore_filters(&mut matcher_builder, true);
    let Some(matcher) = matcher_builder.build_matchers().into_iter().next() else {
        return Err(ScanError::IgnoreMatcherUnavailable);
    };
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
        if !file_type.is_dir() && (file_type.is_symlink() || is_secret_name(entry.file_name())) {
            return true;
        }
        let Ok(relative) = entry.path().strip_prefix(&root_for_filter) else {
            unavailable_for_filter.store(true, Ordering::Relaxed);
            return false;
        };
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
        if matched.is_ignore() {
            if let Ok(mut ignored) = ignored_for_filter.lock() {
                ignored.insert(entry.path().to_path_buf());
            } else {
                unavailable_for_filter.store(true, Ordering::Relaxed);
            }
            return false;
        }
        true
    });

    let mut discovered = BTreeMap::new();
    for result in builder.build() {
        let entry = result.map_err(walk_error)?;
        if entry.depth() == 0 || entry.file_type().is_some_and(|kind| kind.is_dir()) {
            continue;
        }
        let relative = relative_path(root, entry.path())?;
        let metadata =
            fs::symlink_metadata(entry.path()).map_err(|source| ScanError::Metadata {
                path: entry.path().to_path_buf(),
                source,
            })?;
        discovered.insert(
            relative,
            ClassifiedEntry {
                absolute: entry.path().to_path_buf(),
                metadata,
            },
        );
    }
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
    })
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

fn is_builtin_excluded_directory(entry: &DirEntry) -> bool {
    entry.file_type().is_some_and(|kind| kind.is_dir())
        && matches!(
            entry.file_name(),
            name if name == OsStr::new("node_modules")
                || name == OsStr::new("target")
                || name == OsStr::new("dist")
                || name == OsStr::new(".git")
        )
}

fn is_secret_pattern(relative: &str) -> bool {
    let Some(name) = Path::new(relative).file_name().and_then(OsStr::to_str) else {
        return true;
    };
    is_secret_name(OsStr::new(name))
}

fn is_secret_name(name: &OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return true;
    };
    name.starts_with(".env") || name.ends_with(".pem") || name.starts_with("id_rsa")
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
        let inspected = Arc::new(Mutex::new(Vec::new()));
        let inspected_for_scan = Arc::clone(&inspected);
        let result = scan_impl(
            &fixture.store,
            fixture.repo,
            &fixture.root,
            ScanOptions::default(),
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
        for path in [".env.local", "server.pem", "id_rsa"] {
            assert_eq!(skipped.get(path), Some(&SkipReason::SecretPattern));
            assert!(!result.added.contains(&PathBuf::from(path)));
        }
        let inspected = inspected.lock().expect("inspection recorder should lock");
        assert!(inspected.iter().all(|path| {
            !matches!(
                path.file_name().and_then(|name| name.to_str()),
                Some(".env.local" | "server.pem" | "id_rsa")
            )
        }));
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
            ScanOptions {
                max_file_bytes: 1_000_000,
            },
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
