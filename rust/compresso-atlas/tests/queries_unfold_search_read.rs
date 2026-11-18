use std::fs;
use std::path::PathBuf;

use compresso_atlas::{
    AtlasError, Indexer, MAX_READ_FILE_BYTES, QueryError, UnfoldResult, outline, plan_context,
    read_bounded, search, unfold,
};
use compresso_core::{TokenCounter, TokenProvider, counter_for};
use compresso_store::{Store, SymbolKind, SymbolSelector};
use tempfile::TempDir;

#[test]
fn test_unfold_unique_selector_exact_body() {
    let fixture = IndexedFixture::new(&[(
        "src/lib.rs",
        "pub fn unique(value: u32) -> u32 {\n    value + 1\n}\n",
    )]);
    let before = fixture.store.total_changes().expect("changes should load");

    let result = unfold(
        &fixture.store,
        fixture.repo,
        &selector("src/lib.rs", "unique"),
    )
    .expect("unique symbol should unfold");
    let after = fixture.store.total_changes().expect("changes should load");

    match result {
        UnfoldResult::Found {
            path,
            symbol,
            content,
            stale,
            reindex_hint,
        } => {
            assert_eq!(path, "src/lib.rs");
            assert_eq!(symbol.qualified_name, "unique");
            assert_eq!(symbol.start_line, 1);
            assert_eq!(symbol.end_line, 3);
            assert_eq!(
                content,
                "pub fn unique(value: u32) -> u32 {\n    value + 1\n}\n"
            );
            assert!(!stale);
            assert!(reindex_hint.is_none());
        }
        other => panic!("expected found result, got {other:?}"),
    }
    assert_eq!(before, after, "unfold must remain read-only");
}

#[test]
fn test_unfold_overload_returns_candidates() {
    let fixture = IndexedFixture::new(&[(
        "src/lib.rs",
        "pub struct Worker;\nimpl Worker {\n    pub fn run(&self) {}\n    pub fn run(&self, value: u8) -> u8 { value }\n}\n",
    )]);

    let result = unfold(
        &fixture.store,
        fixture.repo,
        &SymbolSelector {
            kind: Some(SymbolKind::Method),
            ..selector("src/lib.rs", "Worker::run")
        },
    )
    .expect("overloads should return ambiguity");

    match result {
        UnfoldResult::Ambiguous {
            candidates,
            stale,
            reindex_hint,
        } => {
            assert_eq!(candidates.len(), 2);
            assert_eq!(
                candidates
                    .iter()
                    .map(|candidate| candidate.start_line)
                    .collect::<Vec<_>>(),
                [3, 4]
            );
            assert!(candidates.iter().all(|candidate| {
                candidate.path == "src/lib.rs"
                    && candidate.qualified_name == "Worker::run"
                    && candidate.kind == SymbolKind::Method
                    && candidate.signature_hash.is_some()
            }));
            assert!(!stale);
            assert!(reindex_hint.is_none());
        }
        other => panic!("expected ambiguous result, got {other:?}"),
    }
}

#[test]
fn test_unfold_trait_impl_disambiguated_by_signature() {
    let fixture = IndexedFixture::new(&[(
        "src/lib.rs",
        "pub struct Worker;\npub trait First { fn run(&self); }\npub trait Second { fn run(&self, value: u8) -> u8; }\nimpl First for Worker { fn run(&self) {} }\nimpl Second for Worker { fn run(&self, value: u8) -> u8 { value } }\n",
    )]);
    let skeleton =
        outline(&fixture.store, fixture.repo, "src/lib.rs").expect("outline should load");
    let desired = skeleton
        .symbols
        .iter()
        .find(|symbol| {
            symbol.qualified_name == "Worker::run"
                && symbol.signature.as_deref() == Some("fn run(&self, value: u8) -> u8")
        })
        .expect("second trait implementation should be indexed");
    let signature_hash = blake3::hash(
        desired
            .signature
            .as_deref()
            .expect("desired symbol should have a signature")
            .as_bytes(),
    )
    .to_hex()
    .to_string();

    let result = unfold(
        &fixture.store,
        fixture.repo,
        &SymbolSelector {
            kind: Some(SymbolKind::Method),
            signature_hash: Some(signature_hash),
            ..selector("src/lib.rs", "Worker::run")
        },
    )
    .expect("signature should disambiguate trait implementation");

    match result {
        UnfoldResult::Found {
            symbol, content, ..
        } => {
            assert_eq!(symbol.start_line, 5);
            assert_eq!(
                content,
                "impl Second for Worker { fn run(&self, value: u8) -> u8 { value } }\n"
            );
        }
        other => panic!("expected found result, got {other:?}"),
    }
}

#[test]
fn test_unfold_stale_relocates_full_selector() {
    let fixture = IndexedFixture::new(&[(
        "src/lib.rs",
        "pub fn target() -> u8 { 1 }\npub fn target(value: u8) -> u8 { value }\n",
    )]);
    let skeleton =
        outline(&fixture.store, fixture.repo, "src/lib.rs").expect("outline should load");
    let desired = skeleton
        .symbols
        .iter()
        .find(|symbol| symbol.signature.as_deref() == Some("pub fn target() -> u8"))
        .expect("zero-argument target should be indexed");
    let signature_hash = blake3::hash(
        desired
            .signature
            .as_deref()
            .expect("target should have a signature")
            .as_bytes(),
    )
    .to_hex()
    .to_string();
    fs::write(
        fixture.root.join("src/lib.rs"),
        "// moved\n// without reindex\n\npub fn target(value: u8) -> u8 { value + 1 }\npub fn target() -> u8 {\n    9\n}\n",
    )
    .expect("indexed file should change");
    let before = fixture.store.total_changes().expect("changes should load");

    let result = unfold(
        &fixture.store,
        fixture.repo,
        &SymbolSelector {
            signature_hash: Some(signature_hash),
            start_line_hint: Some(1),
            ..selector("src/lib.rs", "target")
        },
    )
    .expect("stale selector should relocate");
    let after = fixture.store.total_changes().expect("changes should load");

    match result {
        UnfoldResult::Found {
            symbol,
            content,
            stale,
            reindex_hint,
            ..
        } => {
            assert_eq!(symbol.start_line, 5);
            assert_eq!(symbol.end_line, 7);
            assert_eq!(content, "pub fn target() -> u8 {\n    9\n}\n");
            assert!(stale);
            assert!(
                reindex_hint
                    .as_deref()
                    .is_some_and(|hint| hint.contains("compresso index"))
            );
        }
        other => panic!("expected found result, got {other:?}"),
    }
    assert_eq!(before, after, "stale reparse must remain read-only");
}

#[test]
fn test_unfold_deleted_not_found() {
    let fixture = IndexedFixture::new(&[("src/lib.rs", "pub fn removed() -> u8 { 1 }\n")]);
    fs::write(fixture.root.join("src/lib.rs"), "pub fn replacement() {}\n")
        .expect("symbol should be deleted without reindex");

    let result = unfold(
        &fixture.store,
        fixture.repo,
        &selector("src/lib.rs", "removed"),
    )
    .expect("deleted symbol should be a typed result");

    match result {
        UnfoldResult::NotFound {
            path,
            qualified_name,
            stale,
            reindex_hint,
        } => {
            assert_eq!(path, "src/lib.rs");
            assert_eq!(qualified_name, "removed");
            assert!(stale);
            assert!(reindex_hint.contains("compresso index"));
        }
        other => panic!("expected not-found result, got {other:?}"),
    }
}

#[test]
fn test_unfold_ambiguity_candidates_capped_and_deterministic() {
    let mut source = String::from("pub struct Worker;\nimpl Worker {\n");
    for value in 0..25 {
        source.push_str(&format!(
            "    pub fn run(&self, value_{value}: u8) -> u8 {{ value_{value} }}\n"
        ));
    }
    source.push_str("}\n");
    let fixture = IndexedFixture::new(&[("src/lib.rs", &source)]);
    let selector = SymbolSelector {
        kind: Some(SymbolKind::Method),
        start_line_hint: Some(15),
        ..selector("src/lib.rs", "Worker::run")
    };

    let first = unfold(&fixture.store, fixture.repo, &selector).expect("unfold should succeed");
    let second = unfold(&fixture.store, fixture.repo, &selector).expect("unfold should repeat");

    assert_eq!(first, second);
    match first {
        UnfoldResult::Ambiguous { candidates, .. } => {
            assert_eq!(candidates.len(), 20);
            assert_eq!(candidates[0].start_line, 15);
        }
        other => panic!("expected ambiguous result, got {other:?}"),
    }
}

#[test]
fn test_unfold_outside_root_is_typed() {
    let fixture = IndexedFixture::new(&[("src/lib.rs", "pub fn local() {}\n")]);
    let outside = fixture
        .root
        .parent()
        .expect("root should have a parent")
        .join("outside.rs");
    fs::write(&outside, "pub fn outside() {}\n").expect("outside file should be written");

    let error = unfold(
        &fixture.store,
        fixture.repo,
        &selector("../outside.rs", "outside"),
    )
    .expect_err("outside selector should fail");

    assert!(matches!(error, AtlasError::OutsideRoot { .. }));
}

#[test]
fn test_search_ranked_and_deterministic() {
    let fixture = IndexedFixture::new(&[
        ("src/a.rs", "pub fn SessionManager() {}\n"),
        ("src/b.rs", "pub fn make_SessionManager_for_tests() {}\n"),
    ]);
    let before = fixture.store.total_changes().expect("changes should load");

    let first =
        search(&fixture.store, fixture.repo, "SessionManager", 10).expect("search should succeed");
    let second =
        search(&fixture.store, fixture.repo, "SessionManager", 10).expect("search should repeat");
    let after = fixture.store.total_changes().expect("changes should load");

    assert_eq!(first.len(), 2);
    assert_eq!(first[0].path, "src/a.rs");
    assert_eq!(first[0].symbol.qualified_name, "SessionManager");
    assert!(first.windows(2).all(|pair| pair[0].score <= pair[1].score));
    assert_eq!(
        first
            .iter()
            .map(|hit| (&hit.path, &hit.symbol, hit.score.to_bits()))
            .collect::<Vec<_>>(),
        second
            .iter()
            .map(|hit| (&hit.path, &hit.symbol, hit.score.to_bits()))
            .collect::<Vec<_>>()
    );
    assert_eq!(before, after, "search must remain read-only");
}

#[test]
fn test_read_bounded_rejects_file_over_byte_limit_before_loading() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let root = directory.path().join("repo");
    fs::create_dir(&root).expect("root should be created");
    let root = fs::canonicalize(root).expect("root should canonicalize");
    fs::File::create(root.join("huge.log"))
        .and_then(|file| file.set_len(MAX_READ_FILE_BYTES + 1))
        .expect("sparse file should be created");

    for range in [None, Some((1, 1))] {
        let error = read_bounded(&root, "huge.log", range, 100, counter())
            .expect_err("oversized file should be rejected");
        assert!(
            matches!(
                error,
                AtlasError::Query(QueryError::FileTooLarge { limit_bytes, .. })
                    if limit_bytes == MAX_READ_FILE_BYTES
            ),
            "unexpected error: {error:?}"
        );
    }
}

#[test]
fn test_plan_context_skips_body_of_file_grown_past_read_limit() {
    let fixture = IndexedFixture::new(&[
        ("src/small.rs", "pub fn ledger_flush() -> u32 {\n    1\n}\n"),
        (
            "src/grown.rs",
            "pub fn ledger_rotate() -> u32 {\n    2\n}\n",
        ),
    ]);
    let before = plan_context(&fixture.store, fixture.repo, "ledger flush rotate", 800)
        .expect("context should be served");
    assert!(
        before
            .symbol_bodies
            .iter()
            .any(|body| body.path == "src/grown.rs"),
        "fixture must place the grown file in the evidence pack first"
    );
    fs::OpenOptions::new()
        .write(true)
        .open(fixture.root.join("src/grown.rs"))
        .and_then(|file| file.set_len(MAX_READ_FILE_BYTES + 1))
        .expect("indexed file should grow");

    let plan = plan_context(&fixture.store, fixture.repo, "ledger flush rotate", 800)
        .expect("context should still be served");

    let paths = plan
        .symbol_bodies
        .iter()
        .map(|body| body.path.as_str())
        .collect::<Vec<_>>();
    assert!(paths.contains(&"src/small.rs"), "{paths:?}");
    assert!(!paths.contains(&"src/grown.rs"), "{paths:?}");
}

#[test]
fn test_read_bounded_inclusive_range() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let root = directory.path().join("repo");
    fs::create_dir(&root).expect("root should be created");
    let root = fs::canonicalize(root).expect("root should canonicalize");
    fs::write(root.join("sample.txt"), "one\ntwo\nthree\nfour\n")
        .expect("sample should be written");
    let counter = counter();

    let result =
        read_bounded(&root, "sample.txt", Some((2, 3)), 100, counter).expect("range should read");

    assert_eq!(result.path, "sample.txt");
    assert_eq!(result.content, "two\nthree\n");
    assert_eq!((result.start_line, result.end_line), (2, 3));
    assert_eq!(
        result.file_tokens_total,
        counter.count("one\ntwo\nthree\nfour\n")
    );
    assert_eq!(result.tokens_used, counter.count(&result.content));
    assert_eq!(result.token_provider, TokenProvider::Heuristic);
    assert!(!result.truncated);
    assert!(result.elision.is_none());
}

#[test]
fn test_read_bounded_clamps_end_line_to_eof_and_reports_resolved_range() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let root = directory.path().join("repo");
    fs::create_dir(&root).expect("root should be created");
    let root = fs::canonicalize(root).expect("root should canonicalize");
    fs::write(root.join("sample.txt"), "one\ntwo\nthree\nfour\n")
        .expect("sample should be written");

    let result = read_bounded(&root, "sample.txt", Some((2, 400)), 100, counter())
        .expect("an end line beyond EOF should clamp instead of failing");

    assert_eq!(result.content, "two\nthree\nfour\n");
    assert_eq!((result.start_line, result.end_line), (2, 4));
    assert_eq!(result.selected_end_line, 4);
    assert_eq!(result.file_lines_total, 4);
    assert!(result.range_clamped);
    assert!(!result.truncated);
}

#[test]
fn test_read_bounded_token_truncation() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let root = directory.path().join("repo");
    fs::create_dir(&root).expect("root should be created");
    let root = fs::canonicalize(root).expect("root should canonicalize");
    let source = "0123456789 abcdefghijklmnopqrstuvwxyz\n".repeat(20);
    fs::write(root.join("large.txt"), &source).expect("large sample should be written");
    let counter = counter();

    for budget in [0, 1, 10] {
        let result =
            read_bounded(&root, "large.txt", None, budget, counter).expect("read should truncate");

        assert!(result.truncated);
        assert!(counter.count(&result.content) <= budget);
        assert_eq!(result.tokens_used, counter.count(&result.content));
        assert!(source.starts_with(&result.content));
        assert!(
            result
                .elision
                .as_deref()
                .is_some_and(|value| value.contains("elided"))
        );
    }
}

#[test]
fn test_read_bounded_missing_path_is_typed() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let root = directory.path().join("repo");
    fs::create_dir(&root).expect("root should be created");
    let root = fs::canonicalize(root).expect("root should canonicalize");

    assert!(matches!(
        read_bounded(&root, "missing.txt", None, 100, counter()),
        Err(AtlasError::Query(QueryError::NotFound { .. }))
    ));
}

#[test]
fn test_read_bounded_rejects_invalid_ranges() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let root = directory.path().join("repo");
    fs::create_dir(&root).expect("root should be created");
    let root = fs::canonicalize(root).expect("root should canonicalize");
    fs::write(root.join("sample.txt"), "one\ntwo\n").expect("sample should be written");

    for range in [(0, 1), (2, 1), (3, 3)] {
        assert!(matches!(
            read_bounded(&root, "sample.txt", Some(range), 100, counter()),
            Err(AtlasError::Query(QueryError::InvalidRange { .. }))
        ));
    }
}

#[test]
fn test_read_bounded_rejects_dotdot_and_absolute_escapes() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let root = directory.path().join("repo");
    fs::create_dir(&root).expect("root should be created");
    let root = fs::canonicalize(root).expect("root should canonicalize");
    let outside = directory.path().join("outside.txt");
    fs::write(&outside, "outside\n").expect("outside file should be written");

    for path in [
        "../outside.txt".to_owned(),
        outside.to_string_lossy().into_owned(),
    ] {
        assert!(matches!(
            read_bounded(&root, &path, None, 100, counter()),
            Err(AtlasError::OutsideRoot { .. })
        ));
    }
}

#[cfg(unix)]
#[test]
fn test_read_bounded_refuses_symlink_escape_and_internal_symlink() {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let root = directory.path().join("repo");
    fs::create_dir_all(root.join("real")).expect("root should be created");
    let root = fs::canonicalize(root).expect("root should canonicalize");
    let outside = directory.path().join("outside.txt");
    fs::write(&outside, "outside\n").expect("outside file should be written");
    fs::write(root.join("real/inside.txt"), "inside\n").expect("inside file should be written");
    symlink(&outside, root.join("escape.txt")).expect("escape symlink should be created");
    symlink(root.join("real/inside.txt"), root.join("inside-link.txt"))
        .expect("internal symlink should be created");

    assert!(matches!(
        read_bounded(&root, "escape.txt", None, 100, counter()),
        Err(AtlasError::OutsideRoot { .. })
    ));
    // A link inside the root is refused like a missing file: the scanner
    // never indexes or follows one, so its target was never vetted.
    assert!(matches!(
        read_bounded(&root, "inside-link.txt", None, 100, counter()),
        Err(AtlasError::Query(
            compresso_atlas::QueryError::NotFound { .. }
        ))
    ));
}

#[cfg(unix)]
#[test]
fn test_stored_root_retarget_is_rejected_by_context_unfold_and_read() {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let root_a = directory.path().join("repo-a");
    let root_b = directory.path().join("repo-b");
    fs::create_dir_all(root_a.join("src")).expect("repo A should be created");
    fs::create_dir_all(root_b.join("src")).expect("repo B should be created");
    fs::write(
        root_a.join("src/lib.rs"),
        "pub fn session_target() -> &'static str { \"repository a\" }\n",
    )
    .expect("repo A source should be written");
    fs::write(
        root_b.join("src/lib.rs"),
        "pub fn session_target() -> &'static str { \"repo_b_private_canary_7319\" }\n",
    )
    .expect("repo B source should be written");
    let store = Store::open(&directory.path().join("compresso.db")).expect("store should open");
    let indexer = Indexer::new(&store, counter());
    indexer.index(&root_a, |_| {}).expect("repo A should index");
    indexer.index(&root_b, |_| {}).expect("repo B should index");
    let root_a = fs::canonicalize(root_a).expect("repo A should canonicalize");
    let root_b = fs::canonicalize(root_b).expect("repo B should canonicalize");
    let repo_a = store
        .repo_by_root(&root_a)
        .expect("repo A lookup should succeed")
        .expect("repo A should be approved")
        .id;
    let parked_a = root_a.with_file_name("repo-a-before-retarget");
    fs::rename(&root_a, &parked_a).expect("repo A should move away from its stored root");
    symlink(&root_b, &root_a).expect("stored root should retarget to repo B");

    assert!(matches!(
        plan_context(&store, repo_a, "session_target", 800),
        Err(AtlasError::OutsideRoot { .. })
    ));
    assert!(matches!(
        unfold(&store, repo_a, &selector("src/lib.rs", "session_target")),
        Err(AtlasError::OutsideRoot { .. })
    ));
    assert!(matches!(
        read_bounded(&root_a, "src/lib.rs", None, 800, counter()),
        Err(AtlasError::OutsideRoot { .. })
    ));
}

struct IndexedFixture {
    _temp: TempDir,
    root: PathBuf,
    store: Store,
    repo: i64,
}

impl IndexedFixture {
    fn new(files: &[(&str, &str)]) -> Self {
        let temp = tempfile::tempdir().expect("temporary directory should be created");
        let root = temp.path().join("repo");
        fs::create_dir(&root).expect("repository root should be created");
        for (path, source) in files {
            let destination = root.join(path);
            fs::create_dir_all(
                destination
                    .parent()
                    .expect("fixture path should have a parent"),
            )
            .expect("fixture parent should be created");
            fs::write(destination, source).expect("fixture source should be written");
        }
        let store = Store::open(&temp.path().join("compresso.db")).expect("store should open");
        Indexer::new(&store, counter())
            .index(&root, |_| {})
            .expect("fixture should index");
        let canonical_root = fs::canonicalize(&root).expect("root should canonicalize");
        let repo = store
            .repo_by_root(&canonical_root)
            .expect("repository lookup should succeed")
            .expect("repository should exist")
            .id;
        Self {
            _temp: temp,
            root,
            store,
            repo,
        }
    }
}

fn selector(path: &str, qualified_name: &str) -> SymbolSelector {
    SymbolSelector {
        path: path.to_owned(),
        qualified_name: qualified_name.to_owned(),
        kind: None,
        signature_hash: None,
        start_line_hint: None,
    }
}

fn counter() -> &'static dyn TokenCounter {
    counter_for(TokenProvider::Heuristic)
}
