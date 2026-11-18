use compresso_atlas::parsers::{Extraction, Language, MAX_CALLS_PER_FILE, ParserRegistry};
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
