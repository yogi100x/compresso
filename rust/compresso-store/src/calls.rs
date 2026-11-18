//! Call sites inside indexed symbols (D89, migration 0008).
//!
//! A row records what a caller wrote, never which symbol it reaches: the
//! callee is resolved by Atlas at query time against the current symbols and
//! import edges, so re-indexing one file can never leave another file's rows
//! pointing at a deleted symbol id.

use rusqlite::{Row, params, params_from_iter};

use crate::files::validate_nonempty;
use crate::repos::ensure_repo_exists;
use crate::symbols::{decode_symbol_record, raw_symbol_from_row};
use crate::{FileId, FileLanguage, RepoId, Store, StoreError, SymbolRecord};

/// Longest callee name or qualifier stored, in bytes.
pub const MAX_CALL_TEXT_BYTES: usize = 256;
/// SQLite host parameters bound per `IN (...)` chunk.
const ID_CHUNK: usize = 400;

/// How a call names its callee.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum CallForm {
    /// A bare name: `run()`, `Widget()`, `new Widget()`.
    Plain,
    /// A name behind a type or module path: `Store::open()`, `os.path.join()`.
    Path,
    /// A call on the caller's own type: `self.x()`, `Self::x()`, `this.x()`, `cls.x()`.
    SelfRef,
    /// A method call on any other expression: `store.search()`.
    Receiver,
    /// A function named as a call argument, not called there:
    /// `.map_err(to_failure)`, `map(parse, items)`, `sorted(key=rank)`.
    Reference,
}

impl CallForm {
    pub(crate) const fn as_db(self) -> &'static str {
        match self {
            Self::Plain => "plain",
            Self::Path => "path",
            Self::SelfRef => "self",
            Self::Receiver => "receiver",
            Self::Reference => "reference",
        }
    }

    fn from_db(value: &str) -> Result<Self, StoreError> {
        match value {
            "plain" => Ok(Self::Plain),
            "path" => Ok(Self::Path),
            "self" => Ok(Self::SelfRef),
            "receiver" => Ok(Self::Receiver),
            "reference" => Ok(Self::Reference),
            _ => Err(StoreError::InvalidStoredValue {
                field: "call_sites.call_form",
                value: value.to_owned(),
            }),
        }
    }
}

/// One call site to persist with its file. `caller` indexes the symbol slice
/// written in the same transaction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CallSiteRecord {
    pub caller: usize,
    pub callee_name: String,
    pub qualifier: Option<String>,
    pub form: CallForm,
    pub line: u32,
}

/// An indexed symbol with its row id and file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IndexedSymbol {
    pub id: i64,
    pub path: String,
    pub language: Option<FileLanguage>,
    pub symbol: SymbolRecord,
}

/// A stored call site with its caller's identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredCallSite {
    pub caller_symbol_id: i64,
    pub caller_qualified_name: String,
    pub from_path: String,
    pub callee_name: String,
    pub qualifier: Option<String>,
    pub form: CallForm,
    pub line: u32,
}

const SYMBOL_COLUMNS: &str = "s.id, s.name, s.qualified_name, s.kind, s.signature, s.start_line, s.end_line, s.token_estimate, f.path, f.language";
const CALL_COLUMNS: &str = "c.caller_symbol_id, caller.qualified_name, f.path, c.callee_name, c.callee_qualifier, c.call_form, c.line";

impl Store {
    /// Lists the repository's symbols with exactly this name, in path and line order.
    pub fn symbols_named(
        &self,
        repo: RepoId,
        name: &str,
    ) -> Result<Vec<IndexedSymbol>, StoreError> {
        let connection = self.connection()?;
        ensure_repo_exists(&connection, repo)?;
        let mut statement = connection.prepare_cached(&format!(
            "SELECT {SYMBOL_COLUMNS} FROM symbols AS s JOIN files AS f ON f.id = s.file_id WHERE s.name = ?1 AND f.repo_id = ?2 ORDER BY f.path ASC, s.start_line ASC, s.end_line ASC, s.qualified_name ASC, s.id ASC"
        ))?;
        let raw = statement
            .query_map(params![name, repo], indexed_symbol_row)?
            .collect::<Result<Vec<_>, _>>()?;
        raw.into_iter().map(decode_indexed_symbol).collect()
    }

    /// Loads indexed symbols by row id; ids from another repository or no
    /// longer present are skipped. Ordered by id.
    pub fn symbols_by_ids(
        &self,
        repo: RepoId,
        ids: &[i64],
    ) -> Result<Vec<IndexedSymbol>, StoreError> {
        let connection = self.connection()?;
        ensure_repo_exists(&connection, repo)?;
        let mut output = Vec::new();
        for chunk in ids.chunks(ID_CHUNK) {
            let placeholders = placeholders(chunk.len());
            let mut statement = connection.prepare(&format!(
                "SELECT {SYMBOL_COLUMNS} FROM symbols AS s JOIN files AS f ON f.id = s.file_id WHERE f.repo_id = ?1 AND s.id IN ({placeholders}) ORDER BY s.id ASC"
            ))?;
            let parameters = std::iter::once(repo).chain(chunk.iter().copied());
            let raw = statement
                .query_map(params_from_iter(parameters), indexed_symbol_row)?
                .collect::<Result<Vec<_>, _>>()?;
            for symbol in raw {
                output.push(decode_indexed_symbol(symbol)?);
            }
        }
        output.sort_by_key(|symbol| symbol.id);
        Ok(output)
    }

    /// Lists the call sites inside the given caller symbols, in caller, line,
    /// and name order.
    pub fn call_sites_from(
        &self,
        repo: RepoId,
        caller_ids: &[i64],
    ) -> Result<Vec<StoredCallSite>, StoreError> {
        let connection = self.connection()?;
        ensure_repo_exists(&connection, repo)?;
        let mut output = Vec::new();
        for chunk in caller_ids.chunks(ID_CHUNK) {
            let placeholders = placeholders(chunk.len());
            let mut statement = connection.prepare(&format!(
                "SELECT {CALL_COLUMNS} FROM call_sites AS c JOIN symbols AS caller ON caller.id = c.caller_symbol_id JOIN files AS f ON f.id = c.from_file_id WHERE c.repo_id = ?1 AND c.caller_symbol_id IN ({placeholders})"
            ))?;
            let parameters = std::iter::once(repo).chain(chunk.iter().copied());
            let raw = statement
                .query_map(params_from_iter(parameters), call_row)?
                .collect::<Result<Vec<_>, _>>()?;
            for call in raw {
                output.push(decode_call(call)?);
            }
        }
        sort_calls(&mut output);
        Ok(output)
    }

    /// Lists every call site in the repository that names `callee_name`.
    pub fn call_sites_named(
        &self,
        repo: RepoId,
        callee_name: &str,
    ) -> Result<Vec<StoredCallSite>, StoreError> {
        let connection = self.connection()?;
        ensure_repo_exists(&connection, repo)?;
        let mut statement = connection.prepare_cached(&format!(
            "SELECT {CALL_COLUMNS} FROM call_sites AS c JOIN symbols AS caller ON caller.id = c.caller_symbol_id JOIN files AS f ON f.id = c.from_file_id WHERE c.repo_id = ?1 AND c.callee_name = ?2"
        ))?;
        let raw = statement
            .query_map(params![repo, callee_name], call_row)?
            .collect::<Result<Vec<_>, _>>()?;
        let mut output = raw
            .into_iter()
            .map(decode_call)
            .collect::<Result<Vec<_>, _>>()?;
        sort_calls(&mut output);
        Ok(output)
    }

    /// Counts the repository's stored call sites.
    pub fn call_site_count(&self, repo: RepoId) -> Result<usize, StoreError> {
        let connection = self.connection()?;
        ensure_repo_exists(&connection, repo)?;
        let count: i64 = connection.query_row(
            "SELECT COUNT(*) FROM call_sites WHERE repo_id = ?1",
            [repo],
            |row| row.get(0),
        )?;
        usize::try_from(count).map_err(|_| StoreError::InvalidStoredValue {
            field: "call_sites count",
            value: count.to_string(),
        })
    }
}

pub(crate) fn validate_call_site(call: &CallSiteRecord, symbols: usize) -> Result<(), StoreError> {
    validate_nonempty("call callee_name", &call.callee_name)?;
    if call.caller >= symbols {
        return Err(StoreError::InvalidField {
            field: "call caller",
            reason: "must index a symbol written with the same file",
        });
    }
    if call.line == 0 {
        return Err(StoreError::InvalidField {
            field: "call line",
            reason: "must be one-based",
        });
    }
    for (field, value) in [
        ("call callee_name", Some(call.callee_name.as_str())),
        ("call qualifier", call.qualifier.as_deref()),
    ] {
        let Some(value) = value else { continue };
        if value.len() > MAX_CALL_TEXT_BYTES || value.contains('\0') || value.trim().is_empty() {
            return Err(StoreError::InvalidField {
                field,
                reason: "must be non-blank, at most 256 bytes, and contain no NUL byte",
            });
        }
    }
    Ok(())
}

pub(crate) fn insert_call_sites(
    transaction: &rusqlite::Transaction<'_>,
    repo: RepoId,
    file: FileId,
    symbol_ids: &[i64],
    calls: &[CallSiteRecord],
) -> Result<(), StoreError> {
    if calls.is_empty() {
        return Ok(());
    }
    let mut statement = transaction.prepare(
        "INSERT INTO call_sites (repo_id, from_file_id, caller_symbol_id, callee_name, callee_qualifier, call_form, line) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
    )?;
    for call in calls {
        let caller = symbol_ids
            .get(call.caller)
            .copied()
            .ok_or(StoreError::InvalidField {
                field: "call caller",
                reason: "must index a symbol written with the same file",
            })?;
        statement.execute(params![
            repo,
            file,
            caller,
            &call.callee_name,
            &call.qualifier,
            call.form.as_db(),
            i64::from(call.line),
        ])?;
    }
    Ok(())
}

fn placeholders(count: usize) -> String {
    (0..count)
        .map(|index| format!("?{}", index + 2))
        .collect::<Vec<_>>()
        .join(", ")
}

type RawIndexedSymbol = (crate::symbols::RawSymbolRecord, String, Option<String>);

fn indexed_symbol_row(row: &Row<'_>) -> rusqlite::Result<RawIndexedSymbol> {
    Ok((raw_symbol_from_row(row, 0)?, row.get(8)?, row.get(9)?))
}

fn decode_indexed_symbol(raw: RawIndexedSymbol) -> Result<IndexedSymbol, StoreError> {
    let (symbol, path, language) = raw;
    let (id, symbol) = decode_symbol_record(symbol)?;
    let language = language.as_deref().map(FileLanguage::from_db).transpose()?;
    Ok(IndexedSymbol {
        id,
        path,
        language,
        symbol,
    })
}

type RawCall = (i64, String, String, String, Option<String>, String, i64);

fn call_row(row: &Row<'_>) -> rusqlite::Result<RawCall> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
    ))
}

fn decode_call(raw: RawCall) -> Result<StoredCallSite, StoreError> {
    let (caller_symbol_id, caller_qualified_name, from_path, callee_name, qualifier, form, line) =
        raw;
    Ok(StoredCallSite {
        caller_symbol_id,
        caller_qualified_name,
        from_path,
        callee_name,
        qualifier,
        form: CallForm::from_db(&form)?,
        line: u32::try_from(line).map_err(|_| StoreError::InvalidStoredValue {
            field: "call_sites.line",
            value: line.to_string(),
        })?,
    })
}

fn sort_calls(calls: &mut [StoredCallSite]) {
    calls.sort_by(|left, right| {
        left.from_path
            .cmp(&right.from_path)
            .then_with(|| left.line.cmp(&right.line))
            .then_with(|| left.caller_symbol_id.cmp(&right.caller_symbol_id))
            .then_with(|| left.callee_name.cmp(&right.callee_name))
            .then_with(|| left.form.cmp(&right.form))
            .then_with(|| left.qualifier.cmp(&right.qualifier))
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FileRecord, SymbolKind};
    use tempfile::TempDir;

    fn setup() -> (TempDir, Store, RepoId) {
        let directory = tempfile::tempdir().expect("temp directory should be created");
        let store = Store::open(&directory.path().join("compresso.db")).expect("store should open");
        let repo = store
            .approve_repo(directory.path())
            .expect("repository should be approved");
        (directory, store, repo)
    }

    fn file(path: &str) -> FileRecord {
        FileRecord {
            path: path.to_owned(),
            hash: "aa".to_owned(),
            language: Some(FileLanguage::Rust),
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
            signature: Some(format!("fn {name}()")),
            start_line,
            end_line: start_line + 2,
            token_estimate: 1,
        }
    }

    fn call(caller: usize, name: &str, line: u32) -> CallSiteRecord {
        CallSiteRecord {
            caller,
            callee_name: name.to_owned(),
            qualifier: None,
            form: CallForm::Plain,
            line,
        }
    }

    #[test]
    fn test_call_sites_are_replaced_with_their_file_and_cascade_on_delete() {
        let (_directory, store, repo) = setup();
        store
            .replace_indexed_file_with_calls(
                repo,
                &file("src/a.rs"),
                &[function("first", 1), function("second", 5)],
                &[],
                &[call(0, "second", 2), call(1, "helper", 6)],
            )
            .expect("file with calls should be written");
        assert_eq!(store.call_site_count(repo).expect("count"), 2);
        let named = store.call_sites_named(repo, "second").expect("named");
        assert_eq!(named.len(), 1);
        assert_eq!(named[0].caller_qualified_name, "first");
        assert_eq!(named[0].from_path, "src/a.rs");
        assert_eq!(named[0].line, 2);

        let callees = store
            .call_sites_from(repo, &[named[0].caller_symbol_id])
            .expect("from");
        assert_eq!(callees, named);

        store
            .replace_indexed_file(repo, &file("src/a.rs"), &[function("first", 1)], &[])
            .expect("rewrite without calls should succeed");
        assert_eq!(store.call_site_count(repo).expect("count"), 0);

        store
            .replace_indexed_file_with_calls(
                repo,
                &file("src/a.rs"),
                &[function("first", 1)],
                &[],
                &[call(0, "x", 2)],
            )
            .expect("file should be written");
        store
            .delete_files(repo, &["src/a.rs".to_owned()])
            .expect("delete should succeed");
        assert_eq!(store.call_site_count(repo).expect("count"), 0);
    }

    #[test]
    fn test_invalid_call_sites_are_rejected_before_any_write() {
        let (_directory, store, repo) = setup();
        for bad in [
            call(1, "x", 2),
            call(0, "", 2),
            call(0, "x", 0),
            call(0, "a\0b", 2),
            CallSiteRecord {
                qualifier: Some("q".repeat(MAX_CALL_TEXT_BYTES + 1)),
                ..call(0, "x", 2)
            },
        ] {
            assert!(
                store
                    .replace_indexed_file_with_calls(
                        repo,
                        &file("src/a.rs"),
                        &[function("first", 1)],
                        &[],
                        &[bad],
                    )
                    .is_err()
            );
        }
        assert!(
            store
                .indexed_file(repo, "src/a.rs")
                .expect("lookup")
                .is_none()
        );
    }

    #[test]
    fn test_symbols_named_and_by_ids_stay_inside_the_repository() {
        let (_directory, store, repo) = setup();
        let other_directory = tempfile::tempdir().expect("temp directory");
        let other = store
            .approve_repo(other_directory.path())
            .expect("second repository");
        store
            .replace_indexed_file(repo, &file("src/a.rs"), &[function("run", 1)], &[])
            .expect("write");
        store
            .replace_indexed_file(other, &file("src/b.rs"), &[function("run", 1)], &[])
            .expect("write");
        let named = store.symbols_named(repo, "run").expect("named");
        assert_eq!(named.len(), 1);
        assert_eq!(named[0].path, "src/a.rs");
        let other_named = store.symbols_named(other, "run").expect("named");
        let by_id = store
            .symbols_by_ids(repo, &[named[0].id, other_named[0].id])
            .expect("by id");
        assert_eq!(by_id, named);
    }
}
