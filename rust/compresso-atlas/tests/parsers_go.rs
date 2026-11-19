use std::path::Path;

use compresso_atlas::parsers::{
    Extraction, ImportKind, ImportResolution, LANGUAGES, Language, ParserRegistry,
};
use compresso_store::{CallForm, SymbolKind};

const SAMPLE: &str = include_str!("fixtures/lang/sample.go");

/// (caller qualified name, callee, qualifier, form, line)
type Row<'a> = (&'a str, &'a str, Option<&'a str>, CallForm, u32);

fn call_rows(extraction: &Extraction) -> Vec<Row<'_>> {
    extraction
        .calls
        .iter()
        .map(|call| {
            (
                extraction.symbols[call.caller].qualified_name.as_str(),
                call.callee_name.as_str(),
                call.qualifier.as_deref(),
                call.form,
                call.line,
            )
        })
        .collect()
}

#[test]
fn test_go_files_select_the_go_parser() {
    assert_eq!(
        ParserRegistry::for_path(Path::new("cmd/server/main.go")),
        Some(Language::Go)
    );
    assert_eq!(
        ParserRegistry::for_path(Path::new("store/db_test.go")),
        Some(Language::Go)
    );
    assert_eq!(Language::Go.as_str(), "go");
}

#[test]
fn test_go_functions_methods_types_and_package_values() {
    let extraction = ParserRegistry::extract(Language::Go, SAMPLE).expect("Go fixture extracts");
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
            )
        })
        .collect::<Vec<_>>();

    assert_eq!(
        actual,
        [
            (
                "MaxRetries",
                "MaxRetries",
                SymbolKind::Const,
                13,
                13,
                Some("const MaxRetries = 3"),
            ),
            (
                "defaultName",
                "defaultName",
                SymbolKind::Const,
                16,
                16,
                Some("const defaultName, backupName = \"main\", \"backup\""),
            ),
            (
                "backupName",
                "backupName",
                SymbolKind::Const,
                16,
                16,
                Some("const defaultName, backupName = \"main\", \"backup\""),
            ),
            (
                "ErrClosed",
                "ErrClosed",
                SymbolKind::Const,
                20,
                20,
                Some("var ErrClosed = errors.New(\"closed\")"),
            ),
            (
                "Runner",
                "Runner",
                SymbolKind::Interface,
                22,
                24,
                Some("type Runner interface"),
            ),
            (
                "Queue",
                "Queue",
                SymbolKind::Struct,
                26,
                28,
                Some("type Queue[T any] struct"),
            ),
            (
                "ID",
                "ID",
                SymbolKind::Type,
                30,
                30,
                Some("type ID = string")
            ),
            (
                "Handler",
                "Handler",
                SymbolKind::Type,
                32,
                32,
                Some("type Handler func(ctx context.Context) error"),
            ),
            (
                "NewQueue",
                "NewQueue",
                SymbolKind::Function,
                34,
                36,
                Some("func NewQueue[T any]() *Queue[T]"),
            ),
            (
                "Push",
                "Queue::Push",
                SymbolKind::Method,
                38,
                41,
                Some("func (q *Queue[T]) Push(item T)"),
            ),
            (
                "grow",
                "Queue::grow",
                SymbolKind::Method,
                43,
                43,
                Some("func (q Queue[T]) grow()"),
            ),
            (
                "run",
                "run",
                SymbolKind::Function,
                45,
                52,
                Some("func run(ctx context.Context) error"),
            ),
        ]
    );
}

#[test]
fn test_go_imports_are_unquoted_package_paths() {
    let extraction = ParserRegistry::extract(Language::Go, SAMPLE).expect("Go fixture extracts");
    let imports = extraction
        .imports
        .iter()
        .map(|import| (import.raw.as_str(), import.kind, &import.resolution))
        .collect::<Vec<_>>();
    let unresolved = ImportResolution::Unresolved("not_indexed".to_owned());
    assert_eq!(
        imports,
        [
            ("context", ImportKind::Import, &unresolved),
            ("fmt", ImportKind::Import, &unresolved),
            (
                "example.com/svc/internal/store",
                ImportKind::Import,
                &unresolved
            ),
            ("errors", ImportKind::Import, &unresolved),
        ]
    );
}

#[test]
fn test_go_calls_record_receiver_package_and_plain_forms() {
    let extraction = ParserRegistry::extract(Language::Go, SAMPLE).expect("Go fixture extracts");
    assert_eq!(
        call_rows(&extraction),
        [
            ("Queue::Push", "item", None, CallForm::Reference, 39),
            ("Queue::Push", "grow", Some("q"), CallForm::SelfRef, 40),
            ("run", "NewQueue", None, CallForm::Plain, 47),
            ("run", "Push", Some("q"), CallForm::Path, 48),
            ("run", "local", None, CallForm::Reference, 48),
            ("run", "Println", Some("fmt"), CallForm::Path, 49),
            ("run", "Open", Some("store"), CallForm::Path, 50),
            ("run", "ctx", None, CallForm::Reference, 50),
            ("run", "Handler", None, CallForm::Plain, 51),
            ("run", "run", None, CallForm::Reference, 51),
        ]
    );
    assert!(!extraction.calls_truncated);
}

#[test]
fn test_go_builtins_and_conversions_are_not_calls() {
    let source = "package p\n\nfunc f(b []byte) int {\n\ts := string(b)\n\treturn len(s) + cap(b) + int(min(1, 2))\n}\n";
    let extraction = ParserRegistry::extract(Language::Go, source).expect("Go source extracts");
    assert_eq!(
        call_rows(&extraction),
        [
            ("f", "b", None, CallForm::Reference, 4),
            ("f", "s", None, CallForm::Reference, 5),
        ]
    );
}

#[test]
fn test_go_local_declarations_and_func_literals_are_not_symbols() {
    let source = "package p\n\nfunc outer() {\n\ttype local struct{}\n\tvar v = 1\n\tconst c = 2\n\tf := func() { inner() }\n\tf()\n\t_ = v\n}\n";
    let extraction = ParserRegistry::extract(Language::Go, source).expect("Go source extracts");
    let names = extraction
        .symbols
        .iter()
        .map(|symbol| symbol.qualified_name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(names, ["outer"]);
    let calls = call_rows(&extraction);
    assert!(calls.contains(&("outer", "inner", None, CallForm::Plain, 7)));
    assert!(calls.contains(&("outer", "f", None, CallForm::Plain, 8)));
}

#[test]
fn test_go_malformed_source_still_extracts_complete_items() {
    let source = "package p\n\nfunc ok() {}\n\nfunc broken( {\n\nimport \"\"\n";
    let extraction = ParserRegistry::extract(Language::Go, source).expect("Go source extracts");
    assert!(
        extraction
            .symbols
            .iter()
            .any(|symbol| symbol.qualified_name == "ok")
    );
    assert!(
        extraction
            .imports
            .iter()
            .all(|import| !import.raw.is_empty())
    );
}

#[test]
fn test_every_table_language_has_unique_extensions_and_parses_its_sample() {
    let mut seen = std::collections::BTreeSet::new();
    for spec in LANGUAGES {
        assert!(
            !spec.extensions.is_empty(),
            "{} has no extension",
            spec.name
        );
        for extension in spec.extensions {
            assert!(seen.insert(*extension), "{extension} is listed twice");
            assert_eq!(
                ParserRegistry::for_path(Path::new(&format!("dir/file.{extension}"))),
                Some(spec.language),
                "{extension} must select {}",
                spec.name
            );
        }
        ParserRegistry::extract(spec.language, spec.smoke_sample)
            .unwrap_or_else(|error| panic!("{} sample must parse: {error}", spec.name));
    }
    assert!(
        LANGUAGES
            .iter()
            .any(|spec| spec.language == Language::Go && spec.has_grammar())
    );
}
