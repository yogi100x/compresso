use std::collections::HashMap;
use std::path::Path;

use rusqlite::{OptionalExtension, Row, params};

use crate::repos::ensure_repo_exists;
use crate::{FileId, RepoId, Store, StoreError};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FileLanguage {
    Rust,
    TypeScript,
    Tsx,
    JavaScript,
    Python,
    Go,
    Java,
    CSharp,
    Shell,
    Text,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileRecord {
    pub path: String,
    pub hash: String,
    pub language: Option<FileLanguage>,
    pub size_bytes: u64,
    pub token_estimate: usize,
    pub indexed_at: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum RelativePathError {
    #[error("path is empty")]
    Empty,
    #[error("path is absolute")]
    Absolute,
    #[error("path contains a parent traversal component")]
    ParentTraversal,
    #[error("path contains a current-directory component")]
    CurrentDirectory,
    #[error("path contains a backslash")]
    Backslash,
    #[error("path contains a NUL byte")]
    Nul,
    #[error("path contains an empty component")]
    EmptyComponent,
}

impl FileLanguage {
    /// The stable lowercase name stored in `files.language` and shown in
    /// tool results.
    pub const fn as_str(self) -> &'static str {
        self.as_db()
    }

    pub(crate) const fn as_db(self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::TypeScript => "typescript",
            Self::Tsx => "tsx",
            Self::JavaScript => "javascript",
            Self::Python => "python",
            Self::Go => "go",
            Self::Java => "java",
            Self::CSharp => "csharp",
            Self::Shell => "shell",
            Self::Text => "text",
        }
    }

    pub(crate) fn from_db(value: &str) -> Result<Self, StoreError> {
        match value {
            "rust" => Ok(Self::Rust),
            "typescript" => Ok(Self::TypeScript),
            "tsx" => Ok(Self::Tsx),
            "javascript" => Ok(Self::JavaScript),
            "python" => Ok(Self::Python),
            "go" => Ok(Self::Go),
            "java" => Ok(Self::Java),
            "csharp" => Ok(Self::CSharp),
            "shell" => Ok(Self::Shell),
            "text" => Ok(Self::Text),
            _ => Err(StoreError::InvalidStoredValue {
                field: "files.language",
                value: value.to_owned(),
            }),
        }
    }
}

pub(crate) struct RawFileRecord {
    path: String,
    hash: String,
    language: Option<String>,
    size_bytes: i64,
    token_estimate: i64,
    indexed_at: i64,
}

impl Store {
    /// Inserts or updates a repository file without changing its stable id.
    pub fn upsert_file(&self, repo: RepoId, record: &FileRecord) -> Result<FileId, StoreError> {
        validate_relative_path(&record.path)?;
        validate_nonempty("file hash", &record.hash)?;
        let size_bytes = sqlite_integer_from_u64("file size_bytes", record.size_bytes)?;
        let token_estimate =
            sqlite_integer_from_usize("file token_estimate", record.token_estimate)?;
        let language = record.language.map(FileLanguage::as_db);
        let connection = self.connection()?;
        ensure_repo_exists(&connection, repo)?;
        connection
            .query_row(
                "INSERT INTO files (repo_id, path, hash, language, size_bytes, token_estimate, indexed_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) ON CONFLICT(repo_id, path) DO UPDATE SET hash = excluded.hash, language = excluded.language, size_bytes = excluded.size_bytes, token_estimate = excluded.token_estimate, indexed_at = excluded.indexed_at RETURNING id",
                params![
                    repo,
                    &record.path,
                    &record.hash,
                    language,
                    size_bytes,
                    token_estimate,
                    record.indexed_at,
                ],
                |row| row.get(0),
            )
            .map_err(StoreError::from)
    }

    /// Returns the last indexed content hash for every stored file in a repository.
    pub fn file_hashes(&self, repo: RepoId) -> Result<HashMap<String, String>, StoreError> {
        let connection = self.connection()?;
        ensure_repo_exists(&connection, repo)?;
        let mut statement =
            connection.prepare("SELECT path, hash FROM files WHERE repo_id = ?1 ORDER BY path")?;
        let rows = statement.query_map([repo], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect::<Result<HashMap<_, _>, _>>()
            .map_err(StoreError::from)
    }

    /// Lists all indexed files in deterministic repository-relative path order.
    pub fn indexed_files(&self, repo: RepoId) -> Result<Vec<FileRecord>, StoreError> {
        let connection = self.connection()?;
        ensure_repo_exists(&connection, repo)?;
        let mut statement = connection.prepare(
            "SELECT path, hash, language, size_bytes, token_estimate, indexed_at FROM files WHERE repo_id = ?1 ORDER BY path ASC, id ASC",
        )?;
        let raw = statement
            .query_map([repo], |row| raw_file_from_row(row, 0))?
            .collect::<Result<Vec<_>, _>>()?;
        raw.into_iter().map(decode_file_record).collect()
    }

    /// Loads one indexed file by its validated repository-relative path.
    pub fn indexed_file(&self, repo: RepoId, path: &str) -> Result<Option<FileRecord>, StoreError> {
        validate_relative_path(path)?;
        let connection = self.connection()?;
        ensure_repo_exists(&connection, repo)?;
        let raw = connection
            .query_row(
                "SELECT path, hash, language, size_bytes, token_estimate, indexed_at FROM files WHERE repo_id = ?1 AND path = ?2",
                params![repo, path],
                |row| raw_file_from_row(row, 0),
            )
            .optional()?;
        raw.map(decode_file_record).transpose()
    }

    /// Deletes repository files, cascading owned rows and downgrading incoming resolved edges.
    pub fn delete_files(&self, repo: RepoId, paths: &[String]) -> Result<(), StoreError> {
        for path in paths {
            validate_relative_path(path)?;
        }
        let mut connection = self.connection()?;
        // Immediate: a deferred transaction that reads first cannot wait for
        // another writer's lock when it upgrades (D106).
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        ensure_repo_exists(&transaction, repo)?;
        {
            let mut downgrade_incoming = transaction.prepare(
                "UPDATE edges SET to_file_id = NULL, resolution_status = 'unresolved', resolution_detail = 'target_deleted' WHERE repo_id = ?1 AND resolution_status = 'resolved' AND to_file_id = (SELECT id FROM files WHERE repo_id = ?1 AND path = ?2)",
            )?;
            let mut statement =
                transaction.prepare("DELETE FROM files WHERE repo_id = ?1 AND path = ?2")?;
            for path in paths {
                downgrade_incoming.execute(params![repo, path])?;
                statement.execute(params![repo, path])?;
            }
        }
        transaction.commit()?;
        Ok(())
    }
}

pub(crate) fn validate_relative_path(path: &str) -> Result<(), StoreError> {
    let reason = if path.is_empty() {
        Some(RelativePathError::Empty)
    } else if Path::new(path).is_absolute()
        || path.starts_with('/')
        || (path.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
            && path.as_bytes().get(1) == Some(&b':'))
    {
        Some(RelativePathError::Absolute)
    } else if path.contains('\0') {
        Some(RelativePathError::Nul)
    } else if path.contains('\\') {
        Some(RelativePathError::Backslash)
    } else if path.split('/').any(|component| component == "..") {
        Some(RelativePathError::ParentTraversal)
    } else if path.split('/').any(|component| component == ".") {
        Some(RelativePathError::CurrentDirectory)
    } else if path.split('/').any(str::is_empty) {
        Some(RelativePathError::EmptyComponent)
    } else {
        None
    };

    if let Some(reason) = reason {
        return Err(StoreError::InvalidRelativePath {
            path: path.to_owned(),
            reason,
        });
    }
    Ok(())
}

pub(crate) fn validate_nonempty(field: &'static str, value: &str) -> Result<(), StoreError> {
    if value.is_empty() {
        return Err(StoreError::InvalidField {
            field,
            reason: "must not be empty",
        });
    }
    if value.contains('\0') {
        return Err(StoreError::InvalidField {
            field,
            reason: "must not contain a NUL byte",
        });
    }
    Ok(())
}

pub(crate) fn sqlite_integer_from_u64(field: &'static str, value: u64) -> Result<i64, StoreError> {
    i64::try_from(value).map_err(|_| StoreError::NumericOutOfRange {
        field,
        value: value.into(),
    })
}

pub(crate) fn sqlite_integer_from_usize(
    field: &'static str,
    value: usize,
) -> Result<i64, StoreError> {
    i64::try_from(value).map_err(|_| StoreError::NumericOutOfRange {
        field,
        value: value as u128,
    })
}

pub(crate) fn raw_file_from_row(row: &Row<'_>, offset: usize) -> rusqlite::Result<RawFileRecord> {
    Ok(RawFileRecord {
        path: row.get(offset)?,
        hash: row.get(offset + 1)?,
        language: row.get(offset + 2)?,
        size_bytes: row.get(offset + 3)?,
        token_estimate: row.get(offset + 4)?,
        indexed_at: row.get(offset + 5)?,
    })
}

pub(crate) fn decode_file_record(raw: RawFileRecord) -> Result<FileRecord, StoreError> {
    let language = raw
        .language
        .as_deref()
        .map(FileLanguage::from_db)
        .transpose()?;
    let size_bytes = u64::try_from(raw.size_bytes).map_err(|_| StoreError::InvalidStoredValue {
        field: "files.size_bytes",
        value: raw.size_bytes.to_string(),
    })?;
    let token_estimate =
        usize::try_from(raw.token_estimate).map_err(|_| StoreError::InvalidStoredValue {
            field: "files.token_estimate",
            value: raw.token_estimate.to_string(),
        })?;
    Ok(FileRecord {
        path: raw.path,
        hash: raw.hash,
        language,
        size_bytes,
        token_estimate,
        indexed_at: raw.indexed_at,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::fs;

    use tempfile::tempdir;

    use crate::{
        EdgeKind, EdgeRecord, EdgeResolution, FileLanguage, FileRecord, RelativePathError, Store,
        StoreError, SymbolKind, SymbolRecord,
    };

    fn setup() -> (tempfile::TempDir, Store, i64) {
        let directory = tempdir().expect("temp directory should be created");
        let root = directory.path().join("repo");
        fs::create_dir(&root).expect("repository directory should be created");
        let store = Store::open(&directory.path().join("compresso.db")).expect("store should open");
        let repo = store
            .approve_repo(&root)
            .expect("repository should approve");
        (directory, store, repo)
    }

    fn file(path: &str, hash: &str) -> FileRecord {
        FileRecord {
            path: path.to_owned(),
            hash: hash.to_owned(),
            language: Some(FileLanguage::Rust),
            size_bytes: 100,
            token_estimate: 20,
            indexed_at: 10,
        }
    }

    fn symbol(name: &str) -> SymbolRecord {
        SymbolRecord {
            name: name.to_owned(),
            qualified_name: name.to_owned(),
            kind: SymbolKind::Function,
            signature: Some(format!("fn {name}()")),
            start_line: 1,
            end_line: 2,
            token_estimate: 5,
        }
    }

    #[test]
    fn test_upsert_file_updates_in_place() {
        let (_directory, store, repo) = setup();

        let first = store
            .upsert_file(repo, &file("src/lib.rs", "aa"))
            .expect("file should insert");
        let mut updated = file("src/lib.rs", "bb");
        updated.size_bytes = 200;
        let second = store
            .upsert_file(repo, &updated)
            .expect("file should update");

        assert_eq!(first, second);
        assert_eq!(
            store.file_hashes(repo).expect("hashes should load"),
            HashMap::from([("src/lib.rs".to_owned(), "bb".to_owned())])
        );
        let row: (i64, String, i64) = store
            .connection()
            .expect("connection lock should succeed")
            .query_row(
                "SELECT COUNT(*), MAX(hash), MAX(size_bytes) FROM files WHERE repo_id = ?1",
                [repo],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("file row should be readable");
        assert_eq!(row, (1, "bb".to_owned(), 200));
    }

    #[test]
    fn test_repo_relative_paths_reject_unsafe_forms() {
        let (_directory, store, repo) = setup();
        let cases = [
            ("", RelativePathError::Empty),
            ("/src/lib.rs", RelativePathError::Absolute),
            ("C:/src/lib.rs", RelativePathError::Absolute),
            ("C:src/lib.rs", RelativePathError::Absolute),
            ("C:", RelativePathError::Absolute),
            ("../src/lib.rs", RelativePathError::ParentTraversal),
            ("src/../lib.rs", RelativePathError::ParentTraversal),
            ("./src/lib.rs", RelativePathError::CurrentDirectory),
            ("src/./lib.rs", RelativePathError::CurrentDirectory),
            (".", RelativePathError::CurrentDirectory),
            ("src\\lib.rs", RelativePathError::Backslash),
            ("src\0lib.rs", RelativePathError::Nul),
            ("src//lib.rs", RelativePathError::EmptyComponent),
            ("src/", RelativePathError::EmptyComponent),
        ];

        for (path, expected) in cases {
            let error = store
                .upsert_file(repo, &file(path, "aa"))
                .expect_err("unsafe path should be rejected");
            assert!(matches!(
                error,
                StoreError::InvalidRelativePath { reason, .. } if reason == expected
            ));
        }
    }

    #[test]
    fn test_delete_files_cascades() {
        let (_directory, store, repo) = setup();
        let source = store
            .upsert_file(repo, &file("src/source.rs", "aa"))
            .expect("source file should insert");
        store
            .upsert_file(repo, &file("src/target.rs", "bb"))
            .expect("target file should insert");
        store
            .replace_symbols(
                source,
                &[symbol("alpha"), symbol("beta")],
                &[EdgeRecord {
                    to_path: "src/target.rs".to_owned(),
                    kind: EdgeKind::Imports,
                    resolution: EdgeResolution::Resolved,
                }],
            )
            .expect("symbols and edge should replace");
        assert_eq!(
            store
                .search_symbols(repo, "alpha", 10)
                .expect("symbol search should succeed")
                .len(),
            1
        );

        store
            .delete_files(repo, &["src/source.rs".to_owned()])
            .expect("source file should delete");

        let connection = store.connection().expect("connection lock should succeed");
        let symbol_count: i64 = connection
            .query_row("SELECT COUNT(*) FROM symbols", [], |row| row.get(0))
            .expect("symbol count should be readable");
        let edge_count: i64 = connection
            .query_row("SELECT COUNT(*) FROM edges", [], |row| row.get(0))
            .expect("edge count should be readable");
        let fts_count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM symbols_fts WHERE symbols_fts MATCH 'alpha'",
                [],
                |row| row.get(0),
            )
            .expect("FTS count should be readable");
        assert_eq!((symbol_count, edge_count, fts_count), (0, 0, 0));
    }

    #[test]
    fn test_delete_target_downgrades_incoming_resolved_edge() {
        let (_directory, store, repo) = setup();
        let source = store
            .upsert_file(repo, &file("src/source.rs", "aa"))
            .expect("source file should insert");
        store
            .upsert_file(repo, &file("src/target.rs", "bb"))
            .expect("target file should insert");
        store
            .replace_symbols(
                source,
                &[],
                &[EdgeRecord {
                    to_path: "src/target.rs".to_owned(),
                    kind: EdgeKind::Imports,
                    resolution: EdgeResolution::Resolved,
                }],
            )
            .expect("resolved edge should insert");

        store
            .delete_files(repo, &["src/target.rs".to_owned()])
            .expect("target file should delete");

        let connection = store.connection().expect("connection lock should succeed");
        let edge: (Option<i64>, String, Option<String>) = connection
            .query_row(
                "SELECT to_file_id, resolution_status, resolution_detail FROM edges WHERE from_file_id = ?1",
                [source],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("incoming edge should remain queryable");
        assert_eq!(
            edge,
            (
                None,
                "unresolved".to_owned(),
                Some("target_deleted".to_owned())
            )
        );
    }

    #[test]
    fn test_upsert_file_rejects_unknown_repository() {
        let (_directory, store, _repo) = setup();

        assert!(matches!(
            store.upsert_file(999, &file("src/lib.rs", "aa")),
            Err(StoreError::RepositoryNotFound(999))
        ));
    }
}
