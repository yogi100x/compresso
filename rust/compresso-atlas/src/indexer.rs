use std::collections::{BTreeSet, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use compresso_core::TokenCounter;
use compresso_store::{EdgeKind, EdgeRecord, EdgeResolution, FileRecord, Store, SymbolRecord};

use crate::parsers::{ImportRecord, ImportResolution, ParseError, ParserRegistry};
use crate::scanner::{AtlasError, scan};

const INDEX_SCHEMA_VERSION: u32 = 1;

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
}

impl<'a> Indexer<'a> {
    pub fn new(store: &'a Store, counter: &'a dyn TokenCounter) -> Self {
        Self {
            store,
            counter,
            parser_version: env!("CARGO_PKG_VERSION").to_owned(),
            schema_version: INDEX_SCHEMA_VERSION,
        }
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
        let scan = scan(self.store, repo, &canonical_root)?;
        let removed_set = scan.removed.iter().cloned().collect::<HashSet<_>>();
        let mut candidates = scan
            .added
            .into_iter()
            .chain(scan.changed)
            .collect::<BTreeSet<_>>();
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

        for (index, relative) in candidates.into_iter().enumerate() {
            before_file(index, &relative)?;
            let relative_text = relative
                .to_str()
                .ok_or_else(|| IndexError::NonUtf8Path {
                    path: relative.clone(),
                })?
                .to_owned();
            let absolute = resolve_candidate(&canonical_root, &relative)?;
            let source =
                fs::read_to_string(&absolute).map_err(|source| IndexError::ReadSource {
                    path: absolute.clone(),
                    source,
                })?;
            let hash = blake3::hash(source.as_bytes()).to_hex().to_string();
            let language = ParserRegistry::for_path(&relative);
            let mut file_symbols = Vec::new();
            let mut edges = Vec::new();

            if let Some(language) = language {
                progress(IndexProgress::Parsing {
                    path: relative.clone(),
                });
                let extraction = ParserRegistry::extract(language, &source).map_err(|source| {
                    IndexError::Parse {
                        path: relative.clone(),
                        source,
                    }
                })?;
                file_symbols = extraction.symbols;
                estimate_symbol_tokens(&source, &relative, &mut file_symbols, self.counter)?;
                for import in extraction.imports {
                    match &import.resolution {
                        ImportResolution::Resolved => imports_resolved += 1,
                        ImportResolution::Ambiguous(_) | ImportResolution::Unresolved(_) => {
                            imports_unresolved += 1;
                        }
                    }
                    edges.push(edge_from_import(import));
                }
                files_indexed += 1;
                symbols += file_symbols.len();
            } else {
                files_skipped += 1;
            }

            self.store.replace_indexed_file(
                repo,
                &FileRecord {
                    path: relative_text,
                    hash,
                    language,
                    size_bytes: source.len() as u64,
                    token_estimate: self.counter.count(&source),
                    indexed_at,
                },
                &file_symbols,
                &edges,
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
    use std::path::PathBuf;

    use compresso_core::{TokenProvider, counter_for};
    use compresso_store::Store;

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
}
