use std::path::Path;

use compresso_atlas::parsers::{
    ImportKind, ImportResolution, Language, ParseError, ParserRegistry,
};
use compresso_store::SymbolKind;

const SAMPLE: &str = include_str!("fixtures/lang/sample.rs");

#[test]
fn test_for_path_extension_map() {
    for (path, expected) in [
        ("foo.rs", Some(Language::Rust)),
        ("foo.ts", Some(Language::TypeScript)),
        ("foo.tsx", Some(Language::Tsx)),
        ("foo.js", Some(Language::JavaScript)),
        ("foo.jsx", Some(Language::JavaScript)),
        ("foo.mjs", Some(Language::JavaScript)),
        ("foo.cjs", Some(Language::JavaScript)),
        ("foo.py", Some(Language::Python)),
        ("foo.go", None),
        ("README", None),
    ] {
        assert_eq!(
            ParserRegistry::for_path(Path::new(path)),
            expected,
            "{path}"
        );
    }
}

#[test]
fn test_rust_extracts_fn_struct_impl() {
    let extraction =
        ParserRegistry::extract(Language::Rust, SAMPLE).expect("Rust fixture should extract");
    let actual = extraction
        .symbols
        .iter()
        .map(|symbol| {
            (
                symbol.name.as_str(),
                symbol.qualified_name.as_str(),
                symbol.kind,
                symbol.start_line,
                symbol.end_line,
                symbol.signature.as_deref(),
                symbol.token_estimate,
            )
        })
        .collect::<Vec<_>>();

    assert_eq!(
        actual,
        [
            (
                "SessionManager",
                "SessionManager",
                SymbolKind::Struct,
                3,
                7,
                Some("pub struct SessionManager"),
                0,
            ),
            (
                "start",
                "SessionManager::start",
                SymbolKind::Method,
                10,
                14,
                Some("pub fn start(&self) -> Result<(), Error>"),
                0,
            ),
            (
                "helper",
                "helper",
                SymbolKind::Function,
                17,
                19,
                Some("pub fn helper()"),
                0,
            ),
        ]
    );
}

#[test]
fn test_rust_imports_as_written() {
    let extraction =
        ParserRegistry::extract(Language::Rust, SAMPLE).expect("Rust fixture should extract");
    let raw = extraction
        .imports
        .iter()
        .map(|import| import.raw.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        raw,
        [
            "crate::graph::ContextGraph",
            "serde::Serialize",
            "serde::Deserialize"
        ]
    );
    assert!(extraction.imports.iter().all(|import| {
        import.kind == ImportKind::Use
            && import.resolved_path.is_none()
            && import.resolution == ImportResolution::Unresolved("not_indexed".to_owned())
    }));
}

#[test]
fn test_rust_extracts_enum_trait_const_type_and_module() {
    let source = r#"pub enum State { Ready, Closed }
pub trait Runner { fn run(&self); }
pub const MAX_RETRIES: usize = 3;
pub type UserId = u64;
pub mod session { pub fn nested() {} }
"#;
    let extraction =
        ParserRegistry::extract(Language::Rust, source).expect("Rust source should extract");
    let actual = extraction
        .symbols
        .iter()
        .map(|symbol| {
            (
                symbol.qualified_name.as_str(),
                symbol.kind,
                symbol.start_line,
                symbol.end_line,
                symbol.signature.as_deref(),
            )
        })
        .collect::<Vec<_>>();

    assert_eq!(
        actual,
        [
            ("State", SymbolKind::Enum, 1, 1, Some("pub enum State")),
            ("Runner", SymbolKind::Trait, 2, 2, Some("pub trait Runner")),
            (
                "MAX_RETRIES",
                SymbolKind::Const,
                3,
                3,
                Some("pub const MAX_RETRIES: usize = 3")
            ),
            (
                "UserId",
                SymbolKind::Type,
                4,
                4,
                Some("pub type UserId = u64")
            ),
            ("session", SymbolKind::Module, 5, 5, Some("pub mod session")),
            (
                "nested",
                SymbolKind::Function,
                5,
                5,
                Some("pub fn nested()")
            ),
        ]
    );
}

#[test]
fn test_rust_grouped_use_expansion_is_recursive_and_ordered() {
    let source = "use crate::{graph::{ContextGraph, Edge}, tokens::Counter, self};";
    let extraction =
        ParserRegistry::extract(Language::Rust, source).expect("Rust imports should extract");
    let actual = extraction
        .imports
        .iter()
        .map(|import| import.raw.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        actual,
        [
            "crate::graph::ContextGraph",
            "crate::graph::Edge",
            "crate::tokens::Counter",
            "crate"
        ]
    );
}

#[test]
fn test_rust_malformed_no_panic() {
    let outcome = std::panic::catch_unwind(|| {
        ParserRegistry::extract(Language::Rust, "pub fn intact() {}\nfn broken( {")
    });
    assert!(outcome.is_ok(), "malformed Rust must never panic");
    match outcome.expect("panic outcome should be available") {
        Ok(extraction) => assert!(
            extraction
                .symbols
                .iter()
                .any(|symbol| symbol.name == "intact"),
            "complete symbols before malformed input should survive"
        ),
        Err(ParseError::ParseFailed) => {}
        Err(error) => panic!("unexpected malformed-source error: {error}"),
    }
}

#[test]
fn test_unimplemented_language_is_typed_unsupported_language() {
    assert!(matches!(
        ParserRegistry::extract(Language::JavaScript, "export const value = 1;"),
        Err(ParseError::UnsupportedLanguage(Language::JavaScript))
    ));
}
