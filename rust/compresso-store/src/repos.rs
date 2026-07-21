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

    use tempfile::tempdir;

    use crate::{Store, StoreError};

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
