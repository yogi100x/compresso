use std::time::{Duration, Instant};

use compresso_atlas::parsers::{
    Extraction, Language, MAX_CALL_NODES_PER_FILE, MAX_CALLS_PER_FILE, ParserRegistry,
};
use compresso_store::CallForm;

/// (caller qualified name, callee, qualifier, form, line)
type Row<'a> = (&'a str, &'a str, Option<&'a str>, CallForm, u32);

fn rows(extraction: &Extraction) -> Vec<Row<'_>> {
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
fn test_rust_calls_record_form_qualifier_caller_and_line() {
    let source = "\
struct Store;
impl Store {
    fn open() -> Self { Self::init(); Store }
    fn init() {}
    fn save(&self) { self.flush(); helper(); }
    fn flush(&self) {}
}
fn helper() {
    let store = Store::open();
    store.save();
    crate::util::log::<u8>(1);
    Vec::<u8>::new();
    println!(\"{}\", run());
    let value = Some(1);
    let closure = || inner();
}
";
    let extraction = ParserRegistry::extract(Language::Rust, source).expect("Rust should extract");
    assert_eq!(
        rows(&extraction),
        [
            ("Store::open", "init", Some("Self"), CallForm::SelfRef, 3),
            ("Store::save", "flush", Some("self"), CallForm::SelfRef, 5),
            ("Store::save", "helper", None, CallForm::Plain, 5),
            ("helper", "open", Some("Store"), CallForm::Path, 9),
            ("helper", "save", None, CallForm::Receiver, 10),
            ("helper", "log", Some("crate::util"), CallForm::Path, 11),
            ("helper", "new", Some("Vec"), CallForm::Path, 12),
            ("helper", "run", None, CallForm::Plain, 13),
            ("helper", "inner", None, CallForm::Plain, 15),
        ]
    );
}

#[test]
fn test_python_calls_record_self_dotted_and_receiver_forms() {
    let source = "\
import os

setup()

class Queue:
    def push(self, item):
        self.validate(item)
        os.path.join('a', 'b')
        items().append(item)
        return Queue()

def run():
    def nested():
        work()
    nested()
";
    let extraction =
        ParserRegistry::extract(Language::Python, source).expect("Python should extract");
    assert_eq!(
        rows(&extraction),
        [
            (
                "Queue::push",
                "validate",
                Some("self"),
                CallForm::SelfRef,
                7
            ),
            ("Queue::push", "item", None, CallForm::Reference, 7),
            ("Queue::push", "join", Some("os.path"), CallForm::Path, 8),
            ("Queue::push", "items", None, CallForm::Plain, 9),
            ("Queue::push", "append", None, CallForm::Receiver, 9),
            ("Queue::push", "Queue", None, CallForm::Plain, 10),
            ("run", "work", None, CallForm::Plain, 14),
            ("run", "nested", None, CallForm::Plain, 15),
        ]
    );
}

#[test]
fn test_script_calls_record_this_member_new_and_skip_require() {
    let source = "\
const fs = require('fs');
export class Service {
  start() {
    this.load();
    const client = new Client();
    api.v1.fetch();
    getItems().map(render);
  }
}
export function boot() { new Service().start(); }
";
    for language in [Language::JavaScript, Language::TypeScript, Language::Tsx] {
        let extraction = ParserRegistry::extract(language, source).expect("script should extract");
        assert_eq!(
            rows(&extraction),
            [
                ("Service::start", "load", Some("this"), CallForm::SelfRef, 4),
                ("Service::start", "Client", None, CallForm::Plain, 5),
                ("Service::start", "fetch", Some("api.v1"), CallForm::Path, 6),
                ("Service::start", "getItems", None, CallForm::Plain, 7),
                ("Service::start", "map", None, CallForm::Receiver, 7),
                ("Service::start", "render", None, CallForm::Reference, 7),
                ("boot", "Service", None, CallForm::Plain, 10),
                ("boot", "start", None, CallForm::Receiver, 10),
            ],
            "{language:?}"
        );
    }
}

#[test]
fn test_repeated_calls_keep_the_first_site_per_caller_and_form() {
    let source = "fn a() {\n    b();\n    b();\n    x.b();\n}\nfn b() {}\n";
    let extraction = ParserRegistry::extract(Language::Rust, source).expect("Rust should extract");
    assert_eq!(
        rows(&extraction),
        [
            ("a", "b", None, CallForm::Plain, 2),
            ("a", "b", None, CallForm::Receiver, 4),
        ]
    );
}

#[test]
fn test_calls_per_file_are_capped() {
    let mut source = String::from("fn caller() {\n");
    for index in 0..(MAX_CALLS_PER_FILE + 50) {
        source.push_str(&format!("    callee_{index}();\n"));
    }
    source.push_str("}\n");
    let extraction = ParserRegistry::extract(Language::Rust, &source).expect("Rust should extract");
    assert_eq!(extraction.calls.len(), MAX_CALLS_PER_FILE);
    assert_eq!(extraction.calls[0].callee_name, "callee_0");
}

#[test]
fn test_malformed_calls_never_yield_empty_or_garbled_names() {
    let cases = [
        (
            Language::Rust,
            "fn a() {\n    b(;\n    ::(1);\n    c.();\n    <T as X>::y();\n}\n",
        ),
        (Language::Python, "def a():\n    b(\n    .c()\n    (\n"),
        (
            Language::JavaScript,
            "function a() {\n  b(;\n  .c();\n  new ();\n}\n",
        ),
        (
            Language::TypeScript,
            "function a(): void {\n  b<(;\n  this.();\n}\n",
        ),
        (
            Language::Tsx,
            "function a() {\n  return <X onClick={() => b(}/>;\n}\n",
        ),
    ];
    for (language, source) in cases {
        let extraction =
            ParserRegistry::extract(language, source).expect("malformed source should extract");
        for call in &extraction.calls {
            assert!(!call.callee_name.is_empty(), "{language:?}: {call:?}");
            assert!(
                call.callee_name.chars().all(|character| character == '_'
                    || character == '$'
                    || character.is_alphanumeric()),
                "{language:?}: {call:?}"
            );
            assert!(call.line >= 1);
            assert!(call.caller < extraction.symbols.len());
            if let Some(qualifier) = &call.qualifier {
                assert!(!qualifier.trim().is_empty(), "{language:?}: {call:?}");
            }
        }
    }
}

#[test]
fn test_shell_and_text_record_no_calls() {
    for (language, source) in [
        (Language::Shell, "build() { make all; }\nbuild\n"),
        (Language::Text, "# Title\nrun() here\n"),
    ] {
        let extraction = ParserRegistry::extract(language, source).expect("should extract");
        assert!(extraction.calls.is_empty());
    }
}

#[test]
fn test_very_deep_member_chain_does_not_overflow_and_is_a_receiver_call() {
    let chain = (0..40_000)
        .map(|index| format!("m{index}"))
        .collect::<Vec<_>>()
        .join(".");
    for (language, source) in [
        (Language::Python, format!("def run():\n    {chain}.go()\n")),
        (
            Language::JavaScript,
            format!("function run() {{\n  {chain}.go();\n}}\n"),
        ),
    ] {
        // Run on a small thread stack: a recursive walk over the chain would
        // overflow it.
        let extraction = std::thread::Builder::new()
            .stack_size(256 * 1024)
            .spawn(move || ParserRegistry::extract(language, &source))
            .expect("thread should start")
            .join()
            .expect("extraction must not overflow the stack")
            .expect("deep chain should extract");
        assert_eq!(
            rows(&extraction),
            [("run", "go", None, CallForm::Receiver, 2)],
            "{language:?}"
        );
    }
}

#[test]
fn test_huge_number_of_calls_and_functions_is_bounded_and_reports_truncation() {
    // 4,000 functions, each with 40 calls of which most repeat: far more call
    // nodes than the examination budget, few unique records per function.
    let mut source = String::new();
    for function in 0..4_000 {
        source.push_str(&format!("fn f{function}() {{\n"));
        for call in 0..40 {
            source.push_str(&format!("    g{}();\n", call % 3));
        }
        source.push_str("}\n");
    }
    let started = Instant::now();
    let extraction = ParserRegistry::extract(Language::Rust, &source).expect("should extract");
    let elapsed = started.elapsed();
    assert!(extraction.calls_truncated);
    assert!(extraction.calls.len() <= MAX_CALLS_PER_FILE);
    const { assert!(4_000 * 40 > MAX_CALL_NODES_PER_FILE) };
    assert!(
        elapsed < Duration::from_secs(20),
        "extraction took {elapsed:?}"
    );

    let small = ParserRegistry::extract(Language::Rust, "fn a() { b(); }\n").expect("extract");
    assert!(!small.calls_truncated);
}

#[test]
fn test_rust_calls_inside_macro_arguments_are_recorded() {
    let source = "\
struct S;
impl S {
    fn check(&self) {
        assert_eq!(self.value(), compute(S::make(), item.len()));
        format!(\"{}\", nested!(inner()));
        let v = vec![Some(1), build()];
    }
}
macro_rules! define {
    () => { fn generated() { hidden(); } };
}
";
    let extraction = ParserRegistry::extract(Language::Rust, source).expect("Rust should extract");
    assert_eq!(
        rows(&extraction),
        [
            ("S::check", "value", Some("self"), CallForm::SelfRef, 4),
            ("S::check", "compute", None, CallForm::Plain, 4),
            ("S::check", "make", Some("S"), CallForm::Path, 4),
            ("S::check", "len", None, CallForm::Receiver, 4),
            ("S::check", "inner", None, CallForm::Plain, 5),
            ("S::check", "build", None, CallForm::Plain, 6),
        ]
    );
}

#[test]
fn test_names_passed_as_call_arguments_are_recorded_as_references() {
    let rust = ParserRegistry::extract(
        Language::Rust,
        "fn run() {\n    result.map_err(to_failure);\n    apply(1, handler, x.y);\n}\n",
    )
    .expect("Rust should extract");
    assert_eq!(
        rows(&rust),
        [
            ("run", "map_err", None, CallForm::Receiver, 2),
            ("run", "to_failure", None, CallForm::Reference, 2),
            ("run", "apply", None, CallForm::Plain, 3),
            ("run", "handler", None, CallForm::Reference, 3),
        ]
    );
    let python = ParserRegistry::extract(
        Language::Python,
        "def run(items):\n    return sorted(items, key=rank)\n",
    )
    .expect("Python should extract");
    assert_eq!(
        rows(&python),
        [
            ("run", "sorted", None, CallForm::Plain, 2),
            ("run", "items", None, CallForm::Reference, 2),
            ("run", "rank", None, CallForm::Reference, 2),
        ]
    );
}
