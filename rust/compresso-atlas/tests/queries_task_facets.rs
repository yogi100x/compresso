//! D95: a task that asks several things is ready only when every part is covered.
//!
//! These tests index the committed retrieval corpus, whose
//! `engine/src/report_cache.rs` mirrors a staleness check: a per-report and a
//! whole-catalog function share one comparison helper, constants and other
//! report functions in the same file compete for the file's share, and no line
//! of it says "cheap" or "large".

use std::fs;
use std::path::{Path, PathBuf};

use compresso_atlas::{ContextPlan, Indexer, plan_context};
use compresso_core::{TokenProvider, counter_for};
use compresso_store::Store;
use tempfile::TempDir;

const CACHE: &str = "engine/src/report_cache.rs";

const PROSE_TASK: &str = "In this repository, explain how the report cache decides that a cached report, or the whole catalog, is out of date compared with its source on disk, and how it keeps that check cheap for large sources. Cite source files and function names.";
const TERSE_TASK: &str = "staleness check: how the report cache decides a cached report or the catalog is stale versus disk; cheap check for large sources (length, mtime, digest)";

struct Corpus {
    _temp: TempDir,
    store: Store,
    repo: i64,
}

impl Corpus {
    fn index() -> Self {
        let temp = tempfile::tempdir().expect("temporary directory should be created");
        let root = temp.path().join("repo");
        let corpus = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/retrieval/repo");
        copy_tree(&corpus, &root);
        let store = Store::open(&temp.path().join("compresso.db")).expect("store should open");
        Indexer::new(&store, counter_for(TokenProvider::OpenAiO200k))
            .index(&root, |_| {})
            .expect("corpus should index");
        let repo = store.approve_repo(&root).expect("repo should approve");
        Self {
            _temp: temp,
            store,
            repo,
        }
    }

    fn plan(&self, task: &str, budget: usize) -> ContextPlan {
        plan_context(&self.store, self.repo, task, budget).expect("context should plan")
    }
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).expect("directory created");
    for entry in fs::read_dir(from).expect("corpus directory reads") {
        let entry = entry.expect("entry reads");
        let target: PathBuf = to.join(entry.file_name());
        if entry.file_type().expect("file type").is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).expect("file copied");
        }
    }
}

fn ready(plan: &ContextPlan) -> bool {
    plan.evidence_files_total > 0
        && plan.evidence_files_included >= plan.evidence_files_total
        && plan.concept_coverage_sufficient()
}

fn evidence_names(plan: &ContextPlan) -> Vec<String> {
    plan.symbol_bodies
        .iter()
        .filter(|body| body.path == CACHE)
        .map(|body| body.symbol.qualified_name.clone())
        .collect()
}

#[test]
fn test_plan_context_multi_part_task_without_evidence_for_one_part_is_not_ready() {
    let corpus = Corpus::index();
    for task in [PROSE_TASK, TERSE_TASK] {
        for budget in [1_600, 3_200] {
            let plan = corpus.plan(task, budget);
            assert!(
                !ready(&plan),
                "no excerpt covers the cheap-for-large-sources part, so the pack \
                 must not be ready at {budget}: {task}\n{:?}",
                evidence_names(&plan)
            );
        }
    }
}

#[test]
fn test_plan_context_multi_part_task_includes_the_helper_and_both_of_its_callers() {
    let corpus = Corpus::index();
    for task in [PROSE_TASK, TERSE_TASK] {
        let names = evidence_names(&corpus.plan(task, 1_600));
        for gold in ["catalog_stale", "report_current", "source_unchanged"] {
            assert!(
                names.iter().any(|name| name == gold),
                "{gold} is missing for {task}: {names:?}"
            );
        }
    }
}

#[test]
fn test_plan_context_names_the_uncovered_part_of_the_task() {
    let corpus = Corpus::index();
    let plan = corpus.plan(TERSE_TASK, 1_600);
    assert_eq!(plan.task_facets_total, 4, "{:?}", plan.uncovered_facets);
    assert_eq!(plan.task_facets_covered, 3);
    assert_eq!(plan.uncovered_facets, ["cheap check for large sources"]);
    assert!(!plan.concept_coverage_sufficient());
    assert!(!plan.evidence_ready());

    let prose = corpus.plan(PROSE_TASK, 1_600);
    assert_eq!(
        prose.uncovered_facets,
        ["how it keeps that check cheap for large sources"],
        "the trailing instruction to cite sources is not a part of the task"
    );
}

#[test]
fn test_plan_context_reads_out_of_date_as_stale() {
    let corpus = Corpus::index();
    let plan = corpus.plan(
        "How does the cache notice that the catalog is out of date?",
        1_600,
    );
    assert!(
        plan.search_queries.iter().any(|query| query == "stale"),
        "`out of date` is searched as the word code uses: {:?}",
        plan.search_queries
    );
    assert!(
        !plan
            .search_queries
            .iter()
            .any(|query| query == "date" || query == "out"),
        "the idiom's words are not concepts on their own: {:?}",
        plan.search_queries
    );
    let names = evidence_names(&plan);
    assert!(
        names.iter().any(|name| name == "catalog_stale"),
        "the catalog's stale check is evidence: {names:?}"
    );
}

#[test]
fn test_plan_context_one_part_tasks_keep_their_readiness() {
    let corpus = Corpus::index();
    for task in [
        "How is the retry backoff delay worked out between failed attempts?",
        "Where do we reject a job whose payload is too large, and what error does the caller get back?",
    ] {
        let plan = corpus.plan(task, 1_600);
        assert!(ready(&plan), "{task} should stay ready");
        assert_eq!(plan.task_facets_covered, plan.task_facets_total, "{task}");
    }
}
