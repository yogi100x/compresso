use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::{OptionalExtension, params};

use crate::files::validate_nonempty;
use crate::{Store, StoreError, unix_timestamp};

pub type RepoId = i64;
pub type FileId = i64;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RepoRecord {
    pub id: RepoId,
    pub root: PathBuf,
    pub approved_at: i64,
    pub parser_version: String,
    pub schema_version: u32,
    pub last_indexed_at: Option<i64>,
}

/// Opaque guard binding a destructive removal to the exact Store state previewed.
///
/// The connection-local write count detects mutation through this [`Store`] handle, while
/// SQLite's data version detects commits made through another connection. Callers can inspect
/// the repository that will be removed, but cannot forge the Store revision fields.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RepoRemovalPreview {
    repository: RepoRecord,
    local_total_changes: u64,
    data_version: i64,
}

impl RepoRemovalPreview {
    /// Returns the exact repository generation represented by this preview.
    #[must_use]
    pub fn repository(&self) -> &RepoRecord {
        &self.repository
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IndexVersionRecord {
    pub parser_version: String,
    pub schema_version: u32,
    pub created_at: i64,
}

impl Store {
    /// Approves an existing repository directory after filesystem canonicalization.
    pub fn approve_repo(&self, root: &Path) -> Result<RepoId, StoreError> {
        let canonical =
            fs::canonicalize(root).map_err(|source| StoreError::CanonicalizeRepository {
                path: root.to_path_buf(),
                source,
            })?;
        if !canonical.is_dir() {
            return Err(StoreError::RepositoryRootNotDirectory { path: canonical });
        }
        let root_text = path_text(&canonical)?;
        let approved_at = unix_timestamp()?;
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO repositories (root, approved_at, parser_version) VALUES (?1, ?2, 'unindexed') ON CONFLICT(root) DO NOTHING",
            params![root_text, approved_at],
        )?;
        connection
            .query_row(
                "SELECT id FROM repositories WHERE root = ?1",
                [root_text],
                |row| row.get(0),
            )
            .map_err(StoreError::from)
    }

    /// Finds a repository by its already-canonicalized root path.
    pub fn repo_by_root(&self, canonical_root: &Path) -> Result<Option<RepoRecord>, StoreError> {
        let root_text = path_text(canonical_root)?;
        let connection = self.connection()?;
        let raw = connection
            .query_row(
                "SELECT id, root, approved_at, parser_version, schema_version, last_indexed_at FROM repositories WHERE root = ?1",
                [root_text],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, Option<i64>>(5)?,
                    ))
                },
            )
            .optional()?;
        raw.map(decode_repo).transpose()
    }

    /// Lists every approved repository in deterministic canonical-root order.
    pub fn approved_repositories(&self) -> Result<Vec<RepoRecord>, StoreError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT id, root, approved_at, parser_version, schema_version, last_indexed_at FROM repositories ORDER BY root ASC, id ASC",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, Option<i64>>(5)?,
            ))
        })?;

        rows.map(|row| row.map_err(StoreError::from).and_then(decode_repo))
            .collect()
    }

    /// Prepares an approval removal bound to the exact repository and Store revision observed.
    ///
    /// Sampling occurs under an immediate transaction so the repository row and both revision
    /// counters describe one state while concurrent writers are excluded. The transaction is
    /// released before this method returns; callers may safely prompt for confirmation afterward.
    pub fn prepare_repo_removal(
        &self,
        canonical_root: &Path,
    ) -> Result<Option<RepoRemovalPreview>, StoreError> {
        if !canonical_root.is_absolute() {
            return Err(StoreError::RepositoryRootNotAbsolute {
                path: canonical_root.to_path_buf(),
            });
        }
        let root_text = path_text(canonical_root)?;
        let mut connection = self.connection()?;
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let data_version: i64 =
            transaction.pragma_query_value(None, "data_version", |row| row.get(0))?;
        let local_total_changes = transaction.total_changes();
        let raw = transaction
            .query_row(
                "SELECT id, root, approved_at, parser_version, schema_version, last_indexed_at FROM repositories WHERE root = ?1",
                [root_text],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, Option<i64>>(5)?,
                    ))
                },
            )
            .optional()?;
        let repository = raw.map(decode_repo).transpose()?;
        transaction.commit()?;
        Ok(repository.map(|repository| RepoRemovalPreview {
            repository,
            local_total_changes,
            data_version,
        }))
    }

    /// Applies a prepared removal only if the repository and Store revision are unchanged.
    ///
    /// An immediate transaction excludes new writers before the revision checks and exact row
    /// comparison. Any intervening write fails closed, including remove/re-approve cycles whose
    /// replacement row happens to reuse the same SQLite id and timestamp.
    pub fn remove_repo_if_unchanged(
        &self,
        preview: &RepoRemovalPreview,
    ) -> Result<RepoRecord, StoreError> {
        let mut connection = self.connection()?;
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let data_version: i64 =
            transaction.pragma_query_value(None, "data_version", |row| row.get(0))?;
        let local_total_changes = transaction.total_changes();
        if data_version != preview.data_version
            || local_total_changes != preview.local_total_changes
        {
            return Err(StoreError::RepositoryApprovalChanged {
                path: preview.repository.root.clone(),
            });
        }

        let root_text = path_text(&preview.repository.root)?;
        let current = transaction
            .query_row(
                "SELECT id, root, approved_at, parser_version, schema_version, last_indexed_at FROM repositories WHERE root = ?1",
                [root_text],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, Option<i64>>(5)?,
                    ))
                },
            )
            .optional()?
            .map(decode_repo)
            .transpose()?;
        if current.as_ref() != Some(&preview.repository) {
            return Err(StoreError::RepositoryApprovalChanged {
                path: preview.repository.root.clone(),
            });
        }

        let removed = transaction.execute(
            "DELETE FROM repositories WHERE id = ?1 AND root = ?2",
            params![preview.repository.id, root_text],
        )?;
        if removed != 1 {
            return Err(StoreError::RepositoryApprovalChanged {
                path: preview.repository.root.clone(),
            });
        }
        transaction.commit()?;
        Ok(preview.repository.clone())
    }

    /// Removes the repository whose persisted canonical root exactly matches `canonical_root`.
    ///
    /// This intentionally performs no filesystem canonicalization so an approval can be
    /// removed after its on-disk directory has disappeared. Callers must pass an absolute
    /// root; SQLite equality supplies the exact-match boundary and foreign keys cascade all
    /// repository-owned index rows in the same transaction.
    pub fn remove_repo(&self, canonical_root: &Path) -> Result<Option<RepoRecord>, StoreError> {
        let Some(preview) = self.prepare_repo_removal(canonical_root)? else {
            return Ok(None);
        };
        self.remove_repo_if_unchanged(&preview).map(Some)
    }

    /// Loads one approved repository by its stable identifier.
    pub fn repository(&self, repo: RepoId) -> Result<RepoRecord, StoreError> {
        let connection = self.connection()?;
        let raw = connection
            .query_row(
                "SELECT id, root, approved_at, parser_version, schema_version, last_indexed_at FROM repositories WHERE id = ?1",
                [repo],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, Option<i64>>(5)?,
                    ))
                },
            )
            .optional()?
            .ok_or(StoreError::RepositoryNotFound(repo))?;
        decode_repo(raw)
    }

    /// Returns the most recently stamped parser/schema version for a repository.
    pub fn latest_index_version(
        &self,
        repo: RepoId,
    ) -> Result<Option<IndexVersionRecord>, StoreError> {
        let connection = self.connection()?;
        ensure_repo_exists(&connection, repo)?;
        let raw = connection
            .query_row(
                "SELECT parser_version, schema_version, created_at FROM index_versions WHERE repo_id = ?1 ORDER BY created_at DESC, rowid DESC LIMIT 1",
                [repo],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                },
            )
            .optional()?;
        raw.map(decode_index_version).transpose()
    }

    /// Appends an index-version stamp and updates the repository's current version atomically.
    pub fn stamp_index_version(
        &self,
        repo: RepoId,
        parser_version: &str,
        schema_version: u32,
    ) -> Result<(), StoreError> {
        validate_nonempty("index parser_version", parser_version)?;
        if schema_version == 0 {
            return Err(StoreError::InvalidField {
                field: "index schema_version",
                reason: "must be positive",
            });
        }
        let created_at = unix_timestamp()?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        ensure_repo_exists(&transaction, repo)?;
        transaction.execute(
            "INSERT INTO index_versions (repo_id, parser_version, schema_version, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![repo, parser_version, i64::from(schema_version), created_at],
        )?;
        transaction.execute(
            "UPDATE repositories SET parser_version = ?2, schema_version = ?3, last_indexed_at = ?4 WHERE id = ?1",
            params![repo, parser_version, i64::from(schema_version), created_at],
        )?;
        transaction.commit()?;
        Ok(())
    }
}

pub(crate) fn ensure_repo_exists(
    connection: &rusqlite::Connection,
    repo: RepoId,
) -> Result<(), StoreError> {
    let exists = connection
        .query_row("SELECT 1 FROM repositories WHERE id = ?1", [repo], |_| {
            Ok(())
        })
        .optional()?
        .is_some();
    if !exists {
        return Err(StoreError::RepositoryNotFound(repo));
    }
    Ok(())
}

fn path_text(path: &Path) -> Result<&str, StoreError> {
    path.to_str().ok_or_else(|| StoreError::NonUtf8Path {
        path: path.to_path_buf(),
    })
}

fn decode_repo(
    raw: (i64, String, i64, String, i64, Option<i64>),
) -> Result<RepoRecord, StoreError> {
    let (id, root, approved_at, parser_version, schema_version, last_indexed_at) = raw;
    let schema_version =
        u32::try_from(schema_version).map_err(|_| StoreError::InvalidStoredValue {
            field: "repositories.schema_version",
            value: schema_version.to_string(),
        })?;
    Ok(RepoRecord {
        id,
        root: PathBuf::from(root),
        approved_at,
        parser_version,
        schema_version,
        last_indexed_at,
    })
}

fn decode_index_version(raw: (String, i64, i64)) -> Result<IndexVersionRecord, StoreError> {
    let (parser_version, schema_version, created_at) = raw;
    let schema_version =
        u32::try_from(schema_version).map_err(|_| StoreError::InvalidStoredValue {
            field: "index_versions.schema_version",
            value: schema_version.to_string(),
        })?;
    if schema_version == 0 {
        return Err(StoreError::InvalidStoredValue {
            field: "index_versions.schema_version",
            value: schema_version.to_string(),
        });
    }
    Ok(IndexVersionRecord {
        parser_version,
        schema_version,
        created_at,
    })
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use tempfile::tempdir;

    use crate::{
        EdgeKind, EdgeRecord, EdgeResolution, FileLanguage, FileRecord, Store, StoreError,
        SymbolKind, SymbolRecord,
    };

    #[test]
    fn test_approve_repo_idempotent() {
        let directory = tempdir().expect("temp directory should be created");
        let repo_root = directory.path().join("x");
        fs::create_dir(&repo_root).expect("repository directory should be created");
        let alias = repo_root.join("..").join("x");
        let database = directory.path().join("compresso.db");
        let store = Store::open(&database).expect("store should open");

        let first = store.approve_repo(&alias).expect("alias should approve");
        let second = store
            .approve_repo(&repo_root)
            .expect("canonical path should approve");

        assert_eq!(first, second);
        let canonical = fs::canonicalize(repo_root).expect("repository should canonicalize");
        let record = store
            .repo_by_root(&canonical)
            .expect("repository lookup should succeed")
            .expect("repository should exist");
        assert_eq!(record.id, first);
        assert_eq!(record.root, canonical);
        assert_eq!(record.parser_version, "unindexed");
        assert_eq!(record.schema_version, 1);
        assert_eq!(record.last_indexed_at, None);
        assert_eq!(
            store
                .repository(first)
                .expect("repository id lookup should succeed"),
            record
        );
        assert!(matches!(
            store.repository(i64::MAX),
            Err(StoreError::RepositoryNotFound(id)) if id == i64::MAX
        ));

        let count: i64 = store
            .connection()
            .expect("connection lock should succeed")
            .query_row("SELECT COUNT(*) FROM repositories", [], |row| row.get(0))
            .expect("repository count should be readable");
        assert_eq!(count, 1);
    }

    #[test]
    fn test_approve_repo_rejects_missing_path_and_file() {
        let directory = tempdir().expect("temp directory should be created");
        let store = Store::open(&directory.path().join("compresso.db")).expect("store should open");
        let missing = directory.path().join("missing");
        assert!(matches!(
            store.approve_repo(&missing),
            Err(StoreError::CanonicalizeRepository { .. })
        ));

        let file = directory.path().join("file.txt");
        fs::write(&file, "content").expect("test file should be written");
        let canonical_file = fs::canonicalize(&file).expect("test file should canonicalize");
        assert!(matches!(
            store.approve_repo(&file),
            Err(StoreError::RepositoryRootNotDirectory { path }) if path == canonical_file
        ));
    }

    #[test]
    fn test_approved_repositories_sorted_by_root() {
        let directory = tempdir().expect("temp directory should be created");
        let alpha = directory.path().join("alpha");
        let zeta = directory.path().join("zeta");
        fs::create_dir(&alpha).expect("alpha repository should be created");
        fs::create_dir(&zeta).expect("zeta repository should be created");
        let store = Store::open(&directory.path().join("compresso.db")).expect("store should open");

        let zeta_id = store.approve_repo(&zeta).expect("zeta should approve");
        let alpha_id = store.approve_repo(&alpha).expect("alpha should approve");

        let repositories = store
            .approved_repositories()
            .expect("approved repositories should list");
        assert_eq!(repositories.len(), 2);
        assert_eq!(repositories[0].id, alpha_id);
        assert_eq!(repositories[1].id, zeta_id);
        assert_eq!(
            repositories
                .iter()
                .map(|repository| repository.root.clone())
                .collect::<Vec<_>>(),
            vec![
                fs::canonicalize(alpha).expect("alpha should canonicalize"),
                fs::canonicalize(zeta).expect("zeta should canonicalize")
            ]
        );
    }

    #[test]
    fn test_remove_repo_exact_match_preserves_adjacent_prefix() {
        let directory = tempdir().expect("temp directory should be created");
        let root = directory.path().join("repo");
        let adjacent = directory.path().join("repo-copy");
        fs::create_dir(&root).expect("repository should be created");
        fs::create_dir(&adjacent).expect("adjacent repository should be created");
        let store = Store::open(&directory.path().join("compresso.db")).expect("store should open");
        let removed_id = store
            .approve_repo(&root)
            .expect("repository should approve");
        let adjacent_id = store
            .approve_repo(&adjacent)
            .expect("adjacent repository should approve");
        let canonical = fs::canonicalize(&root).expect("repository should canonicalize");
        let adjacent_canonical =
            fs::canonicalize(&adjacent).expect("adjacent repository should canonicalize");

        let removed = store
            .remove_repo(&canonical)
            .expect("exact repository removal should succeed")
            .expect("exact repository should be removed");

        assert_eq!(removed.id, removed_id);
        assert_eq!(removed.root, canonical);
        assert_eq!(
            store
                .repo_by_root(&adjacent_canonical)
                .expect("adjacent lookup should succeed")
                .expect("adjacent repository should remain")
                .id,
            adjacent_id
        );
        assert_eq!(
            store
                .remove_repo(&directory.path().join("repo-child"))
                .expect("unknown exact root should be handled"),
            None
        );
        assert!(matches!(
            store.remove_repo(Path::new("repo")),
            Err(StoreError::RepositoryRootNotAbsolute { path }) if path == Path::new("repo")
        ));
    }

    #[test]
    fn test_remove_repo_cascades_indexed_rows_after_directory_disappears() {
        let directory = tempdir().expect("temp directory should be created");
        let root = directory.path().join("repo");
        fs::create_dir(&root).expect("repository should be created");
        let canonical = fs::canonicalize(&root).expect("repository should canonicalize");
        let store = Store::open(&directory.path().join("compresso.db")).expect("store should open");
        let repo = store
            .approve_repo(&root)
            .expect("repository should approve");
        let target = FileRecord {
            path: "src/target.rs".to_owned(),
            hash: "target-hash".to_owned(),
            language: Some(FileLanguage::Rust),
            size_bytes: 20,
            token_estimate: 5,
            indexed_at: 1,
        };
        store
            .replace_indexed_file(
                repo,
                &target,
                &[SymbolRecord {
                    name: "Target".to_owned(),
                    qualified_name: "Target".to_owned(),
                    kind: SymbolKind::Struct,
                    signature: Some("pub struct Target".to_owned()),
                    start_line: 1,
                    end_line: 1,
                    token_estimate: 3,
                }],
                &[],
            )
            .expect("target file should index");
        let source = FileRecord {
            path: "src/source.rs".to_owned(),
            hash: "source-hash".to_owned(),
            language: Some(FileLanguage::Rust),
            size_bytes: 40,
            token_estimate: 10,
            indexed_at: 1,
        };
        store
            .replace_indexed_file(
                repo,
                &source,
                &[SymbolRecord {
                    name: "source".to_owned(),
                    qualified_name: "source".to_owned(),
                    kind: SymbolKind::Function,
                    signature: Some("pub fn source()".to_owned()),
                    start_line: 1,
                    end_line: 2,
                    token_estimate: 5,
                }],
                &[EdgeRecord {
                    to_path: target.path.clone(),
                    kind: EdgeKind::References,
                    resolution: EdgeResolution::Resolved,
                }],
            )
            .expect("source file should index");
        store
            .stamp_index_version(repo, "parser-v1", 1)
            .expect("index version should stamp");
        fs::remove_dir(&root).expect("empty repository directory should be removable");

        let removed = store
            .remove_repo(&canonical)
            .expect("stored root should remove without filesystem access")
            .expect("approved repository should be removed");

        assert_eq!(removed.id, repo);
        assert_eq!(removed.root, canonical);
        assert!(
            store
                .repo_by_root(&removed.root)
                .expect("removed root lookup should succeed")
                .is_none()
        );
        let connection = store.connection().expect("connection lock should succeed");
        for table in [
            "repositories",
            "files",
            "symbols",
            "symbols_fts",
            "edges",
            "index_versions",
        ] {
            let count: i64 = connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .expect("cascade count should be readable");
            assert_eq!(count, 0, "{table} should be empty after removal");
        }
    }

    #[test]
    fn test_prepared_removal_rejects_cross_connection_reapproval_generation() {
        let directory = tempdir().expect("temp directory should be created");
        let root = directory.path().join("repo");
        fs::create_dir(&root).expect("repository should be created");
        let source = root.join("source.rs");
        fs::write(&source, "pub fn remains_on_disk() {}\n")
            .expect("repository source should be written");
        let canonical = fs::canonicalize(&root).expect("repository should canonicalize");
        let database = directory.path().join("compresso.db");
        let store = Store::open(&database).expect("store should open");
        let external = Store::open(&database).expect("second store connection should open");
        let original_id = store
            .approve_repo(&root)
            .expect("repository should approve");
        let preview = store
            .prepare_repo_removal(&canonical)
            .expect("removal should prepare")
            .expect("repository should be approved");

        external
            .remove_repo(&canonical)
            .expect("original approval should remove")
            .expect("original approval should exist");
        let replacement_id = external
            .approve_repo(&root)
            .expect("repository should re-approve");

        assert_eq!(
            replacement_id, original_id,
            "SQLite should reuse the sole row id"
        );
        assert!(matches!(
            store.remove_repo_if_unchanged(&preview),
            Err(StoreError::RepositoryApprovalChanged { path }) if path == canonical
        ));
        assert_eq!(
            store
                .repo_by_root(&canonical)
                .expect("replacement lookup should succeed")
                .expect("replacement approval should remain")
                .id,
            replacement_id
        );
        assert_eq!(
            fs::read_to_string(source).expect("repository source should remain"),
            "pub fn remains_on_disk() {}\n"
        );
    }

    #[test]
    fn test_index_version_stamp_updates_ledger_and_repository() {
        let directory = tempdir().expect("temp directory should be created");
        let root = directory.path().join("repo");
        fs::create_dir(&root).expect("repository should be created");
        let store = Store::open(&directory.path().join("compresso.db")).expect("store should open");
        let repo = store
            .approve_repo(&root)
            .expect("repository should approve");
        assert_eq!(
            store
                .latest_index_version(repo)
                .expect("version lookup should succeed"),
            None
        );

        store
            .stamp_index_version(repo, "0.0.2", 1)
            .expect("version should stamp");

        let version = store
            .latest_index_version(repo)
            .expect("version lookup should succeed")
            .expect("version should exist");
        assert_eq!(version.parser_version, "0.0.2");
        assert_eq!(version.schema_version, 1);
        assert!(version.created_at > 0);
        let canonical = fs::canonicalize(root).expect("root should canonicalize");
        let repository = store
            .repo_by_root(&canonical)
            .expect("repository lookup should succeed")
            .expect("repository should exist");
        assert_eq!(repository.parser_version, "0.0.2");
        assert_eq!(repository.schema_version, 1);
        assert_eq!(repository.last_indexed_at, Some(version.created_at));
    }
}
