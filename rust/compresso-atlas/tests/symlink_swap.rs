//! A file the index lists that is later replaced by a symbolic link must
//! never serve the link's target: not a secret file, not another in-root
//! file, and not anything outside the root. Every read path is covered.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::symlink;
use std::path::PathBuf;

use compresso_atlas::{
    AtlasError, CardOptions, GrepOptions, Indexer, QueryError, UnfoldResult, grep, module_cards,
    outline_current, plan_context, read_bounded, repository_stale, unfold,
};
use compresso_core::{TokenCounter, TokenProvider, counter_for};
use compresso_store::{Store, SymbolSelector};
use tempfile::TempDir;

const SECRET: &str = "supersecret_canary_4471";

struct Fixture {
    _temp: TempDir,
    root: PathBuf,
    store: Store,
    repo: i64,
}

impl Fixture {
    /// Indexes `notes.txt` and `src/lib.rs`, then swaps both for symbolic
    /// links to the in-repository `.env`, which the index never held.
    fn swapped() -> Self {
        let temp = tempfile::tempdir().expect("temporary directory");
        let root = temp.path().join("repo");
        fs::create_dir_all(root.join("src")).expect("source directory");
        fs::write(root.join("notes.txt"), "ordinary notes\n").expect("notes");
        fs::write(
            root.join("src/lib.rs"),
            "//! Ordinary library.\npub fn run() { ok() }\n",
        )
        .expect("library");
        fs::write(
            root.join(".env"),
            format!("//! {SECRET} leak.\npub fn run() {{ {SECRET}() }}\n"),
        )
        .expect("secret file");
        let store = Store::open(&temp.path().join("compresso.db")).expect("store");
        Indexer::new(&store, counter())
            .index(&root, |_| {})
            .expect("index");
        let root = fs::canonicalize(&root).expect("canonical root");
        let repo = store
            .repo_by_root(&root)
            .expect("lookup")
            .expect("approved")
            .id;
        for path in ["notes.txt", "src/lib.rs"] {
            fs::remove_file(root.join(path)).expect("remove indexed file");
            symlink(root.join(".env"), root.join(path)).expect("symlink to the secret");
        }
        Self {
            _temp: temp,
            root,
            store,
            repo,
        }
    }
}

fn counter() -> &'static dyn TokenCounter {
    counter_for(TokenProvider::Heuristic)
}

fn assert_not_found<T: std::fmt::Debug>(result: Result<T, AtlasError>) {
    assert!(
        matches!(result, Err(AtlasError::Query(QueryError::NotFound { .. }))),
        "{result:?}"
    );
}

#[test]
fn test_read_refuses_an_indexed_file_swapped_for_a_symlink_to_a_secret() {
    let fixture = Fixture::swapped();
    for path in ["notes.txt", "src/lib.rs"] {
        assert_not_found(read_bounded(&fixture.root, path, None, 1_000, counter()));
        assert_not_found(compresso_atlas::file_line_count(&fixture.root, path));
    }
}

#[test]
fn test_grep_skips_a_swapped_symlink_and_reports_it_skipped() {
    let fixture = Fixture::swapped();
    let patterns = vec![SECRET.to_owned()];
    let report = grep(
        &fixture.store,
        fixture.repo,
        &GrepOptions {
            patterns: &patterns,
            regex: false,
            case_insensitive: false,
            path: None,
            max_files: 20,
            max_matches_per_file: 5,
            context_lines: 0,
            scopes: &[],
        },
    )
    .expect("grep");
    assert_eq!(report.results[0].matches_total, 0);
    assert_eq!(report.files_skipped_unreadable, 2);
    assert!(!report.complete());
}

#[test]
fn test_unfold_outline_context_and_cards_never_serve_the_symlink_target() {
    let fixture = Fixture::swapped();
    let selector = SymbolSelector {
        path: "src/lib.rs".to_owned(),
        qualified_name: "run".to_owned(),
        kind: None,
        signature_hash: None,
        start_line_hint: None,
    };
    match unfold(&fixture.store, fixture.repo, &selector).expect("unfold") {
        UnfoldResult::Found { content, .. } => panic!("served {content:?}"),
        other => assert!(!format!("{other:?}").contains(SECRET)),
    }
    assert_not_found(outline_current(&fixture.store, fixture.repo, "src/lib.rs"));

    let plan = plan_context(&fixture.store, fixture.repo, "run ok", 800).expect("plan");
    assert!(!plan.rendered.contains(SECRET), "{}", plan.rendered);

    let cards = module_cards(
        &fixture.store,
        fixture.repo,
        &fixture.root,
        CardOptions {
            path: None,
            max_cards: 10,
            budget_tokens: 2_000,
        },
    )
    .expect("cards");
    for card in &cards.cards {
        assert!(!card.rendered.contains(SECRET), "{}", card.rendered);
    }
    // One planted link marks the index stale; it never fails the check.
    assert!(repository_stale(&fixture.store, fixture.repo).expect("staleness check"));
}

#[test]
fn test_symlinks_inside_the_root_are_refused_and_escapes_stay_outside_root() {
    let fixture = Fixture::swapped();
    let root = &fixture.root;
    fs::create_dir_all(root.join("real")).expect("real directory");
    fs::write(root.join("real/inside.txt"), "inside\n").expect("inside file");
    symlink(root.join("real/inside.txt"), root.join("inside-link.txt")).expect("file link");
    symlink(root.join("real"), root.join("linked-dir")).expect("directory link");
    let outside = fixture._temp.path().join("outside.txt");
    fs::write(&outside, "outside\n").expect("outside file");
    symlink(&outside, root.join("escape.txt")).expect("escape link");

    assert_eq!(
        read_bounded(root, "real/inside.txt", None, 100, counter())
            .expect("a plain file is served")
            .content,
        "inside\n"
    );
    assert_not_found(read_bounded(root, "inside-link.txt", None, 100, counter()));
    assert_not_found(read_bounded(
        root,
        "linked-dir/inside.txt",
        None,
        100,
        counter(),
    ));
    assert!(matches!(
        read_bounded(root, "escape.txt", None, 100, counter()),
        Err(AtlasError::OutsideRoot { .. })
    ));
    // Reaching the secret directly is refused the same way.
    assert_not_found(read_bounded(root, ".env", None, 100, counter()));
}
