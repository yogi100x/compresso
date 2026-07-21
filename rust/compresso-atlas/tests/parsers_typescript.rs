use compresso_atlas::parsers::{
    ImportKind, ImportResolution, Language, ParseError, ParserRegistry,
};
use compresso_store::SymbolKind;

const TYPESCRIPT_SAMPLE: &str = include_str!("fixtures/lang/sample.ts");
const TSX_SAMPLE: &str = include_str!("fixtures/lang/sample.tsx");

#[test]
fn test_ts_extracts_class_and_interface() {
    let extraction = ParserRegistry::extract(Language::TypeScript, TYPESCRIPT_SAMPLE)
        .expect("TypeScript fixture should extract");
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
                "RetryPolicy",
                "RetryPolicy",
                SymbolKind::Interface,
                1,
                4,
                Some("export interface RetryPolicy"),
                0,
            ),
            (
                "HttpClient",
                "HttpClient",
                SymbolKind::Class,
                6,
                10,
                Some("export class HttpClient"),
                0,
            ),
            (
                "get",
                "HttpClient::get",
                SymbolKind::Method,
                7,
                9,
                Some("async get(url: string): Promise<Response>"),
                0,
            ),
            (
                "parseUrl",
                "parseUrl",
                SymbolKind::Function,
                12,
                12,
                Some("export const parseUrl = (s: string) =>"),
                0,
            ),
        ]
    );
}

#[test]
fn test_ts_imports() {
    let source = r#"import { z } from "zod";
import fs from "node:fs";
export { helper } from "./helper";
export * from './all';
"#;
    let extraction = ParserRegistry::extract(Language::TypeScript, source)
        .expect("TypeScript imports should extract");
    let actual = extraction
        .imports
        .iter()
        .map(|import| (import.raw.as_str(), import.kind))
        .collect::<Vec<_>>();
    assert_eq!(
        actual,
        [
            ("zod", ImportKind::Import),
            ("node:fs", ImportKind::Import),
            ("./helper", ImportKind::ExportFrom),
            ("./all", ImportKind::ExportFrom),
        ]
    );
    assert!(extraction.imports.iter().all(|import| {
        import.resolved_path.is_none()
            && import.resolution == ImportResolution::Unresolved("not_indexed".to_owned())
    }));
}

#[test]
fn test_tsx_component_extracted_with_distinct_dialect() {
    let extraction = ParserRegistry::extract(Language::Tsx, TSX_SAMPLE)
        .expect("TSX fixture with JSX should extract");
    assert_eq!(extraction.symbols.len(), 1);
    let component = &extraction.symbols[0];
    assert_eq!(component.qualified_name, "UserCard");
    assert_eq!(component.kind, SymbolKind::Function);
    assert_eq!((component.start_line, component.end_line), (1, 3));
    assert_eq!(
        component.signature.as_deref(),
        Some("export function UserCard(props: { name: string }): JSX.Element")
    );

    let arrow = ParserRegistry::extract(
        Language::Tsx,
        "export const Badge = () => <span>ready</span>;",
    )
    .expect("TSX arrow component should extract");
    assert_eq!(arrow.symbols[0].qualified_name, "Badge");
    assert_eq!(arrow.symbols[0].kind, SymbolKind::Function);
}

#[test]
fn test_type_alias_enum_and_exported_const_kinds() {
    let source = r#"export type UserId = string;
export enum Status { Ready, Closed }
export const API_VERSION = "v1";
const internalValue = 1;
let mutableArrow = () => 1;
"#;
    let extraction = ParserRegistry::extract(Language::TypeScript, source)
        .expect("TypeScript declarations should extract");
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
                "UserId",
                SymbolKind::Type,
                Some("export type UserId = string")
            ),
            ("Status", SymbolKind::Enum, Some("export enum Status")),
            (
                "API_VERSION",
                SymbolKind::Const,
                Some("export const API_VERSION = \"v1\"")
            ),
        ]
    );
}

#[test]
fn test_nested_declarations_and_object_methods_are_not_module_symbols() {
    let source = r#"export function outer() {
  function nested() {}
  const nestedArrow = () => 1;
  const object = { method() {} };
  return object;
}
"#;
    let extraction = ParserRegistry::extract(Language::TypeScript, source)
        .expect("nested TypeScript should extract safely");
    let names = extraction
        .symbols
        .iter()
        .map(|symbol| symbol.qualified_name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(names, ["outer"]);
}

#[test]
fn test_overloads_and_interface_methods_are_not_emitted() {
    let source = r#"export interface Api {
  fetch(id: string): string;
  fetch(id: number): string;
}
export function format(value: string): string;
export function format(value: number): string;
export function format(value: string | number): string { return String(value); }
export class Client {
  fetch(id: string): string;
  fetch(id: number): string;
  fetch(id: string | number): string { return String(id); }
}
"#;
    let extraction = ParserRegistry::extract(Language::TypeScript, source)
        .expect("overloaded TypeScript should extract");
    let names = extraction
        .symbols
        .iter()
        .map(|symbol| symbol.qualified_name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(names, ["Api", "format", "Client", "Client::fetch"]);
}

#[test]
fn test_anonymous_defaults_are_skipped_but_named_defaults_extract() {
    let source = r#"export default function () {}
export default class {}
export default function namedDefault() {}
export default class NamedDefault {}
"#;
    let extraction = ParserRegistry::extract(Language::TypeScript, source)
        .expect("default declarations should extract safely");
    let names = extraction
        .symbols
        .iter()
        .map(|symbol| symbol.qualified_name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(names, ["namedDefault", "NamedDefault"]);
}

#[test]
fn test_typescript_malformed_returns_partial_without_panic() {
    let outcome = std::panic::catch_unwind(|| {
        ParserRegistry::extract(
            Language::TypeScript,
            "export function intact() {}\nexport class Broken { method(",
        )
    });
    assert!(outcome.is_ok(), "malformed TypeScript must never panic");
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
