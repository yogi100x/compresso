use std::fs;

use compresso_atlas::{Indexer, plan_context};
use compresso_core::{TokenProvider, counter_for};
use compresso_store::{FileLanguage, Store};

#[test]
fn test_shell_and_generic_text_index_incrementally_with_secret_paths_excluded() {
    let temp = tempfile::tempdir().expect("temporary directory should be created");
    let root = temp.path().join("repo");
    fs::create_dir_all(root.join("scripts")).expect("scripts directory should be created");
    fs::create_dir_all(root.join("docs")).expect("docs directory should be created");
    fs::write(
        root.join("scripts/refresh-ig-token.sh"),
        "#!/usr/bin/env bash\n# Refresh Instagram long-lived access token.\nTOKEN_NAME=INSTAGRAM_ACCESS_TOKEN\nrefresh_token() {\n  printf '%s\\n' \"$TOKEN_NAME\"\n}\n",
    )
    .expect("shell fixture should be written");
    fs::write(
        root.join("docs/authentication.md"),
        "# Authentication\n\nOAuth credentials stay local.\n\n## Sessions\n\nSession cookies expire.\n",
    )
    .expect("Markdown fixture should be written");
    fs::write(
        root.join("notes.txt"),
        "Password login is not implemented.\n\nAPI tokens authenticate outbound services.\n",
    )
    .expect("text fixture should be written");
    fs::write(
        root.join(".env.secret-notes.md"),
        "# credential canary must never be indexed\n",
    )
    .expect("secret Markdown fixture should be written");
    fs::write(
        root.join("config.toml"),
        "secret_canary = \"unsupported config must not be searchable\"\n",
    )
    .expect("unsupported config fixture should be written");

    let store = Store::open(&temp.path().join("compresso.db")).expect("store should open");
    let indexer = Indexer::new(&store, counter_for(TokenProvider::Heuristic));
    let first = indexer
        .index(&root, |_| {})
        .expect("repository should index");
    let canonical = fs::canonicalize(&root).expect("root should canonicalize");
    let repo = store
        .repo_by_root(&canonical)
        .expect("repository lookup should succeed")
        .expect("repository should exist")
        .id;

    assert_eq!(first.files_indexed, 3);
    assert_eq!(
        store
            .indexed_files(repo)
            .expect("files should load")
            .into_iter()
            .filter_map(|file| file.language.map(|language| (file.path, language)))
            .collect::<Vec<_>>(),
        [
            ("docs/authentication.md".to_owned(), FileLanguage::Text),
            ("notes.txt".to_owned(), FileLanguage::Text),
            (
                "scripts/refresh-ig-token.sh".to_owned(),
                FileLanguage::Shell
            ),
        ]
    );
    assert!(
        store
            .search_symbols(repo, "credential canary", 20)
            .expect("secret search should succeed")
            .is_empty()
    );
    assert!(
        store
            .search_symbols(repo, "unsupported config", 20)
            .expect("unsupported search should succeed")
            .is_empty()
    );

    let implementation_plan = plan_context(
        &store,
        repo,
        "Find authentication OAuth credentials sessions tokens and password handling",
        1_600,
    )
    .expect("context should plan");
    assert!(
        implementation_plan
            .rendered
            .contains("scripts/refresh-ig-token.sh")
    );
    assert!(
        implementation_plan
            .rendered
            .contains("Refresh Instagram long-lived access token")
    );
    assert!(
        !implementation_plan
            .rendered
            .contains("docs/authentication.md")
    );
    assert!(implementation_plan.tokens_used <= 1_600);

    let documentation_plan = plan_context(
        &store,
        repo,
        "Find documentation about authentication OAuth credentials sessions and password handling",
        1_600,
    )
    .expect("documentation context should plan");
    assert!(
        documentation_plan
            .rendered
            .contains("docs/authentication.md")
    );
    assert!(documentation_plan.rendered.contains("notes.txt"));

    let second = indexer
        .index(&root, |_| {})
        .expect("unchanged repository should no-op");
    assert_eq!(second.files_indexed, 0);
}

#[test]
fn test_shell_text_parser_fingerprint_forces_one_time_reindex() {
    let temp = tempfile::tempdir().expect("temporary directory should be created");
    let root = temp.path().join("repo");
    fs::create_dir(&root).expect("repository should be created");
    fs::write(root.join("README.md"), "# Authentication\n\nOAuth.\n")
        .expect("text fixture should be written");
    let store = Store::open(&temp.path().join("compresso.db")).expect("store should open");
    let repo = store
        .approve_repo(&root)
        .expect("repository should approve");
    store
        .stamp_index_version(repo, env!("CARGO_PKG_VERSION"), 1)
        .expect("legacy parser version should stamp");

    let report = Indexer::new(&store, counter_for(TokenProvider::Heuristic))
        .index(&root, |_| {})
        .expect("repository should reindex");
    let version = store
        .latest_index_version(repo)
        .expect("version should load")
        .expect("version should exist");

    assert!(report.stale);
    assert_eq!(report.files_indexed, 1);
    assert_ne!(version.parser_version, env!("CARGO_PKG_VERSION"));
    assert!(version.parser_version.contains("shell-text"));
}

#[test]
fn test_context_prefers_shell_evidence_over_duplicate_documentation_mentions() {
    let temp = tempfile::tempdir().expect("temporary directory should be created");
    let root = temp.path().join("repo");
    fs::create_dir_all(root.join("scripts")).expect("scripts directory should be created");
    fs::create_dir_all(root.join("docs")).expect("docs directory should be created");
    fs::write(
        root.join("scripts/refresh-token.sh"),
        "#!/bin/sh\n# Refresh the access_token used by the local worker.\nACCESS_TOKEN_NAME=access_token\n",
    )
    .expect("shell fixture should be written");
    fs::write(
        root.join("docs/provider.md"),
        "# access_token\n\nThe access_token appears repeatedly in user documentation.\n",
    )
    .expect("documentation fixture should be written");
    let store = Store::open(&temp.path().join("compresso.db")).expect("store should open");
    Indexer::new(&store, counter_for(TokenProvider::Heuristic))
        .index(&root, |_| {})
        .expect("repository should index");
    let canonical = fs::canonicalize(&root).expect("root should canonicalize");
    let repo = store
        .repo_by_root(&canonical)
        .expect("repository lookup should succeed")
        .expect("repository should exist")
        .id;

    let plan =
        plan_context(&store, repo, "access_token", 800).expect("context should be assembled");

    assert!(plan.rendered.contains("scripts/refresh-token.sh"));
    assert!(
        !plan.rendered.contains("docs/provider.md"),
        "generic prose must not crowd out implementation evidence for the same query"
    );

    let documentation = plan_context(&store, repo, "documentation access_token", 800)
        .expect("explicit documentation context should be assembled");
    assert!(documentation.rendered.contains("scripts/refresh-token.sh"));
    assert!(documentation.rendered.contains("docs/provider.md"));
}
