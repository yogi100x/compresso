use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use compresso_core::Mode;
use compresso_eval::EvalReport;
use compresso_eval::campaign::cli::run as run_cli;
use compresso_eval::campaign::{
    AdapterError, AdapterKind, AttemptFailureKind, CampaignError, CampaignPolicy, CampaignReport,
    CampaignRunner, Checkpoint, CheckpointError, FixtureSuiteAdapter, StartMode, SuiteAdapter,
    SuiteDescriptor, TaskContext, TaskMeasurement, TaskSpec, TaskState, resolve_adapter,
};
use serde_json::{Value, json};
use tempfile::TempDir;

fn smoke_suite() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/campaign-smoke")
}

fn smoke_adapter() -> Arc<FixtureSuiteAdapter> {
    Arc::new(FixtureSuiteAdapter::load(&smoke_suite()).expect("smoke suite should load"))
}

fn policy(concurrency: usize, max_attempts: u32) -> CampaignPolicy {
    CampaignPolicy {
        concurrency,
        max_attempts,
        task_timeout: Duration::from_secs(30),
    }
}

fn states(checkpoint: &Checkpoint) -> BTreeMap<&str, (TaskState, u32)> {
    checkpoint
        .tasks
        .iter()
        .map(|task| (task.id.as_str(), (task.state, task.attempts)))
        .collect()
}

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).expect("file should be readable"))
        .expect("file should hold JSON")
}

fn write_json(path: &Path, value: &Value) {
    fs::write(
        path,
        serde_json::to_vec_pretty(value).expect("value should encode"),
    )
    .expect("file should be writable");
}

/// Wraps an adapter and requests a campaign stop after `limit` attempts.
struct StopAfter {
    inner: Arc<FixtureSuiteAdapter>,
    limit: usize,
    seen: AtomicUsize,
    stop: Mutex<Option<Arc<AtomicBool>>>,
}

impl SuiteAdapter for StopAfter {
    fn descriptor(&self) -> SuiteDescriptor {
        self.inner.descriptor()
    }

    fn tasks(&self) -> Result<Vec<TaskSpec>, AdapterError> {
        self.inner.tasks()
    }

    fn run_task(
        &self,
        task: &TaskSpec,
        context: &TaskContext,
    ) -> Result<TaskMeasurement, AdapterError> {
        let result = self.inner.run_task(task, context);
        if self.seen.fetch_add(1, Ordering::SeqCst) + 1 >= self.limit
            && let Some(stop) = self.stop.lock().expect("stop lock").as_ref()
        {
            stop.store(true, Ordering::SeqCst);
        }
        result
    }
}

/// A synthetic suite whose behavior is supplied by the test.
struct ScriptedAdapter<F> {
    ids: Vec<String>,
    behavior: F,
}

impl<F> SuiteAdapter for ScriptedAdapter<F>
where
    F: Fn(&TaskSpec, &TaskContext) -> Result<TaskMeasurement, AdapterError> + Send + Sync + 'static,
{
    fn descriptor(&self) -> SuiteDescriptor {
        SuiteDescriptor {
            id: "scripted".to_owned(),
            version: "1".to_owned(),
            mode: Mode::Balanced,
        }
    }

    fn tasks(&self) -> Result<Vec<TaskSpec>, AdapterError> {
        Ok(self
            .ids
            .iter()
            .map(|id| TaskSpec { id: id.clone() })
            .collect())
    }

    fn run_task(
        &self,
        task: &TaskSpec,
        context: &TaskContext,
    ) -> Result<TaskMeasurement, AdapterError> {
        (self.behavior)(task, context)
    }
}

fn resolved() -> TaskMeasurement {
    TaskMeasurement {
        resolved: true,
        baseline_tokens: 100,
        compressed_tokens: 60,
    }
}

#[test]
fn test_campaign_drives_every_task_to_a_terminal_state_with_bounded_retries() {
    let directory = TempDir::new().expect("temporary directory");
    let checkpoint_path = directory.path().join("campaign.json");
    let adapter = smoke_adapter();
    let runner = CampaignRunner::new(adapter.clone(), policy(1, 2), &checkpoint_path)
        .expect("policy should be valid");

    let outcome = runner.run(StartMode::Fresh).expect("campaign should run");

    assert!(!outcome.interrupted);
    assert!(outcome.checkpoint.is_complete());
    assert_eq!(
        states(&outcome.checkpoint),
        BTreeMap::from([
            ("task-01-resolved", (TaskState::Passed, 1)),
            ("task-02-unresolved", (TaskState::Failed, 1)),
            ("task-03-flaky", (TaskState::Passed, 2)),
            ("task-04-broken", (TaskState::Exhausted, 2)),
            ("task-05-resolved", (TaskState::Passed, 1)),
        ])
    );
    let broken = &outcome.checkpoint.tasks[3];
    let failure = broken
        .last_failure
        .as_ref()
        .expect("failure should be recorded");
    assert_eq!(failure.kind, AttemptFailureKind::Error);
    assert_eq!(failure.attempt, 2);
    assert!(broken.measurement.is_none());
    assert_eq!(adapter.invocations().values().sum::<u32>(), 7);

    let persisted = Checkpoint::load(&checkpoint_path).expect("checkpoint should load");
    assert_eq!(persisted, outcome.checkpoint);
    assert!(
        !directory.path().join("campaign.json.tmp").exists(),
        "atomic write must not leave its temporary file behind"
    );
}

#[test]
fn test_campaign_resume_does_not_rerun_completed_tasks() {
    let directory = TempDir::new().expect("temporary directory");
    let checkpoint_path = directory.path().join("campaign.json");

    let first_inner = smoke_adapter();
    let first = Arc::new(StopAfter {
        inner: first_inner.clone(),
        limit: 2,
        seen: AtomicUsize::new(0),
        stop: Mutex::new(None),
    });
    let runner = CampaignRunner::new(first.clone(), policy(1, 2), &checkpoint_path)
        .expect("policy should be valid");
    *first.stop.lock().expect("stop lock") = Some(runner.stop_handle());

    let interrupted = runner
        .run(StartMode::Fresh)
        .expect("interrupted run should report");

    assert!(interrupted.interrupted);
    assert_eq!(
        first_inner.invocations(),
        BTreeMap::from([
            ("task-01-resolved".to_owned(), 1),
            ("task-02-unresolved".to_owned(), 1),
        ])
    );
    assert_eq!(
        states(&interrupted.checkpoint)["task-03-flaky"],
        (TaskState::Pending, 0)
    );

    let second = smoke_adapter();
    let resumed = CampaignRunner::new(second.clone(), policy(2, 2), &checkpoint_path)
        .expect("policy should be valid")
        .run(StartMode::Resume)
        .expect("resume should complete");

    assert!(!resumed.interrupted);
    assert!(resumed.recovered.is_empty());
    assert!(resumed.checkpoint.is_complete());
    assert_eq!(
        second.invocations(),
        BTreeMap::from([
            ("task-03-flaky".to_owned(), 2),
            ("task-04-broken".to_owned(), 2),
            ("task-05-resolved".to_owned(), 1),
        ]),
        "tasks finished before the interruption must not be executed again"
    );
    assert_eq!(
        states(&resumed.checkpoint)["task-01-resolved"],
        (TaskState::Passed, 1)
    );
}

#[test]
fn test_campaign_resume_counts_attempt_interrupted_by_crash() {
    let directory = TempDir::new().expect("temporary directory");
    let exhausted_path = directory.path().join("exhausted.json");
    let retried_path = directory.path().join("retried.json");

    for path in [&exhausted_path, &retried_path] {
        let runner = CampaignRunner::new(smoke_adapter(), policy(1, 2), path)
            .expect("policy should be valid");
        runner.stop_handle().store(true, Ordering::SeqCst);
        let outcome = runner.run(StartMode::Fresh).expect("stopped run");
        assert!(outcome.interrupted);

        // Simulate a process that died after persisting `running`.
        let mut checkpoint = read_json(path);
        checkpoint["tasks"][0]["state"] = json!("running");
        checkpoint["tasks"][0]["attempts"] = json!(1);
        write_json(path, &checkpoint);
    }

    let single_attempt = smoke_adapter();
    let exhausted = CampaignRunner::new(single_attempt.clone(), policy(1, 1), &exhausted_path)
        .expect("policy should be valid")
        .run(StartMode::Resume)
        .expect("resume should complete");
    assert_eq!(exhausted.recovered, ["task-01-resolved"]);
    assert_eq!(
        states(&exhausted.checkpoint)["task-01-resolved"],
        (TaskState::Exhausted, 1)
    );
    assert_eq!(
        exhausted.checkpoint.tasks[0]
            .last_failure
            .as_ref()
            .map(|failure| failure.kind),
        Some(AttemptFailureKind::Interrupted)
    );
    assert!(
        !single_attempt
            .invocations()
            .contains_key("task-01-resolved"),
        "a crash-looping task must not be retried beyond its attempt budget"
    );

    let two_attempts = smoke_adapter();
    let retried = CampaignRunner::new(two_attempts.clone(), policy(1, 2), &retried_path)
        .expect("policy should be valid")
        .run(StartMode::Resume)
        .expect("resume should complete");
    assert_eq!(
        states(&retried.checkpoint)["task-01-resolved"],
        (TaskState::Passed, 2)
    );
    assert_eq!(two_attempts.invocations()["task-01-resolved"], 1);
}

#[test]
fn test_campaign_resume_applies_attempt_budget_to_pending_retries() {
    let directory = TempDir::new().expect("temporary directory");
    let checkpoint_path = directory.path().join("campaign.json");
    let runner = CampaignRunner::new(smoke_adapter(), policy(1, 3), &checkpoint_path)
        .expect("policy should be valid");
    runner.stop_handle().store(true, Ordering::SeqCst);
    runner.run(StartMode::Fresh).expect("stopped run");

    // A task that already spent two attempts and was waiting for a third.
    let mut checkpoint = read_json(&checkpoint_path);
    checkpoint["tasks"][0]["attempts"] = json!(2);
    write_json(&checkpoint_path, &checkpoint);

    let adapter = smoke_adapter();
    let resumed = CampaignRunner::new(adapter.clone(), policy(1, 2), &checkpoint_path)
        .expect("policy should be valid")
        .run(StartMode::Resume)
        .expect("resume should complete");

    assert!(resumed.checkpoint.is_complete());
    assert_eq!(
        states(&resumed.checkpoint)["task-01-resolved"],
        (TaskState::Exhausted, 2)
    );
    assert!(
        !adapter.invocations().contains_key("task-01-resolved"),
        "a lowered attempt budget must not grant an attempt past the bound"
    );
}

#[test]
fn test_campaign_start_mode_guards_checkpoint_existence() {
    let directory = TempDir::new().expect("temporary directory");
    let checkpoint_path = directory.path().join("campaign.json");
    let runner = CampaignRunner::new(smoke_adapter(), policy(1, 1), &checkpoint_path)
        .expect("policy should be valid");

    assert!(matches!(
        runner.run(StartMode::Resume),
        Err(CampaignError::CheckpointMissing { .. })
    ));
    runner.run(StartMode::Fresh).expect("fresh run");
    let before = fs::read(&checkpoint_path).expect("checkpoint bytes");
    assert!(matches!(
        runner.run(StartMode::Fresh),
        Err(CampaignError::CheckpointExists { .. })
    ));
    assert_eq!(
        fs::read(&checkpoint_path).expect("checkpoint bytes"),
        before,
        "a refused fresh run must not touch the existing checkpoint"
    );
}

#[test]
fn test_campaign_resume_rejects_corrupt_or_mismatched_checkpoint() {
    let directory = TempDir::new().expect("temporary directory");
    let checkpoint_path = directory.path().join("campaign.json");
    let runner = CampaignRunner::new(smoke_adapter(), policy(1, 2), &checkpoint_path)
        .expect("policy should be valid");
    runner.run(StartMode::Fresh).expect("fresh run");
    let valid = read_json(&checkpoint_path);

    let resume = || match runner.run(StartMode::Resume) {
        Err(CampaignError::Checkpoint(error)) => error,
        other => panic!("resume should be refused, got {other:?}"),
    };

    fs::write(&checkpoint_path, b"{\"schema_version\": 1, \"tasks\": [")
        .expect("truncated checkpoint");
    assert!(matches!(resume(), CheckpointError::Malformed { .. }));

    let mut future = valid.clone();
    future["schema_version"] = json!(2);
    write_json(&checkpoint_path, &future);
    assert!(matches!(
        resume(),
        CheckpointError::UnsupportedSchemaVersion { found: 2, .. }
    ));

    let mut other_suite = valid.clone();
    other_suite["suite"]["version"] = json!("1999.01");
    write_json(&checkpoint_path, &other_suite);
    assert!(matches!(resume(), CheckpointError::SuiteMismatch { .. }));

    let mut other_tasks = valid.clone();
    other_tasks["tasks"][4]["id"] = json!("task-99-unknown");
    write_json(&checkpoint_path, &other_tasks);
    assert!(matches!(
        resume(),
        CheckpointError::TaskSetMismatch { task } if task == "task-05-resolved" || task == "task-99-unknown"
    ));

    let mut inconsistent = valid.clone();
    inconsistent["tasks"][0]["state"] = json!("failed");
    write_json(&checkpoint_path, &inconsistent);
    assert!(matches!(resume(), CheckpointError::Malformed { .. }));

    write_json(&checkpoint_path, &valid);
    let outcome = runner
        .run(StartMode::Resume)
        .expect("valid checkpoint resumes");
    assert!(outcome.checkpoint.is_complete());
}

#[test]
fn test_campaign_task_timeout_cancels_attempt_and_exhausts_retries() {
    let directory = TempDir::new().expect("temporary directory");
    let adapter = Arc::new(
        FixtureSuiteAdapter::from_json(
            r#"{
                "schema_version": 1,
                "suite": "slow",
                "version": "1",
                "tasks": [
                    {"id": "fast", "resolved": true, "baseline_tokens": 10, "compressed_tokens": 5},
                    {"id": "hung", "resolved": true, "baseline_tokens": 10, "compressed_tokens": 5,
                     "delay_ms": 600000}
                ]
            }"#,
        )
        .expect("suite should parse"),
    );
    let runner = CampaignRunner::new(
        adapter.clone(),
        CampaignPolicy {
            concurrency: 2,
            max_attempts: 2,
            // Generous for the fast task on a loaded runner, tiny next to the
            // ten-minute scripted delay of the hung one.
            task_timeout: Duration::from_secs(2),
        },
        directory.path().join("campaign.json"),
    )
    .expect("policy should be valid");

    let started = Instant::now();
    let outcome = runner.run(StartMode::Fresh).expect("campaign should run");

    assert!(
        started.elapsed() < Duration::from_secs(60),
        "the runner must not wait for a hung task"
    );
    assert_eq!(
        states(&outcome.checkpoint),
        BTreeMap::from([
            ("fast", (TaskState::Passed, 1)),
            ("hung", (TaskState::Exhausted, 2)),
        ])
    );
    assert_eq!(
        outcome.checkpoint.tasks[1]
            .last_failure
            .as_ref()
            .map(|failure| failure.kind),
        Some(AttemptFailureKind::Timeout)
    );
    assert_eq!(adapter.invocations()["hung"], 2);
}

#[test]
fn test_campaign_never_exceeds_concurrency_limit() {
    const LIMIT: usize = 3;
    let directory = TempDir::new().expect("temporary directory");
    let in_flight = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let (flight, high) = (in_flight.clone(), peak.clone());
    let adapter = Arc::new(ScriptedAdapter {
        ids: (0..9).map(|index| format!("task-{index}")).collect(),
        behavior: move |_: &TaskSpec, _: &TaskContext| {
            let now = flight.fetch_add(1, Ordering::SeqCst) + 1;
            high.fetch_max(now, Ordering::SeqCst);
            // Hold the slot until the limit has been reached once, so the
            // peak is deterministic rather than timing-dependent.
            let deadline = Instant::now() + Duration::from_secs(20);
            while high.load(Ordering::SeqCst) < LIMIT && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(2));
            }
            flight.fetch_sub(1, Ordering::SeqCst);
            Ok(resolved())
        },
    });

    let outcome = CampaignRunner::new(
        adapter,
        policy(LIMIT, 1),
        directory.path().join("campaign.json"),
    )
    .expect("policy should be valid")
    .run(StartMode::Fresh)
    .expect("campaign should run");

    assert!(outcome.checkpoint.is_complete());
    assert_eq!(peak.load(Ordering::SeqCst), LIMIT);
    assert_eq!(in_flight.load(Ordering::SeqCst), 0);
}

#[test]
fn test_campaign_persists_running_state_before_each_attempt() {
    let directory = TempDir::new().expect("temporary directory");
    let checkpoint_path = directory.path().join("campaign.json");
    let observed_path = checkpoint_path.clone();
    let observations = Arc::new(Mutex::new(Vec::new()));
    let sink = observations.clone();
    let adapter = Arc::new(ScriptedAdapter {
        ids: vec!["alpha".to_owned(), "beta".to_owned()],
        behavior: move |task: &TaskSpec, context: &TaskContext| {
            let durable = Checkpoint::load(&observed_path).expect("checkpoint is always loadable");
            let record = durable
                .tasks
                .iter()
                .find(|record| record.id == task.id)
                .expect("task is recorded")
                .clone();
            sink.lock()
                .expect("sink lock")
                .push((record.state, record.attempts, context.attempt));
            if context.attempt == 1 {
                panic!("first attempt panics");
            }
            Ok(resolved())
        },
    });

    let outcome = CampaignRunner::new(adapter, policy(1, 2), &checkpoint_path)
        .expect("policy should be valid")
        .run(StartMode::Fresh)
        .expect("a panicking adapter must not abort the campaign");

    assert_eq!(
        states(&outcome.checkpoint),
        BTreeMap::from([
            ("alpha", (TaskState::Passed, 2)),
            ("beta", (TaskState::Passed, 2)),
        ])
    );
    assert_eq!(
        outcome.checkpoint.tasks[0]
            .last_failure
            .as_ref()
            .map(|failure| failure.kind),
        Some(AttemptFailureKind::Panic)
    );
    let observations = observations.lock().expect("sink lock");
    assert_eq!(observations.len(), 4);
    for (state, attempts, attempt) in observations.iter() {
        assert_eq!(*state, TaskState::Running);
        assert_eq!(attempts, attempt);
    }
}

#[test]
fn test_campaign_rejects_invalid_policy_and_task_lists() {
    let directory = TempDir::new().expect("temporary directory");
    let path = directory.path().join("campaign.json");
    for invalid in [
        CampaignPolicy {
            concurrency: 0,
            ..CampaignPolicy::default()
        },
        CampaignPolicy {
            max_attempts: 0,
            ..CampaignPolicy::default()
        },
        CampaignPolicy {
            task_timeout: Duration::ZERO,
            ..CampaignPolicy::default()
        },
    ] {
        assert!(matches!(
            CampaignRunner::new(smoke_adapter(), invalid, &path),
            Err(CampaignError::InvalidPolicy { .. })
        ));
    }

    let duplicate = Arc::new(ScriptedAdapter {
        ids: vec!["same".to_owned(), "same".to_owned()],
        behavior: |_: &TaskSpec, _: &TaskContext| Ok(resolved()),
    });
    let error = CampaignRunner::new(duplicate, policy(1, 1), &path)
        .expect("policy should be valid")
        .run(StartMode::Fresh)
        .expect_err("duplicate identifiers are refused");
    assert!(matches!(
        error,
        CampaignError::Checkpoint(CheckpointError::InvalidTaskList { .. })
    ));
    assert!(!path.exists());
}

fn stable_report(checkpoint: &Checkpoint) -> Value {
    let mut report = serde_json::to_value(CampaignReport::from_checkpoint(checkpoint))
        .expect("report should encode");
    for fixture in report["fixtures"].as_array_mut().expect("fixtures array") {
        fixture["duration_ms"] = json!(0);
    }
    report["aggregate"]["p95_latency_ms"] = json!(0);
    report
}

#[test]
fn test_campaign_report_uses_eval_report_shape_and_ignores_completion_order() {
    let directory = TempDir::new().expect("temporary directory");
    let run = |name: &str, concurrency: usize| {
        CampaignRunner::new(
            smoke_adapter(),
            policy(concurrency, 2),
            directory.path().join(name),
        )
        .expect("policy should be valid")
        .run(StartMode::Fresh)
        .expect("campaign should run")
        .checkpoint
    };
    let serial = stable_report(&run("serial.json", 1));
    let parallel = stable_report(&run("parallel.json", 4));
    assert_eq!(serial, parallel);

    let mut plain = serial.clone();
    let campaign = plain
        .as_object_mut()
        .expect("report object")
        .remove("campaign")
        .expect("campaign section");
    let report: EvalReport =
        serde_json::from_value(plain).expect("remaining keys are exactly an evaluation report");

    assert_eq!(report.schema_version, 1);
    assert_eq!(
        report
            .fixtures
            .iter()
            .map(|fixture| fixture.id.as_str())
            .collect::<Vec<_>>(),
        [
            "campaign-smoke/task-01-resolved",
            "campaign-smoke/task-02-unresolved",
            "campaign-smoke/task-03-flaky",
            "campaign-smoke/task-04-broken",
            "campaign-smoke/task-05-resolved",
        ]
    );
    assert_eq!(serial["fixtures"][0]["kind"], "campaign");
    assert_eq!(serial["fixtures"][0]["reduction_pct"], 50.0);
    assert_eq!(
        serial["fixtures"][3]["failures"],
        json!([{"kind": "campaign_task", "state": "exhausted"}])
    );
    assert_eq!(report.aggregate.compression_fixtures, 5);
    assert_eq!(report.aggregate.eligible_fixtures, 4);
    // Reductions 50%, 25%, 30%, 75% -> median of the middle pair.
    assert!((report.aggregate.median_reduction_eligible - 0.40).abs() < 1e-12);
    assert!(report.recall.fixtures.is_empty());

    assert_eq!(campaign["suite"], "campaign-smoke");
    assert_eq!(campaign["complete"], true);
    assert_eq!(campaign["tasks_total"], 5);
    assert_eq!(campaign["passed"], 3);
    assert_eq!(campaign["failed"], 1);
    assert_eq!(campaign["exhausted"], 1);
    assert_eq!(campaign["resolved_rate"], 0.6);
    assert_eq!(campaign["attempts_total"], 7);
    assert_eq!(campaign["retried_tasks"], 2);
}

#[test]
fn test_external_adapters_require_opt_in_and_are_not_built_in() {
    for kind in [AdapterKind::SweBenchVerified, AdapterKind::TerminalBench] {
        assert!(kind.is_external());
        assert!(matches!(
            resolve_adapter(kind, &smoke_suite(), false),
            Err(CampaignError::ExternalAdapterNotAllowed { adapter }) if adapter == kind.name()
        ));
        assert!(matches!(
            resolve_adapter(kind, &smoke_suite(), true),
            Err(CampaignError::ExternalAdapterUnavailable { adapter }) if adapter == kind.name()
        ));
    }
    assert!(!AdapterKind::Fixture.is_external());
    assert!(resolve_adapter(AdapterKind::Fixture, &smoke_suite(), false).is_ok());
}

fn cli(arguments: &[&str]) -> (u8, String, String) {
    let mut output = Vec::new();
    let mut errors = Vec::new();
    let code = run_cli(
        std::iter::once("compresso-eval-campaign")
            .chain(arguments.iter().copied())
            .map(Into::into),
        &mut output,
        &mut errors,
    );
    (
        code.value(),
        String::from_utf8(output).expect("stdout is UTF-8"),
        String::from_utf8(errors).expect("stderr is UTF-8"),
    )
}

#[test]
fn test_campaign_cli_runs_resumes_and_reports_status_as_json() {
    let directory = TempDir::new().expect("temporary directory");
    let checkpoint = directory.path().join("campaign.json");
    let checkpoint = checkpoint.to_str().expect("UTF-8 path");
    let suite = smoke_suite();
    let suite = suite.to_str().expect("UTF-8 path");

    let (code, output, errors) = cli(&[
        "run",
        "--suite",
        suite,
        "--checkpoint",
        checkpoint,
        "--concurrency",
        "2",
        "--json",
    ]);
    assert_eq!((code, errors.as_str()), (0, ""));
    let report: Value = serde_json::from_str(&output).expect("run prints one JSON report");
    assert_eq!(report["campaign"]["complete"], true);
    assert_eq!(report["fixtures"].as_array().map(Vec::len), Some(5));

    let (code, _, errors) = cli(&[
        "run",
        "--suite",
        suite,
        "--checkpoint",
        checkpoint,
        "--json",
    ]);
    assert_eq!(code, 1);
    let error: Value = serde_json::from_str(&errors).expect("errors are JSON");
    assert_eq!(error["error"]["code"], "checkpoint_exists");

    let (code, resumed, _) = cli(&[
        "run",
        "--suite",
        suite,
        "--checkpoint",
        checkpoint,
        "--resume",
        "--json",
    ]);
    assert_eq!(code, 0);
    let (code, status, _) = cli(&["status", "--checkpoint", checkpoint, "--json"]);
    assert_eq!(code, 0);
    assert_eq!(resumed, status);
    assert_eq!(
        serde_json::from_str::<Value>(&status).expect("status JSON")["campaign"],
        report["campaign"]
    );
}

#[test]
fn test_campaign_cli_refuses_external_adapter_without_opt_in() {
    let directory = TempDir::new().expect("temporary directory");
    let checkpoint = directory.path().join("campaign.json");
    let base = [
        "run",
        "--suite",
        "unused",
        "--checkpoint",
        checkpoint.to_str().expect("UTF-8 path"),
        "--adapter",
        "swe-bench-verified",
        "--json",
    ];

    let (code, output, errors) = cli(&base);
    assert_eq!((code, output.as_str()), (1, ""));
    let error: Value = serde_json::from_str(&errors).expect("errors are JSON");
    assert_eq!(error["error"]["code"], "external_adapter_not_allowed");

    let mut allowed = base.to_vec();
    allowed.push("--allow-external");
    let (code, _, errors) = cli(&allowed);
    assert_eq!(code, 1);
    let error: Value = serde_json::from_str(&errors).expect("errors are JSON");
    assert_eq!(error["error"]["code"], "external_adapter_unavailable");
    assert!(!checkpoint.exists());

    let (code, _, _) = cli(&["run", "--suite", "unused"]);
    assert_eq!(code, 2, "missing required arguments are a usage error");
}
