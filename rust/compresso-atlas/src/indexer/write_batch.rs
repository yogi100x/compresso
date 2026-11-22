//! Batched index writes (D106).
//!
//! Prepared files are buffered and written several per transaction, so a
//! reader still sees a file with all of its symbols, edges, and call sites or
//! not at all. Progress events and report counts are produced per file, in
//! candidate order, as each file is committed, so they match a per-file write
//! exactly. A [`WritePacer`] leaves the write lock free for a moment after
//! each second of back-to-back writes, so another server on the same store
//! is never starved.

use std::mem;
use std::time::{Duration, Instant};

use compresso_store::{CallSiteRecord, IndexedFileWrite, RepoId, Store, WritePacer};

use super::{IndexProgress, PrepareFailure, PreparedFile};
use crate::scanner::AtlasError;

/// Bounds on what is buffered before a write and on how long one write may
/// hold the store's write lock.
#[derive(Clone, Copy, Debug)]
pub(super) struct BatchLimits {
    /// Files buffered before they are written.
    pub(super) max_files: usize,
    /// Source bytes of the buffered files (a proxy for their rows) before
    /// they are written.
    pub(super) max_source_bytes: u64,
    /// Age of the oldest buffered file before the buffer is written, so
    /// progress and readers never fall far behind a slow run.
    pub(super) max_age: Duration,
    /// Time after which a write commits and releases the lock, even with
    /// files left: the lock is held for at most this plus one file's write
    /// and the commit.
    pub(super) max_hold: Duration,
}

impl BatchLimits {
    pub(super) const DEFAULT: Self = Self {
        max_files: 32,
        max_source_bytes: 8 << 20,
        max_age: Duration::from_millis(250),
        max_hold: Duration::from_millis(200),
    };
}

/// Report counts accumulated as files are committed.
#[derive(Debug, Default)]
pub(super) struct Tally {
    pub(super) files_indexed: usize,
    pub(super) files_skipped: usize,
    pub(super) symbols: usize,
    pub(super) imports_resolved: usize,
    pub(super) imports_unresolved: usize,
    pub(super) call_sites: usize,
    pub(super) call_site_files_truncated: usize,
    pub(super) symbol_files_truncated: usize,
    pub(super) call_site_files_rejected: usize,
}

/// Buffers prepared files on the committing thread and writes them in order.
pub(super) struct BatchWriter<'a, P> {
    store: &'a Store,
    repo: RepoId,
    progress: P,
    limits: BatchLimits,
    pending: Vec<PreparedFile>,
    pending_source_bytes: u64,
    oldest: Option<Instant>,
    pacer: WritePacer,
    pub(super) tally: Tally,
}

impl<'a, P: FnMut(IndexProgress)> BatchWriter<'a, P> {
    pub(super) fn new(
        store: &'a Store,
        repo: RepoId,
        progress: P,
        limits: BatchLimits,
        files_skipped: usize,
    ) -> Self {
        Self {
            store,
            repo,
            progress,
            limits,
            pending: Vec::new(),
            pending_source_bytes: 0,
            oldest: None,
            pacer: WritePacer::default(),
            tally: Tally {
                files_skipped,
                ..Tally::default()
            },
        }
    }

    /// Returns the progress callback, the counts, and the pacer (for the
    /// run's link pass) once every buffered file is written.
    pub(super) fn into_parts(self) -> (P, Tally, WritePacer) {
        debug_assert!(self.pending.is_empty(), "flush before taking the parts");
        (self.progress, self.tally, self.pacer)
    }

    /// Adds the next prepared file in candidate order, writing the buffer
    /// when it reaches a limit.
    pub(super) fn push(&mut self, prepared: PreparedFile) -> Result<(), AtlasError> {
        self.pending_source_bytes = self
            .pending_source_bytes
            .saturating_add(prepared.record.size_bytes);
        self.pending.push(prepared);
        let oldest = *self.oldest.get_or_insert_with(Instant::now);
        if self.pending.len() >= self.limits.max_files
            || self.pending_source_bytes >= self.limits.max_source_bytes
            || oldest.elapsed() >= self.limits.max_age
        {
            self.flush()?;
        }
        Ok(())
    }

    /// Writes the buffer when its oldest file has waited `max_age`; called
    /// while the committing thread waits for the next file in order.
    pub(super) fn flush_if_aged(&mut self) -> Result<(), AtlasError> {
        if self
            .oldest
            .is_some_and(|oldest| oldest.elapsed() >= self.limits.max_age)
        {
            self.flush()?;
        }
        Ok(())
    }

    /// Handles a file whose preparation failed: every file before it is
    /// written first, then the failure is reported with the progress event a
    /// per-file run would have emitted.
    pub(super) fn fail(&mut self, path: &std::path::Path, failure: PrepareFailure) -> AtlasError {
        if let Err(error) = self.flush() {
            return error;
        }
        if failure.parsing_started {
            (self.progress)(IndexProgress::Parsing {
                path: path.to_path_buf(),
            });
        }
        *failure.error
    }

    /// Writes every buffered file, in as many transactions as the lock-hold
    /// bound needs.
    ///
    /// When one file fails to write, the files before it stay committed and
    /// counted, the failed file keeps its previous rows, the files after it
    /// are not written (the next index run writes them, since their stored
    /// hashes are unchanged), and the store's error is returned.
    pub(super) fn flush(&mut self) -> Result<(), AtlasError> {
        let pending = mem::take(&mut self.pending);
        self.pending_source_bytes = 0;
        self.oldest = None;
        if pending.is_empty() {
            return Ok(());
        }
        // The call extractor already drops every call the store would refuse;
        // this is the second line of defence. A file whose call sites would
        // still fail validation is written with its symbols and imports and
        // without call sites, so one hostile file never aborts the index.
        let rejected = pending
            .iter()
            .map(|prepared| {
                compresso_store::validate_call_sites(&prepared.calls, prepared.symbols.len())
                    .is_err()
            })
            .collect::<Vec<_>>();
        // Java and C# (D105) and Go (D111): a file whose call sites are cut
        // or dropped holds one marker row, written in the same transaction
        // as the file, so `impact` can tell that its calls may be missing.
        // The marker is not a call site and no report count includes it.
        let marked = pending
            .iter()
            .zip(&rejected)
            .map(|(prepared, rejected)| {
                ((prepared.calls_truncated || *rejected)
                    && !prepared.symbols.is_empty()
                    && crate::parsers::languages::calls_are_package_evidence(&prepared.record.path))
                .then(|| {
                    let mut calls = if *rejected {
                        Vec::new()
                    } else {
                        prepared.calls.clone()
                    };
                    calls.push(CallSiteRecord {
                        caller: 0,
                        callee_name: compresso_store::CALL_SITES_CUT_MARKER.to_owned(),
                        qualifier: None,
                        form: compresso_store::CallForm::Plain,
                        line: 1,
                    });
                    calls
                })
            })
            .collect::<Vec<_>>();
        let writes = pending
            .iter()
            .zip(&rejected)
            .zip(&marked)
            .map(|((prepared, rejected), marked)| IndexedFileWrite {
                record: &prepared.record,
                symbols: &prepared.symbols,
                edges: &prepared.edges,
                calls: match marked {
                    Some(calls) => calls,
                    None if *rejected => &[] as &[CallSiteRecord],
                    None => &prepared.calls,
                },
            })
            .collect::<Vec<_>>();
        let mut start = 0;
        while start < writes.len() {
            let (store, repo, max_hold) = (self.store, self.repo, self.limits.max_hold);
            match self
                .pacer
                .write(|| store.replace_indexed_files(repo, &writes[start..], max_hold))
            {
                Ok(written) => {
                    for index in start..start + written {
                        self.account(&pending[index], rejected[index]);
                    }
                    start += written;
                }
                Err(error) => {
                    for index in start..start + error.written {
                        self.account(&pending[index], rejected[index]);
                    }
                    // A per-file write announced the failing file before
                    // writing it; a failure that belongs to no file (the
                    // lock, the commit) announces nothing.
                    if let Some(failed) = error.failed.map(|failed| &pending[start + failed])
                        && failed.parsed
                    {
                        (self.progress)(IndexProgress::Parsing {
                            path: failed.record.path.clone().into(),
                        });
                    }
                    return Err((*error.source).into());
                }
            }
        }
        Ok(())
    }

    /// Emits the progress event and counts of one committed file.
    fn account(&mut self, prepared: &PreparedFile, rejected: bool) {
        let tally = &mut self.tally;
        if prepared.parsed {
            (self.progress)(IndexProgress::Parsing {
                path: prepared.record.path.clone().into(),
            });
            tally.files_indexed += 1;
            tally.symbols += prepared.symbols.len();
            tally.imports_resolved += prepared.imports_resolved;
            tally.imports_unresolved += prepared.imports_unresolved;
        } else {
            tally.files_skipped += 1;
        }
        if rejected {
            tally.call_site_files_rejected += 1;
        } else if prepared.parsed {
            tally.call_sites += prepared.calls.len();
            tally.call_site_files_truncated += usize::from(prepared.calls_truncated);
            tally.symbol_files_truncated += usize::from(prepared.symbols_truncated);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::fs;
    use std::path::{Path, PathBuf};

    use compresso_core::{TokenProvider, counter_for};
    use compresso_store::Store;

    use super::*;
    use crate::go_imports::GoModules;
    use crate::indexer::{Indexer, PrepareContext, prepare_file};
    use crate::rust_imports::RustWorkspace;
    use crate::{TraceOptions, TraceResult, impact, trace};

    /// Writes one prepared file through the batch writer, as one batch,
    /// without the run's link pass.
    fn write_without_linking(store: &Store, repo: RepoId, root: &Path, path: &str) {
        let known_paths = ["app/a.ts", "lib/b.ts", "lib/other.ts"]
            .into_iter()
            .map(str::to_owned)
            .collect::<BTreeSet<_>>();
        let context = PrepareContext {
            root,
            known_paths: &known_paths,
            rust_workspace: &RustWorkspace::default(),
            go_modules: &GoModules::default(),
            declared: &crate::declared_imports::DeclaredPackages::default(),
            counter: counter_for(TokenProvider::Heuristic),
            indexed_at: 2,
        };
        let prepared = prepare_file(&context, &PathBuf::from(path))
            .unwrap_or_else(|_| panic!("{path} should prepare"));
        let mut writer = BatchWriter::new(store, repo, |_| {}, BatchLimits::DEFAULT, 0);
        writer.push(prepared).expect("buffered");
        writer.flush().expect("written");
    }

    #[test]
    fn test_readers_between_a_batch_commit_and_the_link_pass_see_dependents_or_incomplete() {
        // An incremental run changes app/a.ts to import lib/b.ts, which it
        // adds; a.ts sorts first, so its import is stored pending and only
        // the run's link pass (or, after a kill, the next run's) links it.
        // Readers in that window must find the dependent once b.ts is
        // committed, and call the answer incomplete before that.
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let root = fs::canonicalize(directory.path())
            .expect("root")
            .join("repo");
        fs::create_dir_all(root.join("app")).expect("app directory");
        fs::create_dir_all(root.join("lib")).expect("lib directory");
        fs::write(
            root.join("app/a.ts"),
            "import { other } from '../lib/other';\nexport function run(xs: number[]) { return xs.map(other); }\n",
        )
        .expect("a");
        fs::write(
            root.join("lib/other.ts"),
            "export function other(x: number) { return x; }\n",
        )
        .expect("other");
        let store = Store::open(&directory.path().join("compresso.db")).expect("store should open");
        Indexer::new(&store, counter_for(TokenProvider::Heuristic))
            .index(&root, |_| {})
            .expect("initial index");
        let repo = store
            .repo_by_root(&root)
            .expect("lookup")
            .expect("repository")
            .id;

        fs::write(
            root.join("app/a.ts"),
            "import { double } from '../lib/b';\nexport function run(xs: number[]) { return xs.map(double); }\n",
        )
        .expect("a changed");
        fs::write(
            root.join("lib/b.ts"),
            "export function double(x: number) { return x * 2; }\n",
        )
        .expect("b added");

        // Window 1: a.ts committed, b.ts not yet.
        write_without_linking(&store, repo, &root, "app/a.ts");
        let report = impact(&store, repo, "lib/other.ts").expect("impact");
        assert!(
            !report.complete,
            "a pending import must make impact incomplete"
        );
        assert!(
            !store
                .incomplete_edge_sources(repo)
                .expect("sources")
                .is_empty(),
            "the CLI's completeness agrees"
        );
        let callers =
            trace(&store, repo, None, Some("other"), TraceOptions::default()).expect("trace");
        assert!(
            !callers.complete,
            "a pending import must make trace incomplete"
        );

        // Window 2: b.ts committed in a later batch, still not linked.
        write_without_linking(&store, repo, &root, "lib/b.ts");
        let report = impact(&store, repo, "lib/b.ts").expect("impact");
        assert_eq!(
            report
                .dependents
                .iter()
                .map(|entry| entry.path.as_str())
                .collect::<Vec<_>>(),
            ["app/a.ts"]
        );
        assert!(report.complete);
        assert!(
            store
                .incomplete_edge_sources(repo)
                .expect("sources")
                .is_empty()
        );
        let callers =
            trace(&store, repo, None, Some("double"), TraceOptions::default()).expect("trace");
        assert!(callers.complete);
        let TraceResult::Callers(callers) = callers.result else {
            panic!("expected callers, got {:?}", callers.result);
        };
        assert_eq!(callers.len(), 1, "{callers:?}");
    }

    fn synthetic(path: &str) -> PreparedFile {
        PreparedFile {
            record: compresso_store::FileRecord {
                path: path.to_owned(),
                hash: format!("hash-{path}"),
                language: Some(compresso_store::FileLanguage::TypeScript),
                size_bytes: 10,
                token_estimate: 2,
                indexed_at: 1,
            },
            symbols: Vec::new(),
            edges: Vec::new(),
            calls: Vec::new(),
            calls_truncated: false,
            symbols_truncated: false,
            parsed: true,
            imports_resolved: 0,
            imports_unresolved: 0,
        }
    }

    #[test]
    fn test_buffered_files_commit_while_a_later_file_is_slow_to_prepare() {
        // The buffer's age bound must hold while the committing thread waits
        // for the next file in order, not only when that file arrives.
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let store = Store::open(&directory.path().join("compresso.db")).expect("store should open");
        let repo = store
            .approve_repo(directory.path())
            .expect("repository should be approved");
        let limits = BatchLimits {
            max_age: std::time::Duration::from_millis(50),
            ..BatchLimits::DEFAULT
        };
        let candidates = [PathBuf::from("a.ts"), PathBuf::from("b.ts")];
        let seen_while_waiting = std::sync::atomic::AtomicBool::new(false);
        let writer = std::cell::RefCell::new(BatchWriter::new(&store, repo, |_| {}, limits, 0));

        crate::indexer::prepare_in_parallel_with(
            directory.path(),
            &candidates,
            2,
            1 << 30,
            |candidate| {
                if candidate == Path::new("b.ts") {
                    let started = std::time::Instant::now();
                    while started.elapsed() < std::time::Duration::from_secs(30) {
                        if store
                            .file_hashes(repo)
                            .expect("hashes")
                            .contains_key("a.ts")
                        {
                            seen_while_waiting.store(true, std::sync::atomic::Ordering::Relaxed);
                            break;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(10));
                    }
                }
                Ok(synthetic(candidate.to_str().expect("path")))
            },
            |_, outcome| {
                let prepared = outcome.map_err(|failure| *failure.error)?;
                writer.borrow_mut().push(prepared)
            },
            || writer.borrow_mut().flush_if_aged(),
        )
        .expect("run");
        writer.borrow_mut().flush().expect("flush");

        assert!(
            seen_while_waiting.load(std::sync::atomic::Ordering::Relaxed),
            "a.ts must be committed while b.ts is still being prepared"
        );
    }

    #[test]
    fn test_a_cut_java_file_gets_its_marker_row_in_the_same_write_and_no_report_count() {
        // D105's marker row for a file whose call sites were cut is written
        // with the file in its batch transaction; it is not a call site and
        // no `IndexReport` count includes it.
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let store = Store::open(&directory.path().join("compresso.db")).expect("store should open");
        let repo = store
            .approve_repo(directory.path())
            .expect("repository should be approved");
        let mut prepared = synthetic("src/A.java");
        prepared.record.language = Some(compresso_store::FileLanguage::Java);
        prepared.symbols = vec![compresso_store::SymbolRecord {
            name: "A".to_owned(),
            qualified_name: "A".to_owned(),
            kind: compresso_store::SymbolKind::Class,
            signature: None,
            start_line: 1,
            end_line: 3,
            token_estimate: 1,
        }];
        prepared.calls = vec![CallSiteRecord {
            caller: 0,
            callee_name: "run".to_owned(),
            qualifier: None,
            form: compresso_store::CallForm::Plain,
            line: 2,
        }];
        prepared.calls_truncated = true;
        let mut writer = BatchWriter::new(&store, repo, |_| {}, BatchLimits::DEFAULT, 0);
        writer.push(prepared).expect("buffered");
        writer.flush().expect("written");
        let (_, tally, _) = writer.into_parts();

        assert_eq!(
            store.call_site_count(repo).expect("count"),
            1,
            "the marker is no call"
        );
        assert_eq!(
            store
                .files_with_cut_call_sites(repo)
                .expect("marked files")
                .into_iter()
                .collect::<Vec<_>>(),
            ["src/A.java"]
        );
        assert_eq!(tally.call_sites, 1);
        assert_eq!(tally.call_site_files_truncated, 1);
    }

    #[test]
    fn test_a_cut_go_file_gets_its_marker_row_and_a_cut_script_file_none() {
        // E17-46 (D111): Go reads call sites as same-package evidence, so a
        // cut Go file is marked as a Java file is; other languages are not.
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let store = Store::open(&directory.path().join("compresso.db")).expect("store should open");
        let repo = store
            .approve_repo(directory.path())
            .expect("repository should be approved");
        let mut writer = BatchWriter::new(&store, repo, |_| {}, BatchLimits::DEFAULT, 0);
        for (path, language) in [
            ("store/store.go", compresso_store::FileLanguage::Go),
            ("src/a.ts", compresso_store::FileLanguage::TypeScript),
        ] {
            let mut prepared = synthetic(path);
            prepared.record.language = Some(language);
            prepared.symbols = vec![compresso_store::SymbolRecord {
                name: "run".to_owned(),
                qualified_name: "run".to_owned(),
                kind: compresso_store::SymbolKind::Function,
                signature: None,
                start_line: 1,
                end_line: 3,
                token_estimate: 1,
            }];
            prepared.calls = vec![CallSiteRecord {
                caller: 0,
                callee_name: "open".to_owned(),
                qualifier: None,
                form: compresso_store::CallForm::Plain,
                line: 2,
            }];
            prepared.calls_truncated = true;
            writer.push(prepared).expect("buffered");
        }
        writer.flush().expect("written");
        let (_, tally, _) = writer.into_parts();

        assert_eq!(store.call_site_count(repo).expect("count"), 2);
        assert_eq!(
            store
                .files_with_cut_call_sites(repo)
                .expect("marked files")
                .into_iter()
                .collect::<Vec<_>>(),
            ["store/store.go"]
        );
        assert_eq!(tally.call_sites, 2);
    }
}
