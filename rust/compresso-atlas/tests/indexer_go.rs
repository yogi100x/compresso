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
    // E17-46 (D111) amends D102's output here: the importer first, then the
    // package's test file that calls `Open` (`references`); `build.go` (a
    // composite literal only) and `doc.go` show no call: `unproven`.
    let report = impact(&fixture.store, fixture.repo(), "store/store.go").expect("impact");
    let dependents = report
        .dependents
        .iter()
        .map(|entry| entry.path.as_str())
        .collect::<Vec<_>>();
    assert_eq!(dependents, ["cmd/server/main.go", "store/store_test.go"]);
    assert_eq!(report.unproven, 2);
    assert!(!report.complete);
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

fn index_files(files: &[(&str, &str)]) -> (TempDir, Store, RepoId) {
    let temp = tempfile::tempdir().expect("temporary directory");
    let root = temp.path().join("repo");
    for (path, source) in files {
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
    (temp, store, repo)
}

fn callees_of(store: &Store, repo: RepoId, from: &str) -> Vec<(String, CallResolution)> {
    let report = trace(store, repo, Some(from), None, TraceOptions::default()).expect("trace");
    match report.result {
        TraceResult::Callees(callees) => callees
            .into_iter()
            .map(|callee| (callee.symbol.qualified_name, callee.resolution))
            .collect(),
        TraceResult::NoPath => Vec::new(),
        other => panic!("expected callees, got {other:?}"),
    }
}

/// A package and a variable look alike in Go syntax. A qualifier that names
/// an external import is a library call; a qualifier that names neither an
/// import nor a package directory is a variable, and its call falls back to
/// a method of that name, labelled `name_only` whichever file it is in.
#[test]
fn test_go_library_calls_stay_external_and_variable_calls_are_name_only() {
    let (_temp, store, repo) = index_files(&[
        ("go.mod", "module example.com/svc\n"),
        (
            "main.go",
            "package main\n\nimport \"fmt\"\n\nfunc main() {\n\tl := &Logger{}\n\tl.Flush()\n\tfmt.Printf(\"x\")\n}\n\ntype Logger struct{}\n\nfunc (l *Logger) Flush() {}\n",
        ),
        (
            "log/logger.go",
            "package log\n\nimport \"fmt\"\n\ntype Logger struct{}\n\nfunc (l *Logger) Printf(format string) {\n\tfmt.Printf(format)\n}\n",
        ),
    ]);
    let found = callees_of(&store, repo, "main");
    assert_eq!(
        found,
        [("Logger::Flush".to_owned(), CallResolution::NameOnly)],
        "fmt.Printf is a library call, not the repository method"
    );
    assert!(
        callees_of(&store, repo, "Logger::Printf").is_empty(),
        "no false self-cycle through fmt.Printf"
    );
}

#[test]
fn test_go_package_qualifier_never_resolves_into_its_own_directory() {
    let (_temp, store, repo) = index_files(&[
        ("go.mod", "module example.com/svc\n"),
        (
            "internal/json/json.go",
            "package json\n\nimport \"encoding/json\"\n\nfunc Marshal(v any) ([]byte, error) {\n\treturn json.Marshal(v)\n}\n",
        ),
    ]);
    assert!(callees_of(&store, repo, "Marshal").is_empty());
}

#[test]
fn test_go_aliased_import_calls_resolve_to_the_package() {
    let (_temp, store, repo) = index_files(&[
        ("go.mod", "module example.com/svc\n"),
        (
            "cmd/main.go",
            "package main\n\nimport st \"example.com/svc/store\"\n\nfunc main() { st.Open() }\n",
        ),
        ("store/store.go", "package store\n\nfunc Open() {}\n"),
    ]);
    assert_eq!(
        callees_of(&store, repo, "main"),
        [("Open".to_owned(), CallResolution::Import)]
    );
}

#[test]
fn test_go_plain_calls_resolve_only_within_their_package() {
    let (_temp, store, repo) = index_files(&[
        ("go.mod", "module example.com/svc\n"),
        ("a/a.go", "package a\n\nfunc run() {\n\tvalidate()\n}\n"),
        ("b/b.go", "package b\n\nfunc validate() {}\n"),
    ]);
    assert!(callees_of(&store, repo, "run").is_empty());
}

#[test]
fn test_go_same_package_calls_skip_test_files_for_non_test_callers() {
    let (_temp, store, repo) = index_files(&[
        ("go.mod", "module example.com/svc\n"),
        (
            "store/store.go",
            "package store\n\nfunc Open() {\n\thelper()\n}\n",
        ),
        ("store/helper.go", "package store\n\nfunc helper() {}\n"),
        (
            "store/helper_test.go",
            "package store_test\n\nfunc helper() {}\n",
        ),
    ]);
    assert_eq!(
        callees_of(&store, repo, "Open"),
        [("helper".to_owned(), CallResolution::SamePackage)]
    );
}

#[test]
fn test_a_changed_go_mod_re_resolves_go_imports() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let root = temp.path().join("repo");
    for (path, source) in [
        ("go.mod", "module example.com/old\n"),
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
    let canonical = fs::canonicalize(&root).expect("root canonicalizes");
    let repo = store
        .repo_by_root(&canonical)
        .expect("lookup")
        .expect("repository")
        .id;
    assert!(store.resolved_edges(repo).expect("edges").is_empty());
    fs::write(root.join("go.mod"), "module example.com/svc\n").expect("go.mod");
    Indexer::new(&store, counter)
        .index(&root, |_| {})
        .expect("second index");
    let edges = store
        .resolved_edges(repo)
        .expect("edges")
        .into_iter()
        .map(|edge| (edge.from_path, edge.to_path))
        .collect::<Vec<_>>();
    assert_eq!(
        edges,
        [("cmd/main.go".to_owned(), "store/store.go".to_owned())]
    );
}

/// E17-35 (D100) with E17-37: a `go.mod` the repository's own
/// `info/exclude` hides is never read, so it declares no module.
#[test]
fn test_an_excluded_go_mod_declares_no_module() {
    let (_temp, store, repo) = index_files(&[
        (".git/info/exclude", "/go.mod\n"),
        ("go.mod", "module example.com/svc\n"),
        (
            "cmd/main.go",
            "package main\n\nimport \"example.com/svc/store\"\n\nfunc main() { store.Open() }\n",
        ),
        ("store/store.go", "package store\n\nfunc Open() {}\n"),
    ]);
    assert!(store.resolved_edges(repo).expect("edges").is_empty());
    assert!(
        store
            .indexed_file(repo, "go.mod")
            .expect("lookup")
            .is_none()
    );
}
