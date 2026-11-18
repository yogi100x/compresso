use std::fmt::Write;

use rusqlite::{OptionalExtension, Row, params};

use crate::calls::{CallSiteRecord, insert_call_sites, validate_call_site};
use crate::files::{
    decode_file_record, raw_file_from_row, sqlite_integer_from_u64, sqlite_integer_from_usize,
    validate_nonempty, validate_relative_path,
};
use crate::repos::ensure_repo_exists;
use crate::{FileId, FileLanguage, FileRecord, RepoId, Store, StoreError};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum SymbolKind {
    Function,
    Method,
    Struct,
    Class,
    Enum,
    Trait,
    Interface,
    Const,
    Type,
    Module,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SymbolRecord {
    pub name: String,
    pub qualified_name: String,
    pub kind: SymbolKind,
    pub signature: Option<String>,
    pub start_line: u32,
    pub end_line: u32,
    pub token_estimate: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EdgeKind {
    Imports,
    References,
    Tests,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EdgeResolution {
    Resolved,
    Ambiguous(Vec<String>),
    Unresolved(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EdgeRecord {
    pub to_path: String,
    pub kind: EdgeKind,
    pub resolution: EdgeResolution,
}

/// One fully resolved stored dependency edge, named by repository-relative paths.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedEdgeRecord {
    pub from_path: String,
    pub to_path: String,
    pub kind: EdgeKind,
}

/// Detail of an unresolved edge whose target lies outside the repository: the
/// standard library or a dependency the importing crate declares.
pub const EDGE_DETAIL_EXTERNAL: &str = "external";
/// Detail of an unresolved edge that names the importing file itself.
pub const EDGE_DETAIL_SAME_FILE: &str = "same_file";

/// Unresolved edges with these details cannot change which repository files
/// depend on each other, so they do not make a dependency answer incomplete.
const CLOSURE_NEUTRAL_UNRESOLVED: &str = "(resolution_status = 'unresolved' AND COALESCE(resolution_detail, '') IN ('external', 'same_file'))";

/// Counts unresolved and ambiguous dependency edges for completeness reporting.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct EdgeResolutionCounts {
    /// Unresolved edges that may hide a dependent.
    pub unresolved: usize,
    pub ambiguous: usize,
    /// Unresolved edges known to point outside the repository or at the
    /// importing file; not counted in `unresolved`.
    pub external: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SymbolHit {
    pub file: FileRecord,
    pub symbol: SymbolRecord,
    pub rank: f64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SymbolSelector {
    pub path: String,
    pub qualified_name: String,
    pub kind: Option<SymbolKind>,
    pub signature_hash: Option<String>,
    pub start_line_hint: Option<u32>,
}

impl SymbolKind {
    pub(crate) const fn as_db(self) -> &'static str {
        match self {
            Self::Function => "function",
            Self::Method => "method",
            Self::Struct => "struct",
            Self::Class => "class",
            Self::Enum => "enum",
            Self::Trait => "trait",
            Self::Interface => "interface",
            Self::Const => "const",
            Self::Type => "type",
            Self::Module => "module",
        }
    }

    fn from_db(value: &str) -> Result<Self, StoreError> {
        match value {
            "function" => Ok(Self::Function),
            "method" => Ok(Self::Method),
            "struct" => Ok(Self::Struct),
            "class" => Ok(Self::Class),
            "enum" => Ok(Self::Enum),
            "trait" => Ok(Self::Trait),
            "interface" => Ok(Self::Interface),
            "const" => Ok(Self::Const),
            "type" => Ok(Self::Type),
            "module" => Ok(Self::Module),
            _ => Err(StoreError::InvalidStoredValue {
                field: "symbols.kind",
                value: value.to_owned(),
            }),
        }
    }
}

impl EdgeKind {
    const fn as_db(self) -> &'static str {
        match self {
            Self::Imports => "imports",
            Self::References => "references",
            Self::Tests => "tests",
        }
    }

    fn from_db(value: &str) -> Result<Self, StoreError> {
        match value {
            "imports" => Ok(Self::Imports),
            "references" => Ok(Self::References),
            "tests" => Ok(Self::Tests),
            _ => Err(StoreError::InvalidStoredValue {
                field: "edges.kind",
                value: value.to_owned(),
            }),
        }
    }
}

pub(crate) struct RawSymbolRecord {
    id: i64,
    name: String,
    qualified_name: String,
    kind: String,
    signature: Option<String>,
    start_line: i64,
    end_line: i64,
    token_estimate: i64,
}

struct PreparedEdge {
    to_path: String,
    to_file_id: Option<FileId>,
    resolution_status: &'static str,
    resolution_detail: Option<String>,
    kind: &'static str,
}

impl Store {
    /// Atomically replaces a file's symbols and outgoing dependency edges.
    pub fn replace_symbols(
        &self,
        file: FileId,
        symbols: &[SymbolRecord],
        edges: &[EdgeRecord],
    ) -> Result<(), StoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let repo = transaction
            .query_row("SELECT repo_id FROM files WHERE id = ?1", [file], |row| {
                row.get(0)
            })
            .optional()?
            .ok_or(StoreError::FileNotFound(file))?;
        replace_symbols_in_transaction(&transaction, file, repo, symbols, edges, false)?;
        transaction.commit()?;
        Ok(())
    }

    /// Atomically upserts one indexed file and replaces all of its derived rows.
    pub fn replace_indexed_file(
        &self,
        repo: RepoId,
        record: &FileRecord,
        symbols: &[SymbolRecord],
        edges: &[EdgeRecord],
    ) -> Result<FileId, StoreError> {
        self.replace_indexed_file_with_calls(repo, record, symbols, edges, &[])
    }

    /// Atomically upserts one indexed file and replaces all of its derived
    /// rows, including the call sites inside its symbols (D89).
    pub fn replace_indexed_file_with_calls(
        &self,
        repo: RepoId,
        record: &FileRecord,
        symbols: &[SymbolRecord],
        edges: &[EdgeRecord],
        calls: &[CallSiteRecord],
    ) -> Result<FileId, StoreError> {
        validate_relative_path(&record.path)?;
        for call in calls {
            validate_call_site(call, symbols.len())?;
        }
        validate_nonempty("file hash", &record.hash)?;
        let size_bytes = sqlite_integer_from_u64("file size_bytes", record.size_bytes)?;
        let token_estimate =
            sqlite_integer_from_usize("file token_estimate", record.token_estimate)?;
        let language = record.language.map(|language| language.as_db());
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        ensure_repo_exists(&transaction, repo)?;
        let file = transaction.query_row(
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
        )?;
        let symbol_ids =
            replace_symbols_in_transaction(&transaction, file, repo, symbols, edges, true)?;
        insert_call_sites(&transaction, repo, file, &symbol_ids, calls)?;
        transaction.execute(
            "UPDATE edges SET to_file_id = ?3, resolution_status = 'resolved', resolution_detail = NULL WHERE repo_id = ?1 AND to_path = ?2 AND to_file_id IS NULL AND (resolution_status = 'resolved' OR (resolution_status = 'unresolved' AND resolution_detail = 'target_deleted'))",
            params![repo, &record.path, file],
        )?;
        transaction.commit()?;
        Ok(file)
    }

    /// Searches indexed symbol fields using a literal FTS5 trigram expression.
    pub fn search_symbols(
        &self,
        repo: RepoId,
        query: &str,
        limit: usize,
    ) -> Result<Vec<SymbolHit>, StoreError> {
        self.search_symbols_with_excluded_language(repo, query, limit, None)
    }

    /// Searches for symbols that contain every whitespace-separated term of
    /// `query`, in any order and any letter case.
    ///
    /// Symbols containing the query as one exact phrase rank first, followed by
    /// the remaining all-terms matches; each group is bm25-ordered. A one-word
    /// query is exactly [`Store::search_symbols`].
    pub fn search_symbols_all_terms(
        &self,
        repo: RepoId,
        query: &str,
        limit: usize,
    ) -> Result<Vec<SymbolHit>, StoreError> {
        let mut hits = self.search_symbols(repo, query, limit)?;
        let Some(terms) = MultiWordQuery::parse(query) else {
            return Ok(hits);
        };
        // Widening passes, most specific first: the phrase with punctuation
        // and spacing normalized, then every term in any order.
        for expression in [fts_literal(&terms.phrase), terms.all_terms] {
            if hits.len() >= limit {
                break;
            }
            let window = limit.saturating_add(hits.len());
            for hit in self.search_symbols_matching(repo, &expression, window, None)? {
                if hits.len() == limit {
                    break;
                }
                let already_ranked = hits
                    .iter()
                    .any(|ranked| ranked.file.path == hit.file.path && ranked.symbol == hit.symbol);
                if !already_ranked {
                    hits.push(hit);
                }
            }
        }
        Ok(hits)
    }

    /// Searches indexed symbol fields while excluding one persisted language
    /// before ranking and limiting the result set.
    pub fn search_symbols_excluding_language(
        &self,
        repo: RepoId,
        query: &str,
        limit: usize,
        excluded_language: FileLanguage,
    ) -> Result<Vec<SymbolHit>, StoreError> {
        self.search_symbols_with_excluded_language(repo, query, limit, Some(excluded_language))
    }

    fn search_symbols_with_excluded_language(
        &self,
        repo: RepoId,
        query: &str,
        limit: usize,
        excluded_language: Option<FileLanguage>,
    ) -> Result<Vec<SymbolHit>, StoreError> {
        validate_search_query(query)?;
        self.search_symbols_matching(repo, &fts_literal(query), limit, excluded_language)
    }

    /// Runs one already-escaped FTS5 expression against the symbol index.
    fn search_symbols_matching(
        &self,
        repo: RepoId,
        fts_query: &str,
        limit: usize,
        excluded_language: Option<FileLanguage>,
    ) -> Result<Vec<SymbolHit>, StoreError> {
        let limit = sqlite_integer_from_usize("symbol search limit", limit)?;
        let connection = self.connection()?;
        ensure_repo_exists(&connection, repo)?;
        if limit == 0 {
            return Ok(Vec::new());
        }

        let excluded_language = excluded_language.map(FileLanguage::as_db);
        let mut statement = connection.prepare(
            "SELECT f.path, f.hash, f.language, f.size_bytes, f.token_estimate, f.indexed_at, s.id, s.name, s.qualified_name, s.kind, s.signature, s.start_line, s.end_line, s.token_estimate, bm25(symbols_fts) FROM symbols_fts JOIN symbols AS s ON s.id = symbols_fts.rowid JOIN files AS f ON f.id = s.file_id WHERE symbols_fts MATCH ?1 AND f.repo_id = ?2 AND (?3 IS NULL OR f.language IS NULL OR f.language != ?3) ORDER BY bm25(symbols_fts) ASC, f.path ASC, s.qualified_name ASC, s.kind ASC, s.start_line ASC, s.end_line ASC, s.id ASC LIMIT ?4",
        )?;
        let raw = statement
            .query_map(params![fts_query, repo, excluded_language, limit], |row| {
                Ok((
                    raw_file_from_row(row, 0)?,
                    raw_symbol_from_row(row, 6)?,
                    row.get::<_, f64>(14)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;

        raw.into_iter()
            .map(|(file, symbol, rank)| {
                Ok(SymbolHit {
                    file: decode_file_record(file)?,
                    symbol: decode_symbol_record(symbol)?.1,
                    rank,
                })
            })
            .collect()
    }

    /// Lists a file's symbols in deterministic source order.
    pub fn symbols_in_file(
        &self,
        repo: RepoId,
        path: &str,
    ) -> Result<Vec<SymbolRecord>, StoreError> {
        validate_relative_path(path)?;
        let connection = self.connection()?;
        ensure_repo_exists(&connection, repo)?;
        let mut statement = connection.prepare(
            "SELECT s.id, s.name, s.qualified_name, s.kind, s.signature, s.start_line, s.end_line, s.token_estimate FROM symbols AS s JOIN files AS f ON f.id = s.file_id WHERE f.repo_id = ?1 AND f.path = ?2 ORDER BY s.start_line ASC, s.end_line ASC, s.qualified_name ASC, s.kind ASC, s.id ASC",
        )?;
        let raw = statement
            .query_map(params![repo, path], |row| raw_symbol_from_row(row, 0))?
            .collect::<Result<Vec<_>, _>>()?;
        raw.into_iter()
            .map(|symbol| decode_symbol_record(symbol).map(|(_, symbol)| symbol))
            .collect()
    }

    /// Lists fully resolved edges in deterministic path and kind order.
    pub fn resolved_edges(&self, repo: RepoId) -> Result<Vec<ResolvedEdgeRecord>, StoreError> {
        let connection = self.connection()?;
        ensure_repo_exists(&connection, repo)?;
        let mut statement = connection.prepare(
            "SELECT source.path, target.path, e.kind FROM edges AS e JOIN files AS source ON source.id = e.from_file_id JOIN files AS target ON target.id = e.to_file_id WHERE e.repo_id = ?1 AND e.resolution_status = 'resolved' ORDER BY source.path ASC, target.path ASC, e.kind ASC, e.id ASC",
        )?;
        let raw = statement
            .query_map([repo], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        raw.into_iter()
            .map(|(from_path, to_path, kind)| {
                Ok(ResolvedEdgeRecord {
                    from_path,
                    to_path,
                    kind: EdgeKind::from_db(&kind)?,
                })
            })
            .collect()
    }

    /// Lists files whose outgoing edges may become resolvable when repository paths expand.
    pub fn unresolved_edge_sources(&self, repo: RepoId) -> Result<Vec<String>, StoreError> {
        let connection = self.connection()?;
        ensure_repo_exists(&connection, repo)?;
        let mut statement = connection.prepare(
            "SELECT DISTINCT source.path FROM edges AS e JOIN files AS source ON source.id = e.from_file_id WHERE e.repo_id = ?1 AND (e.resolution_status != 'resolved' OR e.to_file_id IS NULL) ORDER BY source.path ASC",
        )?;
        statement
            .query_map([repo], |row| row.get(0))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    /// Returns the files whose imports may hide a dependent: an ambiguous
    /// edge, an unresolved edge that is not known to be external, or a
    /// resolved edge whose target is gone.
    pub fn incomplete_edge_sources(&self, repo: RepoId) -> Result<Vec<String>, StoreError> {
        let connection = self.connection()?;
        ensure_repo_exists(&connection, repo)?;
        let neutral = CLOSURE_NEUTRAL_UNRESOLVED
            .replace("resolution_status", "e.resolution_status")
            .replace("resolution_detail", "e.resolution_detail");
        let mut statement = connection.prepare(&format!(
            "SELECT DISTINCT source.path FROM edges AS e JOIN files AS source ON source.id = e.from_file_id WHERE e.repo_id = ?1 AND (e.resolution_status != 'resolved' OR e.to_file_id IS NULL) AND NOT {neutral} ORDER BY source.path ASC"
        ))?;
        statement
            .query_map([repo], |row| row.get(0))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    /// Returns separate unresolved and ambiguous edge counts for one repository.
    pub fn edge_resolution_counts(&self, repo: RepoId) -> Result<EdgeResolutionCounts, StoreError> {
        let connection = self.connection()?;
        ensure_repo_exists(&connection, repo)?;
        let (unresolved, ambiguous, external) = connection.query_row(
            &format!(
                "SELECT COALESCE(SUM(CASE WHEN resolution_status = 'unresolved' AND NOT {CLOSURE_NEUTRAL_UNRESOLVED} THEN 1 ELSE 0 END), 0), COALESCE(SUM(CASE WHEN resolution_status = 'ambiguous' THEN 1 ELSE 0 END), 0), COALESCE(SUM(CASE WHEN {CLOSURE_NEUTRAL_UNRESOLVED} THEN 1 ELSE 0 END), 0) FROM edges WHERE repo_id = ?1"
            ),
            [repo],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )?;
        Ok(EdgeResolutionCounts {
            external: usize::try_from(external).map_err(|_| StoreError::InvalidStoredValue {
                field: "edges external count",
                value: external.to_string(),
            })?,
            unresolved: usize::try_from(unresolved).map_err(|_| {
                StoreError::InvalidStoredValue {
                    field: "edges unresolved count",
                    value: unresolved.to_string(),
                }
            })?,
            ambiguous: usize::try_from(ambiguous).map_err(|_| StoreError::InvalidStoredValue {
                field: "edges ambiguous count",
                value: ambiguous.to_string(),
            })?,
        })
    }

    /// Returns every symbol matching an explicit identity selector, never an arbitrary first row.
    pub fn symbols_by_identity(
        &self,
        repo: RepoId,
        selector: &SymbolSelector,
    ) -> Result<Vec<(FileRecord, SymbolRecord)>, StoreError> {
        validate_relative_path(&selector.path)?;
        validate_nonempty("symbol selector qualified_name", &selector.qualified_name)?;
        if selector.start_line_hint == Some(0) {
            return Err(StoreError::InvalidField {
                field: "symbol selector start_line_hint",
                reason: "must be one-based",
            });
        }
        let signature_hash = selector
            .signature_hash
            .as_deref()
            .map(normalize_signature_hash)
            .transpose()?;
        let kind = selector.kind.map(SymbolKind::as_db);
        let connection = self.connection()?;
        ensure_repo_exists(&connection, repo)?;
        let mut statement = connection.prepare(
            "SELECT f.path, f.hash, f.language, f.size_bytes, f.token_estimate, f.indexed_at, s.id, s.name, s.qualified_name, s.kind, s.signature, s.start_line, s.end_line, s.token_estimate FROM files AS f JOIN symbols AS s ON s.file_id = f.id WHERE f.repo_id = ?1 AND f.path = ?2 AND s.qualified_name = ?3 AND (?4 IS NULL OR s.kind = ?4) ORDER BY s.start_line ASC, s.end_line ASC, s.kind ASC, COALESCE(s.signature, '') ASC, s.name ASC, s.id ASC",
        )?;
        let raw = statement
            .query_map(
                params![repo, &selector.path, &selector.qualified_name, kind],
                |row| Ok((raw_file_from_row(row, 0)?, raw_symbol_from_row(row, 6)?)),
            )?
            .collect::<Result<Vec<_>, _>>()?;

        let mut candidates = raw
            .into_iter()
            .map(|(file, symbol)| {
                let file = decode_file_record(file)?;
                let (id, symbol) = decode_symbol_record(symbol)?;
                Ok((id, file, symbol))
            })
            .collect::<Result<Vec<_>, StoreError>>()?;
        if let Some(expected) = signature_hash {
            candidates.retain(|(_, _, symbol)| {
                symbol.signature.as_deref().is_some_and(|signature| {
                    blake3::hash(signature.as_bytes()).to_hex().as_str() == expected
                })
            });
        }
        if let Some(hint) = selector.start_line_hint {
            candidates.sort_by(|left, right| {
                left.2
                    .start_line
                    .abs_diff(hint)
                    .cmp(&right.2.start_line.abs_diff(hint))
                    .then_with(|| left.2.start_line.cmp(&right.2.start_line))
                    .then_with(|| left.2.end_line.cmp(&right.2.end_line))
                    .then_with(|| left.2.kind.cmp(&right.2.kind))
                    .then_with(|| left.2.signature.cmp(&right.2.signature))
                    .then_with(|| left.0.cmp(&right.0))
            });
        }
        Ok(candidates
            .into_iter()
            .map(|(_, file, symbol)| (file, symbol))
            .collect())
    }
}

fn replace_symbols_in_transaction(
    transaction: &rusqlite::Transaction<'_>,
    file: FileId,
    repo: RepoId,
    symbols: &[SymbolRecord],
    edges: &[EdgeRecord],
    allow_pending_resolved: bool,
) -> Result<Vec<i64>, StoreError> {
    for edge in edges {
        validate_nonempty("edge to_path", &edge.to_path)?;
    }
    let prepared_edges = edges
        .iter()
        .map(|edge| prepare_edge(transaction, repo, edge, allow_pending_resolved))
        .collect::<Result<Vec<_>, _>>()?;
    transaction.execute("DELETE FROM edges WHERE from_file_id = ?1", [file])?;
    transaction.execute("DELETE FROM symbols WHERE file_id = ?1", [file])?;

    let mut symbol_ids = Vec::with_capacity(symbols.len());
    {
        let mut statement = transaction.prepare(
            "INSERT INTO symbols (file_id, name, qualified_name, kind, signature, start_line, end_line, token_estimate) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )?;
        for symbol in symbols {
            validate_symbol(symbol)?;
            symbol_ids.push(statement.insert(params![
                file,
                &symbol.name,
                &symbol.qualified_name,
                symbol.kind.as_db(),
                &symbol.signature,
                i64::from(symbol.start_line),
                i64::from(symbol.end_line),
                sqlite_integer_from_usize("symbol token_estimate", symbol.token_estimate)?,
            ])?);
        }
    }

    {
        let mut statement = transaction.prepare(
            "INSERT INTO edges (repo_id, from_file_id, to_path, to_file_id, resolution_status, resolution_detail, kind) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )?;
        for edge in prepared_edges {
            statement.execute(params![
                repo,
                file,
                edge.to_path,
                edge.to_file_id,
                edge.resolution_status,
                edge.resolution_detail,
                edge.kind,
            ])?;
        }
    }
    Ok(symbol_ids)
}

fn validate_symbol(symbol: &SymbolRecord) -> Result<(), StoreError> {
    validate_nonempty("symbol name", &symbol.name)?;
    validate_nonempty("symbol qualified_name", &symbol.qualified_name)?;
    if symbol.start_line == 0 || symbol.end_line < symbol.start_line {
        return Err(StoreError::InvalidSymbolLineRange {
            start_line: symbol.start_line,
            end_line: symbol.end_line,
        });
    }
    if symbol
        .signature
        .as_deref()
        .is_some_and(|value| value.contains('\0'))
    {
        return Err(StoreError::InvalidField {
            field: "symbol signature",
            reason: "must not contain a NUL byte",
        });
    }
    sqlite_integer_from_usize("symbol token_estimate", symbol.token_estimate)?;
    Ok(())
}

fn prepare_edge(
    connection: &rusqlite::Connection,
    repo: RepoId,
    edge: &EdgeRecord,
    allow_pending_resolved: bool,
) -> Result<PreparedEdge, StoreError> {
    let (to_file_id, resolution_status, resolution_detail) = match &edge.resolution {
        EdgeResolution::Resolved => {
            validate_relative_path(&edge.to_path)?;
            let target = connection
                .query_row(
                    "SELECT id FROM files WHERE repo_id = ?1 AND path = ?2",
                    params![repo, &edge.to_path],
                    |row| row.get(0),
                )
                .optional()?;
            match target {
                Some(target) => (Some(target), "resolved", None),
                None if allow_pending_resolved => {
                    (None, "resolved", Some("pending_target".to_owned()))
                }
                None => {
                    return Err(StoreError::ResolvedEdgeTargetNotFound {
                        repo,
                        path: edge.to_path.clone(),
                    });
                }
            }
        }
        EdgeResolution::Ambiguous(candidates) => {
            let mut candidates = candidates.clone();
            for candidate in &candidates {
                validate_relative_path(candidate)?;
            }
            candidates.sort();
            candidates.dedup();
            if candidates.len() < 2 {
                return Err(StoreError::InvalidEdgeResolution {
                    path: edge.to_path.clone(),
                    reason: "ambiguous resolution requires at least two distinct candidate paths",
                });
            }
            (None, "ambiguous", Some(json_string_array(&candidates)))
        }
        EdgeResolution::Unresolved(reason) => {
            validate_nonempty("unresolved edge reason", reason)?;
            (None, "unresolved", Some(reason.clone()))
        }
    };
    Ok(PreparedEdge {
        to_path: edge.to_path.clone(),
        to_file_id,
        resolution_status,
        resolution_detail,
        kind: edge.kind.as_db(),
    })
}

fn validate_search_query(query: &str) -> Result<(), StoreError> {
    if query.is_empty() {
        return Err(StoreError::InvalidSearchQuery {
            reason: "query must not be empty",
        });
    }
    if query.contains('\0') {
        return Err(StoreError::InvalidSearchQuery {
            reason: "query must not contain a NUL byte",
        });
    }
    Ok(())
}

fn fts_literal(query: &str) -> String {
    format!("\"{}\"", query.replace('"', "\"\""))
}

/// Shortest term, in characters, that the trigram tokenizer can match.
const FTS_MIN_TERM_CHARS: usize = 3;

/// A multi-word query split into searchable terms.
///
/// Terms are split on whitespace and stripped of surrounding punctuation;
/// punctuation inside a term (`Session::start`, `safety_gate`) is kept. Every
/// term is quoted, so no query text is ever parsed as FTS5 syntax.
struct MultiWordQuery {
    /// The terms joined by single spaces, for a normalized phrase match.
    phrase: String,
    /// An FTS5 expression requiring every distinct term. Terms too short for
    /// the trigram index are dropped rather than forcing zero matches.
    all_terms: String,
}

impl MultiWordQuery {
    /// Returns `None` for a one-word query, which stays a literal search, and
    /// when no term is long enough to search.
    fn parse(query: &str) -> Option<Self> {
        query.split_whitespace().nth(1)?;
        let words = query
            .split_whitespace()
            .map(|word| {
                word.trim_matches(|character: char| {
                    !character.is_alphanumeric() && character != '_'
                })
            })
            .filter(|word| !word.is_empty())
            .collect::<Vec<_>>();
        let mut terms = Vec::<&str>::new();
        for word in &words {
            if word.chars().count() >= FTS_MIN_TERM_CHARS
                && !terms.iter().any(|seen| seen.eq_ignore_ascii_case(word))
            {
                terms.push(word);
            }
        }
        if terms.is_empty() {
            return None;
        }
        Some(Self {
            phrase: words.join(" "),
            all_terms: terms
                .into_iter()
                .map(fts_literal)
                .collect::<Vec<_>>()
                .join(" AND "),
        })
    }
}

fn normalize_signature_hash(hash: &str) -> Result<String, StoreError> {
    if hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(StoreError::InvalidSignatureHash(hash.to_owned()));
    }
    Ok(hash.to_ascii_lowercase())
}

pub(crate) fn raw_symbol_from_row(
    row: &Row<'_>,
    offset: usize,
) -> rusqlite::Result<RawSymbolRecord> {
    Ok(RawSymbolRecord {
        id: row.get(offset)?,
        name: row.get(offset + 1)?,
        qualified_name: row.get(offset + 2)?,
        kind: row.get(offset + 3)?,
        signature: row.get(offset + 4)?,
        start_line: row.get(offset + 5)?,
        end_line: row.get(offset + 6)?,
        token_estimate: row.get(offset + 7)?,
    })
}

pub(crate) fn decode_symbol_record(
    raw: RawSymbolRecord,
) -> Result<(i64, SymbolRecord), StoreError> {
    let start_line = u32::try_from(raw.start_line).map_err(|_| StoreError::InvalidStoredValue {
        field: "symbols.start_line",
        value: raw.start_line.to_string(),
    })?;
    let end_line = u32::try_from(raw.end_line).map_err(|_| StoreError::InvalidStoredValue {
        field: "symbols.end_line",
        value: raw.end_line.to_string(),
    })?;
    let token_estimate =
        usize::try_from(raw.token_estimate).map_err(|_| StoreError::InvalidStoredValue {
            field: "symbols.token_estimate",
            value: raw.token_estimate.to_string(),
        })?;
    Ok((
        raw.id,
        SymbolRecord {
            name: raw.name,
            qualified_name: raw.qualified_name,
            kind: SymbolKind::from_db(&raw.kind)?,
            signature: raw.signature,
            start_line,
            end_line,
            token_estimate,
        },
    ))
}

fn json_string_array(values: &[String]) -> String {
    let mut output = String::from("[");
    for (index, value) in values.iter().enumerate() {
        if index > 0 {
            output.push(',');
        }
        write_json_string(&mut output, value);
    }
    output.push(']');
    output
}

fn write_json_string(output: &mut String, value: &str) {
    output.push('"');
    for character in value.chars() {
        match character {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\u{08}' => output.push_str("\\b"),
            '\u{0c}' => output.push_str("\\f"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            character if character <= '\u{1f}' => {
                write!(output, "\\u{:04x}", u32::from(character))
                    .expect("writing to a String cannot fail");
            }
            character => output.push(character),
        }
    }
    output.push('"');
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use crate::{
        EdgeKind, EdgeRecord, EdgeResolution, FileLanguage, FileRecord, Store, StoreError,
        SymbolKind, SymbolRecord, SymbolSelector,
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

    fn file(path: &str) -> FileRecord {
        FileRecord {
            path: path.to_owned(),
            hash: format!("hash-{path}"),
            language: Some(FileLanguage::Rust),
            size_bytes: 100,
            token_estimate: 20,
            indexed_at: 10,
        }
    }

    fn symbol(name: &str, qualified_name: &str, start_line: u32) -> SymbolRecord {
        SymbolRecord {
            name: name.to_owned(),
            qualified_name: qualified_name.to_owned(),
            kind: SymbolKind::Method,
            signature: Some(format!("fn {name}(&self)")),
            start_line,
            end_line: start_line + 2,
            token_estimate: 5,
        }
    }

    #[test]
    fn test_replace_symbols_transactional() {
        let (_directory, store, repo) = setup();
        let file_id = store
            .upsert_file(repo, &file("src/lib.rs"))
            .expect("file should insert");
        let originals = [
            symbol("one", "Type::one", 1),
            symbol("two", "Type::two", 5),
            symbol("three", "Type::three", 9),
        ];
        store
            .replace_symbols(file_id, &originals, &[])
            .expect("original symbols should insert");
        let invalid = SymbolRecord {
            start_line: 8,
            end_line: 7,
            ..symbol("invalid", "Type::invalid", 8)
        };

        let error = store
            .replace_symbols(
                file_id,
                &[symbol("replacement", "Type::replacement", 1), invalid],
                &[],
            )
            .expect_err("invalid replacement should fail");

        assert!(matches!(error, StoreError::InvalidSymbolLineRange { .. }));
        assert_eq!(
            store
                .symbols_in_file(repo, "src/lib.rs")
                .expect("symbols should load"),
            originals
        );
    }

    #[test]
    fn test_replace_indexed_file_rolls_back_file_and_symbols() {
        let (_directory, store, repo) = setup();
        let original_file = file("src/lib.rs");
        let originals = [symbol("original", "Type::original", 1)];
        store
            .replace_indexed_file(repo, &original_file, &originals, &[])
            .expect("original index should insert");
        let invalid = SymbolRecord {
            start_line: 8,
            end_line: 7,
            ..symbol("invalid", "Type::invalid", 8)
        };
        let mut replacement_file = original_file.clone();
        replacement_file.hash = "replacement-hash".to_owned();

        let error = store
            .replace_indexed_file(
                repo,
                &replacement_file,
                &[symbol("replacement", "Type::replacement", 1), invalid],
                &[],
            )
            .expect_err("invalid derived rows should roll back the file upsert");

        assert!(matches!(error, StoreError::InvalidSymbolLineRange { .. }));
        assert_eq!(
            store
                .file_hashes(repo)
                .expect("file hashes should load")
                .get("src/lib.rs"),
            Some(&original_file.hash)
        );
        assert_eq!(
            store
                .symbols_in_file(repo, "src/lib.rs")
                .expect("symbols should load"),
            originals
        );
    }

    #[test]
    fn test_replace_indexed_file_links_pending_target_order_independently() {
        let (_directory, store, repo) = setup();
        store
            .replace_indexed_file(
                repo,
                &file("src/a.rs"),
                &[],
                &[EdgeRecord {
                    to_path: "src/b.rs".to_owned(),
                    kind: EdgeKind::Imports,
                    resolution: EdgeResolution::Resolved,
                }],
            )
            .expect("pending source should insert");
        assert!(
            store
                .resolved_edges(repo)
                .expect("resolved edges should load")
                .is_empty(),
            "pending edges must remain invisible until their target exists"
        );

        store
            .replace_indexed_file(
                repo,
                &file("src/b.rs"),
                &[],
                &[EdgeRecord {
                    to_path: "src/a.rs".to_owned(),
                    kind: EdgeKind::Imports,
                    resolution: EdgeResolution::Resolved,
                }],
            )
            .expect("cycle target should insert and relink");

        let edges = store
            .resolved_edges(repo)
            .expect("resolved cycle should load");
        assert_eq!(
            edges
                .iter()
                .map(|edge| (edge.from_path.as_str(), edge.to_path.as_str()))
                .collect::<Vec<_>>(),
            [("src/a.rs", "src/b.rs"), ("src/b.rs", "src/a.rs")]
        );
    }

    #[test]
    fn test_pending_edge_relink_rolls_back_with_invalid_target_file() {
        let (_directory, store, repo) = setup();
        store
            .replace_indexed_file(
                repo,
                &file("src/source.rs"),
                &[],
                &[EdgeRecord {
                    to_path: "src/target.rs".to_owned(),
                    kind: EdgeKind::Imports,
                    resolution: EdgeResolution::Resolved,
                }],
            )
            .expect("pending source should insert");
        let invalid = SymbolRecord {
            start_line: 8,
            end_line: 7,
            ..symbol("invalid", "invalid", 8)
        };

        store
            .replace_indexed_file(repo, &file("src/target.rs"), &[invalid], &[])
            .expect_err("invalid target should roll back its insert and incoming relink");

        assert!(
            store
                .indexed_file(repo, "src/target.rs")
                .expect("target lookup should succeed")
                .is_none()
        );
        assert!(
            store
                .resolved_edges(repo)
                .expect("resolved edges should load")
                .is_empty()
        );
        store
            .replace_indexed_file(repo, &file("src/target.rs"), &[], &[])
            .expect("valid target should insert");
        assert_eq!(
            store
                .resolved_edges(repo)
                .expect("resolved edge should load")
                .len(),
            1
        );
    }

    #[test]
    fn test_search_symbols_trigram() {
        let (_directory, store, repo) = setup();
        for path in ["src/b.rs", "src/a.rs"] {
            let file_id = store
                .upsert_file(repo, &file(path))
                .expect("file should insert");
            store
                .replace_symbols(
                    file_id,
                    &[symbol("start", "SessionManager::start", 10)],
                    &[],
                )
                .expect("symbol should insert");
        }

        let hits = store
            .search_symbols(repo, "essionMan", 10)
            .expect("trigram search should succeed");

        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].symbol.qualified_name, "SessionManager::start");
        assert_eq!(hits[0].file.path, "src/a.rs");
        assert_eq!(hits[1].file.path, "src/b.rs");
        assert!(hits[0].rank.is_finite());
    }

    #[test]
    fn test_search_symbols_excluding_language_filters_before_limit() {
        let (_directory, store, repo) = setup();
        let mut documentation = file("docs/auth.md");
        documentation.language = Some(FileLanguage::Text);
        let mut shell = file("scripts/auth.sh");
        shell.language = Some(FileLanguage::Shell);
        for record in [documentation, shell] {
            let file_id = store
                .upsert_file(repo, &record)
                .expect("file should insert");
            store
                .replace_symbols(file_id, &[symbol("access_token", "access_token", 1)], &[])
                .expect("symbol should insert");
        }

        let hits = store
            .search_symbols_excluding_language(repo, "access_token", 1, FileLanguage::Text)
            .expect("filtered search should succeed");

        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].file.path, "scripts/auth.sh");
        assert_eq!(hits[0].file.language, Some(FileLanguage::Shell));
    }

    #[test]
    fn test_search_symbols_all_terms_matches_any_order_with_phrase_first() {
        let (_directory, store, repo) = setup();
        let file_id = store
            .upsert_file(repo, &file("src/lib.rs"))
            .expect("file should insert");
        let mut heading = symbol("Safety gate overview", "Safety gate overview", 1);
        heading.signature = None;
        store
            .replace_symbols(
                file_id,
                &[
                    heading,
                    symbol("safety_gate", "safety_gate", 10),
                    symbol("gate_for_safety", "gate_for_safety", 20),
                    symbol("safety_only", "safety_only", 30),
                ],
                &[],
            )
            .expect("symbols should insert");
        let names = |query: &str, limit: usize| {
            store
                .search_symbols_all_terms(repo, query, limit)
                .expect("all-terms search should succeed")
                .into_iter()
                .map(|hit| hit.symbol.name)
                .collect::<Vec<_>>()
        };

        assert!(
            store
                .search_symbols(repo, "safety gate", 10)
                .expect("literal search should succeed")
                .iter()
                .all(|hit| hit.symbol.name == "Safety gate overview"),
            "the literal search only sees the contiguous phrase"
        );
        let hits = names("safety gate", 10);
        assert_eq!(hits.len(), 3);
        assert_eq!(hits[0], "Safety gate overview");
        assert!(!hits.contains(&"safety_only".to_owned()));
        assert_eq!(names("SAFETY  Gate", 10), hits);
        assert_eq!(names("(safety)   gate;", 10), hits);
        assert_eq!(names("safety gate", 2), hits[..2]);
        assert_eq!(names("safety gate", 1), ["Safety gate overview"]);
        let mut without_short_term = names("safety of gate", 10);
        without_short_term.sort_unstable();
        let mut expected = hits.clone();
        expected.sort_unstable();
        assert_eq!(without_short_term, expected, "short terms are dropped");
        assert!(names("safety absent", 10).is_empty());
        assert_eq!(names("safety", 10).len(), 4);
    }

    #[test]
    fn test_multi_word_query_quotes_every_term() {
        assert!(super::MultiWordQuery::parse("safety_gate").is_none());
        assert!(super::MultiWordQuery::parse("a b").is_none());
        let quoted = super::MultiWordQuery::parse("say(\"hello\") NOT world")
            .expect("three words should parse");
        assert_eq!(
            quoted.all_terms,
            "\"say(\"\"hello\" AND \"NOT\" AND \"world\""
        );
        assert_eq!(quoted.phrase, "say(\"hello NOT world");
        let repeated = super::MultiWordQuery::parse("Session::start,  of session::START")
            .expect("repeated words should parse");
        assert_eq!(repeated.all_terms, "\"Session::start\"");
        assert_eq!(repeated.phrase, "Session::start of session::START");
    }

    #[test]
    fn test_search_symbols_escapes_fts_literal() {
        let (_directory, store, repo) = setup();
        let file_id = store
            .upsert_file(repo, &file("src/lib.rs"))
            .expect("file should insert");
        let mut quoted = symbol("say", "say", 1);
        quoted.signature = Some("fn say(\"hello\")".to_owned());
        store
            .replace_symbols(file_id, &[quoted], &[])
            .expect("symbol should insert");

        let hits = store
            .search_symbols(repo, "say(\"hello\")", 10)
            .expect("quoted literal search should not be parsed as FTS syntax");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].symbol.name, "say");
    }

    #[test]
    fn test_symbols_by_identity_qualified_and_ambiguous() {
        let (_directory, store, repo) = setup();
        let file_id = store
            .upsert_file(repo, &file("src/lib.rs"))
            .expect("file should insert");
        let mut first = symbol("run", "Worker::run", 10);
        first.signature = Some("fn run(&self)".to_owned());
        let mut second = symbol("run", "Worker::run", 30);
        second.signature = Some("fn run(&self, input: &str)".to_owned());
        store
            .replace_symbols(file_id, &[first.clone(), second.clone()], &[])
            .expect("ambiguous symbols should insert");

        let selector = SymbolSelector {
            path: "src/lib.rs".to_owned(),
            qualified_name: "Worker::run".to_owned(),
            kind: Some(SymbolKind::Method),
            signature_hash: None,
            start_line_hint: Some(28),
        };
        let candidates = store
            .symbols_by_identity(repo, &selector)
            .expect("candidate lookup should succeed");

        assert_eq!(candidates.len(), 2);
        assert!(candidates.iter().all(|(file, _)| file.path == "src/lib.rs"));
        assert_eq!(candidates[0].1, second);
        assert_eq!(candidates[1].1, first);

        let signature_hash = blake3::hash(b"fn run(&self)").to_hex().to_string();
        let exact = store
            .symbols_by_identity(
                repo,
                &SymbolSelector {
                    signature_hash: Some(signature_hash),
                    start_line_hint: None,
                    ..selector
                },
            )
            .expect("signature selector should succeed");
        assert_eq!(exact.len(), 1);
        assert_eq!(exact[0].1, first);
    }

    #[test]
    fn test_symbols_by_identity_rejects_invalid_signature_hash() {
        let (_directory, store, repo) = setup();
        let selector = SymbolSelector {
            path: "src/lib.rs".to_owned(),
            qualified_name: "Worker::run".to_owned(),
            kind: None,
            signature_hash: Some("not-a-blake3-hash".to_owned()),
            start_line_hint: None,
        };

        assert!(matches!(
            store.symbols_by_identity(repo, &selector),
            Err(StoreError::InvalidSignatureHash(hash)) if hash == "not-a-blake3-hash"
        ));
    }

    #[test]
    fn test_replace_edges_encodes_explicit_resolution() {
        let (_directory, store, repo) = setup();
        let source = store
            .upsert_file(repo, &file("src/source.rs"))
            .expect("source should insert");
        let target = store
            .upsert_file(repo, &file("src/target.rs"))
            .expect("target should insert");
        store
            .replace_symbols(
                source,
                &[],
                &[
                    EdgeRecord {
                        to_path: "src/target.rs".to_owned(),
                        kind: EdgeKind::Imports,
                        resolution: EdgeResolution::Resolved,
                    },
                    EdgeRecord {
                        to_path: "crate::choice".to_owned(),
                        kind: EdgeKind::References,
                        resolution: EdgeResolution::Ambiguous(vec![
                            "src/a.rs".to_owned(),
                            "src/b.rs".to_owned(),
                        ]),
                    },
                    EdgeRecord {
                        to_path: "missing".to_owned(),
                        kind: EdgeKind::Tests,
                        resolution: EdgeResolution::Unresolved("not found".to_owned()),
                    },
                ],
            )
            .expect("edges should insert");

        let connection = store.connection().expect("connection lock should succeed");
        let rows: Vec<(String, Option<i64>, String, Option<String>)> = connection
            .prepare(
                "SELECT to_path, to_file_id, resolution_status, resolution_detail FROM edges ORDER BY id",
            )
            .expect("edge query should prepare")
            .query_map([], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })
            .expect("edges should be queryable")
            .collect::<Result<_, _>>()
            .expect("edges should decode");
        drop(connection);
        assert_eq!(
            rows[0],
            (
                "src/target.rs".to_owned(),
                Some(target),
                "resolved".to_owned(),
                None
            )
        );
        assert_eq!(
            rows[1],
            (
                "crate::choice".to_owned(),
                None,
                "ambiguous".to_owned(),
                Some("[\"src/a.rs\",\"src/b.rs\"]".to_owned())
            )
        );
        assert_eq!(
            rows[2],
            (
                "missing".to_owned(),
                None,
                "unresolved".to_owned(),
                Some("not found".to_owned())
            )
        );

        assert_eq!(
            store
                .edge_resolution_counts(repo)
                .expect("resolution counts should load"),
            crate::EdgeResolutionCounts {
                unresolved: 1,
                ambiguous: 1,
                external: 0,
            }
        );
    }

    #[test]
    fn test_resolved_edges_are_typed_ordered_and_exclude_unresolved_rows() {
        let (_directory, store, repo) = setup();
        let first = store
            .upsert_file(repo, &file("src/a.rs"))
            .expect("first source should insert");
        let second = store
            .upsert_file(repo, &file("src/b.rs"))
            .expect("second source should insert");
        store
            .upsert_file(repo, &file("src/core.rs"))
            .expect("target should insert");
        store
            .replace_symbols(
                second,
                &[],
                &[
                    EdgeRecord {
                        to_path: "src/core.rs".to_owned(),
                        kind: EdgeKind::References,
                        resolution: EdgeResolution::Resolved,
                    },
                    EdgeRecord {
                        to_path: "missing".to_owned(),
                        kind: EdgeKind::Imports,
                        resolution: EdgeResolution::Unresolved("not indexed".to_owned()),
                    },
                ],
            )
            .expect("second edges should insert");
        store
            .replace_symbols(
                first,
                &[],
                &[EdgeRecord {
                    to_path: "src/core.rs".to_owned(),
                    kind: EdgeKind::Imports,
                    resolution: EdgeResolution::Resolved,
                }],
            )
            .expect("first edge should insert");

        let edges = store
            .resolved_edges(repo)
            .expect("resolved edges should load");

        assert_eq!(
            edges,
            [
                super::ResolvedEdgeRecord {
                    from_path: "src/a.rs".to_owned(),
                    to_path: "src/core.rs".to_owned(),
                    kind: EdgeKind::Imports,
                },
                super::ResolvedEdgeRecord {
                    from_path: "src/b.rs".to_owned(),
                    to_path: "src/core.rs".to_owned(),
                    kind: EdgeKind::References,
                },
            ]
        );
        assert_eq!(
            store
                .unresolved_edge_sources(repo)
                .expect("unresolved sources should load"),
            ["src/b.rs"]
        );
    }

    #[test]
    fn test_external_and_same_file_imports_do_not_count_against_completeness() {
        use crate::{EDGE_DETAIL_EXTERNAL, EDGE_DETAIL_SAME_FILE};

        let (_directory, store, repo) = setup();
        let unresolved = |detail: &str| EdgeRecord {
            to_path: format!("use::{detail}"),
            kind: EdgeKind::Imports,
            resolution: EdgeResolution::Unresolved(detail.to_owned()),
        };
        let only_external = store
            .upsert_file(repo, &file("src/only_external.rs"))
            .expect("file should insert");
        store
            .replace_symbols(
                only_external,
                &[],
                &[
                    unresolved(EDGE_DETAIL_EXTERNAL),
                    unresolved(EDGE_DETAIL_SAME_FILE),
                ],
            )
            .expect("external edges should insert");
        let mixed = store
            .upsert_file(repo, &file("src/mixed.rs"))
            .expect("file should insert");
        store
            .replace_symbols(
                mixed,
                &[],
                &[unresolved(EDGE_DETAIL_EXTERNAL), unresolved("not_indexed")],
            )
            .expect("mixed edges should insert");

        assert_eq!(
            store
                .edge_resolution_counts(repo)
                .expect("resolution counts should load"),
            crate::EdgeResolutionCounts {
                unresolved: 1,
                ambiguous: 0,
                external: 3,
            }
        );
        assert_eq!(
            store
                .incomplete_edge_sources(repo)
                .expect("incomplete sources should load"),
            ["src/mixed.rs"]
        );
        // Reindexing still revisits every file with an unresolved edge.
        assert_eq!(
            store
                .unresolved_edge_sources(repo)
                .expect("unresolved sources should load"),
            ["src/mixed.rs", "src/only_external.rs"]
        );
    }

    #[test]
    fn test_resolved_edge_target_must_be_in_same_repository() {
        let (directory, store, source_repo) = setup();
        let source = store
            .upsert_file(source_repo, &file("src/source.rs"))
            .expect("source should insert");
        let other_root = directory.path().join("other-repo");
        fs::create_dir(&other_root).expect("other repository should be created");
        let other_repo = store
            .approve_repo(&other_root)
            .expect("other repository should approve");
        store
            .upsert_file(other_repo, &file("src/target.rs"))
            .expect("other target should insert");

        assert!(matches!(
            store.replace_symbols(
                source,
                &[],
                &[EdgeRecord {
                    to_path: "src/target.rs".to_owned(),
                    kind: EdgeKind::Imports,
                    resolution: EdgeResolution::Resolved,
                }],
            ),
            Err(StoreError::ResolvedEdgeTargetNotFound { repo, path })
                if repo == source_repo && path == "src/target.rs"
        ));
    }
}
