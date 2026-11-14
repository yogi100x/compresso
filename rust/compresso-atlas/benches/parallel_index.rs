//! Serial versus parallel repository scan and first index.
//!
//! The corpus is generated into a temporary directory so the comparison is
//! repeatable and independent of the checkout. Both variants produce the same
//! index; only the worker count differs.

use std::fmt::Write as _;
use std::fs;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::time::Duration;

use compresso_atlas::{Concurrency, Indexer, ScanOptions, scan_with_concurrency};
use compresso_core::{TokenProvider, counter_for};
use compresso_store::Store;
use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use tempfile::TempDir;

const DIRECTORIES: usize = 40;
const FILES_PER_DIRECTORY: usize = 30;
const FUNCTIONS_PER_FILE: usize = 40;

fn rust_source(seed: usize) -> String {
    let mut source = String::from("use crate::shared::Context;\n\n");
    for function in 0..FUNCTIONS_PER_FILE {
        let _ = write!(
            source,
            "/// Computes value {function} for module {seed}.\npub fn compute_{seed}_{function}(context: &Context, input: u64) -> u64 {{\n    let scaled = input.wrapping_mul({function} + 1);\n    if scaled % 2 == 0 {{\n        scaled + context.offset()\n    }} else {{\n        scaled.saturating_sub(context.offset())\n    }}\n}}\n\n"
        );
    }
    source
}

fn typescript_source(seed: usize) -> String {
    let mut source = String::from("import { Context } from './shared';\n\n");
    for function in 0..FUNCTIONS_PER_FILE {
        let _ = write!(
            source,
            "export function compute{seed}x{function}(context: Context, input: number): number {{\n  const scaled = input * ({function} + 1);\n  return scaled % 2 === 0 ? scaled + context.offset : scaled - context.offset;\n}}\n\n"
        );
    }
    source
}

fn python_source(seed: usize) -> String {
    let mut source = String::from("from .shared import Context\n\n");
    for function in 0..FUNCTIONS_PER_FILE {
        let _ = write!(
            source,
            "def compute_{seed}_{function}(context: Context, value: int) -> int:\n    scaled = value * ({function} + 1)\n    if scaled % 2 == 0:\n        return scaled + context.offset\n    return scaled - context.offset\n\n\n"
        );
    }
    source
}

fn write_corpus(root: &Path) {
    for directory in 0..DIRECTORIES {
        let path = root.join(format!("src/module_{directory:02}"));
        fs::create_dir_all(&path).expect("benchmark directory should be created");
        for file in 0..FILES_PER_DIRECTORY {
            let seed = directory * FILES_PER_DIRECTORY + file;
            let (name, source) = match file % 3 {
                0 => (format!("unit_{file:02}.rs"), rust_source(seed)),
                1 => (format!("unit_{file:02}.ts"), typescript_source(seed)),
                _ => (format!("unit_{file:02}.py"), python_source(seed)),
            };
            fs::write(path.join(name), source).expect("benchmark source should be written");
        }
    }
}

struct Corpus {
    _directory: TempDir,
    root: PathBuf,
}

fn corpus() -> Corpus {
    let directory = tempfile::tempdir().expect("benchmark directory should be created");
    let root = directory.path().join("repo");
    write_corpus(&root);
    let root = fs::canonicalize(root).expect("benchmark root should canonicalize");
    Corpus {
        _directory: directory,
        root,
    }
}

fn fresh_store() -> (TempDir, Store) {
    let directory = tempfile::tempdir().expect("store directory should be created");
    let store = Store::open(&directory.path().join("compresso.db")).expect("store should open");
    (directory, store)
}

fn bench_scan(criterion: &mut Criterion) {
    let corpus = corpus();
    let (_store_directory, store) = fresh_store();
    let repo = store
        .approve_repo(&corpus.root)
        .expect("benchmark root should be approved");
    let mut group = criterion.benchmark_group("atlas_scan_1200_files");
    group
        .sample_size(20)
        .measurement_time(Duration::from_secs(8));
    for (name, concurrency) in [
        ("serial", Concurrency::serial()),
        ("parallel", Concurrency::default()),
    ] {
        group.bench_function(name, |bencher| {
            bencher.iter(|| {
                black_box(
                    scan_with_concurrency(
                        &store,
                        repo,
                        &corpus.root,
                        ScanOptions::default(),
                        concurrency,
                    )
                    .expect("benchmark scan should succeed"),
                )
            });
        });
    }
    group.finish();
}

fn bench_index(criterion: &mut Criterion) {
    let corpus = corpus();
    let counter = counter_for(TokenProvider::OpenAiO200k);
    let mut group = criterion.benchmark_group("atlas_first_index_1200_files");
    group
        .sample_size(10)
        .measurement_time(Duration::from_secs(30));
    for (name, concurrency) in [
        ("serial", Concurrency::serial()),
        ("parallel", Concurrency::default()),
    ] {
        group.bench_function(name, |bencher| {
            bencher.iter_batched(
                fresh_store,
                |(directory, store)| {
                    let report = Indexer::new(&store, counter)
                        .with_concurrency(concurrency)
                        .index(&corpus.root, |_| {})
                        .expect("benchmark index should succeed");
                    black_box(report);
                    (directory, store)
                },
                BatchSize::PerIteration,
            );
        });
    }
    group.finish();
}

criterion_group!(benches, bench_scan, bench_index);
criterion_main!(benches);
