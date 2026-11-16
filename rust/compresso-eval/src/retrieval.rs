//! Retrieval evaluation: whether the context planner returns the files that
//! answer a task (06-fixtures.md §12, D84).

use std::collections::BTreeSet;
use std::fs;
use std::path::{Component, Path, PathBuf};

use compresso_atlas::{ContextPlan, Indexer, plan_context};
use compresso_core::{TokenProvider, counter_for};
use compresso_store::Store;
use serde::{Deserialize, Serialize};

use crate::fixtures::EvalError;
use crate::runner::{RunnerStage, TemporaryDirectory, copy_tree};

/// Top-level fixture directory holding the retrieval corpus.
pub const RETRIEVAL_DIRECTORY: &str = "retrieval";
const MANIFEST_FILE: &str = "tasks.toml";
const REPOSITORY_DIRECTORY: &str = "repo";
const MANIFEST_SCHEMA_VERSION: u32 = 1;

/// The committed retrieval corpus: one source tree and its gold task set.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RetrievalCorpus {
    /// The `retrieval/` directory containing `tasks.toml` and `repo/`.
    pub directory: PathBuf,
    pub budget_tokens: usize,
    pub tasks: Vec<RetrievalTask>,
}

/// One developer question with the files that must appear as evidence.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetrievalTask {
    pub id: String,
    pub task: String,
    /// Corpus-relative paths; empty for a concept the corpus does not contain.
    pub gold_files: Vec<String>,
    #[serde(default)]
    pub expect_ready: Option<bool>,
    #[serde(default)]
    pub known_gap: Option<KnownGap>,
}

/// A documented planner limitation that keeps a task out of the gated aggregate.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum KnownGap {
    /// The concept is written only inside a function body.
    BodyOnly,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RetrievalManifest {
    schema_version: u32,
    budget_tokens: usize,
    task: Vec<RetrievalTask>,
}

/// The `retrieval` section of the evaluation report.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct RetrievalReport {
    pub budget_tokens: usize,
    pub corpus_files: usize,
    /// Set when the corpus could not be evaluated; the gates then fail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<RunnerStage>,
    /// Gated tasks only: every task without a `known_gap`.
    pub aggregate: RetrievalAggregate,
    pub known_gaps: KnownGapAggregate,
    pub tasks: Vec<RetrievalTaskOutcome>,
}

/// Aggregate over the gated tasks. Recall and precision average the tasks
/// that have gold files; absent-concept tasks count toward `tasks`,
/// `false_ready`, `ready_mismatches`, and `mean_tokens` only.
#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
pub struct RetrievalAggregate {
    pub tasks: usize,
    pub gold_tasks: usize,
    pub full_recall_tasks: usize,
    pub mean_recall: f64,
    pub mean_precision: f64,
    pub false_ready: usize,
    pub ready_mismatches: usize,
    pub mean_tokens: f64,
}

/// Reported, ungated results of the `known_gap` tasks.
#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
pub struct KnownGapAggregate {
    pub tasks: usize,
    pub full_recall_tasks: usize,
    pub mean_recall: f64,
}

/// One task's measured result. No corpus content is emitted.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct RetrievalTaskOutcome {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub known_gap: Option<KnownGap>,
    pub gold_files: usize,
    pub gold_files_found: usize,
    pub missing_gold_files: Vec<String>,
    pub evidence_items: usize,
    /// Share of gold files present as evidence; zero without gold files.
    pub recall: f64,
    /// Share of evidence items whose path is a gold file; zero without evidence.
    pub precision: f64,
    pub tokens_used: usize,
    pub ready: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect_ready: Option<bool>,
    /// Ready while a gold file is missing, or ready on an absent concept.
    pub false_ready: bool,
}

impl RetrievalCorpus {
    /// Loads `<root>/retrieval/tasks.toml`. `None` means only that the
    /// directory does not exist; a present but invalid corpus is an error.
    pub fn load(root: &Path) -> Result<Option<Self>, EvalError> {
        let directory = root.join(RETRIEVAL_DIRECTORY);
        if !directory.is_dir() {
            return Ok(None);
        }
        let manifest_path = directory.join(MANIFEST_FILE);
        let source = fs::read_to_string(&manifest_path).map_err(|source| EvalError::Read {
            path: manifest_path.clone(),
            source,
        })?;
        let manifest =
            toml::from_str::<RetrievalManifest>(&source).map_err(|source| EvalError::Manifest {
                path: manifest_path.clone(),
                source: Box::new(source),
            })?;
        let invalid = |message: String| EvalError::InvalidFixture {
            path: manifest_path.clone(),
            message,
        };
        if manifest.schema_version != MANIFEST_SCHEMA_VERSION {
            return Err(invalid(format!(
                "unsupported retrieval manifest schema version {}",
                manifest.schema_version
            )));
        }
        if manifest.budget_tokens == 0 {
            return Err(invalid(
                "budget_tokens must be greater than zero".to_owned(),
            ));
        }
        let repository = directory.join(REPOSITORY_DIRECTORY);
        let mut ids = BTreeSet::new();
        for task in &manifest.task {
            validate_task(task, &repository).map_err(&invalid)?;
            if !ids.insert(task.id.as_str()) {
                return Err(invalid(format!("duplicate retrieval task id {}", task.id)));
            }
        }
        if !manifest
            .task
            .iter()
            .any(|task| task.known_gap.is_none() && !task.gold_files.is_empty())
        {
            return Err(invalid(
                "the retrieval manifest needs at least one gated task with gold files".to_owned(),
            ));
        }
        Ok(Some(Self {
            directory,
            budget_tokens: manifest.budget_tokens,
            tasks: manifest.task,
        }))
    }

    fn repository(&self) -> PathBuf {
        self.directory.join(REPOSITORY_DIRECTORY)
    }
}

fn validate_task(task: &RetrievalTask, repository: &Path) -> Result<(), String> {
    let id_valid = !task.id.is_empty()
        && task.id.split('-').all(|part| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|character| character.is_ascii_lowercase() || character.is_ascii_digit())
        });
    if !id_valid {
        return Err(format!("retrieval task id {:?} is not kebab-case", task.id));
    }
    if task.task.trim().is_empty() {
        return Err(format!("retrieval task {} has an empty task", task.id));
    }
    let mut seen = BTreeSet::new();
    for gold in &task.gold_files {
        let lexical = Path::new(gold)
            .components()
            .all(|component| matches!(component, Component::Normal(_)));
        if gold.is_empty() || gold.contains('\\') || !lexical {
            return Err(format!(
                "retrieval task {} gold file {gold:?} is not a corpus-relative path",
                task.id
            ));
        }
        if !repository.join(gold).is_file() {
            return Err(format!(
                "retrieval task {} gold file {gold} is not in the corpus",
                task.id
            ));
        }
        if !seen.insert(gold.as_str()) {
            return Err(format!(
                "retrieval task {} lists gold file {gold} twice",
                task.id
            ));
        }
    }
    if task.gold_files.is_empty() {
        if task.expect_ready != Some(false) {
            return Err(format!(
                "retrieval task {} has no gold files and must set expect_ready = false",
                task.id
            ));
        }
        if task.known_gap.is_some() {
            return Err(format!(
                "retrieval task {} has no gold files and cannot be a known gap",
                task.id
            ));
        }
    }
    Ok(())
}

/// Indexes the corpus into a temporary store and plans every task. A failure
/// is reported in the section, never dropped, so the gates fail closed.
pub fn evaluate_retrieval(corpus: &RetrievalCorpus) -> RetrievalReport {
    match run_tasks(corpus) {
        Ok((corpus_files, tasks)) => {
            RetrievalReport::from_outcomes(corpus.budget_tokens, corpus_files, tasks)
        }
        Err(stage) => RetrievalReport {
            budget_tokens: corpus.budget_tokens,
            corpus_files: 0,
            failure: Some(stage),
            aggregate: RetrievalAggregate::default(),
            known_gaps: KnownGapAggregate::default(),
            tasks: Vec::new(),
        },
    }
}

fn run_tasks(corpus: &RetrievalCorpus) -> Result<(usize, Vec<RetrievalTaskOutcome>), RunnerStage> {
    let temporary = TemporaryDirectory::new().map_err(|_| RunnerStage::PrepareRepository)?;
    let repository = temporary.path.join("repository");
    copy_tree(&corpus.repository(), &repository).map_err(|_| RunnerStage::CopyRepository)?;
    let store = Store::open(&temporary.path.join("eval.db")).map_err(|_| RunnerStage::OpenStore)?;
    let counter = counter_for(TokenProvider::OpenAiO200k);
    let index = Indexer::new(&store, counter)
        .index(&repository, |_| {})
        .map_err(|_| RunnerStage::IndexRepository)?;
    let repo = store
        .approve_repo(&repository)
        .map_err(|_| RunnerStage::LoadRepository)?;

    // A gold file the indexer skipped can never be evidence: that is a
    // corpus authoring error, not a ranking result.
    for task in &corpus.tasks {
        for gold in &task.gold_files {
            let symbols = store
                .symbols_in_file(repo, gold)
                .map_err(|_| RunnerStage::LoadRepository)?;
            if symbols.is_empty() {
                return Err(RunnerStage::ValidateFixture);
            }
        }
    }

    let mut outcomes = Vec::with_capacity(corpus.tasks.len());
    for task in &corpus.tasks {
        let plan = plan_context(&store, repo, &task.task, corpus.budget_tokens)
            .map_err(|_| RunnerStage::PlanContext)?;
        outcomes.push(task_outcome(task, &plan));
    }
    Ok((index.files_indexed, outcomes))
}

/// The readiness rule the product reports for a pack (04 §`compresso_context`).
fn plan_ready(plan: &ContextPlan) -> bool {
    plan.evidence_files_included >= plan.evidence_files_total
        && plan.evidence_files_total > 0
        && plan.concept_coverage_sufficient()
}

fn task_outcome(task: &RetrievalTask, plan: &ContextPlan) -> RetrievalTaskOutcome {
    let evidence_paths = plan
        .symbol_bodies
        .iter()
        .map(|body| body.path.as_str())
        .collect::<Vec<_>>();
    score_task(task, &evidence_paths, plan.tokens_used, plan_ready(plan))
}

/// Scores one task from the evidence paths the planner returned.
pub fn score_task(
    task: &RetrievalTask,
    evidence_paths: &[&str],
    tokens_used: usize,
    ready: bool,
) -> RetrievalTaskOutcome {
    let missing_gold_files = task
        .gold_files
        .iter()
        .filter(|gold| !evidence_paths.contains(&gold.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    let gold_files = task.gold_files.len();
    let gold_files_found = gold_files - missing_gold_files.len();
    let gold_evidence = evidence_paths
        .iter()
        .filter(|path| task.gold_files.iter().any(|gold| gold == *path))
        .count();
    let recall = ratio(gold_files_found, gold_files);
    RetrievalTaskOutcome {
        id: task.id.clone(),
        known_gap: task.known_gap,
        gold_files,
        gold_files_found,
        missing_gold_files,
        evidence_items: evidence_paths.len(),
        recall,
        precision: ratio(gold_evidence, evidence_paths.len()),
        tokens_used,
        ready,
        expect_ready: task.expect_ready,
        false_ready: ready && (gold_files == 0 || gold_files_found < gold_files),
    }
}

fn ratio(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        return 0.0;
    }
    numerator as f64 / denominator as f64
}

fn mean(values: impl Iterator<Item = f64>) -> f64 {
    let (total, count) = values.fold((0.0, 0_usize), |(total, count), value| {
        (total + value, count + 1)
    });
    if count == 0 {
        return 0.0;
    }
    total / count as f64
}

impl RetrievalReport {
    /// Builds the section from per-task outcomes, keeping `known_gap` tasks
    /// out of the gated aggregate.
    pub fn from_outcomes(
        budget_tokens: usize,
        corpus_files: usize,
        tasks: Vec<RetrievalTaskOutcome>,
    ) -> Self {
        let gated = tasks
            .iter()
            .filter(|task| task.known_gap.is_none())
            .collect::<Vec<_>>();
        let gold = gated
            .iter()
            .filter(|task| task.gold_files > 0)
            .collect::<Vec<_>>();
        let aggregate = RetrievalAggregate {
            tasks: gated.len(),
            gold_tasks: gold.len(),
            full_recall_tasks: gold
                .iter()
                .filter(|task| task.gold_files_found == task.gold_files)
                .count(),
            mean_recall: mean(gold.iter().map(|task| task.recall)),
            mean_precision: mean(gold.iter().map(|task| task.precision)),
            false_ready: gated.iter().filter(|task| task.false_ready).count(),
            ready_mismatches: gated
                .iter()
                .filter(|task| {
                    task.expect_ready
                        .is_some_and(|expected| expected != task.ready)
                })
                .count(),
            mean_tokens: mean(gated.iter().map(|task| task.tokens_used as f64)),
        };
        let gaps = tasks
            .iter()
            .filter(|task| task.known_gap.is_some())
            .collect::<Vec<_>>();
        let known_gaps = KnownGapAggregate {
            tasks: gaps.len(),
            full_recall_tasks: gaps
                .iter()
                .filter(|task| task.gold_files_found == task.gold_files)
                .count(),
            mean_recall: mean(gaps.iter().map(|task| task.recall)),
        };
        Self {
            budget_tokens,
            corpus_files,
            failure: None,
            aggregate,
            known_gaps,
            tasks,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(id: &str, gold_files: &[&str]) -> RetrievalTask {
        RetrievalTask {
            id: id.to_owned(),
            task: "How does it work?".to_owned(),
            gold_files: gold_files.iter().map(|gold| (*gold).to_owned()).collect(),
            expect_ready: gold_files.is_empty().then_some(false),
            known_gap: None,
        }
    }

    #[test]
    fn test_score_task_measures_recall_and_precision_by_path() {
        let outcome = score_task(
            &task("two-gold", &["a.rs", "b.rs"]),
            &["a.rs", "a.rs", "c.rs", "d.rs"],
            120,
            false,
        );

        assert_eq!(outcome.gold_files_found, 1);
        assert_eq!(outcome.missing_gold_files, ["b.rs"]);
        assert_eq!(outcome.recall, 0.5);
        assert_eq!(outcome.precision, 0.5);
        assert!(!outcome.false_ready);
    }

    #[test]
    fn test_score_task_without_evidence_has_zero_precision() {
        let outcome = score_task(&task("nothing", &["a.rs"]), &[], 0, false);

        assert_eq!(outcome.recall, 0.0);
        assert_eq!(outcome.precision, 0.0);
    }

    #[test]
    fn test_ready_with_missing_gold_file_is_false_ready() {
        let partial = score_task(&task("partial", &["a.rs", "b.rs"]), &["a.rs"], 10, true);
        let full = score_task(&task("full", &["a.rs"]), &["a.rs"], 10, true);

        assert!(partial.false_ready);
        assert!(!full.false_ready);
    }

    #[test]
    fn test_ready_on_absent_concept_is_false_ready() {
        let ready = score_task(&task("absent", &[]), &["c.rs"], 10, true);
        let withheld = score_task(&task("absent", &[]), &["c.rs"], 10, false);

        assert!(ready.false_ready);
        assert!(!withheld.false_ready);
        let report = RetrievalReport::from_outcomes(1600, 3, vec![ready]);
        assert_eq!(report.aggregate.false_ready, 1);
        assert_eq!(report.aggregate.ready_mismatches, 1);
    }

    #[test]
    fn test_known_gap_tasks_are_reported_outside_the_gated_aggregate() {
        let mut gap = task("gap", &["hidden.rs"]);
        gap.known_gap = Some(KnownGap::BodyOnly);
        let outcomes = vec![
            score_task(&task("found", &["a.rs"]), &["a.rs", "b.rs"], 100, true),
            score_task(&gap, &["b.rs"], 300, true),
            score_task(&task("absent", &[]), &[], 50, false),
        ];

        let report = RetrievalReport::from_outcomes(1600, 3, outcomes);

        assert_eq!(report.tasks.len(), 3);
        assert_eq!(report.aggregate.tasks, 2);
        assert_eq!(report.aggregate.gold_tasks, 1);
        assert_eq!(report.aggregate.full_recall_tasks, 1);
        assert_eq!(report.aggregate.mean_recall, 1.0);
        assert_eq!(report.aggregate.mean_precision, 0.5);
        assert_eq!(report.aggregate.false_ready, 0);
        assert_eq!(report.aggregate.mean_tokens, 75.0);
        assert_eq!(report.known_gaps.tasks, 1);
        assert_eq!(report.known_gaps.full_recall_tasks, 0);
        assert_eq!(report.known_gaps.mean_recall, 0.0);
    }
}
