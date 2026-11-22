//! E17-49 (D114): a grep with a path filter names where its patterns also
//! match in indexed files outside the filter.

use std::fs;
use std::path::{Path, PathBuf};

use compresso_atlas::{
    GREP_OUTSIDE_SCAN_MAX_FILES, GrepOptions, Indexer, OutsideMatches, grep, matches_outside_filter,
};
use compresso_core::{TokenProvider, counter_for};
use compresso_store::{FileLanguage, FileRecord, RepoId, Store};
use tempfile::TempDir;

struct Fixture {
    _directory: TempDir,
    root: PathBuf,
    store: Store,
}

/// The probe's shape: the controller holds the handler; the route passes
/// it to a wrapper at module scope, after a validation step.
const CONTROLLER: &str = "export default class AuthController {\n  static async login(req, res) {\n    res.send(\"ok\");\n  }\n}\n";
const ROUTES: &str = "import AuthController from \"../controllers/auth\";\nimport { wrapper } from \"../wrapper\";\n\nrouter.post(\"/login\", (req, res, next) => { Schema.handle(req, res, next, AuthValidator.login()) }, wrapper(AuthController.login));\n";

impl Fixture {
    fn new(files: &[(&str, &str)]) -> Self {
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

    fn repo(&self) -> RepoId {
        self.store
            .repo_by_root(&self.root)
            .expect("store")
            .expect("repo")
            .id
    }

    fn outside(&self, pattern: &str, path: Option<&str>) -> Option<OutsideMatches> {
        self.outside_with(&[pattern.to_owned()], path, &[])
    }

    fn outside_with(
        &self,
        patterns: &[String],
        path: Option<&str>,
        scopes: &[Option<String>],
    ) -> Option<OutsideMatches> {
        let options = GrepOptions {
            patterns,
            regex: false,
            case_insensitive: false,
            path,
            max_files: 20,
            max_matches_per_file: 5,
            context_lines: 0,
            scopes,
            select_rows: true,
        };
        let report = grep(&self.store, self.repo(), &options).expect("grep");
        matches_outside_filter(&self.store, self.repo(), &options, &report).expect("note")
    }
}

fn write(root: &Path, path: &str, contents: &str) {
    let target = root.join(path);
    fs::create_dir_all(target.parent().expect("parent")).expect("parent dir");
    fs::write(target, contents).expect("file");
}

fn named(outside: &OutsideMatches) -> Vec<String> {
    outside
        .files
        .iter()
        .map(|(path, matches)| format!("{path} ({matches})"))
        .collect()
}

fn auth_fixture() -> Fixture {
    Fixture::new(&[
        ("src/controllers/auth.ts", CONTROLLER),
        ("src/routes/auth.ts", ROUTES),
        (
            "src/session.ts",
            "export function establish(req, user) {\n  return req.login(user);\n}\n",
        ),
        ("src/other.ts", "export const x = 1;\n"),
    ])
}

#[test]
fn test_a_filtered_search_names_the_files_outside_where_the_pattern_matches() {
    let fixture = auth_fixture();
    let outside = fixture
        .outside("login", Some("src/controllers/**"))
        .expect("matches outside the filter");
    // Matching lines, most first, then by path: the route's module-scope
    // line names the handler, the session file only the word.
    assert_eq!(
        named(&outside),
        ["src/routes/auth.ts (1)", "src/session.ts (1)"]
    );
    assert_eq!((outside.files_total, outside.matches_total), (2, 2));
    assert!(!outside.at_least);
}

#[test]
fn test_no_note_without_a_filter_or_without_a_match_inside_or_outside() {
    let fixture = auth_fixture();
    assert_eq!(fixture.outside("login", None), None, "no filter");
    assert_eq!(
        fixture.outside("res.send", Some("src/controllers")),
        None,
        "nothing outside"
    );
    assert_eq!(
        fixture.outside("establish", Some("src/controllers")),
        None,
        "nothing inside"
    );
}

#[test]
fn test_a_pattern_scope_is_a_filter_too() {
    let fixture = auth_fixture();
    let outside = fixture
        .outside_with(
            &["login".to_owned()],
            None,
            &[Some("src/controllers".to_owned())],
        )
        .expect("the scope left matches out");
    assert_eq!(outside.files_total, 2);
}

#[test]
fn test_at_most_three_files_are_named_by_count_then_path() {
    let fixture = Fixture::new(&[
        ("in/a.ts", "needle\n"),
        ("out/b.ts", "needle\n"),
        ("out/a.ts", "needle\n"),
        ("out/c.ts", "needle\nneedle\nneedle\n"),
        ("out/d.ts", "needle\nneedle\n"),
    ]);
    let outside = fixture.outside("needle", Some("in")).expect("outside");
    assert_eq!(
        named(&outside),
        ["out/c.ts (3)", "out/d.ts (2)", "out/a.ts (1)"]
    );
    assert_eq!((outside.files_total, outside.matches_total), (4, 7));
}

#[test]
fn test_an_excluded_or_secret_file_is_never_counted_or_named() {
    let fixture = Fixture::new(&[
        ("src/controllers/auth.ts", CONTROLLER),
        ("generated/use.ts", "login();\n"),
        ("src/app.ts", "login();\n"),
    ]);
    // Now ignored, while the index still holds the file.
    write(&fixture.root, ".gitignore", "generated/\n");
    // A secret path's row, written directly: the scanner never indexes one.
    let source = "login();\n";
    write(&fixture.root, "config/.env.local", source);
    fixture
        .store
        .replace_indexed_file(
            fixture.repo(),
            &FileRecord {
                path: "config/.env.local".to_owned(),
                hash: blake3::hash(source.as_bytes()).to_hex().to_string(),
                language: Some(FileLanguage::TypeScript),
                size_bytes: source.len() as u64,
                token_estimate: 2,
                indexed_at: 10,
            },
            &[],
            &[],
        )
        .expect("secret row");
    // A binary file is never indexed, so never read.
    fs::write(fixture.root.join("blob.bin"), b"login\0\x01").expect("binary");
    let outside = fixture
        .outside("login", Some("src/controllers"))
        .expect("src/app.ts is outside");
    assert_eq!(named(&outside), ["src/app.ts (1)"]);
    assert_eq!(outside.files_total, 1);
}

#[test]
fn test_the_outside_scan_stays_within_its_file_bound() {
    assert_eq!(GREP_OUTSIDE_SCAN_MAX_FILES, 2_000);
    let fixture = auth_fixture();
    let outside = fixture
        .outside("login", Some("src/controllers"))
        .expect("outside");
    // Only the three files outside the filter were read.
    assert_eq!(outside.files_scanned, 3);
}
