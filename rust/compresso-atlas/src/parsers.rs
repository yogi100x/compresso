use std::collections::HashSet;
use std::path::Path;

use compresso_store::{SymbolKind, SymbolRecord};
use tree_sitter::{Node, Parser, Query, QueryCursor, StreamingIterator};

const RUST_QUERY: &str = include_str!("../queries/rust.scm");
const NOT_INDEXED: &str = "not_indexed";

pub type Language = compresso_store::FileLanguage;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ImportKind {
    Use,
    Import,
    ExportFrom,
    Require,
    FromImport,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ImportResolution {
    Resolved,
    Ambiguous(Vec<String>),
    Unresolved(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportRecord {
    pub raw: String,
    pub kind: ImportKind,
    pub resolved_path: Option<String>,
    pub resolution: ImportResolution,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Extraction {
    pub symbols: Vec<SymbolRecord>,
    pub imports: Vec<ImportRecord>,
}

#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("language {0:?} is not implemented by this parser registry")]
    UnsupportedLanguage(Language),
    #[error("failed to configure the Rust parser: {0}")]
    Language(String),
    #[error("invalid Rust extraction query: {0}")]
    Query(String),
    #[error("parser did not return a syntax tree")]
    ParseFailed,
}

pub struct ParserRegistry;

impl ParserRegistry {
    pub fn for_path(path: &Path) -> Option<Language> {
        let extension = path.extension()?.to_str()?;
        match extension {
            "rs" => Some(Language::Rust),
            "ts" => Some(Language::TypeScript),
            "tsx" => Some(Language::Tsx),
            "js" | "jsx" | "mjs" | "cjs" => Some(Language::JavaScript),
            "py" => Some(Language::Python),
            _ => None,
        }
    }

    pub fn extract(lang: Language, source: &str) -> Result<Extraction, ParseError> {
        match lang {
            Language::Rust => extract_rust(source),
            Language::TypeScript | Language::Tsx | Language::JavaScript | Language::Python => {
                Err(ParseError::UnsupportedLanguage(lang))
            }
        }
    }
}

#[derive(Clone, Copy)]
enum CaptureKind {
    Symbol(SymbolKind),
    Use,
}

#[derive(Clone, Copy)]
struct Candidate<'tree> {
    node: Node<'tree>,
    kind: CaptureKind,
}

fn extract_rust(source: &str) -> Result<Extraction, ParseError> {
    let language = tree_sitter_rust::LANGUAGE.into();
    let mut parser = Parser::new();
    parser
        .set_language(&language)
        .map_err(|error| ParseError::Language(error.to_string()))?;
    let tree = parser.parse(source, None).ok_or(ParseError::ParseFailed)?;
    let query =
        Query::new(&language, RUST_QUERY).map_err(|error| ParseError::Query(error.to_string()))?;
    let capture_names = query.capture_names();
    let mut cursor = QueryCursor::new();
    let mut query_matches = cursor.matches(&query, tree.root_node(), source.as_bytes());
    let mut candidates = Vec::new();

    while let Some(query_match) = query_matches.next() {
        for capture in query_match.captures {
            let Some(kind) = capture_kind(capture_names[capture.index as usize]) else {
                continue;
            };
            candidates.push(Candidate {
                node: capture.node,
                kind,
            });
        }
    }

    candidates.sort_by_key(|candidate| {
        (
            candidate.node.start_byte(),
            candidate.node.end_byte(),
            capture_priority(candidate.kind),
        )
    });
    let mut seen_ranges = HashSet::new();
    candidates.retain(|candidate| {
        seen_ranges.insert((candidate.node.start_byte(), candidate.node.end_byte()))
    });

    let mut symbols = Vec::new();
    let mut imports = Vec::new();
    for candidate in candidates {
        match candidate.kind {
            CaptureKind::Symbol(kind) => {
                if let Some(symbol) = rust_symbol(candidate.node, kind, source) {
                    symbols.push(symbol);
                }
            }
            CaptureKind::Use => {
                imports.extend(rust_imports(candidate.node, source));
            }
        }
    }

    Ok(Extraction { symbols, imports })
}

fn capture_kind(name: &str) -> Option<CaptureKind> {
    match name {
        "definition.function" => Some(CaptureKind::Symbol(SymbolKind::Function)),
        "definition.struct" => Some(CaptureKind::Symbol(SymbolKind::Struct)),
        "definition.enum" => Some(CaptureKind::Symbol(SymbolKind::Enum)),
        "definition.trait" => Some(CaptureKind::Symbol(SymbolKind::Trait)),
        "definition.const" => Some(CaptureKind::Symbol(SymbolKind::Const)),
        "definition.type" => Some(CaptureKind::Symbol(SymbolKind::Type)),
        "definition.module" => Some(CaptureKind::Symbol(SymbolKind::Module)),
        "import.use" => Some(CaptureKind::Use),
        _ => None,
    }
}

const fn capture_priority(kind: CaptureKind) -> u8 {
    match kind {
        CaptureKind::Symbol(_) => 0,
        CaptureKind::Use => 1,
    }
}

fn rust_symbol(node: Node<'_>, mut kind: SymbolKind, source: &str) -> Option<SymbolRecord> {
    let name_node = node.child_by_field_name("name")?;
    let name = node_text(name_node, source)?.to_owned();
    let qualified_name = if kind == SymbolKind::Function {
        if let Some(owner) = implementation_owner(node, source) {
            kind = SymbolKind::Method;
            format!("{owner}::{name}")
        } else {
            name.clone()
        }
    } else {
        name.clone()
    };
    let signature = symbol_signature(node, source);

    Some(SymbolRecord {
        name,
        qualified_name,
        kind,
        signature,
        start_line: line_number(node.start_position().row),
        end_line: line_number(node.end_position().row),
        token_estimate: 0,
    })
}

fn implementation_owner(function: Node<'_>, source: &str) -> Option<String> {
    let declaration_list = function.parent()?;
    if declaration_list.kind() != "declaration_list" {
        return None;
    }
    let implementation = declaration_list.parent()?;
    if implementation.kind() != "impl_item" {
        return None;
    }
    let owner = implementation.child_by_field_name("type")?;
    Some(node_text(owner, source)?.trim().to_owned())
}

fn symbol_signature(node: Node<'_>, source: &str) -> Option<String> {
    let end_byte = node
        .child_by_field_name("body")
        .or_else(|| direct_body_child(node))
        .map_or(node.end_byte(), |body| body.start_byte());
    let signature = source.get(node.start_byte()..end_byte)?.trim();
    let signature = signature.strip_suffix(';').unwrap_or(signature).trim_end();
    (!signature.is_empty()).then(|| signature.to_owned())
}

fn direct_body_child(node: Node<'_>) -> Option<Node<'_>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).find(|child| {
        matches!(
            child.kind(),
            "block" | "declaration_list" | "enum_variant_list" | "field_declaration_list"
        )
    })
}

fn rust_imports(node: Node<'_>, source: &str) -> Vec<ImportRecord> {
    let Some(argument) = node
        .child_by_field_name("argument")
        .and_then(|argument| node_text(argument, source))
    else {
        return Vec::new();
    };
    let mut expanded = Vec::new();
    expand_use_fragment(argument, "", &mut expanded);
    expanded
        .into_iter()
        .filter(|raw| !raw.is_empty())
        .map(|raw| ImportRecord {
            raw,
            kind: ImportKind::Use,
            resolved_path: None,
            resolution: ImportResolution::Unresolved(NOT_INDEXED.to_owned()),
        })
        .collect()
}

fn expand_use_fragment(fragment: &str, base: &str, expanded: &mut Vec<String>) {
    let fragment = fragment.trim();
    if let Some((open, close)) = outer_brace_range(fragment) {
        let local_base = join_use_path(base, fragment[..open].trim_end_matches(':').trim());
        for item in split_group_items(&fragment[open + 1..close]) {
            expand_use_fragment(item, &local_base, expanded);
        }
        return;
    }

    let fragment = strip_alias(fragment);
    if fragment == "self" {
        if !base.is_empty() {
            expanded.push(base.to_owned());
        }
        return;
    }
    expanded.push(join_use_path(base, fragment));
}

fn outer_brace_range(fragment: &str) -> Option<(usize, usize)> {
    let mut depth = 0_u32;
    let mut open = None;
    for (index, character) in fragment.char_indices() {
        match character {
            '{' => {
                if depth == 0 {
                    open = Some(index);
                }
                depth += 1;
            }
            '}' if depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    return open.map(|open| (open, index));
                }
            }
            _ => {}
        }
    }
    None
}

fn split_group_items(group: &str) -> Vec<&str> {
    let mut items = Vec::new();
    let mut depth = 0_u32;
    let mut start = 0;
    for (index, character) in group.char_indices() {
        match character {
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                items.push(&group[start..index]);
                start = index + character.len_utf8();
            }
            _ => {}
        }
    }
    items.push(&group[start..]);
    items
}

fn strip_alias(fragment: &str) -> &str {
    fragment
        .split_once(" as ")
        .map_or(fragment, |(path, _)| path)
        .trim()
}

fn join_use_path(base: &str, suffix: &str) -> String {
    let suffix = suffix.trim();
    if base.is_empty() {
        normalize_use_path(suffix)
    } else if suffix.is_empty() {
        base.to_owned()
    } else {
        format!("{base}::{}", normalize_use_path(suffix))
    }
}

fn normalize_use_path(path: &str) -> String {
    path.split("::")
        .map(str::trim)
        .collect::<Vec<_>>()
        .join("::")
}

fn node_text<'source>(node: Node<'_>, source: &'source str) -> Option<&'source str> {
    source.get(node.start_byte()..node.end_byte())
}

fn line_number(zero_based_row: usize) -> u32 {
    u32::try_from(zero_based_row)
        .unwrap_or(u32::MAX - 1)
        .saturating_add(1)
}
