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
/// Hits fetched per query for cross-query scoring; only the first
/// [`PLAN_CONTEXT_HITS_PER_QUERY`] count toward `matches_total`.
const PLAN_CONTEXT_SCORING_WINDOW: usize = 24;
/// Scored hits whose bodies are read to rank by what the code contains.
const PLAN_CONTEXT_RERANK_POOL: usize = 40;
/// Compounds kept per window size (three-word, then two-word).
const PLAN_CONTEXT_COMPOUND_LIMIT: usize = 12;
const PLAN_CONTEXT_UNCOVERED_LIMIT: usize = 8;
/// A hit scoring at least this much matched a compound concept by name or body.
const PLAN_CONTEXT_STRONG_SCORE: f64 = 4.0;
/// Generic-word hits scoring below this share of the best hit are dropped as noise.
const PLAN_CONTEXT_SCORE_FLOOR_RATIO: f64 = 0.25;
/// A hit scoring at least this much matched a specific concept and is kept
/// whatever the best hit scored; only single generic-word matches are noise.
const PLAN_CONTEXT_SPECIFIC_MATCH_SCORE: f64 = 2.0;
const PLAN_CONTEXT_TEST_PENALTY: f64 = 0.4;
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
    /// Modeled raw matched-evidence token baseline before excerpt packing.
    pub estimated_raw_source_tokens: usize,
    pub tokens_used: usize,
    pub token_provider: TokenProvider,
    pub truncated: bool,
}

impl ContextPlan {
    /// Whether the included evidence mentions at least half of the task's
    /// salient words. A pack that matched only generic words fails this even
    /// when every candidate file fitted the budget.
    #[must_use]
    pub const fn concept_coverage_sufficient(&self) -> bool {
        self.task_concepts_total == 0
            || self.task_concepts_covered.saturating_mul(2) >= self.task_concepts_total
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
                path: path.clone(),
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
            let content = source_line_range(&source, &path, symbol.start_line, symbol.end_line)?;
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
                source_line_range(&source, &resolved, start_line, selected_end_line)?,
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
    let counter = counter_for(TokenProvider::OpenAiO200k);
    let mut concepts = TaskConcepts::parse(task);
    let (search_queries, pool, matches_total, search_truncated) =
        search_task_concepts(store, repo, task, &mut concepts)?;

    // Read each pooled symbol once: the body both ranks the hit by what the
    // code contains and becomes its evidence excerpt.
    let mut scored = Vec::with_capacity(pool.len());
    for ranked in pool {
        let selector = SymbolSelector {
            path: ranked.hit.path.clone(),
            qualified_name: ranked.hit.symbol.qualified_name.clone(),
            kind: Some(ranked.hit.symbol.kind),
            signature_hash: ranked
                .hit
                .symbol
                .signature
                .as_deref()
                .map(|signature| blake3::hash(signature.as_bytes()).to_hex().to_string()),
            start_line_hint: Some(ranked.hit.symbol.start_line),
        };
        // A file that grew past the read ceiling since indexing loses its
        // body excerpt; the rest of the evidence pack is still served.
        let unfolded = match unfold(store, repo, &selector) {
            Err(AtlasError::Query(QueryError::FileTooLarge { .. })) => None,
            other => Some(other?),
        };
        let body = match unfolded {
            Some(UnfoldResult::Found {
                path,
                symbol,
                content,
                ..
            }) => Some(ContextSymbolBody {
                path,
                excerpt_start_line: symbol.start_line,
                excerpt_end_line: symbol.end_line,
                truncated: false,
                symbol,
                content,
            }),
            _ => None,
        };
        let mut score = ranked.score;
        if let Some(body) = &body {
            score += concepts.body_score(&body.content);
        }
        if !concepts.wants_tests && is_test_hit(&ranked.hit) {
            score *= PLAN_CONTEXT_TEST_PENALTY;
        }
        scored.push((score, ranked.order, ranked.hit, body));
    }
    scored.sort_by(|left, right| right.0.total_cmp(&left.0).then(left.1.cmp(&right.1)));
    let score_floor = scored
        .first()
        .map_or(0.0, |best| best.0 * PLAN_CONTEXT_SCORE_FLOOR_RATIO);
    scored.retain(|candidate| {
        candidate.0 >= score_floor || candidate.0 >= PLAN_CONTEXT_SPECIFIC_MATCH_SCORE
    });
    scored.truncate(PLAN_CONTEXT_SEARCH_LIMIT);
    let hits = scored
        .iter()
        .map(|candidate| candidate.2.clone())
        .collect::<Vec<_>>();

    let candidate_outlines = matching_context_outlines(store, repo, &hits)?;
    let mut bodies_by_path = Vec::<(String, Vec<(ContextSymbolBody, bool)>)>::new();
    for (score, _, hit, body) in scored {
        let path_index = bodies_by_path
            .iter()
            .position(|(path, _)| path == &hit.path)
            .unwrap_or_else(|| {
                bodies_by_path.push((hit.path.clone(), Vec::new()));
                bodies_by_path.len() - 1
            });
        let path_bodies = &mut bodies_by_path[path_index].1;
        if path_bodies.len() == PLAN_CONTEXT_BODIES_PER_FILE {
            continue;
        }
        if let Some(body) = body {
            path_bodies.push((body, score >= PLAN_CONTEXT_STRONG_SCORE));
        }
    }
    let mut candidate_bodies = Vec::new();
    for rank in 0..PLAN_CONTEXT_BODIES_PER_FILE {
        for (_, bodies) in &bodies_by_path {
            if let Some(body) = bodies.get(rank) {
                candidate_bodies.push(body.clone());
            }
        }
    }
    let evidence_files_total = bodies_by_path
        .iter()
        .filter(|(_, bodies)| !bodies.is_empty())
        .count();
    let estimated_raw_source_tokens = candidate_bodies.iter().fold(0_usize, |total, (body, _)| {
        total.saturating_add(body.symbol.token_estimate)
    });

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
    for (body, strong) in &candidate_bodies {
        let content_budget = if *strong {
            PLAN_CONTEXT_STRONG_EVIDENCE_TOKENS
        } else {
            PLAN_CONTEXT_EVIDENCE_CONTENT_TOKENS
        };
        let evidence = compact_context_evidence(body, content_budget, counter, &concepts);
        let item = render_context_evidence(&evidence);
        let trial = render_context_item(
            &rendered,
            "# Matching evidence excerpts\n",
            &item,
            symbol_bodies.is_empty(),
        );
        if counter.count(&trial) <= budget_tokens {
            rendered = trial;
            symbol_bodies.push(evidence);
        }
    }
    let included_paths = symbol_bodies
        .iter()
        .map(|body| body.path.as_str())
        .collect::<BTreeSet<_>>();
    let evidence_files_included = included_paths.len();
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
    let task_concepts_total = concepts.salient.len();
    let task_concepts_covered = task_concepts_total - uncovered_concepts.len();
    let tokens_used = counter.count(&rendered);
    let truncated = search_truncated
        || map.truncated
        || included_map.is_none()
        || symbol_bodies.len() < candidate_bodies.len();
    Ok(ContextPlan {
        rendered,
        repo_map: included_map,
        outlines,
        symbol_bodies,
        search_queries,
        matches_total,
        evidence_files_total,
        evidence_files_included,
        task_concepts_total,
        task_concepts_covered,
        uncovered_concepts: uncovered_concepts
            .into_iter()
            .take(PLAN_CONTEXT_UNCOVERED_LIMIT)
            .collect(),
        estimated_raw_source_tokens,
        tokens_used,
        token_provider: counter.provider(),
        truncated,
    })
}

/// One pooled search hit with its cross-query score and its position in the
/// rank-interleaved order, which breaks score ties.
struct RankedHit {
    hit: SearchHit,
    score: f64,
    order: usize,
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
    concepts: &mut TaskConcepts,
) -> Result<(Vec<String>, Vec<RankedHit>, usize, bool), AtlasError> {
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
        for hit in matched {
            *scores.entry(hit_identity(&hit)).or_default() += compound.weight();
            compound_hits.push(hit);
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
    for hit in compound_hits {
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
    pool.truncate(PLAN_CONTEXT_RERANK_POOL);
    Ok((search_queries, pool, matches_total, truncated))
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
    wants_tests: bool,
}

struct TaskCompound {
    /// Identifier spellings searched in the symbol index.
    queries: Vec<String>,
    /// Lowercase words joined without separators, compared against
    /// separator-free lowercase source text.
    joined: String,
    words: usize,
    /// 1.0 for a compound naming few symbols, less for one naming many.
    specificity: f64,
}

impl TaskCompound {
    fn weight(&self) -> f64 {
        let base = if self.words >= 3 { 6.0 } else { 4.0 };
        base * self.specificity
    }
}

impl TaskConcepts {
    fn parse(task: &str) -> Self {
        let words = task
            .split(|character: char| !character.is_alphanumeric() && character != '_')
            .filter(|word| !word.is_empty())
            .map(str::to_ascii_lowercase)
            .collect::<Vec<_>>();
        let wants_tests = words
            .iter()
            .any(|word| matches!(word.as_str(), "test" | "tests" | "tested" | "testing"));

        let mut salient = Vec::new();
        for word in &words {
            if word.chars().count() < 3 || is_context_stop_word(word) || is_generic_task_word(word)
            {
                continue;
            }
            let singular = singular_context_term(word);
            let duplicate = salient.iter().any(|term: &String| {
                term == word
                    || singular.as_ref() == Some(term)
                    || singular_context_term(term).as_ref() == Some(word)
            });
            if !duplicate {
                salient.push(word.clone());
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
                    words: size,
                    specificity: 1.0,
                });
            }
        }
        Self {
            compounds,
            salient,
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

    /// Index of the first body line containing a compound concept.
    fn focus_line(&self, content: &str) -> Option<usize> {
        if self.compounds.is_empty() {
            return None;
        }
        content.split_inclusive('\n').position(|line| {
            let squeezed = squeeze_separators(&line.to_lowercase());
            self.compounds
                .iter()
                .any(|compound| compound.specificity >= 1.0 && squeezed.contains(&compound.joined))
        })
    }

    /// Salient words that no included evidence names or contains.
    fn uncovered(&self, bodies: &[ContextSymbolBody]) -> Vec<String> {
        let haystacks = bodies
            .iter()
            .map(|body| {
                format!(
                    "{}\n{}\n{}\n{}",
                    body.path,
                    body.symbol.qualified_name,
                    body.symbol.signature.as_deref().unwrap_or_default(),
                    body.content
                )
                .to_lowercase()
            })
            .collect::<Vec<_>>();
        self.salient
            .iter()
            .filter(|term| !haystacks.iter().any(|text| text_mentions(text, term)))
            .cloned()
            .collect()
    }
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
    let path = hit.path.to_ascii_lowercase();
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
    let name = hit.symbol.qualified_name.to_ascii_lowercase();
    in_test_directory
        || test_file
        || hit.symbol.name.to_ascii_lowercase().starts_with("test_")
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
    for word in words {
        if word.chars().count() < 3 || is_context_stop_word(&word) {
            continue;
        }
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
    let first_line = if fits_whole {
        0
    } else {
        concepts
            .focus_line(&body.content)
            .map_or(0, |line| line.saturating_sub(1))
    };
    let mut content = String::new();
    for line in body.content.split_inclusive('\n').skip(first_line) {
        let candidate = format!("{content}{line}");
        if counter.count(&candidate) > content_budget {
            break;
        }
        content = candidate;
    }
    let mut first_line = first_line;
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

fn render_context_evidence(body: &ContextSymbolBody) -> String {
    let mut rendered = format!(
        "## {}:{}-{} — {} ({:?}",
        body.path,
        body.excerpt_start_line,
        body.excerpt_end_line,
        body.symbol.qualified_name,
        body.symbol.kind,
    );
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

enum ResolvedEntry {
    File(PathBuf),
    Missing,
}

fn canonical_query_root(root: &Path) -> Result<PathBuf, AtlasError> {
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

fn resolve_under_root(root: &Path, requested: &str) -> Result<ResolvedEntry, AtlasError> {
    if is_secret_path(Path::new(requested)) {
        return Ok(ResolvedEntry::Missing);
    }
    let joined = root.join(requested);
    match fs::canonicalize(&joined) {
        Ok(resolved) => {
            ensure_contained(root, &resolved)?;
            if !resolved.is_file() {
                return Err(QueryError::NotAFile { path: resolved }.into());
            }
            Ok(ResolvedEntry::File(resolved))
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
fn file_matches_index(
    path: &Path,
    indexed: &compresso_store::FileRecord,
) -> Result<bool, QueryError> {
    let read_error = |source| QueryError::Read {
        path: path.to_path_buf(),
        source,
    };
    let mut file = fs::File::open(path).map_err(read_error)?;
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

fn read_utf8(path: &Path) -> Result<String, QueryError> {
    read_utf8_capped(path, MAX_READ_FILE_BYTES)
}

/// Reads at most `limit` bytes, failing closed when the file holds more.
///
/// The ceiling is enforced on the open handle rather than on metadata alone,
/// so a file that grows after a size check still cannot exceed it.
fn read_utf8_capped(path: &Path, limit: u64) -> Result<String, QueryError> {
    use std::io::Read as _;

    let read_error = |source| QueryError::Read {
        path: path.to_path_buf(),
        source,
    };
    let too_large = || QueryError::FileTooLarge {
        path: path.to_path_buf(),
        limit_bytes: limit,
    };
    let file = fs::File::open(path).map_err(read_error)?;
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

    use super::{QueryError, STALENESS_HASH_BUFFER_BYTES, content_matches_index, read_utf8_capped};

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

    #[test]
    fn test_read_utf8_capped_accepts_file_at_limit() {
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let path = directory.path().join("at-limit.txt");
        fs::write(&path, "0123456789").expect("file should be written");

        let text = read_utf8_capped(&path, 10).expect("file at the limit should read");

        assert_eq!(text, "0123456789");
    }

    #[test]
    fn test_read_utf8_capped_rejects_one_byte_over_limit() {
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let path = directory.path().join("over-limit.txt");
        fs::write(&path, "0123456789A").expect("file should be written");

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
