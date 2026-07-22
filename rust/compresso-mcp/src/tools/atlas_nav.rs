use compresso_atlas::{
    AtlasError, QueryError, UnfoldResult, outline_current, repo_map_focused, repository_stale,
    unfold,
};
use compresso_core::{TokenCounter, operation_identity};
use compresso_store::{Store, SymbolKind, SymbolSelector};
use rmcp::model::JsonObject;
use serde_json::{Value, json};

use super::common::{
    PathState, ToolFailure, ToolResult, cap_values, checked_arguments, language_name,
    map_atlas_error, navigation_counter, optional_string, optional_u32, required_string,
    resolve_repository, symbol_kind_name, symbol_value, token_budget, validate_repo_path,
};

const MAP_DEFAULT_TOKENS: usize = 2_000;
const OUTLINE_DEFAULT_TOKENS: usize = 1_500;
const UNFOLD_DEFAULT_TOKENS: usize = 4_000;
const ELISION_MARKER: &str = "\n[content elided: token budget exceeded]";

pub(crate) fn handles(name: &str) -> bool {
    matches!(
        name,
        "compresso_repo_map" | "compresso_outline" | "compresso_unfold"
    )
}

pub(crate) fn dispatch(store: &Store, name: &str, arguments: Option<JsonObject>) -> ToolResult {
    match name {
        "compresso_repo_map" => repo_map_tool(store, arguments),
        "compresso_outline" => outline_tool(store, arguments),
        "compresso_unfold" => unfold_tool(store, arguments),
        _ => Err(ToolFailure::internal()),
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
        "reindex_hint": stale.then_some(super::common::REINDEX_HINT),
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
