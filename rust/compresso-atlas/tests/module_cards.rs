//! E17-26: deterministic module cards (D91).

use std::fs;
use std::path::{Path, PathBuf};

use compresso_atlas::{
    AtlasError, CardOptions, Indexer, ModuleCards, QueryError, module_cards, repo_map,
};
use compresso_core::{TokenProvider, counter_for};
use compresso_store::{
    EdgeKind, EdgeRecord, EdgeResolution, FileLanguage, FileRecord, RepoId, Store, SymbolKind,
    SymbolRecord,
};
use tempfile::TempDir;

struct Fixture {
    _temp: TempDir,
    root: PathBuf,
    store: Store,
    repo: RepoId,
}

impl Fixture {
    fn new() -> Self {
        let temp = TempDir::new().expect("temporary directory");
        let root = temp.path().join("repo");
        write(
            &root,
            "src/lib.rs",
            "//! Session recovery core.\n//! Restores interrupted sessions.\n\nmod util;\n\npub use crate::util::helper;\n\npub struct Recovery;\n\npub fn recover() -> usize { util::helper() }\n\nfn private_step() {}\n",
        );
        write(
            &root,
            "src/util/mod.rs",
            "//! Shared helpers for recovery.\n\npub fn helper() -> usize { 1 }\n\npub trait Step {}\n",
        );
        write(
            &root,
            "tools/main.py",
            "\"\"\"Command-line entry for maintenance.\"\"\"\n\ndef main():\n    pass\n\ndef _hidden():\n    pass\n",
        );
        write(
            &root,
            "web/index.ts",
            "/**\n * Browser bootstrap.\n */\nexport function start() {}\nfunction local() {}\n",
        );
        let store = Store::open(&temp.path().join("compresso.db")).expect("store");
        Indexer::new(&store, counter_for(TokenProvider::Heuristic))
            .index(&root, |_| {})
            .expect("repository indexes");
        let root = fs::canonicalize(&root).expect("root canonicalizes");
        let repo = store
            .repo_by_root(&root)
            .expect("lookup")
            .expect("repository exists")
            .id;
        Self {
            _temp: temp,
            root,
            store,
            repo,
        }
    }

    fn cards(&self, path: Option<&str>, max_cards: usize, budget_tokens: usize) -> ModuleCards {
        module_cards(
            &self.store,
            self.repo,
            &self.root,
            CardOptions {
                path,
                max_cards,
                budget_tokens,
            },
        )
        .expect("cards build")
    }
}

fn write(root: &Path, path: &str, contents: &str) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().expect("parent")).expect("directories");
    fs::write(path, contents).expect("file written");
}

#[test]
fn test_cards_summarize_each_directory_deterministically() {
    let fixture = Fixture::new();
    let before = fixture.store.total_changes().expect("changes");
    let first = fixture.cards(None, 12, 4_000);
    let second = fixture.cards(None, 12, 4_000);
    assert_eq!(first, second);
    assert_eq!(
        before,
        fixture.store.total_changes().expect("changes"),
        "cards never write to SQLite"
    );
    assert!(!first.truncated);
    assert_eq!(first.cards_total, 4);

    let card = |path: &str| {
        first
            .cards
            .iter()
            .find(|card| card.path == path)
            .unwrap_or_else(|| panic!("card for {path}"))
    };
    let src = card("src");
    assert_eq!(src.purpose.as_deref(), Some("Session recovery core."));
    assert_eq!(src.entry_points, ["lib.rs"]);
    let public = src
        .public_symbols
        .iter()
        .map(|symbol| (symbol.name.as_str(), symbol.kind))
        .collect::<Vec<_>>();
    assert!(public.contains(&("Recovery", SymbolKind::Struct)));
    assert!(public.contains(&("recover", SymbolKind::Function)));
    assert!(!public.iter().any(|(name, _)| *name == "private_step"));
    assert_eq!(src.files, 1);

    let util = card("src/util");
    assert_eq!(
        util.purpose.as_deref(),
        Some("Shared helpers for recovery.")
    );
    assert_eq!(util.entry_points, ["mod.rs"]);
    assert_eq!(
        util.public_symbols.first().map(|symbol| symbol.kind),
        Some(SymbolKind::Trait),
        "types and traits rank before functions"
    );

    let tools = card("tools");
    assert_eq!(
        tools.purpose.as_deref(),
        Some("Command-line entry for maintenance.")
    );
    assert_eq!(tools.entry_points, ["main.py"]);
    assert!(
        !tools
            .public_symbols
            .iter()
            .any(|symbol| symbol.name == "_hidden")
    );

    let web = card("web");
    assert_eq!(web.purpose.as_deref(), Some("Browser bootstrap."));
    assert_eq!(
        web.public_symbols
            .iter()
            .map(|symbol| symbol.name.as_str())
            .collect::<Vec<_>>(),
        ["start"]
    );

    let counter = counter_for(first.token_provider);
    for card in &first.cards {
        assert_eq!(card.tokens, counter.count(&card.rendered));
        assert!(card.rendered.starts_with(&format!("{}/ (", card.path)));
    }
}

#[test]
fn test_card_dependencies_come_from_import_edges() {
    let fixture = Fixture::new();
    // Seed one explicit import between directories so the assertion does not
    // depend on language-specific resolution details.
    let edges = [EdgeRecord {
        to_path: "src/util/mod.rs".to_owned(),
        kind: EdgeKind::Imports,
        resolution: EdgeResolution::Resolved,
    }];
    seed_file(&fixture, "web/app.ts", FileLanguage::TypeScript, &edges);

    let cards = fixture.cards(None, 12, 4_000);
    let util = cards
        .cards
        .iter()
        .find(|card| card.path == "src/util")
        .expect("util card");
    assert!(util.used_by.contains(&"web".to_owned()), "{util:?}");
    assert_eq!(util.used_by_total, util.used_by.len());
    let web = cards
        .cards
        .iter()
        .find(|card| card.path == "web")
        .expect("web card");
    assert!(web.depends_on.contains(&"src/util".to_owned()), "{web:?}");
    assert!(web.rendered.contains("uses: src/util"));
    assert_eq!(
        cards.cards.first().map(|card| card.path.as_str()),
        Some("src/util"),
        "the most-depended-on directory ranks first"
    );
}

#[test]
fn test_cards_exclude_secret_paths_and_their_edges() {
    let fixture = Fixture::new();
    let edges = [EdgeRecord {
        to_path: "src/lib.rs".to_owned(),
        kind: EdgeKind::Imports,
        resolution: EdgeResolution::Resolved,
    }];
    seed_file(&fixture, "keys/.env.local.rs", FileLanguage::Rust, &edges);

    let cards = fixture.cards(None, 50, 100_000);
    for card in &cards.cards {
        assert_ne!(
            card.path, "keys",
            "a directory of only secret files has no card"
        );
        assert!(!card.rendered.contains(".env"), "{}", card.rendered);
        assert!(!card.used_by.contains(&"keys".to_owned()));
    }
    assert_eq!(cards.cards_total, 4);
}

#[test]
fn test_cards_respect_card_cap_and_token_budget() {
    let fixture = Fixture::new();
    let one = fixture.cards(None, 1, 4_000);
    assert_eq!(one.cards.len(), 1);
    assert!(one.truncated);

    let full = fixture.cards(None, 50, 100_000);
    for budget in 0..full.tokens_used {
        let bounded = fixture.cards(None, 50, budget);
        assert!(bounded.tokens_used <= budget, "budget {budget}");
        assert!(bounded.truncated, "budget {budget}");
    }
    assert!(!full.truncated);
}

#[test]
fn test_card_path_zooms_into_a_subtree_and_rejects_unsafe_prefixes() {
    let fixture = Fixture::new();
    let zoomed = fixture.cards(Some("src"), 12, 4_000);
    assert_eq!(
        zoomed
            .cards
            .iter()
            .map(|card| card.path.as_str())
            .collect::<Vec<_>>(),
        ["src/util", "src"],
        "src imports src/util, so src/util ranks first"
    );
    assert_eq!(zoomed.cards_total, 2);
    let leaf = fixture.cards(Some("src/util/"), 12, 4_000);
    assert_eq!(leaf.cards.len(), 1);
    assert!(fixture.cards(Some("nothing"), 12, 4_000).cards.is_empty());

    for unsafe_prefix in ["../outside", "/etc", "src/../.."] {
        let error = module_cards(
            &fixture.store,
            fixture.repo,
            &fixture.root,
            CardOptions {
                path: Some(unsafe_prefix),
                max_cards: 12,
                budget_tokens: 4_000,
            },
        )
        .expect_err("unsafe prefix is rejected");
        assert!(matches!(
            error,
            AtlasError::Query(QueryError::InvalidFocus { .. })
        ));
    }
}

#[test]
fn test_stale_files_never_contribute_a_purpose_line() {
    let fixture = Fixture::new();
    fs::write(
        fixture.root.join("src/lib.rs"),
        "//! Rewritten after indexing; must not be quoted.\npub fn recover() {}\n",
    )
    .expect("file rewritten");
    let cards = fixture.cards(Some("src"), 12, 4_000);
    let src = cards
        .cards
        .iter()
        .find(|card| card.path == "src")
        .expect("src card");
    assert_eq!(src.purpose, None);
    assert!(!src.rendered.contains("Rewritten"));
}

#[test]
fn test_cards_leave_the_existing_repo_map_unchanged() {
    let fixture = Fixture::new();
    let before = repo_map(&fixture.store, fixture.repo, 2_000).expect("map");
    let _ = fixture.cards(None, 12, 4_000);
    assert_eq!(
        repo_map(&fixture.store, fixture.repo, 2_000).expect("map"),
        before
    );
}

fn seed_file(fixture: &Fixture, path: &str, language: FileLanguage, edges: &[EdgeRecord]) {
    let name = Path::new(path)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .expect("stem")
        .trim_start_matches('.')
        .replace('.', "_");
    fixture
        .store
        .replace_indexed_file(
            fixture.repo,
            &FileRecord {
                path: path.to_owned(),
                hash: "0".repeat(64),
                language: Some(language),
                size_bytes: 1,
                token_estimate: 1,
                indexed_at: 0,
            },
            &[SymbolRecord {
                name: name.clone(),
                qualified_name: name,
                kind: SymbolKind::Function,
                signature: Some("pub fn seeded()".to_owned()),
                start_line: 1,
                end_line: 1,
                token_estimate: 1,
            }],
            edges,
        )
        .expect("seeded file stored");
}
