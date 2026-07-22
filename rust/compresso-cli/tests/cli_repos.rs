use std::ffi::OsString;
use std::fs;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};

use clap::Parser;
use compresso_cli::{Cli, CliExitCode, UnavailableAccountClient, execute_with_store_and_input};
use compresso_store::{FileLanguage, FileRecord, Store, StoreError};
use serde_json::Value;
use tempfile::TempDir;

#[test]
fn test_repos_list_is_deterministic_in_human_and_json_modes() {
    let fixture = Fixture::new();
    let zeta = fixture.directory.path().join("zeta");
    let alpha = fixture.directory.path().join("alpha");
    fs::create_dir(&zeta).expect("zeta repository should be created");
    fs::create_dir(&alpha).expect("alpha repository should be created");
    fixture
        .store
        .approve_repo(&zeta)
        .expect("zeta should approve");
    let alpha_id = fixture
        .store
        .approve_repo(&alpha)
        .expect("alpha should approve");
    fixture
        .store
        .stamp_index_version(alpha_id, "parser-v1", 1)
        .expect("alpha index version should stamp");

    let (exit, output, errors) = fixture.run_json(&["repos", "list", "--json"], "");

    assert_eq!(exit, CliExitCode::Success);
    assert!(errors.is_null());
    assert_eq!(output["schema_version"], 1);
    assert_eq!(output["data"]["action"], "list");
    let repositories = output["data"]["repositories"]
        .as_array()
        .expect("repositories should be an array");
    assert_eq!(repositories.len(), 2);
    assert_eq!(repositories[0]["root"], canonical_text(&alpha));
    assert_eq!(repositories[0]["indexed"], true);
    assert_eq!(repositories[0]["parser_version"], "parser-v1");
    assert_eq!(repositories[1]["root"], canonical_text(&zeta));
    assert_eq!(repositories[1]["indexed"], false);

    let (exit, output, errors) = fixture.run(&["repos", "list"], "");
    assert_eq!(exit, CliExitCode::Success);
    assert!(errors.is_empty());
    assert_eq!(
        output,
        format!(
            "Approved repositories:\n{} (indexed)\n{} (not indexed)\n",
            canonical_text(&alpha),
            canonical_text(&zeta)
        )
    );
}

#[test]
fn test_repos_list_reports_empty_store() {
    let fixture = Fixture::new();

    let (exit, output, errors) = fixture.run(&["repos", "list"], "");

    assert_eq!(exit, CliExitCode::Success);
    assert_eq!(output, "No approved repositories.\n");
    assert!(errors.is_empty());
}

#[test]
fn test_remove_repo_dry_run_is_write_free_and_never_prefix_matches() {
    let fixture = Fixture::new();
    let root = fixture.directory.path().join("repo");
    let adjacent = fixture.directory.path().join("repo-copy");
    fs::create_dir(&root).expect("repository should be created");
    fs::create_dir(&adjacent).expect("adjacent repository should be created");
    fixture
        .store
        .approve_repo(&root)
        .expect("root should approve");
    fixture
        .store
        .approve_repo(&adjacent)
        .expect("adjacent root should approve");
    let changes_before = fixture
        .store
        .total_changes()
        .expect("change count should load");

    let (exit, output, errors) = fixture.run_json(
        &[
            "repos",
            "remove",
            &canonical_text(&root),
            "--dry-run",
            "--json",
        ],
        "",
    );

    assert_eq!(exit, CliExitCode::Success);
    assert!(errors.is_null());
    assert_eq!(output["data"]["action"], "remove");
    assert_eq!(output["data"]["dry_run"], true);
    assert_eq!(output["data"]["removed"], false);
    assert_eq!(output["data"]["repository"]["root"], canonical_text(&root));
    assert_eq!(
        fixture
            .store
            .total_changes()
            .expect("change count should load"),
        changes_before
    );

    let prefix = root.join("child-that-does-not-exist");
    let (exit, output, errors) = fixture.run_json(
        &[
            "repos",
            "remove",
            prefix.to_str().expect("temporary path should be UTF-8"),
            "--yes",
            "--json",
        ],
        "",
    );
    assert_eq!(exit, CliExitCode::Failure);
    assert!(output.is_null());
    assert_eq!(errors["error"]["code"], "repository_not_found");
    assert_eq!(
        fixture
            .store
            .approved_repositories()
            .expect("repositories should list")
            .len(),
        2
    );
}

#[test]
fn test_remove_repo_requires_confirmation_and_cascades_when_root_is_missing() {
    let fixture = Fixture::new();
    let root = fixture.directory.path().join("repo");
    fs::create_dir(&root).expect("repository should be created");
    let canonical = fs::canonicalize(&root).expect("repository should canonicalize");
    let repo = fixture
        .store
        .approve_repo(&root)
        .expect("root should approve");
    fixture
        .store
        .upsert_file(
            repo,
            &FileRecord {
                path: "src/lib.rs".to_owned(),
                hash: "hash".to_owned(),
                language: Some(FileLanguage::Rust),
                size_bytes: 20,
                token_estimate: 5,
                indexed_at: 1,
            },
        )
        .expect("file should index");
    fixture
        .store
        .stamp_index_version(repo, "parser-v1", 1)
        .expect("version should stamp");
    fs::remove_dir(&root).expect("missing-root scenario should be created");
    let root_text = canonical.to_str().expect("temporary path should be UTF-8");

    let (exit, output, errors) = fixture.run_json(&["repos", "remove", root_text, "--json"], "");
    assert_eq!(exit, CliExitCode::Failure);
    assert_eq!(output["data"]["removed"], false);
    assert_eq!(errors["error"]["code"], "confirmation_required");
    assert!(
        fixture
            .store
            .repo_by_root(&canonical)
            .expect("repository lookup should succeed")
            .is_some()
    );

    let (exit, output, errors) =
        fixture.run_json(&["repos", "remove", root_text, "--yes", "--json"], "");
    assert_eq!(exit, CliExitCode::Success);
    assert!(errors.is_null());
    assert_eq!(output["data"]["removed"], true);
    assert_eq!(output["data"]["repository"]["root"], root_text);
    assert!(
        fixture
            .store
            .repo_by_root(&canonical)
            .expect("repository lookup should succeed")
            .is_none()
    );
    assert!(matches!(
        fixture.store.indexed_files(repo),
        Err(StoreError::RepositoryNotFound(id)) if id == repo
    ));
}

#[test]
fn test_remove_repo_human_confirmation_can_decline_or_approve() {
    let fixture = Fixture::new();
    let root = fixture.directory.path().join("repo");
    fs::create_dir(&root).expect("repository should be created");
    let source = root.join("source.rs");
    fs::write(&source, "pub fn stays_on_disk() {}\n").expect("repository source should be written");
    fixture
        .store
        .approve_repo(&root)
        .expect("root should approve");
    let canonical = fs::canonicalize(&root).expect("repository should canonicalize");
    let root_text = canonical.to_str().expect("temporary path should be UTF-8");

    let (exit, output, errors) = fixture.run(&["repos", "remove", root_text], "no\n");
    assert_eq!(exit, CliExitCode::Success);
    assert_eq!(
        output,
        format!(
            "Would remove approval and indexed metadata for {root_text}.\nCancelled; repository remains approved.\n"
        )
    );
    assert_eq!(errors, "Remove this repository? [y/N] ");
    assert!(
        fixture
            .store
            .repo_by_root(&canonical)
            .expect("repository lookup should succeed")
            .is_some()
    );

    let (exit, output, errors) = fixture.run(&["repos", "remove", root_text], "yes\n");
    assert_eq!(exit, CliExitCode::Success);
    assert_eq!(
        output,
        format!(
            "Would remove approval and indexed metadata for {root_text}.\nRemoved approval and indexed metadata for {root_text}.\n"
        )
    );
    assert_eq!(errors, "Remove this repository? [y/N] ");
    assert_eq!(
        fs::read_to_string(source).expect("source should remain on disk"),
        "pub fn stays_on_disk() {}\n"
    );
}

#[cfg(unix)]
#[test]
fn test_remove_prefers_exact_stored_root_when_stale_path_now_links_to_another_repo() {
    let fixture = Fixture::new();
    let root_a = fixture.directory.path().join("repo-a");
    let root_b = fixture.directory.path().join("repo-b");
    fs::create_dir(&root_a).expect("repository A should be created");
    fs::create_dir(&root_b).expect("repository B should be created");
    fs::write(root_a.join("a.rs"), "pub fn source_a() {}\n")
        .expect("repository A source should be written");
    fs::write(root_b.join("b.rs"), "pub fn source_b() {}\n")
        .expect("repository B source should be written");
    let canonical_a = fs::canonicalize(&root_a).expect("repository A should canonicalize");
    let canonical_b = fs::canonicalize(&root_b).expect("repository B should canonicalize");
    fixture
        .store
        .approve_repo(&canonical_a)
        .expect("repository A should approve");
    fixture
        .store
        .approve_repo(&canonical_b)
        .expect("repository B should approve");

    let parked_a = fixture.directory.path().join("repo-a-moved");
    fs::rename(&root_a, &parked_a).expect("repository A should be moved");
    std::os::unix::fs::symlink(&canonical_b, &root_a)
        .expect("stale repository A path should link to repository B");
    assert_eq!(
        fs::canonicalize(&root_a).expect("replacement symlink should resolve"),
        canonical_b
    );

    let root_a_text = canonical_a
        .to_str()
        .expect("temporary path should be UTF-8");
    let (exit, output, errors) =
        fixture.run_json(&["repos", "remove", root_a_text, "--yes", "--json"], "");

    assert_eq!(exit, CliExitCode::Success, "{errors}");
    assert!(errors.is_null());
    assert_eq!(output["data"]["repository"]["root"], root_a_text);
    assert!(
        fixture
            .store
            .repo_by_root(&canonical_a)
            .expect("repository A lookup should succeed")
            .is_none(),
        "the exact stale approval should be removed"
    );
    assert!(
        fixture
            .store
            .repo_by_root(&canonical_b)
            .expect("repository B lookup should succeed")
            .is_some(),
        "the symlink target approval must remain"
    );
    assert_eq!(
        fs::read_to_string(parked_a.join("a.rs")).expect("source A should remain"),
        "pub fn source_a() {}\n"
    );
    assert_eq!(
        fs::read_to_string(root_b.join("b.rs")).expect("source B should remain"),
        "pub fn source_b() {}\n"
    );
    assert!(
        fs::symlink_metadata(&root_a)
            .expect("replacement symlink should remain")
            .file_type()
            .is_symlink()
    );
}

#[test]
fn test_remove_repo_confirmation_rejects_reapproved_generation() {
    let fixture = Fixture::new();
    let root = fixture.directory.path().join("repo");
    fs::create_dir(&root).expect("repository should be created");
    let source = root.join("source.rs");
    fs::write(&source, "pub fn stays_on_disk() {}\n").expect("repository source should be written");
    let original_id = fixture
        .store
        .approve_repo(&root)
        .expect("root should approve");
    let canonical = fs::canonicalize(&root).expect("repository should canonicalize");
    let root_text = canonical.to_str().expect("temporary path should be UTF-8");
    let external =
        Store::open(&fixture.database_path()).expect("independent store connection should open");
    let mut input = ReapproveOnFirstRead {
        input: Cursor::new(b"yes\n".to_vec()),
        store: external,
        root: canonical.clone(),
        replacement_id: None,
    };

    let (exit, output, errors) =
        fixture.run_with_reader(&["repos", "remove", root_text], &mut input);

    assert_eq!(exit, CliExitCode::Failure);
    assert_eq!(
        output,
        format!("Would remove approval and indexed metadata for {root_text}.\n")
    );
    assert!(errors.starts_with("Remove this repository? [y/N] "));
    assert!(errors.contains("error[repository_changed]"));
    assert!(errors.contains("nothing was removed"));
    let replacement_id = input
        .replacement_id
        .expect("reader should replace the approval while confirmation is open");
    assert_eq!(
        replacement_id, original_id,
        "SQLite should reuse the sole row id"
    );
    assert_eq!(
        fixture
            .store
            .repo_by_root(&canonical)
            .expect("replacement lookup should succeed")
            .expect("replacement approval should remain")
            .id,
        replacement_id
    );
    assert_eq!(
        fs::read_to_string(source).expect("repository source should remain"),
        "pub fn stays_on_disk() {}\n"
    );
}

struct ReapproveOnFirstRead {
    input: Cursor<Vec<u8>>,
    store: Store,
    root: PathBuf,
    replacement_id: Option<i64>,
}

impl Read for ReapproveOnFirstRead {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if self.replacement_id.is_none() {
            self.store
                .remove_repo(&self.root)
                .expect("approval mutation should succeed")
                .expect("previewed approval should still exist before mutation");
            self.replacement_id = Some(
                self.store
                    .approve_repo(&self.root)
                    .expect("repository should re-approve"),
            );
        }
        self.input.read(buffer)
    }
}

struct Fixture {
    directory: TempDir,
    cwd: PathBuf,
    store: Store,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let cwd = directory.path().join("cwd");
        fs::create_dir(&cwd).expect("working directory should be created");
        let store =
            Store::open(&directory.path().join("state/compresso.db")).expect("store should open");
        Self {
            directory,
            cwd,
            store,
        }
    }

    fn run(&self, arguments: &[&str], input: &str) -> (CliExitCode, String, String) {
        self.run_with_reader(arguments, &mut Cursor::new(input.as_bytes()))
    }

    fn run_with_reader(
        &self,
        arguments: &[&str],
        input: &mut dyn Read,
    ) -> (CliExitCode, String, String) {
        let argv = std::iter::once(OsString::from("compresso"))
            .chain(arguments.iter().map(OsString::from))
            .collect::<Vec<_>>();
        let cli = Cli::try_parse_from(argv).expect("command should parse");
        let mut output = Vec::new();
        let mut errors = Vec::new();
        let exit = execute_with_store_and_input(
            cli,
            &UnavailableAccountClient,
            &self.store,
            &self.cwd,
            input,
            &mut output,
            &mut errors,
        );
        (
            exit,
            String::from_utf8(output).expect("stdout should be UTF-8"),
            String::from_utf8(errors).expect("stderr should be UTF-8"),
        )
    }

    fn database_path(&self) -> PathBuf {
        self.directory.path().join("state/compresso.db")
    }

    fn run_json(&self, arguments: &[&str], input: &str) -> (CliExitCode, Value, Value) {
        let (exit, output, errors) = self.run(arguments, input);
        (exit, parse_or_null(&output), parse_or_null(&errors))
    }
}

fn canonical_text(path: &Path) -> String {
    fs::canonicalize(path)
        .expect("path should canonicalize")
        .to_string_lossy()
        .into_owned()
}

fn parse_or_null(value: &str) -> Value {
    if value.is_empty() {
        Value::Null
    } else {
        serde_json::from_str(value).expect("command output should be JSON")
    }
}
