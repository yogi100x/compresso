use compresso_atlas::parsers::{
    ImportKind, ImportResolution, Language, ParseError, ParserRegistry,
};
use compresso_store::SymbolKind;

const SAMPLE: &str = include_str!("fixtures/lang/sample.js");

#[test]
fn test_js_functions_and_class() {
    let extraction = ParserRegistry::extract(Language::JavaScript, SAMPLE)
        .expect("JavaScript fixture should extract");
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
                "connect",
                "connect",
                SymbolKind::Function,
                1,
                3,
                Some("function connect(url)"),
                0,
            ),
            (
                "retry",
                "retry",
                SymbolKind::Function,
                5,
                7,
                Some("const retry = async (fn) =>"),
                0,
            ),
            (
                "Pool",
                "Pool",
                SymbolKind::Class,
                9,
                13,
                Some("class Pool"),
                0,
            ),
            (
                "acquire",
                "Pool::acquire",
                SymbolKind::Method,
                10,
                12,
                Some("acquire()"),
                0,
            ),
        ]
    );
}

#[test]
fn test_js_cjs_and_esm_imports_in_source_order() {
    let source = r#"const path = require("path");
import express, { Router as ExpressRouter } from "express";
const fs = require('node:fs');
import * as utilities from "./utilities.js";
"#;
    let extraction = ParserRegistry::extract(Language::JavaScript, source)
        .expect("JavaScript dependencies should extract");
    let actual = extraction
        .imports
        .iter()
        .map(|import| (import.raw.as_str(), import.kind))
        .collect::<Vec<_>>();
    assert_eq!(
        actual,
        [
            ("path", ImportKind::Require),
            ("express", ImportKind::Import),
            ("node:fs", ImportKind::Require),
            ("./utilities.js", ImportKind::Import),
        ]
    );
    assert!(extraction.imports.iter().all(|import| {
        import.resolved_path.is_none()
            && import.resolution == ImportResolution::Unresolved("not_indexed".to_owned())
    }));
}

#[test]
fn test_js_dynamic_and_noncanonical_require_ignored() {
    let source = r#"const plugin = require(pluginName);
const templated = require(`plugin-${name}`);
const destructured = require("valid-static");
const { readFile } = require("node:fs/promises");
loader.require("not-cjs");
"#;
    let extraction = ParserRegistry::extract(Language::JavaScript, source)
        .expect("dynamic requires should be ignored safely");
    let imports = extraction
        .imports
        .iter()
        .map(|import| import.raw.as_str())
        .collect::<Vec<_>>();
    assert_eq!(imports, ["valid-static", "node:fs/promises"]);
    assert!(
        extraction.symbols.is_empty(),
        "non-function consts and destructuring must not become symbols"
    );
}

#[test]
fn test_js_const_arrow_and_function_expression_only() {
    let source = r#"const arrow = (value) => value;
const expression = function (value) { return value; };
let mutableArrow = () => 1;
const plain = 1;
const { destructured = () => 1 } = source;
"#;
    let extraction = ParserRegistry::extract(Language::JavaScript, source)
        .expect("JavaScript const functions should extract");
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
                "arrow",
                SymbolKind::Function,
                Some("const arrow = (value) =>")
            ),
            (
                "expression",
                SymbolKind::Function,
                Some("const expression = function (value)")
            ),
        ]
    );
}

#[test]
fn test_js_module_exports_assignment() {
    let source = r#"module.exports = { connect, retry };
if (process.env.HIDDEN) { module.exports = hidden; }
module.exports.handler = connect;
"#;
    let extraction = ParserRegistry::extract(Language::JavaScript, source)
        .expect("module exports should extract");
    assert_eq!(extraction.symbols.len(), 1);
    let export = &extraction.symbols[0];
    assert_eq!(export.name, "module.exports");
    assert_eq!(export.qualified_name, "module.exports");
    assert_eq!(export.kind, SymbolKind::Module);
    assert_eq!((export.start_line, export.end_line), (1, 1));
    assert_eq!(
        export.signature.as_deref(),
        Some("module.exports = { connect, retry }")
    );
}

#[test]
fn test_js_nested_object_and_anonymous_defaults_are_skipped() {
    let source = r#"export default function () {}
export default class {}
export default function namedDefault() {}
export default class NamedDefault {}
function outer() {
  function nested() {}
  const nestedArrow = () => 1;
  const object = { method() {} };
  return object;
}
"#;
    let extraction = ParserRegistry::extract(Language::JavaScript, source)
        .expect("unstable JavaScript declarations should be skipped safely");
    let names = extraction
        .symbols
        .iter()
        .map(|symbol| symbol.qualified_name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(names, ["namedDefault", "NamedDefault", "outer"]);
}

#[test]
fn test_jsx_routes_through_javascript_grammar() {
    let source = "export function Card({ name }) { return <article>{name}</article>; }";
    let extraction = ParserRegistry::extract(Language::JavaScript, source)
        .expect("JavaScript grammar should accept JSX");
    assert_eq!(extraction.symbols[0].qualified_name, "Card");
    assert_eq!(extraction.symbols[0].kind, SymbolKind::Function);
}

#[test]
fn test_javascript_malformed_returns_partial_without_panic() {
    let outcome = std::panic::catch_unwind(|| {
        ParserRegistry::extract(
            Language::JavaScript,
            "function intact() {}\nclass Broken { method(",
        )
    });
    assert!(outcome.is_ok(), "malformed JavaScript must never panic");
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
