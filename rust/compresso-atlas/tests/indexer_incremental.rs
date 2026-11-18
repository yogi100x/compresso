use std::fs;
use std::path::{Path, PathBuf};

use compresso_atlas::{IndexProgress, Indexer};
use compresso_core::{TokenProvider, counter_for};
use compresso_store::{EdgeKind, Store};
use tempfile::TempDir;

const SOURCE_PATHS: [&str; 8] = [
    "src/app.js",
    "src/app.ts",
    "src/lib.rs",
    "src/main.py",
    "src/main.rs",
    "src/types.ts",
    "src/util.js",
    "src/util.py",
];

#[test]
fn test_full_then_noop_index() {
    let fixture = Fixture::new();
    let indexer = fixture.indexer();
    let mut first_progress = Vec::new();
    let first = indexer
        .index(&fixture.root, |event| first_progress.push(event))
        .expect("full index should succeed");

    assert_eq!(first.files_indexed, 8);
    assert_eq!(first.files_removed, 0);
    assert_eq!(first.files_skipped, 0);
    assert!(first.symbols >= 8);
    assert_eq!(first.imports_resolved, 1);
    assert_eq!(first.imports_unresolved, 0);
    assert!(first.duration_ms > 0);
    assert!(!first.stale);
    assert_eq!(parsing_paths(&first_progress), SOURCE_PATHS);
    let symbol_count = fixture.symbol_count();

    let mut second_progress = Vec::new();
    let second = indexer
        .index(&fixture.root, |event| second_progress.push(event))
        .expect("no-op index should succeed");
    assert_eq!(second.files_indexed, 0);
    assert_eq!(second.files_removed, 0);
    assert!(parsing_paths(&second_progress).is_empty());
    assert_eq!(fixture.symbol_count(), symbol_count);
}

#[test]
fn test_touch_one_file_reparses_only_it() {
    let fixture = Fixture::new();
    let indexer = fixture.indexer();
    indexer
        .index(&fixture.root, |_| {})
        .expect("initial index should succeed");
    fs::write(
        fixture.root.join("src/app.ts"),
        "export function changedApp(): string { return \"changed\"; }\n",
    )
    .expect("TypeScript fixture should change");

    let mut progress = Vec::new();
    let report = indexer
        .index(&fixture.root, |event| progress.push(event))
        .expect("incremental index should succeed");

    assert_eq!(report.files_indexed, 1);
    assert_eq!(parsing_paths(&progress), ["src/app.ts"]);
    let symbols = fixture
        .store
        .symbols_in_file(fixture.repo(), "src/app.ts")
        .expect("changed symbols should load");
    assert_eq!(symbols[0].qualified_name, "changedApp");
    assert!(symbols[0].token_estimate > 0);
}

#[test]
fn test_delete_cascades() {
    let fixture = Fixture::new();
    let indexer = fixture.indexer();
    indexer
        .index(&fixture.root, |_| {})
        .expect("initial index should succeed");
    fs::remove_file(fixture.root.join("src/util.py")).expect("Python fixture should be removed");

    let mut progress = Vec::new();
    let report = indexer
        .index(&fixture.root, |event| progress.push(event))
        .expect("delete index should succeed");

    assert_eq!(report.files_removed, 1);
    assert!(parsing_paths(&progress).is_empty());
    assert_eq!(removed_paths(&progress), ["src/util.py"]);
    assert!(
        fixture
            .store
            .symbols_in_file(fixture.repo(), "src/util.py")
            .expect("removed symbol lookup should succeed")
            .is_empty()
    );
    assert!(
        !fixture
            .store
            .file_hashes(fixture.repo())
            .expect("file hashes should load")
            .contains_key("src/util.py")
    );
}

#[test]
fn test_parser_version_bump_flags_stale_and_reparses_all() {
    let fixture = Fixture::new();
    fixture
        .indexer()
        .index(&fixture.root, |_| {})
        .expect("initial index should succeed");
    fixture
        .store
        .stamp_index_version(fixture.repo(), "0.0.1", 1)
        .expect("old parser version should be stamped");
    let upgraded = Indexer::new(&fixture.store, counter_for(TokenProvider::Heuristic))
        .with_parser_version("0.0.2");
    let mut progress = Vec::new();

    let report = upgraded
        .index(&fixture.root, |event| progress.push(event))
        .expect("stale index should fully reparse");

    assert!(report.stale);
    assert_eq!(report.files_indexed, 8);
    assert_eq!(parsing_paths(&progress), SOURCE_PATHS);
    let version = fixture
        .store
        .latest_index_version(fixture.repo())
        .expect("version lookup should succeed")
        .expect("version should exist");
    assert_eq!(version.parser_version, "0.0.2");
    assert_eq!(version.schema_version, 1);
}

#[test]
fn test_file_and_symbol_token_estimates_use_counter() {
    let fixture = Fixture::new();
    fixture
        .indexer()
        .index(&fixture.root, |_| {})
        .expect("initial index should succeed");
    let source = fs::read_to_string(fixture.root.join("src/app.ts"))
        .expect("fixture source should be readable");
    let expected = counter_for(TokenProvider::Heuristic).count(&source);
    let hits = fixture
        .store
        .search_symbols(fixture.repo(), "appName", 1)
        .expect("indexed symbol should be searchable");
    assert_eq!(hits[0].file.token_estimate, expected);
    assert!(hits[0].symbol.token_estimate > 0);
}

#[test]
fn test_secret_files_never_reach_store_or_fts() {
    let temp = tempfile::tempdir().expect("temporary directory should be created");
    let root = temp.path().join("repo");
    fs::create_dir_all(root.join("src")).expect("source directory should be created");
    fs::write(
        root.join("src/visible.rs"),
        "pub fn visible_symbol() -> bool { true }\n",
    )
    .expect("visible source should be written");
    for relative in [".ENV.secret_canary.rs", "ID_RSA_secret_canary.rs"] {
        fs::write(
            root.join(relative),
            "pub fn secret_canary_must_never_be_indexed() -> bool { true }\n",
        )
        .expect("secret source should be written");
    }
    let store = Store::open(&temp.path().join("compresso.db")).expect("store should open");

    let report = Indexer::new(&store, counter_for(TokenProvider::Heuristic))
        .index(&root, |_| {})
        .expect("repository should index");
    let canonical_root = fs::canonicalize(&root).expect("root should canonicalize");
    let repo = store
        .repo_by_root(&canonical_root)
        .expect("repository lookup should succeed")
        .expect("repository should exist")
        .id;
    let files = store.file_hashes(repo).expect("file hashes should load");

    assert_eq!(report.files_indexed, 1);
    assert_eq!(report.files_skipped, 2);
    assert_eq!(
        files.keys().map(String::as_str).collect::<Vec<_>>(),
        ["src/visible.rs"]
    );
    assert!(
        store
            .search_symbols(repo, "secret_canary_must_never_be_indexed", 20)
            .expect("secret FTS lookup should succeed")
            .is_empty()
    );
}

#[test]
fn test_repository_local_imports_resolve_against_scanned_paths() {
    let temp = tempfile::tempdir().expect("temporary directory should be created");
    let root = temp.path().join("repo");
    copy_dir(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/code/mini-monorepo/repo"),
        &root,
    );
    fs::create_dir_all(root.join("python/pkg")).expect("Python package should be created");
    fs::write(
        root.join("python/pkg/main.py"),
        "from .helper import local_value\n\nprint(local_value())\n",
    )
    .expect("Python importer should be written");
    fs::write(
        root.join("python/pkg/helper.py"),
        "def local_value() -> int:\n    return 7\n",
    )
    .expect("Python target should be written");
    let store = Store::open(&temp.path().join("compresso.db")).expect("store should open");
    Indexer::new(&store, counter_for(TokenProvider::Heuristic))
        .index(&root, |_| {})
        .expect("mini monorepo should index");
    let canonical_root = fs::canonicalize(&root).expect("root should canonicalize");
    let repo = store
        .repo_by_root(&canonical_root)
        .expect("repository lookup should succeed")
        .expect("repository should exist")
        .id;

    let edges = store
        .resolved_edges(repo)
        .expect("resolved edges should load");
    let actual = edges
        .iter()
        .map(|edge| (edge.from_path.as_str(), edge.to_path.as_str(), edge.kind))
        .collect::<Vec<_>>();

    assert!(actual.contains(&("rust/src/main.rs", "rust/src/session.rs", EdgeKind::Imports,)));
    assert!(actual.contains(&("web/src/index.ts", "web/src/api.ts", EdgeKind::Imports,)));
    assert!(actual.contains(&(
        "python/pkg/main.py",
        "python/pkg/helper.py",
        EdgeKind::Imports,
    )));
}

#[test]
fn test_adding_missing_local_target_resolves_without_ordinary_reparse() {
    let temp = tempfile::tempdir().expect("temporary directory should be created");
    let root = temp.path().join("repo");
    fs::create_dir_all(root.join("src")).expect("source directory should be created");
    fs::write(
        root.join("src/importer.ts"),
        "import { target } from \"./target\";\nexport const value = target;\n",
    )
    .expect("importer should be written");
    fs::write(
        root.join("src/unrelated.ts"),
        "export const unrelated = 1;\n",
    )
    .expect("unrelated file should be written");
    let store = Store::open(&temp.path().join("compresso.db")).expect("store should open");
    let indexer = Indexer::new(&store, counter_for(TokenProvider::Heuristic));
    let first = indexer
        .index(&root, |_| {})
        .expect("missing-target index should succeed");
    assert_eq!(first.imports_unresolved, 1);
    fs::write(root.join("src/target.ts"), "export const target = 7;\n")
        .expect("target should be added");

    let mut added_progress = Vec::new();
    indexer
        .index(&root, |event| added_progress.push(event))
        .expect("target addition should resolve import");
    let canonical_root = fs::canonicalize(&root).expect("root should canonicalize");
    let repo = store
        .repo_by_root(&canonical_root)
        .expect("repository lookup should succeed")
        .expect("repository should exist")
        .id;
    assert_eq!(
        parsing_paths(&added_progress),
        ["src/importer.ts", "src/target.ts"]
    );
    assert_eq!(
        store
            .resolved_edges(repo)
            .expect("resolved edges should load")
            .iter()
            .map(|edge| (edge.from_path.as_str(), edge.to_path.as_str()))
            .collect::<Vec<_>>(),
        [("src/importer.ts", "src/target.ts")]
    );

    fs::write(
        root.join("src/unrelated.ts"),
        "export const unrelated = 2;\n",
    )
    .expect("unrelated file should change");
    let mut touch_progress = Vec::new();
    indexer
        .index(&root, |event| touch_progress.push(event))
        .expect("ordinary touch should index");
    assert_eq!(parsing_paths(&touch_progress), ["src/unrelated.ts"]);

    let mut noop_progress = Vec::new();
    indexer
        .index(&root, |event| noop_progress.push(event))
        .expect("no-op should succeed");
    assert!(parsing_paths(&noop_progress).is_empty());
}

#[test]
fn test_removing_targets_reconsiders_only_affected_importers() {
    let temp = tempfile::tempdir().expect("temporary directory should be created");
    let root = temp.path().join("repo");
    fs::create_dir_all(root.join("src")).expect("source directory should be created");
    for (path, source) in [
        (
            "src/ambiguous.ts",
            "import { choice } from \"./choice\";\nexport const selected = choice;\n",
        ),
        (
            "src/resolved.ts",
            "import { gone } from \"./gone\";\nexport const selected = gone;\n",
        ),
        ("src/choice.js", "export const choice = 1;\n"),
        ("src/choice.ts", "export const choice = 2;\n"),
        ("src/gone.ts", "export const gone = 3;\n"),
        ("src/unrelated.ts", "export const unrelated = 4;\n"),
    ] {
        fs::write(root.join(path), source).expect("fixture source should be written");
    }
    let store = Store::open(&temp.path().join("compresso.db")).expect("store should open");
    let indexer = Indexer::new(&store, counter_for(TokenProvider::Heuristic));
    indexer
        .index(&root, |_| {})
        .expect("initial repository should index");
    fs::remove_file(root.join("src/choice.js")).expect("ambiguous target should be removed");
    fs::remove_file(root.join("src/gone.ts")).expect("resolved target should be removed");

    let mut progress = Vec::new();
    indexer
        .index(&root, |event| progress.push(event))
        .expect("target removal should reconsider importers");
    let canonical_root = fs::canonicalize(&root).expect("root should canonicalize");
    let repo = store
        .repo_by_root(&canonical_root)
        .expect("repository lookup should succeed")
        .expect("repository should exist")
        .id;

    assert_eq!(
        parsing_paths(&progress),
        ["src/ambiguous.ts", "src/resolved.ts"]
    );
    assert_eq!(removed_paths(&progress), ["src/choice.js", "src/gone.ts"]);
    assert_eq!(
        store
            .resolved_edges(repo)
            .expect("resolved edges should load")
            .iter()
            .map(|edge| (edge.from_path.as_str(), edge.to_path.as_str()))
            .collect::<Vec<_>>(),
        [("src/ambiguous.ts", "src/choice.ts")]
    );
    assert_eq!(
        store
            .unresolved_edge_sources(repo)
            .expect("unresolved sources should load"),
        ["src/resolved.ts"]
    );
}

struct Fixture {
    _temp: TempDir,
    root: PathBuf,
    store: Store,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().expect("temporary directory should be created");
        let root = temp.path().join("repo");
        copy_dir(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/index-repo"),
            &root,
        );
        let store = Store::open(&temp.path().join("compresso.db")).expect("store should open");
        Self {
            _temp: temp,
            root,
            store,
        }
    }

    fn indexer(&self) -> Indexer<'_> {
        Indexer::new(&self.store, counter_for(TokenProvider::Heuristic))
    }

    fn repo(&self) -> i64 {
        let root = fs::canonicalize(&self.root).expect("fixture root should canonicalize");
        self.store
            .repo_by_root(&root)
            .expect("repository lookup should succeed")
            .expect("repository should be approved")
            .id
    }

    fn symbol_count(&self) -> usize {
        SOURCE_PATHS
            .iter()
            .map(|path| {
                self.store
                    .symbols_in_file(self.repo(), path)
                    .expect("symbols should load")
                    .len()
            })
            .sum()
    }
}

fn parsing_paths(events: &[IndexProgress]) -> Vec<&str> {
    events
        .iter()
        .filter_map(|event| match event {
            IndexProgress::Parsing { path } => path.to_str(),
            IndexProgress::Removed { .. } => None,
        })
        .collect()
}

fn removed_paths(events: &[IndexProgress]) -> Vec<&str> {
    events
        .iter()
        .filter_map(|event| match event {
            IndexProgress::Removed { path } => Some(path.as_str()),
            IndexProgress::Parsing { .. } => None,
        })
        .collect()
}

fn copy_dir(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).expect("fixture destination should be created");
    let mut entries = fs::read_dir(source)
        .expect("fixture directory should be readable")
        .collect::<Result<Vec<_>, _>>()
        .expect("fixture entries should be readable");
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let destination_path = destination.join(entry.file_name());
        if entry
            .file_type()
            .expect("fixture type should load")
            .is_dir()
        {
            copy_dir(&entry.path(), &destination_path);
        } else {
            fs::copy(entry.path(), destination_path).expect("fixture file should be copied");
        }
    }
}

#[test]
fn test_index_survives_python_import_statement_with_no_module_name() {
    let fixture = Fixture::new();
    // A syntactically invalid import (as found in parser test corpora) must
    // not abort indexing of the whole repository.
    fs::write(
        fixture.root.join("src/broken_import.py"),
        "import ,\nimport x, y,\n",
    )
    .expect("fixture file should be written");

    let report = fixture
        .indexer()
        .index(&fixture.root, |_| {})
        .expect("a malformed import must not fail the index");

    assert_eq!(report.files_indexed, 9);
    assert!(
        fixture
            .store
            .unresolved_edge_sources(fixture.repo())
            .expect("unresolved edge sources should load")
            .iter()
            .any(|path| path == "src/broken_import.py"),
        "the named imports on the malformed file's second line are still recorded"
    );
}

#[test]
fn test_index_records_call_sites_and_survives_malformed_calls() {
    let fixture = Fixture::new();
    fs::write(
        fixture.root.join("src/calls.rs"),
        "pub fn outer() {\n    inner();\n    crate::lib::helper();\n}\npub fn inner() {}\n",
    )
    .expect("fixture file should be written");
    for (path, source) in [
        (
            "src/broken_calls.rs",
            "fn a() {\n    b(;\n    ::(1);\n    c.();\n}\n",
        ),
        ("src/broken_calls.py", "def a():\n    b(\n    .c()\n"),
        (
            "src/broken_calls.ts",
            "function a() {\n  b<(;\n  this.();\n  new ();\n}\n",
        ),
    ] {
        fs::write(fixture.root.join(path), source).expect("fixture file should be written");
    }

    let report = fixture
        .indexer()
        .index(&fixture.root, |_| {})
        .expect("malformed calls must not fail the index");

    assert_eq!(report.files_indexed, 12);
    assert!(report.call_sites >= 2);
    let repo = fixture.repo();
    assert_eq!(
        fixture
            .store
            .call_site_count(repo)
            .expect("call sites should count"),
        report.call_sites
    );
    let inner = fixture
        .store
        .call_sites_named(repo, "inner")
        .expect("call sites should load");
    assert!(inner.iter().any(|call| call.from_path == "src/calls.rs"
        && call.caller_qualified_name == "outer"
        && call.line == 2));

    // Re-indexing a changed file replaces its call sites.
    fs::write(fixture.root.join("src/calls.rs"), "pub fn outer() {}\n")
        .expect("fixture file should be rewritten");
    fixture
        .indexer()
        .index(&fixture.root, |_| {})
        .expect("incremental index should succeed");
    assert!(
        fixture
            .store
            .call_sites_named(repo, "inner")
            .expect("call sites should load")
            .iter()
            .all(|call| call.from_path != "src/calls.rs")
    );
}

#[test]
fn test_default_parser_version_marks_call_site_extraction() {
    let fixture = Fixture::new();
    fixture
        .indexer()
        .index(&fixture.root, |_| {})
        .expect("index should succeed");
    let version = fixture
        .store
        .latest_index_version(fixture.repo())
        .expect("version lookup should succeed")
        .expect("version should exist");
    // Indexes stamped before call sites existed hold none, so the suffix
    // change makes the next index pass reparse every file once.
    assert!(version.parser_version.ends_with("+calls-v1"));
}
