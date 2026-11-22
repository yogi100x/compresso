//! E17-49 (D114): a grep with a path filter names the use sites, outside
//! the filter, of the definitions it listed inside it.

use std::fs;
use std::path::{Path, PathBuf};

use compresso_atlas::{
    GREP_OUTSIDE_DEFINITIONS, GREP_OUTSIDE_SITES_PER_NAME, GrepOptions, Indexer, OutsideUses, grep,
    use_sites_outside_filter,
};
use compresso_core::{TokenProvider, counter_for};
use compresso_store::{
    CallForm, CallSiteRecord, FileLanguage, FileRecord, RepoId, Store, SymbolKind, SymbolRecord,
};
use tempfile::TempDir;

struct Fixture {
    _directory: TempDir,
    root: PathBuf,
    store: Store,
}

const HANDLER: &str =
    "export function handleLogin(request, response) {\n  return response.ok();\n}\n";
const ROUTES: &str = "import { handleLogin } from \"../handlers/auth\";\nimport { validate } from \"../validate\";\n\nexport function routes(router) {\n  router.post(\"/login\", validate, handleLogin);\n}\n";
const APP: &str = "import { handleLogin } from \"./handlers/auth\";\n\nexport function start(request, response) {\n  return handleLogin(request, response);\n}\n";

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

    fn repo(&self) -> RepoId {
        self.store
            .repo_by_root(&self.root)
            .expect("store")
            .expect("repo")
            .id
    }

    fn outside(&self, pattern: &str, path: Option<&str>) -> Option<OutsideUses> {
        let patterns = [pattern.to_owned()];
        self.outside_with(&patterns, path, &[], true)
    }

    fn outside_with(
        &self,
        patterns: &[String],
        path: Option<&str>,
        scopes: &[Option<String>],
        regex: bool,
    ) -> Option<OutsideUses> {
        let options = GrepOptions {
            patterns,
            regex,
            case_insensitive: false,
            path,
            max_files: 20,
            max_matches_per_file: 5,
            context_lines: 0,
            scopes,
            select_rows: true,
        };
        let report = grep(&self.store, self.repo(), &options).expect("grep");
        use_sites_outside_filter(&self.store, self.repo(), &options, &report).expect("hint")
    }
}

fn write(root: &Path, path: &str, contents: &str) {
    let target = root.join(path);
    fs::create_dir_all(target.parent().expect("parent")).expect("parent dir");
    fs::write(target, contents).expect("file");
}

fn sites(outside: &OutsideUses) -> Vec<String> {
    outside
        .sites
        .iter()
        .map(|site| format!("{}:{} {}", site.path, site.line, site.name))
        .collect()
}

fn auth_fixture() -> Fixture {
    Fixture::new(&[
        ("src/handlers/auth.ts", HANDLER),
        ("src/routes/auth.ts", ROUTES),
        ("src/app.ts", APP),
        (
            "src/validate.ts",
            "export function validate(request) {\n  return true;\n}\n",
        ),
        (
            "src/unused.ts",
            "export function unusedHelper() {\n  return 1;\n}\n",
        ),
    ])
}

#[test]
fn test_a_filtered_definition_names_its_use_sites_outside_the_filter() {
    let fixture = auth_fixture();
    let outside = fixture
        .outside("function handleLogin", Some("src/handlers"))
        .expect("use sites outside the filter");
    assert_eq!(outside.total, 2, "{outside:?}");
    assert!(!outside.at_least);
    assert_eq!(
        sites(&outside),
        [
            "src/app.ts:4 handleLogin",
            "src/routes/auth.ts:5 handleLogin"
        ]
    );
}

#[test]
fn test_no_hint_without_a_path_filter() {
    let fixture = auth_fixture();
    assert_eq!(fixture.outside("function handleLogin", None), None);
}

#[test]
fn test_no_hint_when_every_use_site_is_inside_the_filter() {
    let fixture = auth_fixture();
    // `src` holds the definition and both use sites.
    assert_eq!(fixture.outside("function handleLogin", Some("src")), None);
    // A definition without any use site gives no hint either.
    assert_eq!(
        fixture.outside("function unusedHelper", Some("src/unused.ts")),
        None
    );
    // A use-site line alone (no definition listed) gives no hint.
    assert_eq!(
        fixture.outside("handleLogin\\(request", Some("src/app.ts")),
        None
    );
}

#[test]
fn test_a_pattern_scope_is_a_filter_too() {
    let fixture = auth_fixture();
    let patterns = ["function handleLogin".to_owned()];
    let scopes = [Some("src/handlers".to_owned())];
    let outside = fixture
        .outside_with(&patterns, None, &scopes, false)
        .expect("the scope hid the use sites");
    assert_eq!(outside.total, 2);
}

#[test]
fn test_a_use_site_in_an_excluded_or_secret_file_is_never_named() {
    let fixture = Fixture::new(&[
        ("src/handlers/auth.ts", HANDLER),
        ("generated/use.ts", APP),
        ("src/app.ts", APP),
    ]);
    // Now ignored, while the index still holds the file's rows.
    write(&fixture.root, ".gitignore", "generated/\n");
    // A secret path's rows, written directly: the scanner never indexes one.
    let source = "handleLogin(a, b);\n";
    fixture
        .store
        .replace_indexed_file_with_calls(
            fixture.repo(),
            &FileRecord {
                path: "config/.env.local".to_owned(),
                hash: blake3::hash(source.as_bytes()).to_hex().to_string(),
                language: Some(FileLanguage::TypeScript),
                size_bytes: source.len() as u64,
                token_estimate: 5,
                indexed_at: 10,
            },
            &[SymbolRecord {
                name: "top".to_owned(),
                qualified_name: "top".to_owned(),
                kind: SymbolKind::Function,
                signature: None,
                start_line: 1,
                end_line: 1,
                token_estimate: 5,
            }],
            &[],
            &[CallSiteRecord {
                caller: 0,
                callee_name: "handleLogin".to_owned(),
                qualifier: None,
                form: CallForm::Plain,
                line: 1,
            }],
        )
        .expect("secret row");
    let outside = fixture
        .outside("function handleLogin", Some("src/handlers"))
        .expect("src/app.ts is outside");
    assert_eq!(sites(&outside), ["src/app.ts:4 handleLogin"]);
    assert_eq!(outside.total, 1, "{outside:?}");
}

#[test]
fn test_the_hint_reads_a_bounded_number_of_rows() {
    assert_eq!(
        (GREP_OUTSIDE_DEFINITIONS, GREP_OUTSIDE_SITES_PER_NAME),
        (4, 64)
    );
    // Six handlers, each called from 70 functions outside the filter.
    let names = (0..6)
        .map(|index| format!("handler{index}"))
        .collect::<Vec<_>>();
    let mut files = Vec::new();
    for name in &names {
        files.push((
            format!("src/handlers/{name}.ts"),
            format!("export function {name}(request) {{\n  return 1;\n}}\n"),
        ));
        let callers = (0..70)
            .map(|caller| {
                format!("export function use_{name}_{caller}(request) {{\n  {name}(request);\n}}\n")
            })
            .collect::<String>();
        files.push((format!("src/routes/{name}.ts"), callers));
    }
    let borrowed = files
        .iter()
        .map(|(path, text)| (path.as_str(), text.as_str()))
        .collect::<Vec<_>>();
    let fixture = Fixture::new(&borrowed);
    let outside = fixture
        .outside("export function handler", Some("src/handlers"))
        .expect("use sites outside");
    assert!(outside.at_least, "{outside:?}");
    assert_eq!(
        outside.total,
        GREP_OUTSIDE_DEFINITIONS * GREP_OUTSIDE_SITES_PER_NAME
    );
    assert!(
        outside.rows_read <= GREP_OUTSIDE_DEFINITIONS * (GREP_OUTSIDE_SITES_PER_NAME + 1),
        "{}",
        outside.rows_read
    );
    assert_eq!(outside.sites.len(), 2);
}

/// A method call on another value (`socket.send(data)`) does not use a
/// free function `send`, even one defined once in the repository.
#[test]
fn test_a_method_call_on_another_value_is_not_a_use_of_a_function() {
    let fixture = Fixture::new(&[
        (
            "src/net/send.ts",
            "export function send(data) {\n  return data;\n}\n",
        ),
        (
            "src/app.ts",
            "import { send } from \"./net/send\";\n\nexport function start(socket, data) {\n  socket.send(data);\n}\n\nexport function direct(data) {\n  return send(data);\n}\n",
        ),
    ]);
    let outside = fixture
        .outside("function send", Some("src/net"))
        .expect("the direct call is a use site");
    assert_eq!(sites(&outside), ["src/app.ts:8 send"]);
    assert_eq!(outside.total, 1, "{outside:?}");
}
