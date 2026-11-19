//! Gates, verification, and index freshness of the opt-in edit operations
//! (E17-32). Rollback and post-validation swaps are covered by the unit tests
//! in `src/edit.rs`, which can inject a failure between validation and write.

use std::fs;
use std::path::{Path, PathBuf};

use compresso_atlas::{
    EDIT_MAX_OPERATIONS, EDIT_MAX_SITES, EDIT_MAX_TEXT_BYTES, EditBound, EditError, EditOperation,
    EditReport, EditRequest, IndexState, Indexer, InsertPosition, LineEnding, Refusal, apply_edits,
    search,
};
use compresso_core::{TokenProvider, counter_for};
use compresso_store::Store;
use tempfile::TempDir;

const DECISIONS: &str = "/// Formats a timestamp; see format_utc_now for the clock.
pub fn format_utc(seconds: i64) -> String {
    seconds.to_string()
}

pub fn format_utc_now() -> String {
    format_utc(0)
}

pub fn reformat_utc(value: &str) -> String {
    let parsed = format_utc(1);
    format!(\"{value}{parsed}\")
}

struct Holder {
    format_utc2: u8,
}

fn uses(x: Holder) -> u8 {
    let label = \"call format_utc_v2 here\";
    let _ = (format_utc(2), label);
    x.format_utc2
}
";

struct Fixture {
    _directory: TempDir,
    root: PathBuf,
    store: Store,
}

impl Fixture {
    fn new(files: &[(&str, &[u8])]) -> Self {
        let directory = TempDir::new().expect("temporary directory");
        let root = fs::canonicalize(directory.path())
            .expect("canonical temp")
            .join("repo");
        fs::create_dir_all(&root).expect("repo");
        for (path, contents) in files {
            write(&root, path, contents);
        }
        let store = Store::open(&directory.path().join("store.db")).expect("store");
        let fixture = Self {
            _directory: directory,
            root,
            store,
        };
        fixture.index();
        fixture
    }

    fn index(&self) {
        Indexer::new(&self.store, counter_for(TokenProvider::OpenAiO200k))
            .index(&self.root, |_| {})
            .expect("index");
    }

    fn request(&self, operations: Vec<EditOperation>, dry_run: bool) -> EditRequest {
        EditRequest {
            operations,
            dry_run,
            protected: Vec::new(),
        }
    }

    fn apply(&self, operations: Vec<EditOperation>) -> Result<EditReport, EditError> {
        self.apply_request(&self.request(operations, false))
    }

    fn apply_request(&self, request: &EditRequest) -> Result<EditReport, EditError> {
        apply_edits(
            &self.store,
            &self.root,
            request,
            counter_for(TokenProvider::OpenAiO200k),
        )
    }

    fn read(&self, path: &str) -> Vec<u8> {
        fs::read(self.root.join(path)).expect("file")
    }

    fn text(&self, path: &str) -> String {
        String::from_utf8(self.read(path)).expect("utf-8")
    }

    fn repo(&self) -> i64 {
        self.store
            .repo_by_root(&self.root)
            .expect("store")
            .expect("repo")
            .id
    }
}

fn write(root: &Path, path: &str, contents: &[u8]) {
    let target = root.join(path);
    fs::create_dir_all(target.parent().expect("parent")).expect("parent dir");
    fs::write(target, contents).expect("file");
}

fn replace(path: &str, old: &str, new: &str, expected_count: usize) -> EditOperation {
    EditOperation::Replace {
        path: path.to_owned(),
        old: old.to_owned(),
        new: new.to_owned(),
        expected_count,
        expected_hash: None,
    }
}

fn rename(old: &str, new: &str, path: Option<&str>, expected: Option<usize>) -> EditOperation {
    EditOperation::Rename {
        old: old.to_owned(),
        new: new.to_owned(),
        path: path.map(str::to_owned),
        expected_count: expected,
        allow_existing: false,
    }
}

fn refusal(error: EditError) -> Refusal {
    match error {
        EditError::Refused { reason, .. } => reason,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[test]
fn test_rename_touches_only_whole_identifiers_and_reports_leftovers() {
    let fixture = Fixture::new(&[("src/decisions.rs", DECISIONS.as_bytes())]);
    let report = fixture
        .apply(vec![rename(
            "format_utc",
            "format_rfc3339_utc",
            Some("src/decisions.rs"),
            Some(4),
        )])
        .expect("rename applies");
    let text = fixture.text("src/decisions.rs");
    assert!(text.contains("pub fn format_rfc3339_utc(seconds: i64)"));
    assert_eq!(text.matches("format_rfc3339_utc(").count(), 4);
    for kept in [
        "format_utc_now",
        "reformat_utc",
        "x.format_utc2",
        "\"call format_utc_v2 here\"",
    ] {
        assert!(text.contains(kept), "{kept} must be untouched");
    }
    let operation = &report.operations[0];
    assert_eq!(operation.sites, 4);
    let outcome = operation.rename.as_ref().expect("rename outcome");
    assert_eq!(outcome.remaining_whole_total, 0);
    // format_utc_now (doc comment and definition), reformat_utc, the
    // format_utc2 field and its use, and the string remain as substrings.
    assert_eq!(outcome.remaining_substring_total, 6);
    assert!(
        outcome
            .remaining_substring
            .iter()
            .any(|location| location.text.contains("reformat_utc"))
    );
    assert_eq!(report.files[0].lines, [2, 7, 11, 21]);
    assert!(report.files[0].verified);
    assert_eq!(report.index, IndexState::Fresh);
}

#[test]
fn test_rename_counts_definitions_calls_and_doc_comments_as_sites() {
    let source =
        "/// Use format_utc for timestamps.\npub fn format_utc() {}\nfn main() { format_utc(); }\n";
    let fixture = Fixture::new(&[("lib.rs", source.as_bytes())]);
    let report = fixture
        .apply(vec![rename("format_utc", "format_rfc3339_utc", None, None)])
        .expect("rename applies");
    assert_eq!(report.operations[0].sites, 3);
    assert_eq!(
        fixture.text("lib.rs"),
        source.replace("format_utc", "format_rfc3339_utc")
    );
}

#[test]
fn test_rename_reports_a_string_leftover_as_a_substring_match() {
    let source = "fn format_utc() {}\nconst NAME: &str = \"xformat_utc\";\n";
    let fixture = Fixture::new(&[("lib.rs", source.as_bytes())]);
    let report = fixture
        .apply(vec![rename("format_utc", "format_iso", None, None)])
        .expect("rename applies");
    let outcome = report.operations[0].rename.as_ref().expect("outcome");
    assert_eq!(outcome.remaining_whole_total, 0);
    assert_eq!(outcome.remaining_substring_total, 1);
    assert_eq!(outcome.remaining_substring[0].line, 2);
}

#[test]
fn test_index_is_fresh_after_an_edit() {
    let fixture = Fixture::new(&[("src/decisions.rs", DECISIONS.as_bytes())]);
    let repo = fixture.repo();
    assert!(
        !search(&fixture.store, repo, "format_utc", 10)
            .expect("search")
            .is_empty()
    );
    fixture
        .apply(vec![rename("format_utc", "format_rfc3339_utc", None, None)])
        .expect("rename applies");
    let new_hits = search(&fixture.store, repo, "format_rfc3339_utc", 10).expect("search");
    assert!(
        new_hits
            .iter()
            .any(|hit| hit.symbol.qualified_name == "format_rfc3339_utc")
    );
    let old_hits = search(&fixture.store, repo, "format_utc", 10).expect("search");
    assert!(
        old_hits
            .iter()
            .all(|hit| hit.symbol.qualified_name != "format_utc"),
        "{old_hits:?}"
    );
    let record = fixture
        .store
        .indexed_file(repo, "src/decisions.rs")
        .expect("store")
        .expect("indexed");
    assert_eq!(
        record.hash,
        blake3::hash(&fixture.read("src/decisions.rs"))
            .to_hex()
            .to_string()
    );
}

#[test]
fn test_expected_count_mismatch_writes_nothing_and_names_the_lines() {
    let source = "a = 1\nb = a\nc = a\n";
    let fixture = Fixture::new(&[("vars.txt", source.as_bytes())]);
    let error = fixture
        .apply(vec![
            replace("vars.txt", "1", "2", 1),
            replace("vars.txt", "= a", "= z", 1),
        ])
        .expect_err("second operation finds two");
    match error {
        EditError::CountMismatch {
            operation,
            expected,
            actual,
            locations,
        } => {
            assert_eq!((operation, expected, actual), (2, 1, 2));
            assert_eq!(
                locations
                    .iter()
                    .map(|location| location.line)
                    .collect::<Vec<_>>(),
                [2, 3]
            );
        }
        other => panic!("unexpected {other:?}"),
    }
    assert_eq!(fixture.text("vars.txt"), source, "all or nothing");

    let error = fixture
        .apply(vec![rename("a", "z", None, Some(5))])
        .expect_err("rename count differs");
    assert!(matches!(
        error,
        EditError::CountMismatch {
            expected: 5,
            actual: 3,
            ..
        }
    ));
    assert_eq!(fixture.text("vars.txt"), source);
}

#[test]
fn test_replace_and_insert_compose_in_order_on_one_file() {
    let fixture = Fixture::new(&[("notes.txt", b"one\ntwo\nthree\n")]);
    let report = fixture
        .apply(vec![
            replace("notes.txt", "two", "TWO", 1),
            EditOperation::Insert {
                path: "notes.txt".to_owned(),
                anchor: "TWO\n".to_owned(),
                text: "two and a half\n".to_owned(),
                position: InsertPosition::After,
                expected_hash: None,
            },
            EditOperation::Insert {
                path: "notes.txt".to_owned(),
                anchor: "one".to_owned(),
                text: "zero\n".to_owned(),
                position: InsertPosition::Before,
                expected_hash: None,
            },
        ])
        .expect("edits apply");
    assert_eq!(
        fixture.text("notes.txt"),
        "zero\none\nTWO\ntwo and a half\nthree\n"
    );
    assert_eq!(report.files.len(), 1);
    assert_eq!(report.files[0].sites, 3);
    // Lines refer to the edited file: earlier sites move with later splices.
    assert_eq!(report.files[0].lines, [1, 3, 4]);
}

#[test]
fn test_insert_requires_a_unique_anchor() {
    let fixture = Fixture::new(&[("notes.txt", b"x\nx\n")]);
    let error = fixture
        .apply(vec![EditOperation::Insert {
            path: "notes.txt".to_owned(),
            anchor: "x".to_owned(),
            text: "y".to_owned(),
            position: InsertPosition::After,
            expected_hash: None,
        }])
        .expect_err("anchor occurs twice");
    assert!(matches!(
        error,
        EditError::CountMismatch {
            expected: 1,
            actual: 2,
            ..
        }
    ));
}

#[test]
fn test_crlf_and_bom_are_preserved() {
    let fixture = Fixture::new(&[(
        "win.txt",
        "\u{feff}first line\r\nsecond line\r\n".as_bytes(),
    )]);
    let report = fixture
        .apply(vec![replace(
            "win.txt",
            "first line\nsecond",
            "first line\nnew line\nsecond",
            1,
        )])
        .expect("edit applies");
    assert_eq!(
        fixture.read("win.txt"),
        "\u{feff}first line\r\nnew line\r\nsecond line\r\n".as_bytes()
    );
    assert_eq!(report.files[0].line_ending, LineEnding::Crlf);
    assert!(report.files[0].bom);
}

#[cfg(unix)]
#[test]
fn test_permissions_are_preserved() {
    use std::os::unix::fs::PermissionsExt as _;
    let fixture = Fixture::new(&[("run.sh", b"#!/bin/sh\necho old\n")]);
    fs::set_permissions(
        fixture.root.join("run.sh"),
        fs::Permissions::from_mode(0o751),
    )
    .expect("chmod");
    fixture
        .apply(vec![replace("run.sh", "old", "new", 1)])
        .expect("edit applies");
    let mode = fs::metadata(fixture.root.join("run.sh"))
        .expect("metadata")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o751);
    assert_eq!(fixture.text("run.sh"), "#!/bin/sh\necho new\n");
}

#[test]
fn test_dry_run_writes_nothing_and_returns_the_diff() {
    let source = "fn alpha() {}\nfn beta() { alpha(); }\n";
    let fixture = Fixture::new(&[("lib.rs", source.as_bytes())]);
    let indexed_before = fixture
        .store
        .indexed_file(fixture.repo(), "lib.rs")
        .expect("store");
    let report = fixture
        .apply_request(&fixture.request(vec![rename("alpha", "gamma", None, Some(2))], true))
        .expect("dry run");
    assert!(report.dry_run);
    assert_eq!(fixture.text("lib.rs"), source);
    assert_eq!(report.bytes_written, 0);
    assert_eq!(report.index, IndexState::NotRun);
    assert!(!report.files[0].verified);
    assert_eq!(
        report.diff.as_deref(),
        Some(
            "--- a/lib.rs\n+++ b/lib.rs\n@@ -1,2 +1,2 @@\n-fn alpha() {}\n-fn beta() { alpha(); }\n+fn gamma() {}\n+fn beta() { gamma(); }\n"
        )
    );
    assert_eq!(
        fixture
            .store
            .indexed_file(fixture.repo(), "lib.rs")
            .expect("store"),
        indexed_before
    );
}

#[test]
fn test_secret_gitignored_and_unindexed_paths_are_refused() {
    let fixture = Fixture::new(&[
        (".gitignore", b"ignored.rs\n"),
        ("ignored.rs", b"fn x() {}\n"),
        (".env", b"KEY=value\n"),
        ("kept.rs", b"fn x() {}\n"),
    ]);
    write(&fixture.root, "late.rs", b"fn x() {}\n");
    assert_eq!(
        refusal(
            fixture
                .apply(vec![replace(".env", "value", "other", 1)])
                .unwrap_err()
        ),
        Refusal::NotFound,
        "a secret path reads as missing, as in every read tool"
    );
    assert_eq!(
        refusal(
            fixture
                .apply(vec![replace("ignored.rs", "x", "y", 1)])
                .unwrap_err()
        ),
        Refusal::Excluded
    );
    assert_eq!(
        refusal(
            fixture
                .apply(vec![replace("late.rs", "x", "y", 1)])
                .unwrap_err()
        ),
        Refusal::NotIndexed
    );
    assert_eq!(
        refusal(
            fixture
                .apply(vec![replace("../outside.rs", "x", "y", 1)])
                .unwrap_err()
        ),
        Refusal::InvalidPath
    );
    assert_eq!(
        refusal(
            fixture
                .apply(vec![replace(".git/config", "x", "y", 1)])
                .unwrap_err()
        ),
        Refusal::Protected
    );
    assert_eq!(fixture.text("ignored.rs"), "fn x() {}\n");
    assert_eq!(fixture.text(".env"), "KEY=value\n");
    // A rename skips what it may not edit and says so; it never edits it.
    let report = fixture
        .apply(vec![rename("x", "y", None, None)])
        .expect("rename applies to kept.rs");
    assert_eq!(report.operations[0].files, ["kept.rs"]);
    assert_eq!(fixture.text("ignored.rs"), "fn x() {}\n");
    assert_eq!(fixture.text("late.rs"), "fn x() {}\n");
}

#[test]
fn test_protected_directories_are_refused() {
    let fixture = Fixture::new(&[("store/notes.txt", b"x\n")]);
    let request = EditRequest {
        operations: vec![replace("store/notes.txt", "x", "y", 1)],
        dry_run: false,
        protected: vec![fixture.root.join("store")],
    };
    assert_eq!(
        refusal(fixture.apply_request(&request).unwrap_err()),
        Refusal::Protected
    );
    assert_eq!(fixture.text("store/notes.txt"), "x\n");
}

#[test]
fn test_oversize_and_non_utf8_files_are_refused() {
    let fixture = Fixture::new(&[("big.txt", b"x\n"), ("bytes.txt", b"x\n")]);
    write(&fixture.root, "big.txt", "x\n".repeat(600_000).as_bytes());
    write(&fixture.root, "bytes.txt", b"x\n\xff\xfe\n");
    assert_eq!(
        refusal(
            fixture
                .apply(vec![replace("big.txt", "x", "y", 1)])
                .unwrap_err()
        ),
        Refusal::TooLarge
    );
    assert_eq!(
        refusal(
            fixture
                .apply(vec![replace("bytes.txt", "x", "y", 1)])
                .unwrap_err()
        ),
        Refusal::NotUtf8
    );
    // An edit that would grow a file past the limit is refused too.
    let fixture = Fixture::new(&[("grow.txt", b"seed\n")]);
    let error = fixture
        .apply(vec![replace(
            "grow.txt",
            "seed",
            &"y".repeat(EDIT_MAX_TEXT_BYTES),
            1,
        )])
        .map(|_| ())
        .and_then(|()| {
            fixture.apply(
                (0..EDIT_MAX_OPERATIONS)
                    .map(|_| EditOperation::Insert {
                        path: "grow.txt".to_owned(),
                        anchor: "\n".to_owned(),
                        text: "z".repeat(EDIT_MAX_TEXT_BYTES),
                        position: InsertPosition::Before,
                        expected_hash: None,
                    })
                    .collect(),
            )
        })
        .expect_err("over a megabyte");
    assert!(
        matches!(
            error,
            EditError::Bound {
                bound: EditBound::FileBytes,
                ..
            }
        ),
        "{error:?}"
    );
}

#[cfg(unix)]
#[test]
fn test_symlink_components_and_links_out_of_the_root_are_refused() {
    let fixture = Fixture::new(&[("src/a.rs", b"fn a() {}\n"), ("other/a.rs", b"fn a() {}\n")]);
    // A directory swapped for a link after indexing.
    fs::rename(fixture.root.join("src"), fixture.root.join("src-old")).expect("move");
    std::os::unix::fs::symlink(fixture.root.join("other"), fixture.root.join("src"))
        .expect("symlink");
    assert_eq!(
        refusal(
            fixture
                .apply(vec![replace("src/a.rs", "a", "b", 1)])
                .unwrap_err()
        ),
        Refusal::NotFound,
        "a path through a symbolic link reads as missing, as in every read tool"
    );
    // An indexed file swapped for a link that leaves the root.
    let outside = fixture.root.parent().expect("parent").join("outside.rs");
    fs::write(&outside, "fn a() {}\n").expect("outside");
    fs::remove_file(fixture.root.join("other/a.rs")).expect("remove");
    std::os::unix::fs::symlink(&outside, fixture.root.join("other/a.rs")).expect("symlink");
    let error = fixture
        .apply(vec![replace("other/a.rs", "a", "b", 1)])
        .expect_err("outside root");
    assert!(matches!(error, EditError::OutsideRoot { .. }), "{error:?}");
    assert_eq!(
        fs::read_to_string(&outside).expect("outside"),
        "fn a() {}\n"
    );
}

#[test]
fn test_stale_files_are_edited_against_current_content_and_marked() {
    let fixture = Fixture::new(&[("lib.rs", b"fn a() {}\n")]);
    write(&fixture.root, "lib.rs", b"fn a() {}\nfn b() {}\n");
    let report = fixture
        .apply(vec![replace("lib.rs", "fn b", "fn c", 1)])
        .expect("edit applies");
    assert!(report.files[0].stale);
    assert_eq!(fixture.text("lib.rs"), "fn a() {}\nfn c() {}\n");
}

#[test]
fn test_a_content_hash_precondition_is_enforced() {
    let fixture = Fixture::new(&[("lib.rs", b"fn a() {}\n")]);
    let hash = blake3::hash(b"fn a() {}\n").to_hex().to_string();
    let wrong = "0".repeat(64);
    let error = fixture
        .apply(vec![EditOperation::Replace {
            path: "lib.rs".to_owned(),
            old: "a".to_owned(),
            new: "b".to_owned(),
            expected_count: 1,
            expected_hash: Some(wrong),
        }])
        .expect_err("hash differs");
    assert!(matches!(error, EditError::HashMismatch { .. }));
    fixture
        .apply(vec![EditOperation::Replace {
            path: "lib.rs".to_owned(),
            old: "fn a".to_owned(),
            new: "fn b".to_owned(),
            expected_count: 1,
            expected_hash: Some(hash),
        }])
        .expect("hash matches");
    assert_eq!(fixture.text("lib.rs"), "fn b() {}\n");
}

#[test]
fn test_bounds_are_enforced_before_anything_is_written() {
    let fixture = Fixture::new(&[("many.txt", "v_x ".repeat(EDIT_MAX_SITES + 1).as_bytes())]);
    let error = fixture
        .apply(vec![rename("v_x", "v_y", None, None)])
        .expect_err("too many sites");
    assert!(
        matches!(
            error,
            EditError::Bound {
                bound: EditBound::Sites,
                ..
            }
        ),
        "{error:?}"
    );
    assert!(!fixture.text("many.txt").contains("v_y"));
    let too_many = (0..=EDIT_MAX_OPERATIONS)
        .map(|_| replace("many.txt", "v_x", "v_y", 1))
        .collect();
    assert!(matches!(
        fixture.apply(too_many),
        Err(EditError::Invalid { .. })
    ));
    let oversized = replace("many.txt", &"v".repeat(EDIT_MAX_TEXT_BYTES + 1), "x", 1);
    assert!(matches!(
        fixture.apply(vec![oversized]),
        Err(EditError::Invalid { .. })
    ));
    let nul = replace("many.txt", "v_x", "a\0b", 1);
    assert!(matches!(
        fixture.apply(vec![nul]),
        Err(EditError::Invalid { .. })
    ));
    assert!(matches!(
        fixture.apply(vec![rename("v_x", "not an identifier", None, None)]),
        Err(EditError::Invalid { .. })
    ));
}

#[test]
fn test_a_rename_with_no_whole_match_reports_substrings_and_writes_nothing() {
    let fixture = Fixture::new(&[("lib.rs", b"fn format_utc_now() {}\n")]);
    match fixture.apply(vec![rename("format_utc", "format_iso", None, None)]) {
        Err(EditError::NoMatches {
            substring_total,
            substring,
            ..
        }) => {
            assert_eq!(substring_total, 1);
            assert_eq!(substring[0].line, 1);
        }
        other => panic!("unexpected {other:?}"),
    }
    assert_eq!(fixture.text("lib.rs"), "fn format_utc_now() {}\n");
}

#[test]
fn test_ignore_files_and_agent_ci_configuration_are_refused() {
    let fixture = Fixture::new(&[
        (".gitignore", b"private/\n"),
        ("private/notes.txt", b"hidden\n"),
        (".claude/settings.json", b"{\"hooks\": {}}\n"),
        (".github/workflows/ci.yml", b"run: tool_name\n"),
        ("sub/.gitattributes", b"* text\n"),
        (".mcp.json", b"{}\n"),
        ("src/lib.rs", b"fn tool_name() {}\n"),
    ]);
    for (path, old) in [
        (".gitignore", "private/\n"),
        (".claude/settings.json", "{}"),
        (".github/workflows/ci.yml", "tool_name"),
        ("sub/.gitattributes", "text"),
        (".mcp.json", "{}"),
    ] {
        assert_eq!(
            refusal(fixture.apply(vec![replace(path, old, "x", 1)]).unwrap_err()),
            Refusal::ControlFile,
            "{path}"
        );
    }
    assert_eq!(fixture.text(".gitignore"), "private/\n");
    // A rename edits the source, skips the CI file that names the
    // identifier, and says the rename is incomplete.
    let report = fixture
        .apply(vec![rename("tool_name", "tool_label", None, None)])
        .expect("rename applies to the source");
    let outcome = report.operations[0].rename.as_ref().expect("outcome");
    assert_eq!(report.operations[0].files, ["src/lib.rs"]);
    assert!(!outcome.complete);
    assert_eq!(outcome.files_skipped.len(), 1);
    assert_eq!(outcome.files_skipped[0].path, ".github/workflows/ci.yml");
    assert_eq!(outcome.files_skipped[0].reason, Refusal::ControlFile);
    assert_eq!(fixture.text(".github/workflows/ci.yml"), "run: tool_name\n");
    // Control files without the name do not make a rename incomplete.
    let report = fixture
        .apply(vec![rename("tool_label", "tool_tag", Some("src/**"), None)])
        .expect("scoped rename");
    assert!(
        report.operations[0]
            .rename
            .as_ref()
            .expect("outcome")
            .complete
    );
    let report = fixture
        .apply(vec![rename("tool_tag", "tool_mark", None, None)])
        .expect("unscoped rename");
    assert!(
        report.operations[0]
            .rename
            .as_ref()
            .expect("outcome")
            .complete
    );
}

#[test]
fn test_renaming_into_an_existing_name_needs_allow_existing() {
    let source = "fn old_name() {}\nfn new_name() {}\nfn main() { old_name(); }\n";
    let fixture = Fixture::new(&[("lib.rs", source.as_bytes())]);
    match fixture.apply(vec![rename("old_name", "new_name", None, None)]) {
        Err(EditError::NameExists {
            count, locations, ..
        }) => {
            assert_eq!(count, 1);
            assert_eq!(locations[0].line, 2);
        }
        other => panic!("unexpected {other:?}"),
    }
    assert_eq!(fixture.text("lib.rs"), source);
    let report = fixture
        .apply(vec![EditOperation::Rename {
            old: "old_name".to_owned(),
            new: "new_name".to_owned(),
            path: None,
            expected_count: None,
            allow_existing: true,
        }])
        .expect("explicitly allowed");
    assert_eq!(
        report.operations[0]
            .rename
            .as_ref()
            .expect("outcome")
            .new_name_preexisting,
        1
    );
}

#[test]
fn test_a_rename_never_rewrites_escapes_or_string_prefixes() {
    let source = "def f(n):\n    return f\"{n}\\n\" + r'x'\n";
    let fixture = Fixture::new(&[("m.py", source.as_bytes())]);
    fixture
        .apply(vec![rename("n", "count", None, Some(2))])
        .expect("two real uses of n");
    assert_eq!(
        fixture.text("m.py"),
        "def f(count):\n    return f\"{count}\\n\" + r'x'\n"
    );
    fixture
        .apply(vec![rename("f", "g", None, Some(1))])
        .expect("only the definition");
    assert!(fixture.text("m.py").contains("return f\"{count}"));
}

#[cfg(unix)]
#[test]
fn test_a_file_without_write_permission_is_refused() {
    use std::os::unix::fs::PermissionsExt as _;
    let fixture = Fixture::new(&[("locked.txt", b"x\n")]);
    fs::set_permissions(
        fixture.root.join("locked.txt"),
        fs::Permissions::from_mode(0o444),
    )
    .expect("chmod");
    assert_eq!(
        refusal(
            fixture
                .apply(vec![replace("locked.txt", "x", "y", 1)])
                .unwrap_err()
        ),
        Refusal::ReadOnly
    );
    assert_eq!(fixture.text("locked.txt"), "x\n");
}

#[test]
fn test_a_rename_stops_at_the_file_bound() {
    let files = (0..=compresso_atlas::EDIT_MAX_FILES)
        .map(|index| (format!("f{index:03}.txt"), b"shared_name\n".to_vec()))
        .collect::<Vec<_>>();
    let borrowed = files
        .iter()
        .map(|(path, contents)| (path.as_str(), contents.as_slice()))
        .collect::<Vec<_>>();
    let fixture = Fixture::new(&borrowed);
    let error = fixture
        .apply(vec![rename("shared_name", "other_name", None, None)])
        .expect_err("201 files");
    assert!(
        matches!(
            error,
            EditError::Bound {
                bound: EditBound::Files,
                ..
            }
        ),
        "{error:?}"
    );
    assert_eq!(fixture.text("f000.txt"), "shared_name\n");
}

#[test]
fn test_a_call_stops_at_the_write_byte_bound() {
    // Seventeen files just under the 1 MB file limit exceed 16 MiB in total.
    let filler = "x".repeat(990_000);
    let files = (0..17)
        .map(|index| (format!("big{index:02}.txt"), format!("token_a\n{filler}\n")))
        .collect::<Vec<_>>();
    let borrowed = files
        .iter()
        .map(|(path, contents)| (path.as_str(), contents.as_bytes()))
        .collect::<Vec<_>>();
    let fixture = Fixture::new(&borrowed);
    let error = fixture
        .apply(vec![rename("token_a", "token_b", None, None)])
        .expect_err("over 16 MiB");
    assert!(
        matches!(
            error,
            EditError::Bound {
                bound: EditBound::WriteBytes,
                ..
            }
        ),
        "{error:?}"
    );
    assert!(fixture.text("big00.txt").starts_with("token_a\n"));
}

#[test]
fn test_agent_instruction_files_are_refused_in_any_case() {
    let fixture = Fixture::new(&[
        ("AGENTS.md", b"Use helper_name.\n"),
        ("docs/claude.md", b"helper_name\n"),
        (".Claude/settings.json", b"{}\n"),
        ("Jenkinsfile", b"sh helper_name\n"),
        ("src/lib.rs", b"fn helper_name() {}\n"),
    ]);
    for path in [
        "AGENTS.md",
        "docs/claude.md",
        ".Claude/settings.json",
        "Jenkinsfile",
    ] {
        let old = if path.ends_with(".json") {
            "{}"
        } else {
            "helper_name"
        };
        assert_eq!(
            refusal(fixture.apply(vec![replace(path, old, "x", 1)]).unwrap_err()),
            Refusal::ControlFile,
            "{path}"
        );
    }
    let report = fixture
        .apply(vec![rename("helper_name", "helper_label", None, None)])
        .expect("rename applies to the source");
    let outcome = report.operations[0].rename.as_ref().expect("outcome");
    assert!(!outcome.complete);
    let mut skipped = outcome
        .files_skipped
        .iter()
        .map(|file| file.path.as_str())
        .collect::<Vec<_>>();
    skipped.sort_unstable();
    assert_eq!(skipped, ["AGENTS.md", "Jenkinsfile", "docs/claude.md"]);
    assert_eq!(fixture.text("AGENTS.md"), "Use helper_name.\n");
}
