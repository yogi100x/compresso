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
        let writes = pending
            .iter()
            .zip(&rejected)
            .map(|(prepared, rejected)| IndexedFileWrite {
                record: &prepared.record,
                symbols: &prepared.symbols,
                edges: &prepared.edges,
                calls: if *rejected {
                    &[] as &[CallSiteRecord]
                } else {
                    &prepared.calls
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
                    let failed = &pending[start + error.written];
                    if failed.parsed {
                        (self.progress)(IndexProgress::Parsing {
                            path: failed.record.path.clone().into(),
                        });
                    }
                    return Err(error.source.into());
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
        }
    }
}
