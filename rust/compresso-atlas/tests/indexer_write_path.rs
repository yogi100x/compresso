//! The index write path (E17-41, D106): imports are linked once per run and
//! files are committed in bounded batches, with the same final store as a
//! per-file write.

use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use compresso_atlas::Indexer;
use compresso_core::{TokenProvider, counter_for};
use compresso_store::Store;
use tempfile::TempDir;

/// Writes `files` TypeScript files that each import `externals` modules from
/// outside the repository (never resolvable) and the next file in path order
/// (a target that is written later in the same run).
fn write_external_heavy_repository(root: &Path, files: usize, externals: usize) {
    for index in 0..files {
        let mut source = String::new();
        for module in 0..externals {
            source.push_str(&format!(
                "import {{ e{module} }} from 'external-{}/part{module}';\n",
                (index + module) % 97
            ));
        }
        let next = (index + 1) % files;
        source.push_str(&format!(
            "import {{ f{next} }} from './file_{next:05}';\nexport function f{index}(): number {{ return f{next}() + e0(); }}\n"
        ));
        fs::write(root.join(format!("file_{index:05}.ts")), source)
            .expect("generated source should be written");
    }
}

/// Writes the carriers (20 files holding 100,000 imports of modules outside
/// the repository, sorted first) and/or 2,500 small files importing each
/// other, then indexes them into a fresh store; returns the index time.
fn index_carriers_and_small_files(carriers: bool, small_files: bool) -> Duration {
    const CARRIERS: usize = 20;
    const EXTERNALS_PER_CARRIER: usize = 5_000;
    const FILES: usize = 2_500;
    let directory = TempDir::new().expect("temporary directory should be created");
    let root = directory.path().join("repo");
    fs::create_dir_all(&root).expect("repository directory should be created");
    for carrier in (0..CARRIERS).filter(|_| carriers) {
        let mut source = String::new();
        for module in 0..EXTERNALS_PER_CARRIER {
            source.push_str(&format!("import {{ e }} from 'ext-{carrier}-{module}';\n"));
        }
        fs::write(root.join(format!("a_carrier_{carrier:02}.ts")), source)
            .expect("carrier should be written");
    }
    for index in (0..FILES).filter(|_| small_files) {
        let next = (index + 1) % FILES;
        fs::write(
            root.join(format!("file_{index:05}.ts")),
            format!(
                "import {{ f }} from './file_{next:05}';\nexport function f{index}() {{ return f(); }}\n"
            ),
        )
        .expect("generated source should be written");
    }
    let store = Store::open(&directory.path().join("compresso.db")).expect("store should open");

    let started = Instant::now();
    let report = Indexer::new(&store, counter_for(TokenProvider::Heuristic))
        .index(&root, |_| {})
        .expect("index should succeed");
    let elapsed = started.elapsed();

    let carriers = if carriers { CARRIERS } else { 0 };
    let files = if small_files { FILES } else { 0 };
    assert_eq!(report.files_indexed, carriers + files);
    assert_eq!(report.imports_unresolved, carriers * EXTERNALS_PER_CARRIER);
    assert_eq!(report.imports_resolved, files);
    let repo = repo_id(&store, &root);
    assert_eq!(
        store.resolved_edges(repo).expect("edges should load").len(),
        files,
        "every local import, including those written before their target, is linked"
    );
    elapsed
}

#[test]
fn test_full_index_with_many_unresolved_external_imports_finishes_in_bounded_time() {
    // Before D106 every committed file ran one statement that scanned every
    // unresolved edge in the repository: each of 2,500 small files written
    // after carriers holding 100,000 external imports paid one scan of
    // them, so indexing both together took over ten times as long as
    // indexing each alone (51.8 s against about 4 s in a debug build). One
    // link pass per run keeps the sum. The bound is relative to the two
    // parts measured in the same test, so a slower machine scales both
    // sides; 3x leaves room for noise on a loaded runner.
    let carriers_alone = index_carriers_and_small_files(true, false);
    let small_files_alone = index_carriers_and_small_files(false, true);
    let together = index_carriers_and_small_files(true, true);

    let parts = carriers_alone + small_files_alone;
    assert!(
        together < parts * 3,
        "together {together:?}; carriers alone {carriers_alone:?}, small files alone {small_files_alone:?}"
    );
}

fn repo_id(store: &Store, root: &Path) -> compresso_store::RepoId {
    store
        .repo_by_root(&fs::canonicalize(root).expect("root should canonicalize"))
        .expect("repository lookup should succeed")
        .expect("repository should exist")
        .id
}

/// Everything an index stores for a repository that the public API reads,
/// in a deterministic order.
fn snapshot(store: &Store, root: &Path) -> Vec<String> {
    let repo = repo_id(store, root);
    let mut rows = Vec::new();
    let mut hashes = store
        .file_hashes(repo)
        .expect("hashes should load")
        .into_iter()
        .collect::<Vec<_>>();
    hashes.sort_unstable();
    for (path, hash) in hashes {
        let record = store
            .indexed_file(repo, &path)
            .expect("file should load")
            .expect("file should exist");
        rows.push(format!(
            "file {path} {hash} {:?} {} {}",
            record.language, record.size_bytes, record.token_estimate
        ));
        for symbol in store.symbols_in_file(repo, &path).expect("symbols") {
            rows.push(format!(
                "symbol {path} {} {:?} {} {} {:?}",
                symbol.qualified_name,
                symbol.kind,
                symbol.start_line,
                symbol.end_line,
                symbol.signature
            ));
        }
    }
    for edge in store.resolved_edges(repo).expect("resolved edges") {
        rows.push(format!("resolved {edge:?}"));
    }
    for path in store
        .unresolved_edge_sources(repo)
        .expect("unresolved sources")
    {
        rows.push(format!("unresolved-source {path}"));
    }
    rows.push(format!(
        "edge-counts {:?}",
        store.edge_resolution_counts(repo).expect("edge counts")
    ));
    rows.push(format!(
        "call-sites {}",
        store.call_site_count(repo).expect("call site count")
    ));
    rows
}

#[test]
fn test_an_index_run_links_imports_an_interrupted_run_left_pending() {
    // A run killed after committing files but before its link pass leaves
    // imports of later-written files pending. The next run, with nothing to
    // re-index, links them.
    let directory = TempDir::new().expect("temporary directory should be created");
    let root = directory.path().join("repo");
    fs::create_dir_all(&root).expect("repository directory should be created");
    let importer = "import { b } from './b';\nexport function a(): number { return b(); }\n";
    let target = "export function b(): number { return 1; }\n";
    fs::write(root.join("a.ts"), importer).expect("importer should be written");
    fs::write(root.join("b.ts"), target).expect("target should be written");
    let store = Store::open(&directory.path().join("compresso.db")).expect("store should open");
    let fresh = Store::open(&directory.path().join("fresh.db")).expect("store should open");
    let counter = counter_for(TokenProvider::Heuristic);
    Indexer::new(&fresh, counter)
        .index(&root, |_| {})
        .expect("reference index should succeed");

    // Replays what the interrupted run committed: both files, unlinked.
    let repo = store
        .approve_repo(&fs::canonicalize(&root).expect("root should canonicalize"))
        .expect("repository should be approved");
    let fresh_repo = repo_id(&fresh, &root);
    let record = |path: &str| {
        let mut record = fresh
            .indexed_file(fresh_repo, path)
            .expect("file should load")
            .expect("file should exist");
        record.indexed_at = 1;
        record
    };
    let symbols = |path: &str| {
        fresh
            .symbols_in_file(fresh_repo, path)
            .expect("symbols")
            .into_iter()
            .map(|symbol| compresso_store::SymbolRecord {
                name: symbol.name,
                qualified_name: symbol.qualified_name,
                kind: symbol.kind,
                signature: symbol.signature,
                start_line: symbol.start_line,
                end_line: symbol.end_line,
                token_estimate: symbol.token_estimate,
            })
            .collect::<Vec<_>>()
    };
    let (a, b) = (record("a.ts"), record("b.ts"));
    let (a_symbols, b_symbols) = (symbols("a.ts"), symbols("b.ts"));
    let edges = [compresso_store::EdgeRecord {
        to_path: "b.ts".to_owned(),
        kind: compresso_store::EdgeKind::Imports,
        resolution: compresso_store::EdgeResolution::Resolved,
    }];
    let batch = [
        compresso_store::IndexedFileWrite {
            record: &a,
            symbols: &a_symbols,
            edges: &edges,
            calls: &[],
        },
        compresso_store::IndexedFileWrite {
            record: &b,
            symbols: &b_symbols,
            edges: &[],
            calls: &[],
        },
    ];
    store
        .replace_indexed_files(repo, &batch, Duration::from_secs(60))
        .expect("interrupted run's writes");
    // Stored unlinked (no target id), though readers already see it by path.
    assert_eq!(
        store.unresolved_edge_sources(repo).expect("sources"),
        ["a.ts"]
    );
    assert_eq!(store.resolved_edges(repo).expect("edges").len(), 1);

    let report = Indexer::new(&store, counter)
        .index(&root, |_| {})
        .expect("next index should succeed");

    assert_eq!(report.files_indexed, 0, "nothing changed on disk");
    assert_eq!(
        store.resolved_edges(repo).expect("edges"),
        fresh.resolved_edges(fresh_repo).expect("edges")
    );
    assert_eq!(store.resolved_edges(repo).expect("edges").len(), 1);
    assert!(
        store
            .unresolved_edge_sources(repo)
            .expect("sources")
            .is_empty(),
        "the next run's link pass stored the target id"
    );
}

fn write_tree(root: &Path, files: &[(&str, String)]) {
    for (path, body) in files {
        let absolute = root.join(path);
        fs::create_dir_all(absolute.parent().expect("parent")).expect("directory");
        fs::write(absolute, body).expect("source should be written");
    }
}

fn module(index: usize, imports: &[usize]) -> (String, String) {
    let mut body = String::new();
    for target in imports {
        body.push_str(&format!(
            "import {{ f{target} }} from '../m{}/f{target}';\n",
            target % 5
        ));
    }
    body.push_str("import { ext } from 'external-lib';\n");
    body.push_str(&format!(
        "export function f{index}(): number {{ return {}; }}\n",
        imports
            .iter()
            .map(|target| format!("f{target}()"))
            .chain(std::iter::once("ext()".to_owned()))
            .collect::<Vec<_>>()
            .join(" + ")
    ));
    (format!("src/m{}/f{index}.ts", index % 5), body)
}

#[test]
fn test_incremental_runs_end_identical_to_a_fresh_full_index() {
    let directory = TempDir::new().expect("temporary directory should be created");
    let root = directory.path().join("repo");
    let counter = counter_for(TokenProvider::Heuristic);
    let initial = (0..40)
        .map(|index| module(index, &[(index + 1) % 40, (index + 17) % 40]))
        .collect::<Vec<_>>();
    let initial = initial
        .iter()
        .map(|(path, body)| (path.as_str(), body.clone()))
        .collect::<Vec<_>>();
    write_tree(&root, &initial);
    let store = Store::open(&directory.path().join("compresso.db")).expect("store should open");
    Indexer::new(&store, counter)
        .index(&root, |_| {})
        .expect("initial index should succeed");

    // Add files that existing files already import (pending until now),
    // modify some, remove one that others import, and rename another.
    for index in [3, 9] {
        fs::remove_file(root.join(module(index, &[]).0)).expect("file should be removed");
    }
    let (renamed_path, renamed_body) = module(12, &[13]);
    fs::remove_file(root.join(&renamed_path)).expect("file should be removed");
    let added = (40..46)
        .map(|index| module(index, &[index - 39, 12, 3]))
        .chain([
            ("src/renamed/f12.ts".to_owned(), renamed_body),
            module(7, &[41, 44]),
        ])
        .chain((0..3).map(|index| module(index, &[40 + index, 9])))
        .collect::<Vec<_>>();
    let added = added
        .iter()
        .map(|(path, body)| (path.as_str(), body.clone()))
        .collect::<Vec<_>>();
    write_tree(&root, &added);
    // Files that import the not-yet-present 40..45 before they exist.
    Indexer::new(&store, counter)
        .index(&root, |_| {})
        .expect("incremental index should succeed");
    // A removed target comes back.
    write_tree(&root, &[(module(3, &[]).0.as_str(), module(3, &[4]).1)]);
    Indexer::new(&store, counter)
        .index(&root, |_| {})
        .expect("second incremental index should succeed");

    let fresh = Store::open(&directory.path().join("fresh.db")).expect("store should open");
    Indexer::new(&fresh, counter)
        .index(&root, |_| {})
        .expect("fresh index should succeed");
    assert_eq!(snapshot(&store, &root), snapshot(&fresh, &root));
}

#[test]
fn test_a_second_store_reads_and_writes_during_a_full_index_without_lock_errors() {
    // Another server on the same store (D103) reads and writes its ledger
    // while this process indexes; the batched writer must never hold the
    // write lock near the 5 s busy timeout.
    let directory = TempDir::new().expect("temporary directory should be created");
    let root = directory.path().join("repo");
    fs::create_dir_all(&root).expect("repository directory should be created");
    write_external_heavy_repository(&root, 600, 20);
    let database = directory.path().join("compresso.db");
    let store = Store::open(&database).expect("store should open");
    let done = std::sync::atomic::AtomicBool::new(false);

    let (report, longest_wait, operations) = std::thread::scope(|scope| {
        let other = scope.spawn(|| {
            let other = Store::open(&database).expect("second store should open");
            let mut longest = Duration::ZERO;
            let mut operations = 0_usize;
            while !done.load(std::sync::atomic::Ordering::Relaxed) {
                let started = Instant::now();
                let session = other
                    .start_session("test-client", "stdio")
                    .expect("a ledger write must not fail during an index");
                other
                    .close_session(&session)
                    .expect("a ledger write must not fail during an index");
                longest = longest.max(started.elapsed());
                if let Some(repo) = other
                    .repo_by_root(&fs::canonicalize(&root).expect("root"))
                    .expect("a read must not fail during an index")
                {
                    other
                        .file_hashes(repo.id)
                        .expect("a read must not fail during an index");
                }
                operations += 1;
                std::thread::sleep(Duration::from_millis(5));
            }
            (longest, operations)
        });
        let report =
            Indexer::new(&store, counter_for(TokenProvider::Heuristic)).index(&root, |_| {});
        // Stops the other thread before any assertion, so a failure fails
        // the test instead of hanging it.
        done.store(true, std::sync::atomic::Ordering::Relaxed);
        let (longest, operations) = other.join().expect("second store thread");
        (
            report.expect("index should succeed while another store writes"),
            longest,
            operations,
        )
    });

    assert_eq!(report.files_indexed, 600);
    assert!(operations > 0);
    // The writes above already failed the test on any lock error; this
    // only keeps a wait well inside the 5 s busy timeout.
    assert!(
        longest_wait < Duration::from_millis(4_500),
        "a ledger write waited {longest_wait:?}"
    );
}

#[test]
fn test_removing_more_files_than_one_delete_chunk_removes_them_all_in_order() {
    let directory = TempDir::new().expect("temporary directory should be created");
    let root = directory.path().join("repo");
    fs::create_dir_all(&root).expect("repository directory should be created");
    for index in 0..150 {
        fs::write(
            root.join(format!("f{index:03}.ts")),
            format!(
                "import {{ g }} from './f{:03}';\nexport function f{index}() {{ return g(); }}\n",
                (index + 1) % 150
            ),
        )
        .expect("source should be written");
    }
    let store = Store::open(&directory.path().join("compresso.db")).expect("store should open");
    let indexer = Indexer::new(&store, counter_for(TokenProvider::Heuristic));
    indexer
        .index(&root, |_| {})
        .expect("initial index should succeed");
    for index in 0..140 {
        fs::remove_file(root.join(format!("f{index:03}.ts"))).expect("file should be removed");
    }

    let mut removed = Vec::new();
    let report = indexer
        .index(&root, |event| {
            if let compresso_atlas::IndexProgress::Removed { path } = event {
                removed.push(path);
            }
        })
        .expect("index should succeed");

    assert_eq!(report.files_removed, 140);
    let expected = (0..140)
        .map(|index| format!("f{index:03}.ts"))
        .collect::<Vec<_>>();
    assert_eq!(removed, expected);
    let repo = repo_id(&store, &root);
    assert_eq!(store.file_hashes(repo).expect("hashes").len(), 10);
    // f149 imports f000, which is gone; nothing stays linked to a removed file.
    assert!(
        store
            .resolved_edges(repo)
            .expect("edges")
            .iter()
            .all(|edge| !expected.contains(&edge.to_path))
    );
}

/// Whole-package import records (D105) and every resolved edge, sorted.
fn package_state(store: &Store, root: &Path) -> (Vec<(String, String)>, Vec<String>) {
    let repo = repo_id(store, root);
    let mut records = store
        .unresolved_edge_targets_with_detail(repo, compresso_store::EDGE_DETAIL_PACKAGE)
        .expect("package records");
    records.sort();
    let edges = store
        .resolved_edges(repo)
        .expect("resolved edges")
        .into_iter()
        .map(|edge| format!("{edge:?}"))
        .collect();
    (records, edges)
}

#[test]
fn test_package_import_records_are_never_linked_as_file_edges() {
    // D105 stores a Java or C# whole-package import as one `unresolved`
    // record with detail `package` whose `to_path` is the package name.
    // The link pass (D106) must leave it alone even when a file exists at
    // a path equal to that name, in full, incremental, and repaired runs.
    let directory = TempDir::new().expect("temporary directory should be created");
    let root = directory.path().join("repo");
    write_tree(
        &root,
        &[
            (
                "src/com/acme/util/Strings.java",
                "package com.acme.util;\npublic class Strings {\n    public static String trim(String s) { return s; }\n}\n".to_owned(),
            ),
            (
                "src/com/acme/app/Main.java",
                "package com.acme.app;\nimport com.acme.util.*;\npublic class Main {\n    public static void main(String[] args) { Strings.trim(\"x\"); }\n}\n".to_owned(),
            ),
            (
                "Util/Text.cs",
                "namespace Acme.Util {\n    public class Text { public static string Trim(string s) { return s; } }\n}\n".to_owned(),
            ),
            (
                "App/Program.cs",
                "using Acme.Util;\nnamespace Acme.App {\n    public class Program { public static void Main() { Text.Trim(\"x\"); } }\n}\n".to_owned(),
            ),
            // Files whose paths equal the package and namespace names.
            ("com.acme.util", "not a package\n".to_owned()),
            ("Acme.Util", "not a namespace\n".to_owned()),
        ],
    );
    let store = Store::open(&directory.path().join("compresso.db")).expect("store should open");
    let indexer = Indexer::new(&store, counter_for(TokenProvider::Heuristic));
    indexer.index(&root, |_| {}).expect("full index");
    let repo = repo_id(&store, &root);
    let hashes = store.file_hashes(repo).expect("hashes");
    assert!(hashes.contains_key("com.acme.util") && hashes.contains_key("Acme.Util"));
    let full = package_state(&store, &root);
    assert_eq!(
        full.0
            .iter()
            .map(|(_, raw)| raw.as_str())
            .collect::<Vec<_>>(),
        ["Acme.Util", "com.acme.util.*"],
        "{full:?}"
    );
    assert!(
        full.1
            .iter()
            .all(|edge| !edge.contains("\"com.acme.util\"") && !edge.contains("\"Acme.Util\"")),
        "{full:?}"
    );

    // Incremental: an added file and a changed importer.
    write_tree(
        &root,
        &[
            ("notes.md", "# Notes\n".to_owned()),
            (
                "src/com/acme/app/Main.java",
                "package com.acme.app;\nimport com.acme.util.*;\npublic class Main {\n    public static void main(String[] args) { Strings.trim(\"y\"); }\n}\n".to_owned(),
            ),
        ],
    );
    indexer.index(&root, |_| {}).expect("incremental index");
    assert_eq!(package_state(&store, &root), full);

    // A repair pass (what the next run does after an interrupted one) and a
    // no-op run change nothing.
    assert_eq!(store.link_pending_edges(repo).expect("link"), 0);
    indexer.index(&root, |_| {}).expect("no-op index");
    assert_eq!(package_state(&store, &root), full);
    assert_eq!(
        store.edge_resolution_counts(repo).expect("counts").pending,
        0
    );
}
