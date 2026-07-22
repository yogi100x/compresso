use std::fs;
use std::path::Path;

use compresso_eval::{EvalRunError, EvalRunner, FixtureSet};
use tempfile::TempDir;

#[test]
fn test_probe_passes_on_preserved_answer() {
    let temporary = TempDir::new().expect("temporary fixture root should be created");
    write_fixture(
        temporary.path(),
        "preserved-answer",
        r#"[[input.nodes]]
node_id = "diagnostic"
kind = "summary"
content = "The build failed: Cannot find module 'left-pad'"
"#,
        100,
        r#"[[recall_probes]]
question = "What caused the webpack build failure?"
answer_substring = "Cannot find module 'left-pad'"
protected = true
"#,
    );

    let report = try_run(temporary.path()).expect("preserved probe should evaluate");

    assert_eq!(report.recall.fixtures.len(), 1);
    assert_eq!(report.recall.fixtures[0].fixture, "preserved-answer");
    assert_eq!(report.recall.fixtures[0].probes.len(), 1);
    assert_eq!(report.recall.fixtures[0].probes[0].probe_index, 0);
    assert!(report.recall.fixtures[0].probes[0].passed);
    assert_eq!(report.recall.protected_probe_recall, 1.0);
    assert_eq!(report.recall.overall_probe_recall, 1.0);

    let json = serde_json::to_value(&report).expect("report should serialize");
    assert_eq!(json["recall"]["protected_probe_recall"], 1.0);
    assert_eq!(json["recall"]["overall_probe_recall"], 1.0);
    assert_eq!(json["recall"]["fixtures"][0]["fixture"], "preserved-answer");
    assert_eq!(json["recall"]["fixtures"][0]["probes"][0]["probe_index"], 0);
}

#[test]
fn test_probe_failure_counted_not_fatal() {
    let temporary = TempDir::new().expect("temporary fixture root should be created");
    write_pack_budget_fixture(
        temporary.path(),
        "dropped-answer",
        r#"[[recall_probes]]
answer_substring = "drop-me-probe"
protected = false
"#,
    );

    let report = try_run(temporary.path()).expect("compressed probe failure should not abort");
    let outcome = &report.recall.fixtures[0].probes[0];

    assert!(!outcome.passed);
    assert_eq!(outcome.fixture, "dropped-answer");
    assert_eq!(outcome.probe_index, 0);
    assert_eq!(report.recall.overall_probe_recall, 0.0);
    assert_eq!(report.recall.protected_probe_recall, 1.0);
    assert!(
        report.fixtures[0]
            .output
            .as_ref()
            .expect("compression output should be available")
            .nodes
            .iter()
            .all(|node| node.id != "discarded"),
        "pack_budget should remove the low-value node carrying the probe answer",
    );
}

#[test]
fn test_broken_probe_errors_on_passthrough() {
    let temporary = TempDir::new().expect("temporary fixture root should be created");
    write_fixture(
        temporary.path(),
        "broken-probe",
        r#"[[input.nodes]]
node_id = "diagnostic"
kind = "summary"
content = "The input contains a different fact."
"#,
        100,
        r#"[[recall_probes]]
answer_substring = "this answer never existed"
protected = false
"#,
    );

    let error = try_run(temporary.path()).expect_err("broken baseline probe should abort the run");

    assert_eq!(error.code(), "broken_probe");
    assert!(error.to_string().contains("broken-probe"));
    assert!(error.to_string().contains("probe 0"));
    assert!(matches!(
        error,
        EvalRunError::BrokenProbe {
            fixture,
            probe_index: 0,
        } if fixture == "broken-probe"
    ));

    let fixtures = FixtureSet::load(temporary.path()).expect("synthetic fixture should load");
    let compatibility_report = EvalRunner.run(&fixtures);
    assert_eq!(compatibility_report.recall.overall_probe_recall, 0.0);
}

#[test]
fn test_protected_probes_bucketed_separately() {
    let temporary = TempDir::new().expect("temporary fixture root should be created");
    write_pack_budget_fixture(
        temporary.path(),
        "mixed-probes",
        r#"[[recall_probes]]
answer_substring = "keep-me-probe"
protected = true

[[recall_probes]]
answer_substring = "drop-me-probe"
protected = false
"#,
    );

    let report = try_run(temporary.path()).expect("mixed probes should evaluate");
    let recall = &report.recall;

    assert_eq!(recall.fixtures.len(), 1);
    assert_eq!(recall.fixtures[0].probes.len(), 2);
    assert!(recall.fixtures[0].probes[0].protected);
    assert!(recall.fixtures[0].probes[0].passed);
    assert!(!recall.fixtures[0].probes[1].protected);
    assert!(!recall.fixtures[0].probes[1].passed);
    assert_eq!(recall.protected_probe_recall, 1.0);
    assert_eq!(recall.overall_probe_recall, 0.5);
}

fn try_run(root: &Path) -> Result<compresso_eval::EvalReport, EvalRunError> {
    let fixtures = FixtureSet::load(root).expect("synthetic fixture should load");
    EvalRunner.try_run(&fixtures)
}

fn write_pack_budget_fixture(root: &Path, name: &str, probes: &str) {
    let directory = root.join("logs").join(name);
    fs::create_dir_all(&directory).expect("fixture directory should be created");
    fs::write(
        directory.join("retained.txt"),
        "keep-me-probe is the authoritative fact",
    )
    .expect("retained fixture input should be written");
    let low_value = format!("drop-me-probe {}", "filler ".repeat(200));
    fs::write(directory.join("discarded.txt"), low_value)
        .expect("discarded fixture input should be written");
    let input = r#"[[input.files]]
path = "retained.txt"
node_id = "retained"
kind = "summary"
priority = 1.0

[[input.files]]
path = "discarded.txt"
node_id = "discarded"
kind = "summary"
priority = 0.0
"#;
    write_fixture(root, name, input, 50, probes);
}

fn write_fixture(root: &Path, name: &str, input: &str, max_tokens: usize, probes: &str) {
    let directory = root.join("logs").join(name);
    fs::create_dir_all(&directory).expect("fixture directory should be created");
    fs::write(
        directory.join("fixture.toml"),
        format!(
            r#"name = "{name}"
category = "logs"
description = "Synthetic recall evaluation fixture."
eligible = false
mode = "safe"

[budget]
max_tokens = {max_tokens}
max_latency_ms = 200

{input}

[expectations]
min_reduction_pct = 0.0
expect_fallback = false

{probes}"#,
        ),
    )
    .expect("fixture manifest should be written");
}
