use std::fs;
use std::path::{Path, PathBuf};

use compresso_eval::retrieval::evaluate_retrieval;
use compresso_eval::{EvalRunner, FixtureSet, Gates, KnownGap, RetrievalCorpus, RunnerStage};
use tempfile::TempDir;

fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures")
}

fn gates_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/plan/gates.toml")
}

fn committed_corpus() -> RetrievalCorpus {
    RetrievalCorpus::load(&fixtures_root())
        .expect("committed retrieval corpus should load")
        .expect("fixtures/retrieval should exist")
}

fn write_corpus(root: &Path, manifest: &str) {
    let repository = root.join("retrieval/repo");
    fs::create_dir_all(&repository).expect("repository directory");
    fs::write(
        repository.join("queue.rs"),
        "pub fn enqueue_job(id: u64) -> u64 {\n    id\n}\n",
    )
    .expect("source file");
    fs::write(root.join("retrieval/tasks.toml"), manifest).expect("manifest");
}

#[test]
fn test_committed_retrieval_manifest_shape() {
    let corpus = committed_corpus();

    assert_eq!(corpus.budget_tokens, 1600);
    assert_eq!(corpus.tasks.len(), 23);
    let gaps = corpus
        .tasks
        .iter()
        .filter(|task| task.known_gap == Some(KnownGap::BodyOnly))
        .count();
    let absent = corpus
        .tasks
        .iter()
        .filter(|task| task.gold_files.is_empty())
        .count();
    assert_eq!(gaps, 0, "D85 gates every body-only task");
    for body_only in [
        "graceful-shutdown",
        "upload-checksum",
        "clock-skew",
        "reconnect-jitter",
        "vacuum-statement",
    ] {
        assert!(
            corpus
                .tasks
                .iter()
                .any(|task| task.id == body_only && task.expect_ready == Some(true)),
            "{body_only} must be a gated body-only task"
        );
    }
    assert!(absent >= 2);
}

#[test]
fn test_committed_retrieval_corpus_passes_committed_gates() {
    let set = FixtureSet::load(&fixtures_root()).expect("committed fixtures should load");
    let report = EvalRunner.run(&FixtureSet {
        fixtures: Vec::new(),
        retrieval: set.retrieval,
    });
    let retrieval = report.retrieval.as_ref().expect("retrieval section");
    let gates = Gates::load(&gates_path()).expect("committed gates should load");

    assert_eq!(retrieval.failure, None);
    assert_eq!(retrieval.tasks.len(), 23);
    assert_eq!(retrieval.aggregate.tasks, 23);
    assert_eq!(retrieval.aggregate.gold_tasks, 20);
    assert_eq!(retrieval.known_gaps.tasks, 0);
    assert!(retrieval.aggregate.mean_recall >= gates.retrieval.mean_recall_min);
    assert!(retrieval.aggregate.mean_precision >= gates.retrieval.mean_precision_min);
    assert!(retrieval.aggregate.false_ready <= gates.retrieval.false_ready_max);
    assert!(retrieval.aggregate.ready_mismatches <= gates.retrieval.ready_mismatches_max);
    for task in &retrieval.tasks {
        assert!(task.tokens_used <= retrieval.budget_tokens, "{}", task.id);
        if task.gold_files == 0 {
            assert!(!task.ready, "absent concept {} was ready", task.id);
        }
    }
}

#[test]
fn test_retrieval_report_is_deterministic() {
    let corpus = committed_corpus();

    assert_eq!(evaluate_retrieval(&corpus), evaluate_retrieval(&corpus));
}

#[test]
fn test_retrieval_section_serializes_under_top_level_key() {
    let set = FixtureSet::load(&fixtures_root()).expect("committed fixtures should load");
    let report = EvalRunner.run(&FixtureSet {
        fixtures: Vec::new(),
        retrieval: set.retrieval,
    });
    let json = serde_json::to_value(&report).expect("report should serialize");

    assert_eq!(json["schema_version"], 1);
    for field in [
        "tasks",
        "gold_tasks",
        "full_recall_tasks",
        "mean_recall",
        "mean_precision",
        "false_ready",
        "ready_mismatches",
        "mean_tokens",
    ] {
        assert!(
            json["retrieval"]["aggregate"].get(field).is_some(),
            "retrieval aggregate is missing {field}"
        );
    }
    assert!(json["retrieval"]["tasks"].is_array());
    assert!(json["retrieval"]["known_gaps"].is_object());
}

#[test]
fn test_fixture_root_without_retrieval_directory_has_no_section() {
    let temporary = TempDir::new().expect("temporary directory");

    let set = FixtureSet::load(temporary.path()).expect("empty root should load");
    let report = EvalRunner.run(&set);

    assert!(set.retrieval.is_none());
    assert!(report.retrieval.is_none());
    let json = serde_json::to_value(&report).expect("report should serialize");
    assert!(json.get("retrieval").is_none());
}

#[test]
fn test_retrieval_directory_without_manifest_is_a_load_error() {
    let temporary = TempDir::new().expect("temporary directory");
    fs::create_dir_all(temporary.path().join("retrieval/repo")).expect("corpus directory");

    assert!(FixtureSet::load(temporary.path()).is_err());
}

#[test]
fn test_manifest_rejects_gold_file_outside_corpus_and_ready_absent_task() {
    for (manifest, expected) in [
        (
            "schema_version = 1\nbudget_tokens = 1600\n[[task]]\nid = \"missing\"\ntask = \"Where is it?\"\ngold_files = [\"nowhere.rs\"]\n",
            "is not in the corpus",
        ),
        (
            "schema_version = 1\nbudget_tokens = 1600\n[[task]]\nid = \"escape\"\ntask = \"Where is it?\"\ngold_files = [\"../tasks.toml\"]\n",
            "not a corpus-relative path",
        ),
        (
            "schema_version = 1\nbudget_tokens = 1600\n[[task]]\nid = \"found\"\ntask = \"Where is it?\"\ngold_files = [\"queue.rs\"]\n[[task]]\nid = \"absent\"\ntask = \"Where is it?\"\ngold_files = []\n",
            "must set expect_ready = false",
        ),
        (
            "schema_version = 1\nbudget_tokens = 1600\n[[task]]\nid = \"found\"\ntask = \"Where is it?\"\ngold_files = [\"queue.rs\"]\nsurprise = true\n",
            "unknown field",
        ),
        (
            "schema_version = 1\nbudget_tokens = 1600\n[[task]]\nid = \"gap\"\ntask = \"Where is it?\"\ngold_files = [\"queue.rs\"]\nknown_gap = \"body_only\"\n",
            "at least one gated task",
        ),
    ] {
        let temporary = TempDir::new().expect("temporary directory");
        write_corpus(temporary.path(), manifest);

        let error = RetrievalCorpus::load(temporary.path())
            .expect_err("invalid manifest should fail")
            .to_string();

        assert!(error.contains(expected), "{error}");
    }
}

#[test]
fn test_small_corpus_finds_named_symbol_and_withholds_absent_concept() {
    let temporary = TempDir::new().expect("temporary directory");
    write_corpus(
        temporary.path(),
        "schema_version = 1\nbudget_tokens = 1600\n[[task]]\nid = \"found\"\ntask = \"How do we enqueue a job?\"\ngold_files = [\"queue.rs\"]\n[[task]]\nid = \"absent\"\ntask = \"How does the autoscaler add replicas?\"\ngold_files = []\nexpect_ready = false\n",
    );
    let corpus = RetrievalCorpus::load(temporary.path())
        .expect("corpus should load")
        .expect("corpus should exist");

    let report = evaluate_retrieval(&corpus);

    assert_eq!(report.failure, None);
    assert_eq!(report.corpus_files, 1);
    assert_eq!(report.aggregate.full_recall_tasks, 1);
    assert_eq!(report.aggregate.mean_recall, 1.0);
    assert_eq!(report.aggregate.false_ready, 0);
}

#[test]
fn test_gold_file_without_indexed_symbols_fails_closed() {
    let temporary = TempDir::new().expect("temporary directory");
    write_corpus(
        temporary.path(),
        "schema_version = 1\nbudget_tokens = 1600\n[[task]]\nid = \"unindexed\"\ntask = \"Where is the note?\"\ngold_files = [\"notes.bin\"]\n",
    );
    fs::write(
        temporary.path().join("retrieval/repo/notes.bin"),
        [0_u8, 159, 146, 150],
    )
    .expect("binary file");
    let corpus = RetrievalCorpus::load(temporary.path())
        .expect("corpus should load")
        .expect("corpus should exist");

    let report = evaluate_retrieval(&corpus);
    let gates = Gates::load(&gates_path()).expect("committed gates should load");
    let evaluated = gates.evaluate(&EvalRunner.run(&FixtureSet {
        fixtures: Vec::new(),
        retrieval: Some(corpus),
    }));

    assert_eq!(report.failure, Some(RunnerStage::ValidateFixture));
    assert!(report.tasks.is_empty());
    assert!(!evaluated.passed);
}
