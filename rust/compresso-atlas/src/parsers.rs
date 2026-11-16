use std::collections::HashSet;
use std::path::Path;

use compresso_store::{SymbolKind, SymbolRecord};
use tree_sitter::{Node, Parser, Query, QueryCursor, StreamingIterator};

mod shell;
mod text;

const RUST_QUERY: &str = include_str!("../queries/rust.scm");
const JAVASCRIPT_QUERY: &str = include_str!("../queries/javascript.scm");
const PYTHON_QUERY: &str = include_str!("../queries/python.scm");
const TYPESCRIPT_QUERY: &str = include_str!("../queries/typescript.scm");
const TSX_QUERY: &str = include_str!("../queries/tsx.scm");
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
        match extension {
            "rs" => Some(Language::Rust),
            "ts" => Some(Language::TypeScript),
            "tsx" => Some(Language::Tsx),
            "js" | "jsx" | "mjs" | "cjs" => Some(Language::JavaScript),
            "py" => Some(Language::Python),
            "sh" | "bash" | "zsh" => Some(Language::Shell),
            "md" | "markdown" | "txt" | "rst" | "adoc" => Some(Language::Text),
            _ => None,
        }
    }

    pub fn extract(lang: Language, source: &str) -> Result<Extraction, ParseError> {
        match lang {
            Language::Rust => extract_rust(source),
            Language::TypeScript => extract_typescript(Language::TypeScript, source),
            Language::Tsx => extract_typescript(Language::Tsx, source),
            Language::JavaScript => extract_javascript(source),
            Language::Python => extract_python(source),
            Language::Shell => Ok(shell::extract(source)),
            Language::Text => Ok(text::extract(source)),
        }
    }
}

#[derive(Clone, Copy)]
enum CaptureKind {
    Symbol(SymbolKind),
    Variable,
    ModuleExport,
    PythonConstant,
    Import(ImportKind),
    Use,
}

#[derive(Clone, Copy)]
struct Candidate<'tree> {
    node: Node<'tree>,
    kind: CaptureKind,
}

fn extract_rust(source: &str) -> Result<Extraction, ParseError> {
    extract_with_query(
        tree_sitter_rust::LANGUAGE.into(),
        "Rust",
        RUST_QUERY,
        source,
        process_rust_candidate,
    )
}

fn extract_typescript(lang: Language, source: &str) -> Result<Extraction, ParseError> {
    let (language, language_name, query) = match lang {
        Language::TypeScript => (
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            "TypeScript",
            TYPESCRIPT_QUERY,
        ),
        Language::Tsx => (
            tree_sitter_typescript::LANGUAGE_TSX.into(),
            "TSX",
            TSX_QUERY,
        ),
        _ => return Err(ParseError::UnsupportedLanguage(lang)),
    };
    extract_with_query(
        language,
        language_name,
        query,
        source,
        process_typescript_candidate,
    )
}

fn extract_javascript(source: &str) -> Result<Extraction, ParseError> {
    extract_with_query(
        tree_sitter_javascript::LANGUAGE.into(),
        "JavaScript",
        JAVASCRIPT_QUERY,
        source,
        process_javascript_candidate,
    )
}

fn extract_python(source: &str) -> Result<Extraction, ParseError> {
    extract_with_query(
        tree_sitter_python::LANGUAGE.into(),
        "Python",
        PYTHON_QUERY,
        source,
        process_python_candidate,
    )
}

fn extract_with_query(
    language: tree_sitter::Language,
    language_name: &'static str,
    query_source: &str,
    source: &str,
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
    let query = Query::new(&language, query_source).map_err(|error| ParseError::Query {
        language: language_name,
        message: error.to_string(),
    })?;
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

    let mut extraction = Extraction {
        symbols: Vec::new(),
        imports: Vec::new(),
    };
    for candidate in candidates {
        process_candidate(candidate, source, &mut extraction);
    }
    // Error recovery can yield an import whose module name is a zero-width
    // placeholder (`import ,`). It names nothing, and an empty edge target is
    // rejected by the store, which would abort the whole repository index.
    extraction
        .imports
        .retain(|import| !import.raw.trim().is_empty());

    Ok(extraction)
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
