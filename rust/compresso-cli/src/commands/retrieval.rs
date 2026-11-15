use std::fs;
use std::path::{Path, PathBuf};

use clap::{Args, ValueEnum};
use compresso_atlas::{
    AtlasError, BoundedRead, ContextPlan, QueryError, SearchOptions, UnfoldResult, impact, outline,
    plan_context, read_bounded, repo_map, search_with_diagnostics, unfold,
};
use compresso_core::{TokenProvider, counter_for};
use compresso_store::{
    EdgeKind, FileLanguage, RepoRecord, Store, StoreError, SymbolKind, SymbolRecord, SymbolSelector,
};
use serde_json::{Value, json};

use super::{OutputArgs, RootCommand};

const DEFAULT_MAP_TOKENS: usize = 2_000;
const DEFAULT_READ_TOKENS: usize = 2_000;
const DEFAULT_UNFOLD_TOKENS: usize = 4_000;
const DEFAULT_CONTEXT_TOKENS: usize = 800;
const DEFAULT_SEARCH_LIMIT: usize = 20;
const DEFAULT_IMPACT_RESULTS: usize = 100;
const MIN_TOKEN_BUDGET: usize = 100;
const MAX_TOKEN_BUDGET: usize = 100_000;

#[derive(Args, Debug, Default)]
pub struct RepositoryArgs {
    /// Absolute approved repository root; defaults to the approved cwd ancestor.
    #[arg(long, value_name = "PATH")]
    pub root: Option<PathBuf>,
}

#[derive(Args, Debug)]
pub struct MapArgs {
    #[command(flatten)]
    pub repository: RepositoryArgs,
    #[arg(long, default_value_t = DEFAULT_MAP_TOKENS)]
    pub max_tokens: usize,
    #[command(flatten)]
    pub output: OutputArgs,
}

#[derive(Args, Debug)]
pub struct SearchArgs {
    pub query: String,
    #[command(flatten)]
    pub repository: RepositoryArgs,
    /// Repository-relative file or directory to restrict hits to.
    #[arg(long, value_name = "REPO_PATH")]
    pub path: Option<String>,
    #[arg(long, default_value_t = DEFAULT_SEARCH_LIMIT)]
    pub limit: usize,
    #[command(flatten)]
    pub output: OutputArgs,
}

#[derive(Args, Debug)]
pub struct ReadArgs {
    pub path: String,
    #[command(flatten)]
    pub repository: RepositoryArgs,
    #[arg(long)]
    pub start_line: Option<u32>,
    #[arg(long)]
    pub end_line: Option<u32>,
    #[arg(long, default_value_t = DEFAULT_READ_TOKENS)]
    pub max_tokens: usize,
    #[command(flatten)]
    pub output: OutputArgs,
}

#[derive(Args, Debug)]
pub struct OutlineArgs {
    pub path: String,
    #[command(flatten)]
    pub repository: RepositoryArgs,
    #[command(flatten)]
    pub output: OutputArgs,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum CliSymbolKind {
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

#[derive(Args, Debug)]
pub struct UnfoldArgs {
    pub path: String,
    pub qualified_name: String,
    #[command(flatten)]
    pub repository: RepositoryArgs,
    #[arg(long, value_enum)]
    pub kind: Option<CliSymbolKind>,
    #[arg(long)]
    pub signature_hash: Option<String>,
    #[arg(long)]
    pub start_line_hint: Option<u32>,
    #[arg(long, default_value_t = DEFAULT_UNFOLD_TOKENS)]
    pub max_tokens: usize,
    #[command(flatten)]
    pub output: OutputArgs,
}

#[derive(Args, Debug)]
pub struct ContextArgs {
    pub task: String,
    #[command(flatten)]
    pub repository: RepositoryArgs,
    #[arg(long = "max-tokens", alias = "budget", default_value_t = DEFAULT_CONTEXT_TOKENS)]
    pub max_tokens: usize,
    #[command(flatten)]
    pub output: OutputArgs,
}

#[derive(Args, Debug)]
pub struct ImpactArgs {
    pub path: String,
    #[command(flatten)]
    pub repository: RepositoryArgs,
    #[arg(long, default_value_t = 2)]
    pub max_depth: u8,
    #[arg(long, default_value_t = DEFAULT_IMPACT_RESULTS)]
    pub max_results: usize,
    #[command(flatten)]
    pub output: OutputArgs,
}

pub(crate) struct Rendered {
    pub json: Value,
    pub human: String,
}

pub(crate) struct RetrievalError {
    pub code: &'static str,
    pub message: String,
}

pub(crate) fn execute(
    command: RootCommand,
    store: &Store,
    cwd: &Path,
) -> Result<Rendered, RetrievalError> {
    match command {
        RootCommand::Map(arguments) => execute_map(store, cwd, arguments),
        RootCommand::Search(arguments) => execute_search(store, cwd, arguments),
        RootCommand::Read(arguments) => execute_read(store, cwd, arguments),
        RootCommand::Outline(arguments) => execute_outline(store, cwd, arguments),
        RootCommand::Unfold(arguments) => execute_unfold(store, cwd, arguments),
        RootCommand::Context(arguments) => execute_context(store, cwd, arguments),
        RootCommand::Impact(arguments) => execute_impact(store, cwd, arguments),
        _ => Err(RetrievalError::internal()),
    }
}

fn execute_map(store: &Store, cwd: &Path, arguments: MapArgs) -> Result<Rendered, RetrievalError> {
    validate_budget(arguments.max_tokens)?;
    let repository = resolve_repository(store, arguments.repository.root.as_deref(), cwd)?;
    let result = repo_map(store, repository.id, arguments.max_tokens).map_err(map_atlas_error)?;
    let data = json!({
        "root": repository.root,
        "map": result.rendered.clone(),
        "files_total": result.files_total,
        "files_shown": result.files_shown,
        "tokens_used": result.tokens_used,
        "token_provider": result.token_provider,
        "truncated": result.truncated,
    });
    Ok(rendered(data, result.rendered))
}

fn execute_search(
    store: &Store,
    cwd: &Path,
    arguments: SearchArgs,
) -> Result<Rendered, RetrievalError> {
    compresso_atlas::validate_search_query(&arguments.query).map_err(map_query_error)?;
    if !(1..=100).contains(&arguments.limit) {
        return Err(RetrievalError::invalid("limit must be between 1 and 100"));
    }
    let repository = resolve_repository(store, arguments.repository.root.as_deref(), cwd)?;
    let report = search_with_diagnostics(
        store,
        repository.id,
        &arguments.query,
        SearchOptions {
            limit: arguments.limit,
            path: arguments.path.as_deref(),
        },
    )
    .map_err(map_atlas_error)?;
    let diagnostics = report
        .diagnostics
        .iter()
        .map(|diagnostic| {
            json!({
                "code": diagnostic.code.as_str(),
                "severity": "info",
                "message": diagnostic.message,
            })
        })
        .collect::<Vec<_>>();
    let hits = report
        .hits
        .into_iter()
        .map(|hit| {
            json!({
                "path": hit.path,
                "symbol": symbol_json(&hit.symbol),
                "score": hit.score,
            })
        })
        .collect::<Vec<_>>();
    let data = json!({
        "query": arguments.query,
        "path": arguments.path,
        "hits_total": hits.len(),
        "hits": hits,
        "diagnostics": diagnostics,
        "truncated": false,
    });
    Ok(rendered_pretty(data))
}

fn execute_read(
    store: &Store,
    cwd: &Path,
    arguments: ReadArgs,
) -> Result<Rendered, RetrievalError> {
    validate_budget(arguments.max_tokens)?;
    if arguments.path.is_empty() {
        return Err(RetrievalError::invalid("path must not be empty"));
    }
    let repository = resolve_repository(store, arguments.repository.root.as_deref(), cwd)?;
    let counter = counter_for(TokenProvider::OpenAiO200k);
    let range = match (arguments.start_line, arguments.end_line) {
        (None, None) => None,
        (None, Some(_)) => {
            return Err(RetrievalError::invalid("--end-line requires --start-line"));
        }
        (Some(start_line), Some(end_line)) => Some((start_line, end_line)),
        (Some(start_line), None) => {
            let full = read_bounded(&repository.root, &arguments.path, None, usize::MAX, counter)
                .map_err(map_atlas_error)?;
            Some((start_line, full.end_line))
        }
    };
    let result = read_bounded(
        &repository.root,
        &arguments.path,
        range,
        arguments.max_tokens,
        counter,
    )
    .map_err(map_atlas_error)?;
    Ok(render_bounded_read(result))
}

fn execute_outline(
    store: &Store,
    cwd: &Path,
    arguments: OutlineArgs,
) -> Result<Rendered, RetrievalError> {
    if arguments.path.is_empty() {
        return Err(RetrievalError::invalid("path must not be empty"));
    }
    let repository = resolve_repository(store, arguments.repository.root.as_deref(), cwd)?;
    validate_file_boundary(&repository, &arguments.path)?;
    let result = outline(store, repository.id, &arguments.path).map_err(map_atlas_error)?;
    let symbols = result.symbols.iter().map(symbol_json).collect::<Vec<_>>();
    let data = json!({
        "path": result.path,
        "language": result.language.map(language_name),
        "symbols": symbols,
    });
    Ok(rendered_pretty(data))
}

fn execute_unfold(
    store: &Store,
    cwd: &Path,
    arguments: UnfoldArgs,
) -> Result<Rendered, RetrievalError> {
    validate_budget(arguments.max_tokens)?;
    if arguments.path.is_empty() || arguments.qualified_name.trim().is_empty() {
        return Err(RetrievalError::invalid(
            "path and qualified name must not be empty",
        ));
    }
    let repository = resolve_repository(store, arguments.repository.root.as_deref(), cwd)?;
    let selector = SymbolSelector {
        path: arguments.path,
        qualified_name: arguments.qualified_name,
        kind: arguments.kind.map(CliSymbolKind::store_kind),
        signature_hash: arguments.signature_hash,
        start_line_hint: arguments.start_line_hint,
    };
    let result = unfold(store, repository.id, &selector).map_err(map_atlas_error)?;
    match result {
        UnfoldResult::Found {
            path,
            symbol,
            stale,
            reindex_hint,
            ..
        } => {
            let counter = counter_for(TokenProvider::OpenAiO200k);
            let body = read_bounded(
                &repository.root,
                &path,
                Some((symbol.start_line, symbol.end_line)),
                arguments.max_tokens,
                counter,
            )
            .map_err(map_atlas_error)?;
            let data = json!({
                "status": "found",
                "path": path,
                "symbol": symbol_json(&symbol),
                "content": body.content.clone(),
                "stale": stale,
                "reindex_hint": reindex_hint,
                "candidates": [],
                "tokens_used": body.tokens_used,
                "token_provider": body.token_provider,
                "truncated": body.truncated,
                "elision": body.elision,
            });
            Ok(rendered(data, body.content))
        }
        UnfoldResult::Ambiguous {
            candidates,
            stale,
            reindex_hint,
        } => {
            let candidates = candidates
                .into_iter()
                .map(|candidate| {
                    json!({
                        "path": candidate.path,
                        "qualified_name": candidate.qualified_name,
                        "kind": symbol_kind_name(candidate.kind),
                        "signature_hash": candidate.signature_hash,
                        "start_line": candidate.start_line,
                        "end_line": candidate.end_line,
                    })
                })
                .collect::<Vec<_>>();
            let data = json!({
                "status": "ambiguous",
                "stale": stale,
                "reindex_hint": reindex_hint,
                "candidates": candidates,
                "tokens_used": 0,
                "truncated": false,
            });
            Ok(rendered_pretty(data))
        }
        UnfoldResult::NotFound {
            path,
            qualified_name,
            stale,
            reindex_hint,
        } => {
            let data = json!({
                "status": "not_found",
                "path": path,
                "qualified_name": qualified_name,
                "stale": stale,
                "reindex_hint": reindex_hint,
                "candidates": [],
                "tokens_used": 0,
                "truncated": false,
            });
            Ok(rendered_pretty(data))
        }
    }
}

fn execute_context(
    store: &Store,
    cwd: &Path,
    arguments: ContextArgs,
) -> Result<Rendered, RetrievalError> {
    validate_budget(arguments.max_tokens)?;
    if arguments.task.trim().len() < 3 {
        return Err(RetrievalError::invalid(
            "task must contain at least three characters",
        ));
    }
    let repository = resolve_repository(store, arguments.repository.root.as_deref(), cwd)?;
    let plan = plan_context(store, repository.id, &arguments.task, arguments.max_tokens)
        .map_err(map_atlas_error)?;
    Ok(render_context(arguments.task, plan))
}

fn execute_impact(
    store: &Store,
    cwd: &Path,
    arguments: ImpactArgs,
) -> Result<Rendered, RetrievalError> {
    if arguments.path.is_empty() {
        return Err(RetrievalError::invalid("path must not be empty"));
    }
    if !(1..=2).contains(&arguments.max_depth) {
        return Err(RetrievalError::invalid("max depth must be between 1 and 2"));
    }
    if !(1..=500).contains(&arguments.max_results) {
        return Err(RetrievalError::invalid(
            "max results must be between 1 and 500",
        ));
    }
    let repository = resolve_repository(store, arguments.repository.root.as_deref(), cwd)?;
    validate_file_boundary(&repository, &arguments.path)?;
    let report = impact(store, repository.id, &arguments.path).map_err(map_atlas_error)?;
    let mut dependents = report
        .dependents
        .into_iter()
        .filter(|entry| entry.depth <= arguments.max_depth)
        .collect::<Vec<_>>();
    let truncated = dependents.len() > arguments.max_results;
    dependents.truncate(arguments.max_results);
    let dependents = dependents
        .into_iter()
        .map(|entry| {
            json!({
                "path": entry.path,
                "depth": entry.depth,
                "edge_kinds": entry.edge_kinds.into_iter().map(edge_kind_name).collect::<Vec<_>>(),
            })
        })
        .collect::<Vec<_>>();
    let unresolved_sources = store
        .incomplete_edge_sources(repository.id)
        .map_err(map_store_error)?;
    let complete = unresolved_sources.is_empty() && !truncated;
    let data = json!({
        "path": report.path,
        "dependents": dependents,
        "complete": complete,
        "unresolved_or_ambiguous_sources": unresolved_sources,
        "truncated": truncated,
    });
    Ok(rendered_pretty(data))
}

fn render_bounded_read(result: BoundedRead) -> Rendered {
    let data = json!({
        "path": result.path,
        "content": result.content.clone(),
        "start_line": result.start_line,
        "end_line": result.end_line,
        "selected_end_line": result.selected_end_line,
        "file_lines_total": result.file_lines_total,
        "range_clamped": result.range_clamped,
        "file_tokens_total": result.file_tokens_total,
        "tokens_used": result.tokens_used,
        "token_provider": result.token_provider,
        "truncated": result.truncated,
        "elision": result.elision,
    });
    rendered(data, result.content)
}

fn render_context(task: String, plan: ContextPlan) -> Rendered {
    let retrieval_status = if plan.matches_total == 0 {
        "no_matches"
    } else {
        "matched"
    };
    let retrieval_note = (plan.matches_total == 0).then_some(
        "No indexed symbols matched the derived task queries. This is not proof that the repository contains no relevant implementation.",
    );
    let map_excerpt = plan.repo_map.as_ref().map(|map| map.rendered.clone());
    let outlines = plan
        .outlines
        .iter()
        .map(|item| {
            json!({
                "path": item.path,
                "language": item.language.map(language_name),
                "symbols": item.symbols.iter().map(symbol_json).collect::<Vec<_>>(),
            })
        })
        .collect::<Vec<_>>();
    let bodies = plan
        .symbol_bodies
        .iter()
        .map(|body| {
            json!({
                "path": body.path,
                "symbol": symbol_json(&body.symbol),
                "content": body.content,
                "excerpt_end_line": body.excerpt_end_line,
                "truncated": body.truncated,
            })
        })
        .collect::<Vec<_>>();
    let data = json!({
        "task": task,
        "retrieval_status": retrieval_status,
        "retrieval_note": retrieval_note,
        "search_queries": &plan.search_queries,
        "matches_total": plan.matches_total,
        "evidence_coverage": {
            "files_included": plan.evidence_files_included,
            "candidate_files": plan.evidence_files_total,
        },
        "plan": {
            "map_excerpt": map_excerpt,
            "outlines": outlines,
            "bodies": bodies,
        },
        "tokens_used": plan.tokens_used,
        "token_provider": plan.token_provider,
        "truncated": plan.truncated,
    });
    rendered(data, plan.rendered)
}

fn validate_file_boundary(repository: &RepoRecord, path: &str) -> Result<(), RetrievalError> {
    let counter = counter_for(TokenProvider::OpenAiO200k);
    read_bounded(&repository.root, path, None, 0, counter)
        .map(|_| ())
        .map_err(map_atlas_error)
}

fn validate_budget(budget: usize) -> Result<(), RetrievalError> {
    if (MIN_TOKEN_BUDGET..=MAX_TOKEN_BUDGET).contains(&budget) {
        Ok(())
    } else {
        Err(RetrievalError::invalid(
            "max tokens must be between 100 and 100000",
        ))
    }
}

fn resolve_repository(
    store: &Store,
    requested_root: Option<&Path>,
    cwd: &Path,
) -> Result<RepoRecord, RetrievalError> {
    if let Some(root) = requested_root {
        if !root.is_absolute() {
            return Err(RetrievalError::invalid("--root must be an absolute path"));
        }
        let canonical = fs::canonicalize(root).map_err(|_| RetrievalError::not_indexed())?;
        if !canonical.is_dir() {
            return Err(RetrievalError::invalid("--root must name a directory"));
        }
        let repository = store
            .repo_by_root(&canonical)
            .map_err(map_store_error)?
            .ok_or_else(RetrievalError::not_indexed)?;
        return require_indexed(repository);
    }

    let canonical = fs::canonicalize(cwd).map_err(|_| RetrievalError::not_indexed())?;
    for ancestor in canonical.ancestors() {
        if let Some(repository) = store.repo_by_root(ancestor).map_err(map_store_error)? {
            return require_indexed(repository);
        }
    }
    Err(RetrievalError::not_indexed())
}

fn require_indexed(repository: RepoRecord) -> Result<RepoRecord, RetrievalError> {
    if repository.last_indexed_at.is_some() {
        Ok(repository)
    } else {
        Err(RetrievalError::not_indexed())
    }
}

fn rendered(data: Value, human: String) -> Rendered {
    Rendered {
        json: json!({ "schema_version": 1, "data": data }),
        human,
    }
}

fn rendered_pretty(data: Value) -> Rendered {
    let human = serde_json::to_string_pretty(&data)
        .unwrap_or_else(|_| "local repository query completed".to_owned());
    rendered(data, human)
}

fn symbol_json(symbol: &SymbolRecord) -> Value {
    json!({
        "name": symbol.name,
        "qualified_name": symbol.qualified_name,
        "kind": symbol_kind_name(symbol.kind),
        "signature": symbol.signature,
        "start_line": symbol.start_line,
        "end_line": symbol.end_line,
        "token_estimate": symbol.token_estimate,
    })
}

const fn symbol_kind_name(kind: SymbolKind) -> &'static str {
    match kind {
        SymbolKind::Function => "function",
        SymbolKind::Method => "method",
        SymbolKind::Struct => "struct",
        SymbolKind::Class => "class",
        SymbolKind::Enum => "enum",
        SymbolKind::Trait => "trait",
        SymbolKind::Interface => "interface",
        SymbolKind::Const => "const",
        SymbolKind::Type => "type",
        SymbolKind::Module => "module",
    }
}

const fn language_name(language: FileLanguage) -> &'static str {
    match language {
        FileLanguage::Rust => "rust",
        FileLanguage::TypeScript => "typescript",
        FileLanguage::Tsx => "tsx",
        FileLanguage::JavaScript => "javascript",
        FileLanguage::Python => "python",
        FileLanguage::Shell => "shell",
        FileLanguage::Text => "text",
    }
}

const fn edge_kind_name(kind: EdgeKind) -> &'static str {
    match kind {
        EdgeKind::Imports => "imports",
        EdgeKind::References => "references",
        EdgeKind::Tests => "tests",
    }
}

impl CliSymbolKind {
    const fn store_kind(self) -> SymbolKind {
        match self {
            Self::Function => SymbolKind::Function,
            Self::Method => SymbolKind::Method,
            Self::Struct => SymbolKind::Struct,
            Self::Class => SymbolKind::Class,
            Self::Enum => SymbolKind::Enum,
            Self::Trait => SymbolKind::Trait,
            Self::Interface => SymbolKind::Interface,
            Self::Const => SymbolKind::Const,
            Self::Type => SymbolKind::Type,
            Self::Module => SymbolKind::Module,
        }
    }
}

fn map_atlas_error(error: AtlasError) -> RetrievalError {
    match error {
        AtlasError::OutsideRoot { .. } => RetrievalError::outside_root(),
        AtlasError::FileNotIndexed { .. } => RetrievalError::not_found(),
        AtlasError::Query(error) => map_query_error(error),
        AtlasError::Store(error) => map_store_error(error),
        AtlasError::Scan(_) | AtlasError::Index(_) => RetrievalError::internal(),
    }
}

fn map_query_error(error: QueryError) -> RetrievalError {
    match error {
        QueryError::NotFound { .. } => RetrievalError::not_found(),
        invalid @ (QueryError::InvalidFocus { .. }
        | QueryError::InvalidSearchQuery { .. }
        | QueryError::InvalidSearchPath { .. }
        | QueryError::InvalidImpactOptions { .. }
        | QueryError::InvalidRange { .. }
        | QueryError::InvalidSymbolRange { .. }) => RetrievalError::invalid(&invalid.to_string()),
        QueryError::NotAFile { .. } => {
            RetrievalError::invalid("repository path must name a regular file")
        }
        QueryError::Read { source, .. } if source.kind() == std::io::ErrorKind::InvalidData => {
            RetrievalError::invalid("repository file must contain UTF-8 text")
        }
        QueryError::FileTooLarge { limit_bytes, .. } => RetrievalError::file_too_large(limit_bytes),
        QueryError::CanonicalizeRoot { .. }
        | QueryError::RootNotDirectory { .. }
        | QueryError::ResolvePath { .. }
        | QueryError::Read { .. }
        | QueryError::Parse { .. } => RetrievalError::internal(),
    }
}

fn map_store_error(error: StoreError) -> RetrievalError {
    match error {
        invalid @ (StoreError::InvalidRelativePath { .. }
        | StoreError::InvalidField { .. }
        | StoreError::InvalidSymbolLineRange { .. }
        | StoreError::InvalidSearchQuery { .. }
        | StoreError::InvalidSignatureHash(_)) => RetrievalError::invalid(&invalid.to_string()),
        StoreError::RepositoryNotFound(_) => RetrievalError::not_indexed(),
        _ => RetrievalError::internal(),
    }
}

impl RetrievalError {
    fn invalid(message: &str) -> Self {
        Self {
            code: "invalid_arguments",
            message: message.to_owned(),
        }
    }

    fn not_indexed() -> Self {
        Self {
            code: "not_indexed",
            message: "repository is not indexed; run `compresso index` in the repository and retry"
                .to_owned(),
        }
    }

    fn not_found() -> Self {
        Self {
            code: "not_found",
            message: "repository file was not found".to_owned(),
        }
    }

    fn outside_root() -> Self {
        Self {
            code: "outside_root",
            message: "path resolves outside the approved repository root".to_owned(),
        }
    }

    fn file_too_large(limit_bytes: u64) -> Self {
        Self {
            code: "file_too_large",
            message: format!(
                "repository file exceeds the {} MiB read limit",
                limit_bytes / (1024 * 1024)
            ),
        }
    }

    fn internal() -> Self {
        Self {
            code: "internal",
            message: "local repository query failed".to_owned(),
        }
    }
}
