use compresso_atlas::{
    AtlasError, CardOptions, DEFAULT_MAX_CARDS, QueryError, UnfoldResult, module_cards,
    other_sql_definitions, outline_current, repo_map_focused, repository_stale, unfold,
};
use compresso_core::{TokenCounter, operation_identity};
use compresso_store::{Store, SymbolKind, SymbolSelector};
use rmcp::model::JsonObject;
use serde_json::{Value, json};
use std::path::Path;

use super::Form;
use super::common::{
    PathState, ToolFailure, ToolResult, checked_arguments, language_name, map_atlas_error,
    navigation_counter, optional_bool, optional_string, optional_u32, required_string,
    resolve_repository, symbol_kind_name, symbol_value, token_budget, validate_repo_path,
    with_budget,
};
use super::text;

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

pub(crate) fn dispatch(
    store: &Store,
    repository_root: Option<&Path>,
    name: &str,
    arguments: Option<JsonObject>,
    form: Form,
) -> ToolResult {
    match name {
        "compresso_repo_map" => repo_map_tool(store, repository_root, arguments, form),
        "compresso_outline" => outline_tool(store, repository_root, arguments, form),
        "compresso_unfold" => unfold_tool(store, repository_root, arguments, form),
        _ => Err(ToolFailure::internal()),
    }
}

fn repo_map_tool(
    store: &Store,
    repository_root: Option<&Path>,
    arguments: Option<JsonObject>,
    form: Form,
) -> ToolResult {
    let mut arguments =
        checked_arguments(arguments, &["root", "focus", "max_tokens", "cards", "path"])?;
    let root = optional_string(&mut arguments, "root", false)?;
    let focus = optional_string(&mut arguments, "focus", true)?;
    let max_tokens = token_budget(&mut arguments, MAP_DEFAULT_TOKENS)?;
    let cards_requested = optional_bool(&mut arguments, "cards")?;
    let cards_path = optional_string(&mut arguments, "path", false)?;
    // A card zoom path implies cards; `cards: false` with a path is contradictory.
    let with_cards = match (cards_requested, cards_path.is_some()) {
        (Some(false), true) => {
            return Err(ToolFailure::invalid(
                "argument 'path' zooms module cards and requires 'cards' to be true or omitted.",
            ));
        }
        (requested, has_path) => requested.unwrap_or(false) || has_path,
    };
    let repository = resolve_repository(store, repository_root, root.as_deref())?;
    let content_budget = form.content_budget(max_tokens);

    // Cards take up to three quarters of the budget; the map keeps the rest.
    let cards = with_cards
        .then(|| {
            module_cards(
                store,
                repository.id,
                &repository.root,
                CardOptions {
                    path: cards_path.as_deref(),
                    max_cards: DEFAULT_MAX_CARDS,
                    budget_tokens: content_budget.saturating_mul(3) / 4,
                },
            )
        })
        .transpose()
        .map_err(map_atlas_error)?;
    let map_budget =
        content_budget.saturating_sub(cards.as_ref().map_or(0, |cards| cards.tokens_used));
    let map = repo_map_focused(store, repository.id, focus.as_deref(), map_budget)
        .map_err(map_atlas_error)?;
    let stale = repository_stale(store, repository.id).map_err(map_atlas_error)?;
    let operation_id = operation_identity().id;
    let mut payload = json!({
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
    });
    if let (Some(cards), Some(object)) = (cards, payload.as_object_mut()) {
        // Additive fields, present only when cards were requested.
        object.insert(
            "cards".to_owned(),
            json!(
                cards
                    .cards
                    .iter()
                    .map(|card| card.rendered.as_str())
                    .collect::<Vec<_>>()
            ),
        );
        object.insert("cards_total".to_owned(), json!(cards.cards_total));
        object.insert("cards_shown".to_owned(), json!(cards.cards.len()));
        object.insert("cards_tokens_used".to_owned(), json!(cards.tokens_used));
        object.insert("cards_truncated".to_owned(), json!(cards.truncated));
    }
    Ok(with_budget(payload, max_tokens))
}

fn outline_tool(
    store: &Store,
    repository_root: Option<&Path>,
    arguments: Option<JsonObject>,
    form: Form,
) -> ToolResult {
    let mut arguments = checked_arguments(arguments, &["root", "path", "max_tokens"])?;
    let root = optional_string(&mut arguments, "root", false)?;
    let path = required_string(&mut arguments, "path")?;
    let max_tokens = token_budget(&mut arguments, OUTLINE_DEFAULT_TOKENS)?;
    let repository = resolve_repository(store, repository_root, root.as_deref())?;
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
    let (symbols, tokens_used, truncated) =
        form.cap(all_symbols, max_tokens, counter, text::outline_row);
    let elision = truncated.then(|| {
        format!(
            "[{} symbols elided: token budget exceeded]",
            current.outline.symbols.len().saturating_sub(symbols.len())
        )
    });
    let operation_id = operation_identity().id;
    let payload = json!({
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
    });
    Ok(with_budget(payload, max_tokens))
}

fn unfold_tool(
    store: &Store,
    repository_root: Option<&Path>,
    arguments: Option<JsonObject>,
    form: Form,
) -> ToolResult {
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
    let repository = resolve_repository(store, repository_root, root.as_deref())?;
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

    let payload = match result {
        UnfoldResult::Found {
            path,
            symbol,
            content,
            stale,
            reindex_hint,
        } => {
            let (content, tokens_used, truncated) =
                bound_text(&content, form.content_budget(max_tokens), counter);
            // E17-42 (D107): a SQL definition lists the other definitions of
            // its name in path order, last first; every other language lists
            // none.
            // They are listed only within what the token budget leaves after
            // the content, so they never push a result past `max_tokens`;
            // `truncated` speaks for the content only, and `elision` says
            // how many of the other definitions are listed.
            let mut tokens_used = tokens_used;
            let mut others = Vec::new();
            let (definitions, total) = other_sql_definitions(store, repository.id, &path, &symbol)
                .map_err(map_atlas_error)?;
            for candidate in definitions {
                let row = json!({
                    "path": candidate.path,
                    "qualified_name": candidate.qualified_name,
                    "kind": symbol_kind_name(candidate.kind),
                    "signature_hash": candidate.signature_hash,
                    "start_line": candidate.start_line,
                    "end_line": candidate.end_line
                });
                let cost = counter.count(&row.to_string());
                if tokens_used.saturating_add(cost) > max_tokens {
                    break;
                }
                tokens_used += cost;
                others.push(row);
            }
            let elision = (others.len() < total).then(|| {
                format!(
                    "[{total} other definitions in path order, last first; {} shown]",
                    others.len()
                )
            });
            let mut payload = json!({
                "schema_version": 1,
                "operation_id": operation_id,
                "status": "found",
                "path": path,
                "qualified_name": selector.qualified_name,
                "symbol": symbol_value(&symbol),
                "content": content,
                "stale": stale,
                "reindex_hint": reindex_hint,
                "candidates": others,
                "tokens_used": tokens_used,
                "token_provider": counter.provider(),
                "truncated": truncated
            });
            if let Some(elision) = elision {
                payload["elision"] = json!(elision);
            }
            payload
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
                form.cap(all_candidates, max_tokens, counter, text::candidate_row);
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
    };
    Ok(with_budget(payload, max_tokens))
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
    use super::super::common::cap_values;
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
