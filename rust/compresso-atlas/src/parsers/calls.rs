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
/// Call expressions examined per file, kept or not. Bounds the work a file of
/// millions of repeated or rejected calls can cost.
pub const MAX_CALL_NODES_PER_FILE: usize = 65_536;
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

/// Extracted calls plus whether a per-file bound stopped the walk early.
pub(super) struct ExtractedCalls {
    pub(super) calls: Vec<CallSiteRecord>,
    pub(super) truncated: bool,
}

pub(super) fn extract(
    syntax: CallSyntax,
    root: Node<'_>,
    source: &str,
    symbols: &[SymbolRecord],
) -> ExtractedCalls {
    let Some(enclosing) = EnclosingCallers::new(symbols) else {
        return ExtractedCalls {
            calls: Vec::new(),
            truncated: false,
        };
    };

    let mut calls = Vec::new();
    let mut seen = HashSet::new();
    let mut examined = 0_usize;
    let mut truncated = false;
    // Cursor-based pre-order walk: no recursion, so nesting depth cannot
    // exhaust the stack.
    let mut cursor = root.walk();
    'walk: loop {
        let node = cursor.node();
        let found = if is_call_node(syntax, node.kind()) {
            callee(syntax, node, source).into_iter().collect()
        } else if syntax == CallSyntax::Rust && node.kind() == "token_tree" {
            macro_argument_calls(node, source)
        } else {
            Vec::new()
        };
        for callee in found {
            examined += 1;
            if examined > MAX_CALL_NODES_PER_FILE {
                truncated = true;
                break 'walk;
            }
            if let Some((record, name_start)) = record(callee, source, &enclosing)
                && seen.insert((
                    record.caller,
                    record.callee_name.clone(),
                    record.qualifier.clone(),
                    record.form,
                ))
            {
                calls.push((record.line, name_start, record));
                if calls.len() == MAX_CALLS_PER_FILE {
                    truncated = true;
                    break 'walk;
                }
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
    ExtractedCalls {
        calls: calls.into_iter().map(|(_, _, record)| record).collect(),
        truncated,
    }
}

const fn is_call_node(syntax: CallSyntax, kind: &str) -> bool {
    match syntax {
        CallSyntax::Rust => const_eq(kind, "call_expression"),
        CallSyntax::Python => const_eq(kind, "call"),
        CallSyntax::Script => const_eq(kind, "call_expression") || const_eq(kind, "new_expression"),
    }
}

const fn const_eq(left: &str, right: &str) -> bool {
    let (left, right) = (left.as_bytes(), right.as_bytes());
    if left.len() != right.len() {
        return false;
    }
    let mut index = 0;
    while index < left.len() {
        if left[index] != right[index] {
            return false;
        }
        index += 1;
    }
    true
}

/// The innermost indexed function or method for every source line, computed
/// once per file in O(lines + symbols log symbols) with a sweep over the
/// symbol ranges, so each call finds its caller in O(1).
struct EnclosingCallers {
    by_line: Vec<Option<usize>>,
}

impl EnclosingCallers {
    fn new(symbols: &[SymbolRecord]) -> Option<Self> {
        let mut ranges = symbols
            .iter()
            .enumerate()
            .filter(|(_, symbol)| matches!(symbol.kind, SymbolKind::Function | SymbolKind::Method))
            .map(|(index, symbol)| (symbol.start_line, symbol.end_line, index))
            .collect::<Vec<_>>();
        if ranges.is_empty() {
            return None;
        }
        // Outer ranges first at each start line, so the inner one ends on top;
        // among equal ranges the first symbol ends on top.
        ranges.sort_by(|left, right| {
            left.0
                .cmp(&right.0)
                .then_with(|| right.1.cmp(&left.1))
                .then_with(|| right.2.cmp(&left.2))
        });
        let last_line = ranges.iter().map(|range| range.1).max().unwrap_or(0);
        let mut by_line = Vec::with_capacity(last_line as usize + 1);
        by_line.push(None);
        let mut stack: Vec<(u32, u32, usize)> = Vec::new();
        let mut next = 0;
        for line in 1..=last_line {
            while next < ranges.len() && ranges[next].0 == line {
                stack.push(ranges[next]);
                next += 1;
            }
            while stack.last().is_some_and(|top| top.1 < line) {
                stack.pop();
            }
            by_line.push(stack.last().map(|top| top.2));
        }
        Some(Self { by_line })
    }

    fn at(&self, line: u32) -> Option<usize> {
        self.by_line.get(line as usize).copied().flatten()
    }
}

fn record(
    callee: Callee<'_>,
    source: &str,
    enclosing: &EnclosingCallers,
) -> Option<(CallSiteRecord, usize)> {
    let line = line_number(callee.name.start_position().row);
    let caller = enclosing.at(line)?;
    let name = node_text(callee.name, source)?;
    if !valid_name(name) {
        return None;
    }
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

/// Calls written inside a macro's arguments (`assert!(run())`,
/// `format!("{}", self.name())`). Tree-sitter keeps macro arguments as a flat
/// token tree, so a call is read from tokens: a name directly followed by a
/// parenthesized group, optionally behind `Type::`, `self.`, or `expr.`.
/// `fn name(` in a macro body and `name!(` (a nested macro) are not calls.
fn macro_argument_calls<'tree>(tree: Node<'tree>, source: &str) -> Vec<Callee<'tree>> {
    let mut cursor = tree.walk();
    let tokens = tree.children(&mut cursor).collect::<Vec<_>>();
    let mut calls = Vec::new();
    for (index, token) in tokens.iter().enumerate() {
        if token.kind() != "identifier" {
            continue;
        }
        let Some(next) = tokens.get(index + 1) else {
            continue;
        };
        if next.kind() != "token_tree"
            || !node_text(*next, source).is_some_and(|text| text.starts_with('('))
        {
            continue;
        }
        let previous = index.checked_sub(1).map(|previous| tokens[previous]);
        let before = index.checked_sub(2).map(|before| tokens[before]);
        let previous_kind = previous.map(|token| token.kind());
        if previous_kind == Some("fn") {
            continue;
        }
        let name = node_text(*token, source).unwrap_or_default();
        let callee = match (previous_kind, before) {
            (Some("::"), Some(owner)) if owner.kind() == "identifier" || owner.kind() == "self" => {
                let owner_text = node_text(owner, source).unwrap_or_default();
                Callee {
                    name: *token,
                    qualifier: Some(owner_text.to_owned()),
                    form: if owner_text == "Self" {
                        CallForm::SelfRef
                    } else {
                        CallForm::Path
                    },
                }
            }
            (Some("::"), _) => continue,
            (Some("."), Some(receiver_token))
                if node_text(receiver_token, source) == Some("self") =>
            {
                Callee {
                    name: *token,
                    qualifier: Some("self".to_owned()),
                    form: CallForm::SelfRef,
                }
            }
            (Some("."), _) => receiver(*token),
            _ if RUST_PRELUDE_VARIANTS.contains(&name) => continue,
            _ => plain(*token),
        };
        calls.push(callee);
    }
    calls
}

fn rust_callee<'tree>(function: Node<'tree>, source: &str) -> Option<Callee<'tree>> {
    let mut function = function;
    while function.kind() == "generic_function" {
        function = function.child_by_field_name("function")?;
    }
    match function.kind() {
        "identifier" => Some(plain(function)),
        "scoped_identifier" => {
            let name = function.child_by_field_name("name")?;
            if name.kind() != "identifier" {
                return None;
            }
            let qualifier = function
                .child_by_field_name("path")
                .filter(|path| path.byte_range().len() <= MAX_CALL_TEXT_BYTES * 4)
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

/// Whether `node` is a chain of plain names (`a`, `a.b`, `a.b.c`). Walks
/// the chain iteratively; a chain longer than a stored qualifier is rejected
/// before it is walked.
fn is_dotted(node: Node<'_>, member_kind: &str, member_field: &str) -> bool {
    if node.byte_range().len() > MAX_CALL_TEXT_BYTES {
        return false;
    }
    let mut node = node;
    loop {
        match node.kind() {
            "identifier" => return true,
            kind if kind == member_kind => {
                let member_ok = node
                    .child_by_field_name(member_field)
                    .is_some_and(|member| {
                        matches!(member.kind(), "identifier" | "property_identifier")
                    });
                let Some(object) = node.child_by_field_name("object") else {
                    return false;
                };
                if !member_ok {
                    return false;
                }
                node = object;
            }
            _ => return false,
        }
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
