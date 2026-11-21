//! Go symbols and imports (E17-37, D102).
//!
//! Package-level functions, methods (qualified `Receiver::Name`, with the
//! receiver's `*` and type parameters dropped), type declarations (struct,
//! interface, and every other type or alias), package-level `const` and
//! `var` names (both stored as `const`: the store has no variable kind), and
//! import paths. Declarations inside function bodies, interface method
//! elements, struct fields, and the blank identifier `_` are not symbols.
//! Signatures are capped at [`super::MAX_SIGNATURE_BYTES`].

use compresso_store::{SymbolKind, SymbolRecord};
use tree_sitter::Node;

use super::{
    Candidate, CaptureKind, Extraction, ImportKind, bounded_signature, line_number, node_text,
    unresolved_import,
};

pub(super) fn process_candidate(candidate: Candidate<'_>, source: &str, output: &mut Extraction) {
    let node = candidate.node;
    match candidate.kind {
        CaptureKind::Symbol(SymbolKind::Function) => {
            output.symbols.extend(function(node, source));
        }
        CaptureKind::Symbol(SymbolKind::Method) => output.symbols.extend(method(node, source)),
        CaptureKind::Symbol(SymbolKind::Type) => types(node, source, &mut output.symbols),
        CaptureKind::Symbol(SymbolKind::Const) => {
            values(node, "const", source, &mut output.symbols);
        }
        CaptureKind::Variable => values(node, "var", source, &mut output.symbols),
        CaptureKind::Import(_) => output.imports.extend(import(node, source)),
        CaptureKind::Symbol(_)
        | CaptureKind::ModuleExport
        | CaptureKind::PythonConstant
        | CaptureKind::Use => {}
    }
}

fn function(node: Node<'_>, source: &str) -> Option<SymbolRecord> {
    let name = symbol_name(node.child_by_field_name("name")?, source)?;
    Some(record(
        name.clone(),
        name,
        SymbolKind::Function,
        node,
        body_signature(node, source),
    ))
}

fn method(node: Node<'_>, source: &str) -> Option<SymbolRecord> {
    let name = symbol_name(node.child_by_field_name("name")?, source)?;
    let owner = receiver_type(node, source)?;
    Some(record(
        name.clone(),
        format!("{owner}::{name}"),
        SymbolKind::Method,
        node,
        body_signature(node, source),
    ))
}

/// A declared name, unless it is the blank identifier.
fn symbol_name(node: Node<'_>, source: &str) -> Option<String> {
    let name = node_text(node, source)?;
    (name != "_").then(|| name.to_owned())
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

/// The specs of one package-level declaration (the query anchors it under
/// `source_file`), each with the declaration's range when it is the only
/// spec, or its own range inside a parenthesized group. Children are walked
/// once per declaration, so a group of any size costs linear time.
fn specs<'tree>(declaration: Node<'tree>, spec_kinds: &[&str]) -> (bool, Vec<Node<'tree>>) {
    let mut grouped = false;
    let mut specs = Vec::new();
    let mut cursor = declaration.walk();
    for child in declaration.children(&mut cursor) {
        match child.kind() {
            "(" => grouped = true,
            "var_spec_list" => {
                grouped = true;
                let mut inner = child.walk();
                specs.extend(
                    child
                        .named_children(&mut inner)
                        .filter(|spec| spec_kinds.contains(&spec.kind())),
                );
            }
            kind if spec_kinds.contains(&kind) => specs.push(child),
            _ => {}
        }
    }
    (grouped, specs)
}

fn types(declaration: Node<'_>, source: &str, output: &mut Vec<SymbolRecord>) {
    let (grouped, specs) = specs(declaration, &["type_spec", "type_alias"]);
    for spec in specs {
        let Some(name) = spec
            .child_by_field_name("name")
            .and_then(|name| symbol_name(name, source))
        else {
            continue;
        };
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
        let (range, start) = if grouped {
            (spec, spec.start_byte())
        } else {
            (declaration, declaration.start_byte())
        };
        let text = source.get(start..body_start).unwrap_or_default();
        let signature = if grouped {
            bounded_signature(&format!("type {}", text.trim()))
        } else {
            bounded_signature(text)
        };
        output.push(record(name.clone(), name, kind, range, signature));
    }
}

/// One symbol per named `const` or `var` at package level; `_` is skipped.
/// A spec that declares one name keeps its whole text as the signature; a
/// spec of several names gives each `keyword name type`, so the signatures
/// of one spec never repeat its text once per name.
fn values(declaration: Node<'_>, keyword: &str, source: &str, output: &mut Vec<SymbolRecord>) {
    let spec_kind = if keyword == "const" {
        "const_spec"
    } else {
        "var_spec"
    };
    let (grouped, specs) = specs(declaration, &[spec_kind]);
    for spec in specs {
        let range = if grouped { spec } else { declaration };
        let mut cursor = spec.walk();
        let names = spec
            .children_by_field_name("name", &mut cursor)
            .filter(|name| name.kind() == "identifier")
            .collect::<Vec<_>>();
        let whole = if names.len() == 1 {
            let text = if grouped {
                format!(
                    "{keyword} {}",
                    node_text(spec, source).unwrap_or_default().trim()
                )
            } else {
                node_text(declaration, source)
                    .unwrap_or_default()
                    .to_owned()
            };
            bounded_signature(&text)
        } else {
            None
        };
        let type_text = spec
            .child_by_field_name("type")
            .and_then(|kind| node_text(kind, source));
        let type_cut = type_text.is_some_and(super::is_cut);
        let type_text = type_text.map(super::signature_prefix);
        for name in names {
            let Some(name) = symbol_name(name, source) else {
                continue;
            };
            let signature = whole.clone().or_else(|| {
                super::marked_signature(
                    &super::per_name_text(match type_text {
                        Some(type_text) => format!("{keyword} {name} {type_text}"),
                        None => format!("{keyword} {name}"),
                    }),
                    type_cut,
                )
            });
            output.push(record(
                name.clone(),
                name,
                SymbolKind::Const,
                range,
                signature,
            ));
        }
    }
}

fn body_signature(node: Node<'_>, source: &str) -> Option<String> {
    let end_byte = node
        .child_by_field_name("body")
        .map_or(node.end_byte(), |body| body.start_byte());
    bounded_signature(source.get(node.start_byte()..end_byte)?)
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

/// The name a Go import is used by when it has no alias: the last path
/// segment, skipping a major-version segment (`…/chi/v5` gives `chi`) and a
/// `.vN` suffix (`gopkg.in/yaml.v3` gives `yaml`), with `-` read as `_`.
pub(crate) fn package_local_name(import: &str) -> String {
    let mut segments = import.rsplit('/');
    let mut last = segments.next().unwrap_or(import);
    let is_version = |segment: &str| {
        segment
            .strip_prefix('v')
            .is_some_and(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
    };
    if is_version(last)
        && let Some(previous) = segments.next()
    {
        last = previous;
    }
    if let Some((stem, version)) = last.rsplit_once('.')
        && is_version(version)
    {
        last = stem;
    }
    last.replace('-', "_")
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

#[cfg(test)]
mod tests {
    use super::package_local_name;

    #[test]
    fn test_package_local_name_skips_versions_and_reads_dashes_as_underscores() {
        assert_eq!(package_local_name("fmt"), "fmt");
        assert_eq!(package_local_name("encoding/json"), "json");
        assert_eq!(package_local_name("github.com/go-chi/chi/v5"), "chi");
        assert_eq!(package_local_name("gopkg.in/yaml.v3"), "yaml");
        assert_eq!(package_local_name("example.com/go-kit"), "go_kit");
    }
}
