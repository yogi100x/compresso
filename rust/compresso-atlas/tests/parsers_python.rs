use compresso_atlas::parsers::{
    ImportKind, ImportResolution, Language, ParseError, ParserRegistry,
};
use compresso_store::SymbolKind;

const SAMPLE: &str = include_str!("fixtures/lang/sample.py");

#[test]
fn test_py_class_and_methods() {
    let extraction =
        ParserRegistry::extract(Language::Python, SAMPLE).expect("Python fixture should extract");
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
                "JobQueue",
                "JobQueue",
                SymbolKind::Class,
                1,
                6,
                Some("class JobQueue:"),
                0,
            ),
            (
                "push",
                "JobQueue::push",
                SymbolKind::Method,
                2,
                3,
                Some("def push(self, job):"),
                0,
            ),
            (
                "drain",
                "JobQueue::drain",
                SymbolKind::Method,
                5,
                6,
                Some("async def drain(self):"),
                0,
            ),
            (
                "make_queue",
                "make_queue",
                SymbolKind::Function,
                8,
                9,
                Some("def make_queue():"),
                0,
            ),
            (
                "MAX_RETRIES",
                "MAX_RETRIES",
                SymbolKind::Const,
                11,
                11,
                Some("MAX_RETRIES = 3"),
                0,
            ),
        ]
    );
}

#[test]
fn test_py_imports_three_forms_and_multiple_names() {
    let source = r#"import os, sys as system
import numpy as np
from typing import Optional, Iterable as It
from .models import User, Account as AccountModel
from .. import config
from package import *
"#;
    let extraction =
        ParserRegistry::extract(Language::Python, source).expect("Python imports should extract");
    let actual = extraction
        .imports
        .iter()
        .map(|import| (import.raw.as_str(), import.kind))
        .collect::<Vec<_>>();
    assert_eq!(
        actual,
        [
            ("os", ImportKind::Import),
            ("sys", ImportKind::Import),
            ("numpy", ImportKind::Import),
            ("typing.Optional", ImportKind::FromImport),
            ("typing.Iterable", ImportKind::FromImport),
            (".models.User", ImportKind::FromImport),
            (".models.Account", ImportKind::FromImport),
            ("..config", ImportKind::FromImport),
        ]
    );
    assert!(extraction.imports.iter().all(|import| {
        import.resolved_path.is_none()
            && import.resolution == ImportResolution::Unresolved("not_indexed".to_owned())
    }));
}

#[test]
fn test_py_decorated_line_range_and_signature() {
    let source = r#"@trace
@retry(attempts=3)
def push(job):
    return job
"#;
    let extraction = ParserRegistry::extract(Language::Python, source)
        .expect("decorated Python function should extract");
    assert_eq!(extraction.symbols.len(), 1);
    let function = &extraction.symbols[0];
    assert_eq!(function.qualified_name, "push");
    assert_eq!((function.start_line, function.end_line), (3, 4));
    assert_eq!(
        function.signature.as_deref(),
        Some("@trace\n@retry(attempts=3)\ndef push(job):")
    );
}

#[test]
fn test_py_decorated_async_method_preserves_async() {
    let source = r#"class Worker:
    @logged("jobs")
    @retry(attempts=2)
    async def drain(self) -> None:
        return None
"#;
    let extraction = ParserRegistry::extract(Language::Python, source)
        .expect("decorated async method should extract");
    assert_eq!(extraction.symbols.len(), 2);
    let method = &extraction.symbols[1];
    assert_eq!(method.qualified_name, "Worker::drain");
    assert_eq!(method.kind, SymbolKind::Method);
    assert_eq!((method.start_line, method.end_line), (4, 5));
    assert_eq!(
        method.signature.as_deref(),
        Some("@logged(\"jobs\")\n@retry(attempts=2)\nasync def drain(self) -> None:")
    );
}

#[test]
fn test_py_module_uppercase_and_annotated_constants_only() {
    let source = r#"MAX_RETRIES: int = 3
API_V2 = "v2"
lowercase = 1
Mixed_Case = 2
__all__ = ["MAX_RETRIES"]
LEFT, RIGHT = (1, 2)
class Settings:
    CLASS_CONST = 4
def configure():
    LOCAL_CONST = 5
    settings.VALUE = 6
"#;
    let extraction =
        ParserRegistry::extract(Language::Python, source).expect("Python constants should extract");
    let actual = extraction
        .symbols
        .iter()
        .map(|symbol| {
            (
                symbol.qualified_name.as_str(),
                symbol.kind,
                symbol.signature.as_deref(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        actual,
        [
            (
                "MAX_RETRIES",
                SymbolKind::Const,
                Some("MAX_RETRIES: int = 3")
            ),
            ("API_V2", SymbolKind::Const, Some("API_V2 = \"v2\"")),
            ("Settings", SymbolKind::Class, Some("class Settings:")),
            ("configure", SymbolKind::Function, Some("def configure():")),
        ]
    );
}

#[test]
fn test_py_nested_functions_and_classes_are_not_emitted() {
    let source = r#"def outer():
    def nested():
        return None
    class Nested:
        def method(self):
            return None
    return nested

class Outer:
    class Inner:
        def nested_method(self):
            return None
"#;
    let extraction = ParserRegistry::extract(Language::Python, source)
        .expect("nested Python declarations should extract safely");
    let names = extraction
        .symbols
        .iter()
        .map(|symbol| symbol.qualified_name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(names, ["outer", "Outer"]);
}

#[test]
fn test_python_malformed_returns_partial_without_panic() {
    let outcome = std::panic::catch_unwind(|| {
        ParserRegistry::extract(
            Language::Python,
            "def intact():\n    return True\n\nclass Broken:\n    def method(",
        )
    });
    assert!(outcome.is_ok(), "malformed Python must never panic");
    match outcome.expect("panic outcome should be available") {
        Ok(extraction) => assert!(
            extraction
                .symbols
                .iter()
                .any(|symbol| symbol.name == "intact")
        ),
        Err(ParseError::ParseFailed) => {}
        Err(error) => panic!("unexpected malformed-source error: {error}"),
    }
}

#[test]
fn test_python_import_with_no_module_name_is_not_extracted() {
    let extraction = ParserRegistry::extract(Language::Python, "import ,\nimport x, y,\n")
        .expect("malformed imports should still extract");
    let raw = extraction
        .imports
        .iter()
        .map(|import| import.raw.as_str())
        .collect::<Vec<_>>();

    assert_eq!(raw, ["x", "y"]);
}
