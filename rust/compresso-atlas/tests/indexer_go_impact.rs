//! Go `impact` through the package (E17-46, D111): the files of a Go
//! package share their declarations without an import, so a file of the
//! same directory with a stored call that resolves, by the trace rules, to a
//! function, struct, or method the file defines is listed (`references`);
//! every other file of the directory is counted `unproven`, or `unevaluated`
//! when its call sites were cut or a check bound was reached.

use std::fs;
use std::path::{Path, PathBuf};

use compresso_atlas::{
    CallResolution, ImpactOptions, ImpactReport, Indexer, TraceOptions, TraceResult, impact,
    impact_with_options, trace,
};
use compresso_core::{TokenProvider, counter_for};
use compresso_store::{EdgeKind, RepoId, Store};
use tempfile::TempDir;

struct Repo {
    _temp: TempDir,
    root: PathBuf,
    store: Store,
    repo: RepoId,
}

fn write_files(root: &Path, files: &[(&str, &str)]) {
    for (path, source) in files {
        let absolute = root.join(path);
        fs::create_dir_all(absolute.parent().expect("parent")).expect("directory");
        fs::write(absolute, source).expect("file");
    }
}

fn index_files(files: &[(&str, &str)]) -> Repo {
    let temp = tempfile::tempdir().expect("temporary directory");
    let root = temp.path().join("repo");
    write_files(&root, files);
    let store = Store::open(&temp.path().join("compresso.db")).expect("store opens");
    Indexer::new(&store, counter_for(TokenProvider::Heuristic))
        .index(&root, |_| {})
        .expect("index");
    let canonical = fs::canonicalize(&root).expect("root canonicalizes");
    let repo = store
        .repo_by_root(&canonical)
        .expect("lookup")
        .expect("repository")
        .id;
    Repo {
        _temp: temp,
        root,
        store,
        repo,
    }
}

impl Repo {
    fn reindex(&self) {
        Indexer::new(&self.store, counter_for(TokenProvider::Heuristic))
            .index(&self.root, |_| {})
            .expect("re-index");
    }

    fn impact(&self, path: &str) -> ImpactReport {
        impact(&self.store, self.repo, path).expect("impact")
    }
}

/// One dependent as (path, kinds, package).
type Listed = (String, Vec<EdgeKind>, Option<String>);

/// The dependents as (path, kinds, package).
fn listed(report: &ImpactReport) -> Vec<Listed> {
    report
        .dependents
        .iter()
        .map(|entry| {
            (
                entry.path.clone(),
                entry.edge_kinds.clone(),
                entry.package.clone(),
            )
        })
        .collect()
}

fn references(path: &str, package: &str) -> Listed {
    (
        path.to_owned(),
        vec![EdgeKind::References],
        Some(package.to_owned()),
    )
}

const GO_MOD: (&str, &str) = ("go.mod", "module example.com/svc\n");

#[test]
fn test_a_package_mate_calling_a_function_of_the_file_is_a_references_dependent() {
    let repo = index_files(&[
        GO_MOD,
        ("store/parse.go", "package store\n\nfunc Parse() {}\n"),
        (
            "store/load.go",
            "package store\n\nfunc Load() {\n\tParse()\n}\n",
        ),
    ]);
    let report = repo.impact("store/parse.go");
    assert_eq!(listed(&report), [references("store/load.go", "store")]);
    assert_eq!(report.unproven, 0);
    assert_eq!(report.unevaluated, 0);
    assert!(report.complete, "{report:?}");
}

#[test]
fn test_a_package_mate_without_a_call_is_unproven() {
    let repo = index_files(&[
        GO_MOD,
        ("store/parse.go", "package store\n\nfunc Parse() {}\n"),
        (
            "store/load.go",
            "package store\n\nfunc Load() {\n\tParse()\n}\n",
        ),
        // A composite literal or a constant read records no call.
        (
            "store/idle.go",
            "package store\n\ntype Idle struct{}\n\nfunc idle() int { return limit }\n",
        ),
    ]);
    let report = repo.impact("store/parse.go");
    assert_eq!(listed(&report), [references("store/load.go", "store")]);
    assert_eq!(report.unproven, 1);
    assert!(!report.complete);
}

/// Package clauses are not stored (D102): a `package store_test` file is
/// told apart only by its import of the directory's package, which Go
/// allows only an external test package to make. It is listed by that
/// import as before. One that does not import the package and calls a
/// helper of its own of the same name as one the file defines is not
/// evidence (the name is defined twice in the directory): `unproven`.
#[test]
fn test_an_external_test_package_reaches_the_file_only_by_import() {
    let repo = index_files(&[
        GO_MOD,
        (
            "store/parse.go",
            "package store\n\nfunc Parse() {}\n\nfunc helper() {}\n",
        ),
        (
            "store/parse_test.go",
            "package store_test\n\nimport (\n\t\"testing\"\n\n\t\"example.com/svc/store\"\n)\n\nfunc TestParse(t *testing.T) {\n\tstore.Parse()\n}\n",
        ),
        (
            "store/helper_test.go",
            "package store_test\n\nfunc helper() {}\n\nfunc useHelper() {\n\thelper()\n}\n",
        ),
    ]);
    let report = repo.impact("store/parse.go");
    assert_eq!(
        listed(&report),
        [(
            "store/parse_test.go".to_owned(),
            vec![EdgeKind::Imports],
            None
        )]
    );
    assert_eq!(report.unproven, 1, "helper_test.go may use it");
    assert!(!report.complete);
}

/// A `_test.go` file of the package that calls the file is a dependent; a
/// test file's own dependents are only other test files (a non-test file
/// never reaches test code).
#[test]
fn test_an_internal_test_file_is_a_dependent_and_test_code_reaches_only_test_files() {
    let repo = index_files(&[
        GO_MOD,
        ("store/parse.go", "package store\n\nfunc Parse() {}\n"),
        (
            "store/parse_test.go",
            "package store\n\nimport \"testing\"\n\nfunc TestParse(t *testing.T) {\n\tnewFixture()\n\tParse()\n}\n",
        ),
        (
            "store/fixture_test.go",
            "package store\n\nfunc newFixture() {}\n",
        ),
    ]);
    let report = repo.impact("store/parse.go");
    assert_eq!(
        listed(&report),
        [references("store/parse_test.go", "store")]
    );
    assert_eq!(report.unproven, 1, "fixture_test.go may use it");

    let report = impact_with_options(
        &repo.store,
        repo.repo,
        "store/fixture_test.go",
        ImpactOptions {
            max_depth: 1,
            max_results: 100,
        },
    )
    .expect("impact");
    assert_eq!(
        listed(&report),
        [references("store/parse_test.go", "store")]
    );
    assert_eq!(report.unproven, 0, "parse.go is not test code");
    assert!(report.complete, "{report:?}");
}

/// Methods share names across types: `s.Flush()` on a variable is never
/// evidence, even when one method of that name exists in the package. A
/// call on the enclosing method's receiver names its type exactly and is.
#[test]
fn test_a_receiver_method_call_is_not_evidence_and_a_self_call_is() {
    let repo = index_files(&[
        GO_MOD,
        (
            "store/store.go",
            "package store\n\ntype Store struct{}\n\nfunc (s *Store) Flush() {}\n",
        ),
        // `s.Flush()` on a variable (a path call with qualifier `s`).
        (
            "store/run.go",
            "package store\n\nfunc run(s *Store) {\n\ts.Flush()\n}\n",
        ),
        // `fresh().Flush()` on an expression (a receiver call).
        (
            "store/fresh.go",
            "package store\n\nfunc fresh() *Store { return nil }\n\nfunc use() {\n\tfresh().Flush()\n}\n",
        ),
        (
            "store/close.go",
            "package store\n\nfunc (s *Store) Close() {\n\ts.Flush()\n}\n",
        ),
    ]);
    // Trace still resolves the receiver call by its name alone (D102,
    // unchanged): that is why it is no evidence here.
    let report = trace(
        &repo.store,
        repo.repo,
        Some("use"),
        None,
        TraceOptions::default(),
    )
    .expect("trace");
    let TraceResult::Callees(callees) = report.result else {
        panic!("expected callees");
    };
    assert!(
        callees
            .iter()
            .any(|callee| callee.symbol.qualified_name == "Store::Flush"
                && callee.resolution == CallResolution::SamePackage),
        "{callees:?}"
    );

    let report = repo.impact("store/store.go");
    assert_eq!(listed(&report), [references("store/close.go", "store")]);
    assert_eq!(report.unproven, 2, "run.go and fresh.go may use it");
    assert!(!report.complete);
}

/// A name defined in two files of the directory (build-constrained
/// variants) is ambiguous by the trace rules, so a call to it is no
/// evidence for either.
#[test]
fn test_a_call_to_a_name_two_files_define_is_not_evidence() {
    let repo = index_files(&[
        GO_MOD,
        ("sys/open_linux.go", "package sys\n\nfunc open() {}\n"),
        ("sys/open_windows.go", "package sys\n\nfunc open() {}\n"),
        ("sys/run.go", "package sys\n\nfunc Run() {\n\topen()\n}\n"),
    ]);
    let report = repo.impact("sys/open_linux.go");
    assert!(report.dependents.is_empty(), "{report:?}");
    assert_eq!(report.unproven, 2);
    assert!(!report.complete);
}

/// A file that declares nothing at package level (`doc.go`) can be used by
/// no other file of its package.
#[test]
fn test_a_file_that_declares_nothing_has_no_package_dependents() {
    let repo = index_files(&[
        GO_MOD,
        (
            "store/doc.go",
            "// Package store keeps jobs.\npackage store\n",
        ),
        ("store/parse.go", "package store\n\nfunc Parse() {}\n"),
    ]);
    let report = repo.impact("store/doc.go");
    assert!(report.dependents.is_empty());
    assert_eq!(report.unproven, 0);
    assert!(report.complete, "{report:?}");
}

#[test]
fn test_a_mate_cut_at_the_examined_call_bound_is_unevaluated() {
    let calls = "g(); ".repeat(70_000);
    let mate =
        format!("package store\n\nfunc g() {{}}\n\nfunc run() {{\n\t{calls}\n\tParse()\n}}\n");
    let repo = index_files(&[
        GO_MOD,
        ("store/parse.go", "package store\n\nfunc Parse() {}\n"),
        ("store/mate.go", mate.as_str()),
    ]);
    assert_eq!(
        repo.store
            .files_with_cut_call_sites(repo.repo)
            .expect("cut")
            .into_iter()
            .collect::<Vec<_>>(),
        ["store/mate.go"]
    );
    let report = repo.impact("store/parse.go");
    assert!(report.dependents.is_empty(), "{report:?}");
    assert_eq!(report.unevaluated, 1);
    assert_eq!(report.unproven, 0);
    assert!(!report.complete);
    // The marker is no call: trace still lists the cut file's kept calls only.
    let report = trace(
        &repo.store,
        repo.repo,
        None,
        Some("g"),
        TraceOptions::default(),
    )
    .expect("trace");
    let TraceResult::Callers(callers) = report.result else {
        panic!("expected callers");
    };
    assert_eq!(callers.len(), 1, "{callers:?}");
}

/// A Go file defining more than 400 names (functions, struct types, and
/// methods) is not checked (`unevaluated`), and at most 64 files are
/// checked per `impact` (D105's file bound, reused).
#[test]
fn test_the_evidence_bounds_count_what_they_skip_as_unevaluated() {
    let mut many = String::from("package wide\n\n");
    for index in 0..401 {
        many.push_str(&format!("func F{index}() {{}}\n"));
    }
    let repo = index_files(&[
        GO_MOD,
        ("wide/many.go", many.as_str()),
        ("wide/user.go", "package wide\n\nfunc use() {\n\tF0()\n}\n"),
    ]);
    let report = repo.impact("wide/many.go");
    assert!(report.dependents.is_empty(), "{report:?}");
    assert_eq!(report.unevaluated, 1);
    assert_eq!(report.unproven, 0);
    assert!(!report.complete);

    let mut files = vec![
        GO_MOD,
        ("pkg/target.go", "package pkg\n\nfunc Target() {}\n"),
    ];
    let sources = (0..70)
        .map(|index| {
            (
                format!("pkg/f{index:02}.go"),
                format!("package pkg\n\nfunc f{index:02}() {{\n\tTarget()\n}}\n"),
            )
        })
        .collect::<Vec<_>>();
    files.extend(
        sources
            .iter()
            .map(|(path, source)| (path.as_str(), source.as_str())),
    );
    let repo = index_files(&files);
    let report = repo.impact("pkg/target.go");
    assert_eq!(report.total, 70);
    assert!(
        report
            .dependents
            .iter()
            .all(|entry| entry.depth == 1 && entry.edge_kinds == [EdgeKind::References])
    );
    // The target and 63 of its dependents are checked; 7 are not.
    assert_eq!(report.unevaluated, 7);
    assert!(!report.complete);
}

/// What an `impact` answer says: dependents, unproven, unevaluated, complete.
type Summary = (Vec<Listed>, usize, usize, bool);

fn summary(report: &ImpactReport) -> Summary {
    (
        listed(report),
        report.unproven,
        report.unevaluated,
        report.complete,
    )
}

/// Evidence is read at query time from per-file rows, so an incremental
/// index that adds or removes a file of the package answers as a full index
/// of the same tree.
#[test]
fn test_incremental_index_of_a_package_matches_a_full_index() {
    let base = [
        GO_MOD,
        ("store/parse.go", "package store\n\nfunc Parse() {}\n"),
        (
            "store/load.go",
            "package store\n\nfunc Load() {\n\tParse()\n}\n",
        ),
    ];
    let added = (
        "store/save.go",
        "package store\n\nfunc Save() {\n\tParse()\n}\n",
    );
    let idle = ("store/idle.go", "package store\n\nfunc idle() {}\n");
    let repo = index_files(&base);
    write_files(&repo.root, &[added, idle]);
    repo.reindex();
    let full = index_files(&[base[0], base[1], base[2], added, idle]);
    assert_eq!(
        summary(&repo.impact("store/parse.go")),
        summary(&full.impact("store/parse.go"))
    );
    assert_eq!(repo.impact("store/parse.go").unproven, 1);

    fs::remove_file(repo.root.join("store/load.go")).expect("remove");
    fs::remove_file(repo.root.join("store/idle.go")).expect("remove");
    repo.reindex();
    let full = index_files(&[base[0], base[1], added]);
    let incremental = repo.impact("store/parse.go");
    assert_eq!(
        summary(&incremental),
        summary(&full.impact("store/parse.go"))
    );
    assert_eq!(listed(&incremental), [references("store/save.go", "store")]);
    assert!(incremental.complete);
}

/// A store indexed before the cut marker was written for Go re-parses every
/// file once, so a Go file cut at the call bound gains its marker.
#[test]
fn test_a_store_indexed_before_go_cut_markers_reindexes() {
    // The version stamped before E17-46.
    const OLD_VERSION: &str =
        "0.1.0+atlas-shell-text-v1+rust-imports-v2+sql-v1+java-csharp-v1+go-v1+calls-v1";
    let calls = "g(); ".repeat(70_000);
    let mate = format!("package store\n\nfunc g() {{}}\n\nfunc run() {{\n\t{calls}\n}}\n");
    let temp = tempfile::tempdir().expect("temporary directory");
    let root = temp.path().join("repo");
    write_files(
        &root,
        &[
            GO_MOD,
            ("store/parse.go", "package store\n\nfunc Parse() {}\n"),
            ("store/mate.go", mate.as_str()),
        ],
    );
    let store = Store::open(&temp.path().join("compresso.db")).expect("store opens");
    let counter = counter_for(TokenProvider::Heuristic);
    Indexer::new(&store, counter)
        .with_parser_version(OLD_VERSION)
        .index(&root, |_| {})
        .expect("old index");
    let canonical = fs::canonicalize(&root).expect("root canonicalizes");
    let repo = store
        .repo_by_root(&canonical)
        .expect("lookup")
        .expect("repository")
        .id;
    // What the older parser left behind: the cut file without its marker.
    let record = store
        .indexed_file(repo, "store/mate.go")
        .expect("lookup")
        .expect("file");
    let symbols = store
        .symbols_in_file(repo, "store/mate.go")
        .expect("symbols");
    store
        .replace_indexed_file(repo, &record, &symbols, &[])
        .expect("old rows");
    Indexer::new(&store, counter)
        .with_parser_version(OLD_VERSION)
        .index(&root, |_| {})
        .expect("same-version index");
    assert!(
        store
            .files_with_cut_call_sites(repo)
            .expect("cut")
            .is_empty()
    );

    Indexer::new(&store, counter)
        .index(&root, |_| {})
        .expect("new index");
    let version = store
        .latest_index_version(repo)
        .expect("version")
        .expect("stamped");
    assert!(
        version.parser_version.contains("+go-impact-v1"),
        "{version:?}"
    );
    assert!(
        store
            .files_with_cut_call_sites(repo)
            .expect("cut")
            .contains("store/mate.go")
    );
    let report = impact(&store, repo, "store/parse.go").expect("impact");
    assert_eq!(report.unevaluated, 1);
}

/// The callees `trace` lists for a caller, by qualified name.
fn callees(repo: &Repo, from: &str) -> Vec<String> {
    let report = trace(
        &repo.store,
        repo.repo,
        Some(from),
        None,
        TraceOptions::default(),
    )
    .expect("trace");
    match report.result {
        TraceResult::Callees(callees) => callees
            .into_iter()
            .map(|callee| callee.symbol.qualified_name)
            .collect(),
        TraceResult::NoPath => Vec::new(),
        other => panic!("expected callees, got {other:?}"),
    }
}

/// Review must-fix 1: a name bound inside the calling function (a
/// parameter, a named result, a func-literal parameter, a local) shadows
/// the package's declaration, so a call to it is no evidence and `trace`
/// no longer follows it to the package function. A call to the same name
/// where nothing binds it still counts.
#[test]
fn test_a_call_to_a_name_bound_in_the_function_is_not_evidence() {
    let repo = index_files(&[
        GO_MOD,
        (
            "retry/attempt.go",
            "package retry\n\nfunc attempt() error { return nil }\n",
        ),
        (
            "retry/param.go",
            "package retry\n\nfunc withParam(attempt func() error) error {\n\treturn attempt()\n}\n",
        ),
        (
            "retry/result.go",
            "package retry\n\nfunc withResult() (attempt func() error) {\n\tattempt()\n\treturn nil\n}\n",
        ),
        (
            "retry/literal.go",
            "package retry\n\nfunc withLiteral() {\n\trun := func(attempt func() error) { attempt() }\n\trun(nil)\n}\n",
        ),
        (
            "retry/local.go",
            "package retry\n\nfunc withLocal() {\n\tif true {\n\t\tattempt := func() error { return nil }\n\t\t_ = attempt\n\t}\n\tattempt()\n}\n",
        ),
        (
            "retry/plain.go",
            "package retry\n\nfunc withPlain() error {\n\treturn attempt()\n}\n",
        ),
    ]);
    let report = repo.impact("retry/attempt.go");
    assert_eq!(listed(&report), [references("retry/plain.go", "retry")]);
    assert_eq!(report.unproven, 4, "{report:?}");
    assert!(!report.complete);
    assert!(callees(&repo, "withParam").is_empty());
    assert!(callees(&repo, "withResult").is_empty());
    assert_eq!(callees(&repo, "withPlain"), ["attempt"]);
}

/// Review must-fix 1 (b): a receiver name rebound inside the method (a
/// `range` variable, a func-literal parameter) no longer names the
/// receiver, so a call on it is no self call and no evidence.
#[test]
fn test_a_rebound_receiver_name_is_not_a_self_call() {
    let repo = index_files(&[
        GO_MOD,
        (
            "store/store.go",
            "package store\n\ntype Store struct{}\n\nfunc (s *Store) flush() {}\n",
        ),
        (
            "store/other.go",
            "package store\n\ntype Other struct{}\n\nfunc (o *Other) flush() {}\n",
        ),
        (
            "store/drain.go",
            "package store\n\nfunc (s *Store) drain(o []*Other) {\n\tfor _, s := range o {\n\t\ts.flush()\n\t}\n}\n",
        ),
        (
            "store/spawn.go",
            "package store\n\nfunc (s *Store) spawn() {\n\tgo func(s *Other) { s.flush() }(nil)\n}\n",
        ),
        (
            "store/close.go",
            "package store\n\nfunc (s *Store) close() {\n\ts.flush()\n}\n",
        ),
    ]);
    let report = repo.impact("store/store.go");
    assert_eq!(listed(&report), [references("store/close.go", "store")]);
    assert_eq!(report.unproven, 3, "other.go, drain.go, spawn.go");
}

/// Review should-fix: two types of the package share a method name; a self
/// call reaches the receiver's own type only.
#[test]
fn test_a_self_call_reaches_only_its_own_types_method() {
    let repo = index_files(&[
        GO_MOD,
        (
            "store/store.go",
            "package store\n\ntype Store struct{}\n\nfunc (s *Store) flush() {}\n",
        ),
        (
            "store/other.go",
            "package store\n\ntype Other struct{}\n\nfunc (o *Other) flush() {}\n",
        ),
        (
            "store/close.go",
            "package store\n\nfunc (o *Other) close() {\n\to.flush()\n}\n",
        ),
    ]);
    let report = repo.impact("store/store.go");
    assert!(report.dependents.is_empty(), "{report:?}");
    assert_eq!(report.unproven, 2);
    let report = repo.impact("store/other.go");
    assert_eq!(listed(&report), [references("store/close.go", "store")]);
}

/// Review should-fix 2: a dependent linked at one depth both by a file
/// edge and through a package keeps the file edge's kind only (D105: a
/// file edge at the same depth names the link), whichever link the walk
/// meets first; renaming a directory never changes the label.
#[test]
fn test_a_file_edge_wins_over_a_package_link_in_either_order() {
    for (edge_dir, mate_dir) in [("b", "c"), ("z", "c")] {
        let importer = format!("{edge_dir}/m.go");
        let importer_source = format!(
            "package {edge_dir}\n\nimport \"example.com/svc/a\"\n\nfunc M() {{\n\ta.T()\n}}\n"
        );
        let mate = format!("{mate_dir}/n.go");
        let mate_source = format!(
            "package {mate_dir}\n\nimport \"example.com/svc/a\"\n\nfunc N() {{\n\ta.T()\n}}\n"
        );
        let user = format!("{mate_dir}/x.go");
        let user_source = format!(
            "package {mate_dir}\n\nimport \"example.com/svc/{edge_dir}\"\n\nfunc X() {{\n\t{edge_dir}.M()\n\tN()\n}}\n"
        );
        let repo = index_files(&[
            GO_MOD,
            ("a/t.go", "package a\n\nfunc T() {}\n"),
            (importer.as_str(), importer_source.as_str()),
            (mate.as_str(), mate_source.as_str()),
            (user.as_str(), user_source.as_str()),
        ]);
        let report = repo.impact("a/t.go");
        let entry = report
            .dependents
            .iter()
            .find(|entry| entry.path == user)
            .unwrap_or_else(|| panic!("{user} listed: {report:?}"));
        assert_eq!(entry.depth, 2, "{edge_dir}");
        assert_eq!(entry.edge_kinds, [EdgeKind::Imports], "{edge_dir}");
        assert_eq!(entry.package, None, "{edge_dir}");
    }
}
