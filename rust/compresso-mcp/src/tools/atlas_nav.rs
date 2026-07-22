use std::fs;
use std::path::{Component, Path};

use compresso_atlas::{
    AtlasError, QueryError, UnfoldResult, outline_current, repo_map_focused, repository_stale,
    unfold,
};
use compresso_core::{TokenCounter, TokenProvider, counter_for, operation_identity};
use compresso_store::{FileLanguage, RepoRecord, Store, SymbolKind, SymbolRecord, SymbolSelector};
use rmcp::model::{CallToolResult, Content, JsonObject};
use serde_json::{Map, Value, json};

const MIN_TOKENS: usize = 100;
const MAX_TOKENS: usize = 100_000;
const MAP_DEFAULT_TOKENS: usize = 2_000;
const OUTLINE_DEFAULT_TOKENS: usize = 1_500;
const UNFOLD_DEFAULT_TOKENS: usize = 4_000;
const REINDEX_HINT: &str = "Repository index is stale; run `compresso index` and retry.";
const ELISION_MARKER: &str = "\n[content elided: token budget exceeded]";

pub(crate) fn handles(name: &str) -> bool {
    matches!(
        name,
        "compresso_repo_map" | "compresso_outline" | "compresso_unfold"
    )
}

pub(crate) fn dispatch(store: &Store, name: &str, arguments: Option<JsonObject>) -> CallToolResult {
    let result = match name {
        "compresso_repo_map" => repo_map_tool(store, arguments),
        "compresso_outline" => outline_tool(store, arguments),
        "compresso_unfold" => unfold_tool(store, arguments),
        _ => Err(ToolFailure::internal()),
    };
    match result {
        Ok(payload) => success(payload),
        Err(error) => error.into_result(),
    }
}

fn repo_map_tool(store: &Store, arguments: Option<JsonObject>) -> ToolResult {
    let mut arguments = checked_arguments(arguments, &["root", "focus", "max_tokens"])?;
    let root = optional_string(&mut arguments, "root", false)?;
    let focus = optional_string(&mut arguments, "focus", true)?;
    let max_tokens = token_budget(&mut arguments, MAP_DEFAULT_TOKENS)?;
    let repository = resolve_repository(store, root.as_deref())?;

    let map = repo_map_focused(store, repository.id, focus.as_deref(), max_tokens)
        .map_err(map_atlas_error)?;
    let stale = repository_stale(store, repository.id).map_err(map_atlas_error)?;
    let operation_id = operation_identity().id;
    Ok(json!({
        "schema_version": 1,
        "operation_id": operation_id,
        "root": repository.root.to_string_lossy(),
        "map": map.rendered,
        "files_total": map.files_total,
        "files_shown": map.files_shown,
        "stale": stale,
        "reindex_hint": stale.then_some(REINDEX_HINT),
        "tokens_used": map.tokens_used,
        "token_provider": map.token_provider,
        "truncated": map.truncated
    }))
}

fn outline_tool(store: &Store, arguments: Option<JsonObject>) -> ToolResult {
    let mut arguments = checked_arguments(arguments, &["root", "path", "max_tokens"])?;
    let root = optional_string(&mut arguments, "root", false)?;
    let path = required_string(&mut arguments, "path")?;
    let max_tokens = token_budget(&mut arguments, OUTLINE_DEFAULT_TOKENS)?;
    let repository = resolve_repository(store, root.as_deref())?;
    let path_state = validate_repo_path(&repository.root, &path)?;
    let current = match outline_current(store, repository.id, &path) {
        Ok(current) => current,
        Err(AtlasError::FileNotIndexed { .. }) => {
            return Err(match path_state {
                PathState::File => ToolFailure::new(
                    "not_found",
                    format!(
                        "File '{path}' is on disk but is not indexed as a supported language; use compresso_read."
                    ),
                ),
                PathState::Missing => {
                    ToolFailure::new("not_found", format!("File '{path}' was not found on disk."))
                }
            });
        }
        Err(AtlasError::Query(QueryError::NotFound { .. })) => {
            return Err(ToolFailure::new(
                "not_found",
                format!("File '{path}' was not found on disk."),
            ));
        }
        Err(error) => return Err(map_atlas_error(error)),
    };
    let counter = navigation_counter();
    let all_symbols = current
        .outline
        .symbols
        .iter()
        .map(symbol_value)
        .collect::<Vec<_>>();
    let (symbols, tokens_used, truncated) = cap_values(all_symbols, max_tokens, counter);
    let elision = truncated.then(|| {
        format!(
            "[{} symbols elided: token budget exceeded]",
            current.outline.symbols.len().saturating_sub(symbols.len())
        )
    });
    let operation_id = operation_identity().id;
    Ok(json!({
        "schema_version": 1,
        "operation_id": operation_id,
        "path": current.outline.path,
        "language": current.outline.language.map(language_name),
        "symbols": symbols,
        "stale": current.stale,
        "reindex_hint": current.reindex_hint,
        "elision": elision,
        "tokens_used": tokens_used,
        "token_provider": counter.provider(),
        "truncated": truncated
    }))
}

fn unfold_tool(store: &Store, arguments: Option<JsonObject>) -> ToolResult {
    let mut arguments = checked_arguments(
        arguments,
        &[
            "root",
            "path",
            "qualified_name",
            "kind",
            "signature_hash",
            "start_line_hint",
            "max_tokens",
        ],
    )?;
    let root = optional_string(&mut arguments, "root", false)?;
    let path = required_string(&mut arguments, "path")?;
    let qualified_name = required_string(&mut arguments, "qualified_name")?;
    let kind = optional_string(&mut arguments, "kind", false)?
        .map(|kind| parse_symbol_kind(&kind))
        .transpose()?;
    let signature_hash = optional_string(&mut arguments, "signature_hash", false)?;
    if signature_hash.as_deref().is_some_and(|hash| {
        hash.len() != 64
            || !hash
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    }) {
        return Err(ToolFailure::invalid(
            "signature_hash must contain exactly 64 lowercase hexadecimal characters.",
        ));
    }
    let start_line_hint = optional_u32(&mut arguments, "start_line_hint", 1, u32::MAX)?;
    let max_tokens = token_budget(&mut arguments, UNFOLD_DEFAULT_TOKENS)?;
    let repository = resolve_repository(store, root.as_deref())?;
    validate_repo_path(&repository.root, &path)?;
    let selector = SymbolSelector {
        path,
        qualified_name,
        kind,
        signature_hash,
        start_line_hint,
    };
    let result = unfold(store, repository.id, &selector).map_err(map_atlas_error)?;
    let counter = navigation_counter();
    let operation_id = operation_identity().id;

    Ok(match result {
        UnfoldResult::Found {
            path,
            symbol,
            content,
            stale,
            reindex_hint,
        } => {
            let (content, tokens_used, truncated) = bound_text(&content, max_tokens, counter);
            json!({
                "schema_version": 1,
                "operation_id": operation_id,
                "status": "found",
                "path": path,
                "qualified_name": selector.qualified_name,
                "symbol": symbol_value(&symbol),
                "content": content,
                "stale": stale,
                "reindex_hint": reindex_hint,
                "candidates": [],
                "tokens_used": tokens_used,
                "token_provider": counter.provider(),
                "truncated": truncated
            })
        }
        UnfoldResult::Ambiguous {
            candidates,
            stale,
            reindex_hint,
        } => {
            let atlas_capped = candidates.len() == 20;
            let all_candidates = candidates
                .iter()
                .map(|candidate| {
                    json!({
                        "path": candidate.path,
                        "qualified_name": candidate.qualified_name,
                        "kind": symbol_kind_name(candidate.kind),
                        "signature_hash": candidate.signature_hash,
                        "start_line": candidate.start_line,
                        "end_line": candidate.end_line
                    })
                })
                .collect::<Vec<_>>();
            let (candidates, tokens_used, budget_truncated) =
                cap_values(all_candidates, max_tokens, counter);
            let truncated = atlas_capped || budget_truncated;
            json!({
                "schema_version": 1,
                "operation_id": operation_id,
                "status": "ambiguous",
                "path": selector.path,
                "qualified_name": selector.qualified_name,
                "symbol": Value::Null,
                "content": Value::Null,
                "stale": stale,
                "reindex_hint": reindex_hint,
                "candidates": candidates,
                "elision": truncated.then_some("[additional candidates elided]"),
                "tokens_used": tokens_used,
                "token_provider": counter.provider(),
                "truncated": truncated
            })
        }
        UnfoldResult::NotFound {
            path,
            qualified_name,
            stale,
            reindex_hint,
        } => {
            let tokens_used = counter.count(&qualified_name).min(max_tokens);
            json!({
                "schema_version": 1,
                "operation_id": operation_id,
                "status": "not_found",
                "path": path,
                "qualified_name": qualified_name,
                "symbol": Value::Null,
                "content": Value::Null,
                "stale": stale,
                "reindex_hint": reindex_hint,
                "candidates": [],
                "tokens_used": tokens_used,
                "token_provider": counter.provider(),
                "truncated": false
            })
        }
    })
}

type ToolResult = Result<Value, ToolFailure>;

struct ToolFailure {
    code: &'static str,
    message: String,
}

impl ToolFailure {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    fn invalid(message: impl Into<String>) -> Self {
        Self::new("invalid_arguments", message)
    }

    fn outside_root() -> Self {
        Self::new(
            "outside_root",
            "The requested path resolves outside the approved repository root.",
        )
    }

    fn internal() -> Self {
        Self::new(
            "internal",
            "Compresso could not complete the request because of an internal error.",
        )
    }

    fn into_result(self) -> CallToolResult {
        CallToolResult::error(vec![Content::text(format!(
            "[{}] {}",
            self.code, self.message
        ))])
    }
}

fn success(payload: Value) -> CallToolResult {
    match serde_json::to_string(&payload) {
        Ok(text) => CallToolResult::success(vec![Content::text(text)]),
        Err(_) => ToolFailure::internal().into_result(),
    }
}

fn checked_arguments(
    arguments: Option<JsonObject>,
    allowed: &[&str],
) -> Result<Map<String, Value>, ToolFailure> {
    let arguments = arguments.unwrap_or_default();
    if let Some(unknown) = arguments
        .keys()
        .find(|key| !allowed.contains(&key.as_str()))
    {
        return Err(ToolFailure::invalid(format!(
            "unknown argument '{unknown}'."
        )));
    }
    Ok(arguments)
}

fn required_string(
    arguments: &mut Map<String, Value>,
    name: &'static str,
) -> Result<String, ToolFailure> {
    optional_string(arguments, name, false)?
        .ok_or_else(|| ToolFailure::invalid(format!("missing required string argument '{name}'.")))
}

fn optional_string(
    arguments: &mut Map<String, Value>,
    name: &'static str,
    allow_empty: bool,
) -> Result<Option<String>, ToolFailure> {
    let Some(value) = arguments.remove(name) else {
        return Ok(None);
    };
    let Value::String(value) = value else {
        return Err(ToolFailure::invalid(format!(
            "argument '{name}' must be a string."
        )));
    };
    if !allow_empty && value.is_empty() {
        return Err(ToolFailure::invalid(format!(
            "argument '{name}' must not be empty."
        )));
    }
    Ok(Some(value))
}

fn token_budget(arguments: &mut Map<String, Value>, default: usize) -> Result<usize, ToolFailure> {
    optional_usize(arguments, "max_tokens", MIN_TOKENS, MAX_TOKENS)
        .map(|value| value.unwrap_or(default))
}

fn optional_usize(
    arguments: &mut Map<String, Value>,
    name: &'static str,
    minimum: usize,
    maximum: usize,
) -> Result<Option<usize>, ToolFailure> {
    let Some(value) = arguments.remove(name) else {
        return Ok(None);
    };
    let Some(value) = value.as_u64().and_then(|value| usize::try_from(value).ok()) else {
        return Err(ToolFailure::invalid(format!(
            "argument '{name}' must be an integer from {minimum} through {maximum}."
        )));
    };
    if !(minimum..=maximum).contains(&value) {
        return Err(ToolFailure::invalid(format!(
            "argument '{name}' must be an integer from {minimum} through {maximum}."
        )));
    }
    Ok(Some(value))
}

fn optional_u32(
    arguments: &mut Map<String, Value>,
    name: &'static str,
    minimum: u32,
    maximum: u32,
) -> Result<Option<u32>, ToolFailure> {
    let Some(value) = arguments.remove(name) else {
        return Ok(None);
    };
    let Some(value) = value.as_u64().and_then(|value| u32::try_from(value).ok()) else {
        return Err(ToolFailure::invalid(format!(
            "argument '{name}' must be an integer from {minimum} through {maximum}."
        )));
    };
    if !(minimum..=maximum).contains(&value) {
        return Err(ToolFailure::invalid(format!(
            "argument '{name}' must be an integer from {minimum} through {maximum}."
        )));
    }
    Ok(Some(value))
}

fn resolve_repository(
    store: &Store,
    supplied_root: Option<&str>,
) -> Result<RepoRecord, ToolFailure> {
    if let Some(root) = supplied_root {
        let root = Path::new(root);
        if !root.is_absolute() {
            return Err(ToolFailure::invalid(
                "argument 'root' must be an absolute path.",
            ));
        }
        let canonical = match fs::canonicalize(root) {
            Ok(canonical) if canonical.is_dir() => canonical,
            _ => return Err(not_indexed(root)),
        };
        let repository = store
            .repo_by_root(&canonical)
            .map_err(|_| ToolFailure::internal())?
            .ok_or_else(|| not_indexed(&canonical))?;
        return indexed_repository(repository);
    }

    let repositories = store
        .approved_repositories()
        .map_err(|_| ToolFailure::internal())?;
    let current = std::env::current_dir()
        .ok()
        .and_then(|cwd| fs::canonicalize(cwd).ok());
    if let Some(current) = current
        && let Some(repository) = repositories
            .iter()
            .filter(|repository| current.starts_with(&repository.root))
            .max_by_key(|repository| repository.root.components().count())
    {
        return indexed_repository(repository.clone());
    }
    if repositories.len() == 1 {
        return indexed_repository(repositories[0].clone());
    }
    Err(ToolFailure::new(
        "not_indexed",
        "No indexed repository could be resolved. Supply its absolute root or run `compresso index` in that directory, then retry.",
    ))
}

fn indexed_repository(repository: RepoRecord) -> Result<RepoRecord, ToolFailure> {
    if repository.last_indexed_at.is_none() {
        return Err(not_indexed(&repository.root));
    }
    Ok(repository)
}

fn not_indexed(root: &Path) -> ToolFailure {
    ToolFailure::new(
        "not_indexed",
        format!(
            "Repository '{}' is not indexed by Compresso. Run `compresso index` in that directory, then retry.",
            root.display()
        ),
    )
}

#[derive(Clone, Copy)]
enum PathState {
    File,
    Missing,
}

fn validate_repo_path(root: &Path, requested: &str) -> Result<PathState, ToolFailure> {
    if requested.is_empty()
        || requested.contains('\0')
        || requested.contains('\\')
        || requested.split('/').any(str::is_empty)
    {
        return Err(ToolFailure::invalid(
            "argument 'path' must be a non-empty repository-relative forward-slash path.",
        ));
    }
    let path = Path::new(requested);
    if path.is_absolute() {
        let resolved_or_lexical = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        return if resolved_or_lexical.starts_with(root) {
            Err(ToolFailure::invalid(
                "argument 'path' must be repository-relative.",
            ))
        } else {
            Err(ToolFailure::outside_root())
        };
    }
    for component in path.components() {
        match component {
            Component::Normal(_) => {}
            Component::ParentDir => return Err(ToolFailure::outside_root()),
            Component::CurDir => {
                return Err(ToolFailure::invalid(
                    "argument 'path' must not contain current-directory components.",
                ));
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(ToolFailure::invalid(
                    "argument 'path' must be repository-relative.",
                ));
            }
        }
    }

    let joined = root.join(path);
    match fs::canonicalize(&joined) {
        Ok(canonical) => {
            if !canonical.starts_with(root) {
                return Err(ToolFailure::outside_root());
            }
            if !canonical.is_file() {
                return Err(ToolFailure::invalid(
                    "argument 'path' must identify a regular file.",
                ));
            }
            Ok(PathState::File)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            ensure_missing_path_inside(root, &joined)?;
            Ok(PathState::Missing)
        }
        Err(_) => Err(ToolFailure::internal()),
    }
}

fn ensure_missing_path_inside(root: &Path, missing: &Path) -> Result<(), ToolFailure> {
    let mut ancestor = missing.parent();
    while let Some(path) = ancestor {
        match fs::canonicalize(path) {
            Ok(canonical) if canonical.starts_with(root) => return Ok(()),
            Ok(_) => return Err(ToolFailure::outside_root()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                ancestor = path.parent();
            }
            Err(_) => return Err(ToolFailure::internal()),
        }
    }
    Err(ToolFailure::outside_root())
}

fn map_atlas_error(error: AtlasError) -> ToolFailure {
    match error {
        AtlasError::OutsideRoot { .. } => ToolFailure::outside_root(),
        AtlasError::FileNotIndexed { path, .. } => ToolFailure::new(
            "not_found",
            format!("File '{path}' is not present in the repository index."),
        ),
        AtlasError::Query(QueryError::InvalidFocus { .. })
        | AtlasError::Query(QueryError::InvalidRange { .. })
        | AtlasError::Query(QueryError::InvalidSymbolRange { .. })
        | AtlasError::Query(QueryError::NotAFile { .. }) => {
            ToolFailure::invalid("The request arguments are invalid.")
        }
        AtlasError::Query(QueryError::NotFound { .. }) => {
            ToolFailure::new("not_found", "The requested file was not found on disk.")
        }
        _ => ToolFailure::internal(),
    }
}

fn parse_symbol_kind(kind: &str) -> Result<SymbolKind, ToolFailure> {
    match kind {
        "function" => Ok(SymbolKind::Function),
        "method" => Ok(SymbolKind::Method),
        "struct" => Ok(SymbolKind::Struct),
        "class" => Ok(SymbolKind::Class),
        "enum" => Ok(SymbolKind::Enum),
        "trait" => Ok(SymbolKind::Trait),
        "interface" => Ok(SymbolKind::Interface),
        "const" => Ok(SymbolKind::Const),
        "type" => Ok(SymbolKind::Type),
        "module" => Ok(SymbolKind::Module),
        _ => Err(ToolFailure::invalid(format!(
            "unknown symbol kind '{kind}'."
        ))),
    }
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
    }
}

fn symbol_value(symbol: &SymbolRecord) -> Value {
    json!({
        "qualified_name": symbol.qualified_name,
        "kind": symbol_kind_name(symbol.kind),
        "signature": symbol.signature,
        "start_line": symbol.start_line,
        "end_line": symbol.end_line,
        "token_estimate": symbol.token_estimate
    })
}

fn navigation_counter() -> &'static dyn TokenCounter {
    counter_for(TokenProvider::OpenAiO200k)
}

fn cap_values(
    values: Vec<Value>,
    max_tokens: usize,
    counter: &dyn TokenCounter,
) -> (Vec<Value>, usize, bool) {
    let total = values.len();
    let mut kept = Vec::new();
    for value in values {
        kept.push(value);
        let rendered = serde_json::to_string(&kept).unwrap_or_else(|_| "[]".to_owned());
        if counter.count(&rendered) > max_tokens {
            kept.pop();
            break;
        }
    }
    let rendered = serde_json::to_string(&kept).unwrap_or_else(|_| "[]".to_owned());
    let tokens_used = counter.count(&rendered);
    let truncated = kept.len() < total;
    (kept, tokens_used, truncated)
}

fn bound_text(
    content: &str,
    max_tokens: usize,
    counter: &dyn TokenCounter,
) -> (String, usize, bool) {
    let full_tokens = counter.count(content);
    if full_tokens <= max_tokens {
        return (content.to_owned(), full_tokens, false);
    }

    let marker = if counter.count(ELISION_MARKER) <= max_tokens {
        ELISION_MARKER
    } else {
        "[elided]"
    };
    let boundaries = std::iter::once(0)
        .chain(content.char_indices().map(|(index, _)| index).skip(1))
        .chain(std::iter::once(content.len()))
        .collect::<Vec<_>>();
    let mut low = 0;
    let mut high = boundaries.len();
    while low + 1 < high {
        let middle = (low + high) / 2;
        let candidate = format!("{}{marker}", &content[..boundaries[middle]]);
        if counter.count(&candidate) <= max_tokens {
            low = middle;
        } else {
            high = middle;
        }
    }
    let mut boundary = boundaries[low];
    let mut bounded = format!("{}{marker}", &content[..boundary]);
    while counter.count(&bounded) > max_tokens && boundary > 0 {
        boundary = content[..boundary]
            .char_indices()
            .next_back()
            .map_or(0, |(index, _)| index);
        bounded = format!("{}{marker}", &content[..boundary]);
    }
    let tokens_used = counter.count(&bounded);
    (bounded, tokens_used, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bound_text_includes_marker_and_respects_budget() {
        let content = "token ".repeat(1_000);
        let counter = navigation_counter();
        let (bounded, used, truncated) = bound_text(&content, 100, counter);
        assert!(truncated);
        assert!(bounded.ends_with(ELISION_MARKER));
        assert_eq!(used, counter.count(&bounded));
        assert!(used <= 100);
    }

    #[test]
    fn test_cap_values_preserves_prefix_under_budget() {
        let counter = navigation_counter();
        let values = (0..100)
            .map(|index| json!({ "qualified_name": format!("symbol_{index}") }))
            .collect::<Vec<_>>();
        let (kept, used, truncated) = cap_values(values, 100, counter);

        assert!(truncated);
        assert!(!kept.is_empty());
        assert_eq!(kept[0]["qualified_name"], "symbol_0");
        assert!(used <= 100);
    }
}
