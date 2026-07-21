use std::fs;
use std::path::{Path, PathBuf};

use compresso_atlas::{IndexProgress, Indexer};
use compresso_core::{TokenProvider, counter_for};
use compresso_store::Store;
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
    assert_eq!(first.imports_resolved, 0);
    assert!(first.imports_unresolved > 0);
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
