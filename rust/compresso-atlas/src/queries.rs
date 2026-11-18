use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use compresso_core::{TokenCounter, TokenProvider, counter_for};
use compresso_store::{
    EdgeKind, FileLanguage, RepoId, Store, SymbolKind, SymbolRecord, SymbolSelector,
};

use crate::AtlasError;
use crate::parsers::{ParseError, ParserRegistry};
use crate::scanner::is_secret_path;
use crate::task_facets::{task_clauses, task_focus};
use crate::task_identifiers::{
    NamedRank, TaskIdentifier, line_has_number, named_symbols, task_identifiers, task_numbers,
};

/// Largest repository file a bounded read, unfold, or gate will load (SEC-005).
///
/// Token budgets trim what is returned, but the whole file is decoded and
/// counted first, so the read itself needs a byte ceiling.
pub const MAX_READ_FILE_BYTES: u64 = 16 * 1024 * 1024;
const TOP_SYMBOLS_PER_FILE: usize = 3;
const MAX_AMBIGUITY_CANDIDATES: usize = 20;
const PLAN_CONTEXT_SEARCH_LIMIT: usize = 20;
const PLAN_CONTEXT_QUERY_LIMIT: usize = 20;
const PLAN_CONTEXT_HITS_PER_QUERY: usize = 8;
const PLAN_CONTEXT_SYMBOLS_PER_FILE: usize = 3;
const PLAN_CONTEXT_BODIES_PER_FILE: usize = 2;
const PLAN_CONTEXT_EVIDENCE_CONTENT_TOKENS: usize = 48;
/// Excerpt allowance for evidence that names or contains a compound task concept.
const PLAN_CONTEXT_STRONG_EVIDENCE_TOKENS: usize = 96;
/// Excerpt allowance for the best-scored strong evidence, at most
/// [`PLAN_CONTEXT_TOP_EVIDENCE_ITEMS`] items. It is one twelfth of the pack
/// budget between the strong allowance and this ceiling, so a small pack
/// keeps its room for file diversity.
const PLAN_CONTEXT_TOP_EVIDENCE_TOKENS: usize = 224;
const PLAN_CONTEXT_TOP_EVIDENCE_ITEMS: usize = 3;
const PLAN_CONTEXT_TOP_EVIDENCE_BUDGET_DIVISOR: usize = 12;
/// Compound concepts that name a symbol whose other uses are treated as
/// evidence: the first of the task's specific named compounds, longest first.
const PLAN_CONTEXT_USE_SITE_CONCEPTS: usize = 3;
/// Evidence items one file may hold when the extra ones define or use such a
/// concept; other evidence stops at [`PLAN_CONTEXT_BODIES_PER_FILE`].
const PLAN_CONTEXT_USE_SITE_BODIES_PER_FILE: usize = 3;
/// Constant definitions one pack may add because an included excerpt
/// references them, the tokens each may take, and the index lookups spent
/// resolving them.
const PLAN_CONTEXT_REFERENCED_CONSTANTS: usize = 4;
const PLAN_CONTEXT_REFERENCED_CONSTANT_TOKENS: usize = 40;
const PLAN_CONTEXT_REFERENCED_CONSTANT_LOOKUPS: usize = 16;
/// Hits fetched per query for cross-query scoring; only the first
/// [`PLAN_CONTEXT_HITS_PER_QUERY`] count toward `matches_total`.
const PLAN_CONTEXT_SCORING_WINDOW: usize = 24;
/// Scored hits whose bodies are read to rank by what the code contains.
const PLAN_CONTEXT_RERANK_POOL: usize = 40;
/// Compounds kept per window size (three-word, then two-word).
const PLAN_CONTEXT_COMPOUND_LIMIT: usize = 12;
const PLAN_CONTEXT_UNCOVERED_LIMIT: usize = 8;
/// Uncovered clauses one plan reports, each cut to `PLAN_CONTEXT_FACET_LABEL_CHARS`.
const PLAN_CONTEXT_UNCOVERED_FACET_LIMIT: usize = 4;
const PLAN_CONTEXT_FACET_LABEL_CHARS: usize = 80;
/// A hit scoring at least this much matched a compound concept by name or body.
const PLAN_CONTEXT_STRONG_SCORE: f64 = 4.0;
/// Generic-word hits scoring below this share of the best hit are dropped as noise.
const PLAN_CONTEXT_SCORE_FLOOR_RATIO: f64 = 0.25;
/// A hit scoring at least this much is kept below the relative floor until
/// the stronger evidence covers the task; then only one that defines or uses
/// a named concept, or matched a specific task word, stays.
const PLAN_CONTEXT_SPECIFIC_MATCH_SCORE: f64 = 2.0;
const PLAN_CONTEXT_TEST_PENALTY: f64 = 0.4;
/// Task concepts with no symbol-name match that one plan searches for in file content.
const PLAN_CONTEXT_CONTENT_TERMS: usize = 8;
/// How many of those may be compounds, leaving room for single words.
const PLAN_CONTEXT_CONTENT_COMPOUND_TERMS: usize = 5;
/// Symbols the content scan may add to the candidate pool.
const PLAN_CONTEXT_CONTENT_CANDIDATES: usize = 12;
/// Largest file the content scan reads. Half the indexer's file-size limit:
/// hand-written source sits far below it, and what lies above is generated
/// or data text whose symbols are not useful evidence.
const PLAN_CONTEXT_CONTENT_MAX_FILE_BYTES: u64 = 512 * 1024;
/// Hard caps on one content scan. They are counts and bytes, never a clock,
/// so the same index and files always produce the same plan.
const PLAN_CONTEXT_CONTENT_MAX_FILES: usize = 2_000;
const PLAN_CONTEXT_CONTENT_MAX_BYTES: u64 = 32 * 1024 * 1024;
/// Pool score of a symbol whose body contains an otherwise unmatched task word.
const PLAN_CONTEXT_CONTENT_WORD_SCORE: f64 = 1.0;
/// Pool score of a symbol containing a numeric literal from the task (D88).
const PLAN_CONTEXT_CONTENT_NUMBER_SCORE: f64 = 2.0;
/// Pool score of a symbol whose body uses an identifier the task names (D88).
const PLAN_CONTEXT_CONTENT_IDENTIFIER_SCORE: f64 = 3.0;
/// Named identifiers whose use sites the content scan looks for (D88).
const PLAN_CONTEXT_USE_SITE_IDENTIFIERS: usize = 2;
/// Pool score of a symbol the task names verbatim (D88); strong on its own.
const PLAN_CONTEXT_NAMED_SCORE: f64 = 8.0;
const REINDEX_HINT: &str = "Repository index is stale; run `compresso index` and retry.";

/// A deterministic, token-budgeted structural summary of an indexed repository.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RepoMap {
    pub rendered: String,
    pub files_total: usize,
    pub files_shown: usize,
    pub truncated: bool,
    pub tokens_used: usize,
    pub token_provider: TokenProvider,
}

/// The complete stored symbol skeleton for one indexed file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Outline {
    pub path: String,
    pub language: Option<FileLanguage>,
    pub symbols: Vec<SymbolRecord>,
}

/// Stored outline paired with the current on-disk hash state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CurrentOutline {
    pub outline: Outline,
    pub stale: bool,
    pub reindex_hint: Option<String>,
}

/// Stable identity returned when a selector matches more than one symbol.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SymbolIdentity {
    pub path: String,
    pub qualified_name: String,
    pub kind: SymbolKind,
    pub signature_hash: Option<String>,
    pub start_line: u32,
    pub end_line: u32,
}

/// Selector lookup outcome. Ambiguity and absence are data, never arbitrary selection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UnfoldResult {
    Found {
        path: String,
        symbol: SymbolRecord,
        content: String,
        stale: bool,
        reindex_hint: Option<String>,
    },
    Ambiguous {
        candidates: Vec<SymbolIdentity>,
        stale: bool,
        reindex_hint: Option<String>,
    },
    NotFound {
        path: String,
        qualified_name: String,
        stale: bool,
        reindex_hint: String,
    },
}

/// One deterministic FTS-ranked symbol search result.
#[derive(Clone, Debug, PartialEq)]
pub struct SearchHit {
    pub path: String,
    pub language: Option<FileLanguage>,
    pub symbol: SymbolRecord,
    pub score: f64,
}

/// A filesystem read constrained to an inclusive line interval and hard token budget.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundedRead {
    pub path: String,
    pub content: String,
    pub start_line: u32,
    pub end_line: u32,
    pub selected_end_line: u32,
    pub file_lines_total: u32,
    pub range_clamped: bool,
    pub file_tokens_total: usize,
    /// `file_tokens_total` is an estimate: the file is larger than
    /// [`EXACT_TOKEN_COUNT_MAX_BYTES`], so it was not tokenized whole.
    pub file_tokens_estimated: bool,
    pub tokens_used: usize,
    pub token_provider: TokenProvider,
    pub truncated: bool,
    pub elision: Option<String>,
}

/// One dependent file reached while walking resolved edges in reverse.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImpactEntry {
    pub path: String,
    pub depth: u8,
    pub edge_kinds: Vec<EdgeKind>,
}

/// Caller-supplied bounds for reverse-dependency traversal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ImpactOptions {
    pub max_depth: u8,
    pub max_results: usize,
}

impl Default for ImpactOptions {
    fn default() -> Self {
        Self {
            max_depth: 2,
            max_results: 100,
        }
    }
}

/// The deterministic reverse dependency closure for one indexed file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImpactReport {
    pub path: String,
    pub dependents: Vec<ImpactEntry>,
    pub unresolved_edges: usize,
    pub ambiguous_edges: usize,
    pub complete: bool,
    pub truncated: bool,
}

/// One bounded source excerpt included because its symbol identity was returned by task FTS.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContextSymbolBody {
    pub path: String,
    pub symbol: SymbolRecord,
    pub content: String,
    /// First source line of `content`; later than the symbol start when the
    /// excerpt is centred on the lines matching the task.
    pub excerpt_start_line: u32,
    pub excerpt_end_line: u32,
    pub truncated: bool,
}

/// A deterministic search-driven context pack under one hard tokenizer budget.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContextPlan {
    pub rendered: String,
    pub repo_map: Option<RepoMap>,
    pub outlines: Vec<Outline>,
    pub symbol_bodies: Vec<ContextSymbolBody>,
    /// Constant definitions an included excerpt references by name. They are
    /// rendered after the referencing excerpt and count toward the budget,
    /// but not toward evidence files, concept coverage, or `truncated`.
    pub referenced_constants: Vec<ContextSymbolBody>,
    pub search_queries: Vec<String>,
    pub matches_total: usize,
    pub evidence_files_total: usize,
    pub evidence_files_included: usize,
    /// Distinct salient task words, and how many of them the included
    /// evidence names or contains.
    pub task_concepts_total: usize,
    pub task_concepts_covered: usize,
    /// Salient task words no included evidence mentions, in task order.
    pub uncovered_concepts: Vec<String>,
    /// Clauses of the task that name a concept, and how many of them the
    /// included evidence covers (D95).
    pub task_facets_total: usize,
    pub task_facets_covered: usize,
    /// Clauses the included evidence does not cover, in task order.
    pub uncovered_facets: Vec<String>,
    /// Modeled raw matched-evidence token baseline before excerpt packing.
    pub estimated_raw_source_tokens: usize,
    pub tokens_used: usize,
    pub token_provider: TokenProvider,
    pub truncated: bool,
    /// The content scan stopped at its file or byte cap before reading every
    /// indexed file, so body-only evidence may be missing.
    pub content_scan_truncated: bool,
}

impl ContextPlan {
    /// Whether the included evidence mentions at least half of the task's
    /// salient words and covers every clause of the task that names a
    /// concept. A pack that matched only generic words fails this even when
    /// every candidate file fitted the budget, and so does a pack that
    /// answers one part of a several-part task (D95).
    #[must_use]
    pub const fn concept_coverage_sufficient(&self) -> bool {
        (self.task_concepts_total == 0
            || self.task_concepts_covered.saturating_mul(2) >= self.task_concepts_total)
            && self.task_facets_covered >= self.task_facets_total
    }

    /// The readiness rule every surface reports (04 §`compresso_context`):
    /// some evidence matched, every candidate file fitted, and the concepts
    /// and clauses of the task are covered.
    #[must_use]
    pub const fn evidence_ready(&self) -> bool {
        self.evidence_files_total > 0
            && self.evidence_files_included >= self.evidence_files_total
            && self.concept_coverage_sufficient()
    }
}

/// Typed structural-query and bounded-filesystem failures.
#[derive(Debug, thiserror::Error)]
pub enum QueryError {
    #[error("repository-map focus must be a lexical repository-relative prefix: {focus:?}")]
    InvalidFocus { focus: String },
    #[error("invalid search query at byte {position}: {reason}")]
    InvalidSearchQuery {
        /// Zero-based byte offset of the first offending input.
        position: usize,
        reason: &'static str,
    },
    #[error("search path scope must be a lexical repository-relative path: {path:?}")]
    InvalidSearchPath { path: String },
    #[error("invalid impact options: {reason}")]
    InvalidImpactOptions { reason: &'static str },
    #[error("invalid trace options: {reason}")]
    InvalidTraceOptions { reason: &'static str },
    #[error("invalid read ranges: {reason}")]
    InvalidReadRanges { reason: &'static str },
    #[error("failed to canonicalize repository query root {path:?}")]
    CanonicalizeRoot {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("repository query root is not a directory: {path:?}")]
    RootNotDirectory { path: PathBuf },
    #[error("failed to resolve repository path {path:?}")]
    ResolvePath {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("repository file was not found: {path:?}")]
    NotFound { path: PathBuf },
    #[error("repository path is not a regular file: {path:?}")]
    NotAFile { path: PathBuf },
    #[error("failed to read repository file {path:?}")]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("repository file {path:?} exceeds the {limit_bytes}-byte read limit")]
    FileTooLarge { path: PathBuf, limit_bytes: u64 },
    #[error("invalid inclusive line range {start_line}..={end_line} for {total_lines} lines")]
    InvalidRange {
        start_line: u32,
        end_line: u32,
        total_lines: u32,
    },
    #[error("failed to parse stale repository file {path:?}")]
    Parse {
        path: PathBuf,
        #[source]
        source: ParseError,
    },
    #[error("stored symbol has invalid line range {start_line}..={end_line} in {path:?}")]
    InvalidSymbolRange {
        path: PathBuf,
        start_line: u32,
        end_line: u32,
    },
}

/// Builds a directory tree with the most important indexed files selected first.
///
/// File importance is the number of distinct indexed files with resolved incoming
/// edges. Ties and final tree presentation are repository-relative path ordered.
pub fn repo_map(store: &Store, repo: RepoId, budget_tokens: usize) -> Result<RepoMap, AtlasError> {
    repo_map_focused(store, repo, None, budget_tokens)
}

/// Builds a repository map while ranking a validated relative prefix first.
pub fn repo_map_focused(
    store: &Store,
    repo: RepoId,
    focus: Option<&str>,
    budget_tokens: usize,
) -> Result<RepoMap, AtlasError> {
    let focus = focus.map(validate_map_focus).transpose()?.flatten();
    let counter = counter_for(TokenProvider::OpenAiO200k);
    let files = store
        .indexed_files(repo)?
        .into_iter()
        .filter(|file| !is_secret_path(Path::new(&file.path)))
        .collect::<Vec<_>>();
    let mut importers = BTreeMap::<String, BTreeSet<String>>::new();
    for edge in store.resolved_edges(repo)? {
        if is_secret_path(Path::new(&edge.from_path)) || is_secret_path(Path::new(&edge.to_path)) {
            continue;
        }
        importers
            .entry(edge.to_path)
            .or_default()
            .insert(edge.from_path);
    }

    let mut ranked = files
        .iter()
        .map(|file| {
            let in_degree = importers.get(&file.path).map_or(0, BTreeSet::len);
            let symbols = store.symbols_in_file(repo, &file.path)?;
            let focused = focus.is_some_and(|focus| map_path_matches_focus(&file.path, focus));
            Ok((file.path.clone(), focused, in_degree, symbols))
        })
        .collect::<Result<Vec<_>, compresso_store::StoreError>>()?;
    ranked.sort_by(|left, right| {
        right
            .1
            .cmp(&left.1)
            .then_with(|| right.2.cmp(&left.2))
            .then_with(|| left.0.cmp(&right.0))
    });

    let mut selected = BTreeMap::<String, Vec<String>>::new();
    let mut budget_truncated_symbols = false;
    for (path, _, _, symbols) in ranked {
        let top_symbols = symbols
            .into_iter()
            .take(TOP_SYMBOLS_PER_FILE)
            .map(|symbol| symbol.qualified_name)
            .collect::<Vec<_>>();
        let mut accepted = None;
        for symbol_count in (0..=top_symbols.len()).rev() {
            let mut trial = selected.clone();
            trial.insert(path.clone(), top_symbols[..symbol_count].to_vec());
            let rendered = render_directory_tree(&trial);
            if counter.count(&rendered) <= budget_tokens {
                accepted = Some((trial, symbol_count < top_symbols.len()));
                break;
            }
        }
        let Some((trial, truncated_symbols)) = accepted else {
            break;
        };
        selected = trial;
        budget_truncated_symbols |= truncated_symbols;
    }

    let rendered = render_directory_tree(&selected);
    let tokens_used = counter.count(&rendered);
    let files_total = files.len();
    let files_shown = selected.len();
    Ok(RepoMap {
        rendered,
        files_total,
        files_shown,
        truncated: files_shown < files_total || budget_truncated_symbols,
        tokens_used,
        token_provider: counter.provider(),
    })
}

pub(crate) fn validate_map_focus(focus: &str) -> Result<Option<&str>, QueryError> {
    if focus.is_empty() {
        return Ok(None);
    }
    let bytes = focus.as_bytes();
    let windows_drive = bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':';
    let unsafe_separator = focus.starts_with('\\') || focus.contains('\\');
    let safe_components = Path::new(focus)
        .components()
        .all(|component| matches!(component, Component::Normal(_)));
    if windows_drive || unsafe_separator || !safe_components {
        return Err(QueryError::InvalidFocus {
            focus: focus.to_owned(),
        });
    }
    Ok(Some(focus.trim_end_matches('/')))
}

pub(crate) fn map_path_matches_focus(path: &str, focus: &str) -> bool {
    path == focus
        || path
            .strip_prefix(focus)
            .is_some_and(|remainder| remainder.starts_with('/'))
}

/// Returns every stored symbol for `path` in deterministic source order.
pub fn outline(store: &Store, repo: RepoId, path: &str) -> Result<Outline, AtlasError> {
    if is_secret_path(Path::new(path)) {
        return Err(AtlasError::FileNotIndexed {
            repo,
            path: path.to_owned(),
        });
    }
    let Some(file) = store.indexed_file(repo, path)? else {
        return Err(AtlasError::FileNotIndexed {
            repo,
            path: path.to_owned(),
        });
    };
    let symbols = store.symbols_in_file(repo, path)?;
    Ok(Outline {
        path: file.path,
        language: file.language,
        symbols,
    })
}

/// Returns a stored outline while safely detecting current on-disk hash drift.
pub fn outline_current(
    store: &Store,
    repo: RepoId,
    path: &str,
) -> Result<CurrentOutline, AtlasError> {
    let outline = outline(store, repo, path)?;
    let repository = store.repository(repo)?;
    let root = canonical_query_root(&repository.root)?;
    let Some(indexed) = store.indexed_file(repo, path)? else {
        return Err(AtlasError::FileNotIndexed {
            repo,
            path: path.to_owned(),
        });
    };
    let ResolvedEntry::File(resolved) = resolve_under_root(&root, path)? else {
        return Err(QueryError::NotFound {
            path: root.join(path),
        }
        .into());
    };
    let stale = !file_matches_index(&resolved, &indexed)?;
    Ok(CurrentOutline {
        outline,
        stale,
        reindex_hint: stale.then(|| REINDEX_HINT.to_owned()),
    })
}

/// Reports whether any indexed file has drifted, disappeared, or escaped its root.
pub fn repository_stale(store: &Store, repo: RepoId) -> Result<bool, AtlasError> {
    let repository = store.repository(repo)?;
    let root = canonical_query_root(&repository.root)?;
    for indexed in store.indexed_files(repo)? {
        let resolved = match resolve_under_root(&root, &indexed.path) {
            Ok(ResolvedEntry::File(resolved)) => resolved,
            Ok(ResolvedEntry::Missing)
            | Err(AtlasError::OutsideRoot { .. })
            | Err(AtlasError::Query(QueryError::NotAFile { .. })) => return Ok(true),
            Err(error) => return Err(error),
        };
        if !file_matches_index(&resolved, &indexed)? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Returns an exact current symbol body or a typed ambiguous/not-found outcome.
pub fn unfold(
    store: &Store,
    repo: RepoId,
    selector: &SymbolSelector,
) -> Result<UnfoldResult, AtlasError> {
    let repository = store.repository(repo)?;
    let root = canonical_query_root(&repository.root)?;
    let resolved = resolve_under_root(&root, &selector.path)?;
    let stored = store.symbols_by_identity(repo, selector)?;
    let indexed = store.indexed_file(repo, &selector.path)?;
    let Some(indexed) = indexed else {
        return Ok(not_found(selector, false));
    };
    let ResolvedEntry::File(path) = resolved else {
        return Ok(not_found(selector, true));
    };
    let source = read_utf8(&path)?;
    let stale = blake3::hash(source.as_bytes()).to_hex().as_str() != indexed.hash;
    let mut candidates = if stale {
        let Some(language) = indexed.language else {
            return Ok(not_found(selector, true));
        };
        let extraction =
            ParserRegistry::extract(language, &source).map_err(|source| QueryError::Parse {
                path: path.path().to_path_buf(),
                source,
            })?;
        matching_symbols(extraction.symbols, selector)
    } else {
        stored.into_iter().map(|(_, symbol)| symbol).collect()
    };
    sort_selector_candidates(&mut candidates, selector.start_line_hint);
    let reindex_hint = stale.then(|| REINDEX_HINT.to_owned());

    match candidates.len() {
        0 => Ok(not_found(selector, stale)),
        1 => {
            let symbol = candidates.pop().expect("one candidate was counted");
            let content =
                source_line_range(&source, path.path(), symbol.start_line, symbol.end_line)?;
            Ok(UnfoldResult::Found {
                path: selector.path.clone(),
                symbol,
                content: content.to_owned(),
                stale,
                reindex_hint,
            })
        }
        _ => Ok(UnfoldResult::Ambiguous {
            candidates: candidates
                .iter()
                .take(MAX_AMBIGUITY_CANDIDATES)
                .map(|symbol| symbol_identity(&selector.path, symbol))
                .collect(),
            stale,
            reindex_hint,
        }),
    }
}

/// Returns Store's ranking as typed Atlas hits without reordering it.
///
/// A multi-word query matches symbols containing every term, in any order and
/// letter case, with exact-phrase matches first; one word is a literal match.
pub fn search(
    store: &Store,
    repo: RepoId,
    query: &str,
    limit: usize,
) -> Result<Vec<SearchHit>, AtlasError> {
    search_hits(store.search_symbols_all_terms(repo, query, limit)?)
}

/// Literal contiguous-phrase search used by context planning, which already
/// issues one query per task concept.
fn search_literal(
    store: &Store,
    repo: RepoId,
    query: &str,
    limit: usize,
) -> Result<Vec<SearchHit>, AtlasError> {
    search_hits(store.search_symbols(repo, query, limit)?)
}

fn search_excluding_language(
    store: &Store,
    repo: RepoId,
    query: &str,
    limit: usize,
    excluded_language: FileLanguage,
) -> Result<Vec<SearchHit>, AtlasError> {
    search_hits(store.search_symbols_excluding_language(repo, query, limit, excluded_language)?)
}

pub(crate) fn search_hits(
    hits: Vec<compresso_store::SymbolHit>,
) -> Result<Vec<SearchHit>, AtlasError> {
    hits.into_iter()
        .filter(|hit| !is_secret_path(Path::new(&hit.file.path)))
        .map(|hit| {
            Ok(SearchHit {
                path: hit.file.path,
                language: hit.file.language,
                symbol: hit.symbol,
                score: hit.rank,
            })
        })
        .collect()
}

/// Largest text whose token total is counted exactly by a read. It equals
/// the scanner's file-size limit, so every indexed file keeps an exact
/// total; a larger file reports an estimate instead of being tokenized whole.
pub const EXACT_TOKEN_COUNT_MAX_BYTES: usize = 1_000_000;

/// Counts the lines of one canonicalized in-root file without tokenizing it.
pub fn file_line_count(root: &Path, path: &str) -> Result<u32, AtlasError> {
    let root = canonical_query_root(root)?;
    let ResolvedEntry::File(resolved) = resolve_under_root(&root, path)? else {
        return Err(QueryError::NotFound {
            path: root.join(path),
        }
        .into());
    };
    Ok(line_count(&read_utf8(&resolved)?))
}

/// The token total of `text`: exact up to [`EXACT_TOKEN_COUNT_MAX_BYTES`],
/// otherwise the byte-length estimate, flagged as such.
fn token_total(text: &str, counter: &dyn TokenCounter) -> (usize, bool) {
    if text.len() <= EXACT_TOKEN_COUNT_MAX_BYTES {
        (counter.count(text), false)
    } else {
        (text.len().div_ceil(4), true)
    }
}

/// The longest prefix of `text` within `budget` tokens, and whether anything
/// was cut. The text is examined through a window that doubles until it
/// holds more than the budget, so the work follows the size of the answer
/// instead of the size of the text.
fn fit_within_budget(text: &str, budget: usize, counter: &dyn TokenCounter) -> (String, bool) {
    if budget == usize::MAX {
        return (text.to_owned(), false);
    }
    // About four bytes per token is typical; start well above the budget so
    // ordinary reads settle in one or two steps.
    let mut window = budget.saturating_mul(16).max(4 * 1024);
    loop {
        let mut end = window.min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        let candidate = &text[..end];
        if counter.count(candidate) > budget {
            return (prefix_within_budget(candidate, budget, counter), true);
        }
        if end == text.len() {
            return (text.to_owned(), false);
        }
        window = window.saturating_mul(2);
    }
}

/// Reads one canonicalized in-root file or inclusive line range under a hard budget.
pub fn read_bounded(
    root: &Path,
    path: &str,
    range: Option<(u32, u32)>,
    budget_tokens: usize,
    counter: &dyn TokenCounter,
) -> Result<BoundedRead, AtlasError> {
    let root = canonical_query_root(root)?;
    let ResolvedEntry::File(resolved) = resolve_under_root(&root, path)? else {
        return Err(QueryError::NotFound {
            path: root.join(path),
        }
        .into());
    };
    let source = read_utf8(&resolved)?;
    let total_lines = line_count(&source);
    let (start_line, selected_end_line, range_clamped, selected) = match range {
        Some((start_line, end_line)) => {
            let (selected_end_line, range_clamped) =
                normalize_range(start_line, end_line, total_lines)?;
            (
                start_line,
                selected_end_line,
                range_clamped,
                source_line_range(&source, resolved.path(), start_line, selected_end_line)?,
            )
        }
        None if total_lines == 0 => (0, 0, false, ""),
        None => (1, total_lines, false, source.as_str()),
    };
    let (content, truncated) = fit_within_budget(selected, budget_tokens, counter);
    let whole_file_returned = !truncated && content.len() == source.len();
    let (file_tokens_total, file_tokens_estimated) = token_total(&source, counter);
    let tokens_used = if whole_file_returned {
        file_tokens_total
    } else if content.len() <= EXACT_TOKEN_COUNT_MAX_BYTES {
        counter.count(&content)
    } else {
        // Only an unbudgeted internal read returns this much; its callers
        // use the totals, not an exact count of the returned text.
        content.len().div_ceil(4)
    };
    let (selected_tokens, selected_estimated) = if !truncated {
        (tokens_used, false)
    } else if selected.len() == source.len() {
        (file_tokens_total, file_tokens_estimated)
    } else {
        token_total(selected, counter)
    };
    let end_line = if truncated {
        included_end_line(start_line, &content).min(selected_end_line)
    } else {
        selected_end_line
    };
    let elision = truncated.then(|| {
        // An overage taken from an estimated total is worded as one.
        format!(
            "[lines {}-{} elided: {}{} tokens over budget]",
            end_line.max(start_line),
            selected_end_line,
            if selected_estimated { "about " } else { "" },
            selected_tokens.saturating_sub(tokens_used)
        )
    });
    Ok(BoundedRead {
        path: path.to_owned(),
        content,
        start_line,
        end_line,
        selected_end_line,
        file_lines_total: total_lines,
        range_clamped,
        file_tokens_total,
        file_tokens_estimated,
        tokens_used,
        token_provider: counter.provider(),
        truncated,
        elision,
    })
}

/// Most line ranges one [`read_bounded_ranges`] call reads.
pub const READ_MAX_RANGES: usize = 8;

/// One file range of a multi-range read: the whole file, `start_line`
/// through the end, or an inclusive `start_line..=end_line` (an `end_line`
/// past the end clamps, as in [`read_bounded`]).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReadRequest<'a> {
    pub path: &'a str,
    pub start_line: Option<u32>,
    pub end_line: Option<u32>,
}

/// Reads up to [`READ_MAX_RANGES`] ranges under one shared token budget.
///
/// The request list is validated as a whole first (count, and for every
/// range: a path, `end_line` only with `start_line`, `start_line` at least 1
/// and not after `end_line`). Each range then passes every gate
/// [`read_bounded`] applies (root containment, secret paths, the symlink
/// rule, the 16 MiB ceiling) on its own; a range that fails yields its own
/// error and costs no budget. The budget is shared in request order: each
/// range may use an equal share of what the earlier ranges left, so a large
/// first range cannot starve the rest, and each range reports its own
/// truncation.
pub fn read_bounded_ranges(
    root: &Path,
    requests: &[ReadRequest<'_>],
    budget_tokens: usize,
    counter: &dyn TokenCounter,
) -> Result<Vec<Result<BoundedRead, AtlasError>>, AtlasError> {
    validate_read_requests(requests)?;
    let mut remaining = budget_tokens;
    let mut output = Vec::with_capacity(requests.len());
    for (index, request) in requests.iter().enumerate() {
        let share = remaining / (requests.len() - index);
        let read = read_request(root, request, share, counter);
        if let Ok(read) = &read {
            remaining = remaining.saturating_sub(read.tokens_used.min(share));
        }
        output.push(read);
    }
    Ok(output)
}

/// Rejects a malformed multi-range request before any file is opened.
pub fn validate_read_requests(requests: &[ReadRequest<'_>]) -> Result<(), AtlasError> {
    let invalid = |reason| Err(QueryError::InvalidReadRanges { reason }.into());
    if requests.is_empty() || requests.len() > READ_MAX_RANGES {
        return invalid("give 1 to 8 ranges");
    }
    for request in requests {
        if request.path.is_empty() || request.path.contains('\0') {
            return invalid("every range needs a non-empty path without NUL");
        }
        match (request.start_line, request.end_line) {
            (None, Some(_)) => return invalid("end_line requires start_line"),
            (Some(0), _) => return invalid("start_line must be at least 1"),
            (Some(start), Some(end)) if end < start => {
                return invalid("end_line must not be before start_line");
            }
            _ => {}
        }
    }
    Ok(())
}

fn read_request(
    root: &Path,
    request: &ReadRequest<'_>,
    budget_tokens: usize,
    counter: &dyn TokenCounter,
) -> Result<BoundedRead, AtlasError> {
    let range = match (request.start_line, request.end_line) {
        (Some(start), Some(end)) => Some((start, end)),
        (Some(start), None) => Some((start, file_line_count(root, request.path)?)),
        _ => None,
    };
    read_bounded(root, request.path, range, budget_tokens, counter)
}

/// Returns resolved reverse dependencies at minimum depths one and two only.
pub fn impact(store: &Store, repo: RepoId, path: &str) -> Result<ImpactReport, AtlasError> {
    impact_with_options(store, repo, path, ImpactOptions::default())
}

/// Returns reverse dependencies under explicit depth/result bounds with completeness metadata.
pub fn impact_with_options(
    store: &Store,
    repo: RepoId,
    path: &str,
    options: ImpactOptions,
) -> Result<ImpactReport, AtlasError> {
    if !(1..=2).contains(&options.max_depth) {
        return Err(QueryError::InvalidImpactOptions {
            reason: "max_depth must be between 1 and 2",
        }
        .into());
    }
    if !(1..=500).contains(&options.max_results) {
        return Err(QueryError::InvalidImpactOptions {
            reason: "max_results must be between 1 and 500",
        }
        .into());
    }
    if is_secret_path(Path::new(path)) || store.indexed_file(repo, path)?.is_none() {
        return Err(AtlasError::FileNotIndexed {
            repo,
            path: path.to_owned(),
        });
    }

    let mut incoming = BTreeMap::<String, BTreeMap<String, Vec<EdgeKind>>>::new();
    for edge in store.resolved_edges(repo)? {
        if is_secret_path(Path::new(&edge.from_path)) || is_secret_path(Path::new(&edge.to_path)) {
            continue;
        }
        let kinds = incoming
            .entry(edge.to_path)
            .or_default()
            .entry(edge.from_path)
            .or_default();
        if !kinds.contains(&edge.kind) {
            kinds.push(edge.kind);
            kinds.sort_by_key(|kind| edge_kind_order(*kind));
        }
    }

    let mut discovered = BTreeMap::<String, (u8, Vec<EdgeKind>)>::new();
    let mut frontier = BTreeSet::from([path.to_owned()]);
    for depth in 1..=options.max_depth {
        let mut next = BTreeSet::new();
        for target in &frontier {
            let Some(importers) = incoming.get(target) else {
                continue;
            };
            for (importer, kinds) in importers {
                if importer == path {
                    continue;
                }
                match discovered.get_mut(importer) {
                    Some((known_depth, known_kinds)) if *known_depth == depth => {
                        for kind in kinds {
                            if !known_kinds.contains(kind) {
                                known_kinds.push(*kind);
                            }
                        }
                        known_kinds.sort_by_key(|kind| edge_kind_order(*kind));
                    }
                    Some(_) => {}
                    None => {
                        discovered.insert(importer.clone(), (depth, kinds.clone()));
                        next.insert(importer.clone());
                    }
                }
            }
        }
        frontier = next;
    }

    let mut dependents = discovered
        .into_iter()
        .map(|(path, (depth, kinds))| ImpactEntry {
            path,
            depth,
            edge_kinds: kinds,
        })
        .collect::<Vec<_>>();
    dependents.sort_by(|left, right| {
        left.depth
            .cmp(&right.depth)
            .then_with(|| left.path.cmp(&right.path))
    });
    let truncated = dependents.len() > options.max_results;
    dependents.truncate(options.max_results);
    let resolution_counts = store.edge_resolution_counts(repo)?;
    Ok(ImpactReport {
        path: path.to_owned(),
        dependents,
        unresolved_edges: resolution_counts.unresolved,
        ambiguous_edges: resolution_counts.ambiguous,
        complete: resolution_counts.unresolved == 0
            && resolution_counts.ambiguous == 0
            && !truncated,
        truncated,
    })
}

const fn edge_kind_order(kind: EdgeKind) -> u8 {
    match kind {
        EdgeKind::Imports => 0,
        EdgeKind::References => 1,
        EdgeKind::Tests => 2,
    }
}

/// Builds a focused map and file-diverse, FTS-backed evidence excerpts under one budget.
pub fn plan_context(
    store: &Store,
    repo: RepoId,
    task: &str,
    budget_tokens: usize,
) -> Result<ContextPlan, AtlasError> {
    // Instructions to the answerer name nothing to retrieve, and an idiom
    // no identifier can spell is read as the word code uses (D95). Names and
    // numbers written as code are still read from the task as given (D88).
    let focus = task_focus(task);
    let mut concepts = TaskConcepts::parse(&focus, task);
    let ConceptSearch {
        search_queries,
        pool,
        matches_total,
        truncated: search_truncated,
        content_scan_truncated,
    } = search_task_concepts(store, repo, &focus, task, &mut concepts)?;

    // Read each pooled symbol once: the body both ranks the hit by what the
    // code contains and becomes its evidence excerpt.
    let mut scored = Vec::with_capacity(pool.len());
    for ranked in pool {
        let body = context_symbol_body(store, repo, &ranked.hit)?;
        let mut score = ranked.score;
        let specific = ranked.specific;
        let mut anchored = ranked.defines_use_site;
        if let Some(body) = &body {
            score += concepts.body_score(&body.content);
            anchored |= concepts.mentions_use_site(&body.content);
        }
        if !concepts.wants_tests && is_test_hit(&ranked.hit) {
            score *= PLAN_CONTEXT_TEST_PENALTY;
        }
        scored.push(ContextCandidate {
            score,
            named: ranked.named,
            order: ranked.order,
            hit: ranked.hit,
            top: false,
            raw_tokens: body.as_ref().map_or(0, |body| body.symbol.token_estimate),
            evidence: body,
            anchored,
            specific,
            facet: false,
            focus_line: None,
            facet_index: None,
        });
    }
    // What the task names verbatim leads, whatever prose words score.
    scored.sort_by(|left, right| {
        right
            .named
            .cmp(&left.named)
            .then(right.score.total_cmp(&left.score))
            .then(left.order.cmp(&right.order))
    });
    // Named symbols sort first, so the best score is not always the first.
    let score_floor = scored
        .iter()
        .map(|candidate| candidate.score)
        .reduce(f64::max)
        .map_or(0.0, |best| best * PLAN_CONTEXT_SCORE_FLOOR_RATIO);
    let search = PackSearch {
        search_queries,
        matches_total,
        search_truncated,
        content_scan_truncated,
    };
    // A task asking several things gets each part's best evidence, looked
    // for among the candidates and their neighbours in the leading files,
    // ahead of the floor and the per-file share (D95). Ranked words cannot
    // tell whether a part is covered before the pack is cut: a larger budget
    // lets unrelated excerpts spell a part's words, so the choice is made
    // for every several-part task.
    if concepts.facets.len() > 1 {
        add_facet_evidence(store, repo, &concepts, &mut scored)?;
    }
    assemble_context_plan(
        store,
        repo,
        &concepts,
        scored,
        score_floor,
        budget_tokens,
        &search,
    )
}

/// What the concept search reported, carried into every assembly of a pack.
struct PackSearch {
    search_queries: Vec<String>,
    matches_total: usize,
    search_truncated: bool,
    content_scan_truncated: bool,
}

/// Fits the scored candidates, sorted best first, into one pack under
/// `budget_tokens`.
fn assemble_context_plan(
    store: &Store,
    repo: RepoId,
    concepts: &TaskConcepts,
    mut scored: Vec<ContextCandidate>,
    score_floor: f64,
    budget_tokens: usize,
    search: &PackSearch,
) -> Result<ContextPlan, AtlasError> {
    let counter = counter_for(TokenProvider::OpenAiO200k);
    scored.retain(|candidate| {
        candidate.facet
            || candidate.score >= score_floor
            || candidate.score >= PLAN_CONTEXT_SPECIFIC_MATCH_SCORE
    });
    let mut kept = 0;
    scored.retain(|candidate| {
        if candidate.facet {
            return true;
        }
        kept += 1;
        kept <= PLAN_CONTEXT_SEARCH_LIMIT
    });

    // Excerpt every candidate once. The best strong items get the largest
    // allowance; the same excerpts decide below whether the tail is needed.
    let top_allowance = (budget_tokens / PLAN_CONTEXT_TOP_EVIDENCE_BUDGET_DIVISOR).clamp(
        PLAN_CONTEXT_STRONG_EVIDENCE_TOKENS,
        PLAN_CONTEXT_TOP_EVIDENCE_TOKENS,
    );
    for (rank, candidate) in scored.iter_mut().enumerate() {
        let strong = candidate.facet || candidate.score >= PLAN_CONTEXT_STRONG_SCORE;
        candidate.top = strong && rank < PLAN_CONTEXT_TOP_EVIDENCE_ITEMS;
        let content_budget = if candidate.top {
            top_allowance
        } else if strong {
            PLAN_CONTEXT_STRONG_EVIDENCE_TOKENS
        } else {
            PLAN_CONTEXT_EVIDENCE_CONTENT_TOKENS
        };
        let focus_line = candidate.focus_line;
        candidate.evidence = candidate.evidence.take().map(|body| match focus_line {
            // A clause's evidence starts just above its decision line.
            Some(line) if counter.count(&body.content) > content_budget => {
                excerpt_context_evidence(&body, line.saturating_sub(1), content_budget, counter)
            }
            _ => compact_context_evidence(&body, content_budget, counter, concepts),
        });
    }

    // A candidate below the relative floor stayed only because its absolute
    // score cleared the specific-match bar. Once the evidence that is above
    // the floor, or defines or uses a named task concept, or matched a
    // specific task word, covers the task, the rest matched nothing but
    // broad words and is dropped.
    let is_core = |candidate: &ContextCandidate| {
        candidate.facet
            || candidate.score >= score_floor
            || candidate.anchored
            || candidate.specific
    };
    if scored.iter().any(|candidate| !is_core(candidate)) {
        let core_uncovered = concepts.uncovered(
            scored
                .iter()
                .filter(|candidate| is_core(candidate))
                .filter_map(|candidate| candidate.evidence.as_ref()),
        );
        if core_uncovered.len().saturating_mul(2) <= concepts.salient.len() {
            scored.retain(is_core);
        }
    }
    let hits = scored
        .iter()
        .map(|candidate| candidate.hit.clone())
        .collect::<Vec<_>>();

    let candidate_outlines = matching_context_outlines(store, repo, &hits)?;
    let mut bodies_by_path = Vec::<(String, Vec<PackItem>)>::new();
    let mut estimated_raw_source_tokens = 0_usize;
    // Files keep the order of their best candidate. A clause's chosen
    // evidence leads its file's items and is outside the per-file share; the
    // file's other candidates fill what remains of that share (D95).
    for candidate in &scored {
        if !bodies_by_path
            .iter()
            .any(|(path, _)| path == &candidate.hit.path)
        {
            bodies_by_path.push((candidate.hit.path.clone(), Vec::new()));
        }
    }
    let (chosen, ranked): (Vec<_>, Vec<_>) =
        scored.into_iter().partition(|candidate| candidate.facet);
    for candidate in chosen.into_iter().chain(ranked) {
        let Some(path_index) = bodies_by_path
            .iter()
            .position(|(path, _)| path == &candidate.hit.path)
        else {
            continue;
        };
        let path_bodies = &mut bodies_by_path[path_index].1;
        // A file already holding its share still admits what defines or uses
        // a named task concept: the raise site next to the definition.
        let file_limit = if candidate.facet {
            usize::MAX
        } else if candidate.anchored {
            PLAN_CONTEXT_USE_SITE_BODIES_PER_FILE
        } else {
            PLAN_CONTEXT_BODIES_PER_FILE
        };
        if path_bodies.len() >= file_limit {
            continue;
        }
        if let Some(evidence) = candidate.evidence {
            estimated_raw_source_tokens =
                estimated_raw_source_tokens.saturating_add(candidate.raw_tokens);
            path_bodies.push(PackItem {
                evidence,
                top: candidate.top,
                // A file whose chosen excerpt did not fit is not complete.
                anchored: candidate.anchored || candidate.facet,
            });
        }
    }
    let deepest = bodies_by_path
        .iter()
        .map(|(_, bodies)| bodies.len())
        .max()
        .unwrap_or(0);
    let mut candidate_bodies = Vec::new();
    for rank in 0..deepest {
        for (_, bodies) in &bodies_by_path {
            if let Some(body) = bodies.get(rank) {
                candidate_bodies.push(body.clone());
            }
        }
    }
    let evidence_files_total = candidate_bodies
        .iter()
        .map(|item| item.evidence.path.as_str())
        .collect::<BTreeSet<_>>()
        .len();

    let mut rendered = String::new();
    let mut included_map = None;
    let map = if hits.is_empty() {
        repo_map(store, repo, budget_tokens / 5)?
    } else {
        focused_context_map(&hits, budget_tokens / 5, counter)
    };
    if !map.rendered.is_empty() {
        let trial = render_context_item(
            &rendered,
            "# Relevant repository map\n",
            &map.rendered,
            true,
        );
        if counter.count(&trial) <= budget_tokens {
            rendered = trial;
            included_map = Some(map.clone());
        }
    }

    let mut symbol_bodies = Vec::new();
    let mut constants = ReferencedConstants::default();
    // A file whose definition or use of a named task concept did not fit is
    // not counted as included, so the pack is not reported ready without it.
    let mut use_site_omitted = BTreeSet::new();
    for item in &candidate_bodies {
        let rendered_item = render_context_evidence(&item.evidence, false);
        let trial = render_context_item(
            &rendered,
            "# Matching evidence excerpts\n",
            &rendered_item,
            symbol_bodies.is_empty(),
        );
        if counter.count(&trial) <= budget_tokens {
            rendered = trial;
            symbol_bodies.push(item.evidence.clone());
            constants.append_referenced_by(
                store,
                repo,
                item,
                &candidate_bodies,
                concepts,
                budget_tokens,
                counter,
                &mut rendered,
            )?;
        } else if item.anchored {
            use_site_omitted.insert(item.evidence.path.as_str());
        }
    }
    let referenced_constants = constants.included;
    let included_paths = symbol_bodies
        .iter()
        .map(|body| body.path.as_str())
        .collect::<BTreeSet<_>>();
    let evidence_files_included = included_paths.difference(&use_site_omitted).count();
    let outlines = candidate_outlines
        .into_iter()
        .filter_map(|mut outline| {
            if !included_paths.contains(outline.path.as_str()) {
                return None;
            }
            outline.symbols.retain(|symbol| {
                symbol_bodies
                    .iter()
                    .any(|body| body.path == outline.path && same_symbol(&body.symbol, symbol))
            });
            (!outline.symbols.is_empty()).then_some(outline)
        })
        .collect::<Vec<_>>();

    let uncovered_concepts = concepts.uncovered(&symbol_bodies);
    // A test that spells a clause's words shows the behaviour is tested, not
    // how it works, so it covers a clause only when the task asks for tests.
    let evidence_texts = symbol_bodies
        .iter()
        .filter(|body| concepts.wants_tests || !is_test_symbol(&body.path, &body.symbol))
        .map(evidence_text)
        .collect::<Vec<_>>();
    let uncovered_facets = concepts
        .facets
        .iter()
        .filter(|facet| !facet.covered_by(&evidence_texts))
        .map(|facet| facet.label())
        .collect::<Vec<_>>();
    let task_facets_total = concepts.facets.len();
    let task_facets_covered = task_facets_total - uncovered_facets.len();
    let task_concepts_total = concepts.salient.len();
    let task_concepts_covered = task_concepts_total - uncovered_concepts.len();
    let tokens_used = counter.count(&rendered);
    let truncated = search.search_truncated
        || search.content_scan_truncated
        || map.truncated
        || included_map.is_none()
        || symbol_bodies.len() < candidate_bodies.len();
    Ok(ContextPlan {
        rendered,
        repo_map: included_map,
        outlines,
        symbol_bodies,
        referenced_constants,
        search_queries: search.search_queries.clone(),
        matches_total: search.matches_total,
        evidence_files_total,
        evidence_files_included,
        task_concepts_total,
        task_concepts_covered,
        uncovered_concepts: uncovered_concepts
            .into_iter()
            .take(PLAN_CONTEXT_UNCOVERED_LIMIT)
            .collect(),
        task_facets_total,
        task_facets_covered,
        uncovered_facets: uncovered_facets
            .into_iter()
            .take(PLAN_CONTEXT_UNCOVERED_FACET_LIMIT)
            .collect(),
        estimated_raw_source_tokens,
        tokens_used,
        token_provider: counter.provider(),
        truncated,
        content_scan_truncated: search.content_scan_truncated,
    })
}

/// One ranked symbol on its way into the pack.
#[derive(Clone)]
struct ContextCandidate {
    /// Excerpted with the largest allowance.
    top: bool,
    score: f64,
    named: Option<NamedRank>,
    order: usize,
    hit: SearchHit,
    /// The whole body until the excerpt allowance is applied.
    evidence: Option<ContextSymbolBody>,
    raw_tokens: usize,
    /// Defines or uses one of the task's use-site concepts.
    anchored: bool,
    /// Matched a specific task concept, not only broad words.
    specific: bool,
    /// The best evidence found for one of the task's clauses (D95).
    facet: bool,
    /// Body line a clause's excerpt is centred on.
    focus_line: Option<usize>,
    /// The first clause that chose this candidate.
    facet_index: Option<usize>,
}

/// Leading files whose other symbols are searched for a clause's evidence.
const PLAN_CONTEXT_FACET_FILES: usize = 3;
/// Same-file symbols related by a call to a clause's best evidence that may
/// join it.
const PLAN_CONTEXT_FACET_RELATED: usize = 2;

/// Marks, for each clause of a multi-clause task, the one candidate that
/// mentions most of its words, so it reaches the pack. Candidates are the
/// scored pool plus every other leaf symbol of the leading files: the
/// function next to a matched constant or name is often the one that does
/// what the clause asks. Ties go to code over tests, then the higher score,
/// then the shorter body. A clause no excerpt mentions enough of is left
/// for readiness to report.
fn add_facet_evidence(
    store: &Store,
    repo: RepoId,
    concepts: &TaskConcepts,
    scored: &mut Vec<ContextCandidate>,
) -> Result<(), AtlasError> {
    let mut anchor_files = Vec::<String>::new();
    for candidate in scored.iter() {
        if anchor_files.len() == PLAN_CONTEXT_FACET_FILES {
            break;
        }
        if (concepts.wants_tests || !is_test_hit(&candidate.hit))
            && !anchor_files.contains(&candidate.hit.path)
        {
            anchor_files.push(candidate.hit.path.clone());
        }
    }
    // Every other leaf symbol of the leading files joins the candidates the
    // clauses choose from; only the chosen ones are kept.
    for path in &anchor_files {
        append_file_neighbours(store, repo, path, concepts, scored)?;
    }
    let mut loaded_files = anchor_files;

    for (facet_index, facet) in concepts.facets.iter().enumerate() {
        let needed = facet.words_needed_in_one_excerpt();
        let Some((best, mentioned)) = best_facet_candidate(facet, scored, |_| true) else {
            continue;
        };
        if mentioned < needed {
            continue;
        }
        scored[best].facet = true;
        // What the clause asks is often answered by a helper together with
        // the functions around it: a file-level and a whole-repository check
        // that share one comparison. Same-file symbols that call the chosen
        // one, that it calls, or that call what it calls join it when they
        // name enough of the clause.
        let path = scored[best].hit.path.clone();
        if !loaded_files.contains(&path) {
            append_file_neighbours(store, repo, &path, concepts, scored)?;
            loaded_files.push(path.clone());
        }
        let name = scored[best].hit.symbol.name.clone();
        let chosen_body = scored[best]
            .evidence
            .as_ref()
            .map(|body| body.content.clone())
            .unwrap_or_default();
        let callees = scored
            .iter()
            .filter(|candidate| candidate.hit.path == path && candidate.hit.symbol.name != name)
            .map(|candidate| candidate.hit.symbol.name.clone())
            .filter(|callee| body_calls(&chosen_body, callee))
            .collect::<BTreeSet<_>>();
        let mut group = vec![best];
        for _ in 0..PLAN_CONTEXT_FACET_RELATED {
            let related = best_facet_candidate(facet, scored, |candidate| {
                let Some(body) = candidate.evidence.as_ref() else {
                    return false;
                };
                !candidate.facet
                    && candidate.hit.path == path
                    && (callees.contains(&candidate.hit.symbol.name)
                        || body_calls(&body.content, &name)
                        || callees
                            .iter()
                            .any(|callee| body_calls(&body.content, callee)))
            });
            match related {
                Some((index, mentioned)) if mentioned >= needed => {
                    scored[index].facet = true;
                    group.push(index);
                }
                _ => break,
            }
        }
        for &member in &group {
            scored[member].facet_index.get_or_insert(facet_index);
        }
    }
    // Each chosen excerpt centres on its decision: the first line that calls
    // another chosen symbol of its file, or else the line naming most of its
    // clause.
    for member in 0..scored.len() {
        let Some(facet_index) = scored[member].facet_index else {
            continue;
        };
        let others = scored
            .iter()
            .enumerate()
            .filter(|(other, candidate)| {
                *other != member && candidate.facet && candidate.hit.path == scored[member].hit.path
            })
            .map(|(_, candidate)| candidate.hit.symbol.name.clone())
            .collect::<Vec<_>>();
        scored[member].focus_line = scored[member].evidence.as_ref().and_then(|body| {
            facet_focus_line(&body.content, &concepts.facets[facet_index], &others)
        });
    }
    // Neighbours no clause chose were never ranked candidates.
    scored.retain(|candidate| candidate.facet || candidate.order != usize::MAX);
    Ok(())
}

/// Leaf symbols a clause may choose from, added to `scored` with no rank of
/// their own (`order == usize::MAX`) until a clause chooses one.
fn append_file_neighbours(
    store: &Store,
    repo: RepoId,
    path: &str,
    concepts: &TaskConcepts,
    scored: &mut Vec<ContextCandidate>,
) -> Result<(), AtlasError> {
    for (hit, body) in file_leaf_bodies(store, repo, path)? {
        if (!concepts.wants_tests && is_test_hit(&hit))
            || scored
                .iter()
                .any(|candidate| hit_identity(&candidate.hit) == hit_identity(&hit))
        {
            continue;
        }
        scored.push(ContextCandidate {
            top: false,
            score: concepts.body_score(&body.content),
            named: None,
            order: usize::MAX,
            raw_tokens: body.symbol.token_estimate,
            hit,
            evidence: Some(body),
            anchored: false,
            specific: true,
            facet: false,
            focus_line: None,
            facet_index: None,
        });
    }
    Ok(())
}

/// The eligible candidate mentioning most of `facet`'s words, with that
/// count. Ties go to code over tests, then the higher score, then the
/// shorter body, then the earlier candidate.
fn best_facet_candidate(
    facet: &TaskFacet,
    scored: &[ContextCandidate],
    eligible: impl Fn(&ContextCandidate) -> bool,
) -> Option<(usize, usize)> {
    let mut best: Option<(usize, usize)> = None;
    for (index, candidate) in scored.iter().enumerate() {
        let Some(body) = candidate.evidence.as_ref() else {
            continue;
        };
        if !eligible(candidate) {
            continue;
        }
        let mentioned = facet.mentioned_in(&evidence_text(body));
        let better = best.is_none_or(|(known, known_mentioned)| {
            let known = &scored[known];
            mentioned
                .cmp(&known_mentioned)
                .then_with(|| is_test_hit(&known.hit).cmp(&is_test_hit(&candidate.hit)))
                .then_with(|| candidate.score.total_cmp(&known.score))
                .then_with(|| symbol_lines(&known.hit).cmp(&symbol_lines(&candidate.hit)))
                .is_gt()
        });
        if better {
            best = Some((index, mentioned));
        }
    }
    best
}

/// Index of the first line of `content` that calls one of `related`, or
/// else of the earliest line mentioning most of `facet`'s words.
fn facet_focus_line(content: &str, facet: &TaskFacet, related: &[String]) -> Option<usize> {
    let lines = content.split_inclusive('\n').collect::<Vec<_>>();
    if let Some(line) = lines
        .iter()
        .position(|line| related.iter().any(|name| body_calls(line, name)))
    {
        return Some(line);
    }
    let mut best: Option<(usize, usize)> = None;
    for (index, line) in lines.iter().enumerate() {
        let mentioned = facet.mentioned_in(&line.to_lowercase());
        if mentioned > 0 && best.is_none_or(|(_, known)| mentioned > known) {
            best = Some((index, mentioned));
        }
    }
    best.map(|(index, _)| index)
}

fn symbol_lines(hit: &SearchHit) -> u32 {
    hit.symbol.end_line.saturating_sub(hit.symbol.start_line)
}

/// Whether `content` calls `name`: spells `name(` not preceded by an
/// identifier character.
fn body_calls(content: &str, name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    let pattern = format!("{name}(");
    content.match_indices(&pattern).any(|(start, _)| {
        content[..start]
            .chars()
            .next_back()
            .is_none_or(|previous| !(previous.is_alphanumeric() || previous == '_'))
    })
}

/// Every symbol of one indexed file that contains no other symbol, with its
/// current body. A file that changed since indexing, is too large for the
/// content scan, or is a secret yields nothing.
fn file_leaf_bodies(
    store: &Store,
    repo: RepoId,
    path: &str,
) -> Result<Vec<(SearchHit, ContextSymbolBody)>, AtlasError> {
    let Some(record) = store.indexed_file(repo, path)? else {
        return Ok(Vec::new());
    };
    if record.size_bytes > PLAN_CONTEXT_CONTENT_MAX_FILE_BYTES || is_secret_path(Path::new(path)) {
        return Ok(Vec::new());
    }
    let repository = store.repository(repo)?;
    let Ok(root) = canonical_query_root(&repository.root) else {
        return Ok(Vec::new());
    };
    let Ok(ResolvedEntry::File(resolved)) = resolve_under_root(&root, path) else {
        return Ok(Vec::new());
    };
    let Ok(source) = read_utf8_capped(&resolved, PLAN_CONTEXT_CONTENT_MAX_FILE_BYTES) else {
        return Ok(Vec::new());
    };
    // Stored line ranges describe the bytes that were indexed.
    if blake3::hash(source.as_bytes()).to_hex().as_str() != record.hash {
        return Ok(Vec::new());
    }
    let symbols = store.symbols_in_file(repo, path)?;
    let starts = line_starts(&source);
    let mut bodies = Vec::new();
    for symbol in &symbols {
        let contains_other = symbols.iter().any(|other| {
            !same_symbol(other, symbol)
                && other.start_line >= symbol.start_line
                && other.end_line <= symbol.end_line
        });
        let (Some(&start), true) = (
            starts.get(symbol.start_line.saturating_sub(1) as usize),
            symbol.start_line >= 1 && symbol.end_line >= symbol.start_line,
        ) else {
            continue;
        };
        if contains_other {
            continue;
        }
        let end = starts
            .get(symbol.end_line as usize)
            .copied()
            .unwrap_or(source.len());
        let Some(content) = source.get(start..end) else {
            continue;
        };
        bodies.push((
            SearchHit {
                path: record.path.clone(),
                language: record.language,
                symbol: symbol.clone(),
                score: 0.0,
            },
            ContextSymbolBody {
                path: record.path.clone(),
                symbol: symbol.clone(),
                content: content.to_owned(),
                excerpt_start_line: symbol.start_line,
                excerpt_end_line: symbol.end_line,
                truncated: false,
            },
        ));
    }
    Ok(bodies)
}

/// One excerpt queued for the pack, in rendering order.
#[derive(Clone)]
struct PackItem {
    evidence: ContextSymbolBody,
    /// Excerpted with the largest allowance.
    top: bool,
    /// Defines or uses one of the task's use-site concepts.
    anchored: bool,
}

/// Reads the current body of an indexed symbol; `None` when it can no longer
/// be resolved or its file grew past the read ceiling since indexing, in
/// which case the rest of the evidence pack is still served.
fn context_symbol_body(
    store: &Store,
    repo: RepoId,
    hit: &SearchHit,
) -> Result<Option<ContextSymbolBody>, AtlasError> {
    let selector = SymbolSelector {
        path: hit.path.clone(),
        qualified_name: hit.symbol.qualified_name.clone(),
        kind: Some(hit.symbol.kind),
        signature_hash: hit
            .symbol
            .signature
            .as_deref()
            .map(|signature| blake3::hash(signature.as_bytes()).to_hex().to_string()),
        start_line_hint: Some(hit.symbol.start_line),
    };
    let unfolded = match unfold(store, repo, &selector) {
        Err(AtlasError::Query(QueryError::FileTooLarge { .. })) => return Ok(None),
        other => other?,
    };
    Ok(match unfolded {
        UnfoldResult::Found {
            path,
            symbol,
            content,
            ..
        } => Some(ContextSymbolBody {
            path,
            excerpt_start_line: symbol.start_line,
            excerpt_end_line: symbol.end_line,
            truncated: false,
            symbol,
            content,
        }),
        _ => None,
    })
}

/// Constant definitions added because an included excerpt references them.
#[derive(Default)]
struct ReferencedConstants {
    included: Vec<ContextSymbolBody>,
    /// Identifiers already looked up, found or not.
    seen: BTreeSet<String>,
    lookups: usize,
}

impl ReferencedConstants {
    /// Appends to `rendered` the definition of each constant `evidence`
    /// references, while the pack budget, the constant cap, and the lookup
    /// cap allow. An excerpt with the largest allowance brings every constant
    /// it references; any other brings only those whose name spells a
    /// salient task word. A constant that is a candidate in its own right is
    /// left to its own place in the pack.
    #[allow(clippy::too_many_arguments)]
    fn append_referenced_by(
        &mut self,
        store: &Store,
        repo: RepoId,
        item: &PackItem,
        candidates: &[PackItem],
        concepts: &TaskConcepts,
        budget_tokens: usize,
        counter: &dyn TokenCounter,
        rendered: &mut String,
    ) -> Result<(), AtlasError> {
        let evidence = &item.evidence;
        for identifier in constant_style_identifiers(&evidence.content) {
            let on_task = item.top
                || concepts
                    .salient
                    .iter()
                    .any(|word| line_spells_word(identifier, word));
            if !on_task {
                continue;
            }
            if self.included.len() == PLAN_CONTEXT_REFERENCED_CONSTANTS
                || self.lookups == PLAN_CONTEXT_REFERENCED_CONSTANT_LOOKUPS
            {
                break;
            }
            if identifier == evidence.symbol.name || !self.seen.insert(identifier.to_owned()) {
                continue;
            }
            self.lookups += 1;
            let Some(hit) = resolve_constant(store, repo, identifier, &evidence.path)? else {
                continue;
            };
            if candidates.iter().any(|known| {
                known.evidence.path == hit.path && same_symbol(&known.evidence.symbol, &hit.symbol)
            }) {
                continue;
            }
            let Some(body) = context_symbol_body(store, repo, &hit)? else {
                continue;
            };
            let constant =
                prefix_context_evidence(&body, PLAN_CONTEXT_REFERENCED_CONSTANT_TOKENS, counter);
            let trial = format!("{rendered}{}", render_context_evidence(&constant, true));
            if counter.count(&trial) <= budget_tokens {
                *rendered = trial;
                self.included.push(constant);
            }
        }
        Ok(())
    }
}

/// Identifiers in `content` spelled the way constants are: at least three
/// characters, starting with an ASCII capital, and made of ASCII capitals,
/// digits, and underscores with at least two capitals. In order of first
/// appearance, without repeats.
fn constant_style_identifiers(content: &str) -> Vec<&str> {
    let mut identifiers = Vec::new();
    for word in content.split(|character: char| !character.is_alphanumeric() && character != '_') {
        let constant_style = word.len() >= 3
            && word.starts_with(|first: char| first.is_ascii_uppercase())
            && word.chars().all(|character| {
                character.is_ascii_uppercase() || character.is_ascii_digit() || character == '_'
            })
            && word
                .chars()
                .filter(|character| character.is_ascii_uppercase())
                .count()
                >= 2;
        if constant_style && !identifiers.contains(&word) {
            identifiers.push(word);
        }
    }
    identifiers
}

/// The indexed constant or static named exactly `identifier`. Among several,
/// the one sharing the longest directory prefix with the referencing file,
/// then the first by path and line.
fn resolve_constant(
    store: &Store,
    repo: RepoId,
    identifier: &str,
    referencing_path: &str,
) -> Result<Option<SearchHit>, AtlasError> {
    let shared_prefix = |path: &str| {
        path.split('/')
            .zip(referencing_path.split('/'))
            .take_while(|(left, right)| left == right)
            .count()
    };
    let mut matches = search_literal(store, repo, identifier, PLAN_CONTEXT_SCORING_WINDOW)?
        .into_iter()
        .filter(|hit| hit.symbol.kind == SymbolKind::Const && hit.symbol.name == identifier)
        .collect::<Vec<_>>();
    matches.sort_by(|left, right| {
        shared_prefix(&right.path)
            .cmp(&shared_prefix(&left.path))
            .then_with(|| left.path.cmp(&right.path))
            .then_with(|| left.symbol.start_line.cmp(&right.symbol.start_line))
    });
    Ok(matches.into_iter().next())
}

/// What the symbol-name queries and the content scan found for one task.
struct ConceptSearch {
    search_queries: Vec<String>,
    pool: Vec<RankedHit>,
    matches_total: usize,
    truncated: bool,
    content_scan_truncated: bool,
}

/// One pooled search hit with its cross-query score and its position in the
/// rank-interleaved order, which breaks score ties.
struct RankedHit {
    hit: SearchHit,
    score: f64,
    order: usize,
    /// Named after one of the task's use-site concepts.
    defines_use_site: bool,
    /// Named after a specific compound or a salient word that names few
    /// symbols, matched the whole task phrase, or contains a salient word
    /// the content scan found in few bodies.
    specific: bool,
    /// Defined under, or in a file at, a name the task spells verbatim.
    named: Option<NamedRank>,
}

type HitIdentity = (String, String, SymbolKind, u32, u32);

fn hit_identity(hit: &SearchHit) -> HitIdentity {
    (
        hit.path.clone(),
        hit.symbol.qualified_name.clone(),
        hit.symbol.kind,
        hit.symbol.start_line,
        hit.symbol.end_line,
    )
}

fn search_task_concepts(
    store: &Store,
    repo: RepoId,
    task: &str,
    raw_task: &str,
    concepts: &mut TaskConcepts,
) -> Result<ConceptSearch, AtlasError> {
    let search_queries = task_search_queries(task);
    let text_requested = task_requests_text_evidence(task);
    let mut exclude_text = !text_requested;
    let (mut query_hits, mut source_truncated) =
        context_query_hits(store, repo, &search_queries, exclude_text)?;
    if !text_requested && query_hits.iter().all(Vec::is_empty) {
        exclude_text = false;
        (query_hits, source_truncated) = context_query_hits(store, repo, &search_queries, false)?;
    }

    // A symbol matched by several task concepts, or by a compound of adjacent
    // task words, outranks one that matched a single generic word.
    let mut scores = BTreeMap::<HitIdentity, f64>::new();
    let phrase = task.trim();
    for (query, hits) in search_queries.iter().zip(&query_hits) {
        let weight = if query == phrase && query.contains(char::is_whitespace) {
            3.0
        } else if hits.len() <= PLAN_CONTEXT_HITS_PER_QUERY {
            2.0
        } else {
            1.0
        };
        for hit in hits {
            *scores.entry(hit_identity(hit)).or_default() += weight;
        }
    }
    let mut compound_hits = Vec::new();
    let mut use_site_concepts = 0;
    let mut use_site_definitions = BTreeSet::new();
    let mut specific_hits = BTreeSet::new();
    for compound in &mut concepts.compounds {
        let mut matched = Vec::new();
        for query in &compound.queries {
            for hit in context_scoring_hits(store, repo, query, exclude_text)? {
                if !matched
                    .iter()
                    .any(|known: &SearchHit| hit_identity(known) == hit_identity(&hit))
                {
                    matched.push(hit);
                }
            }
        }
        // A compound naming many symbols (`mcp server`) is a broad concept,
        // not an identifier: it must not outweigh a rare one (`too large`).
        compound.specificity = if matched.len() <= PLAN_CONTEXT_HITS_PER_QUERY {
            1.0
        } else if matched.len() <= PLAN_CONTEXT_HITS_PER_QUERY * 2 {
            0.5
        } else {
            0.25
        };
        compound.name_hits = matched.len();
        // A compound that names a few symbols is a proven identifier whose
        // other uses sit in bodies. Compounds are ordered longest first, so
        // the first ones are the most specific.
        compound.use_site = compound.name_hits > 0
            && compound.specificity >= 1.0
            && use_site_concepts < PLAN_CONTEXT_USE_SITE_CONCEPTS;
        if compound.use_site {
            use_site_concepts += 1;
        }
        for hit in matched {
            if compound.use_site {
                use_site_definitions.insert(hit_identity(&hit));
            }
            if compound.specificity >= 1.0 {
                specific_hits.insert(hit_identity(&hit));
            }
            *scores.entry(hit_identity(&hit)).or_default() += compound.weight();
            compound_hits.push(hit);
        }
    }

    // A name copied from the code outranks any number of prose words.
    let mut named_ranks = BTreeMap::<HitIdentity, NamedRank>::new();
    let mut named_hits = Vec::new();
    for (hit, rank) in named_symbols(store, repo, raw_task, exclude_text)? {
        let identity = hit_identity(&hit);
        let known = named_ranks.entry(identity.clone()).or_insert(rank);
        if *known < rank {
            *known = rank;
        }
        if !named_hits
            .iter()
            .any(|known: &SearchHit| hit_identity(known) == identity)
        {
            *scores.entry(identity.clone()).or_default() += PLAN_CONTEXT_NAMED_SCORE;
            specific_hits.insert(identity);
            named_hits.push(hit);
        }
    }

    // A concept no symbol is named after can still be written in a body:
    // look for those, and only those, in the indexed files themselves.
    let terms = unresolved_content_terms(
        store,
        repo,
        concepts,
        &search_queries,
        &query_hits,
        exclude_text,
    )?;
    let scan = scan_content_terms(store, repo, &terms, text_requested, concepts)?;
    concepts.content_words = scan.words;
    // A query naming only a few symbols is specific when it is the whole
    // task phrase or a salient word; one naming many is broad.
    for (query, hits) in search_queries.iter().zip(&query_hits) {
        let salient = concepts
            .salient
            .iter()
            .any(|word| word == query || singular_context_term(word).as_ref() == Some(query));
        if hits.len() <= PLAN_CONTEXT_HITS_PER_QUERY
            && (salient || (query == phrase && query.contains(char::is_whitespace)))
        {
            specific_hits.extend(hits.iter().map(hit_identity));
        }
    }

    // Interleave queries by rank so one broad concept cannot crowd every
    // other task concept out of the bounded context pack. Only the leading
    // hits of each query count as matches; the wider window adds candidates.
    let mut merged = Vec::new();
    let mut seen = BTreeSet::new();
    let mut matches_total = 0;
    for (start, end) in [
        (0, PLAN_CONTEXT_HITS_PER_QUERY),
        (PLAN_CONTEXT_HITS_PER_QUERY, PLAN_CONTEXT_SCORING_WINDOW),
    ] {
        for rank in start..end {
            for hits in &query_hits {
                let Some(hit) = hits.get(rank) else {
                    continue;
                };
                if seen.insert(hit_identity(hit)) {
                    merged.push(hit.clone());
                }
            }
        }
        if start == 0 {
            matches_total = merged.len();
        }
    }
    for hit in compound_hits.into_iter().chain(named_hits) {
        if seen.insert(hit_identity(&hit)) {
            merged.push(hit);
            matches_total += 1;
        }
    }
    let mut content_hits = BTreeSet::new();
    for (hit, score, word_match) in scan.candidates {
        if word_match {
            specific_hits.insert(hit_identity(&hit));
        }
        *scores.entry(hit_identity(&hit)).or_default() += score;
        content_hits.insert(hit_identity(&hit));
        if seen.insert(hit_identity(&hit)) {
            merged.push(hit);
            matches_total += 1;
        }
    }

    let truncated = source_truncated || matches_total > PLAN_CONTEXT_SEARCH_LIMIT;
    let mut pool = merged
        .into_iter()
        .enumerate()
        .map(|(order, hit)| RankedHit {
            score: scores.get(&hit_identity(&hit)).copied().unwrap_or_default(),
            defines_use_site: use_site_definitions.contains(&hit_identity(&hit)),
            specific: specific_hits.contains(&hit_identity(&hit)),
            named: named_ranks.get(&hit_identity(&hit)).copied(),
            hit,
            order,
        })
        .collect::<Vec<_>>();
    pool.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then(left.order.cmp(&right.order))
    });
    // Content candidates are already capped and known to contain a task
    // concept, and named symbols are few, so the name-hit bound drops neither.
    let mut name_hits = 0;
    pool.retain(|ranked| {
        if ranked.named.is_some() || content_hits.contains(&hit_identity(&ranked.hit)) {
            return true;
        }
        name_hits += 1;
        name_hits <= PLAN_CONTEXT_RERANK_POOL
    });
    Ok(ConceptSearch {
        search_queries,
        pool,
        matches_total,
        truncated,
        content_scan_truncated: scan.truncated,
    })
}

/// One task concept searched for in file content because no symbol name,
/// qualified name, or signature matched it.
struct ContentTerm {
    /// Lowercase text to find: a compound's separator-free spelling, or one word.
    needle: String,
    /// Lowercase text every matching line contains: the compound's first
    /// word, or the word's shortest spelling.
    anchor: String,
    compound: bool,
    /// A numeric literal from the task, matched only as a whole number (D88).
    numeric: bool,
    /// Pool score of a symbol containing the term, before breadth scaling.
    score: f64,
}

impl ContentTerm {
    /// `line` is one source line that contains the anchor, and
    /// `lowercase_line` the same line in ASCII lowercase.
    fn matches_line(&self, line: &str, lowercase_line: &str) -> bool {
        if self.numeric {
            line_has_number(line, &self.needle)
        } else if self.compound {
            squeeze_separators(lowercase_line).contains(&self.needle)
        } else {
            line_spells_word(line, &self.needle)
        }
    }
}

/// The concepts the content scan looks for: at most
/// `PLAN_CONTEXT_CONTENT_TERMS` compounds and salient words, each in task
/// order, then the task's numeric literals and verbatim identifiers in slots
/// of their own (D88).
fn unresolved_content_terms(
    store: &Store,
    repo: RepoId,
    concepts: &TaskConcepts,
    search_queries: &[String],
    query_hits: &[Vec<SearchHit>],
    exclude_text: bool,
) -> Result<Vec<ContentTerm>, AtlasError> {
    let mut terms = Vec::new();
    // The use-site concepts are proven identifiers whose other uses sit in
    // bodies; a compound that names no symbol is only a guess at one.
    let named = concepts
        .compounds
        .iter()
        .filter(|compound| compound.use_site);
    // Shorter guesses first: whatever spells a three-word compound also
    // spells the two-word compounds inside it.
    let unnamed = [2, 3].into_iter().flat_map(|words| {
        concepts
            .compounds
            .iter()
            .filter(move |compound| compound.name_hits == 0 && compound.words == words)
    });
    for compound in named
        .chain(unnamed)
        .take(PLAN_CONTEXT_CONTENT_COMPOUND_TERMS)
    {
        terms.push(ContentTerm {
            needle: compound.joined.clone(),
            anchor: compound.leading.clone(),
            compound: true,
            numeric: false,
            score: compound.weight() * 0.75,
        });
    }
    for word in &concepts.salient {
        if terms.len() >= PLAN_CONTEXT_CONTENT_TERMS {
            break;
        }
        // Numbers are scanned below, as whole literals only.
        if word.bytes().all(|byte| byte.is_ascii_digit()) {
            continue;
        }
        let singular = singular_context_term(word);
        let mut named = false;
        for spelling in std::iter::once(word).chain(singular.as_ref()) {
            named |= match search_queries.iter().position(|query| query == spelling) {
                Some(index) => !query_hits[index].is_empty(),
                None => !context_scoring_hits(store, repo, spelling, exclude_text)?.is_empty(),
            };
        }
        if !named {
            terms.push(ContentTerm {
                needle: word.clone(),
                anchor: singular.unwrap_or_else(|| word.clone()),
                compound: false,
                numeric: false,
                score: PLAN_CONTEXT_CONTENT_WORD_SCORE,
            });
        }
    }
    terms.truncate(PLAN_CONTEXT_CONTENT_TERMS);
    // Numeric literals have their own slots: no symbol is named after one,
    // and a body that holds the exact number is strong evidence.
    terms.extend(concepts.numbers.iter().map(|number| ContentTerm {
        needle: number.clone(),
        anchor: number.clone(),
        compound: false,
        numeric: true,
        score: PLAN_CONTEXT_CONTENT_NUMBER_SCORE,
    }));
    // So do identifiers the task names verbatim: their use sites are bodies.
    terms.extend(concepts.identifiers.iter().map(|name| ContentTerm {
        needle: squeeze_separators(&name.to_ascii_lowercase()),
        anchor: name.to_ascii_lowercase(),
        compound: true,
        numeric: false,
        score: PLAN_CONTEXT_CONTENT_IDENTIFIER_SCORE,
    }));
    Ok(terms)
}

/// Symbols whose bodies contain an unresolved task concept.
#[derive(Default)]
struct ContentScan {
    /// Best-scored first; at most `PLAN_CONTEXT_CONTENT_CANDIDATES`. The
    /// flag marks a symbol containing a scanned single word, which is found
    /// in few bodies or adds nothing.
    candidates: Vec<(SearchHit, f64, bool)>,
    /// Scanned single words that produced a candidate.
    words: Vec<String>,
    /// A file or byte cap ended the scan before every indexed file was read.
    truncated: bool,
}

/// Searches the bound repository's indexed files for `terms` and maps every
/// matching line to the smallest indexed symbol containing it.
///
/// Read-only and deterministic: files come from the index in path order and
/// the scan stops at count and byte caps, never at a deadline. Secret paths,
/// text files the task did not ask for, files above the per-file cap, files
/// that changed since indexing, and files that cannot be read as UTF-8 below
/// the repository root are skipped. Lines outside every symbol are ignored.
fn scan_content_terms(
    store: &Store,
    repo: RepoId,
    terms: &[ContentTerm],
    include_text: bool,
    concepts: &TaskConcepts,
) -> Result<ContentScan, AtlasError> {
    let mut scan = ContentScan::default();
    if terms.is_empty() {
        return Ok(scan);
    }
    let repository = store.repository(repo)?;
    let Ok(root) = canonical_query_root(&repository.root) else {
        return Ok(scan);
    };
    let mut hits = Vec::<SearchHit>::new();
    // What the planner's body rerank will add for each hit, so the candidate
    // cap keeps the symbols it would rank highest.
    let mut body_scores = Vec::<f64>::new();
    let mut term_hits = vec![Vec::<usize>::new(); terms.len()];
    let mut files_scanned = 0_usize;
    let mut bytes_read = 0_u64;
    for record in store.indexed_files(repo)? {
        if record.language.is_none()
            || (record.language == Some(FileLanguage::Text) && !include_text)
            || record.size_bytes > PLAN_CONTEXT_CONTENT_MAX_FILE_BYTES
            || is_secret_path(Path::new(&record.path))
        {
            continue;
        }
        if files_scanned == PLAN_CONTEXT_CONTENT_MAX_FILES
            || bytes_read >= PLAN_CONTEXT_CONTENT_MAX_BYTES
        {
            scan.truncated = true;
            break;
        }
        files_scanned += 1;
        let Ok(ResolvedEntry::File(path)) = resolve_under_root(&root, &record.path) else {
            continue;
        };
        let Ok(source) = read_utf8_capped(&path, PLAN_CONTEXT_CONTENT_MAX_FILE_BYTES) else {
            continue;
        };
        bytes_read = bytes_read.saturating_add(u64::try_from(source.len()).unwrap_or(u64::MAX));
        let lowercase = source.to_ascii_lowercase();
        let mut fresh = None;
        let mut starts = None;
        let mut symbols = None;
        for (term, found) in terms.iter().zip(&mut term_hits) {
            // End of the last line examined. Matches arrive in ascending
            // offset order, so a match before it lies on that line, and the
            // search for a line start never looks behind it: each byte is
            // visited a bounded number of times however long a line is.
            let mut examined_end = 0_usize;
            for (offset, _) in lowercase.match_indices(term.anchor.as_str()) {
                if content_term_saturated(term, found) || fresh == Some(false) {
                    break;
                }
                if offset < examined_end {
                    continue;
                }
                let bytes = lowercase.as_bytes();
                let start = bytes[examined_end..offset]
                    .iter()
                    .rposition(|byte| *byte == b'\n')
                    .map_or(examined_end, |newline| examined_end + newline + 1);
                let end = bytes[offset..]
                    .iter()
                    .position(|byte| *byte == b'\n')
                    .map_or(bytes.len(), |newline| offset + newline);
                examined_end = end;
                if !term.matches_line(&source[start..end], &lowercase[start..end]) {
                    continue;
                }
                // The stored outline maps lines to symbols only for the
                // bytes that were indexed.
                let fresh = *fresh.get_or_insert_with(|| {
                    blake3::hash(source.as_bytes()).to_hex().as_str() == record.hash
                });
                if !fresh {
                    break;
                }
                let starts = starts.get_or_insert_with(|| line_starts(&source));
                let index = starts.partition_point(|&line_start| line_start <= start) - 1;
                let symbols = match &symbols {
                    Some(symbols) => symbols,
                    None => symbols.insert(store.symbols_in_file(repo, &record.path)?),
                };
                let line_number = u32::try_from(index + 1).unwrap_or(u32::MAX);
                let Some(symbol) = smallest_enclosing_symbol(symbols, line_number) else {
                    continue;
                };
                let hit = hits
                    .iter()
                    .position(|known| {
                        known.path == record.path && same_symbol(&known.symbol, symbol)
                    })
                    .unwrap_or_else(|| {
                        let body = source_line_range(
                            &source,
                            path.path(),
                            symbol.start_line,
                            symbol.end_line,
                        );
                        body_scores.push(body.map_or(0.0, |body| concepts.body_score(body)));
                        hits.push(SearchHit {
                            path: record.path.clone(),
                            language: record.language,
                            symbol: symbol.clone(),
                            score: 0.0,
                        });
                        hits.len() - 1
                    });
                if !found.contains(&hit) {
                    found.push(hit);
                }
            }
        }
    }

    let mut scores = vec![0.0_f64; hits.len()];
    let mut word_matches = vec![false; hits.len()];
    for (term, found) in terms.iter().zip(&term_hits) {
        let breadth = if found.len() <= PLAN_CONTEXT_HITS_PER_QUERY {
            1.0
        } else if content_term_saturated(term, found) {
            continue;
        } else if found.len() <= PLAN_CONTEXT_HITS_PER_QUERY * 2 {
            0.5
        } else {
            0.25
        };
        if !term.compound && !found.is_empty() {
            scan.words.push(term.needle.clone());
        }
        for &hit in found {
            scores[hit] += term.score * breadth;
            word_matches[hit] |= !term.compound;
        }
    }
    // Rank as the planner will: test code yields to the code it tests.
    let mut ranked = hits
        .into_iter()
        .zip(scores.into_iter().zip(body_scores).zip(word_matches))
        .filter(|(_, ((score, _), _))| *score > 0.0)
        .map(|(hit, ((score, body_score), word_match))| {
            let mut rank = score + body_score;
            if !concepts.wants_tests && is_test_hit(&hit) {
                rank *= PLAN_CONTEXT_TEST_PENALTY;
            }
            (rank, hit, score, word_match)
        })
        .collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        right
            .0
            .total_cmp(&left.0)
            .then_with(|| left.1.path.cmp(&right.1.path))
            .then_with(|| left.1.symbol.start_line.cmp(&right.1.symbol.start_line))
            .then_with(|| left.1.symbol.end_line.cmp(&right.1.symbol.end_line))
            .then_with(|| {
                left.1
                    .symbol
                    .qualified_name
                    .cmp(&right.1.symbol.qualified_name)
            })
    });
    ranked.truncate(PLAN_CONTEXT_CONTENT_CANDIDATES);
    scan.candidates = ranked
        .into_iter()
        .map(|(_, hit, score, word_match)| (hit, score, word_match))
        .collect();
    Ok(scan)
}

/// A single word found in more symbols than a specific name query returns is
/// a common word, not evidence: it stops being collected and adds nothing.
fn content_term_saturated(term: &ContentTerm, found: &[usize]) -> bool {
    !term.compound && found.len() > PLAN_CONTEXT_HITS_PER_QUERY
}

/// Whether `line` spells `word`, or its singular or plural, as a whole word:
/// alone, or as one part of a `snake_case`, `kebab-case`, or `camelCase`
/// identifier. `one` is not spelled by `done`, nor `check` by `checksum`.
fn line_spells_word(line: &str, word: &str) -> bool {
    let singular = singular_context_term(word);
    let spelled = |part: &str| {
        let part = part.to_ascii_lowercase();
        part == word
            || singular.as_deref() == Some(part.as_str())
            || singular_context_term(&part).as_deref() == Some(word)
    };
    line.split(|character: char| !character.is_alphanumeric())
        .filter(|chunk| !chunk.is_empty())
        .any(|chunk| {
            // Split `verifyChecksum` and `HTTPServer` where the case turns.
            let characters = chunk.char_indices().collect::<Vec<_>>();
            let mut start = 0;
            for window in 1..characters.len() {
                let (offset, current) = characters[window];
                let previous = characters[window - 1].1;
                let next_is_lower = characters
                    .get(window + 1)
                    .is_some_and(|(_, next)| next.is_lowercase());
                let boundary = current.is_uppercase()
                    && (previous.is_lowercase() || (previous.is_uppercase() && next_is_lower));
                if boundary {
                    if spelled(&chunk[start..offset]) {
                        return true;
                    }
                    start = offset;
                }
            }
            spelled(&chunk[start..]) || (start > 0 && spelled(chunk))
        })
}

pub(crate) fn smallest_enclosing_symbol(
    symbols: &[SymbolRecord],
    line: u32,
) -> Option<&SymbolRecord> {
    symbols
        .iter()
        .filter(|symbol| symbol.start_line <= line && line <= symbol.end_line)
        .min_by_key(|symbol| symbol.end_line - symbol.start_line)
}

fn context_scoring_hits(
    store: &Store,
    repo: RepoId,
    query: &str,
    exclude_text: bool,
) -> Result<Vec<SearchHit>, AtlasError> {
    if exclude_text {
        search_excluding_language(
            store,
            repo,
            query,
            PLAN_CONTEXT_SCORING_WINDOW,
            FileLanguage::Text,
        )
    } else {
        search_literal(store, repo, query, PLAN_CONTEXT_SCORING_WINDOW)
    }
}

/// The task's vocabulary beyond its per-word FTS queries: compounds of
/// adjacent words that can name an identifier, and the salient words the
/// evidence is expected to mention.
struct TaskConcepts {
    compounds: Vec<TaskCompound>,
    salient: Vec<String>,
    /// The task's clauses that name at least one salient word (D95).
    facets: Vec<TaskFacet>,
    /// Salient words no symbol is named after that the content scan found in
    /// a symbol body.
    content_words: Vec<String>,
    /// Integer literals of two or more digits written in the task (D88).
    numbers: Vec<String>,
    /// Symbol identifiers written verbatim in the task, for use sites (D88).
    identifiers: Vec<String>,
    wants_tests: bool,
}

struct TaskCompound {
    /// Identifier spellings searched in the symbol index.
    queries: Vec<String>,
    /// Lowercase words joined without separators, compared against
    /// separator-free lowercase source text.
    joined: String,
    /// First word of the compound; any line spelling the compound contains it.
    leading: String,
    words: usize,
    /// Symbols whose name, qualified name, or signature spell the compound.
    name_hits: usize,
    /// 1.0 for a compound naming few symbols, less for one naming many.
    specificity: f64,
    /// One of the few specific named compounds whose uses in other symbols'
    /// bodies are evidence too.
    use_site: bool,
}

/// One clause of the task and the concepts it names (D95).
struct TaskFacet {
    clause: String,
    /// The clause's distinct salient words and numeric literals, lowercase,
    /// in clause order.
    words: Vec<String>,
}

impl TaskFacet {
    /// The clause as reported when uncovered, cut at a character boundary.
    fn label(&self) -> String {
        let mut label = self
            .clause
            .chars()
            .take(PLAN_CONTEXT_FACET_LABEL_CHARS)
            .collect::<String>();
        if self.clause.chars().count() > PLAN_CONTEXT_FACET_LABEL_CHARS {
            label.push('…');
        }
        label
    }

    /// How many of the facet's words lowercase evidence `text` mentions. A
    /// numeric literal counts only as a whole number.
    fn mentioned_in(&self, text: &str) -> usize {
        self.words
            .iter()
            .filter(|word| facet_word_mentioned(text, word))
            .count()
    }

    /// Words one excerpt must mention for the facet to count as covered:
    /// all of a one-word facet, one of a two-word facet, else a third of
    /// the words and never fewer than two. Words scattered over unrelated
    /// excerpts do not answer a clause; an excerpt naming several of them
    /// does.
    fn words_needed_in_one_excerpt(&self) -> usize {
        match self.words.len() {
            0..=2 => 1,
            words => words.div_ceil(3).max(2),
        }
    }

    /// Whether `texts`, the lowercase evidence of a pack, cover the facet:
    /// together they mention at least half of its words, and one of them
    /// mentions `words_needed_in_one_excerpt`.
    fn covered_by(&self, texts: &[String]) -> bool {
        let mentioned = self
            .words
            .iter()
            .filter(|word| texts.iter().any(|text| facet_word_mentioned(text, word)))
            .count();
        let best = texts
            .iter()
            .map(|text| self.mentioned_in(text))
            .max()
            .unwrap_or(0);
        mentioned.saturating_mul(2) >= self.words.len()
            && best >= self.words_needed_in_one_excerpt()
    }
}

fn facet_word_mentioned(text: &str, word: &str) -> bool {
    if word.bytes().all(|byte| byte.is_ascii_digit()) {
        line_has_number(text, word)
    } else {
        text_mentions(text, word)
    }
}

/// A word long enough, and specific enough, that evidence should mention it.
fn is_salient_word(word: &str) -> bool {
    word.chars().count() >= 3 && !is_context_stop_word(word) && !is_generic_task_word(word)
}

/// Whether two lowercase task words name one concept: equal, or one is the
/// other's singular.
fn same_concept_word(left: &str, right: &str) -> bool {
    left == right
        || singular_context_term(left).as_deref() == Some(right)
        || singular_context_term(right).as_deref() == Some(left)
}

impl TaskCompound {
    fn weight(&self) -> f64 {
        let base = if self.words >= 3 { 6.0 } else { 4.0 };
        base * self.specificity
    }
}

impl TaskConcepts {
    fn parse(task: &str, raw_task: &str) -> Self {
        let words = task
            .split(|character: char| !character.is_alphanumeric() && character != '_')
            .filter(|word| !word.is_empty())
            .map(str::to_ascii_lowercase)
            .collect::<Vec<_>>();
        let wants_tests = words
            .iter()
            .any(|word| matches!(word.as_str(), "test" | "tests" | "tested" | "testing"));

        let mut salient = Vec::<String>::new();
        for word in &words {
            if !is_salient_word(word) {
                continue;
            }
            if !salient.iter().any(|term| same_concept_word(term, word)) {
                salient.push(word.clone());
            }
        }
        let mut facets = Vec::<TaskFacet>::new();
        for clause in task_clauses(task) {
            let numbers = task_numbers(&clause);
            let mut facet_words = Vec::<String>::new();
            for word in clause
                .split(|character: char| !character.is_alphanumeric() && character != '_')
                .map(str::to_ascii_lowercase)
            {
                let number = numbers.contains(&word);
                if (number || is_salient_word(&word))
                    && !facet_words
                        .iter()
                        .any(|known| same_concept_word(known, &word))
                {
                    facet_words.push(word);
                }
            }
            if !facet_words.is_empty() && !facets.iter().any(|facet| facet.words == facet_words) {
                facets.push(TaskFacet {
                    clause,
                    words: facet_words,
                });
            }
        }

        // Longer compounds first: `file too large` names `FileTooLarge` and
        // `file_too_large` more precisely than either adjacent pair.
        let mut compounds = Vec::<TaskCompound>::new();
        for size in [3, 2] {
            let mut taken = 0;
            for window in words.windows(size) {
                if taken == PLAN_CONTEXT_COMPOUND_LIMIT {
                    break;
                }
                if window
                    .iter()
                    .any(|word| is_compound_break_word(word) || word.contains('_'))
                {
                    continue;
                }
                let joined = window.concat();
                if joined.chars().count() < 6
                    || compounds.iter().any(|compound| compound.joined == joined)
                {
                    continue;
                }
                taken += 1;
                compounds.push(TaskCompound {
                    queries: vec![joined.clone(), window.join("_")],
                    joined,
                    leading: window[0].clone(),
                    words: size,
                    name_hits: 0,
                    specificity: 1.0,
                    use_site: false,
                });
            }
        }
        Self {
            compounds,
            salient,
            facets,
            content_words: Vec::new(),
            numbers: task_numbers(raw_task),
            identifiers: task_identifiers(raw_task)
                .into_iter()
                .filter_map(|identifier| match identifier {
                    TaskIdentifier::Symbol(name) => Some(name),
                    TaskIdentifier::Path(_) => None,
                })
                .take(PLAN_CONTEXT_USE_SITE_IDENTIFIERS)
                .collect(),
            wants_tests,
        }
    }

    /// Score added for what a symbol body contains: compounds weigh most,
    /// each distinct salient word a little, capped so prose cannot dominate.
    fn body_score(&self, content: &str) -> f64 {
        let lowercase = content.to_lowercase();
        let squeezed = squeeze_separators(&lowercase);
        let compound_score = self
            .compounds
            .iter()
            .filter(|compound| squeezed.contains(&compound.joined))
            .map(|compound| compound.weight() * 0.75)
            .sum::<f64>();
        let salient_matches = self
            .salient
            .iter()
            .filter(|term| text_mentions(&lowercase, term))
            .count();
        compound_score + (salient_matches.min(6) as f64) * 0.5
    }

    /// Whether `content` spells a use-site concept in any identifier case.
    fn mentions_use_site(&self, content: &str) -> bool {
        if !self.compounds.iter().any(|compound| compound.use_site) {
            return false;
        }
        let squeezed = squeeze_separators(&content.to_lowercase());
        self.compounds
            .iter()
            .any(|compound| compound.use_site && squeezed.contains(&compound.joined))
    }

    /// Index of the first body line containing a compound concept, or else
    /// a word only the content scan found.
    fn focus_line(&self, content: &str) -> Option<usize> {
        let compound_line = content.split_inclusive('\n').position(|line| {
            let squeezed = squeeze_separators(&line.to_lowercase());
            self.compounds
                .iter()
                .any(|compound| compound.specificity >= 1.0 && squeezed.contains(&compound.joined))
        });
        compound_line.or_else(|| {
            content.split_inclusive('\n').position(|line| {
                self.content_words
                    .iter()
                    .any(|word| line_spells_word(line, word))
            })
        })
    }

    /// Salient words that no included evidence names or contains.
    fn uncovered<'a>(
        &self,
        bodies: impl IntoIterator<Item = &'a ContextSymbolBody>,
    ) -> Vec<String> {
        let haystacks = bodies.into_iter().map(evidence_text).collect::<Vec<_>>();
        self.salient
            .iter()
            .filter(|term| !haystacks.iter().any(|text| text_mentions(text, term)))
            .cloned()
            .collect()
    }
}

/// Lowercase text in which an evidence item can mention a task word: its
/// path, qualified name, signature, and excerpt.
fn evidence_text(body: &ContextSymbolBody) -> String {
    format!(
        "{}\n{}\n{}\n{}",
        body.path,
        body.symbol.qualified_name,
        body.symbol.signature.as_deref().unwrap_or_default(),
        body.content
    )
    .to_lowercase()
}

/// Whether lowercase `text` contains `term` or its singular form.
fn text_mentions(text: &str, term: &str) -> bool {
    text.contains(term)
        || singular_context_term(term).is_some_and(|singular| text.contains(singular.as_str()))
}

fn squeeze_separators(lowercase: &str) -> String {
    lowercase
        .chars()
        .filter(|character| !character.is_whitespace() && !matches!(character, '_' | '-'))
        .collect()
}

/// Words that end a compound: stop words plus function words that never sit
/// inside an identifier-shaped concept.
fn is_compound_break_word(word: &str) -> bool {
    is_context_stop_word(word)
        || matches!(
            word,
            "a" | "an"
                | "as"
                | "at"
                | "be"
                | "by"
                | "do"
                | "does"
                | "each"
                | "every"
                | "get"
                | "gets"
                | "has"
                | "have"
                | "in"
                | "is"
                | "it"
                | "its"
                | "no"
                | "not"
                | "on"
                | "or"
                | "to"
                | "was"
                | "when"
                | "which"
                | "why"
        )
}

/// Question scaffolding that names no repository concept, so evidence is not
/// expected to mention it.
fn is_generic_task_word(word: &str) -> bool {
    matches!(
        word,
        "also"
            | "been"
            | "before"
            | "after"
            | "cite"
            | "decide"
            | "decides"
            | "defined"
            | "definition"
            | "does"
            | "each"
            | "every"
            | "explain"
            | "function"
            | "functions"
            | "get"
            | "gets"
            | "give"
            | "happen"
            | "happens"
            | "has"
            | "have"
            | "its"
            | "key"
            | "list"
            | "name"
            | "names"
            | "not"
            | "path"
            | "paths"
            | "source"
            | "string"
            | "type"
            | "types"
            | "when"
            | "which"
            | "without"
    )
}

fn is_test_hit(hit: &SearchHit) -> bool {
    is_test_symbol(&hit.path, &hit.symbol)
}

fn is_test_symbol(path: &str, symbol: &SymbolRecord) -> bool {
    let path = path.to_ascii_lowercase();
    let in_test_directory = path
        .split('/')
        .any(|part| matches!(part, "test" | "tests" | "__tests__" | "testdata"));
    let file_stem = path
        .rsplit('/')
        .next()
        .and_then(|name| name.split('.').next())
        .unwrap_or_default();
    let test_file = file_stem.starts_with("test_")
        || file_stem.ends_with("_test")
        || path.contains(".test.")
        || path.contains(".spec.");
    let name = symbol.qualified_name.to_ascii_lowercase();
    in_test_directory
        || test_file
        || symbol.name.to_ascii_lowercase().starts_with("test_")
        || name.starts_with("tests::")
        || name.contains("::tests::")
}

fn context_query_hits(
    store: &Store,
    repo: RepoId,
    search_queries: &[String],
    exclude_text: bool,
) -> Result<(Vec<Vec<SearchHit>>, bool), AtlasError> {
    let mut query_hits = Vec::with_capacity(search_queries.len());
    let mut source_truncated = false;
    for query in search_queries {
        let mut hits = if exclude_text {
            search_excluding_language(
                store,
                repo,
                query,
                PLAN_CONTEXT_SCORING_WINDOW,
                FileLanguage::Text,
            )?
        } else {
            search_literal(store, repo, query, PLAN_CONTEXT_SCORING_WINDOW)?
        };
        source_truncated |= hits.len() > PLAN_CONTEXT_HITS_PER_QUERY;
        // Order only the leading hits; the rest keep their match rank.
        let leading = hits.len().min(PLAN_CONTEXT_HITS_PER_QUERY);
        hits[..leading].sort_by(|left, right| {
            context_language_order(left.language)
                .cmp(&context_language_order(right.language))
                .then_with(|| left.symbol.kind.cmp(&right.symbol.kind))
                .then_with(|| left.score.total_cmp(&right.score))
                .then_with(|| left.path.cmp(&right.path))
                .then_with(|| left.symbol.qualified_name.cmp(&right.symbol.qualified_name))
        });
        query_hits.push(hits);
    }
    Ok((query_hits, source_truncated))
}

fn task_requests_text_evidence(task: &str) -> bool {
    task.split(|character: char| !character.is_alphanumeric())
        .map(str::to_ascii_lowercase)
        .any(|word| {
            matches!(
                word.as_str(),
                "adr"
                    | "design"
                    | "doc"
                    | "docs"
                    | "documentation"
                    | "guide"
                    | "markdown"
                    | "readme"
                    | "spec"
                    | "specification"
            )
        })
}

const fn context_language_order(language: Option<FileLanguage>) -> u8 {
    match language {
        Some(FileLanguage::Shell) => 0,
        Some(
            FileLanguage::Rust
            | FileLanguage::TypeScript
            | FileLanguage::Tsx
            | FileLanguage::JavaScript
            | FileLanguage::Python,
        ) => 1,
        Some(FileLanguage::Text) => 2,
        None => 3,
    }
}

fn task_search_queries(task: &str) -> Vec<String> {
    let words = task
        .split(|character: char| !character.is_alphanumeric() && character != '_')
        .filter(|word| !word.is_empty())
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>();
    let security_context = words.iter().any(|word| is_security_context_term(word));
    let broad_auth_context = words
        .iter()
        .any(|word| matches!(word.as_str(), "auth" | "authentication" | "authorization"))
        || words
            .iter()
            .filter(|word| is_security_context_term(word))
            .count()
            >= 3;
    let mut queries = Vec::new();
    let mut seen = BTreeSet::new();

    if broad_auth_context {
        for query in [
            "login",
            "oauth",
            "scopes",
            "credential",
            "update_env",
            "cookie",
            "access_token",
            "session",
            "password",
            "refresh_token",
            "auth",
            "api_key",
            "middleware",
            "guard",
            "logout",
            "signup",
            "identity",
            "bearer",
        ] {
            push_context_query(&mut queries, &mut seen, query.to_owned());
        }
    }

    // Preserve the original phrase for concise symbol-shaped tasks while
    // avoiding a long literal FTS query for prose requests.
    if words.len() <= 5 {
        push_context_query(&mut queries, &mut seen, task.trim().to_owned());
    }
    // Words are taken from each clause in turn, so the query limit cannot
    // starve the task's last clause (D95). Question scaffolding comes last.
    for word in clause_interleaved_words(task) {
        if security_context && matches!(word.as_str(), "token" | "tokens") {
            for compound in [
                "access_token",
                "oauth_token",
                "refresh_token",
                "bearer_token",
            ] {
                push_context_query(&mut queries, &mut seen, compound.to_owned());
            }
            continue;
        }
        if matches!(word.as_str(), "authentication" | "authorization") {
            for root in ["auth", "authenticate", "authenticated", "authorize"] {
                push_context_query(&mut queries, &mut seen, root.to_owned());
            }
        }
        push_context_query(&mut queries, &mut seen, word.clone());
        if let Some(singular) = singular_context_term(&word) {
            push_context_query(&mut queries, &mut seen, singular);
        }
        if queries.len() == PLAN_CONTEXT_QUERY_LIMIT {
            break;
        }
    }
    if queries.is_empty() {
        push_context_query(&mut queries, &mut seen, task.trim().to_owned());
    }
    queries
}

/// The task's searchable lowercase words: its salient words one from each
/// clause in turn (the first of every clause, then the second, and so on),
/// then its question scaffolding in task order. A one-clause task keeps the
/// order of its salient words.
fn clause_interleaved_words(task: &str) -> Vec<String> {
    let words_of = |clause: &str| {
        clause
            .split(|character: char| !character.is_alphanumeric() && character != '_')
            .map(str::to_ascii_lowercase)
            .filter(|word| word.chars().count() >= 3 && !is_context_stop_word(word))
            .collect::<Vec<_>>()
    };
    let clauses = task_clauses(task)
        .iter()
        .map(|clause| {
            words_of(clause)
                .into_iter()
                .filter(|word| !is_generic_task_word(word))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let longest = clauses.iter().map(Vec::len).max().unwrap_or(0);
    (0..longest)
        .flat_map(|rank| clauses.iter().filter_map(move |words| words.get(rank)))
        .cloned()
        .chain(
            words_of(task)
                .into_iter()
                .filter(|word| is_generic_task_word(word)),
        )
        .collect()
}

fn is_security_context_term(word: &str) -> bool {
    matches!(
        word,
        "auth"
            | "authentication"
            | "authorization"
            | "cookie"
            | "cookies"
            | "credential"
            | "credentials"
            | "login"
            | "oauth"
            | "password"
            | "passwords"
            | "session"
            | "sessions"
    )
}

fn push_context_query(queries: &mut Vec<String>, seen: &mut BTreeSet<String>, query: String) {
    if queries.len() < PLAN_CONTEXT_QUERY_LIMIT && !query.is_empty() && seen.insert(query.clone()) {
        queries.push(query);
    }
}

fn singular_context_term(word: &str) -> Option<String> {
    if word.len() > 4 && word.ends_with('s') && !word.ends_with("ss") {
        return Some(word[..word.len() - 1].to_owned());
    }
    None
}

fn is_context_stop_word(word: &str) -> bool {
    matches!(
        word,
        "about"
            | "all"
            | "and"
            | "any"
            | "are"
            | "code"
            | "behavior"
            | "behaviour"
            | "concise"
            | "concisely"
            | "configuration"
            | "entry"
            | "entries"
            | "environment"
            | "exact"
            | "find"
            | "flow"
            | "flows"
            | "for"
            | "from"
            | "how"
            | "handling"
            | "implementation"
            | "implementations"
            | "include"
            | "including"
            | "into"
            | "locate"
            | "most"
            | "of"
            | "point"
            | "points"
            | "protection"
            | "provider"
            | "providers"
            | "range"
            | "ranges"
            | "related"
            | "relevant"
            | "repository"
            | "show"
            | "summarize"
            | "summary"
            | "that"
            | "the"
            | "their"
            | "this"
            | "through"
            | "user"
            | "use"
            | "using"
            | "variable"
            | "variables"
            | "what"
            | "where"
            | "with"
            | "within"
    )
}

fn matching_context_outlines(
    store: &Store,
    repo: RepoId,
    hits: &[SearchHit],
) -> Result<Vec<Outline>, AtlasError> {
    let mut outlines = Vec::<Outline>::new();
    for hit in hits {
        let outline_index = outlines.iter().position(|outline| outline.path == hit.path);
        let index = match outline_index {
            Some(index) => index,
            None => {
                let stored = outline(store, repo, &hit.path)?;
                outlines.push(Outline {
                    path: stored.path,
                    language: stored.language,
                    symbols: Vec::new(),
                });
                outlines.len() - 1
            }
        };
        let symbols = &mut outlines[index].symbols;
        if symbols.len() < PLAN_CONTEXT_SYMBOLS_PER_FILE
            && !symbols
                .iter()
                .any(|symbol| same_symbol(symbol, &hit.symbol))
        {
            symbols.push(hit.symbol.clone());
        }
    }
    Ok(outlines)
}

fn same_symbol(left: &SymbolRecord, right: &SymbolRecord) -> bool {
    left.qualified_name == right.qualified_name
        && left.kind == right.kind
        && left.start_line == right.start_line
        && left.end_line == right.end_line
}

fn focused_context_map(
    hits: &[SearchHit],
    budget_tokens: usize,
    counter: &dyn TokenCounter,
) -> RepoMap {
    let mut first_symbols = Vec::<(&str, &str)>::new();
    let mut seen_paths = BTreeSet::new();
    for hit in hits {
        if seen_paths.insert(hit.path.as_str()) {
            first_symbols.push((&hit.path, &hit.symbol.qualified_name));
        }
    }
    let files_total = first_symbols.len();
    let mut rendered = String::new();
    let mut files_shown = 0;
    for (path, symbol) in first_symbols {
        let candidate = format!("{rendered}{path} — {symbol}\n");
        if counter.count(&candidate) > budget_tokens {
            continue;
        }
        rendered = candidate;
        files_shown += 1;
    }
    RepoMap {
        tokens_used: counter.count(&rendered),
        rendered,
        files_total,
        files_shown,
        truncated: files_shown < files_total,
        token_provider: counter.provider(),
    }
}

fn render_context_item(current: &str, header: &str, item: &str, first_in_section: bool) -> String {
    let mut rendered = current.to_owned();
    if first_in_section {
        if !rendered.is_empty() && !rendered.ends_with("\n\n") {
            rendered.push('\n');
        }
        rendered.push_str(header);
    }
    rendered.push_str(item);
    rendered
}

fn compact_context_evidence(
    body: &ContextSymbolBody,
    content_budget: usize,
    counter: &dyn TokenCounter,
    concepts: &TaskConcepts,
) -> ContextSymbolBody {
    let fits_whole = counter.count(&body.content) <= content_budget;
    // A body too long for its allowance is excerpted from just above the
    // first line that contains a compound task concept, not from its head.
    let focus_line = if fits_whole {
        0
    } else {
        concepts
            .focus_line(&body.content)
            .map_or(0, |line| line.saturating_sub(1))
    };
    excerpt_context_evidence(body, focus_line, content_budget, counter)
}

/// The leading lines of `body` that fit `content_budget`.
fn prefix_context_evidence(
    body: &ContextSymbolBody,
    content_budget: usize,
    counter: &dyn TokenCounter,
) -> ContextSymbolBody {
    excerpt_context_evidence(body, 0, content_budget, counter)
}

/// Whole lines of `body` from `first_line` on that fit `content_budget`.
/// When the body ends before the allowance does, the excerpt grows upward
/// with the lines above `first_line`, so a match near the end of a long body
/// still arrives with its context.
fn excerpt_context_evidence(
    body: &ContextSymbolBody,
    mut first_line: usize,
    content_budget: usize,
    counter: &dyn TokenCounter,
) -> ContextSymbolBody {
    let lines = body.content.split_inclusive('\n').collect::<Vec<_>>();
    let mut content = String::new();
    let mut reached_end = true;
    for line in lines.iter().skip(first_line) {
        let candidate = format!("{content}{line}");
        if counter.count(&candidate) > content_budget {
            reached_end = false;
            break;
        }
        content = candidate;
    }
    while reached_end && !content.is_empty() && first_line > 0 {
        let candidate = format!("{}{content}", lines[first_line - 1]);
        if counter.count(&candidate) > content_budget {
            break;
        }
        content = candidate;
        first_line -= 1;
    }
    if content.is_empty() && !body.content.is_empty() {
        first_line = 0;
        content = prefix_within_budget(&body.content, content_budget, counter);
    }
    let excerpt_start_line = body
        .symbol
        .start_line
        .saturating_add(u32::try_from(first_line).unwrap_or(u32::MAX))
        .min(body.symbol.end_line.max(body.symbol.start_line));
    let excerpt_lines = line_count(&content).max(u32::from(!content.is_empty()));
    let excerpt_end_line = if excerpt_lines == 0 {
        excerpt_start_line
    } else {
        excerpt_start_line
            .saturating_add(excerpt_lines - 1)
            .min(body.symbol.end_line.max(excerpt_start_line))
    };
    ContextSymbolBody {
        path: body.path.clone(),
        symbol: body.symbol.clone(),
        truncated: content != body.content,
        content,
        excerpt_start_line,
        excerpt_end_line,
    }
}

/// One evidence item. `referenced` marks a constant included because the
/// excerpt above it uses the name.
fn render_context_evidence(body: &ContextSymbolBody, referenced: bool) -> String {
    let mut rendered = format!(
        "## {}:{}-{} — {} ({:?}",
        body.path,
        body.excerpt_start_line,
        body.excerpt_end_line,
        body.symbol.qualified_name,
        body.symbol.kind,
    );
    if referenced {
        rendered.push_str(", referenced above");
    }
    if body.truncated {
        let _ = write!(
            rendered,
            ", excerpt of lines {}-{}",
            body.symbol.start_line, body.symbol.end_line
        );
    }
    rendered.push_str(")\n");
    rendered.push_str(&body.content);
    if !rendered.ends_with('\n') {
        rendered.push('\n');
    }
    rendered
}

fn not_found(selector: &SymbolSelector, stale: bool) -> UnfoldResult {
    UnfoldResult::NotFound {
        path: selector.path.clone(),
        qualified_name: selector.qualified_name.clone(),
        stale,
        reindex_hint: REINDEX_HINT.to_owned(),
    }
}

fn matching_symbols(symbols: Vec<SymbolRecord>, selector: &SymbolSelector) -> Vec<SymbolRecord> {
    let expected_hash = selector
        .signature_hash
        .as_deref()
        .map(str::to_ascii_lowercase);
    symbols
        .into_iter()
        .filter(|symbol| symbol.qualified_name == selector.qualified_name)
        .filter(|symbol| selector.kind.is_none_or(|kind| symbol.kind == kind))
        .filter(|symbol| {
            expected_hash.as_ref().is_none_or(|expected| {
                symbol.signature.as_deref().is_some_and(|signature| {
                    blake3::hash(signature.as_bytes()).to_hex().as_str() == expected
                })
            })
        })
        .collect()
}

fn sort_selector_candidates(symbols: &mut [SymbolRecord], hint: Option<u32>) {
    symbols.sort_by(|left, right| {
        hint.map_or(std::cmp::Ordering::Equal, |hint| {
            left.start_line
                .abs_diff(hint)
                .cmp(&right.start_line.abs_diff(hint))
        })
        .then_with(|| left.start_line.cmp(&right.start_line))
        .then_with(|| left.end_line.cmp(&right.end_line))
        .then_with(|| left.kind.cmp(&right.kind))
        .then_with(|| left.signature.cmp(&right.signature))
        .then_with(|| left.qualified_name.cmp(&right.qualified_name))
    });
}

fn symbol_identity(path: &str, symbol: &SymbolRecord) -> SymbolIdentity {
    SymbolIdentity {
        path: path.to_owned(),
        qualified_name: symbol.qualified_name.clone(),
        kind: symbol.kind,
        signature_hash: symbol
            .signature
            .as_deref()
            .map(|signature| blake3::hash(signature.as_bytes()).to_hex().to_string()),
        start_line: symbol.start_line,
        end_line: symbol.end_line,
    }
}

pub(crate) enum ResolvedEntry {
    File(ResolvedFile),
    Missing,
}

/// A file [`resolve_under_root`] approved, opened only through
/// [`ResolvedFile::open`].
///
/// On Unix it carries the device and inode the gate inspected, and `open`
/// refuses a handle whose identity differs: a component swapped for a
/// symbolic link (or any other file) between the check and the open is
/// never read, whichever directory level was swapped. Elsewhere `open`
/// reopens the checked path, so the check-then-open window remains there.
#[derive(Debug)]
pub(crate) struct ResolvedFile {
    path: PathBuf,
    #[cfg(unix)]
    identity: (u64, u64),
}

impl ResolvedFile {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Opens the approved file, failing as not found if what the path now
    /// names is not the file the gate approved.
    pub(crate) fn open(&self) -> io::Result<fs::File> {
        let file = fs::File::open(&self.path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            let metadata = file.metadata()?;
            if (metadata.dev(), metadata.ino()) != self.identity {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    "the file changed after it was resolved",
                ));
            }
        }
        Ok(file)
    }
}

pub(crate) fn canonical_query_root(root: &Path) -> Result<PathBuf, AtlasError> {
    let canonical = fs::canonicalize(root).map_err(|source| QueryError::CanonicalizeRoot {
        path: root.to_path_buf(),
        source,
    })?;
    if canonical != root {
        return Err(AtlasError::OutsideRoot {
            path: canonical,
            root: root.to_path_buf(),
        });
    }
    if !canonical.is_dir() {
        return Err(QueryError::RootNotDirectory { path: canonical }.into());
    }
    Ok(canonical)
}

/// Resolves a repository-relative path for reading, the one gate every
/// repository file read passes (read, grep, unfold, outline staleness,
/// context-pack content scan, module-card purpose lines).
///
/// The symlink rule: a file is served only when the requested path names it
/// directly. The target must lie inside `root` (otherwise
/// [`AtlasError::OutsideRoot`]); then, if the requested path or the resolved
/// target is a secret path, or any component of the requested path below
/// `root` is a symbolic link, the entry is reported as
/// [`ResolvedEntry::Missing`], exactly like a secret path, so a link reveals
/// nothing about its target. The scanner never indexes or follows a link, so
/// an indexed path that is now a link was swapped after indexing, and a link
/// that was never indexed would serve content the index never vetted.
pub(crate) fn resolve_under_root(
    root: &Path,
    requested: &str,
) -> Result<ResolvedEntry, AtlasError> {
    if is_secret_path(Path::new(requested)) {
        return Ok(ResolvedEntry::Missing);
    }
    let joined = root.join(requested);
    match fs::canonicalize(&joined) {
        Ok(resolved) => {
            ensure_contained(root, &resolved)?;
            let target = resolved.strip_prefix(root).unwrap_or(&resolved);
            if is_secret_path(target) {
                return Ok(ResolvedEntry::Missing);
            }
            let Some(leaf) = direct_leaf(root, Path::new(requested)) else {
                return Ok(ResolvedEntry::Missing);
            };
            if !resolved.is_file() {
                return Err(QueryError::NotAFile { path: resolved }.into());
            }
            approved_file(resolved, &leaf)
        }
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            ensure_missing_path_contained(root, &joined)?;
            Ok(ResolvedEntry::Missing)
        }
        Err(source) => Err(QueryError::ResolvePath {
            path: joined,
            source,
        }
        .into()),
    }
}

/// The canonical file, tied to the inode the link-free walk reached. If the
/// canonical path and the walked path no longer name the same file (a swap
/// between the two checks), nothing is served.
fn approved_file(resolved: PathBuf, leaf: &fs::Metadata) -> Result<ResolvedEntry, AtlasError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let Ok(canonical) = fs::metadata(&resolved) else {
            return Ok(ResolvedEntry::Missing);
        };
        let identity = (leaf.dev(), leaf.ino());
        if (canonical.dev(), canonical.ino()) != identity {
            return Ok(ResolvedEntry::Missing);
        }
        Ok(ResolvedEntry::File(ResolvedFile {
            path: resolved,
            identity,
        }))
    }
    #[cfg(not(unix))]
    {
        let _ = leaf;
        Ok(ResolvedEntry::File(ResolvedFile { path: resolved }))
    }
}

/// The metadata of the file `requested` names, read without following
/// links, when no component of `requested` below `root` is a symbolic link;
/// `None` otherwise. A path that names no component below `root` lexically
/// (an absolute path elsewhere, or one that climbs above `root` with `..`)
/// counts as traversing a link: it can only reach the root through another
/// name for it.
fn direct_leaf(root: &Path, requested: &Path) -> Option<fs::Metadata> {
    let relative = if requested.is_absolute() {
        requested.strip_prefix(root).ok()?
    } else {
        requested
    };
    let mut current = root.to_path_buf();
    let mut depth = 0_usize;
    let mut leaf = None;
    for component in relative.components() {
        match component {
            std::path::Component::Normal(part) => {
                current.push(part);
                depth += 1;
                // A component that cannot be inspected (it changed since
                // canonicalization) is refused like a link.
                let metadata = fs::symlink_metadata(&current).ok()?;
                if metadata.file_type().is_symlink() {
                    return None;
                }
                leaf = Some(metadata);
            }
            std::path::Component::ParentDir => {
                if depth == 0 {
                    return None;
                }
                current.pop();
                depth -= 1;
                leaf = None;
            }
            std::path::Component::CurDir => {}
            std::path::Component::RootDir | std::path::Component::Prefix(_) => return None,
        }
    }
    // After `a/..` the leaf is a directory again; re-read it link-free.
    match leaf {
        Some(leaf) => Some(leaf),
        None => fs::symlink_metadata(&current)
            .ok()
            .filter(|metadata| !metadata.file_type().is_symlink()),
    }
}

fn ensure_missing_path_contained(root: &Path, missing: &Path) -> Result<(), AtlasError> {
    let mut ancestor = missing.parent();
    while let Some(path) = ancestor {
        match fs::canonicalize(path) {
            Ok(resolved) => return ensure_contained(root, &resolved),
            Err(source) if source.kind() == io::ErrorKind::NotFound => {
                ancestor = path.parent();
            }
            Err(source) => {
                return Err(QueryError::ResolvePath {
                    path: path.to_path_buf(),
                    source,
                }
                .into());
            }
        }
    }
    Err(QueryError::NotFound {
        path: missing.to_path_buf(),
    }
    .into())
}

fn ensure_contained(root: &Path, resolved: &Path) -> Result<(), AtlasError> {
    if !resolved.starts_with(root) {
        return Err(AtlasError::OutsideRoot {
            path: resolved.to_path_buf(),
            root: root.to_path_buf(),
        });
    }
    Ok(())
}

/// Chunk size for staleness hashing; the only buffer that check allocates.
const STALENESS_HASH_BUFFER_BYTES: usize = 64 * 1024;

/// How long before the index a modification must be for the file to be
/// trusted without hashing. Covers file systems that record modification
/// times to two seconds.
const STALENESS_MTIME_MARGIN_SECONDS: i64 = 2;

/// What a file's metadata says about it relative to its index record.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Freshness {
    /// The size differs: stale without reading.
    Stale,
    /// Same size and not modified since before it was indexed.
    Unchanged,
    /// Same size, but touched at or after indexing: only the content tells.
    NeedsHash,
}

/// Decides from metadata alone whether a file must be hashed.
///
/// The index stamps `indexed_at` before it reads any file, so every edit
/// made after a file was read has a modification time at or after the stamp
/// and is hashed. The one thing this cannot see is an edit that keeps the
/// size and also sets the modification time back to before the index.
fn freshness_from_metadata(
    size: u64,
    modified_seconds: Option<i64>,
    indexed_size: u64,
    indexed_at: i64,
) -> Freshness {
    if size != indexed_size {
        return Freshness::Stale;
    }
    match modified_seconds {
        Some(modified) if modified.saturating_add(STALENESS_MTIME_MARGIN_SECONDS) <= indexed_at => {
            Freshness::Unchanged
        }
        _ => Freshness::NeedsHash,
    }
}

/// Reports whether the file at `path` still matches its index record.
///
/// A size that differs is stale without reading, a file untouched since
/// before it was indexed is trusted without reading, and anything else is
/// hashed through one fixed buffer. The file is never loaded whole.
pub(crate) fn file_matches_index(
    resolved: &ResolvedFile,
    indexed: &compresso_store::FileRecord,
) -> Result<bool, QueryError> {
    let read_error = |source| QueryError::Read {
        path: resolved.path().to_path_buf(),
        source,
    };
    let mut file = resolved.open().map_err(read_error)?;
    let metadata = file.metadata().map_err(read_error)?;
    let modified_seconds = metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
        .and_then(|elapsed| i64::try_from(elapsed.as_secs()).ok());
    match freshness_from_metadata(
        metadata.len(),
        modified_seconds,
        indexed.size_bytes,
        indexed.indexed_at,
    ) {
        Freshness::Stale => Ok(false),
        Freshness::Unchanged => Ok(true),
        Freshness::NeedsHash => {
            content_matches_index(&mut file, indexed.size_bytes, &indexed.hash).map_err(read_error)
        }
    }
}

/// Hashes `reader` in fixed chunks and compares it with the indexed size and
/// hash. Reading stops as soon as the content is longer than the index
/// recorded, so a file that grows during the check cannot extend it.
fn content_matches_index(
    reader: &mut impl io::Read,
    indexed_size: u64,
    indexed_hash: &str,
) -> io::Result<bool> {
    let mut hasher = blake3::Hasher::new();
    let mut buffer = vec![0_u8; STALENESS_HASH_BUFFER_BYTES];
    let mut total = 0_u64;
    loop {
        let read = match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        total = total.saturating_add(u64::try_from(read).unwrap_or(u64::MAX));
        if total > indexed_size {
            return Ok(false);
        }
        hasher.update(&buffer[..read]);
    }
    Ok(total == indexed_size && hasher.finalize().to_hex().as_str() == indexed_hash)
}

pub(crate) fn read_utf8(resolved: &ResolvedFile) -> Result<String, QueryError> {
    read_utf8_capped(resolved, MAX_READ_FILE_BYTES)
}

/// Reads at most `limit` bytes, failing closed when the file holds more.
///
/// The ceiling is enforced on the open handle rather than on metadata alone,
/// so a file that grows after a size check still cannot exceed it.
fn read_utf8_capped(resolved: &ResolvedFile, limit: u64) -> Result<String, QueryError> {
    use std::io::Read as _;

    let path = resolved.path();
    let read_error = |source| QueryError::Read {
        path: path.to_path_buf(),
        source,
    };
    let too_large = || QueryError::FileTooLarge {
        path: path.to_path_buf(),
        limit_bytes: limit,
    };
    let file = resolved.open().map_err(read_error)?;
    if file.metadata().map_err(read_error)?.len() > limit {
        return Err(too_large());
    }
    let mut bytes = Vec::new();
    file.take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(read_error)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > limit {
        return Err(too_large());
    }
    String::from_utf8(bytes)
        .map_err(|error| read_error(io::Error::new(io::ErrorKind::InvalidData, error)))
}

fn source_line_range<'a>(
    source: &'a str,
    path: &Path,
    start_line: u32,
    end_line: u32,
) -> Result<&'a str, QueryError> {
    let total_lines = line_count(source);
    if start_line == 0 || end_line < start_line || end_line > total_lines {
        return Err(QueryError::InvalidSymbolRange {
            path: path.to_path_buf(),
            start_line,
            end_line,
        });
    }
    let starts = line_starts(source);
    let start = starts[(start_line - 1) as usize];
    let end = starts
        .get(end_line as usize)
        .copied()
        .unwrap_or(source.len());
    Ok(&source[start..end])
}

fn normalize_range(
    start_line: u32,
    end_line: u32,
    total_lines: u32,
) -> Result<(u32, bool), QueryError> {
    if start_line == 0 || end_line < start_line || start_line > total_lines {
        return Err(QueryError::InvalidRange {
            start_line,
            end_line,
            total_lines,
        });
    }
    Ok((end_line.min(total_lines), end_line > total_lines))
}

fn line_starts(source: &str) -> Vec<usize> {
    let mut starts = vec![0];
    starts.extend(
        source
            .bytes()
            .enumerate()
            .filter_map(|(index, byte)| (byte == b'\n').then_some(index + 1)),
    );
    starts
}

fn line_count(source: &str) -> u32 {
    if source.is_empty() {
        return 0;
    }
    let newlines = source.bytes().filter(|byte| *byte == b'\n').count();
    let lines = newlines + usize::from(!source.ends_with('\n'));
    u32::try_from(lines).unwrap_or(u32::MAX)
}

fn prefix_within_budget(text: &str, budget: usize, counter: &dyn TokenCounter) -> String {
    if budget == 0 {
        return String::new();
    }
    let mut boundaries = text
        .char_indices()
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    boundaries.push(text.len());
    boundaries.sort_unstable();
    boundaries.dedup();
    let mut low = 0;
    let mut high = boundaries.len();
    while low < high {
        let middle = low + (high - low) / 2;
        if counter.count(&text[..boundaries[middle]]) <= budget {
            low = middle + 1;
        } else {
            high = middle;
        }
    }
    let mut index = low.saturating_sub(1);
    while counter.count(&text[..boundaries[index]]) > budget {
        if index == 0 {
            return String::new();
        }
        index -= 1;
    }
    text[..boundaries[index]].to_owned()
}

fn included_end_line(start_line: u32, content: &str) -> u32 {
    if content.is_empty() {
        return start_line;
    }
    let complete_newlines = content.bytes().filter(|byte| *byte == b'\n').count() as u32;
    start_line
        .saturating_add(complete_newlines)
        .saturating_sub(u32::from(content.ends_with('\n')))
}

#[derive(Default)]
struct DirectoryNode {
    directories: BTreeMap<String, DirectoryNode>,
    files: BTreeMap<String, Vec<String>>,
}

fn render_directory_tree(files: &BTreeMap<String, Vec<String>>) -> String {
    let mut root = DirectoryNode::default();
    for (path, symbols) in files {
        let mut components = path.split('/').peekable();
        let mut directory = &mut root;
        while let Some(component) = components.next() {
            if components.peek().is_none() {
                directory
                    .files
                    .insert(component.to_owned(), symbols.clone());
            } else {
                directory = directory
                    .directories
                    .entry(component.to_owned())
                    .or_default();
            }
        }
    }

    let mut rendered = String::new();
    render_directory(&root, 0, &mut rendered);
    rendered
}

fn render_directory(directory: &DirectoryNode, depth: usize, output: &mut String) {
    let indent = "  ".repeat(depth);
    for (name, symbols) in &directory.files {
        let _ = write!(output, "{indent}{name}");
        if !symbols.is_empty() {
            let _ = write!(output, "  — {}", symbols.join(", "));
        }
        output.push('\n');
    }
    for (name, child) in &directory.directories {
        let _ = writeln!(output, "{indent}{name}/");
        render_directory(child, depth + 1, output);
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    #[cfg(unix)]
    use std::io;
    use std::path::PathBuf;

    #[cfg(unix)]
    use super::read_utf8;
    use super::{
        QueryError, ResolvedEntry, ResolvedFile, STALENESS_HASH_BUFFER_BYTES,
        content_matches_index, line_spells_word, read_utf8_capped, resolve_under_root,
    };

    /// Serves `total` zero bytes and records how it was read.
    struct MeteredZeros {
        remaining: u64,
        served: u64,
        largest_request: usize,
    }

    impl MeteredZeros {
        fn new(total: u64) -> Self {
            Self {
                remaining: total,
                served: 0,
                largest_request: 0,
            }
        }
    }

    impl std::io::Read for MeteredZeros {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            self.largest_request = self.largest_request.max(buffer.len());
            let count = u64::try_from(buffer.len())
                .unwrap_or(u64::MAX)
                .min(self.remaining);
            let count_usize = usize::try_from(count).expect("count fits the buffer");
            buffer[..count_usize].fill(0);
            self.remaining -= count;
            self.served += count;
            Ok(count_usize)
        }
    }

    fn hash_of_zeros(length: usize) -> String {
        blake3::hash(&vec![0_u8; length]).to_hex().to_string()
    }

    #[test]
    fn test_only_a_file_older_than_the_index_with_the_indexed_size_skips_hashing() {
        use super::{Freshness, freshness_from_metadata};

        let indexed_at = 1_000_i64;
        let cases = [
            // (size on disk, modified, expected)
            (10, Some(900), Freshness::Unchanged),
            (10, Some(998), Freshness::Unchanged),
            // Within the clock-granularity margin, at, or after the index.
            (10, Some(999), Freshness::NeedsHash),
            (10, Some(1_000), Freshness::NeedsHash),
            (10, Some(5_000), Freshness::NeedsHash),
            // No readable modification time.
            (10, None, Freshness::NeedsHash),
            // A different size never needs hashing to be stale.
            (11, Some(900), Freshness::Stale),
            (9, None, Freshness::Stale),
        ];

        for (size, modified, expected) in cases {
            assert_eq!(
                freshness_from_metadata(size, modified, 10, indexed_at),
                expected,
                "size {size}, modified {modified:?}"
            );
        }
    }

    /// Counts like the exact tokenizer and records how much text it was
    /// asked to count, which is the work a read does.
    struct MeteredCounter {
        inner: &'static dyn compresso_core::TokenCounter,
        bytes_counted: std::sync::atomic::AtomicUsize,
    }

    impl MeteredCounter {
        fn new() -> Self {
            Self {
                inner: compresso_core::counter_for(compresso_core::TokenProvider::OpenAiO200k),
                bytes_counted: std::sync::atomic::AtomicUsize::new(0),
            }
        }

        fn bytes(&self) -> usize {
            self.bytes_counted
                .load(std::sync::atomic::Ordering::Relaxed)
        }
    }

    impl compresso_core::TokenCounter for MeteredCounter {
        fn provider(&self) -> compresso_core::TokenProvider {
            self.inner.provider()
        }

        fn count(&self, text: &str) -> usize {
            self.bytes_counted
                .fetch_add(text.len(), std::sync::atomic::Ordering::Relaxed);
            self.inner.count(text)
        }
    }

    fn numbered_lines(total_bytes: usize) -> String {
        let mut text = String::with_capacity(total_bytes + 64);
        let mut line = 0_u32;
        while text.len() < total_bytes {
            line += 1;
            text.push_str(&format!(
                "line {line}: handler finished request with status ok\n"
            ));
        }
        text
    }

    #[test]
    fn test_read_of_a_large_file_counts_about_what_it_returns_not_the_whole_file() {
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let root = fs::canonicalize(directory.path()).expect("root should canonicalize");
        let source = numbered_lines(4 * 1024 * 1024);
        fs::write(root.join("large.log"), &source).expect("file should be written");

        let budgeted = MeteredCounter::new();
        let read = super::read_bounded(&root, "large.log", None, 500, &budgeted)
            .expect("read should succeed");
        assert!(read.truncated);
        assert!(read.tokens_used <= 500 && read.tokens_used > 400);
        assert!(source.starts_with(&read.content));
        assert!(
            read.file_tokens_estimated,
            "a 4 MiB total is not counted exactly"
        );
        let elision = read
            .elision
            .as_deref()
            .expect("a truncated read explains itself");
        assert!(
            elision.contains("about ") && elision.ends_with("tokens over budget]"),
            "an estimated overage must be worded as approximate: {elision}"
        );
        assert!(
            budgeted.bytes() < source.len() / 8,
            "counted {} bytes to return {} bytes",
            budgeted.bytes(),
            read.content.len()
        );

        let ranged = MeteredCounter::new();
        let read = super::read_bounded(&root, "large.log", Some((10, 29)), 500, &ranged)
            .expect("range read should succeed");
        assert!(!read.truncated);
        assert_eq!((read.start_line, read.end_line), (10, 29));
        assert!(
            ranged.bytes() < 64 * 1024,
            "counted {} bytes for twenty lines",
            ranged.bytes()
        );
    }

    #[test]
    fn test_bounded_read_returns_the_same_prefix_as_counting_the_whole_selection() {
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let root = fs::canonicalize(directory.path()).expect("root should canonicalize");
        let source = numbered_lines(300 * 1024);
        fs::write(root.join("mid.log"), &source).expect("file should be written");
        let counter = compresso_core::counter_for(compresso_core::TokenProvider::OpenAiO200k);

        for budget in [1, 37, 500, 4_000] {
            let read = super::read_bounded(&root, "mid.log", None, budget, counter)
                .expect("read should succeed");
            let expected = super::prefix_within_budget(&source, budget, counter);

            assert_eq!(read.content, expected, "budget {budget}");
            assert!(read.truncated);
            // Up to the exact-count limit the total is the tokenizer's own.
            assert!(!read.file_tokens_estimated);
            assert_eq!(read.file_tokens_total, counter.count(&source));
        }
    }

    #[test]
    fn test_growing_window_stays_within_budget_and_close_to_counting_everything() {
        let counter = compresso_core::counter_for(compresso_core::TokenProvider::OpenAiO200k);
        let code = "fn handle(request: &Request) -> Result<Response, Error> {\n    let value = request.header(\"x-id\")?;\n    Ok(Response::new(value))\n}\n".repeat(700);
        let compact_json = format!(
            "[{}]",
            (0..6000)
                .map(|index| format!("{{\"id\":{index},\"ok\":true}}"))
                .collect::<Vec<_>>()
                .join(",")
        );
        let cjk =
            "圧縮は完了しました。次の手順を実行してください、設定を確認します。\n".repeat(900);
        let long_words = (0..4000)
            .map(|index| format!("{}{index} ", "supercalifragilistic".repeat(1 + index % 7)))
            .collect::<String>();
        let blank_heavy = "alpha\n\n\n\n   \t  beta\n\n".repeat(5000);
        let mixed = format!(
            "{}\n{}\n{}",
            "word ".repeat(9000),
            "x".repeat(30_000),
            "9".repeat(20_000)
        );
        let shapes = [
            ("code", code),
            ("compact json", compact_json),
            ("cjk prose", cjk),
            ("long words", long_words),
            ("blank heavy", blank_heavy),
            ("mixed", mixed),
        ];

        for (name, text) in &shapes {
            let whole = counter.count(text);
            for budget in [1, 2, 99, 1_000, 7_777, 20_000] {
                let (content, truncated) = super::fit_within_budget(text, budget, counter);
                let (expected, expected_truncated) = if whole <= budget {
                    (text.clone(), false)
                } else {
                    (super::prefix_within_budget(text, budget, counter), true)
                };

                assert_eq!(truncated, expected_truncated, "{name}, budget {budget}");
                // Token counts are not strictly monotonic in the length of a
                // prefix, so the two searches can settle a few bytes apart.
                // What a read promises is a prefix within the budget.
                assert!(text.starts_with(&content), "{name}, budget {budget}");
                assert!(counter.count(&content) <= budget, "{name}, budget {budget}");
                assert!(
                    content.len().abs_diff(expected.len()) <= 16,
                    "{name}, budget {budget}: {} bytes against {}",
                    content.len(),
                    expected.len()
                );
            }
        }
    }

    #[test]
    fn test_line_spells_word_matches_whole_identifier_words_only() {
        for line in [
            "// 15 is SIGTERM, which asks for a stop",
            "let sigterm_seen = true;",
            "onSigtermReceived();",
            "handle-sigterm",
        ] {
            assert!(line_spells_word(line, "sigterm"), "{line}");
        }
        assert!(line_spells_word("verify the checksums", "checksum"));
        assert!(line_spells_word("verifyChecksum(data)", "checksums"));
        assert!(line_spells_word(
            "let server = HTTPServer::new();",
            "server"
        ));
        assert!(!line_spells_word("if done { return none; }", "one"));
        assert!(!line_spells_word("let checksum = digest(data);", "check"));
        assert!(!line_spells_word("record_completion(now)", "completes"));
    }

    #[test]
    fn test_file_line_count_matches_a_full_read_without_counting_tokens() {
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let root = fs::canonicalize(directory.path()).expect("root should canonicalize");
        fs::write(root.join("lines.txt"), "one\ntwo\nthree").expect("file should be written");
        fs::write(root.join("empty.txt"), "").expect("file should be written");

        assert_eq!(
            super::file_line_count(&root, "lines.txt").expect("lines should count"),
            3
        );
        assert_eq!(
            super::file_line_count(&root, "empty.txt").expect("lines should count"),
            0
        );
    }

    #[test]
    fn test_staleness_hash_matches_unchanged_content_in_bounded_chunks() {
        let length = 3 * STALENESS_HASH_BUFFER_BYTES + 17;
        let mut reader = MeteredZeros::new(length as u64);

        let matches = content_matches_index(&mut reader, length as u64, &hash_of_zeros(length))
            .expect("content should hash");

        assert!(matches);
        assert!(reader.largest_request <= STALENESS_HASH_BUFFER_BYTES);
    }

    #[test]
    fn test_staleness_hash_detects_same_size_content_change() {
        let length = 4096;
        let mut reader = MeteredZeros::new(length);
        let other = blake3::hash(&vec![1_u8; 4096]).to_hex().to_string();

        let matches =
            content_matches_index(&mut reader, length, &other).expect("content should hash");

        assert!(!matches);
    }

    #[test]
    fn test_staleness_hash_stops_reading_a_file_that_grew_past_its_indexed_size() {
        let indexed = 1024_u64;
        // A file that grew to 1 TiB after it was indexed.
        let mut reader = MeteredZeros::new(1 << 40);

        let matches = content_matches_index(&mut reader, indexed, &hash_of_zeros(1024))
            .expect("content should hash");

        assert!(!matches, "a longer file no longer matches the index");
        assert!(
            reader.served <= indexed + STALENESS_HASH_BUFFER_BYTES as u64,
            "read {} bytes of a file indexed at {indexed}",
            reader.served
        );
    }

    /// Writes `name` under a fresh canonical root and resolves it through the gate.
    fn written_and_resolved(
        directory: &tempfile::TempDir,
        name: &str,
        content: &str,
    ) -> (PathBuf, ResolvedFile) {
        let root = fs::canonicalize(directory.path()).expect("root should canonicalize");
        fs::write(root.join(name), content).expect("file should be written");
        let Ok(ResolvedEntry::File(resolved)) = resolve_under_root(&root, name) else {
            panic!("{name} should resolve to a file");
        };
        (root, resolved)
    }

    #[cfg(unix)]
    #[test]
    fn test_a_file_swapped_for_a_symlink_after_resolution_is_never_opened() {
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let (root, resolved) = written_and_resolved(&directory, "notes.txt", "ordinary\n");
        fs::write(root.join(".env"), "TOKEN=swapped_secret\n").expect("secret file");
        fs::remove_file(root.join("notes.txt")).expect("remove");
        std::os::unix::fs::symlink(root.join(".env"), root.join("notes.txt")).expect("swap");

        assert_eq!(
            resolved
                .open()
                .expect_err("a swapped file must not open")
                .kind(),
            io::ErrorKind::NotFound
        );
        assert!(read_utf8(&resolved).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn test_a_parent_directory_swapped_for_a_symlink_after_resolution_is_never_opened() {
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let root = fs::canonicalize(directory.path()).expect("root should canonicalize");
        fs::create_dir_all(root.join("docs")).expect("docs");
        fs::create_dir_all(root.join("private")).expect("private");
        fs::write(root.join("docs/notes.txt"), "ordinary\n").expect("notes");
        fs::write(root.join("private/notes.txt"), "hidden\n").expect("hidden");
        let Ok(ResolvedEntry::File(resolved)) = resolve_under_root(&root, "docs/notes.txt") else {
            panic!("docs/notes.txt should resolve");
        };
        fs::rename(root.join("docs"), root.join("docs-parked")).expect("park");
        std::os::unix::fs::symlink(root.join("private"), root.join("docs")).expect("swap");

        assert!(resolved.open().is_err());
    }

    #[cfg(unix)]
    #[test]
    fn test_hard_linked_ordinary_files_are_served() {
        // Hard links are not refused: shared source trees use them, and a
        // link to a secret can only be made by someone who can already read
        // it (see the symlink rule in docs/plan/04-mcp-and-integrations.md).
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let (root, _) = written_and_resolved(&directory, "a.txt", "shared\n");
        fs::hard_link(root.join("a.txt"), root.join("b.txt")).expect("hard link");
        let Ok(ResolvedEntry::File(resolved)) = resolve_under_root(&root, "b.txt") else {
            panic!("a hard-linked file should resolve");
        };
        assert_eq!(read_utf8(&resolved).expect("read"), "shared\n");
    }

    #[test]
    fn test_read_utf8_capped_accepts_file_at_limit() {
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let (_, path) = written_and_resolved(&directory, "at-limit.txt", "0123456789");

        let text = read_utf8_capped(&path, 10).expect("file at the limit should read");

        assert_eq!(text, "0123456789");
    }

    #[test]
    fn test_read_utf8_capped_rejects_one_byte_over_limit() {
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let (_, path) = written_and_resolved(&directory, "over-limit.txt", "0123456789A");

        let error = read_utf8_capped(&path, 10).expect_err("oversized file should fail");

        assert!(matches!(
            error,
            QueryError::FileTooLarge {
                limit_bytes: 10,
                ..
            }
        ));
    }
}
