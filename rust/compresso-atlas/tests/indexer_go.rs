//! Go across the index (E17-37, D102): package imports become file edges,
//! calls resolve through receivers, packages, and imports, module cards read
//! the package doc, and a store indexed by an older parser re-indexes.

use std::fs;
use std::path::{Path, PathBuf};

use compresso_atlas::{
    CallResolution, CardOptions, Indexer, TraceOptions, TraceResult, impact, module_cards, trace,
};
use compresso_core::{TokenProvider, counter_for};
use compresso_store::{FileLanguage, RepoId, Store, SymbolKind};
use tempfile::TempDir;

const FILES: [(&str, &str); 6] = [
    ("go.mod", "module example.com/svc\n\ngo 1.22\n"),
    (
        "cmd/server/main.go",
        "package main

import (
\t\"fmt\"

\t\"example.com/svc/store\"
)

func main() {
\ts := store.Open(\"jobs\")
\ts.Close()
\tfmt.Println(\"done\")
}
",
    ),
    (
        "store/doc.go",
        "// Package store keeps jobs on disk. It is safe for one writer.
package store
",
    ),
    (
        "store/store.go",
        "package store

type Store struct {
\tname string
}

func Open(name string) *Store {
\treturn newStore(name)
}

func (s *Store) Close() {
\ts.flush()
}

func (s *Store) flush() {}
",
    ),
    (
        "store/build.go",
        "package store

func newStore(name string) *Store {
\treturn &Store{name: name}
}
",
    ),
    (
        "store/store_test.go",
        "package store

import \"testing\"

func TestOpen(t *testing.T) {
\tOpen(\"x\")
}
",
    ),
];

struct Fixture {
    _temp: TempDir,
    root: PathBuf,
    store: Store,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().expect("temporary directory");
        let root = temp.path().join("repo");
        for (path, source) in FILES {
            let absolute = root.join(path);
            fs::create_dir_all(absolute.parent().expect("parent")).expect("directory");
            fs::write(absolute, source).expect("fixture file");
        }
        let store = Store::open(&temp.path().join("compresso.db")).expect("store opens");
        Indexer::new(&store, counter_for(TokenProvider::Heuristic))
            .index(&root, |_| {})
            .expect("fixture indexes");
        Self {
            _temp: temp,
            root,
            store,
        }
    }

    fn repo(&self) -> RepoId {
        let root = fs::canonicalize(&self.root).expect("root canonicalizes");
        self.store
            .repo_by_root(Path::new(&root))
            .expect("lookup")
            .expect("repository")
            .id
    }
}

#[test]
fn test_go_files_are_indexed_with_symbols_and_language() {
    let fixture = Fixture::new();
    let symbols = fixture
        .store
        .symbols_in_file(fixture.repo(), "store/store.go")
        .expect("symbols");
    let names = symbols
        .iter()
        .map(|symbol| (symbol.qualified_name.as_str(), symbol.kind))
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        [
            ("Store", SymbolKind::Struct),
            ("Open", SymbolKind::Function),
            ("Store::Close", SymbolKind::Method),
            ("Store::flush", SymbolKind::Method),
        ]
    );
    let file = fixture
        .store
        .indexed_file(fixture.repo(), "store/store.go")
        .expect("lookup")
        .expect("file");
    assert_eq!(file.language, Some(FileLanguage::Go));
}

#[test]
fn test_go_package_imports_are_edges_to_each_package_file() {
    let fixture = Fixture::new();
    let mut edges = fixture
        .store
        .resolved_edges(fixture.repo())
        .expect("edges")
        .into_iter()
        .map(|edge| (edge.from_path, edge.to_path))
        .collect::<Vec<_>>();
    edges.sort();
    assert_eq!(
        edges,
        [
            ("cmd/server/main.go".to_owned(), "store/build.go".to_owned()),
            ("cmd/server/main.go".to_owned(), "store/doc.go".to_owned()),
            ("cmd/server/main.go".to_owned(), "store/store.go".to_owned()),
        ],
        "test files are not part of the imported package; fmt and testing are external"
    );
    let report = impact(&fixture.store, fixture.repo(), "store/store.go").expect("impact");
    let dependents = report
        .dependents
        .iter()
        .map(|entry| entry.path.as_str())
        .collect::<Vec<_>>();
    assert_eq!(dependents, ["cmd/server/main.go"]);
}

#[test]
fn test_go_calls_resolve_through_imports_packages_and_receivers() {
    let fixture = Fixture::new();
    let report = trace(
        &fixture.store,
        fixture.repo(),
        Some("main"),
        None,
        TraceOptions {
            max_depth: Some(3),
            ..TraceOptions::default()
        },
    )
    .expect("trace");
    let TraceResult::Callees(callees) = &report.result else {
        panic!("expected callees, got {:?}", report.result);
    };
    let found = callees
        .iter()
        .map(|callee| {
            (
                callee.via.as_str(),
                callee.symbol.qualified_name.as_str(),
                callee.resolution,
            )
        })
        .collect::<Vec<_>>();
    assert!(
        found.contains(&("main", "Open", CallResolution::Import)),
        "{found:?}"
    );
    assert!(
        found.contains(&("Open", "newStore", CallResolution::SamePackage)),
        "{found:?}"
    );
    assert!(
        found.contains(&("Store::Close", "Store::flush", CallResolution::SameFile)),
        "{found:?}"
    );
}

#[test]
fn test_go_module_card_reads_the_package_doc_and_exported_names() {
    let fixture = Fixture::new();
    let root = fs::canonicalize(&fixture.root).expect("root canonicalizes");
    let cards = module_cards(
        &fixture.store,
        fixture.repo(),
        &root,
        CardOptions {
            path: Some("store"),
            max_cards: 4,
            budget_tokens: 4_000,
        },
    )
    .expect("cards");
    let card = cards
        .cards
        .iter()
        .find(|card| card.path == "store")
        .expect("store card");
    assert_eq!(
        card.purpose.as_deref(),
        Some("Package store keeps jobs on disk.")
    );
    let public = card
        .public_symbols
        .iter()
        .map(|symbol| symbol.name.as_str())
        .collect::<Vec<_>>();
    assert!(
        public.contains(&"Open") && public.contains(&"Store"),
        "{public:?}"
    );
    assert!(!public.contains(&"newStore"), "{public:?}");
    assert!(card.entry_points.iter().any(|entry| entry == "doc.go"));
}

#[test]
fn test_a_store_indexed_before_go_support_reindexes_go_files() {
    const OLD_VERSION: &str = "0.1.0+atlas-shell-text-v1+rust-imports-v2+calls-v1";
    let temp = tempfile::tempdir().expect("temporary directory");
    let root = temp.path().join("repo");
    fs::create_dir_all(&root).expect("root");
    fs::write(root.join("main.go"), "package main\n\nfunc main() {}\n").expect("file");
    let store = Store::open(&temp.path().join("compresso.db")).expect("store opens");
    let counter = counter_for(TokenProvider::Heuristic);
    Indexer::new(&store, counter)
        .with_parser_version(OLD_VERSION)
        .index(&root, |_| {})
        .expect("old index");
    let canonical = fs::canonicalize(&root).expect("root canonicalizes");
    let repo = store
        .repo_by_root(&canonical)
        .expect("lookup")
        .expect("repository")
        .id;
    // What a parser without Go left behind: the file, no language, no symbols.
    let mut record = store
        .indexed_file(repo, "main.go")
        .expect("lookup")
        .expect("file");
    record.language = None;
    store
        .replace_indexed_file(repo, &record, &[], &[])
        .expect("old rows");

    // The same parser version sees nothing to do for an unchanged file.
    Indexer::new(&store, counter)
        .with_parser_version(OLD_VERSION)
        .index(&root, |_| {})
        .expect("same-version index");
    assert!(
        store
            .symbols_in_file(repo, "main.go")
            .expect("symbols")
            .is_empty()
    );

    // The new parser version re-parses every file.
    Indexer::new(&store, counter)
        .index(&root, |_| {})
        .expect("new index");
    let version = store
        .latest_index_version(repo)
        .expect("version")
        .expect("stamped");
    assert!(
        version.parser_version.contains("+go-v1+calls-v1"),
        "{version:?}"
    );
    let symbols = store.symbols_in_file(repo, "main.go").expect("symbols");
    assert_eq!(symbols.len(), 1);
    assert_eq!(symbols[0].qualified_name, "main");
    let file = store
        .indexed_file(repo, "main.go")
        .expect("lookup")
        .expect("file");
    assert_eq!(file.language, Some(FileLanguage::Go));
}

#[test]
fn test_a_file_added_to_an_imported_package_gains_its_importers() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let root = temp.path().join("repo");
    for (path, source) in [
        ("go.mod", "module example.com/svc\n"),
        (
            "cmd/main.go",
            "package main\n\nimport \"example.com/svc/store\"\n\nfunc main() { store.Open() }\n",
        ),
        ("store/store.go", "package store\n\nfunc Open() {}\n"),
    ] {
        let absolute = root.join(path);
        fs::create_dir_all(absolute.parent().expect("parent")).expect("directory");
        fs::write(absolute, source).expect("file");
    }
    let store = Store::open(&temp.path().join("compresso.db")).expect("store opens");
    let counter = counter_for(TokenProvider::Heuristic);
    Indexer::new(&store, counter)
        .index(&root, |_| {})
        .expect("first index");
    fs::write(
        root.join("store/extra.go"),
        "package store\n\nfunc Extra() {}\n",
    )
    .expect("file");
    Indexer::new(&store, counter)
        .index(&root, |_| {})
        .expect("second index");
    let canonical = fs::canonicalize(&root).expect("root canonicalizes");
    let repo = store
        .repo_by_root(&canonical)
        .expect("lookup")
        .expect("repository")
        .id;
    let report = impact(&store, repo, "store/extra.go").expect("impact");
    let dependents = report
        .dependents
        .iter()
        .map(|entry| entry.path.as_str())
        .collect::<Vec<_>>();
    assert_eq!(dependents, ["cmd/main.go"]);
}

/// Known limit (D102): a package and a variable look alike in Go syntax, so
/// a path call whose qualifier names no repository package falls back to a
/// method of that name anywhere in the repository (`name_only`), as for
/// Python and script variables. A standard-library call can therefore
/// resolve to a lone repository method of the same name.
#[test]
fn test_go_path_calls_without_a_package_fall_back_to_methods_by_name() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let root = temp.path().join("repo");
    for (path, source) in [
        ("go.mod", "module example.com/svc\n"),
        (
            "main.go",
            "package main\n\nimport \"fmt\"\n\nfunc main() {\n\tl := &Logger{}\n\tl.Flush()\n\tfmt.Printf(\"x\")\n}\n",
        ),
        (
            "log/logger.go",
            "package log\n\ntype Logger struct{}\n\nfunc (l *Logger) Flush() {}\n\nfunc (l *Logger) Printf(format string) {}\n",
        ),
    ] {
        let absolute = root.join(path);
        fs::create_dir_all(absolute.parent().expect("parent")).expect("directory");
        fs::write(absolute, source).expect("file");
    }
    let store = Store::open(&temp.path().join("compresso.db")).expect("store opens");
    Indexer::new(&store, counter_for(TokenProvider::Heuristic))
        .index(&root, |_| {})
        .expect("index");
    let canonical = fs::canonicalize(&root).expect("root canonicalizes");
    let repo = store
        .repo_by_root(&canonical)
        .expect("lookup")
        .expect("repository")
        .id;
    let report = trace(&store, repo, Some("main"), None, TraceOptions::default()).expect("trace");
    let TraceResult::Callees(callees) = &report.result else {
        panic!("expected callees, got {:?}", report.result);
    };
    let found = callees
        .iter()
        .map(|callee| (callee.symbol.qualified_name.as_str(), callee.resolution))
        .collect::<Vec<_>>();
    assert!(
        found.contains(&("Logger::Flush", CallResolution::NameOnly)),
        "{found:?}"
    );
    assert!(
        found.contains(&("Logger::Printf", CallResolution::NameOnly)),
        "fmt.Printf falls back to the repository method: {found:?}"
    );
}
