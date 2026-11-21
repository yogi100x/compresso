//! Java and C# extraction (E17-40, D105): declarations by kind and owner,
//! imports as written, call sites by form, and bounds on hostile input.

use std::path::Path;
use std::time::{Duration, Instant};

use compresso_atlas::parsers::{Extraction, Language, ParserRegistry};
use compresso_store::{CallForm, SymbolKind};

const JAVA: &str = include_str!("fixtures/lang/sample.java");
const CSHARP: &str = include_str!("fixtures/lang/sample.cs");

/// Debug builds parse several times slower than release builds; the bounds
/// below are generous for a debug build and still fail a quadratic pass.
const BOUND: Duration = Duration::from_secs(20);

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

fn symbol_rows(extraction: &Extraction) -> Vec<(&str, SymbolKind, u32, u32)> {
    extraction
        .symbols
        .iter()
        .map(|symbol| {
            (
                symbol.qualified_name.as_str(),
                symbol.kind,
                symbol.start_line,
                symbol.end_line,
            )
        })
        .collect()
}

fn signature<'a>(extraction: &'a Extraction, qualified: &str) -> Option<&'a str> {
    extraction
        .symbols
        .iter()
        .find(|symbol| symbol.qualified_name == qualified)
        .and_then(|symbol| symbol.signature.as_deref())
}

#[test]
fn test_java_and_csharp_files_select_their_parsers() {
    assert_eq!(
        ParserRegistry::for_path(Path::new("src/main/java/a/Store.java")),
        Some(Language::Java)
    );
    assert_eq!(
        ParserRegistry::for_path(Path::new("Acme/Store.cs")),
        Some(Language::CSharp)
    );
    assert_eq!(Language::Java.as_str(), "java");
    assert_eq!(Language::CSharp.as_str(), "csharp");
}

#[test]
fn test_java_declarations_are_qualified_through_their_outer_types() {
    let extraction = ParserRegistry::extract(Language::Java, JAVA).expect("Java fixture extracts");
    assert_eq!(
        symbol_rows(&extraction),
        [
            ("com.example.store", SymbolKind::Module, 4, 4),
            ("JobStore", SymbolKind::Class, 12, 65),
            ("JobStore::MAX_JOBS", SymbolKind::Const, 13, 13),
            ("JobStore::MIN_JOBS", SymbolKind::Const, 13, 13),
            ("JobStore::jobs", SymbolKind::Const, 14, 14),
            ("JobStore::JobStore", SymbolKind::Method, 16, 19),
            ("JobStore::JobStore", SymbolKind::Method, 21, 23),
            ("JobStore::load", SymbolKind::Method, 25, 33),
            ("JobStore::validate", SymbolKind::Method, 35, 39),
            ("JobStore::find", SymbolKind::Method, 41, 46),
            ("JobStore::Audit", SymbolKind::Class, 48, 52),
            ("JobStore::Audit::key", SymbolKind::Const, 49, 49),
            ("JobStore::Audit::Audit", SymbolKind::Method, 50, 50),
            ("JobStore::Audit::record", SymbolKind::Method, 51, 51),
            ("JobStore::State", SymbolKind::Enum, 54, 54),
            ("JobStore::State::NEW", SymbolKind::Const, 54, 54),
            ("JobStore::State::DONE", SymbolKind::Const, 54, 54),
            ("JobStore::State::finished", SymbolKind::Method, 54, 54),
            ("JobStore::Listener", SymbolKind::Interface, 56, 56),
            ("JobStore::Listener::changed", SymbolKind::Method, 56, 56),
            ("JobStore::Entry", SymbolKind::Class, 58, 58),
            ("JobStore::Tracked", SymbolKind::Interface, 60, 60),
            ("JobStore::Tracked::value", SymbolKind::Method, 60, 60),
            ("JobStore::main", SymbolKind::Method, 62, 64),
            ("Helper", SymbolKind::Class, 67, 69),
            ("Helper::twice", SymbolKind::Method, 68, 68),
        ]
    );
    // Type parameters are never part of a name; signatures keep them.
    assert_eq!(
        signature(&extraction, "JobStore"),
        Some("public class JobStore<T extends Job> extends BaseStore implements Store")
    );
    assert_eq!(
        signature(&extraction, "JobStore::load"),
        Some("@Override\n    public <R> R load(String key, Function<T, R> mapper)")
    );
    assert_eq!(
        signature(&extraction, "JobStore::MAX_JOBS"),
        Some("public static final int MAX_JOBS")
    );
    assert_eq!(
        signature(&extraction, "JobStore::jobs"),
        Some("private final List<T> jobs")
    );
    assert_eq!(
        signature(&extraction, "JobStore::Listener::changed"),
        Some("void changed(String key)")
    );
    assert_eq!(
        signature(&extraction, "com.example.store"),
        Some("package com.example.store")
    );
}

#[test]
fn test_java_imports_keep_static_and_wildcard_names() {
    let extraction = ParserRegistry::extract(Language::Java, JAVA).expect("Java fixture extracts");
    let imports = extraction
        .imports
        .iter()
        .map(|import| import.raw.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        imports,
        [
            "java.util.List",
            "java.util.function.*",
            "java.util.Collections.sort",
            "com.example.util.Strings",
        ]
    );
}

#[test]
fn test_java_calls_record_plain_self_path_receiver_and_new_forms() {
    let extraction = ParserRegistry::extract(Language::Java, JAVA).expect("Java fixture extracts");
    assert_eq!(
        call_rows(&extraction),
        [
            (
                "JobStore::JobStore",
                "size",
                Some("jobs"),
                CallForm::Path,
                17
            ),
            ("JobStore::JobStore", "of", Some("List"), CallForm::Path, 22),
            ("JobStore::load", "validate", None, CallForm::Plain, 27),
            (
                "JobStore::load",
                "find",
                Some("this"),
                CallForm::SelfRef,
                28
            ),
            (
                "JobStore::load",
                "trim",
                Some("Strings"),
                CallForm::Path,
                29
            ),
            ("JobStore::load", "sort", None, CallForm::Plain, 30),
            ("JobStore::load", "Audit", None, CallForm::Plain, 31),
            ("JobStore::load", "record", None, CallForm::Receiver, 31),
            (
                "JobStore::load",
                "apply",
                Some("mapper"),
                CallForm::Path,
                32
            ),
            (
                "JobStore::validate",
                "isEmpty",
                Some("key"),
                CallForm::Path,
                36
            ),
            (
                "JobStore::validate",
                "IllegalArgumentException",
                None,
                CallForm::Plain,
                37
            ),
            ("JobStore::find", "Runnable", None, CallForm::Plain, 42),
            // A method of an anonymous class is not a symbol: its calls
            // belong to the enclosing method.
            (
                "JobStore::find",
                "toString",
                Some("super"),
                CallForm::SelfRef,
                43
            ),
            ("JobStore::find", "get", Some("jobs"), CallForm::Path, 45),
            (
                "JobStore::Audit::record",
                "println",
                Some("System.out"),
                CallForm::Path,
                51
            ),
            ("JobStore::main", "JobStore", None, CallForm::Plain, 63),
            ("JobStore::main", "load", None, CallForm::Receiver, 63),
        ]
    );
}

#[test]
fn test_csharp_declarations_are_qualified_through_their_outer_types() {
    let extraction =
        ParserRegistry::extract(Language::CSharp, CSHARP).expect("C# fixture extracts");
    assert_eq!(
        symbol_rows(&extraction),
        [
            ("Acme.Jobs", SymbolKind::Module, 7, 68),
            ("JobStore", SymbolKind::Class, 10, 59),
            ("JobStore::MaxJobs", SymbolKind::Const, 12, 12),
            ("JobStore::MinJobs", SymbolKind::Const, 12, 12),
            ("JobStore::jobs", SymbolKind::Const, 13, 13),
            ("JobStore::Count", SymbolKind::Const, 15, 15),
            ("JobStore::Total", SymbolKind::Const, 17, 17),
            ("JobStore::JobStore", SymbolKind::Method, 19, 22),
            ("JobStore::Load", SymbolKind::Method, 24, 33),
            ("JobStore::Validate", SymbolKind::Method, 35, 38),
            ("JobStore::Find", SymbolKind::Method, 40, 40),
            ("JobStore::Reset", SymbolKind::Method, 42, 42),
            ("JobStore::Audit", SymbolKind::Class, 44, 49),
            ("JobStore::Audit::text", SymbolKind::Const, 46, 46),
            ("JobStore::Audit::Audit", SymbolKind::Method, 47, 47),
            ("JobStore::Audit::Record", SymbolKind::Method, 48, 48),
            ("JobStore::State", SymbolKind::Enum, 51, 51),
            ("JobStore::State::New", SymbolKind::Const, 51, 51),
            ("JobStore::State::Done", SymbolKind::Const, 51, 51),
            ("JobStore::IListener", SymbolKind::Interface, 53, 53),
            ("JobStore::IListener::Changed", SymbolKind::Method, 53, 53),
            ("JobStore::Main", SymbolKind::Method, 55, 58),
            ("Acme.Jobs.Internal", SymbolKind::Module, 61, 67),
            ("Point", SymbolKind::Struct, 63, 63),
            ("Point::X", SymbolKind::Const, 63, 63),
            ("Entry", SymbolKind::Class, 64, 64),
            ("Size", SymbolKind::Struct, 65, 65),
            ("Handler", SymbolKind::Type, 66, 66),
        ]
    );
    assert_eq!(
        signature(&extraction, "JobStore"),
        Some("public partial class JobStore<T> : BaseStore, IStore where T : Job")
    );
    assert_eq!(
        signature(&extraction, "JobStore::Count"),
        Some("public int Count")
    );
    assert_eq!(
        signature(&extraction, "JobStore::MaxJobs"),
        Some("public const int MaxJobs")
    );
    assert_eq!(
        signature(&extraction, "JobStore::Load"),
        Some("public R Load<R>(string key, Func<T, R> mapper)")
    );
    assert_eq!(
        signature(&extraction, "Acme.Jobs.Internal"),
        Some("namespace Acme.Jobs.Internal")
    );
}

#[test]
fn test_csharp_usings_name_what_they_import() {
    let extraction =
        ParserRegistry::extract(Language::CSharp, CSHARP).expect("C# fixture extracts");
    let imports = extraction
        .imports
        .iter()
        .map(|import| import.raw.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        imports,
        [
            "System",
            "System.Collections.Generic",
            "System.Math",
            "System.Text.Json.JsonSerializer",
        ]
    );
}

#[test]
fn test_csharp_calls_record_forms_and_rewrite_using_aliases() {
    let extraction =
        ParserRegistry::extract(Language::CSharp, CSHARP).expect("C# fixture extracts");
    assert_eq!(
        call_rows(&extraction),
        [
            ("JobStore::JobStore", "Reset", None, CallForm::Plain, 21),
            ("JobStore::Load", "Validate", None, CallForm::Plain, 26),
            (
                "JobStore::Load",
                "Find",
                Some("this"),
                CallForm::SelfRef,
                27
            ),
            // `Json` is an alias of `System.Text.Json.JsonSerializer`.
            (
                "JobStore::Load",
                "Serialize",
                Some("JsonSerializer"),
                CallForm::Path,
                28
            ),
            (
                "JobStore::Load",
                "WriteLine",
                Some("Console"),
                CallForm::Path,
                29
            ),
            ("JobStore::Load", "Max", None, CallForm::Plain, 29),
            ("JobStore::Load", "Audit", None, CallForm::Plain, 30),
            ("JobStore::Load", "Record", None, CallForm::Receiver, 30),
            (
                "JobStore::Load",
                "Flush",
                Some("base"),
                CallForm::SelfRef,
                31
            ),
            ("JobStore::Load", "mapper", None, CallForm::Plain, 32),
            // `string.IsNullOrEmpty` and `nameof` are never repository calls.
            (
                "JobStore::Validate",
                "ArgumentException",
                None,
                CallForm::Plain,
                37
            ),
            (
                "JobStore::Audit::Record",
                "WriteLine",
                Some("Console"),
                CallForm::Path,
                48
            ),
            ("JobStore::Main", "JobStore", None, CallForm::Plain, 57),
            ("JobStore::Main", "Load", None, CallForm::Receiver, 57),
        ]
    );
}

#[test]
fn test_csharp_file_scoped_and_nested_namespaces_and_partial_types() {
    let source = "namespace Acme.Core;\n\npublic partial class Store { public void A() {} }\npublic partial class Store { public void B() {} }\n#if DEBUG\npublic class Debug { void Run() { } }\n#endif\nclass @class { void @event() {} }\n";
    let extraction = ParserRegistry::extract(Language::CSharp, source).expect("extracts");
    assert_eq!(
        symbol_rows(&extraction),
        [
            ("Acme.Core", SymbolKind::Module, 1, 1),
            // A partial type gives one symbol per declaration.
            ("Store", SymbolKind::Class, 3, 3),
            ("Store::A", SymbolKind::Method, 3, 3),
            ("Store", SymbolKind::Class, 4, 4),
            ("Store::B", SymbolKind::Method, 4, 4),
            ("Debug", SymbolKind::Class, 6, 6),
            ("Debug::Run", SymbolKind::Method, 6, 6),
            ("class", SymbolKind::Class, 8, 8),
            ("class::event", SymbolKind::Method, 8, 8),
        ]
    );
}

#[test]
fn test_java_and_csharp_overloads_are_one_symbol_each_with_their_signatures() {
    let java = "class A {\n  void run() {}\n  void run(int x) {}\n}\n";
    let extraction = ParserRegistry::extract(Language::Java, java).expect("extracts");
    let runs = extraction
        .symbols
        .iter()
        .filter(|symbol| symbol.qualified_name == "A::run")
        .map(|symbol| symbol.signature.as_deref())
        .collect::<Vec<_>>();
    assert_eq!(runs, [Some("void run()"), Some("void run(int x)")]);
    let csharp = "class A {\n  void Run() {}\n  void Run(int x) {}\n}\n";
    let extraction = ParserRegistry::extract(Language::CSharp, csharp).expect("extracts");
    let runs = extraction
        .symbols
        .iter()
        .filter(|symbol| symbol.qualified_name == "A::Run")
        .map(|symbol| symbol.signature.as_deref())
        .collect::<Vec<_>>();
    assert_eq!(runs, [Some("void Run()"), Some("void Run(int x)")]);
}

#[test]
fn test_java_and_csharp_local_and_anonymous_declarations_are_not_symbols() {
    let java = "class A {\n  void run() {\n    class Local { void inner() {} }\n    Runnable r = () -> helper();\n  }\n  static { init(); }\n}\n";
    let extraction = ParserRegistry::extract(Language::Java, java).expect("extracts");
    let names = extraction
        .symbols
        .iter()
        .map(|symbol| symbol.qualified_name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(names, ["A", "A::run"]);
    // A call in a lambda belongs to the method; one in an initializer block
    // belongs to no method and is not recorded.
    assert_eq!(
        call_rows(&extraction),
        [("A::run", "helper", None, CallForm::Plain, 4)]
    );
    let csharp = "class A {\n  void Run() {\n    int Local() => Helper();\n  }\n  ~A() { }\n  public static A operator +(A a, A b) => a;\n  public int this[int i] => 0;\n  public event System.EventHandler Changed;\n}\n";
    let extraction = ParserRegistry::extract(Language::CSharp, csharp).expect("extracts");
    let names = extraction
        .symbols
        .iter()
        .map(|symbol| symbol.qualified_name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(names, ["A", "A::Run"]);
    assert_eq!(
        call_rows(&extraction),
        [("A::Run", "Helper", None, CallForm::Plain, 3)]
    );
}

#[test]
fn test_java_and_csharp_malformed_sources_still_extract_complete_items() {
    let java = "package a.b;\nimport ;\nclass Good { void ok() { run(); } }\nclass { void broken( }\nclass Later { void fine() {} }\n";
    let extraction = ParserRegistry::extract(Language::Java, java).expect("malformed Java");
    let names = extraction
        .symbols
        .iter()
        .map(|symbol| symbol.qualified_name.as_str())
        .collect::<Vec<_>>();
    assert!(names.contains(&"Good::ok"), "{names:?}");
    assert!(names.contains(&"Later::fine"), "{names:?}");
    assert!(
        extraction
            .symbols
            .iter()
            .all(|symbol| !symbol.name.is_empty() && !symbol.qualified_name.contains("::::"))
    );
    assert!(
        extraction
            .imports
            .iter()
            .all(|import| !import.raw.is_empty())
    );
    let csharp = "namespace A {\nusing ;\nclass Good { void Ok() { Run(); } }\nclass { void Broken( }\nclass Later { void Fine() {} }\n";
    let extraction = ParserRegistry::extract(Language::CSharp, csharp).expect("malformed C#");
    let names = extraction
        .symbols
        .iter()
        .map(|symbol| symbol.qualified_name.as_str())
        .collect::<Vec<_>>();
    assert!(names.contains(&"Good::Ok"), "{names:?}");
    assert!(
        extraction
            .symbols
            .iter()
            .all(|symbol| !symbol.name.is_empty())
    );
    assert!(
        extraction
            .imports
            .iter()
            .all(|import| !import.raw.is_empty())
    );
}

#[test]
fn test_java_and_csharp_signatures_are_bounded() {
    let parameters = (0..400)
        .map(|index| format!("int p{index}"))
        .collect::<Vec<_>>()
        .join(", ");
    let initializer = "1 + ".repeat(2_000);
    let java = format!(
        "class A {{\n  void wide({parameters}) {{}}\n  static final int BIG = {initializer}1;\n}}\n"
    );
    let csharp = format!(
        "class A {{\n  void Wide({parameters}) {{}}\n  const int Big = {initializer}1;\n}}\n"
    );
    for (language, source) in [(Language::Java, java), (Language::CSharp, csharp)] {
        let extraction = ParserRegistry::extract(language, &source).expect("extracts");
        assert_eq!(extraction.symbols.len(), 3, "{language:?}");
        for symbol in &extraction.symbols {
            let signature = symbol.signature.as_deref().expect("signature");
            assert!(signature.len() <= 1_024, "{language:?} {}", signature.len());
        }
        assert!(
            extraction
                .symbols
                .iter()
                .filter(|symbol| symbol.kind != SymbolKind::Class)
                .all(|symbol| symbol
                    .signature
                    .as_deref()
                    .is_some_and(|text| text.ends_with('…')))
        );
    }
}

/// E17-40 bounds: 50,000 members in one type extract in linear time.
#[test]
fn test_java_and_csharp_many_members_in_one_type_extract_in_bounded_time() {
    for language in [Language::Java, Language::CSharp] {
        let mut source = String::from("class Big {\n");
        for index in 0..50_000 {
            source.push_str(&format!(
                "  int f{index}; void m{index}() {{ m{index}(); }}\n"
            ));
        }
        source.push_str("}\n");
        let started = Instant::now();
        let extraction = ParserRegistry::extract(language, &source).expect("extracts");
        let elapsed = started.elapsed();
        eprintln!("{language:?}: 100,001 symbols in {elapsed:?}");
        assert_eq!(extraction.symbols.len(), 100_001);
        assert!(extraction.calls_truncated, "the per-file call bound holds");
        assert_eq!(
            extraction.calls.len(),
            compresso_atlas::parsers::MAX_CALLS_PER_FILE
        );
        assert!(elapsed < BOUND, "{language:?} took {elapsed:?}");
    }
}

/// E17-40 bounds: types nested 5,000 deep neither exhaust the stack nor
/// grow names without bound; deeper types and their members are skipped.
#[test]
fn test_java_and_csharp_deep_type_nesting_is_bounded() {
    for language in [Language::Java, Language::CSharp] {
        let depth = 5_000;
        let mut source = String::new();
        for index in 0..depth {
            source.push_str(&format!("class T{index} {{ void m() {{ run(); }}\n"));
        }
        source.push_str(&"}\n".repeat(depth));
        let started = Instant::now();
        let extraction = ParserRegistry::extract(language, &source).expect("extracts");
        let elapsed = started.elapsed();
        eprintln!("{language:?}: {depth} nested types in {elapsed:?}");
        let types = extraction
            .symbols
            .iter()
            .filter(|symbol| symbol.kind == SymbolKind::Class)
            .count();
        assert_eq!(types, 32, "{language:?}");
        assert!(
            extraction
                .symbols
                .iter()
                .all(|symbol| symbol.qualified_name.len() <= 1_024)
        );
        assert!(elapsed < BOUND, "{language:?} took {elapsed:?}");
    }
}

/// E17-40 bounds: namespaces nested 20,000 deep stop growing their names at
/// the stored-name bound; deeper ones are skipped with their contents.
#[test]
fn test_csharp_deep_namespace_nesting_is_bounded() {
    let depth = 20_000;
    let mut source = String::new();
    for _ in 0..depth {
        source.push_str("namespace Abc { ");
    }
    source.push_str("class Deep { void Run() { } }");
    source.push_str(&" }".repeat(depth));
    let started = Instant::now();
    let extraction = ParserRegistry::extract(Language::CSharp, &source).expect("extracts");
    let elapsed = started.elapsed();
    eprintln!("CSharp: {depth} nested namespaces in {elapsed:?}");
    let modules = extraction
        .symbols
        .iter()
        .filter(|symbol| symbol.kind == SymbolKind::Module)
        .count();
    // "Abc" plus ".Abc" per level stays within 1,024 bytes for 256 levels.
    assert_eq!(modules, 256);
    assert!(
        extraction
            .symbols
            .iter()
            .all(|symbol| symbol.qualified_name.len() <= 1_024)
    );
    assert!(
        !extraction
            .symbols
            .iter()
            .any(|symbol| symbol.qualified_name == "Deep"),
        "the class sits below the bound"
    );
    assert!(elapsed < BOUND, "took {elapsed:?}");
}

/// Review M1: one field declaration of 250,000 names behind a 500 KB
/// annotation or attribute (a 1 MB file) copies its prefix once, not once
/// per name.
#[test]
fn test_java_and_csharp_many_field_names_behind_a_huge_annotation_are_linear() {
    let annotation = "x".repeat(500_000);
    let names = vec!["a"; 250_000].join(",");
    for (language, source) in [
        (
            Language::Java,
            format!("class A {{\n  @X(\"{annotation}\") int {names};\n}}\n"),
        ),
        (
            Language::CSharp,
            format!("class A {{\n  [X(\"{annotation}\")] int {names};\n}}\n"),
        ),
    ] {
        let started = Instant::now();
        let extraction = ParserRegistry::extract(language, &source).expect("extracts");
        let elapsed = started.elapsed();
        eprintln!("{language:?}: 250,000 field names behind 500 KB in {elapsed:?}");
        assert_eq!(extraction.symbols.len(), 250_001, "{language:?}");
        assert!(extraction.symbols.iter().all(|symbol| {
            symbol
                .signature
                .as_deref()
                .is_none_or(|text| text.len() <= 1_024)
        }));
        assert!(elapsed < BOUND, "{language:?} took {elapsed:?}");
    }
}

/// E17-40 bounds: one chained call of 100,000 links and one deeply nested
/// argument list extract iteratively and within the per-file bounds.
#[test]
fn test_java_and_csharp_huge_chained_calls_extract_in_bounded_time() {
    for language in [Language::Java, Language::CSharp] {
        let links = 100_000;
        let chain = "next().".repeat(links);
        let nested = format!("{}x{}", "f(".repeat(20_000), ")".repeat(20_000));
        let source = format!(
            "class A {{\n  void run() {{\n    start().{chain}end();\n    {nested};\n  }}\n}}\n"
        );
        let started = Instant::now();
        let extraction = ParserRegistry::extract(language, &source).expect("extracts");
        let elapsed = started.elapsed();
        eprintln!("{language:?}: chained call of {links} links in {elapsed:?}");
        let names = extraction
            .calls
            .iter()
            .map(|call| call.callee_name.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        assert!(
            names.contains("next") && names.contains("end"),
            "{language:?}"
        );
        assert!(elapsed < BOUND, "{language:?} took {elapsed:?}");
    }
}
