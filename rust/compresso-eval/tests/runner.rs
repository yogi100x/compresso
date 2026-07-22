use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use compresso_eval::{EvalReport, EvalRunner, FixtureSet};

static FIXTURES: OnceLock<FixtureSet> = OnceLock::new();
static REPORT: OnceLock<EvalReport> = OnceLock::new();
static RUNNER_LOCK: Mutex<()> = Mutex::new(());

fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures")
}

fn committed_fixtures() -> &'static FixtureSet {
    FIXTURES
        .get_or_init(|| FixtureSet::load(&fixtures_root()).expect("committed fixtures should load"))
}

fn committed_report() -> &'static EvalReport {
    REPORT.get_or_init(|| {
        let _guard = RUNNER_LOCK
            .lock()
            .expect("runner lock should not be poisoned");
        EvalRunner.run(committed_fixtures())
    })
}

#[test]
fn test_runner_full_corpus_report_shape() {
    let set = committed_fixtures();
    let report = committed_report();
    let encoded = serde_json::to_string(&report).expect("report should serialize as JSON");
    let json: serde_json::Value =
        serde_json::from_str(&encoded).expect("serialized report should parse as JSON");

    assert!(
        json.get("fixtures")
            .is_some_and(serde_json::Value::is_array)
    );
    assert!(
        json.get("aggregate")
            .is_some_and(serde_json::Value::is_object)
    );
    for row in json["fixtures"]
        .as_array()
        .expect("fixtures should be an array")
    {
        for field in [
            "id",
            "mode",
            "before_tokens",
            "after_tokens",
            "duration_ms",
            "passed",
        ] {
            assert!(row.get(field).is_some(), "fixture row is missing {field}");
        }
    }
    for field in [
        "median_reduction_eligible",
        "p95_latency_ms",
        "protected_mutations",
        "malformed_json",
        "fallback_rate",
    ] {
        assert!(
            json["aggregate"].get(field).is_some(),
            "aggregate is missing {field}"
        );
    }
    assert_eq!(report.fixtures.len(), set.fixtures.len());
    for fixture in &set.fixtures {
        assert!(report.fixtures.iter().any(|row| row.id == fixture.name));
    }
}

#[test]
fn test_log_fixture_expectations() {
    let report = committed_report();
    let row = report
        .fixtures
        .iter()
        .find(|row| row.id == "repeated-npm-install")
        .expect("repeated log row should be present");

    assert!(
        row.after_tokens < row.before_tokens,
        "repeated log fixture did not shrink: {} -> {} tokens in {} ms; fallback={:?}",
        row.before_tokens,
        row.after_tokens,
        row.duration_ms,
        row.fallback_reason,
    );
    let output = row.output.as_ref().expect("compression rows expose output");
    assert!(
        output
            .nodes
            .iter()
            .any(|node| node.content.contains("[×500"))
    );
}

#[test]
fn test_eval_measurement_is_not_censored_by_the_engine_deadline() {
    let mut fixture = committed_fixtures()
        .fixtures
        .iter()
        .find(|fixture| fixture.name == "repeated-npm-install")
        .expect("repeated log fixture should exist")
        .clone();
    // Zero would be an invalid production deadline. After fixture loading it
    // is a deterministic sentinel proving the measurement runner replaces
    // only the engine's wall-clock cutoff with its external P95 gate.
    fixture.budget.max_latency_ms = 0;
    let report = {
        let _guard = RUNNER_LOCK
            .lock()
            .expect("runner lock should not be poisoned");
        EvalRunner.run(&FixtureSet {
            fixtures: vec![fixture],
        })
    };
    let row = &report.fixtures[0];

    assert!(!row.fallback_used, "fallback={:?}", row.fallback_reason);
    assert!(row.after_tokens < row.before_tokens);
    assert!(row.output.as_ref().is_some_and(|output| {
        output
            .nodes
            .iter()
            .any(|node| node.content.contains("[×500"))
    }));
}

#[test]
fn test_stack_trace_fixture_meets_floor_without_fallback() {
    let row = committed_report()
        .fixtures
        .iter()
        .find(|row| row.id == "stack-trace-dupes")
        .expect("stack trace fixture should be present");

    assert!(!row.fallback_used, "fallback={:?}", row.fallback_reason);
    assert!(
        row.reduction_pct >= 60.0,
        "stack trace reduction was {}% ({} -> {} tokens); failures={:?}",
        row.reduction_pct,
        row.before_tokens,
        row.after_tokens,
        row.failures,
    );
    assert!(row.passed, "failures={:?}", row.failures);
}

#[test]
fn test_failed_expectation_isolated() {
    let set = committed_fixtures();
    let mut bad = set
        .fixtures
        .iter()
        .find(|fixture| fixture.name == "repeated-npm-install")
        .expect("repeated log fixture should exist")
        .clone();
    bad.name = "missing-expectation".to_owned();
    bad.expectations
        .must_preserve
        .push("UNOBTAINIUM".to_owned());
    let good = set
        .fixtures
        .iter()
        .find(|fixture| fixture.name == "stack-trace-dupes")
        .expect("stack trace fixture should exist")
        .clone();
    let isolated = FixtureSet {
        fixtures: vec![bad, good],
    };

    let report = {
        let _guard = RUNNER_LOCK
            .lock()
            .expect("runner lock should not be poisoned");
        EvalRunner.run(&isolated)
    };

    assert_eq!(report.fixtures.len(), 2);
    assert!(
        !report
            .fixtures
            .iter()
            .find(|row| row.id == "missing-expectation")
            .expect("bad row should be present")
            .passed
    );
    assert!(
        report
            .fixtures
            .iter()
            .any(|row| row.id == "stack-trace-dupes")
    );
    assert!(report.aggregate.compression_fixtures > 0);
}

#[test]
fn test_protected_mutation_counter_zero() {
    let report = committed_report();

    assert_eq!(report.aggregate.protected_mutations, 0);
}

#[test]
fn test_eligible_median_is_ratio_while_fixture_reduction_is_percent() {
    let report = committed_report();
    let mut eligible_percentages = report
        .fixtures
        .iter()
        .filter(|row| row.eligible && row.output.is_some())
        .map(|row| row.reduction_pct)
        .collect::<Vec<_>>();
    eligible_percentages.sort_by(f64::total_cmp);
    let expected_ratio = eligible_percentages[eligible_percentages.len() / 2] / 100.0;

    assert!((report.aggregate.median_reduction_eligible - expected_ratio).abs() < f64::EPSILON);
    assert!((0.0..=1.0).contains(&report.aggregate.median_reduction_eligible));
}

#[test]
fn test_atlas_fixture_is_evaluated_outside_compression_aggregates() {
    let set = committed_fixtures();
    let atlas = set
        .fixtures
        .iter()
        .find(|fixture| fixture.name == "mini-monorepo")
        .expect("Atlas fixture should exist")
        .clone();
    let report = {
        let _guard = RUNNER_LOCK
            .lock()
            .expect("runner lock should not be poisoned");
        EvalRunner.run(&FixtureSet {
            fixtures: vec![atlas],
        })
    };
    let row = &report.fixtures[0];

    assert!(row.passed, "Atlas expectations failed: {row:#?}");
    assert!(row.failures.is_empty());
    assert!(row.atlas.as_ref().is_some_and(|metrics| {
        metrics.files_indexed >= 5 && metrics.symbols >= 10 && metrics.resolved_edges >= 2
    }));
    assert_eq!(report.aggregate.compression_fixtures, 0);
    assert_eq!(report.aggregate.median_reduction_eligible, 0.0);
    assert_eq!(report.aggregate.fallback_rate, 0.0);
}

#[test]
fn test_report_is_deterministic_modulo_timings() {
    let set = committed_fixtures();
    let (mut first, mut second) = {
        let _guard = RUNNER_LOCK
            .lock()
            .expect("runner lock should not be poisoned");
        (EvalRunner.run(set), EvalRunner.run(set))
    };
    for report in [&mut first, &mut second] {
        report.aggregate.p95_latency_ms = 0;
        for fixture in &mut report.fixtures {
            fixture.duration_ms = 0;
        }
    }

    assert_eq!(
        serde_json::to_value(first).expect("first report should serialize"),
        serde_json::to_value(second).expect("second report should serialize")
    );
}
