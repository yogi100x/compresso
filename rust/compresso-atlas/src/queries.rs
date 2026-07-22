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

const TOP_SYMBOLS_PER_FILE: usize = 3;
const MAX_AMBIGUITY_CANDIDATES: usize = 20;
const PLAN_CONTEXT_SEARCH_LIMIT: usize = 20;
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
    pub file_tokens_total: usize,
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

/// The deterministic reverse dependency closure for one indexed file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImpactReport {
    pub path: String,
    pub dependents: Vec<ImpactEntry>,
}

/// One exact symbol body included because its identity was returned by task FTS.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContextSymbolBody {
    pub path: String,
    pub symbol: SymbolRecord,
    pub content: String,
}

/// A deterministic search-driven context pack under one hard tokenizer budget.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContextPlan {
    pub rendered: String,
    pub repo_map: Option<RepoMap>,
    pub outlines: Vec<Outline>,
    pub symbol_bodies: Vec<ContextSymbolBody>,
    pub tokens_used: usize,
    pub token_provider: TokenProvider,
    pub truncated: bool,
}

/// Typed structural-query and bounded-filesystem failures.
#[derive(Debug, thiserror::Error)]
pub enum QueryError {
    #[error("repository-map focus must be a lexical repository-relative prefix: {focus:?}")]
    InvalidFocus { focus: String },
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
    let files = store.indexed_files(repo)?;
    let mut importers = BTreeMap::<String, BTreeSet<String>>::new();
    for edge in store.resolved_edges(repo)? {
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

fn validate_map_focus(focus: &str) -> Result<Option<&str>, QueryError> {
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

fn map_path_matches_focus(path: &str, focus: &str) -> bool {
    path == focus
        || path
            .strip_prefix(focus)
            .is_some_and(|remainder| remainder.starts_with('/'))
}

/// Returns every stored symbol for `path` in deterministic source order.
pub fn outline(store: &Store, repo: RepoId, path: &str) -> Result<Outline, AtlasError> {
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
    let current = fs::read(&resolved).map_err(|source| QueryError::Read {
        path: resolved,
        source,
    })?;
    let stale = blake3::hash(&current).to_hex().as_str() != indexed.hash;
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
        let current = fs::read(&resolved).map_err(|source| QueryError::Read {
            path: resolved,
            source,
        })?;
        if blake3::hash(&current).to_hex().as_str() != indexed.hash {
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

/// Returns Store's FTS5/BM25 ranking as typed Atlas hits without reordering it.
pub fn search(
    store: &Store,
    repo: RepoId,
    query: &str,
    limit: usize,
) -> Result<Vec<SearchHit>, AtlasError> {
    store
        .search_symbols(repo, query, limit)?
        .into_iter()
        .map(|hit| {
            Ok(SearchHit {
                path: hit.file.path,
                symbol: hit.symbol,
                score: hit.rank,
            })
        })
        .collect()
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
    let file_tokens_total = counter.count(&source);
    let total_lines = line_count(&source);
    let (start_line, requested_end, selected) = match range {
        Some((start_line, end_line)) => {
            validate_range(start_line, end_line, total_lines)?;
            (
                start_line,
                end_line,
                source_line_range(&source, &resolved, start_line, end_line)?,
            )
        }
        None if total_lines == 0 => (0, 0, ""),
        None => (1, total_lines, source.as_str()),
    };
    let selected_tokens = counter.count(selected);
    let (content, truncated) = if selected_tokens <= budget_tokens {
        (selected.to_owned(), false)
    } else {
        (prefix_within_budget(selected, budget_tokens, counter), true)
    };
    let tokens_used = counter.count(&content);
    let end_line = if truncated {
        included_end_line(start_line, &content).min(requested_end)
    } else {
        requested_end
    };
    let elision = truncated.then(|| {
        format!(
            "[lines {}-{} elided: {} tokens over budget]",
            end_line.max(start_line),
            requested_end,
            selected_tokens.saturating_sub(tokens_used)
        )
    });
    Ok(BoundedRead {
        path: path.to_owned(),
        content,
        start_line,
        end_line,
        file_tokens_total,
        tokens_used,
        token_provider: counter.provider(),
        truncated,
        elision,
    })
}

/// Returns resolved reverse dependencies at minimum depths one and two only.
pub fn impact(store: &Store, repo: RepoId, path: &str) -> Result<ImpactReport, AtlasError> {
    if store.indexed_file(repo, path)?.is_none() {
        return Err(AtlasError::FileNotIndexed {
            repo,
            path: path.to_owned(),
        });
    }

    let mut incoming = BTreeMap::<String, BTreeMap<String, Vec<EdgeKind>>>::new();
    for edge in store.resolved_edges(repo)? {
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
    for depth in 1..=2 {
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
    Ok(ImpactReport {
        path: path.to_owned(),
        dependents,
    })
}

const fn edge_kind_order(kind: EdgeKind) -> u8 {
    match kind {
        EdgeKind::Imports => 0,
        EdgeKind::References => 1,
        EdgeKind::Tests => 2,
    }
}

/// Builds a map, matching-file outlines, and exact FTS-backed bodies under one budget.
pub fn plan_context(
    store: &Store,
    repo: RepoId,
    task: &str,
    budget_tokens: usize,
) -> Result<ContextPlan, AtlasError> {
    let counter = counter_for(TokenProvider::OpenAiO200k);
    let mut hits = search(
        store,
        repo,
        task,
        PLAN_CONTEXT_SEARCH_LIMIT.saturating_add(1),
    )?;
    let search_truncated = hits.len() > PLAN_CONTEXT_SEARCH_LIMIT;
    hits.truncate(PLAN_CONTEXT_SEARCH_LIMIT);

    let map = repo_map(store, repo, budget_tokens / 4)?;
    let mut candidate_outlines = Vec::new();
    let mut outlined_paths = BTreeSet::new();
    for hit in &hits {
        if outlined_paths.insert(hit.path.clone()) {
            candidate_outlines.push(outline(store, repo, &hit.path)?);
        }
    }

    let mut candidate_bodies = Vec::new();
    for hit in &hits {
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
        if let UnfoldResult::Found {
            path,
            symbol,
            content,
            ..
        } = unfold(store, repo, &selector)?
        {
            candidate_bodies.push(ContextSymbolBody {
                path,
                symbol,
                content,
            });
        }
    }

    let mut rendered = String::new();
    let mut included_map = None;
    if !map.rendered.is_empty() {
        let trial = render_context_item(&rendered, "# Repository map\n", &map.rendered, true);
        if counter.count(&trial) <= budget_tokens {
            rendered = trial;
            included_map = Some(map.clone());
        }
    }

    let outline_ceiling = budget_tokens.saturating_sub(budget_tokens / 3);
    let mut outlines = Vec::new();
    for outline in &candidate_outlines {
        let item = render_outline(outline);
        let trial = render_context_item(
            &rendered,
            "# Matching outlines\n",
            &item,
            outlines.is_empty(),
        );
        if counter.count(&trial) <= outline_ceiling {
            rendered = trial;
            outlines.push(outline.clone());
        }
    }

    let mut symbol_bodies = Vec::new();
    for body in &candidate_bodies {
        let item = render_symbol_body(body);
        let trial = render_context_item(
            &rendered,
            "# Matching symbol bodies\n",
            &item,
            symbol_bodies.is_empty(),
        );
        if counter.count(&trial) <= budget_tokens {
            rendered = trial;
            symbol_bodies.push(body.clone());
        }
    }

    let tokens_used = counter.count(&rendered);
    let truncated = search_truncated
        || map.truncated
        || included_map.is_none()
        || outlines.len() < candidate_outlines.len()
        || symbol_bodies.len() < candidate_bodies.len();
    Ok(ContextPlan {
        rendered,
        repo_map: included_map,
        outlines,
        symbol_bodies,
        tokens_used,
        token_provider: counter.provider(),
        truncated,
    })
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

fn render_outline(outline: &Outline) -> String {
    let mut rendered = format!("## {}\n", outline.path);
    for symbol in &outline.symbols {
        let _ = writeln!(
            rendered,
            "- {:?} {} (lines {}-{}, ~{} tokens)",
            symbol.kind,
            symbol.qualified_name,
            symbol.start_line,
            symbol.end_line,
            symbol.token_estimate
        );
    }
    rendered
}

fn render_symbol_body(body: &ContextSymbolBody) -> String {
    let mut rendered = format!(
        "## {} — {} ({:?}, lines {}-{})\n",
        body.path,
        body.symbol.qualified_name,
        body.symbol.kind,
        body.symbol.start_line,
        body.symbol.end_line
    );
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

fn canonical_query_root(root: &Path) -> Result<PathBuf, QueryError> {
    let canonical = fs::canonicalize(root).map_err(|source| QueryError::CanonicalizeRoot {
        path: root.to_path_buf(),
        source,
    })?;
    if !canonical.is_dir() {
        return Err(QueryError::RootNotDirectory { path: canonical });
    }
    Ok(canonical)
}

fn resolve_under_root(root: &Path, requested: &str) -> Result<ResolvedEntry, AtlasError> {
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

fn read_utf8(path: &Path) -> Result<String, QueryError> {
    fs::read_to_string(path).map_err(|source| QueryError::Read {
        path: path.to_path_buf(),
        source,
    })
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

fn validate_range(start_line: u32, end_line: u32, total_lines: u32) -> Result<(), QueryError> {
    if start_line == 0 || end_line < start_line || end_line > total_lines {
        return Err(QueryError::InvalidRange {
            start_line,
            end_line,
            total_lines,
        });
    }
    Ok(())
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
