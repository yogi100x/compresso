use std::fs;
use std::path::{Path, PathBuf};

use compresso_atlas::{
    CallResolution, Indexer, TraceEnd, TraceOptions, TraceReport, TraceResult, trace,
};
use compresso_core::{TokenProvider, counter_for};
use compresso_store::{RepoId, Store};
use tempfile::TempDir;

const FILES: [(&str, &str); 7] = [
    (
        "src/lib.rs",
        "mod a;
mod b;
mod helper;
mod store;

pub fn entry() {
    middle();
}

pub fn middle() {
    store::save();
    helper::shared();
}

pub fn recurse(n: u32) {
    if n > 0 {
        recurse(n - 1);
    }
}

pub fn ping() {
    pong();
}

pub fn pong() {
    ping();
}

pub fn lonely() {}

pub fn ambiguous_caller() {
    dup();
}
",
    ),
    (
        "src/store.rs",
        "pub fn save() {
    write();
}

fn write() {}

pub struct Store;

impl Store {
    pub fn open() -> Self {
        Self::init();
        Store
    }

    fn init() {}
}
",
    ),
    (
        "src/helper.rs",
        "pub fn shared() {\n    crate::store::save();\n}\n",
    ),
    ("src/a.rs", "pub fn dup() {}\n"),
    ("src/b.rs", "pub fn dup() {}\n"),
    (
        "py/app.py",
        "from py import util


class Service:
    def run(self):
        self.step()

    def step(self):
        util.work()


def main():
    Service().run()
",
    ),
    ("py/util.py", "def work():\n    pass\n"),
];

struct Fixture {
    _temp: TempDir,
    root: PathBuf,
    store: Store,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().expect("temporary directory should be created");
        let root = temp.path().join("repo");
        for (path, source) in FILES {
            let absolute = root.join(path);
            fs::create_dir_all(absolute.parent().expect("parent")).expect("directory");
            fs::write(absolute, source).expect("fixture file should be written");
        }
        let store = Store::open(&temp.path().join("compresso.db")).expect("store should open");
        Indexer::new(&store, counter_for(TokenProvider::Heuristic))
            .index(&root, |_| {})
            .expect("fixture should index");
        Self {
            _temp: temp,
            root,
            store,
        }
    }

    fn repo(&self) -> RepoId {
        let root = fs::canonicalize(&self.root).expect("root should canonicalize");
        self.store
            .repo_by_root(Path::new(&root))
            .expect("lookup")
            .expect("repository")
            .id
    }

    fn trace(&self, from: Option<&str>, to: Option<&str>, options: TraceOptions) -> TraceReport {
        trace(&self.store, self.repo(), from, to, options).expect("trace should succeed")
    }
}

fn path_names(report: &TraceReport) -> Vec<Vec<String>> {
    let TraceResult::Paths(paths) = &report.result else {
        panic!("expected paths, got {:?}", report.result);
    };
    paths
        .iter()
        .map(|path| {
            path.iter()
                .map(|hop| hop.symbol.qualified_name.clone())
                .collect()
        })
        .collect()
}

#[test]
fn test_trace_finds_the_shortest_call_path_across_files() {
    let fixture = Fixture::new();
    let report = fixture.trace(Some("entry"), Some("write"), TraceOptions::default());

    assert_eq!(path_names(&report), [["entry", "middle", "save", "write"]]);
    let TraceResult::Paths(paths) = &report.result else {
        unreachable!()
    };
    let hops = &paths[0];
    assert_eq!(hops[0].call_site, None);
    assert_eq!(hops[0].symbol.path, "src/lib.rs");
    assert_eq!(hops[0].symbol.signature.as_deref(), Some("pub fn entry()"));
    let sites = hops[1..]
        .iter()
        .map(|hop| {
            let site = hop.call_site.as_ref().expect("call site");
            (site.path.as_str(), site.line, hop.resolution)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        sites,
        [
            ("src/lib.rs", 7, Some(CallResolution::SameFile)),
            ("src/lib.rs", 11, Some(CallResolution::UniqueName)),
            ("src/store.rs", 2, Some(CallResolution::SameFile)),
        ]
    );
    assert_eq!(hops[3].symbol.path, "src/store.rs");
    assert_eq!(hops[3].symbol.start_line, 5);
    assert!(report.complete);
    assert!(!report.truncated);
    assert_eq!(report.total, 1);
}

#[test]
fn test_trace_reports_no_path_when_none_exists() {
    let fixture = Fixture::new();
    let report = fixture.trace(Some("lonely"), Some("write"), TraceOptions::default());
    assert_eq!(report.result, TraceResult::NoPath);
    assert!(report.complete);
    assert!(!report.depth_limited);

    let report = fixture.trace(Some("write"), Some("entry"), TraceOptions::default());
    assert_eq!(report.result, TraceResult::NoPath);
}

#[test]
fn test_trace_returns_candidates_for_an_ambiguous_name_and_labels_ambiguous_calls() {
    let fixture = Fixture::new();
    let report = fixture.trace(None, Some("dup"), TraceOptions::default());
    let TraceResult::Ambiguous {
        end,
        candidates,
        total,
    } = &report.result
    else {
        panic!("expected ambiguity, got {:?}", report.result);
    };
    assert_eq!(*end, TraceEnd::To);
    assert_eq!(*total, 2);
    assert_eq!(
        candidates
            .iter()
            .map(|candidate| candidate.path.as_str())
            .collect::<Vec<_>>(),
        ["src/a.rs", "src/b.rs"]
    );

    // A path-qualified name picks one.
    let report = fixture.trace(None, Some("src/a.rs:dup"), TraceOptions::default());
    let TraceResult::Callers(callers) = &report.result else {
        panic!("expected callers, got {:?}", report.result);
    };
    assert!(callers.is_empty());
    // The one call to `dup` is ambiguous: reported, not followed or guessed.
    assert!(!report.complete);
    assert_eq!(report.unfollowed_total, 1);
    assert_eq!(report.unfollowed[0].status, "ambiguous");
    assert_eq!(report.unfollowed[0].caller, "ambiguous_caller");
    assert_eq!(
        report.unfollowed[0].candidates,
        ["src/a.rs:dup", "src/b.rs:dup"]
    );

    let report = fixture.trace(Some("ambiguous_caller"), None, TraceOptions::default());
    assert_eq!(report.result, TraceResult::Callees(Vec::new()));
    assert!(!report.complete);
    assert_eq!(report.unfollowed[0].callee, "dup");
}

#[test]
fn test_trace_depth_cap_stops_the_search_and_says_so() {
    let fixture = Fixture::new();
    let options = TraceOptions {
        max_depth: Some(2),
        ..TraceOptions::default()
    };
    let report = fixture.trace(Some("entry"), Some("write"), options);
    assert_eq!(report.result, TraceResult::NoPath);
    assert!(report.depth_limited);
    assert!(!report.complete);
    assert_eq!(report.max_depth, 2);
}

#[test]
fn test_trace_is_cycle_safe_and_marks_recursion() {
    let fixture = Fixture::new();
    assert_eq!(
        path_names(&fixture.trace(Some("ping"), Some("pong"), TraceOptions::default())),
        [["ping", "pong"]]
    );
    assert_eq!(
        path_names(&fixture.trace(Some("ping"), Some("ping"), TraceOptions::default())),
        [["ping", "pong", "ping"]]
    );
    let options = TraceOptions {
        max_depth: Some(8),
        ..TraceOptions::default()
    };
    let report = fixture.trace(Some("ping"), None, options);
    let TraceResult::Callees(callees) = &report.result else {
        panic!("expected callees");
    };
    let rows = callees
        .iter()
        .map(|entry| {
            (
                entry.depth,
                entry.symbol.qualified_name.as_str(),
                entry.cycle,
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(rows, [(1, "pong", false), (2, "ping", true)]);

    let report = fixture.trace(Some("recurse"), None, TraceOptions::default());
    let TraceResult::Callees(callees) = &report.result else {
        panic!("expected callees");
    };
    assert_eq!(callees.len(), 1);
    assert_eq!(callees[0].symbol.qualified_name, "recurse");
    assert!(callees[0].cycle);
    assert_eq!(callees[0].call_site.line, 17);
    assert_eq!(
        path_names(&fixture.trace(Some("recurse"), Some("recurse"), TraceOptions::default())),
        [["recurse", "recurse"]]
    );
}

#[test]
fn test_trace_callers_report_call_sites_totals_and_truncation() {
    let fixture = Fixture::new();
    let report = fixture.trace(None, Some("save"), TraceOptions::default());
    let TraceResult::Callers(callers) = &report.result else {
        panic!("expected callers");
    };
    let rows = callers
        .iter()
        .map(|entry| {
            (
                entry.symbol.qualified_name.as_str(),
                entry.call_site.path.as_str(),
                entry.call_site.line,
                entry.via.as_str(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        rows,
        [
            ("shared", "src/helper.rs", 2, "save"),
            ("middle", "src/lib.rs", 11, "save"),
        ]
    );
    assert_eq!(report.total, 2);
    assert!(!report.truncated);
    assert!(report.complete);
    // Depth 1 found symbols that have callers of their own.
    assert!(report.depth_limited);

    let options = TraceOptions {
        max_depth: Some(3),
        ..TraceOptions::default()
    };
    let report = fixture.trace(None, Some("save"), options);
    let TraceResult::Callers(callers) = &report.result else {
        panic!("expected callers");
    };
    let rows = callers
        .iter()
        .map(|entry| (entry.depth, entry.symbol.qualified_name.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(rows, [(1, "shared"), (1, "middle"), (2, "entry")]);
    assert!(!report.depth_limited);

    let options = TraceOptions {
        max_results: 1,
        ..TraceOptions::default()
    };
    let report = fixture.trace(None, Some("save"), options);
    assert!(report.truncated);
    assert_eq!(report.total, 2);
    assert!(!report.complete);

    let options = TraceOptions {
        max_nodes: 1,
        ..TraceOptions::default()
    };
    let report = fixture.trace(None, Some("save"), options);
    assert!(report.truncated);
}

#[test]
fn test_trace_resolves_self_type_and_python_calls() {
    let fixture = Fixture::new();
    let report = fixture.trace(Some("Store::open"), None, TraceOptions::default());
    let TraceResult::Callees(callees) = &report.result else {
        panic!("expected callees");
    };
    assert_eq!(callees[0].symbol.qualified_name, "Store::init");
    assert_eq!(callees[0].resolution, CallResolution::SameFile);

    let report = fixture.trace(Some("main"), Some("work"), TraceOptions::default());
    assert_eq!(
        path_names(&report),
        [["main", "Service::run", "Service::step", "work"]]
    );
    let TraceResult::Paths(paths) = &report.result else {
        unreachable!()
    };
    let rules = paths[0][1..]
        .iter()
        .map(|hop| hop.resolution)
        .collect::<Vec<_>>();
    assert_eq!(
        rules,
        [
            Some(CallResolution::SameFile),
            Some(CallResolution::SameFile),
            Some(CallResolution::Import),
        ]
    );
    // `Class.method` reads as `Class::method`.
    let report = fixture.trace(Some("Service.step"), None, TraceOptions::default());
    assert!(matches!(report.result, TraceResult::Callees(ref callees) if callees.len() == 1));
}

#[test]
fn test_trace_reports_unknown_names_and_invalid_options() {
    let fixture = Fixture::new();
    let report = fixture.trace(
        Some("no_such_symbol"),
        Some("write"),
        TraceOptions::default(),
    );
    assert_eq!(
        report.result,
        TraceResult::NotFound {
            end: TraceEnd::From,
            name: "no_such_symbol".to_owned(),
        }
    );
    for (from, to, options) in [
        (None, None, TraceOptions::default()),
        (
            Some("entry"),
            None,
            TraceOptions {
                max_depth: Some(9),
                ..TraceOptions::default()
            },
        ),
        (
            Some("entry"),
            None,
            TraceOptions {
                max_paths: 0,
                ..TraceOptions::default()
            },
        ),
        (Some(""), None, TraceOptions::default()),
    ] {
        assert!(trace(&fixture.store, fixture.repo(), from, to, options).is_err());
    }
}

#[test]
fn test_trace_requires_a_reindex_when_the_index_predates_call_sites() {
    let fixture = Fixture::new();
    fixture
        .store
        .stamp_index_version(
            fixture.repo(),
            "0.1.0+atlas-shell-text-v1+rust-imports-v2",
            1,
        )
        .expect("old parser version should be stamped");
    let report = fixture.trace(Some("entry"), Some("write"), TraceOptions::default());
    assert_eq!(report.result, TraceResult::CallGraphMissing);
    assert!(!report.complete);

    Indexer::new(&fixture.store, counter_for(TokenProvider::Heuristic))
        .index(&fixture.root, |_| {})
        .expect("re-index should succeed");
    let report = fixture.trace(Some("entry"), Some("write"), TraceOptions::default());
    assert_eq!(path_names(&report).len(), 1);
}

#[test]
fn test_trace_writes_nothing_to_the_store() {
    let fixture = Fixture::new();
    let before = fixture.store.total_changes().expect("changes");
    fixture.trace(Some("entry"), Some("write"), TraceOptions::default());
    fixture.trace(None, Some("save"), TraceOptions::default());
    fixture.trace(Some("ping"), None, TraceOptions::default());
    assert_eq!(fixture.store.total_changes().expect("changes"), before);
}

#[test]
fn test_trace_labels_a_receiver_call_resolved_only_by_a_unique_name() {
    let fixture = Fixture::new();
    fs::write(
        fixture.root.join("py/other.py"),
        "def drive(service):\n    service.step()\n",
    )
    .expect("fixture file should be written");
    Indexer::new(&fixture.store, counter_for(TokenProvider::Heuristic))
        .index(&fixture.root, |_| {})
        .expect("re-index should succeed");
    let report = fixture.trace(Some("drive"), None, TraceOptions::default());
    let TraceResult::Callees(callees) = &report.result else {
        panic!("expected callees");
    };
    assert_eq!(callees[0].symbol.qualified_name, "Service::step");
    assert_eq!(callees[0].resolution, CallResolution::NameOnly);
}
