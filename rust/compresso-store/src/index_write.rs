//! The indexer's write path (D106): many files per transaction, each file
//! whole to a reader, and pending imports linked once per index run.

use std::time::{Duration, Instant};

use rusqlite::{OptionalExtension, TransactionBehavior, params};
use thiserror::Error;

use crate::repos::ensure_repo_exists;
use crate::symbols::{IndexedFileWrite, LINKABLE_EDGE, validate_indexed_file, write_indexed_file};
use crate::{RepoId, Store, StoreError};

/// A batch write that stopped at one file.
///
/// The first `written` files of the batch are committed; the file at index
/// `written` failed and is left exactly as it was before the call; no later
/// file was attempted.
#[derive(Debug, Error)]
#[error("failed to write file {written} of an index batch")]
pub struct IndexedFilesError {
    pub written: usize,
    #[source]
    pub source: StoreError,
}

/// Keeps back-to-back write transactions from starving another connection
/// that waits for the write lock (D106).
///
/// A waiting connection's busy handler retries at most 100 ms apart, so it
/// can miss every short gap between transactions that follow each other
/// closely. After [`WritePacer::MAX_CONTINUOUS_HOLD`] of writes without a gap
/// of [`WritePacer::LOCK_YIELD`], the pacer leaves the lock free for that
/// long, which always spans one retry.
#[derive(Debug)]
pub struct WritePacer {
    max_continuous: Duration,
    yield_for: Duration,
    continuous: Duration,
    last_end: Option<Instant>,
    #[cfg(test)]
    pauses: usize,
}

impl Default for WritePacer {
    fn default() -> Self {
        Self::new(Self::MAX_CONTINUOUS_HOLD, Self::LOCK_YIELD)
    }
}

impl WritePacer {
    /// Writing time after which the lock is left free.
    pub const MAX_CONTINUOUS_HOLD: Duration = Duration::from_secs(1);
    /// How long the lock is left free: longer than SQLite's longest wait
    /// between two busy-handler retries (100 ms).
    pub const LOCK_YIELD: Duration = Duration::from_millis(120);

    pub fn new(max_continuous: Duration, yield_for: Duration) -> Self {
        Self {
            max_continuous,
            yield_for,
            continuous: Duration::ZERO,
            last_end: None,
            #[cfg(test)]
            pauses: 0,
        }
    }

    /// Runs one write transaction, then pauses when the writes since the
    /// last gap add up to the bound.
    pub fn write<T>(&mut self, write: impl FnOnce() -> T) -> T {
        let began = Instant::now();
        if self
            .last_end
            .is_some_and(|end| began.duration_since(end) >= self.yield_for)
        {
            self.continuous = Duration::ZERO;
        }
        let result = write();
        let ended = Instant::now();
        self.continuous += ended.duration_since(began);
        self.last_end = Some(ended);
        if self.continuous >= self.max_continuous {
            std::thread::sleep(self.yield_for);
            #[cfg(test)]
            {
                self.pauses += 1;
            }
            self.continuous = Duration::ZERO;
            self.last_end = Some(Instant::now());
        }
        result
    }
}

/// How one transaction of [`Store::replace_indexed_files`] ended early.
enum BatchFailure {
    /// The first `written` files are committed; nothing else was kept.
    Kept { written: usize, source: StoreError },
    /// The file at `failed_at` failed part-way; the whole transaction was
    /// rolled back.
    RolledBack {
        failed_at: usize,
        source: StoreError,
    },
}

impl Store {
    /// Writes `files` in order in one transaction, so a reader sees each file
    /// with all of its rows or not at all.
    ///
    /// Returns how many files, counted from the front, were written and
    /// committed: all of them, or fewer when `max_hold` had elapsed after a
    /// file, in which case the caller passes the rest again. The write lock
    /// is therefore held for at most `max_hold` plus one file's write and the
    /// commit. When a file fails, the files before it are committed, the
    /// failing file and every later one are left as they were, and
    /// [`IndexedFilesError`] says how many were written.
    ///
    /// No savepoint is taken per file: the full-text index flushes its
    /// pending terms at every savepoint, which costs as much as a commit
    /// (D106). A file that fails part-way through its rows instead rolls back
    /// the whole transaction, and the files before it are written again in a
    /// new one; failures are rare, and most are caught by validation before
    /// any row is written.
    ///
    /// Incoming edges are not linked here: an import of a file written later
    /// in the same run stays `pending_target` until
    /// [`Store::link_pending_edges`] runs.
    pub fn replace_indexed_files(
        &self,
        repo: RepoId,
        files: &[IndexedFileWrite<'_>],
        max_hold: Duration,
    ) -> Result<usize, IndexedFilesError> {
        let (written, source) = match self.write_files(repo, files, Some(max_hold)) {
            Ok(written) => return Ok(written),
            Err(BatchFailure::Kept { written, source }) => {
                return Err(IndexedFilesError { written, source });
            }
            Err(BatchFailure::RolledBack { failed_at, source }) => (failed_at, source),
        };
        if written == 0 {
            return Err(IndexedFilesError { written, source });
        }
        // Writes the files before the failing one again, without it. They
        // were all written once in this call, so no hold bound applies.
        match self.write_files(repo, &files[..written], None) {
            Ok(rewritten) if rewritten == written => Err(IndexedFilesError { written, source }),
            Ok(_) | Err(_) => Err(IndexedFilesError { written: 0, source }),
        }
    }

    /// One transaction over `files`; see [`Store::replace_indexed_files`].
    fn write_files(
        &self,
        repo: RepoId,
        files: &[IndexedFileWrite<'_>],
        max_hold: Option<Duration>,
    ) -> Result<usize, BatchFailure> {
        if files.is_empty() {
            return Ok(0);
        }
        let nothing_kept = |source| BatchFailure::Kept { written: 0, source };
        let mut connection = self.connection().map_err(nothing_kept)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| nothing_kept(error.into()))?;
        ensure_repo_exists(&transaction, repo).map_err(nothing_kept)?;
        let started = Instant::now();
        let mut written = 0;
        for file in files {
            if let Err(source) = validate_indexed_file(file) {
                // Nothing of this file was written: keep the files before it.
                return match transaction.commit() {
                    Ok(()) => Err(BatchFailure::Kept { written, source }),
                    Err(_) => Err(nothing_kept(source)),
                };
            }
            if let Err(source) = write_indexed_file(&transaction, repo, file) {
                // Dropping the transaction rolls back every file of it.
                return Err(BatchFailure::RolledBack {
                    failed_at: written,
                    source,
                });
            }
            written += 1;
            if max_hold.is_some_and(|max_hold| started.elapsed() >= max_hold) {
                break;
            }
        }
        transaction
            .commit()
            .map_err(|error| nothing_kept(error.into()))?;
        Ok(written)
    }

    /// Links every edge of `repo` that a file now present can satisfy: a
    /// resolved import whose target was written after it (`pending_target`),
    /// and an import whose target was deleted and has been written again
    /// (`target_deleted`). Each becomes `resolved` with no detail, exactly
    /// as [`Store::replace_indexed_file_with_calls`] links one file's.
    ///
    /// One pass over the unlinked edges in id order, through the
    /// `to_file_id` index and the files' `(repo_id, path)` key, so its work
    /// grows with the unlinked edges, not with the files written. The pass
    /// commits every 32,768 unlinked edges, which bounds how
    /// long it holds the write lock. Idempotent: running it again changes
    /// nothing, and running it after an interrupted index repairs the links
    /// that run did not make. Returns the number of edges linked.
    pub fn link_pending_edges(&self, repo: RepoId) -> Result<usize, StoreError> {
        self.link_pending_edges_paced(repo, &mut WritePacer::default())
    }

    /// [`Store::link_pending_edges`] with the caller's [`WritePacer`], so a
    /// run's file writes and its link pass share one bound on continuous
    /// lock use.
    pub fn link_pending_edges_paced(
        &self,
        repo: RepoId,
        pacer: &mut WritePacer,
    ) -> Result<usize, StoreError> {
        self.link_pending_edges_in_chunks(repo, LINK_CHUNK_EDGES, pacer)
    }

    fn link_pending_edges_in_chunks(
        &self,
        repo: RepoId,
        chunk: i64,
        pacer: &mut WritePacer,
    ) -> Result<usize, StoreError> {
        let update = format!(
            "UPDATE edges SET to_file_id = target.id, resolution_status = 'resolved', resolution_detail = NULL FROM files AS target WHERE edges.id > ?2 AND edges.id <= ?3 AND edges.repo_id = ?1 AND {LINKABLE_EDGE} AND target.repo_id = ?1 AND target.path = edges.to_path"
        );
        let mut linked = 0;
        let mut after = 0_i64;
        loop {
            let (chunk_linked, last) = pacer.write(|| -> Result<_, StoreError> {
                let mut connection = self.connection()?;
                let transaction =
                    connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
                ensure_repo_exists(&transaction, repo)?;
                // The last unlinked edge of this chunk; none means the chunk
                // runs to the end of the table.
                let last = transaction
                    .query_row(
                        "SELECT id FROM edges WHERE to_file_id IS NULL AND id > ?1 ORDER BY id LIMIT 1 OFFSET ?2",
                        params![after, chunk - 1],
                        |row| row.get::<_, i64>(0),
                    )
                    .optional()?;
                let chunk_linked = transaction
                    .execute(&update, params![repo, after, last.unwrap_or(i64::MAX)])?;
                transaction.commit()?;
                Ok((chunk_linked, last))
            })?;
            linked += chunk_linked;
            match last {
                Some(last) => after = last,
                None => return Ok(linked),
            }
        }
    }
}

/// Unlinked edges examined per transaction by [`Store::link_pending_edges`]
/// (about 0.15 s of lock hold on the reference machine, D106).
const LINK_CHUNK_EDGES: i64 = 32_768;

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::{
        EdgeKind, EdgeRecord, EdgeResolution, FileLanguage, FileRecord, SymbolKind, SymbolRecord,
    };

    const NO_HOLD_LIMIT: Duration = Duration::from_secs(3600);

    fn store() -> (tempfile::TempDir, Store, RepoId) {
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let store = Store::open(&directory.path().join("compresso.db")).expect("store should open");
        let repo = store
            .approve_repo(directory.path())
            .expect("repository should be approved");
        (directory, store, repo)
    }

    fn file(path: &str, version: &str) -> FileRecord {
        FileRecord {
            path: path.to_owned(),
            hash: format!("{version}-{path}"),
            language: Some(FileLanguage::TypeScript),
            size_bytes: 10,
            token_estimate: 2,
            indexed_at: 1,
        }
    }

    fn function(name: &str, start_line: u32) -> SymbolRecord {
        SymbolRecord {
            name: name.to_owned(),
            qualified_name: name.to_owned(),
            kind: SymbolKind::Function,
            signature: None,
            start_line,
            end_line: start_line,
            token_estimate: 1,
        }
    }

    fn import(to_path: &str, resolution: EdgeResolution) -> EdgeRecord {
        EdgeRecord {
            to_path: to_path.to_owned(),
            kind: EdgeKind::Imports,
            resolution,
        }
    }

    fn write<'a>(
        record: &'a FileRecord,
        symbols: &'a [SymbolRecord],
        edges: &'a [EdgeRecord],
    ) -> IndexedFileWrite<'a> {
        IndexedFileWrite {
            record,
            symbols,
            edges,
            calls: &[],
        }
    }

    /// One stored edge: (from, to_path, linked target, status, detail).
    type StoredEdge = (String, String, Option<String>, String, Option<String>);

    /// Every edge of the repository, sorted.
    fn edges(store: &Store) -> Vec<StoredEdge> {
        let connection = store.connection().expect("connection");
        let mut statement = connection
            .prepare("SELECT source.path, e.to_path, target.path, e.resolution_status, e.resolution_detail FROM edges AS e JOIN files AS source ON source.id = e.from_file_id LEFT JOIN files AS target ON target.id = e.to_file_id ORDER BY 1, 2")
            .expect("statement");
        statement
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            })
            .expect("query")
            .collect::<Result<Vec<_>, _>>()
            .expect("rows")
    }

    #[test]
    fn test_batch_write_commits_the_files_before_a_failing_file_and_leaves_it_unchanged() {
        let (_directory, store, repo) = store();
        for path in ["a.ts", "b.ts", "c.ts"] {
            store
                .replace_indexed_file(repo, &file(path, "old"), &[function("old", 1)], &[])
                .expect("initial write");
        }
        let (a, b, c) = (
            file("a.ts", "new"),
            file("b.ts", "new"),
            file("c.ts", "new"),
        );
        let symbols = [function("new", 1)];
        // An ambiguous edge with one candidate fails after the file row was
        // upserted, so only the savepoint can undo that row.
        let invalid = [import(
            "x.ts",
            EdgeResolution::Ambiguous(vec!["x.ts".to_owned()]),
        )];
        let batch = [
            write(&a, &symbols, &[]),
            write(&b, &symbols, &invalid),
            write(&c, &symbols, &[]),
        ];

        let error = store
            .replace_indexed_files(repo, &batch, NO_HOLD_LIMIT)
            .expect_err("the second file must fail");

        assert_eq!(error.written, 1);
        assert!(matches!(
            error.source,
            StoreError::InvalidEdgeResolution { .. }
        ));
        let hashes = store.file_hashes(repo).expect("hashes");
        assert_eq!(hashes["a.ts"], "new-a.ts");
        assert_eq!(hashes["b.ts"], "old-b.ts");
        assert_eq!(hashes["c.ts"], "old-c.ts");
        for (path, name) in [("a.ts", "new"), ("b.ts", "old"), ("c.ts", "old")] {
            let symbols = store.symbols_in_file(repo, path).expect("symbols");
            assert_eq!(symbols.len(), 1, "{path}");
            assert_eq!(symbols[0].name, name, "{path}");
        }
    }

    #[test]
    fn test_batch_write_reports_a_file_that_fails_validation_without_writing_it() {
        let (_directory, store, repo) = store();
        let (a, b) = (file("a.ts", "new"), file("b.ts", "new"));
        let mut bad = function("bad", 3);
        bad.end_line = 1;
        let symbols = [function("good", 1)];
        let bad_symbols = [bad];
        let batch = [write(&a, &symbols, &[]), write(&b, &bad_symbols, &[])];

        let error = store
            .replace_indexed_files(repo, &batch, NO_HOLD_LIMIT)
            .expect_err("the second file must fail");

        assert_eq!(error.written, 1);
        let hashes = store.file_hashes(repo).expect("hashes");
        assert_eq!(hashes.len(), 1);
        assert!(hashes.contains_key("a.ts"));
    }

    #[test]
    fn test_batch_write_commits_and_returns_once_the_hold_budget_is_spent() {
        let (_directory, store, repo) = store();
        let records = (0..5)
            .map(|index| file(&format!("f{index}.ts"), "v1"))
            .collect::<Vec<_>>();
        let symbols = [function("f", 1)];
        let batch = records
            .iter()
            .map(|record| write(record, &symbols, &[]))
            .collect::<Vec<_>>();

        // A zero budget commits after every file; the caller resumes.
        let mut start = 0;
        let mut calls = 0;
        while start < batch.len() {
            let written = store
                .replace_indexed_files(repo, &batch[start..], Duration::ZERO)
                .expect("write");
            assert_eq!(written, 1);
            start += written;
            calls += 1;
        }
        assert_eq!(calls, 5);
        assert_eq!(
            store
                .replace_indexed_files(repo, &batch, NO_HOLD_LIMIT)
                .expect("write"),
            5
        );
        assert_eq!(
            store
                .replace_indexed_files(repo, &[], Duration::ZERO)
                .expect("empty"),
            0
        );
        assert_eq!(store.file_hashes(repo).expect("hashes").len(), 5);
    }

    #[test]
    fn test_link_pending_edges_links_what_a_per_file_write_links_and_nothing_else() {
        let (_directory, store, repo) = store();
        let other_root = tempfile::tempdir().expect("second directory should be created");
        let other = store
            .approve_repo(other_root.path())
            .expect("second repository should be approved");
        // The other repository has a file at the pending path; it must never
        // satisfy this repository's edge.
        store
            .replace_indexed_file(other, &file("lib/late.ts", "v1"), &[], &[])
            .expect("other repository write");
        let early = file("app/early.ts", "v1");
        let late = file("lib/late.ts", "v1");
        let gone = file("lib/gone.ts", "v1");
        let early_edges = [
            import("lib/late.ts", EdgeResolution::Resolved),
            import("lib/gone.ts", EdgeResolution::Resolved),
            import("react", EdgeResolution::Unresolved("external".to_owned())),
            import(
                "lib/missing.ts",
                EdgeResolution::Unresolved("not_found".to_owned()),
            ),
            import(
                "lib/either",
                EdgeResolution::Ambiguous(vec!["lib/late.ts".to_owned(), "lib/gone.ts".to_owned()]),
            ),
        ];
        let batch = [
            write(&early, &[], &early_edges),
            write(&late, &[], &[]),
            write(&gone, &[], &[]),
        ];
        store
            .replace_indexed_files(repo, &batch, NO_HOLD_LIMIT)
            .expect("batch write");
        // Both targets were written after the importer: pending until linked.
        let pending = edges(&store);
        assert_eq!(
            pending
                .iter()
                .filter(|edge| edge.4.as_deref() == Some("pending_target") && edge.2.is_none())
                .count(),
            2
        );

        assert_eq!(store.link_pending_edges(repo).expect("link"), 2);
        store
            .delete_files(repo, &["lib/gone.ts".to_owned()])
            .expect("delete");
        assert_eq!(store.link_pending_edges(repo).expect("relink"), 0);
        let linked = edges(&store);
        let by_target = |to: &str| {
            linked
                .iter()
                .find(|edge| edge.1 == to)
                .cloned()
                .expect("edge")
        };
        assert_eq!(
            by_target("lib/late.ts"),
            (
                "app/early.ts".to_owned(),
                "lib/late.ts".to_owned(),
                Some("lib/late.ts".to_owned()),
                "resolved".to_owned(),
                None
            )
        );
        assert_eq!(by_target("lib/gone.ts").2, None);
        assert_eq!(by_target("lib/gone.ts").3, "unresolved");
        assert_eq!(
            by_target("lib/gone.ts").4.as_deref(),
            Some("target_deleted")
        );
        assert_eq!(by_target("react").4.as_deref(), Some("external"));
        assert_eq!(by_target("lib/missing.ts").4.as_deref(), Some("not_found"));
        assert_eq!(by_target("lib/either").3, "ambiguous");

        // The deleted target comes back: its edge is linked again.
        store
            .replace_indexed_files(repo, &[write(&gone, &[], &[])], NO_HOLD_LIMIT)
            .expect("rewrite");
        assert_eq!(store.link_pending_edges(repo).expect("relink"), 1);
        assert_eq!(
            by_target_after(&store, "lib/gone.ts"),
            (Some("lib/gone.ts".to_owned()), "resolved".to_owned(), None)
        );
        assert_eq!(store.link_pending_edges(repo).expect("idempotent"), 0);
    }

    fn by_target_after(store: &Store, to: &str) -> (Option<String>, String, Option<String>) {
        let edge = edges(store)
            .into_iter()
            .find(|edge| edge.1 == to)
            .expect("edge");
        (edge.2, edge.3, edge.4)
    }

    #[test]
    fn test_link_pending_edges_matches_per_file_linking_across_chunks() {
        // The same files written per file (linking as each target lands) and
        // in one batch linked afterwards in chunks of three unlinked edges,
        // with external edges interleaved, end identical.
        let records = (0..12)
            .map(|index| file(&format!("f{index:02}.ts"), "v1"))
            .collect::<Vec<_>>();
        let edge_lists = (0..12)
            .map(|index| {
                vec![
                    import(
                        &format!("f{:02}.ts", (index + 1) % 12),
                        EdgeResolution::Resolved,
                    ),
                    import(
                        &format!("f{:02}.ts", (index + 5) % 12),
                        EdgeResolution::Resolved,
                    ),
                    import("ext", EdgeResolution::Unresolved("external".to_owned())),
                ]
            })
            .collect::<Vec<_>>();

        let (_one, per_file, repo) = store();
        for (record, edges) in records.iter().zip(&edge_lists) {
            per_file
                .replace_indexed_file(repo, record, &[], edges)
                .expect("per-file write");
        }
        let (_two, batched, batched_repo) = store();
        let batch = records
            .iter()
            .zip(&edge_lists)
            .map(|(record, edges)| write(record, &[], edges))
            .collect::<Vec<_>>();
        batched
            .replace_indexed_files(batched_repo, &batch, NO_HOLD_LIMIT)
            .expect("batch write");
        let linked = batched
            .link_pending_edges_in_chunks(batched_repo, 3, &mut WritePacer::default())
            .expect("link");

        assert!(linked > 3, "linking must span several chunks");
        assert_eq!(edges(&batched), edges(&per_file));
        assert!(
            edges(&batched)
                .iter()
                .all(|edge| edge.4.as_deref() != Some("pending_target"))
        );
    }

    #[test]
    fn test_write_pacer_leaves_the_lock_free_after_continuous_writes() {
        const WRITE: Duration = Duration::from_millis(30);
        const YIELD: Duration = Duration::from_millis(80);
        let write = || std::thread::sleep(WRITE);

        // Four back-to-back writes pass the 100 ms bound: the lock is then
        // left free for the yield.
        let mut pacer = WritePacer::new(Duration::from_millis(100), YIELD);
        let started = std::time::Instant::now();
        for _ in 0..4 {
            pacer.write(write);
        }
        assert!(pacer.pauses >= 1);
        assert!(started.elapsed() >= WRITE * 4 + YIELD);

        // Writes separated by gaps as long as the yield never pause.
        let mut pacer = WritePacer::new(Duration::from_millis(100), YIELD);
        for _ in 0..4 {
            pacer.write(write);
            std::thread::sleep(YIELD);
        }
        assert_eq!(pacer.pauses, 0);
    }
}
