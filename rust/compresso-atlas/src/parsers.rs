use std::collections::HashSet;
use std::path::Path;
use std::sync::{LazyLock, OnceLock};

use compresso_store::{CallSiteRecord, SymbolKind, SymbolRecord};
use tree_sitter::{Node, Parser, Query, QueryCursor, StreamingIterator};

pub(crate) mod calls;
mod csharp;
mod declarations;
mod go;
mod java;
pub mod languages;
mod shell;
pub(crate) mod sql;
mod text;

use calls::CallSyntax;
pub use calls::{MAX_CALL_NODES_PER_FILE, MAX_CALLS_PER_FILE};
pub(crate) use declarations::modifiers as signature_modifiers;
pub(crate) use go::package_local_name as go_package_local_name;
use languages::Extractor;
pub use languages::{LANGUAGES, LanguageSpec};

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

/// Where an import is written, when that changes what it names (C#).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum ImportContext {
    #[default]
    None,
    /// A `using` inside this namespace: its name is looked up relative to
    /// the enclosing namespaces first (`using Util;` in `Acme` may mean
    /// `Acme.Util`).
    Namespace(String),
    /// A `global using`: it applies to every file of the language.
    Global,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportRecord {
    pub raw: String,
    pub kind: ImportKind,
    pub resolved_path: Option<String>,
    pub resolution: ImportResolution,
    pub context: ImportContext,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Extraction {
    pub symbols: Vec<SymbolRecord>,
    pub imports: Vec<ImportRecord>,
    /// Call sites inside the extracted functions and methods (D89);
    /// `caller` indexes `symbols`.
    pub calls: Vec<CallSiteRecord>,
    /// A per-file call bound ([`MAX_CALLS_PER_FILE`] kept or
    /// [`MAX_CALL_NODES_PER_FILE`] examined) stopped call extraction early.
    pub calls_truncated: bool,
    /// A per-file symbol bound stopped symbol extraction early (SQL,
    /// E17-42); later definitions of the file are not symbols.
    pub symbols_truncated: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("language {0:?} is not implemented by this parser registry")]
    UnsupportedLanguage(Language),
    #[error("failed to configure the {language} parser: {message}")]
    Language {
        language: &'static str,
        message: String,
    },
    #[error("invalid {language} extraction query: {message}")]
    Query {
        language: &'static str,
        message: String,
    },
    #[error("parser did not return a syntax tree")]
    ParseFailed,
}

pub struct ParserRegistry;

impl ParserRegistry {
    pub fn for_path(path: &Path) -> Option<Language> {
        let extension = path.extension()?.to_str()?;
        languages::for_extension(extension).map(|spec| spec.language)
    }

    pub fn extract(lang: Language, source: &str) -> Result<Extraction, ParseError> {
        let spec = languages::spec(lang);
        match spec.extractor {
            Extractor::Query {
                grammar,
                query,
                calls,
                process,
            } => extract_with_query(
                query_slot(lang),
                grammar(),
                spec.name,
                query,
                source,
                calls,
                process,
            ),
            Extractor::Walk {
                grammar,
                calls,
                walk,
            } => extract_with_walk(grammar(), spec.name, source, calls, walk),
            Extractor::Custom(extract) => Ok(extract(source)),
        }
    }
}

/// The compiled-query slot of `language`'s table entry.
fn query_slot(language: Language) -> &'static QuerySlot {
    let index = LANGUAGES
        .iter()
        .position(|spec| spec.language == language)
        .unwrap_or_else(|| unreachable!("every FileLanguage has a table entry"));
    &COMPILED_QUERIES[index]
}

#[derive(Clone, Copy)]
pub(crate) enum CaptureKind {
    Symbol(SymbolKind),
    Variable,
    ModuleExport,
    PythonConstant,
    Import(ImportKind),
    Use,
}

#[derive(Clone, Copy)]
pub(crate) struct Candidate<'tree> {
    node: Node<'tree>,
    kind: CaptureKind,
}

/// A language's extraction query with the kind of each capture, compiled
/// once per process and shared by every thread (`Query` is `Send + Sync`;
/// cursors and parsers stay per call) (E17-43, D108).
struct CompiledQuery {
    query: Query,
    kinds: Vec<Option<CaptureKind>>,
}

/// One slot per table language; only query-based languages fill theirs. A
/// failed compilation is kept as its message and returned on every call.
type QuerySlot = OnceLock<Result<CompiledQuery, String>>;

static COMPILED_QUERIES: LazyLock<Vec<QuerySlot>> =
    LazyLock::new(|| LANGUAGES.iter().map(|_| OnceLock::new()).collect());

fn compiled_query<'slot>(
    slot: &'slot QuerySlot,
    language: &tree_sitter::Language,
    language_name: &'static str,
    query_source: &str,
) -> Result<&'slot CompiledQuery, ParseError> {
    slot.get_or_init(|| {
        #[cfg(test)]
        count_query_compilation(language_name);
        let query = Query::new(language, query_source).map_err(|error| error.to_string())?;
        let kinds = query
            .capture_names()
            .iter()
            .map(|name| capture_kind(name))
            .collect();
        Ok(CompiledQuery { query, kinds })
    })
    .as_ref()
    .map_err(|message| ParseError::Query {
        language: language_name,
        message: message.clone(),
    })
}

fn extract_with_query(
    slot: &QuerySlot,
    language: tree_sitter::Language,
    language_name: &'static str,
    query_source: &str,
    source: &str,
    call_syntax: CallSyntax,
    process_candidate: for<'tree> fn(Candidate<'tree>, &str, &mut Extraction),
) -> Result<Extraction, ParseError> {
    let mut parser = Parser::new();
    parser
        .set_language(&language)
        .map_err(|error| ParseError::Language {
            language: language_name,
            message: error.to_string(),
        })?;
    let tree = parser.parse(source, None).ok_or(ParseError::ParseFailed)?;
    // Compiled on the first parse that needs it, as before, so a query that
    // does not compile fails the same call it failed on main, every time.
    let compiled = compiled_query(slot, &language, language_name, query_source)?;
    let mut cursor = QueryCursor::new();
    let mut query_matches = cursor.matches(&compiled.query, tree.root_node(), source.as_bytes());
    let mut candidates = Vec::new();

    while let Some(query_match) = query_matches.next() {
        for capture in query_match.captures {
            let Some(kind) = compiled.kinds[capture.index as usize] else {
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

    let mut extraction = Extraction {
        symbols: Vec::new(),
        imports: Vec::new(),
        calls: Vec::new(),
        calls_truncated: false,
        symbols_truncated: false,
    };
    // The byte range of each symbol's node, when its recorded lines are the
    // node's lines, so call attribution can tell apart functions that share
    // a line (minified script). Not stored: the store keeps lines only.
    let mut symbol_bytes = Vec::new();
    for candidate in candidates {
        let before = extraction.symbols.len();
        process_candidate(candidate, source, &mut extraction);
        let node = candidate.node;
        for symbol in &extraction.symbols[before..] {
            let same_lines = symbol.start_line == line_number(node.start_position().row)
                && symbol.end_line == line_number(node.end_position().row);
            symbol_bytes.push(same_lines.then(|| (node.start_byte(), node.end_byte())));
        }
    }
    // Error recovery can yield an import whose module name is a zero-width
    // placeholder (`import ,`). It names nothing, and an empty edge target is
    // rejected by the store, which would abort the whole repository index.
    extraction
        .imports
        .retain(|import| !import.raw.trim().is_empty());
    let calls = calls::extract(
        call_syntax,
        tree.root_node(),
        source,
        &extraction.symbols,
        &symbol_bytes,
    );
    extraction.calls = calls.calls;
    extraction.calls_truncated = calls.truncated;

    Ok(extraction)
}

/// Parses `source` and hands its tree to a hand-written declaration walk
/// (Java, C#), then records call sites inside the walked functions and
/// methods exactly as the query-based extractors do.
fn extract_with_walk(
    language: tree_sitter::Language,
    language_name: &'static str,
    source: &str,
    call_syntax: CallSyntax,
    walk: languages::Walk,
) -> Result<Extraction, ParseError> {
    let tree = parse_tree(&language, language_name, source)?;
    let mut extraction = Extraction {
        symbols: Vec::new(),
        imports: Vec::new(),
        calls: Vec::new(),
        calls_truncated: false,
        symbols_truncated: false,
    };
    let mut symbol_bytes = Vec::new();
    walk(tree.root_node(), source, &mut extraction, &mut symbol_bytes);
    debug_assert_eq!(symbol_bytes.len(), extraction.symbols.len());
    extraction
        .imports
        .retain(|import| !import.raw.trim().is_empty());
    let calls = calls::extract(
        call_syntax,
        tree.root_node(),
        source,
        &extraction.symbols,
        &symbol_bytes,
    );
    extraction.calls = calls.calls;
    extraction.calls_truncated = calls.truncated;
    Ok(extraction)
}

fn parse_tree(
    language: &tree_sitter::Language,
    language_name: &'static str,
    source: &str,
) -> Result<tree_sitter::Tree, ParseError> {
    let mut parser = Parser::new();
    parser
        .set_language(language)
        .map_err(|error| ParseError::Language {
            language: language_name,
            message: error.to_string(),
        })?;
    parser.parse(source, None).ok_or(ParseError::ParseFailed)
}

/// The packages (Java) or namespaces (C#) a source file declares, read the
/// same way extraction records them as `module` symbols, so the import map
/// built before a run agrees with what the run stores (E17-40, D105). Empty
/// for every other language and for a source the grammar cannot parse.
pub(crate) fn declared_packages(language: Language, source: &str) -> Vec<String> {
    let spec = languages::spec(language);
    let Some(declarations) = spec.declarations else {
        return Vec::new();
    };
    let Extractor::Walk { grammar, .. } = spec.extractor else {
        return Vec::new();
    };
    parse_tree(&grammar(), spec.name, source)
        .map(|tree| declarations(tree.root_node(), source))
        .unwrap_or_default()
}

/// Largest signature kept for Go, Java, and C# (as for shell and text
/// headings); a longer one is cut at a character boundary and marked `…`.
pub(crate) const MAX_SIGNATURE_BYTES: usize = 1_024;

/// `text` cut a little past [`MAX_SIGNATURE_BYTES`] at a character
/// boundary, so [`bounded_signature`] still sees it was longer and marks it:
/// text reused in a signature per name is cut once, before it is copied, so
/// a declaration of many names never copies a long prefix per name.
fn signature_prefix(text: &str) -> &str {
    const KEPT: usize = MAX_SIGNATURE_BYTES + 8;
    if text.len() <= KEPT {
        return text;
    }
    let mut end = KEPT;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// Extraction queries compiled so far in this process, per language name
/// (tests only; process-wide, since workers compile on their own threads).
#[cfg(test)]
static QUERY_COMPILATIONS: std::sync::Mutex<Vec<(&'static str, usize)>> =
    std::sync::Mutex::new(Vec::new());

#[cfg(test)]
fn count_query_compilation(language_name: &'static str) {
    let mut counts = QUERY_COMPILATIONS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match counts.iter_mut().find(|(name, _)| *name == language_name) {
        Some((_, count)) => *count += 1,
        None => counts.push((language_name, 1)),
    }
}

#[cfg(test)]
fn query_compilations(language_name: &str) -> usize {
    QUERY_COMPILATIONS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .find(|(name, _)| *name == language_name)
        .map_or(0, |(_, count)| *count)
}

#[cfg(test)]
thread_local! {
    /// Bytes of signature text built once per declared name, on this thread
    /// (tests only): pins that a long shared prefix is cut before it is
    /// copied per name (review rounds 3 and 4).
    static PER_NAME_BYTES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Signature text built for one of several names of a declaration; counted
/// in test builds.
fn per_name_text(text: String) -> String {
    #[cfg(test)]
    PER_NAME_BYTES.with(|bytes| bytes.set(bytes.get() + text.len()));
    text
}

/// Whether [`signature_prefix`] cuts `text`.
fn is_cut(text: &str) -> bool {
    text.len() > MAX_SIGNATURE_BYTES + 8
}

/// [`bounded_signature`] of text built from a prefix [`signature_prefix`]
/// cut (`cut`): marked as cut whatever its kept part ends with, exactly as
/// the whole text would have been, so a kept part ending in spaces or `;`
/// never loses the mark.
fn marked_signature(text: &str, cut: bool) -> Option<String> {
    if !cut {
        return bounded_signature(text);
    }
    let text = text.trim_start();
    let mut end = (MAX_SIGNATURE_BYTES - '…'.len_utf8()).min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    Some(format!("{}…", &text[..end]))
}

fn bounded_signature(text: &str) -> Option<String> {
    let signature = trimmed_signature(text)?;
    if signature.len() <= MAX_SIGNATURE_BYTES {
        return Some(signature);
    }
    let mut end = MAX_SIGNATURE_BYTES - '…'.len_utf8();
    while !signature.is_char_boundary(end) {
        end -= 1;
    }
    Some(format!("{}…", &signature[..end]))
}

fn capture_kind(name: &str) -> Option<CaptureKind> {
    match name {
        "definition.function" => Some(CaptureKind::Symbol(SymbolKind::Function)),
        "definition.class" => Some(CaptureKind::Symbol(SymbolKind::Class)),
        "definition.method" => Some(CaptureKind::Symbol(SymbolKind::Method)),
        "definition.interface" => Some(CaptureKind::Symbol(SymbolKind::Interface)),
        "definition.struct" => Some(CaptureKind::Symbol(SymbolKind::Struct)),
        "definition.enum" => Some(CaptureKind::Symbol(SymbolKind::Enum)),
        "definition.trait" => Some(CaptureKind::Symbol(SymbolKind::Trait)),
        "definition.const" => Some(CaptureKind::Symbol(SymbolKind::Const)),
        "definition.type" => Some(CaptureKind::Symbol(SymbolKind::Type)),
        "definition.module" => Some(CaptureKind::Symbol(SymbolKind::Module)),
        "definition.variable" => Some(CaptureKind::Variable),
        "definition.module_export" => Some(CaptureKind::ModuleExport),
        "definition.python_constant" => Some(CaptureKind::PythonConstant),
        "import.import" => Some(CaptureKind::Import(ImportKind::Import)),
        "import.python" => Some(CaptureKind::Import(ImportKind::Import)),
        "import.python_from" => Some(CaptureKind::Import(ImportKind::FromImport)),
        "import.export_from" => Some(CaptureKind::Import(ImportKind::ExportFrom)),
        "import.require" => Some(CaptureKind::Import(ImportKind::Require)),
        "import.use" => Some(CaptureKind::Use),
        _ => None,
    }
}

const fn capture_priority(kind: CaptureKind) -> u8 {
    match kind {
        CaptureKind::Symbol(_)
        | CaptureKind::Variable
        | CaptureKind::ModuleExport
        | CaptureKind::PythonConstant => 0,
        CaptureKind::Import(_) | CaptureKind::Use => 1,
    }
}

fn process_rust_candidate(candidate: Candidate<'_>, source: &str, output: &mut Extraction) {
    match candidate.kind {
        CaptureKind::Symbol(kind) => {
            if let Some(symbol) = rust_symbol(candidate.node, kind, source) {
                output.symbols.push(symbol);
            }
        }
        CaptureKind::Use => output.imports.extend(rust_imports(candidate.node, source)),
        CaptureKind::Variable
        | CaptureKind::ModuleExport
        | CaptureKind::PythonConstant
        | CaptureKind::Import(_) => {}
    }
}

fn process_typescript_candidate(candidate: Candidate<'_>, source: &str, output: &mut Extraction) {
    match candidate.kind {
        CaptureKind::Symbol(kind) => {
            if let Some(symbol) = script_symbol(candidate.node, kind, source) {
                output.symbols.push(symbol);
            }
        }
        CaptureKind::Variable => {
            if let Some(symbol) = typescript_variable_symbol(candidate.node, source) {
                output.symbols.push(symbol);
            }
        }
        CaptureKind::Import(kind) => {
            if let Some(import) = script_import(candidate.node, kind, source) {
                output.imports.push(import);
            }
        }
        CaptureKind::ModuleExport | CaptureKind::PythonConstant | CaptureKind::Use => {}
    }
}

fn process_javascript_candidate(candidate: Candidate<'_>, source: &str, output: &mut Extraction) {
    match candidate.kind {
        CaptureKind::Symbol(kind) => {
            if let Some(symbol) = script_symbol(candidate.node, kind, source) {
                output.symbols.push(symbol);
            }
        }
        CaptureKind::Variable => {
            if let Some(symbol) = javascript_variable_symbol(candidate.node, source) {
                output.symbols.push(symbol);
            }
        }
        CaptureKind::ModuleExport => {
            if let Some(symbol) = javascript_module_export(candidate.node, source) {
                output.symbols.push(symbol);
            }
        }
        CaptureKind::Import(ImportKind::Require) => {
            if let Some(import) = javascript_require(candidate.node, source) {
                output.imports.push(import);
            }
        }
        CaptureKind::Import(kind) => {
            if let Some(import) = script_import(candidate.node, kind, source) {
                output.imports.push(import);
            }
        }
        CaptureKind::PythonConstant | CaptureKind::Use => {}
    }
}

fn process_python_candidate(candidate: Candidate<'_>, source: &str, output: &mut Extraction) {
    match candidate.kind {
        CaptureKind::Symbol(kind) => {
            if let Some(symbol) = python_symbol(candidate.node, kind, source) {
                output.symbols.push(symbol);
            }
        }
        CaptureKind::PythonConstant => {
            if let Some(symbol) = python_constant(candidate.node, source) {
                output.symbols.push(symbol);
            }
        }
        CaptureKind::Import(ImportKind::Import) => output
            .imports
            .extend(python_imports(candidate.node, source)),
        CaptureKind::Import(ImportKind::FromImport) => output
            .imports
            .extend(python_from_imports(candidate.node, source)),
        CaptureKind::Variable
        | CaptureKind::ModuleExport
        | CaptureKind::Import(_)
        | CaptureKind::Use => {}
    }
}

fn python_symbol(node: Node<'_>, kind: SymbolKind, source: &str) -> Option<SymbolRecord> {
    let name = node_text(node.child_by_field_name("name")?, source)?.to_owned();
    let (kind, qualified_name) = match kind {
        SymbolKind::Function => {
            if let Some(owner) = python_class_owner(node, source) {
                (SymbolKind::Method, format!("{owner}::{name}"))
            } else if python_is_module_declaration(node) {
                (SymbolKind::Function, name.clone())
            } else {
                return None;
            }
        }
        SymbolKind::Class if python_is_module_declaration(node) => {
            (SymbolKind::Class, name.clone())
        }
        _ => return None,
    };

    Some(SymbolRecord {
        name,
        qualified_name,
        kind,
        signature: python_declaration_signature(node, source),
        start_line: line_number(node.start_position().row),
        end_line: line_number(node.end_position().row),
        token_estimate: 0,
    })
}

fn python_class_owner(function: Node<'_>, source: &str) -> Option<String> {
    let declaration = python_declaration_wrapper(function);
    let class_body = declaration.parent()?;
    if class_body.kind() != "block" {
        return None;
    }
    let class = class_body.parent()?;
    if class.kind() != "class_definition" || !python_is_module_declaration(class) {
        return None;
    }
    Some(node_text(class.child_by_field_name("name")?, source)?.to_owned())
}

fn python_is_module_declaration(node: Node<'_>) -> bool {
    python_declaration_wrapper(node)
        .parent()
        .is_some_and(|parent| parent.kind() == "module")
}

fn python_declaration_wrapper(node: Node<'_>) -> Node<'_> {
    node.parent()
        .filter(|parent| parent.kind() == "decorated_definition")
        .unwrap_or(node)
}

fn python_declaration_signature(node: Node<'_>, source: &str) -> Option<String> {
    let body = node.child_by_field_name("body")?;
    let declaration = source.get(node.start_byte()..body.start_byte())?.trim();
    if declaration.is_empty() {
        return None;
    }

    let mut parts = Vec::new();
    if let Some(wrapper) = node
        .parent()
        .filter(|parent| parent.kind() == "decorated_definition")
    {
        let mut cursor = wrapper.walk();
        for child in wrapper.named_children(&mut cursor) {
            if child.kind() == "decorator" {
                parts.push(node_text(child, source)?.trim());
            }
        }
    }
    parts.push(declaration);
    Some(parts.join("\n"))
}

fn python_constant(node: Node<'_>, source: &str) -> Option<SymbolRecord> {
    if !python_is_module_statement(node) || node.child_by_field_name("right").is_none() {
        return None;
    }
    let name_node = node.child_by_field_name("left")?;
    if name_node.kind() != "identifier" {
        return None;
    }
    let name = node_text(name_node, source)?;
    if !is_python_constant_name(name) {
        return None;
    }

    Some(SymbolRecord {
        name: name.to_owned(),
        qualified_name: name.to_owned(),
        kind: SymbolKind::Const,
        signature: trimmed_signature(node_text(node, source)?),
        start_line: line_number(node.start_position().row),
        end_line: line_number(node.end_position().row),
        token_estimate: 0,
    })
}

fn python_is_module_statement(node: Node<'_>) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    parent.kind() == "module"
        || (parent.kind() == "expression_statement"
            && parent
                .parent()
                .is_some_and(|grandparent| grandparent.kind() == "module"))
}

fn is_python_constant_name(name: &str) -> bool {
    name.as_bytes().first().is_some_and(u8::is_ascii_uppercase)
        && name
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
}

fn python_imports(node: Node<'_>, source: &str) -> Vec<ImportRecord> {
    let mut imports = Vec::new();
    let mut cursor = node.walk();
    for imported in node.children_by_field_name("name", &mut cursor) {
        let name = if imported.kind() == "aliased_import" {
            imported.child_by_field_name("name")
        } else {
            Some(imported)
        };
        if let Some(raw) = name.and_then(|name| node_text(name, source)) {
            imports.push(unresolved_import(raw.to_owned(), ImportKind::Import));
        }
    }
    imports
}

fn python_from_imports(node: Node<'_>, source: &str) -> Vec<ImportRecord> {
    let Some(module) = node
        .child_by_field_name("module_name")
        .and_then(|module| node_text(module, source))
    else {
        return Vec::new();
    };
    let mut imports = Vec::new();
    let mut cursor = node.walk();
    for imported in node.children_by_field_name("name", &mut cursor) {
        let name = if imported.kind() == "aliased_import" {
            imported.child_by_field_name("name")
        } else {
            Some(imported)
        };
        let Some(name) = name.and_then(|name| node_text(name, source)) else {
            continue;
        };
        let raw = if module.ends_with('.') {
            format!("{module}{name}")
        } else {
            format!("{module}.{name}")
        };
        imports.push(unresolved_import(raw, ImportKind::FromImport));
    }
    imports
}

fn unresolved_import(raw: String, kind: ImportKind) -> ImportRecord {
    ImportRecord {
        raw,
        kind,
        resolved_path: None,
        resolution: ImportResolution::Unresolved(NOT_INDEXED.to_owned()),
        context: ImportContext::None,
    }
}

fn script_symbol(node: Node<'_>, kind: SymbolKind, source: &str) -> Option<SymbolRecord> {
    let (name, qualified_name) = if kind == SymbolKind::Method {
        let owner = script_class_owner(node, source)?;
        let name = node_text(node.child_by_field_name("name")?, source)?.to_owned();
        (name.clone(), format!("{owner}::{name}"))
    } else {
        if !is_module_level(node) {
            return None;
        }
        let name = node_text(node.child_by_field_name("name")?, source)?.to_owned();
        (name.clone(), name)
    };

    Some(SymbolRecord {
        name,
        qualified_name,
        kind,
        signature: script_declaration_signature(node, source),
        start_line: line_number(node.start_position().row),
        end_line: line_number(node.end_position().row),
        token_estimate: 0,
    })
}

fn script_class_owner(method: Node<'_>, source: &str) -> Option<String> {
    let class_body = method.parent()?;
    if class_body.kind() != "class_body" {
        return None;
    }
    let class = class_body.parent()?;
    if class.kind() != "class_declaration" || !is_module_level(class) {
        return None;
    }
    Some(node_text(class.child_by_field_name("name")?, source)?.to_owned())
}

fn typescript_variable_symbol(node: Node<'_>, source: &str) -> Option<SymbolRecord> {
    let declaration = node.parent()?;
    if declaration.kind() != "lexical_declaration" || !is_module_level(declaration) {
        return None;
    }
    let is_const = declaration.child_by_field_name("kind")?.kind() == "const";
    if !is_const {
        return None;
    }
    let exported = export_wrapper(declaration).is_some();
    let value = node.child_by_field_name("value")?;
    let kind = if value.kind() == "arrow_function" {
        SymbolKind::Function
    } else if exported {
        SymbolKind::Const
    } else {
        return None;
    };
    let name_node = node.child_by_field_name("name")?;
    if name_node.kind() != "identifier" {
        return None;
    }
    let name = node_text(name_node, source)?.to_owned();

    Some(SymbolRecord {
        qualified_name: name.clone(),
        name,
        kind,
        signature: typescript_variable_signature(node, declaration, value, source),
        start_line: line_number(node.start_position().row),
        end_line: line_number(node.end_position().row),
        token_estimate: 0,
    })
}

fn script_declaration_signature(node: Node<'_>, source: &str) -> Option<String> {
    let start_byte = export_wrapper(node).map_or(node.start_byte(), |export| export.start_byte());
    let end_byte = node
        .child_by_field_name("body")
        .map_or(node.end_byte(), |body| body.start_byte());
    trimmed_signature(source.get(start_byte..end_byte)?)
}

fn typescript_variable_signature(
    node: Node<'_>,
    declaration: Node<'_>,
    value: Node<'_>,
    source: &str,
) -> Option<String> {
    let start_byte =
        export_wrapper(declaration).map_or(declaration.start_byte(), |export| export.start_byte());
    let end_byte = if value.kind() == "arrow_function" {
        value.child_by_field_name("body")?.start_byte()
    } else {
        node.end_byte()
    };
    trimmed_signature(source.get(start_byte..end_byte)?)
}

fn javascript_variable_symbol(node: Node<'_>, source: &str) -> Option<SymbolRecord> {
    let declaration = node.parent()?;
    if declaration.kind() != "lexical_declaration" || !is_module_level(declaration) {
        return None;
    }
    if declaration.child_by_field_name("kind")?.kind() != "const" {
        return None;
    }
    let value = node.child_by_field_name("value")?;
    if !matches!(value.kind(), "arrow_function" | "function_expression") {
        return None;
    }
    let name_node = node.child_by_field_name("name")?;
    if name_node.kind() != "identifier" {
        return None;
    }
    let name = node_text(name_node, source)?.to_owned();

    Some(SymbolRecord {
        qualified_name: name.clone(),
        name,
        kind: SymbolKind::Function,
        signature: javascript_variable_signature(declaration, value, source),
        start_line: line_number(node.start_position().row),
        end_line: line_number(node.end_position().row),
        token_estimate: 0,
    })
}

fn javascript_variable_signature(
    declaration: Node<'_>,
    value: Node<'_>,
    source: &str,
) -> Option<String> {
    let start_byte =
        export_wrapper(declaration).map_or(declaration.start_byte(), |export| export.start_byte());
    let end_byte = value.child_by_field_name("body")?.start_byte();
    trimmed_signature(source.get(start_byte..end_byte)?)
}

fn javascript_module_export(node: Node<'_>, source: &str) -> Option<SymbolRecord> {
    let statement = node.parent()?;
    if statement.kind() != "expression_statement" || !is_module_level(statement) {
        return None;
    }
    let left = node.child_by_field_name("left")?;
    if node_text(left, source)? != "module.exports" {
        return None;
    }
    let name = "module.exports".to_owned();
    Some(SymbolRecord {
        qualified_name: name.clone(),
        name,
        kind: SymbolKind::Module,
        signature: trimmed_signature(node_text(node, source)?),
        start_line: line_number(node.start_position().row),
        end_line: line_number(node.end_position().row),
        token_estimate: 0,
    })
}

fn javascript_require(node: Node<'_>, source: &str) -> Option<ImportRecord> {
    let callee = node.child_by_field_name("function")?;
    if callee.kind() != "identifier" || node_text(callee, source)? != "require" {
        return None;
    }
    let arguments = node.child_by_field_name("arguments")?;
    if arguments.kind() != "arguments" {
        return None;
    }
    let mut cursor = arguments.walk();
    let mut values = arguments.named_children(&mut cursor);
    let module = values.next()?;
    if values.next().is_some() || module.kind() != "string" {
        return None;
    }
    script_import(module, ImportKind::Require, source)
}

fn script_import(node: Node<'_>, kind: ImportKind, source: &str) -> Option<ImportRecord> {
    let quoted = node_text(node, source)?;
    let raw = unquote_module_specifier(quoted)?.to_owned();
    Some(ImportRecord {
        raw,
        kind,
        resolved_path: None,
        resolution: ImportResolution::Unresolved(NOT_INDEXED.to_owned()),
        context: ImportContext::None,
    })
}

fn unquote_module_specifier(quoted: &str) -> Option<&str> {
    let bytes = quoted.as_bytes();
    let (first, last) = (*bytes.first()?, *bytes.last()?);
    if bytes.len() >= 2 && matches!((first, last), (b'\'', b'\'') | (b'"', b'"')) {
        quoted.get(1..quoted.len() - 1)
    } else {
        None
    }
}

fn is_module_level(node: Node<'_>) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    parent.kind() == "program"
        || (parent.kind() == "export_statement"
            && parent
                .parent()
                .is_some_and(|grandparent| grandparent.kind() == "program"))
}

fn export_wrapper(node: Node<'_>) -> Option<Node<'_>> {
    node.parent()
        .filter(|parent| parent.kind() == "export_statement")
}

fn trimmed_signature(signature: &str) -> Option<String> {
    let signature = signature.trim();
    let signature = signature.strip_suffix(';').unwrap_or(signature).trim_end();
    (!signature.is_empty()).then(|| signature.to_owned())
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
    trimmed_signature(source.get(node.start_byte()..end_byte)?)
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
            context: ImportContext::None,
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
    let text = source.get(node.start_byte()..node.end_byte());
    #[cfg(test)]
    count_text_read(text.map_or(0, str::len));
    text
}

#[cfg(test)]
thread_local! {
    /// Bytes of source text handed out by [`node_text`] (and Go's other
    /// source reads) on this thread (tests only): text a per-spec path
    /// reads is counted, so reading a whole declaration once per spec shows
    /// as quadratic growth (E17-53 review).
    pub(crate) static TEXT_BYTES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Counts `bytes` of source text read; test builds only.
#[cfg(test)]
pub(crate) fn count_text_read(bytes: usize) {
    TEXT_BYTES.with(|total| total.set(total.get() + bytes));
}

fn line_number(zero_based_row: usize) -> u32 {
    u32::try_from(zero_based_row)
        .unwrap_or(u32::MAX - 1)
        .saturating_add(1)
}

#[cfg(test)]
mod tests {
    use super::{
        Language, MAX_SIGNATURE_BYTES, ParserRegistry, bounded_signature, is_cut, marked_signature,
        signature_prefix,
    };

    /// E17-43 (D108): each query-based language compiles its extraction
    /// query once per process, however many files and threads extract with
    /// it; on main it was compiled again for every file (about a fifth of a
    /// worker's time in a sampled profile). Counted, not timed.
    #[test]
    fn test_each_language_query_is_compiled_once_per_process() {
        let samples = [
            (Language::Rust, "Rust", "fn a() { b(); }\n"),
            (
                Language::TypeScript,
                "TypeScript",
                "export function a() { b(); }\n",
            ),
            (Language::Tsx, "TSX", "export const A = () => <div />;\n"),
            (
                Language::JavaScript,
                "JavaScript",
                "function a() { b(); }\n",
            ),
            (Language::Python, "Python", "def a():\n    b()\n"),
            (Language::Go, "Go", "package a\nfunc A() { b() }\n"),
        ];
        std::thread::scope(|scope| {
            for _ in 0..4 {
                scope.spawn(|| {
                    for (language, _, source) in samples {
                        for _ in 0..3 {
                            ParserRegistry::extract(language, source).expect("extracts");
                        }
                    }
                });
            }
        });
        for (language, name, _) in samples {
            assert_eq!(
                super::languages::spec(language).name,
                name,
                "{language:?} is named as its table entry"
            );
            assert_eq!(
                super::query_compilations(name),
                1,
                "{name} query compiled once"
            );
        }
    }

    /// E17-43 (D108): a query that does not compile fails every call with
    /// the same `ParseError::Query` it gave when compiled per file, and is
    /// not compiled again.
    #[test]
    fn test_a_query_that_fails_to_compile_fails_every_call_the_same_way() {
        let slot = super::QuerySlot::new();
        let grammar: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
        let name = "Broken query (test)";
        let errors = (0..3)
            .map(
                |_| match super::compiled_query(&slot, &grammar, name, "(no_such_node) @x") {
                    Err(error) => error.to_string(),
                    Ok(_) => panic!("an invalid query compiles"),
                },
            )
            .collect::<Vec<_>>();
        assert!(errors[0].starts_with("invalid Broken query (test) extraction query: "));
        assert!(errors.iter().all(|error| *error == errors[0]));
        assert_eq!(super::query_compilations(name), 1);
    }

    /// Review M1: text reused per name is cut once, just past the bound, so
    /// the bound still marks a signature cut.
    #[test]
    fn test_signature_prefix_cuts_once_just_past_the_bound() {
        let long = "é".repeat(5_000);
        let prefix = signature_prefix(&long);
        assert!(prefix.len() > MAX_SIGNATURE_BYTES && prefix.len() <= MAX_SIGNATURE_BYTES + 8);
        let signature = bounded_signature(&format!("{prefix} name")).expect("signature");
        assert!(signature.len() <= MAX_SIGNATURE_BYTES && signature.ends_with('…'));
        assert_eq!(signature_prefix("short"), "short");
    }

    /// Rounds 3 and 4 (review M1, S4): 2,000 names behind a 330 KB (Java,
    /// C#) or 500 KB (Go) type copy at most a bounded prefix per name; with
    /// the prefix copied whole they would copy 660 MB or more. Counted, not
    /// timed, so it cannot flake on a slow machine.
    #[test]
    fn test_per_name_signatures_copy_a_bounded_prefix() {
        use super::PER_NAME_BYTES;
        let names = vec!["a"; 2_000].join(",");
        let java_type = format!("List<{}A>", "A,".repeat(165_000));
        let go_type = format!("struct{{ {} int }}", "x".repeat(500_000));
        for (language, source) in [
            (
                Language::Java,
                format!("class A {{\n  {java_type} {names};\n}}\n"),
            ),
            (
                Language::CSharp,
                format!("class A {{\n  {java_type} {names};\n}}\n"),
            ),
            (
                Language::Go,
                format!("package p\n\nvar {} {go_type}\n", names.replace(',', ", ")),
            ),
        ] {
            PER_NAME_BYTES.with(|bytes| bytes.set(0));
            let extraction = ParserRegistry::extract(language, &source).expect("extracts");
            assert!(extraction.symbols.len() >= 2_000, "{language:?}");
            let copied = PER_NAME_BYTES.with(std::cell::Cell::get);
            assert!(copied > 0, "{language:?}");
            assert!(
                copied <= 2_000 * (MAX_SIGNATURE_BYTES + 64),
                "{language:?} copied {copied} bytes"
            );
        }
    }

    /// Round 3 nit: a cut kept part that ends in spaces still gives the
    /// signature the whole text would have given (Go's output unchanged),
    /// and a Java or C# signature built from one keeps its mark.
    #[test]
    fn test_a_cut_ending_in_spaces_keeps_the_whole_texts_signature_and_mark() {
        let kind = format!("{}{}{}", "x".repeat(1_028), " ".repeat(60), "y".repeat(100));
        let whole = bounded_signature(&format!("var a {kind}"));
        let kept = signature_prefix(&kind);
        assert!(is_cut(&kind));
        assert_eq!(marked_signature(&format!("var a {kept}"), true), whole);

        let java = format!(
            "class A {{\n  List<{}{}A> a;\n}}\n",
            "A,".repeat(514),
            " ".repeat(60)
        );
        let extraction = ParserRegistry::extract(Language::Java, &java).expect("extracts");
        let field = extraction
            .symbols
            .iter()
            .find(|symbol| symbol.qualified_name == "A::a")
            .and_then(|symbol| symbol.signature.as_deref())
            .expect("field signature");
        assert!(
            field.len() <= MAX_SIGNATURE_BYTES && field.ends_with('…'),
            "{field}"
        );
    }

    /// E17-46: the Go binding scan visits each node of a function once, so
    /// a function of 100,000 statements, each binding a name and calling
    /// one, costs a pass proportional to its size, not to statements times
    /// calls; counted, not timed.
    #[test]
    fn test_go_binding_scan_is_one_pass_per_function() {
        let visits = |statements: usize| {
            let body = (0..statements)
                .map(|index| format!("\tv{index} := f{index}()\n"))
                .collect::<String>();
            let source = format!("package p\n\nfunc big() {{\n{body}}}\n");
            super::calls::BINDING_NODES.with(|count| count.set(0));
            let extraction = ParserRegistry::extract(Language::Go, &source).expect("extracts");
            let nodes = super::calls::BINDING_NODES.with(std::cell::Cell::get);
            (nodes, extraction.calls.len())
        };
        let (small, small_calls) = visits(50_000);
        let (large, large_calls) = visits(100_000);
        assert!(small_calls > 0 && large_calls > 0);
        // About ten nodes per statement, and twice the statements cost
        // twice the visits.
        assert!(large <= 100_000 * 16, "{large}");
        assert!(large <= small * 2 + 16, "{small} {large}");
    }
}
