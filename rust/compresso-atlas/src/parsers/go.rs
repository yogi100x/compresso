//! Go symbols and imports (E17-37, D102).
//!
//! Package-level functions, methods (qualified `Receiver::Name`, with the
//! receiver's `*` and type parameters dropped), type declarations (struct,
//! interface, and every other type or alias), package-level `const` and
//! `var` names (both stored as `const`: the store has no variable kind), and
//! import paths. Declarations inside function bodies, interface method
//! elements, struct fields, and the blank identifier `_` are not symbols.

use compresso_store::{SymbolKind, SymbolRecord};
use tree_sitter::Node;

use super::{
    Candidate, CaptureKind, Extraction, ImportKind, line_number, node_text, trimmed_signature,
    unresolved_import,
};

pub(super) fn process_candidate(candidate: Candidate<'_>, source: &str, output: &mut Extraction) {
    let node = candidate.node;
    match candidate.kind {
        CaptureKind::Symbol(SymbolKind::Function) => {
            output.symbols.extend(function(node, source));
        }
        CaptureKind::Symbol(SymbolKind::Method) => output.symbols.extend(method(node, source)),
        CaptureKind::Symbol(SymbolKind::Type) => output.symbols.extend(type_symbol(node, source)),
        CaptureKind::Symbol(SymbolKind::Const) => {
            output.symbols.extend(values(node, "const", source));
        }
        CaptureKind::Variable => output.symbols.extend(values(node, "var", source)),
        CaptureKind::Import(_) => output.imports.extend(import(node, source)),
        CaptureKind::Symbol(_)
        | CaptureKind::ModuleExport
        | CaptureKind::PythonConstant
        | CaptureKind::Use => {}
    }
}

fn function(node: Node<'_>, source: &str) -> Option<SymbolRecord> {
    let name = node_text(node.child_by_field_name("name")?, source)?.to_owned();
    Some(record(
        name.clone(),
        name,
        SymbolKind::Function,
        node,
        body_signature(node, source),
    ))
}

fn method(node: Node<'_>, source: &str) -> Option<SymbolRecord> {
    let name = node_text(node.child_by_field_name("name")?, source)?.to_owned();
    let owner = receiver_type(node, source)?;
    Some(record(
        name.clone(),
        format!("{owner}::{name}"),
        SymbolKind::Method,
        node,
        body_signature(node, source),
    ))
}

/// The receiver's base type name: `(s *Server[T])` gives `Server`.
fn receiver_type(method: Node<'_>, source: &str) -> Option<String> {
    let receiver = method.child_by_field_name("receiver")?;
    let mut cursor = receiver.walk();
    let parameter = receiver
        .named_children(&mut cursor)
        .find(|child| child.kind() == "parameter_declaration")?;
    let text = node_text(parameter.child_by_field_name("type")?, source)?;
    let base = text
        .trim()
        .trim_start_matches(['*', '(', ' '])
        .split(['[', ')'])
        .next()?
        .trim();
    is_identifier(base).then(|| base.to_owned())
}

/// The receiver's name inside `method`, when it has one (`s` in `(s *Server)`).
pub(super) fn receiver_name<'source>(
    method: Node<'_>,
    source: &'source str,
) -> Option<&'source str> {
    let receiver = method.child_by_field_name("receiver")?;
    let mut cursor = receiver.walk();
    let parameter = receiver
        .named_children(&mut cursor)
        .find(|child| child.kind() == "parameter_declaration")?;
    let name = node_text(parameter.child_by_field_name("name")?, source)?;
    (name != "_").then_some(name)
}

fn type_symbol(spec: Node<'_>, source: &str) -> Option<SymbolRecord> {
    let declaration = package_declaration(spec, "type_declaration")?;
    let name = node_text(spec.child_by_field_name("name")?, source)?.to_owned();
    let value = spec.child_by_field_name("type");
    let kind = match (spec.kind(), value.map(|value| value.kind())) {
        ("type_spec", Some("struct_type")) => SymbolKind::Struct,
        ("type_spec", Some("interface_type")) => SymbolKind::Interface,
        _ => SymbolKind::Type,
    };
    // A struct or interface signature stops before its `{`.
    let body_start = value
        .filter(|_| matches!(kind, SymbolKind::Struct | SymbolKind::Interface))
        .and_then(|value| {
            let mut cursor = value.walk();
            value
                .children(&mut cursor)
                .find(|child| matches!(child.kind(), "{" | "field_declaration_list"))
                .map(|brace| brace.start_byte())
        })
        .unwrap_or(spec.end_byte());
    let (range, signature) =
        spec_range_and_signature(declaration, spec, "type", body_start, source);
    Some(record(name.clone(), name, kind, range, signature))
}

/// One symbol per named `const` or `var` at package level; `_` is skipped.
fn values(spec: Node<'_>, keyword: &str, source: &str) -> Vec<SymbolRecord> {
    let declaration_kind = if keyword == "const" {
        "const_declaration"
    } else {
        "var_declaration"
    };
    let Some(declaration) = package_declaration(spec, declaration_kind) else {
        return Vec::new();
    };
    let (range, signature) =
        spec_range_and_signature(declaration, spec, keyword, spec.end_byte(), source);
    let mut cursor = spec.walk();
    spec.children_by_field_name("name", &mut cursor)
        .filter(|name| name.kind() == "identifier")
        .filter_map(|name| node_text(name, source))
        .filter(|name| *name != "_")
        .map(|name| {
            record(
                name.to_owned(),
                name.to_owned(),
                SymbolKind::Const,
                range,
                signature.clone(),
            )
        })
        .collect()
}

/// The declaration wrapping `spec` when it sits at package level. A `var`
/// spec may sit inside a parenthesized `var_spec_list`.
fn package_declaration<'tree>(spec: Node<'tree>, kind: &str) -> Option<Node<'tree>> {
    let mut declaration = spec.parent()?;
    if declaration.kind() == "var_spec_list" {
        declaration = declaration.parent()?;
    }
    (declaration.kind() == kind
        && declaration
            .parent()
            .is_some_and(|parent| parent.kind() == "source_file"))
    .then_some(declaration)
}

/// A declaration with one spec is the symbol's whole range (`type X int`); a
/// spec in a parenthesized group is its own range, and its signature is
/// prefixed with the group's keyword.
fn spec_range_and_signature<'tree>(
    declaration: Node<'tree>,
    spec: Node<'tree>,
    keyword: &str,
    end_byte: usize,
    source: &str,
) -> (Node<'tree>, Option<String>) {
    let grouped = {
        let mut cursor = declaration.walk();
        declaration
            .children(&mut cursor)
            .any(|child| matches!(child.kind(), "(" | "var_spec_list"))
    };
    if grouped {
        let text = source.get(spec.start_byte()..end_byte).unwrap_or_default();
        (
            spec,
            trimmed_signature(&format!("{keyword} {}", text.trim())),
        )
    } else {
        let text = source
            .get(declaration.start_byte()..end_byte)
            .unwrap_or_default();
        (declaration, trimmed_signature(text))
    }
}

fn body_signature(node: Node<'_>, source: &str) -> Option<String> {
    let end_byte = node
        .child_by_field_name("body")
        .map_or(node.end_byte(), |body| body.start_byte());
    trimmed_signature(source.get(node.start_byte()..end_byte)?)
}

fn import(spec: Node<'_>, source: &str) -> Option<super::ImportRecord> {
    let path = node_text(spec.child_by_field_name("path")?, source)?;
    let bytes = path.as_bytes();
    let raw = match (bytes.first(), bytes.last()) {
        (Some(b'"'), Some(b'"')) | (Some(b'`'), Some(b'`')) if bytes.len() >= 2 => {
            &path[1..path.len() - 1]
        }
        _ => return None,
    };
    (!raw.trim().is_empty()).then(|| unresolved_import(raw.to_owned(), ImportKind::Import))
}

fn record(
    name: String,
    qualified_name: String,
    kind: SymbolKind,
    range: Node<'_>,
    signature: Option<String>,
) -> SymbolRecord {
    SymbolRecord {
        name,
        qualified_name,
        kind,
        signature,
        start_line: line_number(range.start_position().row),
        end_line: line_number(range.end_position().row),
        token_estimate: 0,
    }
}

fn is_identifier(text: &str) -> bool {
    !text.is_empty()
        && text
            .chars()
            .all(|character| character == '_' || character.is_alphanumeric())
}
