use std::fmt::Write as _;
use std::fs;

use compresso_atlas::{
    AtlasError, Indexer, QueryError, SearchDiagnosticCode, SearchOptions, search,
    search_with_diagnostics, validate_search_query,
};
use compresso_core::{TokenProvider, counter_for};
use compresso_store::Store;
use tempfile::TempDir;

#[test]
fn test_search_missing_path_scope_is_informational_not_an_error() {
    let fixture = IndexedFixture::new(&[("src/session.rs", "pub fn start_session() {}\n")]);

    let report = search_with_diagnostics(
        &fixture.store,
        fixture.repo,
        "start_session",
        SearchOptions {
            limit: 10,
            path: Some("src/missing"),
        },
    )
    .expect("a missing path scope should still succeed");

    assert!(report.hits.is_empty());
    assert_eq!(report.diagnostics.len(), 1);
    assert_eq!(
        report.diagnostics[0].code,
        SearchDiagnosticCode::PathNotFound
    );
    assert_eq!(report.diagnostics[0].code.as_str(), "path_not_found");
    assert!(report.diagnostics[0].message.contains("src/missing"));
}

#[test]
fn test_search_path_scope_filters_to_file_or_directory() {
    let fixture = IndexedFixture::new(&[
        ("src/auth/login.rs", "pub fn handle_request_login() {}\n"),
        ("src/auth_extra.rs", "pub fn handle_request_extra() {}\n"),
        ("src/web/router.rs", "pub fn handle_request_route() {}\n"),
    ]);
    let scoped = |path| {
        search_with_diagnostics(
            &fixture.store,
            fixture.repo,
            "handle_request",
            SearchOptions {
                limit: 10,
                path: Some(path),
            },
        )
        .expect("scoped search should succeed")
    };

    let directory = scoped("src/auth/");
    assert_eq!(hit_paths(&directory.hits), ["src/auth/login.rs"]);
    assert!(directory.diagnostics.is_empty());

    let file = scoped("src/web/router.rs");
    assert_eq!(hit_paths(&file.hits), ["src/web/router.rs"]);

    let unscoped = search_with_diagnostics(
        &fixture.store,
        fixture.repo,
        "handle_request",
        SearchOptions {
            limit: 10,
            path: None,
        },
    )
    .expect("unscoped search should succeed");
    assert_eq!(unscoped.hits.len(), 3);
    assert_eq!(
        hit_paths(&unscoped.hits),
        hit_paths(
            &search(&fixture.store, fixture.repo, "handle_request", 10)
                .expect("plain search should succeed")
        )
    );
}

#[test]
fn test_search_path_scope_is_applied_before_the_hit_limit() {
    let mut noise = String::new();
    for index in 0..40 {
        let _ = writeln!(noise, "pub fn shared_token_{index:02}() {{}}");
    }
    let fixture = IndexedFixture::new(&[
        ("a/noise.rs", noise.as_str()),
        (
            "z/target.rs",
            "pub fn shared_token_target_with_long_name() {}\n",
        ),
    ]);

    let report = search_with_diagnostics(
        &fixture.store,
        fixture.repo,
        "shared_token",
        SearchOptions {
            limit: 2,
            path: Some("z"),
        },
    )
    .expect("scoped search should succeed");

    assert_eq!(hit_paths(&report.hits), ["z/target.rs"]);
    assert!(report.diagnostics.is_empty());
}

#[test]
fn test_search_rejects_path_scope_that_escapes_the_repository() {
    let fixture = IndexedFixture::new(&[("src/session.rs", "pub fn start_session() {}\n")]);
    for path in ["../outside", "/etc", "src\\session.rs"] {
        let error = search_with_diagnostics(
            &fixture.store,
            fixture.repo,
            "start_session",
            SearchOptions {
                limit: 10,
                path: Some(path),
            },
        )
        .expect_err("an escaping path scope must be rejected");
        assert!(
            matches!(
                &error,
                AtlasError::Query(QueryError::InvalidSearchPath { path: rejected }) if rejected == path
            ),
            "{error:?}"
        );
    }
}

#[test]
fn test_search_secret_path_scope_reports_not_found_without_hits() {
    let fixture = IndexedFixture::new(&[("src/session.rs", "pub fn start_session() {}\n")]);
    fs::write(fixture.temp.path().join("repo/.env"), "TOKEN=fixture\n")
        .expect("secret fixture should be written");

    let report = search_with_diagnostics(
        &fixture.store,
        fixture.repo,
        "start_session",
        SearchOptions {
            limit: 10,
            path: Some(".env"),
        },
    )
    .expect("a secret path scope should not fail");

    assert!(report.hits.is_empty());
    assert_eq!(
        report.diagnostics[0].code,
        SearchDiagnosticCode::PathNotFound
    );
}

#[test]
fn test_search_query_below_trigram_length_is_diagnosed() {
    let fixture = IndexedFixture::new(&[("src/io.rs", "pub fn io() {}\n")]);

    let report = search_with_diagnostics(
        &fixture.store,
        fixture.repo,
        "io",
        SearchOptions {
            limit: 10,
            path: None,
        },
    )
    .expect("a short query should still succeed");

    assert!(report.hits.is_empty());
    assert_eq!(
        report.diagnostics[0].code,
        SearchDiagnosticCode::QueryTooShort
    );
    assert!(report.diagnostics[0].message.contains('3'));
}

#[test]
fn test_invalid_query_reports_byte_position_and_reason() {
    let cases = [
        ("", 0, "empty"),
        ("   \t", 0, "whitespace"),
        ("start\0session", 5, "NUL"),
        ("né\0", 3, "NUL"),
    ];
    for (query, expected_position, expected_reason) in cases {
        let error = validate_search_query(query).expect_err("query should be rejected");
        let QueryError::InvalidSearchQuery { position, reason } = &error else {
            panic!("unexpected error: {error:?}");
        };
        assert_eq!(*position, expected_position, "{query:?}");
        assert!(reason.contains(expected_reason), "{reason}");
        let message = error.to_string();
        assert!(
            message.contains(&format!("byte {expected_position}")),
            "{message}"
        );
        assert!(message.contains(expected_reason), "{message}");
    }
    validate_search_query("say(\"hello\") [a-z").expect("literal punctuation is searchable");
}

#[test]
fn test_search_with_diagnostics_rejects_invalid_query_before_searching() {
    let fixture = IndexedFixture::new(&[("src/session.rs", "pub fn start_session() {}\n")]);
    let error = search_with_diagnostics(
        &fixture.store,
        fixture.repo,
        "ab\0",
        SearchOptions {
            limit: 10,
            path: Some("src/missing"),
        },
    )
    .expect_err("an invalid query must be rejected");
    assert!(matches!(
        error,
        AtlasError::Query(QueryError::InvalidSearchQuery { position: 2, .. })
    ));
}

fn hit_paths(hits: &[compresso_atlas::SearchHit]) -> Vec<&str> {
    hits.iter().map(|hit| hit.path.as_str()).collect()
}

struct IndexedFixture {
    temp: TempDir,
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
        Indexer::new(&store, counter_for(TokenProvider::Heuristic))
            .index(&root, |_| {})
            .expect("fixture should index");
        let canonical_root = fs::canonicalize(&root).expect("root should canonicalize");
        let repo = store
            .repo_by_root(&canonical_root)
            .expect("repository lookup should succeed")
            .expect("repository should exist")
            .id;
        Self { temp, store, repo }
    }
}
