use std::fs;
use std::path::{Component, Path};

use compresso_atlas::{AtlasError, QueryError};
use compresso_core::{TokenCounter, TokenProvider, counter_for};
use compresso_store::{FileLanguage, RepoRecord, Store, SymbolKind, SymbolRecord};
use rmcp::model::{CallToolResult, ContentBlock, JsonObject};
use serde_json::{Map, Value, json};

pub(crate) const MIN_TOKENS: usize = 100;
pub(crate) const MAX_TOKENS: usize = 100_000;
pub(crate) const REINDEX_HINT: &str = "Repository index is stale; run `compresso index` and retry.";

pub(crate) type ToolResult = Result<Value, ToolFailure>;

pub(crate) struct ToolFailure {
    code: &'static str,
    message: String,
}

impl ToolFailure {
    pub(crate) fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub(crate) const fn code(&self) -> &'static str {
        self.code
    }

    pub(crate) fn message(&self) -> &str {
        &self.message
    }

    pub(crate) fn invalid(message: impl Into<String>) -> Self {
        Self::new("invalid_arguments", message)
    }

    pub(crate) fn outside_root() -> Self {
        Self::new(
            "outside_root",
            "The requested path resolves outside the approved repository root.",
        )
    }

    pub(crate) fn internal() -> Self {
        Self::new(
            "internal",
            "Compresso could not complete the request because of an internal error.",
        )
    }

    pub(crate) fn into_result(self) -> CallToolResult {
        CallToolResult::error(vec![ContentBlock::text(format!(
            "[{}] {}",
            self.code, self.message
        ))])
    }
}

/// A successful result: one text block holding the JSON document or the
/// compact text form.
pub(crate) fn success_text(text: String) -> CallToolResult {
    CallToolResult::success(vec![ContentBlock::text(text)])
}

/// Records the call's effective `max_tokens` in `payload` so the dispatcher
/// can hold the text form to it. The key never reaches a client.
pub(crate) fn with_budget(mut payload: Value, max_tokens: usize) -> Value {
    if let Some(object) = payload.as_object_mut() {
        object.insert(super::BUDGET_KEY.to_owned(), Value::from(max_tokens));
    }
    payload
}

pub(crate) fn checked_arguments(
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

pub(crate) fn required_string(
    arguments: &mut Map<String, Value>,
    name: &'static str,
) -> Result<String, ToolFailure> {
    optional_string(arguments, name, false)?
        .ok_or_else(|| ToolFailure::invalid(format!("missing required string argument '{name}'.")))
}

pub(crate) fn optional_string(
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

pub(crate) fn optional_bool(
    arguments: &mut Map<String, Value>,
    name: &'static str,
) -> Result<Option<bool>, ToolFailure> {
    let Some(value) = arguments.remove(name) else {
        return Ok(None);
    };
    value
        .as_bool()
        .map(Some)
        .ok_or_else(|| ToolFailure::invalid(format!("argument '{name}' must be a boolean.")))
}

pub(crate) fn token_budget(
    arguments: &mut Map<String, Value>,
    default: usize,
) -> Result<usize, ToolFailure> {
    optional_usize(arguments, "max_tokens", MIN_TOKENS, MAX_TOKENS)
        .map(|value| value.unwrap_or(default))
}

pub(crate) fn optional_usize(
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

pub(crate) fn optional_u32(
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

pub(crate) fn resolve_repository(
    store: &Store,
    session_root: Option<&Path>,
    supplied_root: Option<&str>,
) -> Result<RepoRecord, ToolFailure> {
    let Some(session_root) = session_root else {
        return if supplied_root.is_some() {
            Err(ToolFailure::outside_root())
        } else {
            Err(unresolved_repository())
        };
    };

    let canonical_session_root = match fs::canonicalize(session_root) {
        Ok(canonical) if canonical.is_dir() && canonical == session_root => canonical,
        _ => return Err(ToolFailure::outside_root()),
    };

    if let Some(supplied_root) = supplied_root {
        let supplied_root = Path::new(supplied_root);
        if !supplied_root.is_absolute() {
            return Err(ToolFailure::invalid(
                "argument 'root' must be an absolute path.",
            ));
        }
        let canonical = match fs::canonicalize(supplied_root) {
            Ok(canonical) if canonical.is_dir() => canonical,
            _ => return Err(ToolFailure::outside_root()),
        };
        if canonical != canonical_session_root {
            return Err(ToolFailure::outside_root());
        }
    }

    let repository = store
        .repo_by_root(&canonical_session_root)
        .map_err(|_| ToolFailure::internal())?;
    let Some(repository) = repository else {
        return Err(unresolved_repository());
    };
    indexed_repository(repository)
}

fn unresolved_repository() -> ToolFailure {
    ToolFailure::new(
        "not_indexed",
        "No indexed repository is bound to this MCP session. Start Compresso from an approved repository or pass `--root` when starting the server.",
    )
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
pub(crate) enum PathState {
    File,
    Missing,
}

pub(crate) fn validate_repo_path(root: &Path, requested: &str) -> Result<PathState, ToolFailure> {
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

pub(crate) fn map_atlas_error(error: AtlasError) -> ToolFailure {
    match error {
        AtlasError::OutsideRoot { .. } => ToolFailure::outside_root(),
        AtlasError::FileNotIndexed { path, .. } => ToolFailure::new(
            "not_found",
            format!("File '{path}' is not present in the repository index."),
        ),
        AtlasError::Query(error @ QueryError::InvalidSearchQuery { .. }) => {
            ToolFailure::invalid(format!("argument 'query' is invalid: {error}."))
        }
        AtlasError::Query(error @ QueryError::InvalidReadRanges { .. }) => {
            ToolFailure::invalid(format!("argument 'ranges' is invalid: {error}."))
        }
        AtlasError::Query(QueryError::InvalidSearchPath { .. }) => ToolFailure::invalid(
            "argument 'path' must be a repository-relative path without '..' or backslashes.",
        ),
        AtlasError::Query(QueryError::InvalidFocus { .. })
        | AtlasError::Query(QueryError::InvalidRange { .. })
        | AtlasError::Query(QueryError::InvalidSymbolRange { .. })
        | AtlasError::Query(QueryError::NotAFile { .. }) => {
            ToolFailure::invalid("The request arguments are invalid.")
        }
        AtlasError::Query(QueryError::Read { source, .. })
            if source.kind() == std::io::ErrorKind::InvalidData =>
        {
            ToolFailure::invalid("The requested file is not valid UTF-8 text.")
        }
        AtlasError::Query(QueryError::NotFound { .. }) => {
            ToolFailure::new("not_found", "The requested file was not found on disk.")
        }
        AtlasError::Query(QueryError::FileTooLarge { limit_bytes, .. }) => ToolFailure::new(
            "file_too_large",
            format!(
                "The requested file exceeds the {} MiB read limit and was not loaded.",
                limit_bytes / (1024 * 1024)
            ),
        ),
        _ => ToolFailure::internal(),
    }
}

pub(crate) const fn symbol_kind_name(kind: SymbolKind) -> &'static str {
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

pub(crate) const fn language_name(language: FileLanguage) -> &'static str {
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

pub(crate) fn symbol_value(symbol: &SymbolRecord) -> Value {
    json!({
        "qualified_name": symbol.qualified_name,
        "kind": symbol_kind_name(symbol.kind),
        "signature": symbol.signature,
        "start_line": symbol.start_line,
        "end_line": symbol.end_line,
        "token_estimate": symbol.token_estimate
    })
}

pub(crate) fn navigation_counter() -> &'static dyn TokenCounter {
    counter_for(TokenProvider::OpenAiO200k)
}

pub(crate) fn cap_values(
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

/// [`cap_values`] measured in the text form: the kept values rendered as one
/// `row` per line.
pub(crate) fn cap_rows(
    values: Vec<Value>,
    max_tokens: usize,
    counter: &dyn TokenCounter,
    row: fn(&Value) -> String,
) -> (Vec<Value>, usize, bool) {
    let total = values.len();
    let mut kept = Vec::new();
    let mut rendered = String::new();
    let mut tokens_used = 0;
    for value in values {
        let mut candidate = rendered.clone();
        candidate.push_str(&row(&value));
        candidate.push('\n');
        let tokens = counter.count(&candidate);
        if tokens > max_tokens {
            break;
        }
        rendered = candidate;
        tokens_used = tokens;
        kept.push(value);
    }
    let truncated = kept.len() < total;
    (kept, tokens_used, truncated)
}
