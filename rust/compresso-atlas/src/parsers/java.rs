//! Java symbols and imports (E17-40, D105).
//!
//! The `package` declaration (a `module` symbol named by its dotted name);
//! classes, records (`class`), interfaces and annotation types
//! (`interface`), and enums, with nested types qualified through their
//! outer types (`Outer::Inner`); methods, constructors (`Type::Type`), and
//! annotation elements as methods; fields, interface constants, and enum
//! constants as `const` (the store has no field kind); and import names as
//! written (`a.b.C`, `a.b.*`, `a.b.C.member` for a static import). Type
//! parameters are never part of a name. Local and anonymous classes,
//! lambdas, initializer blocks, and declarations inside method bodies are
//! not symbols. One iterative pass over the declaration levels only: method
//! bodies are never entered, nesting cannot exhaust the stack, and a type
//! nested deeper than [`super::declarations::MAX_TYPE_DEPTH`] or with a
//! qualified name over [`super::declarations::MAX_QUALIFIED_NAME_BYTES`] is
//! skipped with its members. Error-recovery nodes are dropped.

use compresso_store::SymbolKind;
use tree_sitter::Node;

use super::declarations::{Found, Owner, dotted_text, is_identifier};
use super::{Extraction, ImportKind, bounded_signature, node_text, unresolved_import};

pub(super) fn walk(
    root: Node<'_>,
    source: &str,
    output: &mut Extraction,
    symbol_bytes: &mut Vec<Option<(usize, usize)>>,
) {
    let mut found = Found::default();
    // (container, owner): the program, or the body of a type.
    let mut stack = vec![(root, None::<Owner>)];
    while let Some((container, owner)) = stack.pop() {
        let mut cursor = container.walk();
        for child in container.named_children(&mut cursor) {
            if child.is_missing() {
                continue;
            }
            // Error recovery can wrap complete declarations: their level is
            // the one around the error node, and a garbled name is dropped
            // when it is read.
            if child.is_error() {
                stack.push((child, owner.clone()));
                continue;
            }
            match (child.kind(), owner.as_ref()) {
                ("package_declaration", None) => {
                    if let Some(name) = package_name(child, source) {
                        found.module(child, name.clone(), format!("package {name}"));
                    }
                }
                ("import_declaration", None) => {
                    if let Some(raw) = import_name(child, source) {
                        output
                            .imports
                            .push(unresolved_import(raw, ImportKind::Import));
                    }
                }
                (
                    "class_declaration"
                    | "record_declaration"
                    | "interface_declaration"
                    | "annotation_type_declaration"
                    | "enum_declaration",
                    _,
                ) => {
                    let kind = match child.kind() {
                        "interface_declaration" | "annotation_type_declaration" => {
                            SymbolKind::Interface
                        }
                        "enum_declaration" => SymbolKind::Enum,
                        _ => SymbolKind::Class,
                    };
                    let Some(name) = declared_name(child, source) else {
                        continue;
                    };
                    let body = child.child_by_field_name("body");
                    let signature = signature_before(child, body, source);
                    if let Some(inner) =
                        found.type_symbol(child, kind, name, owner.as_ref(), signature)
                        && let Some(body) = body
                    {
                        stack.push((body, Some(inner)));
                    }
                }
                ("enum_body_declarations", Some(_)) => stack.push((child, owner.clone())),
                (
                    "method_declaration"
                    | "constructor_declaration"
                    | "compact_constructor_declaration"
                    | "annotation_type_element_declaration",
                    Some(owner),
                ) => {
                    let Some(name) = declared_name(child, source) else {
                        continue;
                    };
                    let body = child.child_by_field_name("body");
                    let signature = signature_before(child, body, source);
                    found.member(child, SymbolKind::Method, &name, owner, signature);
                }
                ("field_declaration" | "constant_declaration", Some(owner)) => {
                    fields(child, owner, source, &mut found);
                }
                ("enum_constant", Some(owner)) => {
                    let Some(name) = declared_name(child, source) else {
                        continue;
                    };
                    let end = child
                        .child_by_field_name("body")
                        .map_or(child.end_byte(), |body| body.start_byte());
                    let signature = source
                        .get(child.start_byte()..end)
                        .and_then(bounded_signature);
                    found.member(child, SymbolKind::Const, &name, owner, signature);
                }
                _ => {}
            }
        }
    }
    found.finish(output, symbol_bytes);
}

/// The packages a parsed Java file declares, read by the same walk that
/// stores them: its `package` name, or none for the default package.
pub(super) fn declarations(root: Node<'_>, source: &str) -> Vec<String> {
    super::declarations::module_names(walk, root, source)
}

fn package_name(declaration: Node<'_>, source: &str) -> Option<String> {
    let mut cursor = declaration.walk();
    let name = declaration
        .named_children(&mut cursor)
        .find(|child| matches!(child.kind(), "identifier" | "scoped_identifier"))?;
    dotted_text(name, source)
}

/// `a.b.C`, `a.b.*`, or `a.b.C.member` (static), as written without spaces.
fn import_name(declaration: Node<'_>, source: &str) -> Option<String> {
    let mut cursor = declaration.walk();
    let mut name = None;
    let mut wildcard = false;
    for child in declaration.named_children(&mut cursor) {
        match child.kind() {
            "identifier" | "scoped_identifier" => name = dotted_text(child, source),
            "asterisk" => wildcard = true,
            _ => {}
        }
    }
    let name = name?;
    Some(if wildcard { format!("{name}.*") } else { name })
}

fn declared_name(node: Node<'_>, source: &str) -> Option<String> {
    let name = node_text(node.child_by_field_name("name")?, source)?;
    is_identifier(name).then(|| name.to_owned())
}

/// The declaration's text from its first modifier or annotation to its
/// body (or its end), bounded.
fn signature_before(node: Node<'_>, body: Option<Node<'_>>, source: &str) -> Option<String> {
    let end = body.map_or(node.end_byte(), |body| body.start_byte());
    bounded_signature(source.get(node.start_byte()..end)?)
}

/// One `const` per declarator. A declaration of one name keeps its text
/// (initializer included, bounded); several names give each
/// `modifiers type name`, so one declaration's text is never repeated per
/// name.
fn fields(declaration: Node<'_>, owner: &Owner, source: &str, found: &mut Found) {
    let mut cursor = declaration.walk();
    let declarators = declaration
        .children_by_field_name("declarator", &mut cursor)
        .collect::<Vec<_>>();
    let prefix = declaration
        .child_by_field_name("type")
        .and_then(|kind| source.get(declaration.start_byte()..kind.end_byte()));
    for declarator in &declarators {
        let Some(name) = declared_name(*declarator, source) else {
            continue;
        };
        let signature = if declarators.len() == 1 {
            node_text(declaration, source).and_then(bounded_signature)
        } else {
            prefix.and_then(|prefix| bounded_signature(&format!("{} {name}", prefix.trim())))
        };
        let range = if declarators.len() == 1 {
            declaration
        } else {
            *declarator
        };
        found.member(range, SymbolKind::Const, &name, owner, signature);
    }
}
