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

/// D105's bounds, reused: a file defining more than 64 names is not checked
/// (`unevaluated`), and at most 64 files are checked per `impact`.
#[test]
fn test_the_evidence_bounds_count_what_they_skip_as_unevaluated() {
    let mut many = String::from("package wide\n\n");
    for index in 0..65 {
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
