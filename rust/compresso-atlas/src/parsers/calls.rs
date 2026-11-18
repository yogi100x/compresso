//! Call-site extraction over the syntax tree the symbol pass already built (D89).
//!
//! Each call inside an indexed function or method becomes one record: the
//! callee name as written, an optional qualifier (`Store` in `Store::open()`,
//! `os.path` in `os.path.join()`), and the call form. Nothing is resolved
//! here; Atlas resolves names at query time. Calls outside any indexed
//! function or method (module level, class bodies), macros, and calls whose
//! callee is not a plain name (closures, call results, subscripts) are not
//! recorded. Error recovery can leave zero-width or garbled names; such a
//! call is dropped so one malformed file never aborts an index.

use std::collections::HashSet;

use compresso_store::{CallForm, CallSiteRecord, SymbolKind, SymbolRecord};
use tree_sitter::Node;

use super::{line_number, node_text};

/// Call records kept per file, after de-duplication. A generated file with
/// more calls keeps the first ones in source order.
pub const MAX_CALLS_PER_FILE: usize = 4_096;
const MAX_CALL_TEXT_BYTES: usize = compresso_store::MAX_CALL_TEXT_BYTES;
/// Rust prelude variant constructors: never a repository symbol.
const RUST_PRELUDE_VARIANTS: [&str; 3] = ["Some", "Ok", "Err"];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CallSyntax {
    Rust,
    Python,
    Script,
}

struct Callee<'tree> {
    name: Node<'tree>,
    qualifier: Option<String>,
    form: CallForm,
}

pub(super) fn extract(
    syntax: CallSyntax,
    root: Node<'_>,
    source: &str,
    symbols: &[SymbolRecord],
) -> Vec<CallSiteRecord> {
    let callers = symbols
        .iter()
        .enumerate()
        .filter(|(_, symbol)| matches!(symbol.kind, SymbolKind::Function | SymbolKind::Method))
        .map(|(index, symbol)| (symbol.start_line, symbol.end_line, index))
        .collect::<Vec<_>>();
    if callers.is_empty() {
        return Vec::new();
    }

    let mut calls = Vec::new();
    let mut seen = HashSet::new();
    let mut cursor = root.walk();
    'walk: loop {
        let node = cursor.node();
        if let Some(callee) = callee(syntax, node, source)
            && let Some((record, name_start)) = record(callee, source, &callers)
            && seen.insert((
                record.caller,
                record.callee_name.clone(),
                record.qualifier.clone(),
                record.form,
            ))
        {
            calls.push((record.line, name_start, record));
            if calls.len() == MAX_CALLS_PER_FILE {
                break;
            }
        }
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                continue 'walk;
            }
            if !cursor.goto_parent() {
                break 'walk;
            }
        }
    }
    // Source order: by line, then by where the callee name starts on it.
    calls.sort_by_key(|(line, name_start, _)| (*line, *name_start));
    calls.into_iter().map(|(_, _, record)| record).collect()
}

fn record(
    callee: Callee<'_>,
    source: &str,
    callers: &[(u32, u32, usize)],
) -> Option<(CallSiteRecord, usize)> {
    let name = node_text(callee.name, source)?;
    if !valid_name(name) {
        return None;
    }
    let line = line_number(callee.name.start_position().row);
    // The innermost indexed function or method containing the call.
    let caller = callers
        .iter()
        .filter(|(start, end, _)| (*start..=*end).contains(&line))
        .min_by_key(|(start, end, index)| (end - start, std::cmp::Reverse(*start), *index))?
        .2;
    let record = CallSiteRecord {
        caller,
        callee_name: name.to_owned(),
        qualifier: callee.qualifier,
        form: callee.form,
        line,
    };
    Some((record, callee.name.start_byte()))
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_CALL_TEXT_BYTES
        && name
            .chars()
            .all(|character| character == '_' || character == '$' || character.is_alphanumeric())
}

fn callee<'tree>(syntax: CallSyntax, node: Node<'tree>, source: &str) -> Option<Callee<'tree>> {
    match (syntax, node.kind()) {
        (CallSyntax::Rust, "call_expression") => {
            let callee = rust_callee(node.child_by_field_name("function")?, source)?;
            let name = node_text(callee.name, source)?;
            if callee.form == CallForm::Plain && RUST_PRELUDE_VARIANTS.contains(&name) {
                return None;
            }
            Some(callee)
        }
        (CallSyntax::Python, "call") => {
            python_callee(node.child_by_field_name("function")?, source)
        }
        (CallSyntax::Script, "call_expression") => {
            let function = node.child_by_field_name("function")?;
            if function.kind() == "identifier" && node_text(function, source)? == "require" {
                return None;
            }
            script_callee(function, source)
        }
        (CallSyntax::Script, "new_expression") => {
            script_callee(node.child_by_field_name("constructor")?, source)
        }
        _ => None,
    }
}

fn rust_callee<'tree>(function: Node<'tree>, source: &str) -> Option<Callee<'tree>> {
    match function.kind() {
        "identifier" => Some(plain(function)),
        "scoped_identifier" => {
            let name = function.child_by_field_name("name")?;
            if name.kind() != "identifier" {
                return None;
            }
            let qualifier = function
                .child_by_field_name("path")
                .and_then(|path| node_text(path, source))
                .and_then(normalize_rust_path);
            Some(match qualifier {
                None => plain(name),
                Some(qualifier) if qualifier == "Self" => Callee {
                    name,
                    qualifier: Some(qualifier),
                    form: CallForm::SelfRef,
                },
                Some(qualifier) => Callee {
                    name,
                    qualifier: Some(qualifier),
                    form: CallForm::Path,
                },
            })
        }
        "field_expression" => {
            let name = function.child_by_field_name("field")?;
            if name.kind() != "field_identifier" {
                return None;
            }
            let value = function.child_by_field_name("value")?;
            Some(if value.kind() == "self" {
                Callee {
                    name,
                    qualifier: Some("self".to_owned()),
                    form: CallForm::SelfRef,
                }
            } else {
                receiver(name)
            })
        }
        "generic_function" => rust_callee(function.child_by_field_name("function")?, source),
        _ => None,
    }
}

/// Drops generic arguments and whitespace: `Vec::<u8>` and `HashMap<K, V>`
/// become `Vec` and `HashMap`. A path that is only a qualified-self type
/// (`<T as Trait>`) has no usable name and yields `None`.
fn normalize_rust_path(path: &str) -> Option<String> {
    let mut output = String::with_capacity(path.len());
    let mut depth = 0_u32;
    for character in path.chars() {
        match character {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            _ if depth > 0 || character.is_whitespace() => {}
            _ => output.push(character),
        }
    }
    let output = output.trim_matches(':').replace("::::", "::");
    let valid = !output.is_empty()
        && output.len() <= MAX_CALL_TEXT_BYTES
        && output.split("::").all(valid_name);
    valid.then_some(output)
}

fn python_callee<'tree>(function: Node<'tree>, source: &str) -> Option<Callee<'tree>> {
    match function.kind() {
        "identifier" => Some(plain(function)),
        "attribute" => {
            let name = function.child_by_field_name("attribute")?;
            if name.kind() != "identifier" {
                return None;
            }
            let object = function.child_by_field_name("object")?;
            if object.kind() == "identifier" && matches!(node_text(object, source)?, "self" | "cls")
            {
                return Some(Callee {
                    name,
                    qualifier: Some(node_text(object, source)?.to_owned()),
                    form: CallForm::SelfRef,
                });
            }
            Some(dotted(name, object, source, "attribute", "attribute"))
        }
        _ => None,
    }
}

fn script_callee<'tree>(function: Node<'tree>, source: &str) -> Option<Callee<'tree>> {
    match function.kind() {
        "identifier" => Some(plain(function)),
        "member_expression" => {
            let name = function.child_by_field_name("property")?;
            if name.kind() != "property_identifier" {
                return None;
            }
            let object = function.child_by_field_name("object")?;
            if object.kind() == "this" {
                return Some(Callee {
                    name,
                    qualifier: Some("this".to_owned()),
                    form: CallForm::SelfRef,
                });
            }
            Some(dotted(
                name,
                object,
                source,
                "member_expression",
                "property",
            ))
        }
        _ => None,
    }
}

/// A call on a dotted name (`a.b.c()`) keeps `a.b` as its qualifier; any
/// other receiver expression is recorded as a receiver call without one.
fn dotted<'tree>(
    name: Node<'tree>,
    object: Node<'_>,
    source: &str,
    member_kind: &str,
    member_field: &str,
) -> Callee<'tree> {
    if is_dotted(object, member_kind, member_field)
        && let Some(text) = node_text(object, source)
    {
        let qualifier = text
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect::<String>();
        if qualifier.len() <= MAX_CALL_TEXT_BYTES && qualifier.split('.').all(valid_name) {
            return Callee {
                name,
                qualifier: Some(qualifier),
                form: CallForm::Path,
            };
        }
    }
    receiver(name)
}

fn is_dotted(node: Node<'_>, member_kind: &str, member_field: &str) -> bool {
    match node.kind() {
        "identifier" => true,
        kind if kind == member_kind => {
            node.child_by_field_name("object")
                .is_some_and(|object| is_dotted(object, member_kind, member_field))
                && node
                    .child_by_field_name(member_field)
                    .is_some_and(|member| {
                        matches!(member.kind(), "identifier" | "property_identifier")
                    })
        }
        _ => false,
    }
}

const fn plain(name: Node<'_>) -> Callee<'_> {
    Callee {
        name,
        qualifier: None,
        form: CallForm::Plain,
    }
}

const fn receiver(name: Node<'_>) -> Callee<'_> {
    Callee {
        name,
        qualifier: None,
        form: CallForm::Receiver,
    }
}
