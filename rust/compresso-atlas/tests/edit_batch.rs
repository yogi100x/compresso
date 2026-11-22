//! E17-47 (D112): large edit batches. The operation limit and the call's
//! byte bound, files created since the last index, identical replace texts,
//! and errors that name the operation that failed.

use std::fs;
use std::path::{Path, PathBuf};

use compresso_atlas::{
    EDIT_MAX_NEW_DIRECTORIES, EDIT_MAX_NEW_DIRECTORY_DEPTH, EDIT_MAX_OPERATIONS,
    EDIT_MAX_TEXT_BYTES, EditBound, EditError, EditOperation, EditReport, EditRequest, Indexer,
    InsertPosition, Refusal, apply_edits,
};
use compresso_core::{TokenProvider, counter_for};
use compresso_store::Store;
use tempfile::TempDir;

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
        Indexer::new(&store, counter_for(TokenProvider::OpenAiO200k))
            .index(&root, |_| {})
            .expect("index");
        Self {
            _directory: directory,
            root,
            store,
        }
    }

    fn apply(&self, operations: Vec<EditOperation>) -> Result<EditReport, EditError> {
        self.apply_dry(operations, false)
    }

    fn apply_dry(
        &self,
        operations: Vec<EditOperation>,
        dry_run: bool,
    ) -> Result<EditReport, EditError> {
        apply_edits(
            &self.store,
            &self.root,
            &EditRequest {
                operations,
                dry_run,
                protected: Vec::new(),
            },
            counter_for(TokenProvider::OpenAiO200k),
        )
    }

    fn read(&self, path: &str) -> Vec<u8> {
        fs::read(self.root.join(path)).expect("file")
    }

    fn text(&self, path: &str) -> String {
        String::from_utf8(self.read(path)).expect("utf-8")
    }

    fn indexed(&self, path: &str) -> bool {
        let repo = self
            .store
            .repo_by_root(&self.root)
            .expect("store")
            .expect("repo")
            .id;
        self.store
            .indexed_file(repo, path)
            .expect("store")
            .is_some()
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

fn refusal(error: EditError) -> Refusal {
    match error {
        EditError::Refused { reason, .. } => reason,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

/// One line per operation: `value_00 = 0;` ...
fn numbered(count: usize) -> String {
    (0..count)
        .map(|index| format!("value_{index:02} = 0;\n"))
        .collect()
}

#[test]
fn test_a_call_carries_sixty_four_operations_and_the_limit_error_names_both_counts() {
    assert_eq!(EDIT_MAX_OPERATIONS, 64);
    let fixture = Fixture::new(&[("values.txt", numbered(65).as_bytes())]);
    let operations = |count: usize| {
        (0..count)
            .map(|index| {
                replace(
                    "values.txt",
                    &format!("value_{index:02} = 0;"),
                    &format!("value_{index:02} = 1;"),
                    1,
                )
            })
            .collect::<Vec<_>>()
    };
    let error = fixture
        .apply(operations(65))
        .expect_err("65 is over the limit");
    let message = error.to_string();
    assert!(
        message.contains("64") && message.contains("65"),
        "the error names the limit and the count sent: {message}"
    );
    assert!(message.contains("nothing was written"), "{message}");
    assert_eq!(fixture.text("values.txt"), numbered(65));

    let report = fixture.apply(operations(64)).expect("64 operations apply");
    assert_eq!(report.operations.len(), 64);
    assert_eq!(
        fixture.text("values.txt").matches(" = 1;").count(),
        64,
        "all 64 applied"
    );
}

#[test]
fn test_the_call_text_is_bounded_in_total_bytes() {
    let fixture = Fixture::new(&[("values.txt", numbered(64).as_bytes())]);
    // 64 operations of 64 KiB each: every operation is within its own limit,
    // but the call carries 4 MiB of text.
    let operations = (0..64)
        .map(|index| EditOperation::Insert {
            path: "values.txt".to_owned(),
            anchor: format!("value_{index:02} = 0;"),
            text: "x".repeat(EDIT_MAX_TEXT_BYTES),
            position: InsertPosition::After,
            expected_hash: None,
        })
        .collect();
    let error = fixture.apply(operations).expect_err("over the byte bound");
    let message = error.to_string();
    assert!(
        message.contains("4194304"),
        "the error names the byte bound: {message}"
    );
    assert_eq!(fixture.text("values.txt"), numbered(64));
}

#[test]
fn test_a_file_created_after_the_last_index_is_edited_and_indexed() {
    let fixture = Fixture::new(&[("kept.rs", b"fn kept() {}\n")]);
    write(&fixture.root, "src/late.rs", b"fn late() {}\n");
    assert!(!fixture.indexed("src/late.rs"));

    // A dry run edits nothing but already sees the file.
    let dry = fixture
        .apply_dry(vec![replace("src/late.rs", "fn late", "fn later", 1)], true)
        .expect("the dry run sees the new file");
    assert_eq!(dry.files.len(), 1);
    assert_eq!(fixture.text("src/late.rs"), "fn late() {}\n");

    let report = fixture
        .apply(vec![
            replace("src/late.rs", "fn late", "fn later", 1),
            EditOperation::Insert {
                path: "src/late.rs".to_owned(),
                anchor: "fn later() {}\n".to_owned(),
                text: "fn next() {}\n".to_owned(),
                position: InsertPosition::After,
                expected_hash: None,
            },
        ])
        .expect("an unindexed file that passes every gate is edited");
    assert_eq!(report.files[0].path, "src/late.rs");
    assert!(report.index_refreshed);
    assert_eq!(fixture.text("src/late.rs"), "fn later() {}\nfn next() {}\n");
    assert!(fixture.indexed("src/late.rs"), "the edit leaves it indexed");
}

#[test]
fn test_an_unindexed_file_passes_the_same_gates_as_an_indexed_one() {
    let fixture = Fixture::new(&[(".gitignore", b"ignored/\n"), ("kept.rs", b"fn x() {}\n")]);
    write(&fixture.root, "ignored/new.rs", b"fn x() {}\n");
    write(&fixture.root, ".env.local", b"KEY=x\n");
    write(&fixture.root, "blob.txt", b"fn x() {}\n\0");
    write(&fixture.root, "huge.txt", "x\n".repeat(600_000).as_bytes());
    write(&fixture.root, ".github/workflows/new.yml", b"x: 1\n");
    write(&fixture.root, ".git/new", b"x\n");
    let cases: &[(&str, Refusal)] = &[
        ("ignored/new.rs", Refusal::Excluded),
        (".env.local", Refusal::NotFound),
        ("blob.txt", Refusal::NotIndexed),
        ("huge.txt", Refusal::NotIndexed),
        (".github/workflows/new.yml", Refusal::ControlFile),
        (".git/new", Refusal::Protected),
    ];
    for (path, expected) in cases {
        let error = fixture
            .apply(vec![replace(path, "x", "y", 1)])
            .expect_err(path);
        assert_eq!(refusal(error), *expected, "{path}");
    }
    #[cfg(unix)]
    {
        let outside = fixture.root.parent().expect("parent").join("outside.rs");
        fs::write(&outside, "fn x() {}\n").expect("outside");
        std::os::unix::fs::symlink(&outside, fixture.root.join("link.rs")).expect("link");
        fs::create_dir_all(fixture.root.join("real")).expect("dir");
        fs::write(fixture.root.join("real/inner.rs"), "fn x() {}\n").expect("inner");
        std::os::unix::fs::symlink(fixture.root.join("real"), fixture.root.join("alias"))
            .expect("dir link");
        let error = fixture
            .apply(vec![replace("link.rs", "x", "y", 1)])
            .expect_err("a link out of the root");
        // One answer for every link, whatever it points to (review item 3).
        assert!(
            matches!(
                error,
                EditError::Refused {
                    reason: Refusal::NotFound,
                    ..
                }
            ),
            "{error:?}"
        );
        assert_eq!(
            refusal(
                fixture
                    .apply(vec![replace("alias/inner.rs", "x", "y", 1)])
                    .expect_err("through a linked directory")
            ),
            Refusal::NotFound
        );
        assert_eq!(
            fs::read_to_string(&outside).expect("outside"),
            "fn x() {}\n"
        );
    }
    assert_eq!(fixture.text("ignored/new.rs"), "fn x() {}\n");
    assert_eq!(fixture.text(".env.local"), "KEY=x\n");
    assert_eq!(fixture.read("blob.txt"), b"fn x() {}\n\0");
}

#[test]
fn test_identical_old_and_new_is_a_reported_no_op_that_still_checks_its_count() {
    let fixture = Fixture::new(&[("a.rs", b"fn a() {}\nfn b() {}\n")]);
    let report = fixture
        .apply(vec![
            replace("a.rs", "fn a", "fn a", 1),
            replace("a.rs", "fn b", "fn c", 1),
        ])
        .expect("a no-op does not fail the batch");
    assert_eq!(report.operations[0].sites, 0);
    assert!(report.operations[0].unchanged);
    assert!(report.operations[0].files.is_empty());
    assert!(!report.operations[1].unchanged);
    assert_eq!(report.operations[1].sites, 1);
    assert_eq!(fixture.text("a.rs"), "fn a() {}\nfn c() {}\n");

    // Alone, it writes nothing and reports no file.
    let report = fixture
        .apply(vec![replace("a.rs", "fn a", "fn a", 1)])
        .expect("a lone no-op");
    assert!(report.files.is_empty());
    assert!(report.operations[0].unchanged);

    // The expected count is still an assertion about the file.
    let error = fixture
        .apply(vec![
            replace("a.rs", "fn c", "fn d", 1),
            replace("a.rs", "fn", "fn", 1),
        ])
        .expect_err("the count differs");
    assert!(
        matches!(
            error,
            EditError::CountMismatch {
                operation: 2,
                expected: 1,
                actual: 2,
                ..
            }
        ),
        "{error:?}"
    );
    assert_eq!(fixture.text("a.rs"), "fn a() {}\nfn c() {}\n");
}

#[test]
fn test_request_errors_name_the_operation_that_failed() {
    let fixture = Fixture::new(&[("a.rs", b"fn a() {}\n")]);
    let error = fixture
        .apply(vec![
            replace("a.rs", "fn a", "fn b", 1),
            replace("a.rs", "fn b", "fn c", 1),
            replace("a.rs", "", "x", 1),
        ])
        .expect_err("empty old");
    assert!(error.to_string().contains("operation 3"), "{error}");
    let error = fixture
        .apply(vec![
            replace("a.rs", "fn a", "fn b", 1),
            replace("missing.rs", "x", "y", 1),
        ])
        .expect_err("missing file");
    assert!(error.to_string().contains("operation 2"), "{error}");
    assert!(
        matches!(
            error,
            EditError::Refused {
                reason: Refusal::NotFound,
                operation: Some(2),
                ..
            }
        ),
        "a missing unindexed path reads as missing: {error:?}"
    );
    assert_eq!(fixture.text("a.rs"), "fn a() {}\n");
}

#[test]
fn test_a_count_mismatch_after_an_earlier_operation_says_the_text_was_already_edited() {
    let fixture = Fixture::new(&[("a.rs", b"fn a() {}\n")]);
    let error = fixture
        .apply(vec![
            replace("a.rs", "fn a", "fn b", 1),
            replace("a.rs", "fn a", "fn c", 1),
        ])
        .expect_err("the first operation consumed the match");
    assert!(error.to_string().contains("earlier operation"), "{error}");
    // Without an earlier edit of the file the message is unchanged.
    let error = fixture
        .apply(vec![replace("a.rs", "fn z", "fn c", 1)])
        .expect_err("no match");
    assert!(!error.to_string().contains("earlier operation"), "{error}");
}

fn create(path: &str, content: &str) -> EditOperation {
    EditOperation::Create {
        path: path.to_owned(),
        content: content.to_owned(),
    }
}

#[test]
fn test_create_makes_a_new_file_reports_it_and_indexes_it() {
    let fixture = Fixture::new(&[("src/lib.rs", b"pub mod made;\n")]);
    let dry = fixture
        .apply_dry(vec![create("src/made.rs", "pub fn made() {}\n")], true)
        .expect("dry run");
    assert!(
        !fixture.root.join("src/made.rs").exists(),
        "a dry run writes nothing"
    );
    let diff = dry.diff.expect("diff");
    assert!(
        diff.starts_with("--- /dev/null\n+++ b/src/made.rs\n"),
        "{diff}"
    );
    assert!(diff.contains("+pub fn made() {}"), "{diff}");
    assert!(dry.files[0].created);

    let report = fixture
        .apply(vec![
            create("src/made.rs", "pub fn made() {}\n"),
            create("src/empty.rs", ""),
        ])
        .expect("created");
    assert_eq!(fixture.text("src/made.rs"), "pub fn made() {}\n");
    assert_eq!(
        fixture.text("src/empty.rs"),
        "",
        "an empty file is created too"
    );
    assert_eq!(report.files.len(), 2);
    assert!(
        report
            .files
            .iter()
            .all(|file| file.created && file.verified)
    );
    assert_eq!(report.operations[0].kind, "create");
    assert!(fixture.indexed("src/made.rs") && fixture.indexed("src/empty.rs"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = fs::metadata(fixture.root.join("src/made.rs"))
            .expect("metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o111, 0, "a created file is never executable");
        assert_ne!(mode & 0o200, 0, "the owner can write it");
    }
}

#[test]
fn test_create_refuses_any_existing_path() {
    let fixture = Fixture::new(&[("a.rs", b"fn a() {}\n"), ("dir/b.rs", b"fn b() {}\n")]);
    for path in ["a.rs", "dir"] {
        let error = fixture.apply(vec![create(path, "x\n")]).expect_err(path);
        assert_eq!(refusal(error), Refusal::Exists, "{path}");
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(fixture.root.join("nowhere"), fixture.root.join("dangling"))
            .expect("link");
        assert_eq!(
            refusal(fixture.apply(vec![create("dangling", "x\n")]).unwrap_err()),
            Refusal::Exists,
            "a dangling link exists"
        );
        assert!(!fixture.root.join("nowhere").exists());
    }
    // Twice in one call: the second names its operation.
    let error = fixture
        .apply(vec![create("new.rs", "x\n"), create("new.rs", "y\n")])
        .expect_err("created twice");
    assert!(error.to_string().contains("operation 2"), "{error}");
    assert!(!fixture.root.join("new.rs").exists());
    assert_eq!(fixture.text("a.rs"), "fn a() {}\n");
}

#[test]
fn test_create_keeps_every_path_gate_an_indexed_file_has() {
    let fixture = Fixture::new(&[
        (".gitignore", b"ignored/\n"),
        ("ignored/keep.txt", b"x\n"),
        ("src/a.rs", b"fn a() {}\n"),
        ("store/db.txt", b"x\n"),
        (".github/workflows/ci.yml", b"x: 1\n"),
    ]);
    let cases: &[(&str, Refusal)] = &[
        ("ignored/new.rs", Refusal::Excluded),
        (".env", Refusal::Excluded),
        ("src/.env.local", Refusal::Excluded),
        (".git/hooks/pre-commit", Refusal::Protected),
        (".GIT/config", Refusal::Protected),
        (".github/workflows/new.yml", Refusal::ControlFile),
        ("src/.gitignore", Refusal::ControlFile),
        ("AGENTS.md", Refusal::ControlFile),
        ("src/a.rs/new.rs", Refusal::NoDirectory),
        ("../outside.rs", Refusal::InvalidPath),
        ("/abs.rs", Refusal::InvalidPath),
    ];
    for (path, expected) in cases {
        let error = fixture.apply(vec![create(path, "x\n")]).expect_err(path);
        assert_eq!(refusal(error), *expected, "{path}");
    }
    let protected = apply_edits(
        &fixture.store,
        &fixture.root,
        &EditRequest {
            operations: vec![create("store/new.txt", "x\n")],
            dry_run: false,
            protected: vec![fixture.root.join("store")],
        },
        counter_for(TokenProvider::OpenAiO200k),
    )
    .expect_err("protected");
    assert_eq!(refusal(protected), Refusal::Protected);
    #[cfg(unix)]
    {
        let outside = fixture.root.parent().expect("parent").join("elsewhere");
        fs::create_dir_all(&outside).expect("outside dir");
        std::os::unix::fs::symlink(&outside, fixture.root.join("out")).expect("link");
        std::os::unix::fs::symlink(fixture.root.join("src"), fixture.root.join("alias"))
            .expect("link");
        for path in ["out/new.rs", "alias/new.rs"] {
            assert_eq!(
                refusal(fixture.apply(vec![create(path, "x\n")]).unwrap_err()),
                Refusal::NoDirectory,
                "{path}: a directory reached through a link"
            );
        }
        assert_eq!(fs::read_dir(&outside).expect("outside").count(), 0);
    }
    // On a case-insensitive file system `Ignored/` is the ignored directory.
    if fixture.root.join("IGNORED").exists() {
        assert_eq!(
            refusal(
                fixture
                    .apply(vec![create("Ignored/new.rs", "x\n")])
                    .unwrap_err()
            ),
            Refusal::Excluded
        );
    }
    for path in [
        "ignored/new.rs",
        "missing",
        "src/a.rs/new.rs",
        "store/new.txt",
    ] {
        assert!(!fixture.root.join(path).exists(), "{path}");
    }
}

#[cfg(unix)]
#[test]
fn test_create_refuses_an_unwritable_directory_in_dry_runs_too() {
    use std::os::unix::fs::PermissionsExt as _;
    let fixture = Fixture::new(&[("locked/a.rs", b"fn a() {}\n")]);
    let locked = fixture.root.join("locked");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o555)).expect("chmod");
    // A process that may write anywhere (root) cannot observe the refusal.
    if fs::write(locked.join(".probe"), b"").is_ok() {
        let _ = fs::remove_file(locked.join(".probe"));
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).expect("chmod back");
        eprintln!("skipped: this process may write into a 0o555 directory");
        return;
    }
    for dry_run in [true, false] {
        let error = fixture
            .apply_dry(vec![create("locked/new.rs", "x\n")], dry_run)
            .expect_err("unwritable directory");
        assert_eq!(refusal(error), Refusal::ReadOnly, "dry_run={dry_run}");
    }
    assert!(!locked.join("new.rs").exists());
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).expect("chmod back");
}

#[test]
fn test_create_is_all_or_nothing_with_the_other_operations() {
    let fixture = Fixture::new(&[("a.rs", b"fn a() {}\n")]);
    let error = fixture
        .apply(vec![
            create("new.rs", "fn new() {}\n"),
            replace("a.rs", "fn missing", "fn x", 1),
        ])
        .expect_err("the replace fails");
    assert!(
        matches!(error, EditError::CountMismatch { .. }),
        "{error:?}"
    );
    assert!(!fixture.root.join("new.rs").exists(), "nothing was created");

    // A later operation edits the created text; a rename sees it too.
    let report = fixture
        .apply(vec![
            create("new.rs", "fn new() { a(); }\n"),
            EditOperation::Insert {
                path: "new.rs".to_owned(),
                anchor: "fn new()".to_owned(),
                text: "// made\n".to_owned(),
                position: InsertPosition::Before,
                expected_hash: None,
            },
            EditOperation::Rename {
                old: "a".to_owned(),
                new: "renamed".to_owned(),
                path: None,
                expected_count: Some(2),
                allow_existing: false,
            },
        ])
        .expect("applied together");
    assert_eq!(fixture.text("new.rs"), "// made\nfn new() { renamed(); }\n");
    assert_eq!(fixture.text("a.rs"), "fn renamed() {}\n");
    let rename = report.operations[2].rename.as_ref().expect("rename");
    assert!(rename.complete);
    assert_eq!(report.operations[2].files, ["a.rs", "new.rs"]);
}

#[test]
fn test_create_content_is_bounded_text() {
    let fixture = Fixture::new(&[("a.rs", b"fn a() {}\n")]);
    for content in ["a\0b".to_owned(), "x".repeat(EDIT_MAX_TEXT_BYTES + 1)] {
        let error = fixture
            .apply(vec![
                replace("a.rs", "fn a", "fn b", 1),
                create("new.txt", &content),
            ])
            .expect_err("refused");
        assert!(
            matches!(&error, EditError::Invalid { reason } if reason.starts_with("operation 2:")),
            "{error:?}"
        );
    }
    assert!(!fixture.root.join("new.txt").exists());
    assert_eq!(fixture.text("a.rs"), "fn a() {}\n");
}

// --- Review follow-ups (E17-47, D112) ---

/// Control files the review found `create` could plant, and siblings of
/// entries already on the list: refused for edit and for create alike.
const NEW_CONTROL_FILES: &[&str] = &[
    ".yarnrc",
    ".pnpmfile.cjs",
    "mise.toml",
    ".mise.toml",
    "mise.local.toml",
    ".mise.local.toml",
    ".tool-versions",
    "rust-toolchain.toml",
    "rust-toolchain",
    ".exrc",
    ".nvim.lua",
    ".nvimrc",
    ".vimrc",
    ".hg/hgrc",
    ".jj/repo/config.toml",
    ".svn/entries",
    ".yarn/releases/yarn.cjs",
    "AGENT.md",
    ".rules",
    "opencode.json",
    "opencode.jsonc",
    ".opencode/agent.md",
    ".drone.yml",
    "bitbucket-pipelines.yml",
    "azure-pipelines.yaml",
    // Final round of the review.
    "AGENTS.override.md",
    "docs/agents.OVERRIDE.md",
    "CRUSH.md",
    "lefthook-local.yml",
    "lefthook-local.yaml",
    ".lefthook-local.yml",
    "sub/.lefthook-local.yaml",
    "mise.dev.toml",
    "sub/.mise.ci.toml",
    "MISE.Local.Extra.toml",
    "appveyor.yml",
    ".appveyor.yml",
    "appveyor.yaml",
    ".appveyor.yaml",
    "bunfig.toml",
    "pkg/BUNFIG.TOML",
];

#[test]
fn test_control_files_the_review_found_are_refused_for_edit_and_create() {
    let files = NEW_CONTROL_FILES
        .iter()
        .map(|path| (*path, b"x = 1\n".as_slice()))
        .chain([("keep.rs", b"fn k() {}\n".as_slice())])
        .collect::<Vec<_>>();
    let fixture = Fixture::new(&files);
    for path in NEW_CONTROL_FILES {
        let error = fixture
            .apply(vec![replace(path, "x = 1", "x = 2", 1)])
            .expect_err(path);
        assert_eq!(refusal(error).as_str(), "control_file", "edit {path}");
        assert_eq!(fixture.text(path), "x = 1\n", "{path}");
    }
    let fresh = Fixture::new(&[("keep.rs", b"fn k() {}\n")]);
    for path in NEW_CONTROL_FILES {
        if let Some((directory, _)) = path.rsplit_once('/') {
            fs::create_dir_all(fresh.root.join(directory)).expect("dir");
        }
        let error = fresh
            .apply(vec![EditOperation::Create {
                path: (*path).to_owned(),
                content: "x\n".to_owned(),
            }])
            .expect_err(path);
        assert_eq!(refusal(error).as_str(), "control_file", "create {path}");
        assert!(!fresh.root.join(path).exists(), "{path}");
    }
}

#[test]
fn test_create_refuses_hidden_paths_with_a_one_line_reason() {
    let fixture = Fixture::new(&[("src/a.rs", b"fn a() {}\n")]);
    fs::create_dir_all(fixture.root.join("src/.hidden")).expect("dir");
    for path in [".editorconfig", "src/.local.rs", "src/.hidden/x.rs", ".foo"] {
        let error = fixture
            .apply(vec![EditOperation::Create {
                path: path.to_owned(),
                content: "x\n".to_owned(),
            }])
            .expect_err(path);
        let message = error.to_string();
        assert!(!message.contains('\n'), "{message}");
        assert!(message.contains("client"), "{message}");
        assert_eq!(refusal(error).as_str(), "hidden", "{path}");
        assert!(!fixture.root.join(path).exists(), "{path}");
    }
    // An existing hidden file that is not a control file stays editable.
    write(&fixture.root, ".editorconfig", b"x = 1\n");
    fixture
        .apply(vec![replace(".editorconfig", "x = 1", "x = 2", 1)])
        .expect("edit is not create");
}

#[test]
fn test_create_refuses_colons_and_reserved_device_names_everywhere() {
    let fixture = Fixture::new(&[("src/a.rs", b"fn a() {}\n")]);
    for path in [
        "a:b.rs",
        "src/x:y",
        "src/con",
        "nul.txt",
        "COM1.rs",
        "src/lpt9",
        "Aux.h",
        "prn.tar.gz",
        "src/com9.",
        "CONIN$",
        "src/conout$.txt",
        "COM\u{b9}.rs",
        "lpt\u{b2}",
        "src/LPT\u{b3}.c",
    ] {
        let error = fixture
            .apply(vec![EditOperation::Create {
                path: path.to_owned(),
                content: "x\n".to_owned(),
            }])
            .expect_err(path);
        assert_eq!(refusal(error).as_str(), "reserved_name", "{path}");
    }
    for path in [
        "src/console.rs",
        "src/com10.rs",
        "src/auxiliary.rs",
        "src/nullable.rs",
        "src/conin.rs",
        "src/com\u{b4}.rs",
    ] {
        fixture
            .apply(vec![EditOperation::Create {
                path: path.to_owned(),
                content: "x\n".to_owned(),
            }])
            .expect(path);
    }
}

#[test]
fn test_two_creates_differing_only_by_case_are_refused_in_dry_and_real_runs() {
    let fixture = Fixture::new(&[("src/a.rs", b"fn a() {}\n")]);
    for dry_run in [true, false] {
        let error = fixture
            .apply_dry(
                vec![
                    EditOperation::Create {
                        path: "src/Foo.rs".to_owned(),
                        content: "x\n".to_owned(),
                    },
                    EditOperation::Create {
                        path: "src/foo.rs".to_owned(),
                        content: "y\n".to_owned(),
                    },
                ],
                dry_run,
            )
            .expect_err("differ only by case");
        let message = error.to_string();
        assert!(
            message.contains("operation 2") && message.contains("case"),
            "{message}"
        );
    }
    assert!(!fixture.root.join("src/Foo.rs").exists());
    assert!(!fixture.root.join("src/foo.rs").exists());
}

/// A path through a symbolic link is refused alike whatever the link points
/// to, so an answer never reveals whether something outside the root exists.
#[cfg(unix)]
#[test]
fn test_a_link_is_refused_alike_whatever_it_points_to() {
    let fixture = Fixture::new(&[("src/a.rs", b"fn a() {}\n"), ("real/b.rs", b"fn b() {}\n")]);
    let outside = fixture.root.parent().expect("parent").join("outside");
    fs::create_dir_all(&outside).expect("outside");
    fs::write(outside.join("there.rs"), "fn x() {}\n").expect("outside file");
    let link = |name: &str, target: &Path| {
        std::os::unix::fs::symlink(target, fixture.root.join(name)).expect("link")
    };
    link("to_existing.rs", &outside.join("there.rs"));
    link("to_missing.rs", &outside.join("absent.rs"));
    link("to_inside.rs", &fixture.root.join("src/a.rs"));
    link("dir_out", &outside);
    link("dir_in", &fixture.root.join("real"));
    for path in [
        "to_existing.rs",
        "to_missing.rs",
        "to_inside.rs",
        "dir_out/there.rs",
        "dir_out/absent.rs",
        "dir_in/b.rs",
        "missing.rs",
    ] {
        let error = fixture
            .apply(vec![replace(path, "fn", "fn", 1)])
            .expect_err(path);
        assert!(
            matches!(
                error,
                EditError::Refused {
                    reason: Refusal::NotFound,
                    ..
                }
            ),
            "{path}: {error:?}"
        );
    }
    assert_eq!(
        fs::read_to_string(outside.join("there.rs")).expect("outside"),
        "fn x() {}\n"
    );
}

/// Crash leftovers of the edit tool are skipped by exactly their pattern; a
/// user's file that only resembles it is indexed.
#[test]
fn test_the_scanner_skips_exactly_the_edit_temporary_pattern() {
    let fixture = Fixture::new(&[
        ("src/a.rs", b"fn a() {}\n"),
        ("src/.a.rs.compresso-edit-new-4242-7.tmp", b"fn left() {}\n"),
        (".b.rs.compresso-edit-old-1-0.tmp", b"fn left() {}\n"),
        (".notes.compresso-edit-draft.tmp", b"notes\n"),
        (".x.compresso-edit-new-12.tmp", b"notes\n"),
    ]);
    assert!(!fixture.indexed("src/.a.rs.compresso-edit-new-4242-7.tmp"));
    assert!(!fixture.indexed(".b.rs.compresso-edit-old-1-0.tmp"));
    assert!(fixture.indexed(".notes.compresso-edit-draft.tmp"));
    assert!(fixture.indexed(".x.compresso-edit-new-12.tmp"));
}

/// A refresh is observed through a probe file written after the last index:
/// a refresh indexes it.
fn probe(fixture: &Fixture, name: &str) -> String {
    let path = format!("probe_{name}.rs");
    write(&fixture.root, &path, b"fn probe() {}\n");
    path
}

#[test]
fn test_a_target_the_scanner_would_refuse_never_costs_a_refresh() {
    let fixture = Fixture::new(&[(".gitignore", b"ignored/\n"), ("kept.rs", b"fn k() {}\n")]);
    write(&fixture.root, "blob.txt", b"x\n\0");
    write(&fixture.root, "huge.txt", "x\n".repeat(600_000).as_bytes());
    write(&fixture.root, "ignored/x.rs", b"x\n");
    // Only mutated on Unix, where links are made (a Windows build denies the
    // unused `mut`).
    #[cfg_attr(not(unix), allow(unused_mut))]
    let mut targets = vec!["blob.txt", "huge.txt", "ignored/x.rs"];
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(fixture.root.join("kept.rs"), fixture.root.join("link.rs"))
            .expect("link");
        fs::create_dir_all(fixture.root.join("real")).expect("dir");
        write(&fixture.root, "real/y.rs", b"x\n");
        std::os::unix::fs::symlink(fixture.root.join("real"), fixture.root.join("alias"))
            .expect("dir link");
        targets.extend(["link.rs", "alias/y.rs"]);
    }
    for (index, target) in targets.into_iter().enumerate() {
        let probe = probe(&fixture, &index.to_string());
        fixture
            .apply(vec![replace(target, "x", "y", 1)])
            .expect_err(target);
        assert!(!fixture.indexed(&probe), "{target} cost a refresh");
    }
}

#[test]
fn test_an_unchanged_target_a_refresh_did_not_index_is_remembered() {
    let fixture = Fixture::new(&[("kept.rs", b"fn k() {}\n")]);
    // Passes every cheap check, but the scanner skips it.
    let target = ".q.compresso-edit-new-1-1.tmp";
    write(&fixture.root, target, b"x\n");
    let first = probe(&fixture, "first");
    fixture
        .apply(vec![replace(target, "x", "y", 1)])
        .expect_err("not indexed");
    assert!(fixture.indexed(&first), "the first call refreshes");
    let second = probe(&fixture, "second");
    fixture
        .apply(vec![replace(target, "x", "y", 1)])
        .expect_err("not indexed");
    assert!(
        !fixture.indexed(&second),
        "an unchanged target is remembered"
    );
    write(&fixture.root, target, b"x\nmore\n");
    let third = probe(&fixture, "third");
    fixture
        .apply(vec![replace(target, "x", "y", 1)])
        .expect_err("not indexed");
    assert!(fixture.indexed(&third), "a changed target is tried again");
}

/// Final review item 2: a text file that is not valid UTF-8 and holds no
/// NUL (a Latin-1 source file) is skipped like a binary file, and the index
/// continues.
#[test]
fn test_a_non_utf8_text_file_is_skipped_and_the_index_continues() {
    let fixture = Fixture::new(&[
        ("a.rs", b"fn a() {}\n"),
        ("b.py", b"# caf\xe9\ndef f():\n    return 1\n"),
    ]);
    assert!(fixture.indexed("a.rs"));
    assert!(!fixture.indexed("b.py"));
    let repo = fixture
        .store
        .repo_by_root(&fixture.root)
        .expect("store")
        .expect("repo")
        .id;
    let scan = compresso_atlas::scan(&fixture.store, repo, &fixture.root).expect("scan");
    let reasons = format!("{:?}", scan.skipped);
    assert!(reasons.contains("\"b.py\", NotUtf8"), "{reasons}");
    // A multi-byte character split across the scanner's read buffers is
    // still valid UTF-8.
    let mut long = "\u{e9}".repeat(70_000);
    long.push('\n');
    write(&fixture.root, "long.txt", long.as_bytes());
    let probe = probe(&fixture, "utf8");
    fixture
        .apply(vec![replace("long.txt", "\u{e9}\n", "e\n", 1)])
        .expect("a long valid UTF-8 file is indexed and edited");
    assert!(fixture.indexed(&probe));
}

// E17-49 (D114): create makes missing parent directories.

/// Every path under `root`, sorted, directories with a trailing `/`.
fn tree(root: &Path) -> Vec<String> {
    let mut paths = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).expect("read_dir") {
            let entry = entry.expect("entry");
            let path = entry.path();
            let relative = path
                .strip_prefix(root)
                .expect("under root")
                .to_string_lossy()
                .replace('\\', "/");
            let kind = entry.file_type().expect("type");
            if kind.is_dir() && !kind.is_symlink() {
                paths.push(format!("{relative}/"));
                pending.push(path);
            } else {
                paths.push(relative);
            }
        }
    }
    paths.sort();
    paths
}

fn refused_directory(error: &EditError) -> Option<String> {
    match error {
        EditError::Refused { directory, .. } => directory.clone(),
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[test]
fn test_create_makes_nested_missing_directories_and_reports_them() {
    let fixture = Fixture::new(&[("src/lib.rs", b"pub mod a;\n")]);
    let report = fixture
        .apply(vec![create("src/a/b/c/new.rs", "pub fn new() {}\n")])
        .expect("created with its directories");
    assert_eq!(fixture.text("src/a/b/c/new.rs"), "pub fn new() {}\n");
    assert_eq!(
        report.directories_created,
        ["src/a", "src/a/b", "src/a/b/c"],
        "outermost first"
    );
    assert!(report.files[0].created && report.files[0].verified);
    assert!(fixture.indexed("src/a/b/c/new.rs"));
    // A directory that already exists is not reported.
    let again = fixture
        .apply(vec![create("src/a/b/other.rs", "x\n")])
        .expect("into an existing directory");
    assert!(again.directories_created.is_empty());
}

#[test]
fn test_create_dry_run_creates_no_directory_and_reports_what_it_would() {
    let fixture = Fixture::new(&[("src/lib.rs", b"x\n")]);
    let before = tree(&fixture.root);
    let report = fixture
        .apply_dry(vec![create("src/new/deep/x.rs", "x\n")], true)
        .expect("dry run");
    assert_eq!(report.directories_created, ["src/new", "src/new/deep"]);
    assert!(
        report
            .diff
            .expect("diff")
            .contains("+++ b/src/new/deep/x.rs")
    );
    assert_eq!(tree(&fixture.root), before, "a dry run creates nothing");
}

#[test]
fn test_two_creates_share_a_new_parent_in_one_call() {
    let fixture = Fixture::new(&[("src/lib.rs", b"x\n")]);
    let report = fixture
        .apply(vec![
            create("pkg/inner/a.rs", "fn a() {}\n"),
            create("pkg/inner/b.rs", "fn b() {}\n"),
            create("pkg/c.rs", "fn c() {}\n"),
        ])
        .expect("created");
    assert_eq!(report.directories_created, ["pkg", "pkg/inner"]);
    for path in ["pkg/inner/a.rs", "pkg/inner/b.rs", "pkg/c.rs"] {
        assert!(fixture.indexed(path), "{path}");
    }
}

#[test]
fn test_a_refused_directory_component_is_named_with_its_reason() {
    let fixture = Fixture::new(&[(".gitignore", b"build/\n"), ("src/lib.rs", b"x\n")]);
    let before = tree(&fixture.root);
    let cases: &[(&str, Refusal, &str)] = &[
        ("src/.cache/x.rs", Refusal::Hidden, "src/.cache"),
        ("src/.cache/more/x.rs", Refusal::Hidden, "src/.cache"),
        ("src/con/x.rs", Refusal::ReservedName, "src/con"),
        ("src/new/aux.d/x.rs", Refusal::ReservedName, "src/new/aux.d"),
        ("src/a:b/x.rs", Refusal::ReservedName, "src/a:b"),
        (".github/new/x.yml", Refusal::ControlFile, ".github"),
        ("src/.vscode/x.json", Refusal::ControlFile, "src/.vscode"),
        ("src/.git/x", Refusal::Protected, "src/.git"),
        ("build/gen/x.rs", Refusal::Excluded, "build"),
        (
            "src/node_modules/x/y.js",
            Refusal::Excluded,
            "src/node_modules",
        ),
    ];
    for (path, reason, directory) in cases {
        for dry_run in [true, false] {
            let error = fixture
                .apply_dry(vec![create(path, "x\n")], dry_run)
                .expect_err(path);
            let message = error.to_string();
            assert!(
                message.contains(&format!("(directory \"{directory}\")")),
                "{path}: {message}"
            );
            assert!(!message.contains('\n'), "{message}");
            assert_eq!(
                refused_directory(&error).as_deref(),
                Some(*directory),
                "{path}"
            );
            assert_eq!(refusal(error), *reason, "{path}");
        }
    }
    // A refused file name is not a directory.
    let error = fixture
        .apply(vec![create("src/new/.env", "x\n")])
        .expect_err("secret name");
    assert_eq!(refused_directory(&error), None);
    assert_eq!(tree(&fixture.root), before, "nothing was created");
}

#[test]
fn test_create_bounds_the_directories_it_adds() {
    let fixture = Fixture::new(&[("src/lib.rs", b"x\n")]);
    let before = tree(&fixture.root);
    let levels = |count: usize| {
        let mut path = String::from("src");
        for level in 0..count {
            path.push_str(&format!("/d{level}"));
        }
        path.push_str("/x.rs");
        path
    };
    assert_eq!(EDIT_MAX_NEW_DIRECTORY_DEPTH, 8);
    let error = fixture
        .apply(vec![create(
            &levels(EDIT_MAX_NEW_DIRECTORY_DEPTH + 1),
            "x\n",
        )])
        .expect_err("too deep");
    assert!(
        matches!(
            error,
            EditError::CreateBound {
                bound: EditBound::DirectoryDepth,
                limit: 8,
                actual: 9,
                operation: Some(1),
                ..
            }
        ),
        "{error:?}"
    );
    // The error names the operation and the path.
    assert!(
        error
            .to_string()
            .starts_with("operation 1: create \"src/d0/"),
        "{error}"
    );
    assert_eq!(tree(&fixture.root), before);
    let report = fixture
        .apply(vec![create(&levels(EDIT_MAX_NEW_DIRECTORY_DEPTH), "x\n")])
        .expect("at the bound");
    assert_eq!(report.directories_created.len(), 8);

    assert_eq!(EDIT_MAX_NEW_DIRECTORIES, 32);
    let before = tree(&fixture.root);
    let many = (0..=EDIT_MAX_NEW_DIRECTORIES)
        .map(|index| create(&format!("many/d{index:02}/x.rs"), "x\n"))
        .collect::<Vec<_>>();
    // Counted as the operations run: the operation that adds the 33rd fails.
    let error = fixture.apply(many).expect_err("too many directories");
    assert!(
        matches!(
            error,
            EditError::CreateBound {
                bound: EditBound::Directories,
                limit: 32,
                actual: 33,
                operation: Some(32),
                ..
            }
        ),
        "{error:?}"
    );
    assert!(
        error
            .to_string()
            .starts_with("operation 32: create \"many/d31/x.rs\""),
        "{error}"
    );
    assert!(error.to_string().contains("split"), "{error}");
    assert_eq!(tree(&fixture.root), before);
}

#[test]
fn test_a_new_directory_differing_only_by_case_is_refused() {
    let fixture = Fixture::new(&[("src/Utils/a.rs", b"x\n")]);
    let case_insensitive = fixture.root.join("SRC").exists();
    let before = tree(&fixture.root);
    // Two creates of one call.
    for dry_run in [true, false] {
        let error = fixture
            .apply_dry(
                vec![create("src/New/a.rs", "x\n"), create("src/new/b.rs", "y\n")],
                dry_run,
            )
            .expect_err("directories differ only by case");
        assert!(error.to_string().contains("operation 2"), "{error}");
        assert_eq!(refused_directory(&error).as_deref(), Some("src/new"));
        assert_eq!(refusal(error), Refusal::CaseDuplicate);
    }
    // A created file and a created directory of the same name.
    let error = fixture
        .apply(vec![
            create("src/thing", "x\n"),
            create("src/Thing/b.rs", "y\n"),
        ])
        .expect_err("file and directory differ only by case");
    assert_eq!(refusal(error), Refusal::CaseDuplicate);
    let error = fixture
        .apply(vec![
            create("src/thing", "x\n"),
            create("src/thing/b.rs", "y\n"),
        ])
        .expect_err("file and directory of one name");
    assert_eq!(refusal(error), Refusal::Exists);
    assert_eq!(tree(&fixture.root), before);
    // An existing sibling: a case-insensitive file system finds it and
    // writes into it; a case-sensitive one refuses the new spelling.
    let result = fixture.apply(vec![create("src/utils/b.rs", "x\n")]);
    if case_insensitive {
        let report = result.expect("into the existing directory");
        assert!(report.directories_created.is_empty());
        assert!(fixture.root.join("src/Utils/b.rs").exists());
    } else {
        let error = result.expect_err("differs by case from src/Utils");
        assert_eq!(refused_directory(&error).as_deref(), Some("src/utils"));
        assert_eq!(refusal(error), Refusal::CaseDuplicate);
        assert_eq!(tree(&fixture.root), before);
    }
}

/// A linked ancestor is refused with byte-identical answers whatever the
/// link points to, and nothing is created through it or beside it.
#[cfg(unix)]
#[test]
fn test_a_linked_ancestor_is_refused_alike_and_creates_nothing() {
    let fixture = Fixture::new(&[("src/lib.rs", b"x\n"), ("real/there.rs", b"x\n")]);
    let outside = fixture.root.parent().expect("parent").join("outside");
    fs::create_dir_all(&outside).expect("outside");
    fs::write(outside.join("there.rs"), "x\n").expect("outside file");
    // Ignore rules behind a link must not change the answer either.
    fs::write(outside.join(".gitignore"), "new/\n").expect("outside ignore file");
    let targets = [
        fixture.root.join("nowhere"),
        outside.join("there.rs"),
        fixture.root.join("real"),
        outside.clone(),
        outside.join("missing"),
    ];
    let paths = [
        "lnk/x.rs",
        "lnk/there.rs",
        "lnk/new/x.rs",
        "lnk/new/deeper/x.rs",
        "lnk/new/.env",
        "lnk/new/deeper/.env.local",
        "src/lnk/x.rs",
    ];
    let mut answers = Vec::<Vec<String>>::new();
    for target in &targets {
        for name in ["lnk", "src/lnk"] {
            let _ = fs::remove_file(fixture.root.join(name));
            std::os::unix::fs::symlink(target, fixture.root.join(name)).expect("link");
        }
        let inside_before = tree(&fixture.root);
        let outside_before = tree(&outside);
        let mut row = Vec::new();
        for path in paths {
            for dry_run in [true, false] {
                let error = fixture
                    .apply_dry(vec![create(path, "x\n")], dry_run)
                    .expect_err(path);
                row.push(format!("{error:?} | {error}"));
            }
        }
        assert_eq!(tree(&fixture.root), inside_before, "{target:?}");
        assert_eq!(tree(&outside), outside_before, "{target:?}");
        answers.push(row);
    }
    for row in &answers[1..] {
        assert_eq!(
            row, &answers[0],
            "the same answers whatever the link points to"
        );
    }
    assert!(answers[0][0].contains("NoDirectory"), "{}", answers[0][0]);
}

/// E17-49 review: a new name written in decomposed form (a combining mark
/// or conjoining jamo) is refused `unportable_name`, so composed and
/// decomposed spellings of one name get the same answer in dry and real
/// runs; the composed spelling is created.
#[test]
fn test_decomposed_new_names_are_refused_alike_in_dry_and_real_runs() {
    let fixture = Fixture::new(&[("src/lib.rs", b"x\n")]);
    let before = tree(&fixture.root);
    for dry_run in [true, false] {
        for (operations, directory) in [
            (
                vec![
                    create("src/caf\u{e9}/a.rs", "x\n"),
                    create("src/cafe\u{301}/b.rs", "y\n"),
                ],
                Some("src/cafe\u{301}"),
            ),
            (
                vec![
                    create("src/n\u{e9}.rs", "x\n"),
                    create("src/ne\u{301}.rs", "y\n"),
                ],
                None,
            ),
            (
                vec![create("ko/\u{1112}\u{1161}\u{11ab}/a.md", "x\n")],
                Some("ko/\u{1112}\u{1161}\u{11ab}"),
            ),
        ] {
            let error = fixture
                .apply_dry(operations, dry_run)
                .expect_err("a decomposed new name");
            assert_eq!(
                refused_directory(&error).as_deref(),
                directory,
                "dry_run={dry_run}"
            );
            assert_eq!(refusal(error), Refusal::UnportableName, "dry_run={dry_run}");
        }
    }
    assert_eq!(tree(&fixture.root), before);
    fixture
        .apply(vec![create("src/caf\u{e9}/a.rs", "x\n")])
        .expect("the composed spelling");
}

/// E17-49 review: genuinely different names are never a clash, in one call
/// or beside an existing sibling.
#[test]
fn test_distinct_non_ascii_names_are_never_a_clash() {
    let fixture = Fixture::new(&[("src/lib.rs", b"x\n")]);
    for (first, second) in [
        ("ko/\u{d55c}\u{ad6d}.md", "ko/\u{c601}\u{c5b4}.md"),
        ("vi/b\u{1ea1}n/a.md", "vi/b\u{1ea3}n/b.md"),
        (
            "ru/\u{43c}\u{43e}\u{439}/a.md",
            "ru/\u{43c}\u{43e}\u{451}/b.md",
        ),
        ("fr/caf\u{e9}/a.md", "fr/caf\u{e8}/b.md"),
        ("ja/\u{65e5}\u{672c}/a.md", "ja/\u{4e2d}\u{56fd}/b.md"),
    ] {
        fixture
            .apply(vec![create(first, "x\n"), create(second, "y\n")])
            .unwrap_or_else(|error| panic!("{first} and {second}: {error}"));
    }
    // A new directory beside an existing sibling of another name.
    fixture
        .apply(vec![create("ko2/\u{d55c}\u{ad6d}/a.md", "x\n")])
        .expect("first");
    let report = fixture
        .apply(vec![create("ko2/\u{c601}\u{c5b4}/b.md", "y\n")])
        .expect("a different name beside it");
    assert_eq!(report.directories_created, ["ko2/\u{c601}\u{c5b4}"]);
}

/// E17-49 review: a new directory name ending with `.` or a space, or
/// holding a control character, is refused and named.
#[test]
fn test_unportable_new_directory_names_are_refused() {
    let fixture = Fixture::new(&[("src/lib.rs", b"x\n")]);
    let before = tree(&fixture.root);
    for (path, directory) in [
        ("src/gen./x.rs", "src/gen."),
        ("src/gen /x.rs", "src/gen "),
        ("src/a\u{7}b/x.rs", "src/a\u{7}b"),
        ("src/new/line\nbreak/x.rs", "src/new/line\nbreak"),
    ] {
        let error = fixture.apply(vec![create(path, "x\n")]).expect_err(path);
        assert_eq!(
            refused_directory(&error).as_deref(),
            Some(directory),
            "{path:?}"
        );
        assert_eq!(refusal(error), Refusal::UnportableName, "{path:?}");
    }
    assert_eq!(tree(&fixture.root), before);
}

/// E17-49 review: created directories are reported as spelled on disk,
/// the existing part in its canonical spelling.
#[test]
fn test_created_directories_are_reported_as_spelled_on_disk() {
    let fixture = Fixture::new(&[("src/lib.rs", b"x\n")]);
    if !fixture.root.join("SRC").exists() {
        eprintln!("skipped: a case-sensitive file system has no other spelling");
        return;
    }
    let report = fixture
        .apply(vec![create("SRC/casey/x.ts", "x\n")])
        .expect("created");
    assert_eq!(report.directories_created, ["src/casey"]);
}
