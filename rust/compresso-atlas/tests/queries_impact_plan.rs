use std::fs;
use std::path::PathBuf;

use compresso_atlas::{
    AtlasError, ContextPlan, ImpactEntry, ImpactOptions, ImpactReport, QueryError, impact,
    impact_with_options, plan_context, search,
};
use compresso_core::{TokenProvider, counter_for};
use compresso_store::{
    EdgeKind, EdgeRecord, EdgeResolution, FileLanguage, FileRecord, Store, SymbolKind, SymbolRecord,
};
use tempfile::TempDir;

#[test]
fn test_impact_depth_two() {
    let fixture = Fixture::new();
    fixture.seed_impact_chain();
    let before = fixture.store.total_changes().expect("changes should load");

    let first = impact(&fixture.store, fixture.repo, "c.rs").expect("impact should load");
    let second = impact(&fixture.store, fixture.repo, "c.rs").expect("impact should repeat");
    let after = fixture.store.total_changes().expect("changes should load");

    assert_eq!(first, second);
    assert_eq!(
        first,
        ImpactReport {
            path: "c.rs".to_owned(),
            dependents: vec![
                ImpactEntry {
                    path: "b.rs".to_owned(),
                    depth: 1,
                    edge_kinds: vec![EdgeKind::Imports],
                },
                ImpactEntry {
                    path: "a.rs".to_owned(),
                    depth: 2,
                    edge_kinds: vec![EdgeKind::Imports],
                },
            ],
            unresolved_edges: 0,
            ambiguous_edges: 0,
            complete: true,
            truncated: false,
        }
    );
    assert!(!first.dependents.iter().any(|entry| entry.path == "d.rs"));
    assert!(!first.dependents.iter().any(|entry| entry.path == "c.rs"));
    assert!(
        !first
            .dependents
            .iter()
            .any(|entry| entry.path == "tests/a_test.rs"),
        "a depth-three importer must not be reported"
    );
    assert_eq!(before, after, "impact must not write to SQLite");
}

#[test]
fn test_impact_options_bound_depth_results_and_report_completeness() {
    let fixture = Fixture::new();
    fixture.seed_impact_chain();
    let incomplete_source = fixture
        .store
        .upsert_file(fixture.repo, &file("d.rs", "hash", 1))
        .expect("incomplete source should load");
    fixture
        .store
        .replace_symbols(
            incomplete_source,
            &[],
            &[
                EdgeRecord {
                    to_path: "missing".to_owned(),
                    kind: EdgeKind::Imports,
                    resolution: EdgeResolution::Unresolved("not indexed".to_owned()),
                },
                EdgeRecord {
                    to_path: "crate::choice".to_owned(),
                    kind: EdgeKind::References,
                    resolution: EdgeResolution::Ambiguous(vec![
                        "a.rs".to_owned(),
                        "b.rs".to_owned(),
                    ]),
                },
            ],
        )
        .expect("incomplete edges should replace");

    let depth_one = impact_with_options(
        &fixture.store,
        fixture.repo,
        "c.rs",
        ImpactOptions {
            max_depth: 1,
            max_results: 500,
        },
    )
    .expect("depth-one impact should load");
    assert_eq!(
        depth_one
            .dependents
            .iter()
            .map(|entry| (entry.path.as_str(), entry.depth))
            .collect::<Vec<_>>(),
        [("b.rs", 1)]
    );
    assert_eq!(depth_one.unresolved_edges, 1);
    assert_eq!(depth_one.ambiguous_edges, 1);
    assert!(!depth_one.complete);
    assert!(!depth_one.truncated);

    let limited = impact_with_options(
        &fixture.store,
        fixture.repo,
        "c.rs",
        ImpactOptions {
            max_depth: 2,
            max_results: 1,
        },
    )
    .expect("limited impact should load");
    assert_eq!(limited.dependents.len(), 1);
    assert_eq!(limited.dependents[0].path, "b.rs");
    assert!(limited.truncated);
}

#[test]
fn test_impact_options_reject_invalid_bounds() {
    let fixture = Fixture::new();
    fixture.seed_impact_chain();

    for options in [
        ImpactOptions {
            max_depth: 0,
            max_results: 100,
        },
        ImpactOptions {
            max_depth: 2,
            max_results: 501,
        },
    ] {
        assert!(matches!(
            impact_with_options(&fixture.store, fixture.repo, "c.rs", options),
            Err(AtlasError::Query(QueryError::InvalidImpactOptions { .. }))
        ));
    }
}

#[test]
fn test_impact_edge_kinds_reported() {
    let fixture = Fixture::new();
    fixture.seed_impact_chain();

    let report = impact(&fixture.store, fixture.repo, "a.rs").expect("impact should load");

    assert_eq!(
        report.dependents,
        [ImpactEntry {
            path: "tests/a_test.rs".to_owned(),
            depth: 1,
            edge_kinds: vec![EdgeKind::Tests],
        }]
    );
}

#[test]
fn test_plan_context_within_budget_and_section_order() {
    let fixture = Fixture::new();
    fixture.seed_context_repository();
    let before = fixture.store.total_changes().expect("changes should load");

    let plan = plan_context(&fixture.store, fixture.repo, "session retry logic", 800)
        .expect("context should plan");
    let after = fixture.store.total_changes().expect("changes should load");
    let counter = counter_for(plan.token_provider);

    assert!(plan.tokens_used <= 800);
    assert_eq!(plan.tokens_used, counter.count(&plan.rendered));
    assert_eq!(plan.token_provider, TokenProvider::OpenAiO200k);
    assert!(
        plan.outlines.iter().any(|outline| {
            outline.path == "src/session.rs"
                && outline
                    .symbols
                    .iter()
                    .any(|symbol| symbol.qualified_name == "SessionManager")
        }),
        "the task-matched file outline should include SessionManager"
    );
    assert!(
        plan.symbol_bodies
            .iter()
            .any(|body| body.symbol.qualified_name == "SessionManager")
    );

    let map = plan
        .rendered
        .find("# Relevant repository map")
        .expect("map section should render");
    let evidence = plan
        .rendered
        .find("# Matching evidence excerpts")
        .expect("evidence section should render");
    assert!(map < evidence);
    assert_eq!(before, after, "plan_context must not write to SQLite");
}

#[test]
fn test_plan_context_expands_natural_language_task_and_prioritizes_relevant_symbols() {
    let fixture = Fixture::new();
    fixture.seed_authentication_repository();
    let task = "Locate and summarize the repository's authentication implementations, including \
                OAuth, credentials, sessions, passwords, tokens, and browser cookies.";

    let plan = plan_context(&fixture.store, fixture.repo, task, 800).expect("context should plan");
    let outlined_symbols = plan
        .outlines
        .iter()
        .flat_map(|outline| outline.symbols.iter())
        .map(|symbol| symbol.qualified_name.as_str())
        .collect::<Vec<_>>();

    assert!(plan.tokens_used <= 800);
    for expected in [
        "YoutubeOAuth::get_oauth_token",
        "CredentialStore::load_credentials",
        "InstagramClient::login",
        "BrowserCookieAuth::read_browser_cookies",
    ] {
        assert!(
            outlined_symbols.contains(&expected),
            "natural-language planning omitted {expected}: {outlined_symbols:?}"
        );
    }
    assert!(
        !outlined_symbols.contains(&"ModelProvider::complete"),
        "irrelevant symbols must not displace authentication matches"
    );
    assert!(
        !outlined_symbols.contains(&"Tokenizer::count_tokens"),
        "generic token-processing symbols must not displace authentication-token matches"
    );
    assert!(
        !plan.symbol_bodies.is_empty(),
        "a useful context pack must reserve room for at least one source excerpt"
    );
    let evidence_paths = plan
        .symbol_bodies
        .iter()
        .map(|body| body.path.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    assert!(
        evidence_paths.len() >= 4,
        "a broad authentication pack must diversify evidence across at least four files: {evidence_paths:?}"
    );
    assert!(
        plan.rendered.contains("# Matching evidence excerpts"),
        "the rendered pack must identify bounded evidence rather than imply that excerpts are complete bodies"
    );
}

#[test]
fn test_plan_context_prioritizes_auth_concepts_before_generic_prompt_terms() {
    let fixture = Fixture::new();
    fixture.seed_authentication_repository();
    let task = "Find and concisely summarize all authentication-related code: login and signup \
                flows, auth providers, session/token/cookie handling, middleware/route protection, \
                API authorization checks, user identity, logout, and configuration/environment \
                variables.";

    let plan = plan_context(&fixture.store, fixture.repo, task, 800).expect("context should plan");

    assert_eq!(
        &plan.search_queries[..6],
        [
            "login",
            "oauth",
            "scopes",
            "credential",
            "update_env",
            "cookie"
        ]
    );
    for generic in [
        "concisely",
        "provider",
        "providers",
        "route",
        "protection",
        "api",
        "configuration",
        "environment",
        "variables",
    ] {
        assert!(
            !plan.search_queries.iter().any(|query| query == generic),
            "generic prompt term displaced an authentication concept: {generic}"
        );
    }
}

#[test]
fn test_plan_context_budget_tight_values_and_deterministic() {
    let fixture = Fixture::new();
    fixture.seed_context_repository();
    let before = fixture.store.total_changes().expect("changes should load");

    for budget in [0, 1, 2, 5, 10, 20, 40, 80, 160, 320, 800] {
        let first = plan_context(&fixture.store, fixture.repo, "session retry logic", budget)
            .expect("context should plan");
        let second = plan_context(&fixture.store, fixture.repo, "session retry logic", budget)
            .expect("context should repeat");
        let counter = counter_for(first.token_provider);

        assert_eq!(first, second, "budget {budget} must be deterministic");
        assert_eq!(first.tokens_used, counter.count(&first.rendered));
        assert!(
            first.tokens_used <= budget,
            "budget {budget} was exceeded by {} tokens",
            first.tokens_used
        );
    }

    let after = fixture.store.total_changes().expect("changes should load");
    assert_eq!(before, after, "budget planning must remain read-only");
}

#[test]
fn test_plan_context_symbol_bodies_are_task_fts_hits() {
    let fixture = Fixture::new();
    fixture.seed_context_repository();

    let plan = plan_context(
        &fixture.store,
        fixture.repo,
        "session retry logic",
        usize::MAX,
    )
    .expect("context should plan");
    let hits = plan
        .search_queries
        .iter()
        .flat_map(|query| {
            search(&fixture.store, fixture.repo, query, 100).expect("task search should work")
        })
        .collect::<Vec<_>>();

    assert!(!plan.symbol_bodies.is_empty());
    for body in &plan.symbol_bodies {
        assert!(
            hits.iter().any(|hit| {
                hit.path == body.path
                    && hit.symbol.qualified_name == body.symbol.qualified_name
                    && hit.symbol.kind == body.symbol.kind
                    && hit.symbol.start_line == body.symbol.start_line
                    && hit.symbol.end_line == body.symbol.end_line
            }),
            "body {:?} was not backed by a task FTS hit",
            body.symbol.qualified_name
        );
    }
    assert!(
        !plan
            .symbol_bodies
            .iter()
            .any(|body| body.symbol.qualified_name == "CacheManager")
    );
}

struct Fixture {
    _temp: TempDir,
    root: PathBuf,
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
            root,
            store,
            repo,
        }
    }

    fn seed_impact_chain(&self) {
        for path in ["a.rs", "b.rs", "c.rs", "d.rs", "tests/a_test.rs"] {
            self.store
                .upsert_file(self.repo, &file(path, "hash", 1))
                .expect("impact file should insert");
        }
        self.replace_edges("a.rs", &[("b.rs", EdgeKind::Imports)]);
        self.replace_edges("b.rs", &[("c.rs", EdgeKind::Imports)]);
        self.replace_edges(
            "c.rs",
            &[("c.rs", EdgeKind::References), ("d.rs", EdgeKind::Imports)],
        );
        self.replace_edges("tests/a_test.rs", &[("a.rs", EdgeKind::Tests)]);
    }

    fn replace_edges(&self, path: &str, edges: &[(&str, EdgeKind)]) {
        let file = self
            .store
            .upsert_file(self.repo, &file(path, "hash", 1))
            .expect("impact file should load");
        let edges = edges
            .iter()
            .map(|(target, kind)| EdgeRecord {
                to_path: (*target).to_owned(),
                kind: *kind,
                resolution: EdgeResolution::Resolved,
            })
            .collect::<Vec<_>>();
        self.store
            .replace_symbols(file, &[], &edges)
            .expect("impact edges should replace");
    }

    fn seed_context_repository(&self) {
        let session_source = concat!(
            "pub struct SessionManager {\n",
            "    attempts: usize,\n",
            "}\n",
            "\n",
            "impl SessionManager {\n",
            "    pub fn retry_session(&self) -> bool {\n",
            "        self.attempts < 3\n",
            "    }\n",
            "}\n",
        );
        let cache_source = concat!(
            "pub struct CacheManager;\n",
            "\n",
            "impl CacheManager {\n",
            "    pub fn clear(&self) {}\n",
            "}\n",
        );
        self.write_indexed_file(
            "src/session.rs",
            session_source,
            &[
                symbol(
                    "SessionManager",
                    "SessionManager",
                    SymbolKind::Struct,
                    "pub struct SessionManager // session retry logic",
                    1,
                    3,
                ),
                symbol(
                    "retry_session",
                    "SessionManager::retry_session",
                    SymbolKind::Method,
                    "pub fn retry_session(&self) -> bool",
                    6,
                    8,
                ),
            ],
        );
        self.write_indexed_file(
            "src/cache.rs",
            cache_source,
            &[symbol(
                "CacheManager",
                "CacheManager",
                SymbolKind::Struct,
                "pub struct CacheManager",
                1,
                1,
            )],
        );
    }

    fn seed_authentication_repository(&self) {
        self.write_indexed_file(
            "src/youtube_oauth.rs",
            "pub fn get_oauth_token() -> &'static str { \"token\" }\n",
            &[symbol(
                "get_oauth_token",
                "YoutubeOAuth::get_oauth_token",
                SymbolKind::Function,
                "pub fn get_oauth_token(credentials: Credentials) -> AccessToken",
                1,
                1,
            )],
        );
        self.write_indexed_file(
            "src/credentials.rs",
            "pub fn load_credentials() -> bool { true }\n",
            &[symbol(
                "load_credentials",
                "CredentialStore::load_credentials",
                SymbolKind::Function,
                "pub fn load_credentials() -> Credentials",
                1,
                1,
            )],
        );
        self.write_indexed_file(
            "src/instagram.rs",
            "pub fn login() -> bool { true }\n",
            &[symbol(
                "login",
                "InstagramClient::login",
                SymbolKind::Method,
                "pub fn login(username: &str, password: &str, session: &Session) -> bool",
                1,
                1,
            )],
        );
        self.write_indexed_file(
            "src/browser_cookies.rs",
            "pub fn read_browser_cookies() -> bool { true }\n",
            &[symbol(
                "read_browser_cookies",
                "BrowserCookieAuth::read_browser_cookies",
                SymbolKind::Function,
                "pub fn read_browser_cookies() -> CookieJar",
                1,
                1,
            )],
        );
        self.write_indexed_file(
            "src/provider.rs",
            "pub fn complete() -> bool { true }\n",
            &[symbol(
                "complete",
                "ModelProvider::complete",
                SymbolKind::Method,
                "pub fn complete(prompt: &str) -> bool",
                1,
                1,
            )],
        );
        self.write_indexed_file(
            "src/tokenizer.rs",
            "pub fn count_tokens() -> usize { 0 }\n",
            &[symbol(
                "count_tokens",
                "Tokenizer::count_tokens",
                SymbolKind::Method,
                "pub fn count_tokens(input: &str) -> usize",
                1,
                1,
            )],
        );
    }

    fn write_indexed_file(&self, path: &str, source: &str, symbols: &[SymbolRecord]) {
        let destination = self.root.join(path);
        fs::create_dir_all(
            destination
                .parent()
                .expect("fixture path should have a parent"),
        )
        .expect("fixture directory should be created");
        fs::write(&destination, source).expect("fixture source should be written");
        self.store
            .replace_indexed_file(
                self.repo,
                &file(
                    path,
                    blake3::hash(source.as_bytes()).to_hex().as_str(),
                    counter_for(TokenProvider::OpenAiO200k).count(source),
                ),
                symbols,
                &[],
            )
            .expect("context file should index");
    }
}

fn file(path: &str, hash: &str, token_estimate: usize) -> FileRecord {
    FileRecord {
        path: path.to_owned(),
        hash: hash.to_owned(),
        language: Some(FileLanguage::Rust),
        size_bytes: 100,
        token_estimate,
        indexed_at: 10,
    }
}

fn symbol(
    name: &str,
    qualified_name: &str,
    kind: SymbolKind,
    signature: &str,
    start_line: u32,
    end_line: u32,
) -> SymbolRecord {
    SymbolRecord {
        name: name.to_owned(),
        qualified_name: qualified_name.to_owned(),
        kind,
        signature: Some(signature.to_owned()),
        start_line,
        end_line,
        token_estimate: 10,
    }
}

fn _assert_context_plan_is_public(_: &ContextPlan) {}
