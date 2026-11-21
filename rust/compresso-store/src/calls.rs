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
/// A total order over call rows, so a `LIMIT` always keeps the same rows.
/// The callee name of one marker row a file holds when its call extraction
/// stopped at a per-file bound (E17-40, D105): not a call, and never an
/// identifier the extractors write. Every reader of call sites leaves it
/// out except [`Store::files_with_cut_call_sites`].
pub const CALL_SITES_CUT_MARKER: &str = "<call sites cut>";

const CALL_ORDER: &str = "f.path ASC, c.line ASC, c.caller_symbol_id ASC, c.callee_name ASC, c.call_form ASC, c.callee_qualifier ASC, c.id ASC";
const CALL_COLUMNS: &str = "c.caller_symbol_id, caller.qualified_name, f.path, c.callee_name, c.callee_qualifier, c.call_form, c.line";

impl Store {
    /// Lists every repository symbol with exactly this name, in path and
    /// line order, with its row id, path, and language.
    ///
    /// Distinct from [`Store::symbols_named`] on purpose: that one serves
    /// search (a validated query, a row limit, full file records, no row
    /// id); this one serves call resolution, which needs the row id to link
    /// call sites and every definition of the name, since an unlisted one
    /// could change ambiguity into a wrong answer. Its size is bounded by the
    /// number of definitions of one name.
    pub fn indexed_symbols_named(
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

    /// Lists at most `limit` call sites inside the given caller symbols, in
    /// path, line, and name order. Rows are cut in a fixed order (per chunk
    /// of 400 callers, in the order given), so the same arguments always
    /// return the same rows.
    pub fn call_sites_from(
        &self,
        repo: RepoId,
        caller_ids: &[i64],
        limit: usize,
    ) -> Result<Vec<StoredCallSite>, StoreError> {
        let connection = self.connection()?;
        ensure_repo_exists(&connection, repo)?;
        let mut output = Vec::new();
        for chunk in caller_ids.chunks(ID_CHUNK) {
            let remaining = limit.saturating_sub(output.len());
            if remaining == 0 {
                break;
            }
            let placeholders = placeholders(chunk.len());
            let mut statement = connection.prepare(&format!(
                "SELECT {CALL_COLUMNS} FROM call_sites AS c JOIN symbols AS caller ON caller.id = c.caller_symbol_id JOIN files AS f ON f.id = c.from_file_id WHERE c.repo_id = ?1 AND c.caller_symbol_id IN ({placeholders}) AND c.callee_name != '{CALL_SITES_CUT_MARKER}' ORDER BY {CALL_ORDER} LIMIT {}",
                sqlite_limit(remaining)
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

    /// Lists at most `limit` call sites in the repository that name
    /// `callee_name`, cut in path, line, and name order.
    pub fn call_sites_named(
        &self,
        repo: RepoId,
        callee_name: &str,
        limit: usize,
    ) -> Result<Vec<StoredCallSite>, StoreError> {
        let connection = self.connection()?;
        ensure_repo_exists(&connection, repo)?;
        let mut statement = connection.prepare_cached(&format!(
            "SELECT {CALL_COLUMNS} FROM call_sites AS c JOIN symbols AS caller ON caller.id = c.caller_symbol_id JOIN files AS f ON f.id = c.from_file_id WHERE c.repo_id = ?1 AND c.callee_name = ?2 AND c.callee_name != '{CALL_SITES_CUT_MARKER}' ORDER BY {CALL_ORDER} LIMIT ?3"
        ))?;
        let raw = statement
            .query_map(params![repo, callee_name, sqlite_limit(limit)], call_row)?
            .collect::<Result<Vec<_>, _>>()?;
        let mut output = raw
            .into_iter()
            .map(decode_call)
            .collect::<Result<Vec<_>, _>>()?;
        sort_calls(&mut output);
        Ok(output)
    }

    /// The paths of files with a stored plain call (`new T()`, `T(...)`)
    /// whose callee is one of `names`, or a call whose qualifier is, starts
    /// with, or ends with one of `types` as a dotted part (`Keys`,
    /// `Keys.Inner`, `a.b.Keys`), in path order: evidence that a file uses a
    /// type another file defines (E17-40, D105). Names are matched in chunks
    /// of 400 through the callee index; each chunk of types reads the
    /// repository's call sites once.
    pub fn files_with_call_sites_naming(
        &self,
        repo: RepoId,
        names: &[String],
        types: &[String],
    ) -> Result<std::collections::BTreeSet<String>, StoreError> {
        let connection = self.connection()?;
        ensure_repo_exists(&connection, repo)?;
        let mut paths = std::collections::BTreeSet::new();
        for chunk in names.chunks(ID_CHUNK) {
            let mut statement = connection.prepare(&format!(
                "SELECT DISTINCT f.path FROM call_sites AS c JOIN files AS f ON f.id = c.from_file_id WHERE c.repo_id = ?1 AND c.call_form = 'plain' AND c.callee_name IN ({})",
                placeholders(chunk.len())
            ))?;
            let parameters = std::iter::once(rusqlite::types::Value::from(repo)).chain(
                chunk
                    .iter()
                    .map(|name| rusqlite::types::Value::from(name.clone())),
            );
            for path in
                statement.query_map(params_from_iter(parameters), |row| row.get::<_, String>(0))?
            {
                paths.insert(path?);
            }
        }
        for chunk in types.chunks(ID_CHUNK / 4) {
            let conditions = (0..chunk.len())
                .map(|index| {
                    let parameter = index + 2;
                    format!(
                        "c.callee_qualifier = ?{parameter} OR substr(c.callee_qualifier, 1, length(?{parameter}) + 1) = ?{parameter} || '.' OR substr(c.callee_qualifier, -length(?{parameter}) - 1) = '.' || ?{parameter}"
                    )
                })
                .collect::<Vec<_>>()
                .join(" OR ");
            let mut statement = connection.prepare(&format!(
                "SELECT DISTINCT f.path FROM call_sites AS c JOIN files AS f ON f.id = c.from_file_id WHERE c.repo_id = ?1 AND c.callee_qualifier IS NOT NULL AND ({conditions})"
            ))?;
            let parameters = std::iter::once(rusqlite::types::Value::from(repo)).chain(
                chunk
                    .iter()
                    .map(|name| rusqlite::types::Value::from(name.clone())),
            );
            for path in
                statement.query_map(params_from_iter(parameters), |row| row.get::<_, String>(0))?
            {
                paths.insert(path?);
            }
        }
        Ok(paths)
    }

    /// The plain and self call sites in files directly inside `directory`
    /// (`""` is the repository root; files of subdirectories are left out),
    /// marker rows excluded, in path, line, and name order: what the files
    /// of a package that is a directory (Go) call (E17-46, D111). Driven
    /// from `files` by a path range on its `(repo_id, path)` key, then each
    /// file's call sites through their file index, so the rows read are the
    /// directory's own (at the root, every file row of the repository is
    /// read to find the root's files).
    pub fn call_sites_in_directory(
        &self,
        repo: RepoId,
        directory: &str,
    ) -> Result<Vec<StoredCallSite>, StoreError> {
        let connection = self.connection()?;
        ensure_repo_exists(&connection, repo)?;
        let (condition, parameters) = directory_range(repo, directory);
        let mut statement = connection.prepare(&format!(
            "SELECT {CALL_COLUMNS} FROM files AS f JOIN call_sites AS c ON c.from_file_id = f.id JOIN symbols AS caller ON caller.id = c.caller_symbol_id WHERE {condition} AND c.call_form IN ('plain', 'self') AND c.callee_name != '{CALL_SITES_CUT_MARKER}'"
        ))?;
        let raw = statement
            .query_map(params_from_iter(parameters), call_row)?
            .collect::<Result<Vec<_>, _>>()?;
        let mut output = raw
            .into_iter()
            .map(decode_call)
            .collect::<Result<Vec<_>, _>>()?;
        sort_calls(&mut output);
        Ok(output)
    }

    /// The symbols of the files directly inside `directory` (`""` is the
    /// repository root), with their paths, in path and line order (E17-46,
    /// D111). Driven from `files` as [`Store::call_sites_in_directory`] is,
    /// then each file's symbols through their file index.
    pub fn symbols_in_directory(
        &self,
        repo: RepoId,
        directory: &str,
    ) -> Result<Vec<IndexedSymbol>, StoreError> {
        let connection = self.connection()?;
        ensure_repo_exists(&connection, repo)?;
        let (condition, parameters) = directory_range(repo, directory);
        let mut statement = connection.prepare(&format!(
            "SELECT {SYMBOL_COLUMNS} FROM files AS f JOIN symbols AS s ON s.file_id = f.id WHERE {condition} ORDER BY f.path ASC, s.start_line ASC, s.end_line ASC, s.qualified_name ASC, s.id ASC"
        ))?;
        let raw = statement
            .query_map(params_from_iter(parameters), indexed_symbol_row)?
            .collect::<Result<Vec<_>, _>>()?;
        raw.into_iter().map(decode_indexed_symbol).collect()
    }

    /// The paths of files holding the [`CALL_SITES_CUT_MARKER`] row: files
    /// whose call extraction stopped at a per-file bound, so their stored
    /// call sites may miss calls written later in the file.
    pub fn files_with_cut_call_sites(
        &self,
        repo: RepoId,
    ) -> Result<std::collections::BTreeSet<String>, StoreError> {
        let connection = self.connection()?;
        ensure_repo_exists(&connection, repo)?;
        let mut statement = connection.prepare(
            "SELECT DISTINCT f.path FROM call_sites AS c JOIN files AS f ON f.id = c.from_file_id WHERE c.repo_id = ?1 AND c.callee_name = ?2",
        )?;
        let rows = statement
            .query_map(params![repo, CALL_SITES_CUT_MARKER], |row| {
                row.get::<_, String>(0)
            })?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    }

    /// Counts the repository's stored call sites.
    pub fn call_site_count(&self, repo: RepoId) -> Result<usize, StoreError> {
        let connection = self.connection()?;
        ensure_repo_exists(&connection, repo)?;
        let count: i64 = connection.query_row(
            &format!(
                "SELECT COUNT(*) FROM call_sites WHERE repo_id = ?1 AND callee_name != '{CALL_SITES_CUT_MARKER}'"
            ),
            [repo],
            |row| row.get(0),
        )?;
        usize::try_from(count).map_err(|_| StoreError::InvalidStoredValue {
            field: "call_sites count",
            value: count.to_string(),
        })
    }
}

/// Checks call sites exactly as [`Store::replace_indexed_file_with_calls`]
/// does, without writing: `symbols` is the length of the symbol slice the
/// calls index into. Lets a caller drop one file's call sites instead of
/// failing the whole write.
pub fn validate_call_sites(calls: &[CallSiteRecord], symbols: usize) -> Result<(), StoreError> {
    calls
        .iter()
        .try_for_each(|call| validate_call_site(call, symbols))
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
    transaction: &rusqlite::Connection,
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

/// A row limit SQLite accepts; larger limits mean "every row".
fn sqlite_limit(limit: usize) -> i64 {
    i64::try_from(limit).unwrap_or(i64::MAX)
}

/// An SQL condition on `files AS f` selecting the files directly inside
/// `directory` of repository `?1`, with its parameters: a range on the
/// `(repo_id, path)` key from `directory/` up to `directory0` (`0` follows
/// `/`), then no further `/`.
fn directory_range(repo: RepoId, directory: &str) -> (&'static str, Vec<rusqlite::types::Value>) {
    if directory.is_empty() {
        return (
            "f.repo_id = ?1 AND instr(f.path, '/') = 0",
            vec![rusqlite::types::Value::from(repo)],
        );
    }
    (
        "f.repo_id = ?1 AND f.path >= ?2 AND f.path < ?3 AND instr(substr(f.path, length(?2) + 1), '/') = 0",
        vec![
            rusqlite::types::Value::from(repo),
            rusqlite::types::Value::from(format!("{directory}/")),
            rusqlite::types::Value::from(format!("{directory}0")),
        ],
    )
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
        let named = store.call_sites_named(repo, "second", 10).expect("named");
        assert_eq!(named.len(), 1);
        assert_eq!(named[0].caller_qualified_name, "first");
        assert_eq!(named[0].from_path, "src/a.rs");
        assert_eq!(named[0].line, 2);

        let callees = store
            .call_sites_from(repo, &[named[0].caller_symbol_id], 10)
            .expect("from");
        assert_eq!(callees, named);
        assert!(
            store
                .call_sites_from(repo, &[named[0].caller_symbol_id], 0)
                .expect("from")
                .is_empty()
        );

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

    /// E17-46: the directory reads return only the files directly inside
    /// the directory of this repository, and are driven from the files key
    /// and the per-file indexes, never by a scan of symbols or call sites.
    #[test]
    fn test_directory_reads_stay_in_the_directory_and_use_the_file_indexes() {
        let (_directory, store, repo) = setup();
        let other_directory = tempfile::tempdir().expect("temp directory");
        let other = store
            .approve_repo(other_directory.path())
            .expect("second repository");
        for (repo, path) in [
            (repo, "store/a.rs"),
            (repo, "store/b.rs"),
            (repo, "store/deep/c.rs"),
            (repo, "store0.rs"),
            (repo, "root.rs"),
            (other, "store/a.rs"),
        ] {
            store
                .replace_indexed_file_with_calls(
                    repo,
                    &file(path),
                    &[function("run", 1)],
                    &[],
                    &[
                        call(0, "x", 2),
                        CallSiteRecord {
                            form: CallForm::Receiver,
                            ..call(0, "y", 2)
                        },
                    ],
                )
                .expect("write");
        }
        let paths = |calls: Vec<StoredCallSite>| {
            calls
                .into_iter()
                .map(|call| (call.from_path, call.callee_name))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            paths(store.call_sites_in_directory(repo, "store").expect("calls")),
            [
                ("store/a.rs".to_owned(), "x".to_owned()),
                ("store/b.rs".to_owned(), "x".to_owned())
            ]
        );
        assert_eq!(
            paths(store.call_sites_in_directory(repo, "").expect("calls")),
            [
                ("root.rs".to_owned(), "x".to_owned()),
                ("store0.rs".to_owned(), "x".to_owned())
            ]
        );
        let symbols = store.symbols_in_directory(repo, "store").expect("symbols");
        assert_eq!(
            symbols
                .iter()
                .map(|symbol| symbol.path.as_str())
                .collect::<Vec<_>>(),
            ["store/a.rs", "store/b.rs"]
        );

        let connection = store.connection().expect("connection");
        for sql in [
            format!(
                "EXPLAIN QUERY PLAN SELECT {CALL_COLUMNS} FROM files AS f JOIN call_sites AS c ON c.from_file_id = f.id JOIN symbols AS caller ON caller.id = c.caller_symbol_id WHERE {} AND c.call_form IN ('plain', 'self')",
                directory_range(repo, "store").0
            ),
            format!(
                "EXPLAIN QUERY PLAN SELECT {SYMBOL_COLUMNS} FROM files AS f JOIN symbols AS s ON s.file_id = f.id WHERE {}",
                directory_range(repo, "store").0
            ),
        ] {
            let mut statement = connection.prepare(&sql).expect("plan");
            let plan = statement
                .query_map(params![repo, "store/", "store0"], |row| {
                    row.get::<_, String>(3)
                })
                .expect("plan rows")
                .collect::<Result<Vec<_>, _>>()
                .expect("plan rows")
                .join("\n");
            assert!(
                plan.contains("SEARCH f USING") && plan.contains("path>? AND path<?"),
                "{plan}"
            );
            assert!(
                !plan.contains("SCAN c") && !plan.contains("SCAN s"),
                "{plan}"
            );
        }
    }

    #[test]
    fn test_indexed_symbols_named_and_by_ids_stay_inside_the_repository() {
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
        let named = store.indexed_symbols_named(repo, "run").expect("named");
        assert_eq!(named.len(), 1);
        assert_eq!(named[0].path, "src/a.rs");
        let other_named = store.indexed_symbols_named(other, "run").expect("named");
        let by_id = store
            .symbols_by_ids(repo, &[named[0].id, other_named[0].id])
            .expect("by id");
        assert_eq!(by_id, named);
    }
}
