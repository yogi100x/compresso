//! Symbol search with informational diagnostics and precise query validation.

use compresso_store::{RepoId, Store};

use crate::AtlasError;
use crate::queries::{
    QueryError, SearchHit, map_path_matches_focus, search_hits, validate_map_focus,
};
use crate::scanner::is_secret_path;

/// Shortest query, in characters, that the trigram symbol index can match.
pub const SEARCH_MIN_QUERY_CHARS: usize = 3;
/// Ranked symbols examined when a path scope filters hits after ranking.
const SCOPED_FETCH_LIMIT: usize = 1_000;

/// Bounds and optional scope for one symbol search.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SearchOptions<'a> {
    /// Maximum hits returned.
    pub limit: usize,
    /// Repository-relative file or directory that hits must fall under.
    pub path: Option<&'a str>,
}

/// Why a successful search may have returned fewer hits than expected.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SearchDiagnosticCode {
    /// The path scope names no indexed file or directory.
    PathNotFound,
    /// The query is shorter than the trigram index can match.
    QueryTooShort,
    /// The path scope was applied to a capped ranked window.
    PathScopeWindowCapped,
}

impl SearchDiagnosticCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PathNotFound => "path_not_found",
            Self::QueryTooShort => "query_too_short",
            Self::PathScopeWindowCapped => "path_scope_window_capped",
        }
    }
}

/// An informational note attached to a successful search. Never an error.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchDiagnostic {
    pub code: SearchDiagnosticCode,
    pub message: String,
}

/// Ranked hits plus the diagnostics that explain an empty or partial result.
#[derive(Clone, Debug, PartialEq)]
pub struct SearchReport {
    pub hits: Vec<SearchHit>,
    pub diagnostics: Vec<SearchDiagnostic>,
}

/// Rejects queries the symbol index cannot accept, naming the byte offset and cause.
pub fn validate_search_query(query: &str) -> Result<(), QueryError> {
    if query.is_empty() {
        return Err(QueryError::InvalidSearchQuery {
            position: 0,
            reason: "the query is empty",
        });
    }
    if let Some(position) = query.find('\0') {
        return Err(QueryError::InvalidSearchQuery {
            position,
            reason: "a NUL byte cannot be searched",
        });
    }
    if query.trim().is_empty() {
        return Err(QueryError::InvalidSearchQuery {
            position: 0,
            reason: "the query contains only whitespace",
        });
    }
    Ok(())
}

/// Searches the symbol index, reporting a missing path scope or an
/// unmatchable query as diagnostics on a successful, empty result.
///
/// Only a malformed query or a path scope that is not a lexical
/// repository-relative path is an error.
pub fn search_with_diagnostics(
    store: &Store,
    repo: RepoId,
    query: &str,
    options: SearchOptions<'_>,
) -> Result<SearchReport, AtlasError> {
    validate_search_query(query)?;
    let scope = match options.path {
        Some(path) => validate_map_focus(path).map_err(|_| QueryError::InvalidSearchPath {
            path: path.to_owned(),
        })?,
        None => None,
    };
    let mut diagnostics = Vec::new();

    if let Some(scope) = scope {
        let scope_indexed = store
            .file_hashes(repo)?
            .keys()
            .any(|path| map_path_matches_focus(path, scope) && !is_secret_path(path.as_ref()));
        if !scope_indexed {
            diagnostics.push(SearchDiagnostic {
                code: SearchDiagnosticCode::PathNotFound,
                message: format!(
                    "No indexed file or directory matches path scope '{scope}'. Check the path, or run `compresso index` if it was added recently."
                ),
            });
            return Ok(SearchReport {
                hits: Vec::new(),
                diagnostics,
            });
        }
    }

    let fetch_limit = if scope.is_some() {
        options.limit.max(SCOPED_FETCH_LIMIT)
    } else {
        options.limit
    };
    let mut hits = search_hits(store.search_symbols(repo, query, fetch_limit)?)?;
    let window_full = hits.len() >= fetch_limit;
    if let Some(scope) = scope {
        hits.retain(|hit| map_path_matches_focus(&hit.path, scope));
        if window_full && hits.len() < options.limit {
            diagnostics.push(SearchDiagnostic {
                code: SearchDiagnosticCode::PathScopeWindowCapped,
                message: format!(
                    "Path scope '{scope}' was applied to the top {fetch_limit} ranked symbols; lower-ranked matches may exist. Use a more specific query."
                ),
            });
        }
    }
    hits.truncate(options.limit);

    if hits.is_empty() && query.chars().count() < SEARCH_MIN_QUERY_CHARS {
        diagnostics.push(SearchDiagnostic {
            code: SearchDiagnosticCode::QueryTooShort,
            message: format!(
                "Queries shorter than {SEARCH_MIN_QUERY_CHARS} characters cannot match the trigram symbol index. Use a longer query."
            ),
        });
    }
    Ok(SearchReport { hits, diagnostics })
}
