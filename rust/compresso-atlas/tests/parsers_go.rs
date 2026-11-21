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
                Some("const defaultName"),
            ),
            (
                "backupName",
                "backupName",
                SymbolKind::Const,
                16,
                16,
                Some("const backupName"),
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
    // E17-46 (D111): `f` is bound in `outer` (`f := func() ...`), so `f()`
    // may call a local value: it is recorded under the shadow qualifier
    // (trace reports it unplaced), not as a plain call.
    assert!(
        calls.contains(&("outer", "f", Some("<shadowed>"), CallForm::Path, 8)),
        "{calls:?}"
    );
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

#[test]
fn test_go_multi_name_value_signatures_are_per_name_and_bounded() {
    let names = (0..2_000)
        .map(|index| format!("a{index}"))
        .collect::<Vec<_>>();
    let values = vec!["0"; names.len()];
    let source = format!(
        "package p\n\nvar {} = {}\n\nconst Long = \"{}\"\n",
        names.join(", "),
        values.join(", "),
        "x".repeat(4_000)
    );
    let extraction = ParserRegistry::extract(Language::Go, &source).expect("Go source extracts");
    assert_eq!(extraction.symbols.len(), 2_001);
    assert_eq!(extraction.symbols[0].signature.as_deref(), Some("var a0"));
    assert_eq!(
        extraction.symbols[1_999].signature.as_deref(),
        Some("var a1999")
    );
    let total = extraction
        .symbols
        .iter()
        .map(|symbol| symbol.signature.as_deref().unwrap_or_default().len())
        .sum::<usize>();
    assert!(total < source.len(), "{total} signature bytes");
    let long = extraction.symbols.last().expect("Long");
    assert!(long.signature.as_deref().unwrap_or_default().len() <= 1_024);
}

fn const_block(count: usize) -> String {
    let mut source = String::from("package p\n\nconst (\n");
    for index in 0..count {
        source.push_str(&format!("\tC{index} = {index}\n"));
    }
    source.push_str(")\n");
    source
}

/// A grouped declaration is expanded in time linear in its size: eight
/// times the specs cost well under eight squared times the time.
#[test]
fn test_go_huge_const_block_extracts_in_bounded_time() {
    let time = |count: usize| {
        let source = const_block(count);
        let started = std::time::Instant::now();
        let extraction =
            ParserRegistry::extract(Language::Go, &source).expect("Go source extracts");
        assert_eq!(extraction.symbols.len(), count);
        started.elapsed()
    };
    let small = time(30_000);
    let large = time(240_000);
    eprintln!("30k specs {small:?}, 240k specs {large:?}");
    assert!(
        large < small * 14,
        "30k specs {small:?}, 240k specs {large:?}"
    );
}

#[test]
fn test_go_blank_function_and_type_names_are_not_symbols() {
    let source = "package p\n\nfunc _() {}\n\ntype _ struct{}\n\nvar _ = 1\n\nfunc Kept() {}\n";
    let extraction = ParserRegistry::extract(Language::Go, source).expect("Go source extracts");
    let names = extraction
        .symbols
        .iter()
        .map(|symbol| symbol.qualified_name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(names, ["Kept"]);
}

#[test]
fn test_go_indexed_function_values_are_not_calls_to_the_slice() {
    let source =
        "package p\n\nfunc run(handlers []func()) {\n\thandlers[0]()\n\tGeneric[int]()\n}\n";
    let extraction = ParserRegistry::extract(Language::Go, source).expect("Go source extracts");
    let calls = call_rows(&extraction);
    assert!(
        !calls
            .iter()
            .any(|row| row.1 == "handlers" && row.3 == CallForm::Plain),
        "{calls:?}"
    );
}

/// E17-40 review M1: a `var` spec of 250,000 names with a 500 KB type
/// copies the type text once, not once per name.
#[test]
fn test_go_many_value_names_with_a_huge_type_are_linear() {
    let fields = "x".repeat(500_000);
    let names = vec!["a"; 250_000].join(", ");
    let source = format!("package p\n\nvar {names} struct{{ {fields} int }}\n");
    let started = std::time::Instant::now();
    let extraction = ParserRegistry::extract(Language::Go, &source).expect("extracts");
    let elapsed = started.elapsed();
    eprintln!("Go: 250,000 names with a 500 KB type in {elapsed:?}");
    assert_eq!(extraction.symbols.len(), 250_000);
    assert!(extraction.symbols.iter().all(|symbol| {
        symbol
            .signature
            .as_deref()
            .is_some_and(|text| text.len() <= 1_024)
    }));
    assert!(
        elapsed < std::time::Duration::from_secs(20),
        "took {elapsed:?}"
    );
}
