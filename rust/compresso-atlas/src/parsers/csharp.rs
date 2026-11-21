//! C# symbols and imports (E17-40, D105).
//!
//! Namespace declarations, block and file-scoped, as `module` symbols named
//! by their full dotted name (`Acme.Jobs.Internal` for a namespace nested in
//! `Acme.Jobs`); classes and records (`class`), structs and record structs
//! (`struct`), interfaces, enums, and delegates (`type`), with nested types
//! qualified through their outer types (`Outer::Inner`; the namespace is not
//! part of a qualified name); methods and constructors (`Type::Type`) as
//! methods; fields, constants, properties, and enum members as `const` (the
//! store has no field or property kind); and `using` directives (plain,
//! `static`, alias, `global`) by the name they import. A partial type gives
//! one symbol per declaration. Type parameters are never part of a name, and
//! a verbatim identifier (`@class`) is stored without its `@`. Not
//! symbols: destructors, operators, conversion operators, indexers, events,
//! local functions, lambdas, top-level statements, and declarations inside
//! member bodies. One iterative pass over the declaration levels only
//! (namespaces, type bodies, and `#if` blocks between them); the nesting and
//! name bounds of [`super::declarations`] apply. Error-recovery nodes are
//! dropped.

use std::rc::Rc;

use compresso_store::SymbolKind;
use tree_sitter::Node;

use super::declarations::{Found, Owner, declaration_text, dotted_text, is_identifier};
use super::{
    Extraction, ImportContext, ImportKind, bounded_signature, node_text, unresolved_import,
};

/// Where a declaration sits: the namespace around it and, inside a type, its
/// owner.
#[derive(Clone)]
struct Scope {
    namespace: Rc<str>,
    owner: Option<Owner>,
}

pub(super) fn walk(
    root: Node<'_>,
    source: &str,
    output: &mut Extraction,
    symbol_bytes: &mut Vec<Option<(usize, usize)>>,
) {
    let mut found = Found::default();
    let mut stack = vec![(
        root,
        Scope {
            namespace: Rc::from(""),
            owner: None,
        },
    )];
    while let Some((container, scope)) = stack.pop() {
        // A file-scoped namespace applies to the declarations after it.
        let mut scope = scope;
        let mut cursor = container.walk();
        for child in container.named_children(&mut cursor) {
            if child.is_missing() {
                continue;
            }
            // Error recovery can wrap complete declarations: their level is
            // the one around the error node, and a garbled name is dropped
            // when it is read.
            if child.is_error() {
                stack.push((child, scope.clone()));
                continue;
            }
            let kind = child.kind();
            match (kind, scope.owner.as_ref()) {
                ("using_directive", None) => {
                    if let Some(raw) = using_name(child, source) {
                        let mut import = unresolved_import(raw, ImportKind::Import);
                        import.context = if has_token(child, "global") {
                            ImportContext::Global
                        } else if scope.namespace.is_empty() {
                            ImportContext::None
                        } else {
                            ImportContext::Namespace(scope.namespace.to_string())
                        };
                        output.imports.push(import);
                    }
                }
                ("namespace_declaration" | "file_scoped_namespace_declaration", None) => {
                    let Some(name) = namespace_name(child, &scope.namespace, source) else {
                        continue;
                    };
                    found.module(child, name.clone(), format!("namespace {name}"));
                    let inner = Scope {
                        namespace: Rc::from(name.as_str()),
                        owner: None,
                    };
                    if kind == "file_scoped_namespace_declaration" {
                        scope = inner;
                    } else if let Some(body) = child.child_by_field_name("body") {
                        stack.push((body, inner));
                    }
                }
                (
                    "class_declaration"
                    | "struct_declaration"
                    | "interface_declaration"
                    | "enum_declaration"
                    | "record_declaration"
                    | "delegate_declaration",
                    owner,
                ) => {
                    let symbol_kind = match kind {
                        "struct_declaration" => SymbolKind::Struct,
                        "interface_declaration" => SymbolKind::Interface,
                        "enum_declaration" => SymbolKind::Enum,
                        "delegate_declaration" => SymbolKind::Type,
                        "record_declaration" if has_token(child, "struct") => SymbolKind::Struct,
                        _ => SymbolKind::Class,
                    };
                    let Some(name) = declared_name(child, source) else {
                        continue;
                    };
                    let body = child.child_by_field_name("body");
                    let signature = signature_before(child, body, source);
                    if let Some(inner) =
                        found.type_symbol(child, symbol_kind, name, owner, signature)
                        && let Some(body) = body
                    {
                        stack.push((
                            body,
                            Scope {
                                namespace: Rc::clone(&scope.namespace),
                                owner: Some(inner),
                            },
                        ));
                    }
                }
                ("method_declaration" | "constructor_declaration", Some(owner)) => {
                    let Some(name) = declared_name(child, source) else {
                        continue;
                    };
                    let body = child.child_by_field_name("body");
                    let signature = signature_before(child, body, source);
                    found.member(child, SymbolKind::Method, &name, owner, signature);
                }
                ("property_declaration", Some(owner)) => {
                    let Some(name) = declared_name(child, source) else {
                        continue;
                    };
                    let end = child
                        .child_by_field_name("accessors")
                        .or_else(|| child.child_by_field_name("value"))
                        .map_or(child.end_byte(), |body| body.start_byte());
                    let signature = signature_text(child, end, source)
                        .as_deref()
                        .and_then(bounded_signature);
                    found.member(child, SymbolKind::Const, &name, owner, signature);
                }
                ("field_declaration", Some(owner)) => fields(child, owner, source, &mut found),
                ("enum_member_declaration", Some(owner)) => {
                    let Some(name) = declared_name(child, source) else {
                        continue;
                    };
                    let signature = node_text(child, source).and_then(bounded_signature);
                    found.member(child, SymbolKind::Const, &name, owner, signature);
                }
                // `#if` blocks hold declarations of the level around them.
                (kind, _) if kind.starts_with("preproc_") => stack.push((child, scope.clone())),
                _ => {}
            }
        }
    }
    found.finish(output, symbol_bytes);
}

/// The namespaces a parsed C# file declares, read by the same walk that
/// stores them, nested ones by their full name, in source order without
/// repeats.
pub(super) fn declarations(root: Node<'_>, source: &str) -> Vec<String> {
    super::declarations::module_names(walk, root, source)
}

/// The full name of a namespace declared inside `outer`; `None` when it
/// would exceed a stored name, so deep nesting cannot grow names (or the
/// work spent on them) without bound: such a namespace is skipped with its
/// contents.
fn namespace_name(declaration: Node<'_>, outer: &str, source: &str) -> Option<String> {
    let name = dotted_text(declaration.child_by_field_name("name")?, source)?;
    if outer.len() + 1 + name.len() > super::declarations::MAX_QUALIFIED_NAME_BYTES {
        return None;
    }
    Some(if outer.is_empty() {
        name
    } else {
        format!("{outer}.{name}")
    })
}

/// The name a `using` directive imports: `System`, `System.Math` for
/// `using static System.Math;`, `System.Text.Json` for
/// `using Json = System.Text.Json;`; a leading `global::` is dropped.
fn using_name(directive: Node<'_>, source: &str) -> Option<String> {
    let alias = directive.child_by_field_name("name");
    let mut cursor = directive.walk();
    let target = directive.named_children(&mut cursor).find(|child| {
        Some(child.id()) != alias.map(|alias| alias.id())
            && matches!(
                child.kind(),
                "identifier" | "qualified_name" | "alias_qualified_name"
            )
    })?;
    let text = node_text(target, source)?;
    let text = text.trim().strip_prefix("global::").unwrap_or(text);
    let name = text
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>();
    name.split('.')
        .all(|part| is_identifier(part.trim_start_matches('@')))
        .then(|| name.replace('@', ""))
        .filter(|name| name.len() <= super::declarations::MAX_QUALIFIED_NAME_BYTES)
}

/// The aliases a file's `using` directives define, anywhere at namespace
/// level: alias to the last part of the name it stands for
/// (`using Json = System.Text.Json.JsonSerializer;` gives `Json` →
/// `JsonSerializer`). Read once per file for call qualifiers.
pub(super) fn using_aliases(root: Node<'_>, source: &str) -> Vec<(String, String)> {
    let mut aliases = Vec::new();
    let mut stack = vec![root];
    while let Some(container) = stack.pop() {
        let mut cursor = container.walk();
        for child in container.named_children(&mut cursor) {
            match child.kind() {
                "using_directive" => {
                    let Some(alias) = child
                        .child_by_field_name("name")
                        .and_then(|alias| node_text(alias, source))
                    else {
                        continue;
                    };
                    let Some(target) = using_name(child, source) else {
                        continue;
                    };
                    let last = target.rsplit('.').next().unwrap_or(&target).to_owned();
                    aliases.push((alias.trim_start_matches('@').to_owned(), last));
                }
                "namespace_declaration" => {
                    if let Some(body) = child.child_by_field_name("body") {
                        stack.push(body);
                    }
                }
                kind if kind.starts_with("preproc_") => stack.push(child),
                _ => {}
            }
        }
    }
    aliases
}

fn declared_name(node: Node<'_>, source: &str) -> Option<String> {
    let name = node_text(node.child_by_field_name("name")?, source)?;
    let name = name.strip_prefix('@').unwrap_or(name);
    is_identifier(name).then(|| name.to_owned())
}

fn has_token(node: Node<'_>, token: &str) -> bool {
    let mut cursor = node.walk();
    node.children(&mut cursor)
        .any(|child| !child.is_named() && child.kind() == token)
}

fn signature_before(node: Node<'_>, body: Option<Node<'_>>, source: &str) -> Option<String> {
    let end = body.map_or(node.end_byte(), |body| body.start_byte());
    bounded_signature(&signature_text(node, end, source)?)
}

/// The modifiers and attribute lists of a C# declaration.
fn prefix_nodes(node: Node<'_>) -> Vec<Node<'_>> {
    let mut cursor = node.walk();
    node.children(&mut cursor)
        .filter(|child| matches!(child.kind(), "modifier" | "attribute_list"))
        .collect()
}

fn is_annotation(node: &Node<'_>) -> bool {
    node.kind() == "attribute_list"
}

fn signature_text(node: Node<'_>, end: usize, source: &str) -> Option<String> {
    declaration_text(node, end, &prefix_nodes(node), is_annotation, source)
}

/// One `const` per declarator, signatures as for Java fields.
fn fields(declaration: Node<'_>, owner: &Owner, source: &str, found: &mut Found) {
    let mut cursor = declaration.walk();
    let Some(variables) = declaration
        .named_children(&mut cursor)
        .find(|child| child.kind() == "variable_declaration")
    else {
        return;
    };
    let mut inner = variables.walk();
    let declarators = variables
        .named_children(&mut inner)
        .filter(|child| child.kind() == "variable_declarator")
        .collect::<Vec<_>>();
    let prefix = variables
        .child_by_field_name("type")
        .and_then(|kind| signature_text(declaration, kind.end_byte(), source));
    for declarator in &declarators {
        let Some(name) = declared_name(*declarator, source) else {
            continue;
        };
        let signature = if declarators.len() == 1 {
            signature_text(declaration, declaration.end_byte(), source)
                .as_deref()
                .and_then(bounded_signature)
        } else {
            prefix.as_deref().and_then(|prefix| {
                bounded_signature(&super::per_name_text(format!("{prefix} {name}")))
            })
        };
        let range = if declarators.len() == 1 {
            declaration
        } else {
            *declarator
        };
        found.member(range, SymbolKind::Const, &name, owner, signature);
    }
}
