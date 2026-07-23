use std::fs;

use compresso_atlas::parsers::{Language, ParserRegistry};
use compresso_store::SymbolKind;

#[test]
fn test_shell_extracts_script_summary_and_functions_without_execution() {
    let temp = tempfile::tempdir().expect("temporary directory should be created");
    let canary = temp.path().join("parser-must-not-run");
    let source = format!(
        r#"#!/usr/bin/env bash
# Refresh Instagram long-lived access token.
# Reads INSTAGRAM_ACCESS_TOKEN and replaces the stored value.
TOKEN_FILE="${{PROJECT_ROOT}}/.env"

load_token() {{
  printf '%s\n' "$INSTAGRAM_ACCESS_TOKEN"
}}

function refresh_token {{
  touch '{}'
  curl --fail "https://graph.instagram.com/refresh_access_token"
}}
"#,
        canary.display()
    );

    let extraction =
        ParserRegistry::extract(Language::Shell, &source).expect("shell should extract");

    assert!(
        !canary.exists(),
        "indexing shell source must never execute it"
    );
    let script = &extraction.symbols[0];
    assert_eq!(script.qualified_name, "script");
    assert_eq!(script.kind, SymbolKind::Module);
    assert_eq!(script.start_line, 1);
    assert_eq!(script.end_line, 13);
    let signature = script
        .signature
        .as_deref()
        .expect("script summary should be searchable");
    assert!(signature.contains("Refresh Instagram long-lived access token"));
    assert!(signature.contains("INSTAGRAM_ACCESS_TOKEN"));
    assert!(!signature.contains(canary.to_string_lossy().as_ref()));

    let functions = extraction
        .symbols
        .iter()
        .skip(1)
        .map(|symbol| {
            (
                symbol.qualified_name.as_str(),
                symbol.kind,
                symbol.start_line,
                symbol.end_line,
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        functions,
        [
            ("load_token", SymbolKind::Function, 6, 8),
            ("refresh_token", SymbolKind::Function, 10, 13),
        ]
    );
    assert!(extraction.imports.is_empty());
}

#[test]
fn test_shell_malformed_is_partial_deterministic_and_bounded() {
    let mut source =
        String::from("#!/bin/sh\n# Session bootstrap\nintact() {\n  printf ok\n}\nbroken() {\n");
    for index in 0..2_000 {
        source.push_str(&format!("fn_{index}() {{ printf '%s' '{index}'; }}\n"));
    }

    let first =
        ParserRegistry::extract(Language::Shell, &source).expect("malformed shell should degrade");
    let second =
        ParserRegistry::extract(Language::Shell, &source).expect("repeat should also degrade");

    assert_eq!(first, second);
    assert!(
        first
            .symbols
            .iter()
            .any(|symbol| symbol.qualified_name == "intact")
    );
    assert!(
        first.symbols.len() <= 257,
        "one script summary plus at most 256 definitions"
    );
}

#[test]
fn test_generic_text_extracts_searchable_bounded_sections() {
    let source = r#"# Authentication

The CLI uses OAuth device login and stores a refresh token locally.

## Sessions

Session cookies are validated before protected commands run.

## Logout

Logout revokes credentials and clears the local account.
"#;

    let extraction =
        ParserRegistry::extract(Language::Text, source).expect("generic text should extract");
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

    assert_eq!(actual.len(), 3);
    assert_eq!(actual[0].0, "Authentication");
    assert_eq!(actual[0].1, SymbolKind::Module);
    assert_eq!((actual[0].2, actual[0].3), (1, 4));
    assert!(
        actual[0]
            .4
            .is_some_and(|value| value.contains("OAuth device login"))
    );
    assert_eq!(actual[1].0, "Sessions");
    assert_eq!((actual[1].2, actual[1].3), (5, 8));
    assert_eq!(actual[2].0, "Logout");
    assert_eq!((actual[2].2, actual[2].3), (9, 11));
    assert!(extraction.imports.is_empty());
}

#[test]
fn test_generic_text_plain_paragraphs_and_resource_caps() {
    let mut source = String::new();
    for index in 0..1_000 {
        source.push_str(&format!(
            "Paragraph {index} describes credential rotation and session validation.\n\n"
        ));
    }

    let extraction =
        ParserRegistry::extract(Language::Text, &source).expect("plain text should extract");

    assert_eq!(extraction.symbols.len(), 256);
    assert_eq!(extraction.symbols[0].qualified_name, "text@1");
    assert_eq!(extraction.symbols[1].qualified_name, "text@3");
    assert!(extraction.symbols.iter().all(|symbol| {
        symbol
            .signature
            .as_ref()
            .is_some_and(|value| value.len() <= 1_024)
    }));
}

#[test]
fn test_generic_text_empty_input_has_no_invalid_symbol() {
    let extraction =
        ParserRegistry::extract(Language::Text, "").expect("empty text should be accepted");
    assert!(extraction.symbols.is_empty());
    assert!(extraction.imports.is_empty());
}

#[test]
fn test_shell_extraction_does_not_touch_filesystem() {
    let temp = tempfile::tempdir().expect("temporary directory should be created");
    let before = fs::read_dir(temp.path())
        .expect("directory should be readable")
        .count();
    let source = "remove_everything() { rm -rf /; }\n";

    ParserRegistry::extract(Language::Shell, source).expect("shell should extract");

    let after = fs::read_dir(temp.path())
        .expect("directory should remain readable")
        .count();
    assert_eq!(before, after);
}
