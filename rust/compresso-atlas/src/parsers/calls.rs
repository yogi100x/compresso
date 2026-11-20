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
/// Go builtin functions and predeclared types: a plain call to one is a
/// builtin or a conversion, so it is not recorded. A repository function
/// that shadows one of these names loses its plain calls.
const GO_PREDECLARED: [&str; 40] = [
    "any",
    "append",
    "bool",
    "byte",
    "cap",
    "clear",
    "close",
    "complex",
    "complex64",
    "complex128",
    "copy",
    "delete",
    "error",
    "float32",
    "float64",
    "imag",
    "int",
    "int8",
    "int16",
    "int32",
    "int64",
    "len",
    "make",
    "max",
    "min",
    "new",
    "panic",
    "print",
    "println",
    "real",
    "recover",
    "rune",
    "string",
    "uint",
    "uint8",
    "uint16",
    "uint32",
    "uint64",
    "uintptr",
    "comparable",
];
/// The predeclared Go type names: an index of one of these is a type
/// argument, not a slice index.
const GO_PREDECLARED_TYPES: [&str; 22] = [
    "any",
    "bool",
    "byte",
    "comparable",
    "complex64",
    "complex128",
    "error",
    "float32",
    "float64",
    "int",
    "int8",
    "int16",
    "int32",
    "int64",
    "rune",
    "string",
    "uint",
    "uint8",
    "uint16",
    "uint32",
    "uint64",
    "uintptr",
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CallSyntax {
    Rust,
    Python,
    Script,
    Go,
    Java,
    CSharp,
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
    symbol_bytes: &[Option<(usize, usize)>],
) -> ExtractedCalls {
    let Some(enclosing) = EnclosingCallers::new(symbols, symbol_bytes) else {
        return ExtractedCalls {
            calls: Vec::new(),
            truncated: false,
        };
    };

    let receivers = match syntax {
        CallSyntax::Go => GoReceivers::new(root, source),
        CallSyntax::CSharp => GoReceivers {
            using_aliases: super::csharp::using_aliases(root, source)
                .into_iter()
                .collect(),
            ..GoReceivers::default()
        },
        _ => GoReceivers::default(),
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
            callee(syntax, node, source, &receivers)
                .into_iter()
                .collect()
        } else if is_argument_list(syntax, node.kind()) {
            argument_references(node)
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
        CallSyntax::Go => const_eq(kind, "call_expression"),
        CallSyntax::Java => {
            const_eq(kind, "method_invocation") || const_eq(kind, "object_creation_expression")
        }
        CallSyntax::CSharp => {
            const_eq(kind, "invocation_expression") || const_eq(kind, "object_creation_expression")
        }
    }
}

const fn is_argument_list(syntax: CallSyntax, kind: &str) -> bool {
    match syntax {
        CallSyntax::Rust | CallSyntax::Script => const_eq(kind, "arguments"),
        CallSyntax::Python | CallSyntax::Go => const_eq(kind, "argument_list"),
        // Java and C# have no free functions: a bare argument is a value.
        CallSyntax::Java | CallSyntax::CSharp => false,
    }
}

/// Bare names passed as call arguments (`map_err(to_failure)`,
/// `sorted(items, key=rank)`): possible function references. Most are
/// variables; Atlas keeps only those that resolve to a function in the
/// caller's file or a file it imports.
fn argument_references<'tree>(arguments: Node<'tree>) -> Vec<Callee<'tree>> {
    let mut cursor = arguments.walk();
    arguments
        .named_children(&mut cursor)
        .filter_map(|argument| match argument.kind() {
            "identifier" => Some(argument),
            "keyword_argument" => argument
                .child_by_field_name("value")
                .filter(|value| value.kind() == "identifier"),
            _ => None,
        })
        .map(|name| Callee {
            name,
            qualifier: None,
            form: CallForm::Reference,
        })
        .collect()
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

/// The innermost indexed function or method enclosing a call.
///
/// When every function and method carries its node's byte range, a call is
/// attributed by the byte where its name starts, so two functions on one
/// line (minified script) each own their calls. Otherwise it falls back to
/// lines: the innermost range covering the call's line, computed once per
/// file with a sweep over the symbol ranges. Either way a lookup is O(log n)
/// or better after O(n log n) setup.
enum EnclosingCallers {
    Bytes {
        /// (start, end, symbol index), outer before inner at equal starts.
        ranges: Vec<(usize, usize, usize)>,
        /// Index into `ranges` of the innermost range containing each one.
        parents: Vec<Option<usize>>,
    },
    Lines {
        by_line: Vec<Option<usize>>,
    },
}

impl EnclosingCallers {
    fn new(symbols: &[SymbolRecord], symbol_bytes: &[Option<(usize, usize)>]) -> Option<Self> {
        let callable = symbols
            .iter()
            .enumerate()
            .filter(|(_, symbol)| matches!(symbol.kind, SymbolKind::Function | SymbolKind::Method))
            .collect::<Vec<_>>();
        if callable.is_empty() {
            return None;
        }
        let bytes = callable
            .iter()
            .map(|(index, _)| {
                symbol_bytes
                    .get(*index)
                    .copied()
                    .flatten()
                    .map(|(start, end)| (start, end, *index))
            })
            .collect::<Option<Vec<_>>>();
        if let Some(ranges) = bytes {
            return Some(Self::by_bytes(ranges));
        }
        Some(Self::by_lines(&callable))
    }

    fn by_bytes(mut ranges: Vec<(usize, usize, usize)>) -> Self {
        // Outer ranges first at each start, so the inner one is nested last;
        // among equal ranges the first symbol is innermost.
        ranges.sort_by(|left, right| {
            left.0
                .cmp(&right.0)
                .then_with(|| right.1.cmp(&left.1))
                .then_with(|| right.2.cmp(&left.2))
        });
        let mut parents = Vec::with_capacity(ranges.len());
        let mut stack: Vec<usize> = Vec::new();
        for (position, range) in ranges.iter().enumerate() {
            while stack
                .last()
                .is_some_and(|top| ranges[*top].1 < range.1 || ranges[*top].1 <= range.0)
            {
                stack.pop();
            }
            parents.push(stack.last().copied());
            stack.push(position);
        }
        Self::Bytes { ranges, parents }
    }

    fn by_lines(callable: &[(usize, &SymbolRecord)]) -> Self {
        let mut ranges = callable
            .iter()
            .map(|(index, symbol)| (symbol.start_line, symbol.end_line, *index))
            .collect::<Vec<_>>();
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
        Self::Lines { by_line }
    }

    fn at(&self, line: u32, byte: usize) -> Option<usize> {
        match self {
            Self::Lines { by_line } => by_line.get(line as usize).copied().flatten(),
            Self::Bytes { ranges, parents } => {
                let mut position = ranges
                    .partition_point(|range| range.0 <= byte)
                    .checked_sub(1)?;
                // The last range starting at or before the byte, or the
                // nearest one enclosing it.
                while ranges[position].1 <= byte {
                    position = parents[position]?;
                }
                Some(ranges[position].2)
            }
        }
    }
}

fn record(
    callee: Callee<'_>,
    source: &str,
    enclosing: &EnclosingCallers,
) -> Option<(CallSiteRecord, usize)> {
    let line = line_number(callee.name.start_position().row);
    let caller = enclosing.at(line, callee.name.start_byte())?;
    let name = node_text(callee.name, source)?;
    // Every call form passes this one check, so no extraction path can hand
    // the store a name or qualifier it would reject and abort the index with.
    if !valid_name(name)
        || callee
            .qualifier
            .as_deref()
            .is_some_and(|qualifier| !valid_qualifier(qualifier))
    {
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

/// A qualifier is at most [`MAX_CALL_TEXT_BYTES`] and made of valid names
/// joined by `::` (Rust paths) or `.` (dotted names). `Self`, `self`, `this`,
/// and `cls` are single valid names.
fn valid_qualifier(qualifier: &str) -> bool {
    qualifier.len() <= MAX_CALL_TEXT_BYTES
        && qualifier
            .split("::")
            .all(|segment| segment.split('.').all(valid_name))
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_CALL_TEXT_BYTES
        && name
            .chars()
            .all(|character| character == '_' || character == '$' || character.is_alphanumeric())
}

fn callee<'tree>(
    syntax: CallSyntax,
    node: Node<'tree>,
    source: &str,
    receivers: &GoReceivers<'_>,
) -> Option<Callee<'tree>> {
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
        (CallSyntax::Go, "call_expression") => {
            let callee = go_callee(
                node,
                node.child_by_field_name("function")?,
                source,
                receivers,
            )?;
            let name = node_text(callee.name, source)?;
            if callee.form == CallForm::Plain && GO_PREDECLARED.contains(&name) {
                return None;
            }
            Some(callee)
        }
        (CallSyntax::Java, "method_invocation") => java_callee(node, source),
        (CallSyntax::CSharp, "invocation_expression") => {
            csharp_callee(node.child_by_field_name("function")?, source, receivers)
        }
        (CallSyntax::Java | CallSyntax::CSharp, "object_creation_expression") => {
            created_type(node.child_by_field_name("type")?).map(plain)
        }
        _ => None,
    }
}

/// `m()` (a method of the enclosing type, recorded plain), `this.m()` and
/// `super.m()` (self calls), `a.b.m()` on a chain of plain names (a path
/// call: a variable, a field, a type, or a package-qualified type, which
/// look alike in syntax), and `expr.m()` on anything else (a receiver call).
fn java_callee<'tree>(call: Node<'tree>, source: &str) -> Option<Callee<'tree>> {
    let name = call.child_by_field_name("name")?;
    if name.kind() != "identifier" {
        return None;
    }
    let Some(object) = call.child_by_field_name("object") else {
        return Some(plain(name));
    };
    Some(match object.kind() {
        "this" | "super" => Callee {
            name,
            qualifier: Some(object.kind().to_owned()),
            form: CallForm::SelfRef,
        },
        "identifier" | "field_access" => {
            dotted(name, object, source, "field_access", "field", "object")
        }
        _ => receiver(name),
    })
}

/// As for Java, with `base.M()` as the self call on the base type. A call
/// on a predefined type (`string.Join()`) and `nameof(x)` are not calls to
/// repository code. The first part of a path call's qualifier that is a
/// `using` alias is replaced by the last part of the name it stands for.
fn csharp_callee<'tree>(
    function: Node<'tree>,
    source: &str,
    receivers: &GoReceivers<'_>,
) -> Option<Callee<'tree>> {
    match function.kind() {
        "identifier" => (node_text(function, source)? != "nameof").then(|| plain(function)),
        "generic_name" => generic_identifier(function).map(plain),
        "member_access_expression" => {
            let name = function.child_by_field_name("name")?;
            let name = match name.kind() {
                "identifier" => name,
                "generic_name" => generic_identifier(name)?,
                _ => return None,
            };
            let object = function.child_by_field_name("expression")?;
            Some(match object.kind() {
                "this" | "base" => Callee {
                    name,
                    qualifier: Some(object.kind().to_owned()),
                    form: CallForm::SelfRef,
                },
                "predefined_type" => return None,
                "identifier" | "member_access_expression" => {
                    let mut callee = dotted(
                        name,
                        object,
                        source,
                        "member_access_expression",
                        "name",
                        "expression",
                    );
                    if let Some(qualifier) = callee.qualifier.as_mut() {
                        let (first, rest) = qualifier
                            .split_once('.')
                            .map_or((qualifier.as_str(), ""), |(first, rest)| (first, rest));
                        if let Some(target) = receivers.using_aliases.get(first) {
                            *qualifier = if rest.is_empty() {
                                target.clone()
                            } else {
                                format!("{target}.{rest}")
                            };
                        }
                    }
                    callee
                }
                _ => receiver(name),
            })
        }
        "conditional_access_expression" => {
            let mut cursor = function.walk();
            let binding = function
                .named_children(&mut cursor)
                .find(|child| child.kind() == "member_binding_expression")?;
            let name = binding.child_by_field_name("name")?;
            let name = match name.kind() {
                "identifier" => name,
                "generic_name" => generic_identifier(name)?,
                _ => return None,
            };
            Some(receiver(name))
        }
        _ => None,
    }
}

fn generic_identifier(generic: Node<'_>) -> Option<Node<'_>> {
    let mut cursor = generic.walk();
    generic
        .named_children(&mut cursor)
        .find(|child| child.kind() == "identifier")
}

/// The type name a `new` expression creates (`Box` in `new Box<>(1)`,
/// `Inner` in `new a.b.Inner()`): recorded as a plain call, which resolves
/// to the type, as `new X()` does in script. Predefined, array, and
/// implicit (`new()`) types give none.
fn created_type(mut kind: Node<'_>) -> Option<Node<'_>> {
    loop {
        kind = match kind.kind() {
            "type_identifier" | "identifier" => return Some(kind),
            "generic_type" | "generic_name" => {
                let mut cursor = kind.walk();
                kind.named_children(&mut cursor).find(|child| {
                    matches!(
                        child.kind(),
                        "type_identifier" | "identifier" | "scoped_type_identifier"
                    )
                })?
            }
            "scoped_type_identifier" => {
                let mut cursor = kind.walk();
                kind.named_children(&mut cursor)
                    .filter(|child| child.kind() == "type_identifier")
                    .last()?
            }
            "qualified_name" => kind.child_by_field_name("name")?,
            _ => return None,
        };
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
            Some(dotted(
                name,
                object,
                source,
                "attribute",
                "attribute",
                "object",
            ))
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
                "object",
            ))
        }
        _ => None,
    }
}

/// `f()`, `pkg.F()`, and `x.M()`. A call on the enclosing method's receiver
/// name (`s.save()` in `func (s *Store) Close()`) is a self call; a call on
/// any other plain name keeps that name as its qualifier, because a package
/// and a variable look alike in syntax; a qualifier that is an import's
/// alias is replaced by the imported path's last segment. Generic
/// instantiations (`New[int, string]()`) are unwrapped; other callee
/// expressions are receiver calls or dropped.
fn go_callee<'tree>(
    call: Node<'tree>,
    function: Node<'tree>,
    source: &str,
    receivers: &GoReceivers<'_>,
) -> Option<Callee<'tree>> {
    let mut function = function;
    // `New[int, string]()` is an instantiation. `New[int]()` parses as an
    // index expression, like `handlers[i]()`: it is unwrapped only when the
    // index is a predeclared type name, so an indexed function value is
    // never a call to the slice. `New[T]()` with a type parameter is lost.
    loop {
        function = match function.kind() {
            "generic_type" | "type_instantiation_expression" => {
                function.child_by_field_name("type")?
            }
            "index_expression"
                if function
                    .child_by_field_name("index")
                    .and_then(|index| node_text(index, source))
                    .is_some_and(|index| GO_PREDECLARED_TYPES.contains(&index)) =>
            {
                function.child_by_field_name("operand")?
            }
            _ => break,
        };
    }
    match function.kind() {
        "identifier" => Some(plain(function)),
        "selector_expression" => {
            let name = function.child_by_field_name("field")?;
            if name.kind() != "field_identifier" {
                return None;
            }
            let operand = function.child_by_field_name("operand")?;
            if operand.kind() != "identifier" {
                return Some(receiver(name));
            }
            let qualifier = node_text(operand, source)?;
            if receivers.at(call.start_byte()) == Some(qualifier) {
                return Some(Callee {
                    name,
                    qualifier: Some(qualifier.to_owned()),
                    form: CallForm::SelfRef,
                });
            }
            let qualifier = receivers
                .aliases
                .get(qualifier)
                .cloned()
                .unwrap_or_else(|| qualifier.to_owned());
            Some(Callee {
                name,
                qualifier: Some(qualifier),
                form: CallForm::Path,
            })
        }
        _ => None,
    }
}

/// The receiver name of each method declaration, by byte range, read once
/// per file: Go methods are declared only at the top level, so a call's
/// receiver is found by binary search instead of walking its ancestors.
/// Import aliases are read the same way, so `st.Open()` after
/// `import st "…/store"` is recorded with the qualifier `store`.
#[derive(Default)]
struct GoReceivers<'source> {
    /// (start byte, end byte, receiver name), in source order.
    methods: Vec<(usize, usize, Option<&'source str>)>,
    /// Import alias to the imported package's default local name.
    aliases: std::collections::HashMap<&'source str, String>,
    /// C#: `using` alias to the last part of the name it stands for.
    using_aliases: std::collections::HashMap<String, String>,
}

impl<'source> GoReceivers<'source> {
    fn new(root: Node<'_>, source: &'source str) -> Self {
        let mut methods = Vec::new();
        let mut aliases = std::collections::HashMap::new();
        let mut cursor = root.walk();
        for child in root.children(&mut cursor) {
            match child.kind() {
                "method_declaration" => methods.push((
                    child.start_byte(),
                    child.end_byte(),
                    super::go::receiver_name(child, source),
                )),
                "import_declaration" => {
                    let mut specs = Vec::new();
                    let mut inner = child.walk();
                    for spec in child.named_children(&mut inner) {
                        if spec.kind() == "import_spec_list" {
                            let mut list = spec.walk();
                            specs.extend(
                                spec.named_children(&mut list)
                                    .filter(|item| item.kind() == "import_spec"),
                            );
                        } else if spec.kind() == "import_spec" {
                            specs.push(spec);
                        }
                    }
                    for spec in specs {
                        let alias = spec
                            .child_by_field_name("name")
                            .filter(|name| name.kind() == "package_identifier")
                            .and_then(|name| node_text(name, source));
                        let path = spec
                            .child_by_field_name("path")
                            .and_then(|path| node_text(path, source))
                            .map(|path| path.trim_matches(['"', '`']));
                        if let (Some(alias), Some(path)) = (alias, path) {
                            aliases.insert(alias, super::go::package_local_name(path));
                        }
                    }
                }
                _ => {}
            }
        }
        Self {
            methods,
            aliases,
            using_aliases: std::collections::HashMap::new(),
        }
    }

    fn at(&self, byte: usize) -> Option<&'source str> {
        let index = self
            .methods
            .partition_point(|method| method.0 <= byte)
            .checked_sub(1)?;
        let (_, end, name) = self.methods[index];
        if byte < end { name } else { None }
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
    object_field: &str,
) -> Callee<'tree> {
    if is_dotted(object, member_kind, member_field, object_field)
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
fn is_dotted(node: Node<'_>, member_kind: &str, member_field: &str, object_field: &str) -> bool {
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
                let Some(object) = node.child_by_field_name(object_field) else {
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
