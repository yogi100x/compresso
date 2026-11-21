//! E17-42 (D107): SQL definitions through the language table.

use std::path::Path;

use compresso_atlas::parsers::{Language, ParserRegistry};
use compresso_store::SymbolKind;

const SAMPLE: &str = include_str!("fixtures/lang/sample.sql");

#[test]
fn test_sql_files_select_the_sql_extractor() {
    assert_eq!(
        ParserRegistry::for_path(Path::new("db/migrations/0001_init.sql")),
        Some(Language::Sql)
    );
    assert_eq!(Language::Sql.as_str(), "sql");
}

/// A Postgres migration with PL/pgSQL bodies, row-level security policies,
/// triggers, an enum, a partitioned table, a `DO` block, COPY data, and
/// psql meta-commands: one symbol per definition, each covering its whole
/// statement, body included.
#[test]
fn test_a_postgres_migration_lists_its_definitions_with_whole_statement_lines() {
    let extraction = ParserRegistry::extract(Language::Sql, SAMPLE).expect("never fails");
    assert!(extraction.imports.is_empty());
    let found = extraction
        .symbols
        .iter()
        .map(|symbol| {
            (
                symbol.kind,
                symbol.qualified_name.as_str(),
                symbol.start_line,
                symbol.end_line,
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        found,
        [
            (SymbolKind::Module, "(sql file)", 1, 99),
            (SymbolKind::Module, "pgcrypto", 9, 9),
            (SymbolKind::Module, "lending", 10, 10),
            (SymbolKind::Enum, "lending.loan_state", 12, 12),
            (SymbolKind::Type, "isbn", 13, 13),
            (SymbolKind::Struct, "public.books", 17, 22),
            (SymbolKind::Struct, "lending.loans", 24, 29),
            (SymbolKind::Struct, "lending.loans_2026", 31, 32),
            (SymbolKind::Const, "public.books::books_title_key", 34, 35),
            (SymbolKind::Function, "lending.touch_loan", 37, 47),
            (SymbolKind::Function, "lending.open_loans", 49, 53),
            (SymbolKind::Function, "lending.open_loans", 55, 59),
            (SymbolKind::Function, "lending.close_all", 61, 66),
            (SymbolKind::Method, "lending.loans::loans_touch", 68, 70),
            (
                SymbolKind::Method,
                "public.books::\"Readers see every book\"",
                74,
                75
            ),
            (SymbolKind::Method, "public.books::books_write", 77, 78),
            (SymbolKind::Const, "lending.ticket_seq", 80, 80),
            (SymbolKind::Struct, "lending.open_counts", 82, 83),
        ]
    );
    let names = extraction
        .symbols
        .iter()
        .map(|symbol| symbol.name.as_str())
        .collect::<Vec<_>>();
    assert!(names.contains(&"books") && names.contains(&"loans_touch"));
    let overloads = extraction
        .symbols
        .iter()
        .filter(|symbol| symbol.name == "open_loans")
        .map(|symbol| symbol.signature.as_deref().unwrap_or_default())
        .collect::<Vec<_>>();
    assert_eq!(
        overloads,
        [
            "CREATE OR REPLACE FUNCTION lending.open_loans(p_book uuid) RETURNS SETOF lending.loans",
            "CREATE OR REPLACE FUNCTION lending.open_loans(p_book uuid, p_since date) RETURNS SETOF lending.loans",
        ]
    );
}

/// Malformed and hostile sources never fail extraction.
#[test]
fn test_malformed_sql_never_fails_extraction() {
    for source in [
        "",
        ";;;",
        "CREATE",
        "CREATE TABLE",
        "CREATE TABLE \"unclosed",
        "CREATE FUNCTION f() AS $body$ never closed",
        "/* /* */",
        "COPY t FROM stdin;\nno terminator",
        "\\",
        "$",
        "E'\\",
        "\u{0}\u{1}\u{7f}",
        "CREATE TABLE é.ü (x int);",
    ] {
        let extraction = ParserRegistry::extract(Language::Sql, source).expect("never fails");
        let lines = u32::try_from(source.lines().count().max(1)).expect("small");
        for symbol in &extraction.symbols {
            assert!(
                symbol.start_line >= 1
                    && symbol.start_line <= symbol.end_line
                    && symbol.end_line <= lines,
                "{source:?}: {symbol:?}"
            );
        }
    }
}

/// The migration's references, as `(caller, callee, kind)` call sites.
#[test]
fn test_a_postgres_migration_records_what_each_statement_touches() {
    let extraction = ParserRegistry::extract(Language::Sql, SAMPLE).expect("never fails");
    let found = extraction
        .calls
        .iter()
        .map(|call| {
            (
                extraction.symbols[call.caller].qualified_name.as_str(),
                call.callee_name.as_str(),
                call.qualifier.as_deref().unwrap_or_default(),
            )
        })
        .collect::<Vec<_>>();
    for expected in [
        ("lending.loans", "books", "references"),
        ("lending.loans_2026", "loans", "partition_of"),
        ("public.books::books_title_key", "books", "on"),
        ("lending.loans::loans_touch", "loans", "on"),
        ("lending.loans::loans_touch", "touch_loan", "execute"),
        ("public.books::\"Readers see every book\"", "books", "on"),
        ("lending.open_counts", "loans", "from"),
        ("(sql file)", "books", "alter"),
        ("(sql file)", "books", "copy"),
        ("(sql file)", "books", "comment"),
    ] {
        assert!(found.contains(&expected), "{expected:?} in {found:?}");
    }
    // Nothing inside a function body is read.
    assert!(
        !found
            .iter()
            .any(|(caller, ..)| *caller == "lending.open_loans")
    );
}
