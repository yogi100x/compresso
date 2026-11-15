use std::fs;

use compresso_atlas::{
    AtlasError, QueryError, outline, outline_current, repo_map, repo_map_focused, repository_stale,
};
use compresso_core::counter_for;
use compresso_store::{
    EdgeKind, EdgeRecord, EdgeResolution, FileLanguage, FileRecord, Store, SymbolKind, SymbolRecord,
};
use tempfile::TempDir;

#[test]
fn test_repo_map_respects_budget() {
    let fixture = Fixture::new();
    fixture.seed_ranked_repository();

    let before = fixture.store.total_changes().expect("changes should load");
    let first = repo_map(&fixture.store, fixture.repo, 300).expect("map should render");
    let second = repo_map(&fixture.store, fixture.repo, 300).expect("map should repeat");
    let after = fixture.store.total_changes().expect("changes should load");
    let counter = counter_for(first.token_provider);

    assert_eq!(first, second);
    assert_eq!(first.tokens_used, counter.count(&first.rendered));
    assert!(first.tokens_used <= 300);
    assert_eq!(before, after, "repo_map must not write to SQLite");

    for budget in 0..=full_budget_upper_bound(&first) {
        let bounded = repo_map(&fixture.store, fixture.repo, budget).expect("map should render");
        let same_counter = counter_for(bounded.token_provider);
        assert!(same_counter.count(&bounded.rendered) <= budget);
        assert_eq!(bounded.tokens_used, same_counter.count(&bounded.rendered));
    }
}

fn full_budget_upper_bound(map: &compresso_atlas::RepoMap) -> usize {
    map.tokens_used.saturating_add(20)
}

#[test]
fn test_repo_map_ranks_by_indegree() {
    let fixture = Fixture::new();
    fixture.seed_ranked_repository();
    let full = repo_map(&fixture.store, fixture.repo, usize::MAX).expect("full map should render");

    let one_file = (0..=full.tokens_used)
        .find_map(|budget| {
            let map = repo_map(&fixture.store, fixture.repo, budget).ok()?;
            (map.files_shown == 1).then_some(map)
        })
        .expect("some budget should fit exactly one file");

    assert!(one_file.rendered.contains("core.rs"));
    assert!(!one_file.rendered.contains("leaf.rs"));
    assert!(one_file.truncated);
}

#[test]
fn test_repo_map_focus_biases_prefix_under_tight_budget() {
    let fixture = Fixture::new();
    fixture.seed_ranked_repository();
    let full = repo_map_focused(
        &fixture.store,
        fixture.repo,
        Some("src/leaf.rs"),
        usize::MAX,
    )
    .expect("focused map should render");

    let one_file = (0..=full.tokens_used)
        .find_map(|budget| {
            let map =
                repo_map_focused(&fixture.store, fixture.repo, Some("src/leaf.rs"), budget).ok()?;
            (map.files_shown == 1).then_some(map)
        })
        .expect("some budget should fit exactly one focused file");

    assert!(one_file.rendered.contains("leaf.rs"));
    assert!(!one_file.rendered.contains("core.rs"));
    assert!(one_file.truncated);
}

#[test]
fn test_repo_map_focus_rejects_escape_and_absolute() {
    let fixture = Fixture::new();
    fixture.seed_ranked_repository();

    for focus in ["../outside", "src/../outside", "/absolute", r"C:\absolute"] {
        let error = repo_map_focused(&fixture.store, fixture.repo, Some(focus), usize::MAX)
            .expect_err("unsafe focus should be rejected");
        assert!(
            matches!(error, AtlasError::Query(QueryError::InvalidFocus { focus: ref rejected }) if rejected == focus),
            "unexpected error for {focus:?}: {error:?}"
        );
    }
}

#[test]
fn test_repo_map_renders_directory_tree_and_top_symbols() {
    let fixture = Fixture::new();
    fixture.seed_ranked_repository();

    let map = repo_map(&fixture.store, fixture.repo, usize::MAX).expect("map should render");

    assert!(map.rendered.contains("src/\n"));
    assert!(map.rendered.contains("  nested/\n"));
    assert!(map.rendered.contains("    core.rs"));
    assert!(map.rendered.contains("Core, Core::new, Core::run"));
    assert!(!map.rendered.contains("Core::fourth"));
    assert_eq!(map.files_total, 5);
    assert_eq!(map.files_shown, 5);
    assert!(!map.truncated);
}

#[test]
fn test_outline_line_order_and_ranges() {
    let fixture = Fixture::new();
    let file = fixture.file("src/lib.rs");
    let file_id = fixture
        .store
        .upsert_file(fixture.repo, &file)
        .expect("file should insert");
    fixture
        .store
        .replace_symbols(
            file_id,
            &[
                symbol("helper", "helper", SymbolKind::Function, 17, 19, 8),
                symbol(
                    "SessionManager",
                    "SessionManager",
                    SymbolKind::Struct,
                    3,
                    7,
                    12,
                ),
                symbol(
                    "start",
                    "SessionManager::start",
                    SymbolKind::Method,
                    10,
                    14,
                    15,
                ),
            ],
            &[],
        )
        .expect("symbols should insert");
    let before = fixture.store.total_changes().expect("changes should load");

    let result = outline(&fixture.store, fixture.repo, "src/lib.rs").expect("outline should load");
    let after = fixture.store.total_changes().expect("changes should load");

    assert_eq!(result.path, "src/lib.rs");
    assert_eq!(result.language, Some(FileLanguage::Rust));
    assert_eq!(
        result
            .symbols
            .iter()
            .map(|item| (
                item.qualified_name.as_str(),
                item.start_line,
                item.end_line,
                item.token_estimate,
            ))
            .collect::<Vec<_>>(),
        [
            ("SessionManager", 3, 7, 12),
            ("SessionManager::start", 10, 14, 15),
            ("helper", 17, 19, 8),
        ]
    );
    assert_eq!(before, after, "outline must not write to SQLite");
}

#[test]
fn test_outline_missing_path() {
    let fixture = Fixture::new();

    let error =
        outline(&fixture.store, fixture.repo, "src/nope.rs").expect_err("unknown path should fail");

    assert!(matches!(
        error,
        AtlasError::FileNotIndexed { repo, path }
            if repo == fixture.repo && path == "src/nope.rs"
    ));
}

#[test]
fn test_outline_current_reports_hash_drift() {
    let fixture = Fixture::new();
    let repository = fixture
        .store
        .repository(fixture.repo)
        .expect("repository should load");
    let relative = "src/current.rs";
    let path = repository.root.join(relative);
    fs::create_dir_all(path.parent().expect("file should have a parent"))
        .expect("source directory should be created");
    let original = "pub fn current() {}\n";
    fs::write(&path, original).expect("source should be written");
    fixture
        .store
        .upsert_file(
            fixture.repo,
            &FileRecord {
                path: relative.to_owned(),
                hash: blake3::hash(original.as_bytes()).to_hex().to_string(),
                language: Some(FileLanguage::Rust),
                size_bytes: original.len() as u64,
                token_estimate: 6,
                indexed_at: 10,
            },
        )
        .expect("indexed file should insert");

    let fresh =
        outline_current(&fixture.store, fixture.repo, relative).expect("fresh outline should load");
    assert!(!fresh.stale);
    assert_eq!(fresh.reindex_hint, None);

    fs::write(&path, "pub fn changed() {}\n").expect("source should change");
    let stale = outline_current(&fixture.store, fixture.repo, relative)
        .expect("stale outline should still load");
    assert!(stale.stale);
    assert!(stale.reindex_hint.is_some());
    assert_eq!(stale.outline.path, relative);
}

#[test]
fn test_repository_stale_trusts_an_untouched_file_and_rechecks_a_touched_one() {
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    let fixture = Fixture::new();
    let repository = fixture
        .store
        .repository(fixture.repo)
        .expect("repository should load");
    let relative = "src/tracked.rs";
    let path = repository.root.join(relative);
    fs::create_dir_all(path.parent().expect("file should have a parent"))
        .expect("source directory should be created");
    let original = "pub fn tracked() {}\n";
    let same_size_edit = "pub fn changed() {}\n";
    assert_eq!(original.len(), same_size_edit.len());
    fs::write(&path, original).expect("source should be written");
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be after the epoch")
        .as_secs();
    fixture
        .store
        .upsert_file(
            fixture.repo,
            &FileRecord {
                path: relative.to_owned(),
                hash: blake3::hash(original.as_bytes()).to_hex().to_string(),
                language: Some(FileLanguage::Rust),
                size_bytes: original.len() as u64,
                token_estimate: 6,
                indexed_at: i64::try_from(now).expect("timestamp should fit"),
            },
        )
        .expect("indexed file should insert");
    let set_modified = |seconds_before_index: u64| {
        fs::File::options()
            .write(true)
            .open(&path)
            .expect("source should open")
            .set_modified(UNIX_EPOCH + Duration::from_secs(now - seconds_before_index))
            .expect("modification time should be set");
    };

    // A same-size edit made in the same second as the index is hashed.
    fs::write(&path, same_size_edit).expect("source should change");
    set_modified(0);
    assert!(repository_stale(&fixture.store, fixture.repo).expect("touched file should inspect"));

    // A different size is stale whatever the modification time says.
    fs::write(&path, "pub fn tracked_longer() {}\n").expect("source should grow");
    set_modified(3600);
    assert!(repository_stale(&fixture.store, fixture.repo).expect("resized file should inspect"));

    // The documented limit of the fast path: the same size with a
    // modification time well before the index is trusted without hashing.
    fs::write(&path, same_size_edit).expect("source should change");
    set_modified(3600);
    assert!(
        !repository_stale(&fixture.store, fixture.repo).expect("untouched file should inspect")
    );
}

#[test]
fn test_repository_stale_detects_changed_and_deleted_file() {
    let fixture = Fixture::new();
    let repository = fixture
        .store
        .repository(fixture.repo)
        .expect("repository should load");
    let relative = "src/tracked.rs";
    let path = repository.root.join(relative);
    fs::create_dir_all(path.parent().expect("file should have a parent"))
        .expect("source directory should be created");
    let original = "pub fn tracked() {}\n";
    fs::write(&path, original).expect("source should be written");
    fixture
        .store
        .upsert_file(
            fixture.repo,
            &FileRecord {
                path: relative.to_owned(),
                hash: blake3::hash(original.as_bytes()).to_hex().to_string(),
                language: Some(FileLanguage::Rust),
                size_bytes: original.len() as u64,
                token_estimate: 6,
                indexed_at: 10,
            },
        )
        .expect("indexed file should insert");

    assert!(!repository_stale(&fixture.store, fixture.repo).expect("fresh repo should inspect"));
    fs::write(&path, "pub fn drifted() {}\n").expect("source should change");
    assert!(repository_stale(&fixture.store, fixture.repo).expect("drift should inspect"));
    fs::write(&path, original).expect("source should restore");
    assert!(!repository_stale(&fixture.store, fixture.repo).expect("restored repo should inspect"));
    fs::remove_file(&path).expect("source should delete");
    assert!(repository_stale(&fixture.store, fixture.repo).expect("deletion should inspect"));
}

struct Fixture {
    _temp: TempDir,
    store: Store,
    repo: i64,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().expect("temporary directory should be created");
        let root = temp.path().join("repo");
        fs::create_dir(&root).expect("repository root should be created");
        let store = Store::open(&temp.path().join("compresso.db")).expect("store should open");
        let repo = store.approve_repo(&root).expect("repo should approve");
        Self {
            _temp: temp,
            store,
            repo,
        }
    }

    fn seed_ranked_repository(&self) {
        for path in [
            "src/a.rs",
            "src/b.rs",
            "src/c.rs",
            "src/leaf.rs",
            "src/nested/core.rs",
        ] {
            self.store
                .upsert_file(self.repo, &self.file(path))
                .expect("file should insert");
        }

        let core = self
            .store
            .upsert_file(self.repo, &self.file("src/nested/core.rs"))
            .expect("core should load");
        self.store
            .replace_symbols(
                core,
                &[
                    symbol("Core", "Core", SymbolKind::Struct, 1, 5, 12),
                    symbol("new", "Core::new", SymbolKind::Method, 7, 9, 8),
                    symbol("run", "Core::run", SymbolKind::Method, 11, 15, 11),
                    symbol("fourth", "Core::fourth", SymbolKind::Method, 17, 20, 10),
                ],
                &[],
            )
            .expect("core symbols should insert");

        for path in ["src/a.rs", "src/b.rs", "src/c.rs"] {
            let importer = self
                .store
                .upsert_file(self.repo, &self.file(path))
                .expect("importer should load");
            self.store
                .replace_symbols(
                    importer,
                    &[symbol(
                        path.trim_start_matches("src/").trim_end_matches(".rs"),
                        path,
                        SymbolKind::Function,
                        1,
                        2,
                        4,
                    )],
                    &[EdgeRecord {
                        to_path: "src/nested/core.rs".to_owned(),
                        kind: EdgeKind::Imports,
                        resolution: EdgeResolution::Resolved,
                    }],
                )
                .expect("import edge should insert");
        }

        let leaf = self
            .store
            .upsert_file(self.repo, &self.file("src/leaf.rs"))
            .expect("leaf should load");
        self.store
            .replace_symbols(
                leaf,
                &[symbol("Leaf", "Leaf", SymbolKind::Struct, 1, 3, 6)],
                &[],
            )
            .expect("leaf symbols should insert");
    }

    fn file(&self, path: &str) -> FileRecord {
        FileRecord {
            path: path.to_owned(),
            hash: format!("hash-{path}"),
            language: Some(FileLanguage::Rust),
            size_bytes: 100,
            token_estimate: 25,
            indexed_at: 10,
        }
    }
}

fn symbol(
    name: &str,
    qualified_name: &str,
    kind: SymbolKind,
    start_line: u32,
    end_line: u32,
    token_estimate: usize,
) -> SymbolRecord {
    SymbolRecord {
        name: name.to_owned(),
        qualified_name: qualified_name.to_owned(),
        kind,
        signature: Some(format!("{kind:?} {qualified_name}")),
        start_line,
        end_line,
        token_estimate,
    }
}
