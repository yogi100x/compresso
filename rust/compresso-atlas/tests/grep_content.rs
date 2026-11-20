use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use compresso_atlas::{AtlasError, GrepError, GrepOptions, GrepReport, grep};
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

    fn write_indexed(&self, path: &str, source: &str, symbols: &[SymbolRecord]) {
        self.write_indexed_bytes(path, source.as_bytes(), symbols);
    }

    fn write_indexed_bytes(&self, path: &str, source: &[u8], symbols: &[SymbolRecord]) {
        let destination = self.root.join(path);
        fs::create_dir_all(destination.parent().expect("parent")).expect("directory created");
        fs::write(&destination, source).expect("source written");
        self.store
            .replace_indexed_file(
                self.repo,
                &FileRecord {
                    path: path.to_owned(),
                    hash: blake3::hash(source).to_hex().to_string(),
                    language: Some(FileLanguage::Rust),
                    size_bytes: source.len() as u64,
                    token_estimate: source.len() / 4,
                    indexed_at: 10,
                },
                symbols,
                &[],
            )
            .expect("file indexes");
    }

    fn grep(&self, patterns: &[&str], configure: impl FnOnce(&mut Options)) -> GrepReport {
        self.try_grep(patterns, configure).expect("grep succeeds")
    }

    fn try_grep(
        &self,
        patterns: &[&str],
        configure: impl FnOnce(&mut Options),
    ) -> Result<GrepReport, AtlasError> {
        let patterns = patterns.iter().map(|p| (*p).to_owned()).collect::<Vec<_>>();
        let mut options = Options {
            regex: false,
            case_insensitive: false,
            path: None,
            max_files: 20,
            max_matches_per_file: 5,
            context_lines: 0,
            scopes: Vec::new(),
            select_rows: false,
        };
        configure(&mut options);
        grep(
            &self.store,
            self.repo,
            &GrepOptions {
                patterns: &patterns,
                regex: options.regex,
                case_insensitive: options.case_insensitive,
                path: options.path.as_deref(),
                max_files: options.max_files,
                max_matches_per_file: options.max_matches_per_file,
                context_lines: options.context_lines,
                scopes: &options.scopes,
                select_rows: options.select_rows,
            },
        )
    }
}

struct Options {
    regex: bool,
    case_insensitive: bool,
    path: Option<String>,
    max_files: usize,
    max_matches_per_file: usize,
    context_lines: usize,
    /// Per-pattern scopes (E17-36); empty when no pattern has one.
    scopes: Vec<Option<String>>,
    /// Row selection under the per-file cap (E17-39).
    select_rows: bool,
}

fn symbol(name: &str, kind: SymbolKind, start_line: u32, end_line: u32) -> SymbolRecord {
    SymbolRecord {
        name: name.rsplit("::").next().unwrap_or(name).to_owned(),
        qualified_name: name.to_owned(),
        kind,
        signature: None,
        start_line,
        end_line,
        token_estimate: 10,
    }
}

fn paths(report: &GrepReport, pattern: usize) -> Vec<&str> {
    report.results[pattern]
        .files
        .iter()
        .map(|file| file.path.as_str())
        .collect()
}

fn seed_secret_use_sites(fixture: &Fixture) {
    fixture.write_indexed(
        "src/scanner.rs",
        "pub fn is_secret_path(path: &Path) -> bool {\n    path.ends_with(\".env\")\n}\n",
        &[symbol("is_secret_path", SymbolKind::Function, 1, 3)],
    );
    fixture.write_indexed(
        "src/queries.rs",
        "fn resolve(path: &Path) {\n    if is_secret_path(path) {\n        return;\n    }\n}\n\nfn scan() {\n    let skip = is_secret_path(p);\n}\n",
        &[
            symbol("resolve", SymbolKind::Function, 1, 5),
            symbol("scan", SymbolKind::Function, 7, 9),
        ],
    );
    fixture.write_indexed(
        "tests/scanner_test.rs",
        "fn test_secret() {\n    assert!(is_secret_path(p));\n}\n",
        &[symbol("test_secret", SymbolKind::Function, 1, 3)],
    );
}

#[test]
fn test_grep_returns_use_sites_with_enclosing_symbols_grouped_and_ranked() {
    let fixture = Fixture::new();
    seed_secret_use_sites(&fixture);

    let report = fixture.grep(&["is_secret_path", "fn scan"], |_| {});
    let repeated = fixture.grep(&["is_secret_path", "fn scan"], |_| {});

    assert_eq!(report, repeated, "a search is deterministic");
    let uses = &report.results[0];
    assert_eq!(uses.pattern, "is_secret_path");
    assert_eq!(uses.matches_total, 4);
    assert_eq!(uses.files_total, 3);
    // Definition first, then non-test, then test code.
    assert_eq!(
        paths(&report, 0),
        ["src/scanner.rs", "src/queries.rs", "tests/scanner_test.rs"]
    );
    let definition = &uses.files[0].lines[0];
    assert_eq!(definition.line, 1);
    assert!(definition.definition);
    let queries = &uses.files[1];
    assert_eq!(
        queries
            .lines
            .iter()
            .map(|line| (line.line, line.symbol.as_deref(), line.definition))
            .collect::<Vec<_>>(),
        [(2, Some("resolve"), false), (8, Some("scan"), false)]
    );
    assert_eq!(queries.lines[0].snippet, "if is_secret_path(path) {");
    assert_eq!(paths(&report, 1), ["src/queries.rs"]);
    assert!(report.complete());
}

/// E17-36 (D101): a pattern's own scope narrows that pattern only; the
/// other patterns of the call keep the call's scope, and a file no pattern
/// may search is neither counted in scope nor read.
#[test]
fn test_grep_pattern_scope_narrows_only_its_own_pattern() {
    let fixture = Fixture::new();
    seed_secret_use_sites(&fixture);

    let report = fixture.grep(&["is_secret_path", "fn scan"], |options| {
        options.scopes = vec![Some("tests".to_owned()), None];
    });
    assert_eq!(paths(&report, 0), ["tests/scanner_test.rs"]);
    assert_eq!(
        (
            report.results[0].matches_total,
            report.results[0].files_total
        ),
        (1, 1)
    );
    assert_eq!(paths(&report, 1), ["src/queries.rs"]);
    assert_eq!(report.files_in_scope, 3);

    let scoped = fixture.grep(&["is_secret_path", "fn scan"], |options| {
        options.scopes = vec![
            Some("src/scanner.rs".to_owned()),
            Some("src/*.rs".to_owned()),
        ];
    });
    assert_eq!(paths(&scoped, 0), ["src/scanner.rs"]);
    assert_eq!(paths(&scoped, 1), ["src/queries.rs"]);
    assert_eq!(scoped.files_in_scope, 2, "tests/ is in no pattern's scope");

    // The call's own path still applies to every pattern.
    let both = fixture.grep(&["is_secret_path"], |options| {
        options.path = Some("src".to_owned());
        options.scopes = vec![Some("**/*_test.rs".to_owned())];
    });
    assert!(both.results[0].files.is_empty());

    for (scopes, error) in [
        (vec![Some("../etc".to_owned())], "\"../etc\" is invalid"),
        (vec![None, None], "one scope per pattern"),
    ] {
        let failure = fixture
            .try_grep(&["is_secret_path"], |options| options.scopes = scopes)
            .expect_err("rejected");
        assert!(failure.to_string().contains(error), "{failure}");
    }
}

#[test]
fn test_grep_never_reads_secret_paths() {
    let fixture = Fixture::new();
    seed_secret_use_sites(&fixture);
    fixture.write_indexed(
        "config/.env.rs",
        "const TOKEN: &str = \"is_secret_path\";\n",
        &[],
    );
    fixture.write_indexed("keys/id_rsa", "is_secret_path\n", &[]);

    let report = fixture.grep(&["is_secret_path"], |_| {});
    let globbed = fixture.grep(&["is_secret_path"], |options| {
        options.path = Some("**/.env*".to_owned());
    });

    assert!(
        paths(&report, 0)
            .iter()
            .all(|path| !path.contains(".env") && !path.contains("id_rsa")),
        "secret paths must never be searched: {:?}",
        paths(&report, 0)
    );
    assert_eq!(report.files_in_scope, 3);
    assert_eq!(globbed.files_in_scope, 0);
    assert!(globbed.results[0].files.is_empty());
}

#[test]
fn test_grep_skips_files_above_the_read_ceiling() {
    let fixture = Fixture::new();
    seed_secret_use_sites(&fixture);
    let mut oversize = b"is_secret_path\n".to_vec();
    oversize.resize(16 * 1024 * 1024 + 1, b'x');
    fixture.write_indexed_bytes("generated/huge.rs", &oversize, &[]);

    let report = fixture.grep(&["is_secret_path"], |_| {});

    assert_eq!(report.files_skipped_oversize, 1);
    assert!(!paths(&report, 0).contains(&"generated/huge.rs"));
    assert_eq!(report.results[0].files_total, 3);
    // A skipped file may hold matches: the report never claims completeness.
    assert!(!report.output_truncated && !report.scan_truncated);
    assert!(!report.complete());
}

#[test]
fn test_grep_reports_caps_and_totals_when_output_is_truncated() {
    let fixture = Fixture::new();
    for index in 0..6 {
        let body = "    retry_budget += 1;\n".repeat(4);
        fixture.write_indexed(
            &format!("src/step_{index}.rs"),
            &format!("fn step() {{\n{body}}}\n"),
            &[symbol("step", SymbolKind::Function, 1, 6)],
        );
    }

    let report = fixture.grep(&["retry_budget"], |options| {
        options.max_files = 2;
        options.max_matches_per_file = 3;
    });

    let result = &report.results[0];
    assert_eq!(
        result.matches_total, 24,
        "the total counts unlisted matches"
    );
    assert_eq!(result.files_total, 6);
    assert_eq!(result.files.len(), 2);
    assert!(result.files.iter().all(|file| file.lines.len() == 3));
    assert!(result.files.iter().all(|file| file.matches_total == 4));
    assert!(report.output_truncated);
    assert!(!report.scan_truncated);
    assert!(!report.complete());

    let counter = counter_for(TokenProvider::OpenAiO200k);
    let full = fixture.grep(&["retry_budget"], |_| {});
    let fitted = full.fit_to_budget(40, counter);
    assert!(counter.count(&fitted.render_text()) <= 40);
    assert!(fitted.output_truncated);
    assert_eq!(fitted.results[0].matches_total, 24);
    assert!(fitted.results[0].lines_listed() < full.results[0].lines_listed());
}

#[test]
fn test_grep_stays_linear_on_one_very_long_line() {
    let fixture = Fixture::new();
    // One 4 MiB line holding the word many times, then a pattern that a
    // backtracking engine would take exponential time on.
    let long_line = "aaaa sigterm ".repeat(320_000);
    fixture.write_indexed(
        "src/long.rs",
        &format!("// {long_line}\nfn drain() {{ sigterm(); }}\n"),
        &[symbol("drain", SymbolKind::Function, 2, 2)],
    );

    let started = Instant::now();
    let literal = fixture.grep(&["sigterm"], |_| {});
    let pathological = fixture.grep(&["(a+)+$", "(a|aa)*b"], |options| options.regex = true);
    let elapsed = started.elapsed();

    assert!(
        elapsed < Duration::from_secs(5),
        "searching one long line took {elapsed:?}; matching must be linear"
    );
    let file = &literal.results[0].files[0];
    assert_eq!(
        file.matches_total, 2,
        "one match per line, however many on it"
    );
    assert_eq!(file.lines[0].line, 1);
    assert!(file.lines[0].snippet.len() < 200, "a long line is cut");
    assert_eq!(file.lines[1].symbol.as_deref(), Some("drain"));
    assert_eq!(pathological.results.len(), 2);
}

#[test]
fn test_grep_regex_never_runs_past_a_line_end() {
    let fixture = Fixture::new();
    // 50,000 short lines: a pattern that could match across line breaks
    // must not rescan the rest of the file from every line.
    let source = "let value = compute();\r\n".repeat(50_000);
    fixture.write_indexed("src/many.rs", &source, &[]);

    let started = Instant::now();
    let report = fixture.grep(
        &["(?s).+", "[\\s\\S]*compute", "compute\\(\\);$"],
        |options| {
            options.regex = true;
        },
    );
    let elapsed = started.elapsed();

    assert!(
        elapsed < Duration::from_secs(5),
        "a cross-line pattern took {elapsed:?}; each line must be searched once"
    );
    for result in &report.results {
        assert_eq!(result.matches_total, 50_000, "{}", result.pattern);
    }
    assert_eq!(
        report.results[1].files[0].lines[1].snippet,
        "let value = compute();"
    );
}

#[test]
fn test_grep_rejects_invalid_patterns_with_a_typed_error() {
    let fixture = Fixture::new();
    seed_secret_use_sites(&fixture);

    let cases: [(&[&str], bool); 6] = [
        (&[], false),
        (&[""], false),
        (&["ok", "("], true),
        (&["a\nb"], false),
        (&["x"; 9], false),
        (&["(?<!x)y"], true),
    ];
    for (patterns, regex) in cases {
        let error = fixture
            .try_grep(patterns, |options| options.regex = regex)
            .expect_err("invalid pattern rejected");
        assert!(
            matches!(error, AtlasError::Grep(_)),
            "{patterns:?} must fail with a grep error, got {error:?}"
        );
    }
    let long = "x".repeat(513);
    assert!(matches!(
        fixture.try_grep(&[long.as_str()], |_| {}),
        Err(AtlasError::Grep(GrepError::PatternTooLong { index: 0, .. }))
    ));
    assert!(matches!(
        fixture.try_grep(&["ok", "("], |options| options.regex = true),
        Err(AtlasError::Grep(GrepError::InvalidPattern { index: 1, .. }))
    ));
    // The same text is a valid literal.
    assert!(fixture.try_grep(&["("], |_| {}).is_ok());
    assert!(matches!(
        fixture.try_grep(&["x"], |options| options.path =
            Some("../outside".to_owned())),
        Err(AtlasError::Grep(GrepError::InvalidPathFilter { .. }))
    ));
    assert!(matches!(
        fixture.try_grep(&["x"], |options| options.max_files = 0),
        Err(AtlasError::Grep(GrepError::InvalidLimit { .. }))
    ));
}

#[test]
fn test_grep_case_flag() {
    let fixture = Fixture::new();
    fixture.write_indexed(
        "src/log.rs",
        "fn a() { warn!(\"Retry\"); }\nfn b() { warn!(\"retry\"); }\nfn c() { warn!(\"RETRY\"); }\n",
        &[],
    );

    let sensitive = fixture.grep(&["retry"], |_| {});
    let insensitive = fixture.grep(&["retry"], |options| options.case_insensitive = true);

    assert_eq!(sensitive.results[0].matches_total, 1);
    assert_eq!(sensitive.results[0].files[0].lines[0].line, 2);
    assert_eq!(insensitive.results[0].matches_total, 3);
}

#[test]
fn test_grep_path_filter_prefix_and_glob() {
    let fixture = Fixture::new();
    seed_secret_use_sites(&fixture);
    fixture.write_indexed("src/nested/deep.rs", "fn d() { is_secret_path(x); }\n", &[]);

    let prefix = fixture.grep(&["is_secret_path"], |options| {
        options.path = Some("src".to_owned());
    });
    let glob = fixture.grep(&["is_secret_path"], |options| {
        options.path = Some("src/**/*.rs".to_owned());
    });
    let basename = fixture.grep(&["is_secret_path"], |options| {
        options.path = Some("*_test.rs".to_owned());
    });
    let one_file = fixture.grep(&["is_secret_path"], |options| {
        options.path = Some("src/queries.rs".to_owned());
    });

    let mut prefix_paths = paths(&prefix, 0);
    prefix_paths.sort_unstable();
    assert_eq!(
        prefix_paths,
        ["src/nested/deep.rs", "src/queries.rs", "src/scanner.rs"]
    );
    assert_eq!(prefix.files_in_scope, 3);
    assert_eq!(glob.results[0].files_total, 3);
    assert_eq!(paths(&basename, 0), ["tests/scanner_test.rs"]);
    assert_eq!(paths(&one_file, 0), ["src/queries.rs"]);
}

#[test]
fn test_grep_searches_changed_files_as_they_are_now_without_symbols() {
    let fixture = Fixture::new();
    seed_secret_use_sites(&fixture);
    fs::write(
        fixture.root.join("src/queries.rs"),
        "// moved\n\nfn resolve(path: &Path) {\n    if is_secret_path(path) {}\n}\n",
    )
    .expect("file rewritten");

    let report = fixture.grep(&["is_secret_path"], |_| {});

    assert_eq!(report.stale_files, 1);
    let changed = report.results[0]
        .files
        .iter()
        .find(|file| file.path == "src/queries.rs")
        .expect("changed file is still searched");
    assert!(changed.stale);
    assert_eq!(
        changed.lines[0].line, 4,
        "line numbers follow the current file"
    );
    assert_eq!(
        changed.lines[0].symbol, None,
        "stored symbols describe other bytes"
    );
}

#[test]
fn test_grep_skips_missing_and_non_utf8_files() {
    let fixture = Fixture::new();
    seed_secret_use_sites(&fixture);
    fixture.write_indexed_bytes("src/binary.rs", b"is_secret_path\xff\xfe\n", &[]);
    fs::remove_file(fixture.root.join("src/scanner.rs")).expect("file removed");

    let report = fixture.grep(&["is_secret_path"], |_| {});

    assert_eq!(report.files_skipped_unreadable, 2);
    assert_eq!(
        paths(&report, 0),
        ["src/queries.rs", "tests/scanner_test.rs"]
    );
    assert!(!report.complete());
}

#[test]
fn test_grep_keeps_only_the_files_that_rank_and_matches_sort_then_truncate() {
    let fixture = Fixture::new();
    // Files of varied match counts, test and non-test paths, and one that
    // defines a symbol on a matching line, so every rank term matters.
    for index in 0..40 {
        let matches = 1 + (index * 7) % 5;
        let path = if index % 3 == 0 {
            format!("tests/case_{index:02}.rs")
        } else {
            format!("src/unit_{index:02}.rs")
        };
        let body = "    marker_value();\n".repeat(matches);
        let symbols = if index == 37 {
            vec![symbol("marker_value", SymbolKind::Function, 2, 2)]
        } else {
            Vec::new()
        };
        fixture.write_indexed(&path, &format!("fn f() {{\n{body}}}\n"), &symbols);
    }
    let everything = fixture.grep(&["marker_value"], |options| options.max_files = 100);
    let ranked = paths(&everything, 0)
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    assert_eq!(ranked.len(), 40);
    assert_eq!(ranked[0], "src/unit_37.rs", "a defining file ranks first");
    for max_files in [1, 3, 7, 39] {
        let report = fixture.grep(&["marker_value"], |options| options.max_files = max_files);
        assert_eq!(paths(&report, 0), ranked[..max_files].to_vec());
        assert_eq!(report.results[0].files_total, 40);
        assert_eq!(
            report.results[0].matches_total,
            everything.results[0].matches_total
        );
        assert!(report.output_truncated);
    }
}

#[test]
fn test_grep_context_lines_surround_each_match_and_merge_when_overlapping() {
    let fixture = Fixture::new();
    let source = (1..=20)
        .map(|line| match line {
            5 | 7 | 15 => format!("    hit_{line}(); // needle"),
            _ => format!("    line_{line}();"),
        })
        .collect::<Vec<_>>()
        .join("\n");
    fixture.write_indexed("src/context.rs", &format!("{source}\n"), &[]);

    let plain = fixture.grep(&["needle"], |_| {});
    assert!(
        plain.results[0].files[0]
            .lines
            .iter()
            .all(|line| line.before.is_empty() && line.after.is_empty())
    );

    let report = fixture.grep(&["needle"], |options| options.context_lines = 2);
    let lines = &report.results[0].files[0].lines;
    let numbers = |context: &[compresso_atlas::GrepContextLine]| {
        context.iter().map(|line| line.line).collect::<Vec<_>>()
    };
    assert_eq!(lines.len(), 3);
    assert_eq!(numbers(&lines[0].before), [3, 4]);
    assert_eq!(numbers(&lines[0].after), [6], "stops before the next match");
    assert_eq!(numbers(&lines[1].before), Vec::<u32>::new(), "6 was shown");
    assert_eq!(numbers(&lines[1].after), [8, 9]);
    assert_eq!(numbers(&lines[2].before), [13, 14]);
    assert_eq!(numbers(&lines[2].after), [16, 17]);
    assert_eq!(lines[0].before[0].text, "line_3();");

    let rendered = report.render_text();
    let shown = rendered
        .lines()
        .skip(2)
        .filter_map(|row| {
            row.trim_start()
                .split([' ', '-'])
                .next()?
                .parse::<u32>()
                .ok()
        })
        .collect::<Vec<_>>();
    assert_eq!(shown, [3, 4, 5, 6, 7, 8, 9, 13, 14, 15, 16, 17]);

    // Context at the file edges stops at the first and last line.
    let edge = Fixture::new();
    edge.write_indexed("src/edge.rs", "needle_a\nmiddle\nneedle_b", &[]);
    let report = edge.grep(&["needle"], |options| options.context_lines = 10);
    let lines = &report.results[0].files[0].lines;
    assert!(lines[0].before.is_empty());
    assert_eq!(numbers(&lines[0].after), [2]);
    assert!(lines[1].before.is_empty() && lines[1].after.is_empty());

    // Context counts against the token budget as one unit with its match.
    let counter = counter_for(TokenProvider::OpenAiO200k);
    let wide = fixture.grep(&["needle"], |options| options.context_lines = 10);
    let fitted = wide.fit_to_budget(60, counter);
    assert!(counter.count(&fitted.render_text()) <= 60);
    assert!(fitted.output_truncated);

    assert!(matches!(
        fixture.try_grep(&["needle"], |options| options.context_lines = 11),
        Err(AtlasError::Grep(GrepError::InvalidContext { max: 10 }))
    ));
}

/// E17-36 (D101): an ordinary pattern with counted repetitions of a Unicode
/// class compiles; one that is still too large says how to shorten it.
#[test]
fn test_grep_accepts_ordinary_counted_repetitions_and_explains_the_size_bound() {
    let fixture = Fixture::new();
    fixture.write_indexed(
        "src/pool.rs",
        "// allows up to 256 connections\nconst MAX: u32 = 256; // conns\n",
        &[],
    );
    let report = fixture.grep(
        &[r"(?i)(<=|up to|at most|max(imum)?|limit|bound)\D{0,30}256\b|256\D{0,20}(connections|conns)"],
        |options| options.regex = true,
    );
    assert_eq!(report.results[0].matches_total, 2);
    let failure = fixture
        .try_grep(&[r"\w{0,1000}x"], |options| options.regex = true)
        .expect_err("still bounded");
    let message = failure.to_string();
    assert!(message.contains("too large"), "{message}");
    assert!(message.contains("shorten counted repetitions"), "{message}");
}
