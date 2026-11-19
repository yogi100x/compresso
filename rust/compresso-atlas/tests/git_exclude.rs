//! The repository's own `info/exclude` and nested linked worktrees (E17-35,
//! D100). Fixtures are written directly; no test needs a git binary.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use compresso_atlas::{IndexReport, Indexer, is_scanner_excluded};
use compresso_core::{TokenProvider, counter_for};
use compresso_store::Store;
use tempfile::TempDir;

struct Workspace {
    _directory: TempDir,
    base: PathBuf,
    store: Store,
}

impl Workspace {
    fn new() -> Self {
        let directory = TempDir::new().expect("temporary directory");
        let base = fs::canonicalize(directory.path()).expect("canonical temp");
        let store = Store::open(&base.join("store.db")).expect("store");
        Self {
            _directory: directory,
            base,
            store,
        }
    }

    fn write(&self, path: &str, contents: &str) {
        let target = self.base.join(path);
        fs::create_dir_all(target.parent().expect("parent")).expect("parent dir");
        fs::write(target, contents).expect("file");
    }

    fn index(&self, root: &str) -> IndexReport {
        Indexer::new(&self.store, counter_for(TokenProvider::OpenAiO200k))
            .index(&self.base.join(root), |_| {})
            .expect("index")
    }

    fn indexed(&self, root: &str) -> BTreeSet<String> {
        let root = fs::canonicalize(self.base.join(root)).expect("root");
        let repo = self
            .store
            .repo_by_root(&root)
            .expect("store")
            .expect("repo")
            .id;
        self.store
            .file_hashes(repo)
            .expect("hashes")
            .into_keys()
            .collect()
    }

    /// The scan and the single-path check used by the read and edit gates
    /// must agree on every file of the fixture.
    fn assert_parity(&self, root: &str) {
        let indexed = self.indexed(root);
        let root_path = self.base.join(root);
        for relative in files_under(&root_path) {
            let excluded = is_scanner_excluded(&root_path, Path::new(&relative));
            assert_eq!(
                indexed.contains(&relative),
                !excluded,
                "scan and single-path check disagree on {relative}"
            );
        }
    }
}

/// Every regular file under `root` except the contents of `.git` directories,
/// as forward-slash relative paths.
fn files_under(root: &Path) -> Vec<String> {
    let mut found = Vec::new();
    let mut pending = vec![PathBuf::new()];
    while let Some(relative) = pending.pop() {
        for entry in fs::read_dir(root.join(&relative)).expect("directory") {
            let entry = entry.expect("entry");
            let path = relative.join(entry.file_name());
            let kind = entry.file_type().expect("type");
            if kind.is_dir() {
                if entry.file_name() != ".git" {
                    pending.push(path);
                }
            } else if kind.is_file() {
                let text = path
                    .components()
                    .map(|component| component.as_os_str().to_str().expect("utf-8"))
                    .collect::<Vec<_>>()
                    .join("/");
                found.push(text);
            }
        }
    }
    found.sort();
    found
}

#[test]
fn test_info_exclude_is_honoured_by_scan_and_single_path_check() {
    let workspace = Workspace::new();
    workspace.write(
        "repo/.git/info/exclude",
        "\u{feff}*.log\r\n# local rules\r\n**/.claude/worktrees/\r\n",
    );
    workspace.write("repo/.git/HEAD", "ref: refs/heads/main\n");
    workspace.write("repo/src/lib.rs", "pub fn kept() {}\n");
    workspace.write("repo/build.log", "noise\n");
    workspace.write("repo/nested/trace.log", "noise\n");
    workspace.write(
        "repo/.claude/worktrees/other/src/lib.rs",
        "pub fn kept() {}\n",
    );
    // A `.gitignore` rule outranks `info/exclude`, as in git.
    workspace.write("repo/.gitignore", "!keep.log\n");
    workspace.write("repo/keep.log", "kept\n");

    workspace.index("repo");
    let indexed = workspace.indexed("repo");
    assert!(indexed.contains("src/lib.rs"));
    assert!(indexed.contains("keep.log"));
    for excluded in [
        "build.log",
        "nested/trace.log",
        ".claude/worktrees/other/src/lib.rs",
    ] {
        assert!(!indexed.contains(excluded), "{excluded} was indexed");
        assert!(is_scanner_excluded(
            &workspace.base.join("repo"),
            Path::new(excluded)
        ));
    }
    workspace.assert_parity("repo");
}

#[test]
fn test_reindex_removes_rows_for_newly_excluded_files() {
    let workspace = Workspace::new();
    workspace.write("repo/.git/HEAD", "ref: refs/heads/main\n");
    workspace.write("repo/src/lib.rs", "pub fn kept() {}\n");
    workspace.write("repo/.claude/worktrees/a/src/lib.rs", "pub fn kept() {}\n");
    workspace.write("repo/.claude/worktrees/b/src/lib.rs", "pub fn kept() {}\n");
    workspace.index("repo");
    assert_eq!(workspace.indexed("repo").len(), 3);

    workspace.write("repo/.git/info/exclude", "**/.claude/worktrees/\n");
    let report = workspace.index("repo");
    assert_eq!(report.files_removed, 2);
    assert_eq!(
        workspace.indexed("repo"),
        BTreeSet::from(["src/lib.rs".to_owned()])
    );
}

#[test]
fn test_linked_worktree_root_reads_exclude_from_the_common_dir() {
    let workspace = Workspace::new();
    let common = workspace.base.join("main/.git");
    workspace.write("main/.git/HEAD", "ref: refs/heads/main\n");
    workspace.write("main/.git/info/exclude", "*.log\n");
    workspace.write("main/.git/worktrees/abs/commondir", "../..\n");
    workspace.write("main/.git/worktrees/rel/commondir", "../..\r\n");
    // The worktrees sit inside the main checkout (as under
    // `.claude/worktrees/`). One pointer is absolute, written the way git
    // writes it (forward slashes, no verbatim prefix), the other relative,
    // as with `worktree.useRelativePaths`.
    let absolute = common.join("worktrees").join("abs");
    let absolute = absolute.to_str().expect("utf-8");
    let absolute = absolute
        .strip_prefix(r"\\?\")
        .unwrap_or(absolute)
        .replace('\\', "/");
    workspace.write("main/trees/abs/.git", &format!("gitdir: {absolute}\n"));
    workspace.write(
        "main/trees/rel/.git",
        "gitdir: ../../.git/worktrees/rel\r\n",
    );
    for root in ["main/trees/abs", "main/trees/rel"] {
        workspace.write(&format!("{root}/src/lib.rs"), "pub fn kept() {}\n");
        workspace.write(&format!("{root}/out.log"), "noise\n");
        let report = workspace.index(root);
        assert!(!report.git_exclude_unusable, "{root}");
        assert_eq!(
            workspace.indexed(root),
            BTreeSet::from(["src/lib.rs".to_owned()]),
            "{root}"
        );
        workspace.assert_parity(root);
    }
}

#[test]
fn test_a_common_dir_outside_the_roots_ancestors_is_never_read() {
    let workspace = Workspace::new();
    workspace.write("main/.git/HEAD", "ref: refs/heads/main\n");
    workspace.write("main/.git/info/exclude", "*.log\n");
    workspace.write("main/.git/worktrees/wt/commondir", "../..\n");
    // A sibling worktree: its common directory is not below any ancestor of
    // the root, so no exclude rules apply (fail open) and the scan says so.
    workspace.write("wt/.git", "gitdir: ../main/.git/worktrees/wt\n");
    workspace.write("wt/src/lib.rs", "pub fn kept() {}\n");
    workspace.write("wt/out.log", "noise\n");
    let report = workspace.index("wt");
    assert!(report.git_exclude_unusable);
    assert_eq!(
        workspace.indexed("wt"),
        BTreeSet::from(["out.log".to_owned(), "src/lib.rs".to_owned()])
    );
    workspace.assert_parity("wt");
}

#[test]
fn test_network_and_device_pointers_fail_open_and_are_flagged() {
    let workspace = Workspace::new();
    for (index, pointer) in [
        "gitdir: //host/share/repo/.git/worktrees/x",
        "gitdir: \\\\host\\share\\repo\\.git\\worktrees\\x",
        "gitdir: \\\\?\\UNC\\host\\share\\repo\\.git\\worktrees\\x",
        "gitdir: \\\\.\\pipe\\repo",
        "gitdir: C:relative\\.git\\worktrees\\x",
    ]
    .iter()
    .enumerate()
    {
        let root = format!("r{index}");
        workspace.write(&format!("{root}/.git"), &format!("{pointer}\n"));
        workspace.write(&format!("{root}/src/lib.rs"), "pub fn kept() {}\n");
        let report = workspace.index(&root);
        assert!(report.git_exclude_unusable, "{pointer}");
        assert_eq!(
            workspace.indexed(&root),
            BTreeSet::from(["src/lib.rs".to_owned()]),
            "{pointer}"
        );
    }
}

#[test]
fn test_pointer_whitespace_is_part_of_the_path_as_in_git() {
    let workspace = Workspace::new();
    workspace.write("super/.git/HEAD", "ref: refs/heads/main\n");
    workspace.write("super/.git/modules/sub/info/exclude", "*.log\n");
    // Git strips only line endings, so this names `sub<TAB>`, which does not
    // exist: no rules.
    workspace.write("super/sub/.git", "gitdir: ../.git/modules/sub\t\r\n");
    workspace.write("super/sub/out.log", "noise\n");
    workspace.index("super/sub");
    assert!(workspace.indexed("super/sub").contains("out.log"));
    workspace.assert_parity("super/sub");
}

#[test]
fn test_oversized_and_unparsable_excludes_are_flagged() {
    let workspace = Workspace::new();
    workspace.write("big/.git/HEAD", "ref: refs/heads/main\n");
    workspace.write("big/.git/info/exclude", &"*.log\n".repeat(200_000));
    workspace.write("big/out.log", "noise\n");
    let report = workspace.index("big");
    assert!(report.git_exclude_unusable);
    assert!(workspace.indexed("big").contains("out.log"));

    workspace.write("bad/.git/HEAD", "ref: refs/heads/main\n");
    workspace.write("bad/.git/info/exclude", "x[z-a]\n*.log\n");
    workspace.write("bad/out.log", "noise\n");
    workspace.write("bad/src/lib.rs", "pub fn kept() {}\n");
    let report = workspace.index("bad");
    assert!(report.git_exclude_unusable);
    // The parsable rules still apply.
    assert_eq!(
        workspace.indexed("bad"),
        BTreeSet::from(["src/lib.rs".to_owned()])
    );

    workspace.write("clean/.git/HEAD", "ref: refs/heads/main\n");
    workspace.write("clean/src/lib.rs", "pub fn kept() {}\n");
    assert!(!workspace.index("clean").git_exclude_unusable);
}

#[test]
fn test_git_entries_are_never_indexed_in_any_case() {
    let workspace = Workspace::new();
    workspace.write("repo/src/lib.rs", "pub fn kept() {}\n");
    workspace.write("repo/a/.GIT", "gitdir: ../.git/modules/a\n");
    workspace.write("repo/a/src/lib.rs", "pub fn kept() {}\n");
    workspace.write("repo/b/.Git/HEAD", "ref: refs/heads/main\n");
    workspace.write("repo/b/src/lib.rs", "pub fn kept() {}\n");
    workspace.index("repo");
    assert_eq!(
        workspace.indexed("repo"),
        BTreeSet::from([
            "a/src/lib.rs".to_owned(),
            "b/src/lib.rs".to_owned(),
            "src/lib.rs".to_owned(),
        ])
    );
    workspace.assert_parity("repo");
}

#[cfg(unix)]
#[test]
fn test_symlinked_or_special_exclude_paths_are_not_followed() {
    use std::os::unix::fs::symlink;

    let workspace = Workspace::new();
    // `info` is a link to a directory holding an exclude file.
    workspace.write("elsewhere/exclude", "*.log\n");
    workspace.write("linked-dir/.git/HEAD", "ref: refs/heads/main\n");
    symlink(
        workspace.base.join("elsewhere"),
        workspace.base.join("linked-dir/.git/info"),
    )
    .expect("symlink");
    // `exclude` itself is a link.
    workspace.write("linked-file/.git/info/.keep", "");
    symlink(
        workspace.base.join("elsewhere/exclude"),
        workspace.base.join("linked-file/.git/info/exclude"),
    )
    .expect("symlink");
    // `exclude` is a FIFO: it must be neither opened for a blocking read nor
    // applied.
    workspace.write("fifo/.git/info/.keep", "");
    let fifo = std::ffi::CString::new(
        workspace
            .base
            .join("fifo/.git/info/exclude")
            .to_str()
            .expect("utf-8"),
    )
    .expect("path");
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    for root in ["linked-dir", "linked-file", "fifo"] {
        workspace.write(&format!("{root}/out.log"), "noise\n");
        let report = workspace.index(root);
        assert!(report.git_exclude_unusable, "{root}");
        assert!(workspace.indexed(root).contains("out.log"), "{root}");
        workspace.assert_parity(root);
    }
}

#[test]
fn test_submodule_root_reads_exclude_from_its_own_gitdir() {
    let workspace = Workspace::new();
    workspace.write("super/.git/HEAD", "ref: refs/heads/main\n");
    workspace.write("super/.git/modules/sub/HEAD", "ref: refs/heads/main\n");
    workspace.write("super/.git/modules/sub/info/exclude", "*.log\n");
    workspace.write("super/sub/.git", "gitdir: ../.git/modules/sub\n");
    workspace.write("super/sub/src/lib.rs", "pub fn kept() {}\n");
    workspace.write("super/sub/out.log", "noise\n");
    workspace.index("super/sub");
    assert_eq!(
        workspace.indexed("super/sub"),
        BTreeSet::from(["src/lib.rs".to_owned()])
    );
    workspace.assert_parity("super/sub");
}

#[test]
fn test_nested_linked_worktree_is_skipped_by_its_pointer_alone() {
    let workspace = Workspace::new();
    workspace.write("repo/.git/HEAD", "ref: refs/heads/main\n");
    workspace.write("repo/src/lib.rs", "pub fn kept() {}\n");
    // Neither pointer target exists: the decision is lexical and never opens it.
    workspace.write(
        "repo/copies/unix/.git",
        "gitdir: /nonexistent/elsewhere/.git/worktrees/unix\n",
    );
    workspace.write("repo/copies/unix/src/lib.rs", "pub fn kept() {}\n");
    workspace.write(
        "repo/copies/windows/.git",
        "gitdir: C:\\nonexistent\\project\\.git\\worktrees\\windows\r\n",
    );
    workspace.write("repo/copies/windows/src/lib.rs", "pub fn kept() {}\n");

    workspace.index("repo");
    assert_eq!(
        workspace.indexed("repo"),
        BTreeSet::from(["src/lib.rs".to_owned()])
    );
    workspace.assert_parity("repo");
}

#[test]
fn test_submodules_nested_repositories_and_malformed_pointers_are_indexed() {
    let workspace = Workspace::new();
    workspace.write("repo/.git/HEAD", "ref: refs/heads/main\n");
    let pointers = [
        ("sub", "gitdir: ../.git/modules/sub\n".to_owned()),
        // A submodule whose name ends like a worktree path is still a module.
        (
            "deep",
            "gitdir: ../.git/modules/x/.git/worktrees/deep\n".to_owned(),
        ),
        ("garbage", "not a pointer\n".to_owned()),
        ("empty", String::new()),
        ("noprefix", "../.git/worktrees/noprefix\n".to_owned()),
        (
            "oversized",
            format!("gitdir: /{}/.git/worktrees/oversized\n", "a".repeat(70_000)),
        ),
    ];
    for (directory, pointer) in &pointers {
        workspace.write(&format!("repo/{directory}/.git"), pointer);
        workspace.write(&format!("repo/{directory}/src/lib.rs"), "pub fn x() {}\n");
    }
    workspace.write("repo/vendored/.git/HEAD", "ref: refs/heads/main\n");
    workspace.write("repo/vendored/src/lib.rs", "pub fn x() {}\n");

    workspace.index("repo");
    let indexed = workspace.indexed("repo");
    for (directory, _) in &pointers {
        assert!(
            indexed.contains(&format!("{directory}/src/lib.rs")),
            "{directory} was skipped"
        );
        // A `.git` pointer file is git metadata, never repository content.
        assert!(!indexed.contains(&format!("{directory}/.git")));
    }
    assert!(indexed.contains("vendored/src/lib.rs"));
    workspace.assert_parity("repo");
}
