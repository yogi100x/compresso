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
                    package: None,
                },
                ImpactEntry {
                    path: "a.rs".to_owned(),
                    depth: 2,
                    edge_kinds: vec![EdgeKind::Imports],
                    package: None,
                },
            ],
            total: 2,
            unresolved_edges: 0,
            ambiguous_edges: 0,
            complete: true,
            truncated: false,
            unevaluated: 0,
            unproven: 0,
            packages_limited: false,
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
            package: None,
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

#[test]
fn test_plan_context_ranks_compound_identifier_and_body_matches_above_generic_word_hits() {
    let fixture = Fixture::new();
    fixture.seed_oversize_repository();
    let task = "How does a repository file read through the MCP server get rejected when the \
                file is too large: size limit, error variant, where raised";

    let plan =
        plan_context(&fixture.store, fixture.repo, task, 1_600).expect("context should plan");
    let evidence = plan
        .symbol_bodies
        .iter()
        .map(|body| body.symbol.qualified_name.as_str())
        .collect::<Vec<_>>();

    assert!(
        evidence.len() >= 2,
        "the pack must carry evidence: {evidence:?}"
    );
    let leading = &evidence[..2];
    assert!(
        leading.contains(&"file_too_large") && leading.contains(&"read_utf8_capped"),
        "symbols naming or raising the task's compound concept must lead the pack: {evidence:?}"
    );
    let raised = plan
        .symbol_bodies
        .iter()
        .find(|body| body.symbol.qualified_name == "read_utf8_capped")
        .expect("the raising function must be included");
    assert!(
        raised.content.contains("FileTooLarge"),
        "a long body's excerpt must be centred on the matching lines: {:?}",
        raised.content
    );
    assert!(raised.excerpt_start_line > raised.symbol.start_line);
    assert!(
        plan.rendered.contains(&format!(
            "## src/queries.rs:{}-{}",
            raised.excerpt_start_line, raised.excerpt_end_line
        )),
        "the evidence header must cite the excerpt's own line range: {}",
        plan.rendered
    );
    assert!(
        !leading.contains(&"test_file_read_large"),
        "a test matching three generic words must not lead product code: {evidence:?}"
    );
    assert!(plan.concept_coverage_sufficient());
}

#[test]
fn test_plan_context_reports_uncovered_concepts_when_only_generic_words_match() {
    let fixture = Fixture::new();
    fixture.seed_oversize_repository();
    let task = "How is the kerberos ticket renewal schedule read from a keytab file";

    let plan =
        plan_context(&fixture.store, fixture.repo, task, 1_600).expect("context should plan");

    assert!(
        plan.matches_total > 0,
        "generic words still match decoy symbols"
    );
    assert!(plan.task_concepts_total >= 4);
    assert!(
        !plan.concept_coverage_sufficient(),
        "evidence matching only generic words must not count as covering the task: covered {} of {}",
        plan.task_concepts_covered,
        plan.task_concepts_total
    );
    for missing in ["kerberos", "keytab", "renewal"] {
        assert!(
            plan.uncovered_concepts.iter().any(|term| term == missing),
            "{missing} must be reported as uncovered: {:?}",
            plan.uncovered_concepts
        );
    }
}

#[test]
fn test_plan_context_test_code_ranks_normally_when_the_task_asks_for_tests() {
    let fixture = Fixture::new();
    fixture.seed_oversize_repository();

    let plan = plan_context(
        &fixture.store,
        fixture.repo,
        "which tests cover a large file read",
        1_600,
    )
    .expect("context should plan");

    assert_eq!(
        plan.symbol_bodies
            .first()
            .map(|body| body.symbol.qualified_name.as_str()),
        Some("test_file_read_large"),
        "a task about tests must surface the test matching the most concepts first"
    );
}

#[test]
fn test_plan_context_body_only_compound_reaches_the_pack_and_leads_it() {
    let fixture = Fixture::new();
    fixture.seed_body_only_repository();
    let task = "Where do we handle SIGTERM for a graceful shutdown?";
    // Below 1,152 tokens the top excerpt allowance is 96, less than the body.
    let before = fixture.store.total_changes().expect("changes should load");

    let plan =
        plan_context(&fixture.store, fixture.repo, task, 1_100).expect("context should plan");
    let repeated =
        plan_context(&fixture.store, fixture.repo, task, 1_100).expect("context should repeat");
    let after = fixture.store.total_changes().expect("changes should load");

    assert_eq!(plan, repeated, "the content scan must be deterministic");
    assert_eq!(before, after, "the content scan must not write to SQLite");
    let leading = plan
        .symbol_bodies
        .first()
        .expect("the pack must carry evidence");
    assert_eq!(
        (
            leading.path.as_str(),
            leading.symbol.qualified_name.as_str()
        ),
        ("src/worker.rs", "Worker::on_signal"),
        "the smallest symbol whose body spells the compound must lead: {}",
        plan.rendered
    );
    assert!(
        leading.content.contains("graceful shutdown"),
        "the excerpt must show the matching line: {:?}",
        leading.content
    );
    assert!(leading.excerpt_start_line > leading.symbol.start_line);
    assert!(plan.concept_coverage_sufficient());
    assert!(!plan.content_scan_truncated);
    assert!(
        plan.symbol_bodies
            .iter()
            .all(|body| body.symbol.qualified_name != "Worker"),
        "a match maps to the smallest enclosing symbol only: {}",
        plan.rendered
    );
}

#[test]
fn test_plan_context_body_only_single_word_reaches_the_pack_with_the_matching_line() {
    let fixture = Fixture::new();
    fixture.seed_body_only_repository();

    let plan = plan_context(
        &fixture.store,
        fixture.repo,
        "Where is the checksum of an upload verified?",
        1_600,
    )
    .expect("context should plan");

    let finalize = plan
        .symbol_bodies
        .iter()
        .find(|body| body.symbol.qualified_name == "finalize")
        .unwrap_or_else(|| {
            panic!(
                "a word written only in a body must reach the pack: {}",
                plan.rendered
            )
        });
    assert_eq!(finalize.path, "src/blobs.rs");
    assert!(
        finalize.content.contains("checksum"),
        "the excerpt must be centred on the matching line: {:?}",
        finalize.content
    );
    assert!(finalize.excerpt_start_line > finalize.symbol.start_line);
    assert!(
        !plan
            .uncovered_concepts
            .iter()
            .any(|term| term == "checksum"),
        "the word is covered once its body is evidence: {:?}",
        plan.uncovered_concepts
    );
}

#[test]
fn test_plan_context_content_scan_never_reads_secret_or_oversize_files() {
    let fixture = Fixture::new();
    fixture.seed_body_only_repository();
    let hidden = "fn load() {\n    // handles SIGTERM for a graceful shutdown\n}\n";
    fixture.write_indexed_file(
        "deploy/.env.rs",
        hidden,
        &[symbol(
            "load",
            "load",
            SymbolKind::Function,
            "fn load()",
            1,
            3,
        )],
    );
    let mut oversize = String::from(hidden);
    while oversize.len() <= 512 * 1024 {
        oversize.push_str("// padding padding padding padding padding padding padding\n");
    }
    fixture.write_indexed_file(
        "generated/bundle.rs",
        &oversize,
        &[symbol(
            "load",
            "load",
            SymbolKind::Function,
            "fn load()",
            1,
            3,
        )],
    );
    fs::remove_file(fixture.root.join("src/worker.rs")).expect("worker source should be removed");

    let plan = plan_context(
        &fixture.store,
        fixture.repo,
        "Where do we handle SIGTERM for a graceful shutdown?",
        1_600,
    )
    .expect("an unreadable indexed file must not fail the plan");

    assert!(
        plan.symbol_bodies
            .iter()
            .all(|body| body.path != "deploy/.env.rs" && body.path != "generated/bundle.rs"),
        "secret and oversize files must never be scanned: {}",
        plan.rendered
    );
    assert!(!plan.rendered.contains("graceful shutdown"));
    assert!(
        plan.uncovered_concepts.iter().any(|term| term == "sigterm"),
        "content that was not scanned cannot cover a concept: {:?}",
        plan.uncovered_concepts
    );
    assert!(!plan.content_scan_truncated, "skipped files are not a cap");
}

#[test]
fn test_plan_context_content_scan_file_cap_sets_truncated() {
    let fixture = Fixture::new();
    for index in 0..2_000 {
        fixture
            .store
            .upsert_file(
                fixture.repo,
                &file(&format!("bulk/file_{index:04}.rs"), "hash", 1),
            )
            .expect("bulk file should index");
    }
    fixture.write_indexed_file(
        "zeta/late.rs",
        "fn late() {\n    // drains on a graceful shutdown\n}\n",
        &[symbol(
            "late",
            "late",
            SymbolKind::Function,
            "fn late()",
            1,
            3,
        )],
    );
    let task = "Where is a graceful shutdown handled?";

    let plan =
        plan_context(&fixture.store, fixture.repo, task, 1_600).expect("context should plan");

    assert!(plan.content_scan_truncated);
    assert!(plan.truncated);
    assert!(
        plan.symbol_bodies.is_empty(),
        "a file past the cap is not read: {}",
        plan.rendered
    );

    fixture
        .store
        .delete_files(fixture.repo, &["bulk/file_0000.rs".to_owned()])
        .expect("one bulk file should be removed");
    let within =
        plan_context(&fixture.store, fixture.repo, task, 1_600).expect("context should plan");
    assert!(!within.content_scan_truncated);
    assert_eq!(
        within
            .symbol_bodies
            .iter()
            .map(|body| body.path.as_str())
            .collect::<Vec<_>>(),
        ["zeta/late.rs"]
    );
}

#[test]
fn test_plan_context_content_scan_ignores_a_word_common_to_many_bodies() {
    let fixture = Fixture::new();
    for index in 0..9 {
        fixture.write_indexed_file(
            &format!("src/step_{index}.rs"),
            &format!("fn step_{index}() {{\n    // flush the ledger buffer\n}}\n"),
            &[symbol(
                &format!("step_{index}"),
                &format!("step_{index}"),
                SymbolKind::Function,
                &format!("fn step_{index}()"),
                1,
                3,
            )],
        );
    }

    let plan = plan_context(
        &fixture.store,
        fixture.repo,
        "Where is the ledger reconciled?",
        1_600,
    )
    .expect("context should plan");

    assert!(
        plan.symbol_bodies.is_empty(),
        "a word found in more bodies than a specific query returns is not evidence: {}",
        plan.rendered
    );
    assert!(!plan.concept_coverage_sufficient());
}

#[test]
fn test_plan_context_content_scan_reads_text_files_only_when_the_task_asks_for_docs() {
    let fixture = Fixture::new();
    let source = "# Operations\n\nWorkers drain on a graceful shutdown.\n";
    fs::write(fixture.root.join("notes.md"), source).expect("notes should be written");
    fixture
        .store
        .replace_indexed_file(
            fixture.repo,
            &FileRecord {
                language: Some(FileLanguage::Text),
                ..file(
                    "notes.md",
                    blake3::hash(source.as_bytes()).to_hex().as_str(),
                    12,
                )
            },
            &[symbol(
                "Operations",
                "Operations",
                SymbolKind::Module,
                "# Operations",
                1,
                3,
            )],
            &[],
        )
        .expect("notes should index");

    let code_task = plan_context(
        &fixture.store,
        fixture.repo,
        "Where is a graceful shutdown handled?",
        1_600,
    )
    .expect("context should plan");
    assert!(
        code_task.symbol_bodies.is_empty(),
        "prose is not scanned for a code task: {}",
        code_task.rendered
    );

    let docs_task = plan_context(
        &fixture.store,
        fixture.repo,
        "Which docs describe a graceful shutdown?",
        1_600,
    )
    .expect("context should plan");
    assert_eq!(
        docs_task
            .symbol_bodies
            .iter()
            .map(|body| body.path.as_str())
            .collect::<Vec<_>>(),
        ["notes.md"]
    );
}

#[test]
fn test_plan_context_content_scan_work_stays_linear_on_one_very_long_line() {
    let fixture = Fixture::new();
    // One 400 KiB line repeating the scanned word, outside every symbol: each
    // match must not rescan the line from its start.
    let long_line = "sigterm ".repeat(50_000);
    let source = format!("// {long_line}\nfn drain() {{ stop(); }}\n");
    fixture.write_indexed_file(
        "src/drain.rs",
        &source,
        &[symbol(
            "drain",
            "drain",
            SymbolKind::Function,
            "fn drain()",
            2,
            2,
        )],
    );

    let started = std::time::Instant::now();
    let plan = plan_context(
        &fixture.store,
        fixture.repo,
        "where is sigterm handled while draining",
        1_600,
    )
    .expect("context should plan");
    let elapsed = started.elapsed();

    assert!(
        elapsed < std::time::Duration::from_secs(3),
        "scanning one long line took {elapsed:?}; work must be linear in file size"
    );
    assert!(
        plan.symbol_bodies
            .iter()
            .all(|body| !body.content.contains("sigterm")),
        "a line outside every symbol is never evidence"
    );
}

const USE_SITE_TASK: &str = "How does a repository file read get rejected when the file is too \
                             large: size limit, error variant, where raised";

fn evidence_names(plan: &ContextPlan, path: &str) -> Vec<String> {
    plan.symbol_bodies
        .iter()
        .filter(|body| body.path == path)
        .map(|body| body.symbol.qualified_name.clone())
        .collect()
}

#[test]
fn test_plan_context_use_site_of_a_named_concept_joins_its_definition_in_one_file() {
    let fixture = Fixture::new();
    fixture.seed_use_site_repository();

    let plan = plan_context(&fixture.store, fixture.repo, USE_SITE_TASK, 4_000)
        .expect("context should plan");

    let leading = plan
        .symbol_bodies
        .first()
        .expect("the pack must carry evidence");
    assert_eq!(
        leading.symbol.qualified_name, "file_too_large_code",
        "the symbol named after the concept leads its use sites: {}",
        plan.rendered
    );
    let use_sites = evidence_names(&plan, "src/queries.rs");
    assert_eq!(
        use_sites.len(),
        3,
        "a file holds a third item only for a use site, and never a fourth: {use_sites:?}"
    );
    assert!(
        use_sites.iter().any(|name| name == "read_utf8_capped"),
        "the raise site must join the enum and the incidental user: {use_sites:?}"
    );
    let raised = plan
        .symbol_bodies
        .iter()
        .find(|body| body.symbol.qualified_name == "read_utf8_capped")
        .expect("the raise site is evidence");
    assert!(raised.content.contains("QueryError::FileTooLarge"));
    let generic = evidence_names(&plan, "src/mode.rs");
    assert!(
        generic.len() <= 2,
        "symbols that use no named concept keep the two-per-file share: {generic:?}"
    );
    assert!(
        !use_sites
            .iter()
            .any(|name| name == "test_capped_read_reports_file_too_large"),
        "a test that uses the concept is lowered below the product code that does: {use_sites:?}"
    );
    assert!(plan.concept_coverage_sufficient());
}

#[test]
fn test_plan_context_is_not_file_complete_while_a_use_site_excerpt_is_left_out() {
    let fixture = Fixture::new();
    fixture.seed_use_site_repository();
    let full = plan_context(&fixture.store, fixture.repo, USE_SITE_TASK, 4_000)
        .expect("context should plan");
    assert_eq!(evidence_names(&full, "src/queries.rs").len(), 3);
    assert_eq!(full.evidence_files_included, full.evidence_files_total);

    let mut partial_packs = 0;
    for budget in (200..=700).step_by(20) {
        let plan = plan_context(&fixture.store, fixture.repo, USE_SITE_TASK, budget)
            .expect("context should plan");
        let use_sites = evidence_names(&plan, "src/queries.rs").len();
        if (1..3).contains(&use_sites) {
            partial_packs += 1;
            assert!(
                plan.evidence_files_included < plan.evidence_files_total,
                "budget {budget}: a file missing one of its use-site excerpts must not count \
                 as included: {}",
                plan.rendered
            );
            assert!(plan.truncated);
        }
    }
    assert!(
        partial_packs > 0,
        "some budget must fit part of the file's use-site evidence"
    );
}

#[test]
fn test_plan_context_use_site_expansion_stays_within_the_content_scan_caps() {
    let fixture = Fixture::new();
    fixture.write_indexed_file(
        "alpha/codes.rs",
        "pub fn file_too_large_code() -> &'static str {\n    \"file_too_large\"\n}\n",
        &[symbol(
            "file_too_large_code",
            "file_too_large_code",
            SymbolKind::Function,
            "pub fn file_too_large_code() -> &'static str",
            1,
            3,
        )],
    );
    let raise = "fn check(length: u64) -> Result<(), QueryError> {\n    Err(QueryError::FileTooLarge { length })\n}\n";
    let check = symbol(
        "check",
        "check",
        SymbolKind::Function,
        "fn check(length: u64) -> Result<(), QueryError>",
        1,
        3,
    );
    let mut oversize = String::from(raise);
    while oversize.len() <= 512 * 1024 {
        oversize.push_str("// padding padding padding padding padding padding padding\n");
    }
    fixture.write_indexed_file(
        "generated/bundle.rs",
        &oversize,
        std::slice::from_ref(&check),
    );
    for index in 0..1_998 {
        fixture
            .store
            .upsert_file(
                fixture.repo,
                &file(&format!("bulk/file_{index:04}.rs"), "hash", 1),
            )
            .expect("bulk file should index");
    }
    fixture.write_indexed_file("zeta/late.rs", raise, std::slice::from_ref(&check));
    let task = "Where is file too large raised?";

    let capped =
        plan_context(&fixture.store, fixture.repo, task, 1_600).expect("context should plan");

    assert!(
        capped.content_scan_truncated && capped.truncated,
        "the use-site scan is the one bounded content scan and reports its file cap"
    );
    assert_eq!(
        capped
            .symbol_bodies
            .iter()
            .map(|body| body.path.as_str())
            .collect::<Vec<_>>(),
        ["alpha/codes.rs"],
        "a use site past the file cap or in an oversize file is never read: {}",
        capped.rendered
    );

    fixture
        .store
        .delete_files(fixture.repo, &["bulk/file_0000.rs".to_owned()])
        .expect("one bulk file should be removed");
    let within =
        plan_context(&fixture.store, fixture.repo, task, 1_600).expect("context should plan");
    assert!(!within.content_scan_truncated);
    assert_eq!(
        within
            .symbol_bodies
            .iter()
            .map(|body| body.path.as_str())
            .collect::<Vec<_>>(),
        ["alpha/codes.rs", "zeta/late.rs"],
        "inside the caps the use site follows the definition: {}",
        within.rendered
    );
}

#[test]
fn test_plan_context_includes_the_definition_of_a_constant_an_excerpt_references() {
    let fixture = Fixture::new();
    fixture.seed_use_site_repository();
    let counter = counter_for(TokenProvider::OpenAiO200k);

    let plan = plan_context(&fixture.store, fixture.repo, USE_SITE_TASK, 4_000)
        .expect("context should plan");

    let constants = plan
        .referenced_constants
        .iter()
        .map(|body| (body.path.as_str(), body.symbol.name.as_str()))
        .collect::<Vec<_>>();
    assert!(
        constants.contains(&("src/limits.rs", "UTF8_CEILING_BYTES")),
        "the limit behind the name an excerpt compares against must be included: {constants:?}"
    );
    assert!(
        !constants.iter().any(|(_, name)| *name == "DEFAULT_MODE"),
        "an off-task constant referenced by a lower-ranked excerpt is not included: {constants:?}"
    );
    assert!(
        plan.rendered.contains(
            "## src/limits.rs:1-1 — UTF8_CEILING_BYTES (Const, referenced above)\n\
             pub const UTF8_CEILING_BYTES: u64 = 16 * 1024 * 1024;\n"
        ),
        "the definition line follows the excerpt that references it: {}",
        plan.rendered
    );
    let referencing = plan
        .rendered
        .find("UTF8_CEILING_BYTES")
        .expect("an excerpt references the constant");
    assert!(
        referencing
            < plan
                .rendered
                .find("## src/limits.rs:1-1")
                .expect("definition"),
        "a constant never precedes the excerpt that references it"
    );
    assert!(
        plan.symbol_bodies
            .iter()
            .all(|body| body.path != "src/limits.rs"),
        "a referenced constant is not ranked evidence"
    );
    assert_eq!(
        plan.evidence_files_included,
        plan.symbol_bodies
            .iter()
            .map(|body| body.path.as_str())
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        "referenced constants do not count as evidence files"
    );
    assert_eq!(plan.tokens_used, counter.count(&plan.rendered));
}

#[test]
fn test_plan_context_referenced_constants_are_capped_in_count_and_tokens() {
    let fixture = Fixture::new();
    let mut tunables = String::new();
    let mut symbols = Vec::new();
    for (line, name) in ["ALPHA_STEP", "BRAVO_STEP", "CHARLIE_STEP", "DELTA_STEP"]
        .into_iter()
        .enumerate()
    {
        tunables.push_str(&format!("pub const {name}: u32 = {line};\n"));
        let line = u32::try_from(line + 1).expect("line fits");
        symbols.push(symbol(
            name,
            name,
            SymbolKind::Const,
            &format!("pub const {name}: u32"),
            line,
            line,
        ));
    }
    tunables.push_str("pub const ECHO_STEPS: [u32; 64] = [\n");
    for row in 0..16 {
        tunables.push_str(&format!("    {row}, {row}, {row}, {row},\n"));
    }
    tunables.push_str("];\npub const FOXTROT_STEP: u32 = 6;\n");
    symbols.push(symbol(
        "ECHO_STEPS",
        "ECHO_STEPS",
        SymbolKind::Const,
        "pub const ECHO_STEPS: [u32; 64]",
        5,
        22,
    ));
    symbols.push(symbol(
        "FOXTROT_STEP",
        "FOXTROT_STEP",
        SymbolKind::Const,
        "pub const FOXTROT_STEP: u32",
        23,
        23,
    ));
    fixture.write_indexed_file("src/tunables.rs", &tunables, &symbols);
    fixture.write_indexed_file(
        "src/table.rs",
        "fn build_frame_budget_table() -> Vec<u32> {\n    let mut table = vec![ECHO_STEPS[0], ALPHA_STEP, BRAVO_STEP];\n    table.extend([CHARLIE_STEP, DELTA_STEP, FOXTROT_STEP, UNKNOWN_STEP]);\n    table\n}\n",
        &[symbol(
            "build_frame_budget_table",
            "build_frame_budget_table",
            SymbolKind::Function,
            "fn build_frame_budget_table() -> Vec<u32>",
            1,
            5,
        )],
    );
    let counter = counter_for(TokenProvider::OpenAiO200k);

    let plan = plan_context(
        &fixture.store,
        fixture.repo,
        "Where is the frame budget table built?",
        4_000,
    )
    .expect("context should plan");

    assert_eq!(
        plan.referenced_constants
            .iter()
            .map(|body| body.symbol.name.as_str())
            .collect::<Vec<_>>(),
        ["ECHO_STEPS", "ALPHA_STEP", "BRAVO_STEP", "CHARLIE_STEP"],
        "at most four constants, in the order the excerpt references them: {}",
        plan.rendered
    );
    for constant in &plan.referenced_constants {
        assert!(
            counter.count(&constant.content) <= 40,
            "{} exceeds the per-constant allowance",
            constant.symbol.name
        );
    }
    let long = &plan.referenced_constants[0];
    assert!(long.truncated && long.excerpt_start_line == 5 && long.excerpt_end_line < 22);
    assert!(
        !plan.truncated,
        "an omitted constant is not missing evidence"
    );

    for budget in [60, 80, 100, 120, 140] {
        let tight = plan_context(
            &fixture.store,
            fixture.repo,
            "Where is the frame budget table built?",
            budget,
        )
        .expect("context should plan");
        assert!(tight.tokens_used <= budget, "budget {budget} was exceeded");
        assert!(
            tight.referenced_constants.len() < 4,
            "constants that do not fit a tight budget are left out, not forced in"
        );
        if !tight.symbol_bodies.is_empty() {
            assert_eq!(
                tight.evidence_files_included, tight.evidence_files_total,
                "a constant left out never withholds readiness at budget {budget}"
            );
        }
    }
}

#[test]
fn test_plan_context_drops_broad_word_tail_only_when_specific_evidence_covers_the_task() {
    let fixture = Fixture::new();
    fixture.seed_use_site_repository();
    fixture.seed_broad_word_tail();
    let tail = |plan: &ContextPlan| {
        plan.symbol_bodies
            .iter()
            .filter(|body| body.path.starts_with("src/mcp/"))
            .count()
    };

    let covered = plan_context(
        &fixture.store,
        fixture.repo,
        &format!("MCP server: {USE_SITE_TASK}"),
        4_000,
    )
    .expect("context should plan");

    // D95: `MCP server` is a part of the task of its own, so the one symbol
    // that best covers it stays as that part's evidence; the rest of the
    // broad-word tail is dropped.
    assert_eq!(
        tail(&covered),
        1,
        "symbols matching only broad words are dropped once the task is covered: {}",
        covered.rendered
    );
    assert!(covered.concept_coverage_sufficient());
    assert_eq!(
        covered.evidence_files_included, covered.evidence_files_total,
        "dropped tail files are not candidates, so they do not withhold readiness"
    );
    assert!(
        covered
            .symbol_bodies
            .iter()
            .any(|body| body.symbol.qualified_name == "variant_name"),
        "a symbol named after a specific task word is not tail: {}",
        covered.rendered
    );

    let uncovered = plan_context(
        &fixture.store,
        fixture.repo,
        &format!(
            "MCP server: {USE_SITE_TASK}, handshake negotiation capabilities transport framing \
             protocol heartbeat keepalive multiplexing"
        ),
        4_000,
    )
    .expect("context should plan");

    assert!(
        tail(&uncovered) > 0,
        "the tail stays while the stronger evidence leaves most of the task uncovered: {}",
        uncovered.rendered
    );
}

#[test]
fn test_plan_context_top_evidence_gets_a_larger_excerpt_that_scales_with_the_budget() {
    let fixture = Fixture::new();
    fixture.seed_use_site_repository();
    let counter = counter_for(TokenProvider::OpenAiO200k);
    let raise_site = |plan: &ContextPlan| {
        plan.symbol_bodies
            .iter()
            .find(|body| body.symbol.qualified_name == "read_utf8_capped")
            .cloned()
            .unwrap_or_else(|| panic!("the raise site must be evidence: {}", plan.rendered))
    };

    let wide = plan_context(&fixture.store, fixture.repo, USE_SITE_TASK, 4_000)
        .expect("context should plan");
    let narrow = plan_context(&fixture.store, fixture.repo, USE_SITE_TASK, 1_600)
        .expect("context should plan");

    let whole = raise_site(&wide);
    let whole_tokens = counter.count(&whole.content);
    assert!(
        !whole.truncated && whole_tokens > 96 && whole_tokens <= 224,
        "at the default ceiling a top item's body of {whole_tokens} tokens is shown whole"
    );
    let large = wide
        .symbol_bodies
        .iter()
        .filter(|body| counter.count(&body.content) > 96)
        .count();
    assert!(
        (1..=3).contains(&large),
        "only the top three items may exceed the strong allowance, not {large}"
    );
    assert!(
        wide.symbol_bodies
            .iter()
            .all(|body| counter.count(&body.content) <= 224)
    );

    let excerpt = raise_site(&narrow);
    assert!(
        excerpt.truncated && counter.count(&excerpt.content) <= 133,
        "a 1,600-token pack keeps top excerpts at one twelfth of the budget"
    );
    assert!(
        excerpt.content.contains("QueryError::FileTooLarge"),
        "the excerpt stays centred on the matching line: {:?}",
        excerpt.content
    );
    assert_eq!(
        excerpt.excerpt_end_line, excerpt.symbol.end_line,
        "a match near the end of a body is shown with the lines above it"
    );
    assert!(excerpt.excerpt_start_line + 2 < excerpt.excerpt_end_line);
}

#[test]
fn test_plan_context_expansion_never_exceeds_any_budget_and_is_deterministic() {
    let fixture = Fixture::new();
    fixture.seed_use_site_repository();
    fixture.seed_broad_word_tail();
    let before = fixture.store.total_changes().expect("changes should load");

    for budget in [
        0, 1, 8, 20, 40, 64, 100, 150, 200, 260, 320, 400, 520, 640, 800, 1_200, 1_600, 4_000,
    ] {
        let first = plan_context(&fixture.store, fixture.repo, USE_SITE_TASK, budget)
            .expect("context should plan");
        let second = plan_context(&fixture.store, fixture.repo, USE_SITE_TASK, budget)
            .expect("context should repeat");
        let counter = counter_for(first.token_provider);

        assert_eq!(first, second, "budget {budget} must be deterministic");
        assert_eq!(first.tokens_used, counter.count(&first.rendered));
        assert!(
            first.tokens_used <= budget,
            "budget {budget} was exceeded by {} tokens",
            first.tokens_used
        );
        assert!(first.referenced_constants.len() <= 4);
        for constant in &first.referenced_constants {
            assert!(
                first.rendered.contains(&constant.content),
                "a reported constant is in the rendered pack at budget {budget}"
            );
        }
    }

    let after = fixture.store.total_changes().expect("changes should load");
    assert_eq!(before, after, "expansion must remain read-only");
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

    fn seed_oversize_repository(&self) {
        let raising_source = concat!(
            "pub const MAX_READ_FILE_BYTES: u64 = 16 * 1024 * 1024;\n",
            "\n",
            "fn read_utf8_capped(path: &Path) -> Result<String, QueryError> {\n",
            "    let metadata = fs::metadata(path)?;\n",
            "    let length = metadata.len();\n",
            "    let handle = File::open(path)?;\n",
            "    let mut reader = BufReader::new(handle);\n",
            "    let mut buffer = String::new();\n",
            "    let capacity = usize::try_from(length).unwrap_or(0);\n",
            "    buffer.reserve(capacity);\n",
            "    let ceiling = MAX_READ_FILE_BYTES;\n",
            "    let within = length <= ceiling;\n",
            "    let checked = within;\n",
            "    let allowed = checked;\n",
            "    if !allowed {\n",
            "        return Err(QueryError::FileTooLarge { path: path.to_owned(), limit_bytes: ceiling });\n",
            "    }\n",
            "    reader.read_to_string(&mut buffer)?;\n",
            "    Ok(buffer)\n",
            "}\n",
        );
        self.write_indexed_file(
            "src/queries.rs",
            raising_source,
            &[
                symbol(
                    "MAX_READ_FILE_BYTES",
                    "MAX_READ_FILE_BYTES",
                    SymbolKind::Const,
                    "pub const MAX_READ_FILE_BYTES: u64",
                    1,
                    1,
                ),
                symbol(
                    "read_utf8_capped",
                    "read_utf8_capped",
                    SymbolKind::Function,
                    "fn read_utf8_capped(path: &Path) -> Result<String, QueryError>",
                    3,
                    20,
                ),
            ],
        );
        self.write_indexed_file(
            "src/retrieval.rs",
            "fn file_too_large(limit_bytes: u64) -> RetrievalError {\n    RetrievalError::new(\"file_too_large\", limit_bytes)\n}\n",
            &[symbol(
                "file_too_large",
                "file_too_large",
                SymbolKind::Function,
                "fn file_too_large(limit_bytes: u64) -> RetrievalError",
                1,
                3,
            )],
        );
        self.write_indexed_file(
            "tests/read_test.rs",
            "fn test_file_read_large() {\n    assert!(large_file_read().is_err());\n}\n",
            &[symbol(
                "test_file_read_large",
                "test_file_read_large",
                SymbolKind::Function,
                "fn test_file_read_large()",
                1,
                3,
            )],
        );
        for (path, name, signature) in [
            (
                "src/mode.rs",
                "file_mode",
                "fn file_mode(path: &Path) -> u32",
            ),
            ("src/config.rs", "FileMcp", "struct FileMcp"),
            ("src/input.rs", "large_input", "fn large_input() -> Vec<u8>"),
            (
                "src/server.rs",
                "server_tools",
                "fn server_tools() -> Vec<Tool>",
            ),
            (
                "src/reader.rs",
                "reader",
                "fn reader(input: &[u8]) -> Reader",
            ),
            ("src/limits.rs", "RecallLimits", "struct RecallLimits"),
            (
                "src/errors.rs",
                "map_error",
                "fn map_error(error: Error) -> Failure",
            ),
            (
                "src/variant.rs",
                "variant_name",
                "fn variant_name() -> &'static str",
            ),
            ("src/size.rs", "size_hint", "fn size_hint() -> usize"),
            (
                "src/repo.rs",
                "repository_root",
                "fn repository_root() -> PathBuf",
            ),
        ] {
            self.write_indexed_file(
                path,
                &format!("{signature} {{ todo!() }}\n"),
                &[symbol(name, name, SymbolKind::Function, signature, 1, 1)],
            );
        }
    }

    fn seed_body_only_repository(&self) {
        let worker_source = concat!(
            "pub struct Worker {\n",
            "    stopping: AtomicBool,\n",
            "}\n",
            "\n",
            "impl Worker {\n",
            "    pub fn on_signal(&self, number: i32) {\n",
            "        let received = number;\n",
            "        let previous = self.stopping.load(Ordering::Acquire);\n",
            "        let already = previous;\n",
            "        let counter = self.signals.fetch_add(1, Ordering::Relaxed);\n",
            "        let total = counter + 1;\n",
            "        self.metrics.record_signal(received, total, already);\n",
            "        // 15 is SIGTERM, which asks for a graceful shutdown: finish the\n",
            "        // job in hand, then leave the loop.\n",
            "        if received == 15 {\n",
            "            self.stopping.store(true, Ordering::Release);\n",
            "        }\n",
            "    }\n",
            "}\n",
        );
        self.write_indexed_file(
            "src/worker.rs",
            worker_source,
            &[
                symbol(
                    "Worker",
                    "Worker",
                    SymbolKind::Struct,
                    "pub struct Worker",
                    1,
                    19,
                ),
                symbol(
                    "on_signal",
                    "Worker::on_signal",
                    SymbolKind::Function,
                    "pub fn on_signal(&self, number: i32)",
                    6,
                    18,
                ),
            ],
        );
        let blobs_source = concat!(
            "fn finalize(staging: &Path, expected: &str) -> Result<String, BlobError> {\n",
            "    let data = fs::read(staging)?;\n",
            "    let length = data.len();\n",
            "    let quota = self.quota_bytes;\n",
            "    let used = self.used_bytes;\n",
            "    let remaining = quota.saturating_sub(used);\n",
            "    let fits = length <= remaining;\n",
            "    let allowed = fits;\n",
            "    // Compare the checksum before the bytes are committed.\n",
            "    let actual = digest(&data);\n",
            "    if actual != expected {\n",
            "        return Err(BlobError::Integrity);\n",
            "    }\n",
            "    Ok(actual)\n",
            "}\n",
        );
        self.write_indexed_file(
            "src/blobs.rs",
            blobs_source,
            &[symbol(
                "finalize",
                "finalize",
                SymbolKind::Function,
                "fn finalize(staging: &Path, expected: &str) -> Result<String, BlobError>",
                1,
                15,
            )],
        );
        for (path, name, signature) in [
            (
                "src/server.rs",
                "handle_request",
                "fn handle_request(request: Request) -> Response",
            ),
            (
                "src/parts.rs",
                "upload_part",
                "fn upload_part(part: &[u8]) -> PartId",
            ),
            (
                "src/claims.rs",
                "verified_claims",
                "fn verified_claims(token: &str) -> Claims",
            ),
        ] {
            self.write_indexed_file(
                path,
                &format!("{signature} {{ todo!() }}\n"),
                &[symbol(name, name, SymbolKind::Function, signature, 1, 1)],
            );
        }
    }

    /// One file holding an enum variant, an incidental user, the raise site,
    /// a retry wrapper, and a test that all use `FileTooLarge`; the symbol
    /// named after the concept and the limit constant live in other files.
    fn seed_use_site_repository(&self) {
        self.write_indexed_file(
            "src/codes.rs",
            "pub fn file_too_large_code() -> &'static str {\n    \"file_too_large\"\n}\n",
            &[symbol(
                "file_too_large_code",
                "file_too_large_code",
                SymbolKind::Function,
                "pub fn file_too_large_code() -> &'static str",
                1,
                3,
            )],
        );
        self.write_indexed_file(
            "src/limits.rs",
            "pub const UTF8_CEILING_BYTES: u64 = 16 * 1024 * 1024;\npub const DEFAULT_MODE: u32 = 0o644;\n",
            &[
                symbol(
                    "UTF8_CEILING_BYTES",
                    "UTF8_CEILING_BYTES",
                    SymbolKind::Const,
                    "pub const UTF8_CEILING_BYTES: u64",
                    1,
                    1,
                ),
                symbol(
                    "DEFAULT_MODE",
                    "DEFAULT_MODE",
                    SymbolKind::Const,
                    "pub const DEFAULT_MODE: u32",
                    2,
                    2,
                ),
            ],
        );
        let queries_source = concat!(
            "pub enum QueryError {\n",
            "    InvalidFocus { focus: String },\n",
            "    FileTooLarge { path: PathBuf, limit_bytes: u64 },\n",
            "}\n",
            "\n",
            "fn describe_read_error(error: &QueryError) -> String {\n",
            "    match error {\n",
            "        QueryError::InvalidFocus { focus } => format!(\"focus {focus} is not a prefix\"),\n",
            "        QueryError::FileTooLarge { path, limit_bytes } => {\n",
            "            format!(\"{} is above the cap of {limit_bytes}\", path.display())\n",
            "        }\n",
            "    }\n",
            "}\n",
            "\n",
            "fn read_utf8_capped(path: &Path) -> Result<String, QueryError> {\n",
            "    let metadata = fs::metadata(path).map_err(QueryError::from)?;\n",
            "    let length = metadata.len();\n",
            "    let handle = File::open(path).map_err(QueryError::from)?;\n",
            "    let mut reader = BufReader::new(handle);\n",
            "    let mut buffer = String::new();\n",
            "    let capacity = usize::try_from(length).unwrap_or(0);\n",
            "    buffer.reserve(capacity);\n",
            "    let ceiling = UTF8_CEILING_BYTES;\n",
            "    let within = length <= ceiling;\n",
            "    if !within {\n",
            "        return Err(QueryError::FileTooLarge { path: path.to_owned(), limit_bytes: ceiling });\n",
            "    }\n",
            "    reader.read_to_string(&mut buffer).map_err(QueryError::from)?;\n",
            "    Ok(buffer)\n",
            "}\n",
            "\n",
            "fn read_or_empty(path: &Path) -> String {\n",
            "    match read_utf8_capped(path) {\n",
            "        Err(QueryError::FileTooLarge { .. }) => String::new(),\n",
            "        other => other.unwrap_or_default(),\n",
            "    }\n",
            "}\n",
            "\n",
            "fn test_capped_read_reports_file_too_large() {\n",
            "    assert!(matches!(read_utf8_capped(big()), Err(QueryError::FileTooLarge { .. })));\n",
            "}\n",
        );
        self.write_indexed_file(
            "src/queries.rs",
            queries_source,
            &[
                symbol(
                    "QueryError",
                    "QueryError",
                    SymbolKind::Enum,
                    "pub enum QueryError",
                    1,
                    4,
                ),
                symbol(
                    "describe_read_error",
                    "describe_read_error",
                    SymbolKind::Function,
                    "fn describe_read_error(error: &QueryError) -> String",
                    6,
                    13,
                ),
                symbol(
                    "read_utf8_capped",
                    "read_utf8_capped",
                    SymbolKind::Function,
                    "fn read_utf8_capped(path: &Path) -> Result<String, QueryError>",
                    15,
                    30,
                ),
                symbol(
                    "read_or_empty",
                    "read_or_empty",
                    SymbolKind::Function,
                    "fn read_or_empty(path: &Path) -> String",
                    32,
                    37,
                ),
                symbol(
                    "test_capped_read_reports_file_too_large",
                    "test_capped_read_reports_file_too_large",
                    SymbolKind::Function,
                    "fn test_capped_read_reports_file_too_large()",
                    39,
                    41,
                ),
            ],
        );
        self.write_indexed_file(
            "src/mode.rs",
            "fn file_mode(path: &Path) -> u32 {\n    DEFAULT_MODE\n}\nfn file_owner(path: &Path) -> u32 {\n    0\n}\nfn file_group(path: &Path) -> u32 {\n    0\n}\n",
            &[
                symbol("file_mode", "file_mode", SymbolKind::Function, "fn file_mode(path: &Path) -> u32", 1, 3),
                symbol("file_owner", "file_owner", SymbolKind::Function, "fn file_owner(path: &Path) -> u32", 4, 6),
                symbol("file_group", "file_group", SymbolKind::Function, "fn file_group(path: &Path) -> u32", 7, 9),
            ],
        );
        self.write_indexed_file(
            "src/variant.rs",
            "fn variant_name() -> &'static str {\n    \"unknown\"\n}\n",
            &[symbol(
                "variant_name",
                "variant_name",
                SymbolKind::Function,
                "fn variant_name() -> &'static str",
                1,
                3,
            )],
        );
    }

    /// Ten files whose symbols match the task only through the broad word
    /// `server`, which names more symbols than a specific query returns.
    fn seed_broad_word_tail(&self) {
        for index in 0..10 {
            let name = format!("server_route_{index}");
            let signature = format!("fn {name}() -> usize");
            self.write_indexed_file(
                &format!("src/mcp/route_{index}.rs"),
                &format!("{signature} {{\n    {index}\n}}\n"),
                &[symbol(&name, &name, SymbolKind::Function, &signature, 1, 3)],
            );
        }
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
