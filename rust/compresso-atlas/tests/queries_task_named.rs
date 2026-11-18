//! D88: names and numbers a task writes verbatim steer the context pack.

use std::fs;
use std::path::PathBuf;

use compresso_atlas::{ContextPlan, plan_context};
use compresso_core::{TokenProvider, counter_for};
use compresso_store::{FileLanguage, FileRecord, Store, SymbolKind, SymbolRecord};
use tempfile::TempDir;

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
        let root = store.repository(repo).expect("repository loads").root;
        Self {
            _temp: temp,
            root,
            store,
            repo,
        }
    }

    fn write(&self, path: &str, source: &str, symbols: &[SymbolRecord]) {
        let destination = self.root.join(path);
        fs::create_dir_all(destination.parent().expect("parent")).expect("directory created");
        fs::write(&destination, source).expect("source written");
        self.store
            .replace_indexed_file(
                self.repo,
                &FileRecord {
                    path: path.to_owned(),
                    hash: blake3::hash(source.as_bytes()).to_hex().to_string(),
                    language: Some(FileLanguage::Rust),
                    size_bytes: source.len() as u64,
                    token_estimate: counter_for(TokenProvider::OpenAiO200k).count(source),
                    indexed_at: 10,
                },
                symbols,
                &[],
            )
            .expect("file indexes");
    }

    fn plan(&self, task: &str) -> ContextPlan {
        plan_context(&self.store, self.repo, task, 1_600).expect("context should plan")
    }

    /// Ninety files whose names, signatures, and bodies spell the prose of
    /// the planner tasks below, twelve functions that take a `ContextPlan`,
    /// and the one file that defines it.
    fn seed_large_prose_index(&self) {
        for index in 0..90 {
            let name = format!("evidence_budget_stage_{index:02}");
            let signature =
                format!("pub fn {name}(budget: usize, files: &[CandidateFile]) -> Coverage");
            self.write(
                &format!("src/pipeline/stage_{index:02}.rs"),
                &format!(
                    "{signature} {{\n    // trims candidate evidence files to the tight budget, records coverage, marks truncated packs\n    assemble_coverage(budget, files)\n}}\n"
                ),
                &[symbol(&name, SymbolKind::Function, &signature, 1, 4)],
            );
        }
        for index in 0..12 {
            let name = format!("render_plan_{index:02}");
            let signature = format!("pub fn {name}(plan: &ContextPlan) -> String");
            self.write(
                &format!("src/render/plan_{index:02}.rs"),
                &format!("{signature} {{\n    plan.rendered.clone()\n}}\n"),
                &[symbol(&name, SymbolKind::Function, &signature, 1, 3)],
            );
        }
        self.write(
            "src/plan.rs",
            "pub struct ContextPlan {\n    pub rendered: String,\n    pub truncated: bool,\n}\n\npub const PLAN_BUDGET_STEPS: [usize; 3] = [800, 1600, 3200];\n\npub fn widen_plan_budget(step: usize) -> usize {\n    PLAN_BUDGET_STEPS[step]\n}\n",
            &[
                symbol(
                    "ContextPlan",
                    SymbolKind::Struct,
                    "pub struct ContextPlan",
                    1,
                    4,
                ),
                symbol(
                    "PLAN_BUDGET_STEPS",
                    SymbolKind::Const,
                    "pub const PLAN_BUDGET_STEPS: [usize; 3]",
                    6,
                    6,
                ),
                symbol(
                    "widen_plan_budget",
                    SymbolKind::Function,
                    "pub fn widen_plan_budget(step: usize) -> usize",
                    8,
                    10,
                ),
            ],
        );
    }
}

fn symbol(
    name: &str,
    kind: SymbolKind,
    signature: &str,
    start_line: u32,
    end_line: u32,
) -> SymbolRecord {
    SymbolRecord {
        name: name.to_owned(),
        qualified_name: name.to_owned(),
        kind,
        signature: Some(signature.to_owned()),
        start_line,
        end_line,
        token_estimate: 10,
    }
}

fn leading(plan: &ContextPlan) -> (&str, &str) {
    let body = plan
        .symbol_bodies
        .first()
        .unwrap_or_else(|| panic!("the pack must carry evidence: {}", plan.rendered));
    (body.path.as_str(), body.symbol.qualified_name.as_str())
}

#[test]
fn test_plan_context_named_type_leads_the_pack_in_a_large_index() {
    let fixture = Fixture::new();
    fixture.seed_large_prose_index();
    let task = "Explain how the ContextPlan is assembled when the evidence budget is tight, \
                which candidate files get truncated, and how coverage is recorded for them";

    let plan = fixture.plan(task);
    let repeated = fixture.plan(task);

    assert_eq!(plan, repeated, "named ranking must be deterministic");
    assert_eq!(
        leading(&plan),
        ("src/plan.rs", "ContextPlan"),
        "the type the task names verbatim must lead: {}",
        plan.rendered
    );
}

#[test]
fn test_plan_context_snake_screaming_and_path_identifiers_lead_prose_matches() {
    let fixture = Fixture::new();
    fixture.seed_large_prose_index();

    for (task, expected) in [
        (
            "Which stage calls widen_plan_budget when the evidence budget is too tight for candidate files?",
            "widen_plan_budget",
        ),
        (
            "How are PLAN_BUDGET_STEPS chosen when the evidence budget is tight and coverage is truncated?",
            "PLAN_BUDGET_STEPS",
        ),
    ] {
        let plan = fixture.plan(task);
        assert_eq!(
            leading(&plan),
            ("src/plan.rs", expected),
            "{task}: {}",
            plan.rendered
        );
    }

    let plan = fixture.plan(
        "What does src/plan.rs hold for the evidence budget, and how is coverage of truncated candidate files recorded?",
    );
    assert_eq!(leading(&plan).0, "src/plan.rs", "{}", plan.rendered);
}

#[test]
fn test_plan_context_common_name_defined_many_times_does_not_lead() {
    let fixture = Fixture::new();
    fixture.seed_large_prose_index();
    for index in 0..4 {
        fixture.write(
            &format!("src/misc/helper_{index}.rs"),
            "pub fn build_index() -> usize {\n    0\n}\n",
            &[symbol(
                "build_index",
                SymbolKind::Function,
                "pub fn build_index() -> usize",
                1,
                3,
            )],
        );
    }

    let plan = fixture.plan(
        "Does build_index matter when the ContextPlan evidence budget is tight and coverage is truncated?",
    );

    assert_eq!(
        leading(&plan),
        ("src/plan.rs", "ContextPlan"),
        "{}",
        plan.rendered
    );
}

#[test]
fn test_plan_context_numeric_literal_sites_become_evidence() {
    let fixture = Fixture::new();
    fixture.write(
        "src/upload.rs",
        "pub fn check_part(size: usize) -> Result<(), Refused> {\n    if size > 320 * 1024 {\n        return Err(Refused);\n    }\n    Ok(())\n}\n",
        &[symbol(
            "check_part",
            SymbolKind::Function,
            "pub fn check_part(size: usize) -> Result<(), Refused>",
            1,
            6,
        )],
    );
    fixture.write(
        "src/idle.rs",
        "pub fn reap(clock: &Clock) {\n    let idle = clock.idle();\n    if idle > Duration::from_secs(45) {\n        clock.reset();\n    }\n}\n",
        &[symbol("reap", SymbolKind::Function, "pub fn reap(clock: &Clock)", 1, 6)],
    );
    // Decoys that spell the digits only inside longer numbers.
    fixture.write(
        "src/pad.rs",
        "pub fn pad() -> usize {\n    let total = 1320 + 3200 + 450;\n    total\n}\n",
        &[symbol(
            "pad",
            SymbolKind::Function,
            "pub fn pad() -> usize",
            1,
            4,
        )],
    );

    let plan = fixture.plan("Where is the 320 limit enforced?");
    let names = plan
        .symbol_bodies
        .iter()
        .map(|body| body.symbol.qualified_name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(names.first(), Some(&"check_part"), "{}", plan.rendered);
    assert!(
        !names.contains(&"pad"),
        "1320 and 3200 are not 320: {names:?}"
    );
    assert!(
        plan.symbol_bodies[0].content.contains("320 * 1024"),
        "the excerpt shows the literal: {:?}",
        plan.symbol_bodies[0].content
    );

    let plan = fixture.plan("What happens after 45 seconds of inactivity?");
    let names = plan
        .symbol_bodies
        .iter()
        .map(|body| body.symbol.qualified_name.as_str())
        .collect::<Vec<_>>();
    assert!(
        names.contains(&"reap"),
        "a two-digit literal is searched: {}",
        plan.rendered
    );
    assert!(!names.contains(&"pad"), "450 is not 45: {names:?}");
}
