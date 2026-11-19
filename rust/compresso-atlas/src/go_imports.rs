//! Resolution of Go import paths to repository files (E17-37, D102).
//!
//! A Go import names a package, which is a directory. Each `go.mod` in the
//! repository maps its `module` path to its directory; an import path equal
//! to a module path or below it (`example.com/svc/store` under
//! `example.com/svc`) names the matching directory, and resolves to every
//! indexed `.go` file directly in it except `_test.go` files, one edge each.
//! Any other import (the standard library, a third-party module) is
//! external. A `go.mod` under `testdata`, `vendor`, or a directory starting
//! with `_` or `.` is ignored; a module path declared twice resolves through
//! the declaration nearest the importer. Not handled: `replace` directives,
//! `go.work` workspaces beyond the modules their `go.mod` files declare,
//! vendored packages, build constraints that exclude a file, and a package
//! whose `package` clause differs from its directory name (the files are
//! still found by directory).

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use crate::parsers::ImportResolution;
use crate::rust_imports::{EXTERNAL, NOT_INDEXED};

/// Largest `go.mod` read for its module path; a real one is far smaller.
const MAX_GO_MOD_BYTES: u64 = 256 * 1024;

/// The Go modules and packages found in one repository, built once per
/// index run.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct GoModules {
    /// Module path to the repository directories declaring it (`""` for the
    /// root), sorted. A path declared twice resolves through the directory
    /// nearest the importer.
    modules: BTreeMap<String, Vec<String>>,
    /// Package directory to its indexed non-test `.go` files, sorted.
    packages: BTreeMap<String, Vec<String>>,
}

/// Whether a `go.mod` in `directory` can declare an importable module: the
/// Go tool ignores `testdata`, `vendor` holds copies, and directories
/// starting with `_` or `.` are never packages.
fn module_directory_allowed(directory: &str) -> bool {
    directory.is_empty()
        || directory
            .split('/')
            .all(|part| !matches!(part, "testdata" | "vendor") && !part.starts_with(['_', '.']))
}

impl GoModules {
    /// Reads the `go.mod` of every allowed directory that holds an indexed
    /// `.go` file or is an ancestor of one, and groups the indexed non-test
    /// `.go` files by directory.
    pub(crate) fn discover(root: &Path, known_paths: &BTreeSet<String>) -> Self {
        let mut directories = BTreeSet::new();
        for path in known_paths.iter().filter(|path| path.ends_with(".go")) {
            let mut directory = parent_path(path);
            loop {
                if !directories.insert(directory.to_owned()) || directory.is_empty() {
                    break;
                }
                directory = parent_path(directory);
            }
        }
        // Loaded once: every `go.mod` is judged by the scanner's rules.
        let exclusions = crate::scanner::ScannerExclusions::load(root);
        let mut modules = BTreeMap::<String, Vec<String>>::new();
        for directory in directories {
            if !module_directory_allowed(&directory) {
                continue;
            }
            if let Some(module) = read_go_mod(root, &exclusions, &directory)
                .as_deref()
                .and_then(module_path)
            {
                modules.entry(module).or_default().push(directory);
            }
        }
        Self {
            modules,
            packages: packages(known_paths),
        }
    }

    #[cfg(test)]
    pub(crate) fn for_test(modules: &[(&str, &str)], known_paths: &BTreeSet<String>) -> Self {
        Self::with(modules, known_paths)
    }

    #[cfg(test)]
    fn with(modules: &[(&str, &str)], known_paths: &BTreeSet<String>) -> Self {
        let mut map = BTreeMap::<String, Vec<String>>::new();
        for (module, directory) in modules {
            map.entry((*module).to_owned())
                .or_default()
                .push((*directory).to_owned());
        }
        Self {
            modules: map,
            packages: packages(known_paths),
        }
    }

    /// The files an import path names: each is one resolved edge. An
    /// external import, or one whose package has no indexed file, is a
    /// single unresolved record.
    pub(crate) fn resolve(
        &self,
        importer: &str,
        raw: &str,
    ) -> Vec<(Option<String>, ImportResolution)> {
        let Some(directory) = self.package_directory(importer, raw) else {
            return vec![(None, ImportResolution::Unresolved(EXTERNAL.to_owned()))];
        };
        let files = self
            .packages
            .get(&directory)
            .into_iter()
            .flatten()
            .filter(|path| path.as_str() != importer)
            .map(|path| (Some(path.clone()), ImportResolution::Resolved))
            .collect::<Vec<_>>();
        if files.is_empty() {
            vec![(None, ImportResolution::Unresolved(NOT_INDEXED.to_owned()))]
        } else {
            files
        }
    }

    /// The repository directory of the package `raw` names, through the
    /// longest module path that is `raw` or a prefix of it (looked up one
    /// path segment at a time, so the cost does not grow with the number of
    /// modules), and, for a module declared twice, the declaring directory
    /// nearest the importer.
    fn package_directory(&self, importer: &str, raw: &str) -> Option<String> {
        let mut module = raw;
        let directories = loop {
            if let Some(directories) = self.modules.get(module) {
                break directories;
            }
            module = &module[..module.rfind('/')?];
        };
        let directory = directories.iter().max_by_key(|directory| {
            let shared = if directory.is_empty() {
                1
            } else if importer.starts_with(&format!("{directory}/")) {
                directory.len() + 2
            } else {
                0
            };
            (shared, std::cmp::Reverse(directory.as_str()))
        })?;
        let rest = raw[module.len()..].trim_start_matches('/');
        if !rest.is_empty() && rest.split('/').any(|part| matches!(part, "" | "." | "..")) {
            return None;
        }
        Some(match (directory.is_empty(), rest.is_empty()) {
            (_, true) => directory.clone(),
            (true, false) => rest.to_owned(),
            (false, false) => format!("{directory}/{rest}"),
        })
    }
}

fn packages(known_paths: &BTreeSet<String>) -> BTreeMap<String, Vec<String>> {
    let mut packages = BTreeMap::<String, Vec<String>>::new();
    for path in known_paths {
        if path.ends_with(".go") && !path.ends_with("_test.go") {
            packages
                .entry(parent_path(path).to_owned())
                .or_default()
                .push(path.clone());
        }
    }
    packages
}

/// The `module` directive of a `go.mod`, unquoted, without comments.
fn module_path(text: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let line = line.split("//").next().unwrap_or_default().trim();
        let rest = line.strip_prefix("module")?;
        if !rest.starts_with([' ', '\t']) {
            return None;
        }
        let path = rest.trim().trim_matches(['"', '`']).trim();
        (!path.is_empty() && !path.contains(char::is_whitespace)).then(|| path.to_owned())
    })
}

/// Reads `<directory>/go.mod` under the rules the scanner applies to any
/// file: a regular file the scanner's exclusions allow, read through a size
/// cap. Only ancestors of scanned `.go` files are asked.
fn read_go_mod(
    root: &Path,
    exclusions: &crate::scanner::ScannerExclusions,
    directory: &str,
) -> Option<String> {
    use std::io::Read as _;

    let relative = Path::new(directory).join("go.mod");
    if exclusions.excludes(root, &relative) {
        return None;
    }
    let path = root.join(&relative);
    let metadata = fs::symlink_metadata(&path).ok()?;
    if !metadata.file_type().is_file() || metadata.len() > MAX_GO_MOD_BYTES {
        return None;
    }
    let mut text = String::new();
    fs::File::open(&path)
        .ok()?
        .take(MAX_GO_MOD_BYTES + 1)
        .read_to_string(&mut text)
        .ok()?;
    (text.len() as u64 <= MAX_GO_MOD_BYTES).then_some(text)
}

fn parent_path(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(parent, _)| parent)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn known(paths: &[&str]) -> BTreeSet<String> {
        paths.iter().map(|path| (*path).to_owned()).collect()
    }

    #[test]
    fn test_module_path_reads_the_module_directive() {
        assert_eq!(
            module_path("// comment\nmodule example.com/svc // trailing\n\ngo 1.22\n").as_deref(),
            Some("example.com/svc")
        );
        assert_eq!(
            module_path("module \"example.com/q\"\n").as_deref(),
            Some("example.com/q")
        );
        assert_eq!(module_path("modules x\n"), None);
        assert_eq!(module_path("go 1.22\n"), None);
    }

    #[test]
    fn test_import_resolves_to_every_non_test_file_of_the_package() {
        let modules = GoModules::with(
            &[("example.com/svc", "")],
            &known(&[
                "cmd/main.go",
                "store/db.go",
                "store/db_test.go",
                "store/open.go",
                "store/sub/inner.go",
                "store/README.md",
            ]),
        );
        assert_eq!(
            modules.resolve("cmd/main.go", "example.com/svc/store"),
            vec![
                (Some("store/db.go".to_owned()), ImportResolution::Resolved),
                (Some("store/open.go".to_owned()), ImportResolution::Resolved),
            ]
        );
    }

    #[test]
    fn test_external_and_unindexed_imports_stay_single_unresolved_records() {
        let modules = GoModules::with(
            &[("example.com/svc", "services/api")],
            &known(&["services/api/main.go", "services/api/store/db.go"]),
        );
        assert_eq!(
            modules.resolve("services/api/main.go", "fmt"),
            vec![(None, ImportResolution::Unresolved(EXTERNAL.to_owned()))]
        );
        assert_eq!(
            modules.resolve("services/api/main.go", "example.com/svcx/store"),
            vec![(None, ImportResolution::Unresolved(EXTERNAL.to_owned()))]
        );
        assert_eq!(
            modules.resolve("services/api/main.go", "example.com/svc/store"),
            vec![(
                Some("services/api/store/db.go".to_owned()),
                ImportResolution::Resolved
            )]
        );
        assert_eq!(
            modules.resolve("services/api/main.go", "example.com/svc/missing"),
            vec![(None, ImportResolution::Unresolved(NOT_INDEXED.to_owned()))]
        );
        assert_eq!(
            modules.resolve("services/api/main.go", "example.com/svc/../x"),
            vec![(None, ImportResolution::Unresolved(EXTERNAL.to_owned()))]
        );
    }

    #[test]
    fn test_the_longest_module_path_wins() {
        // The nested module lives outside the outer module's matching
        // directory, so only the longest match finds `third_party/tools/gen`.
        let modules = GoModules::with(
            &[
                ("example.com/svc", ""),
                ("example.com/svc/tools", "third_party/tools"),
            ],
            &known(&["tools/gen/gen.go", "third_party/tools/gen/other.go"]),
        );
        assert_eq!(
            modules.resolve("main.go", "example.com/svc/tools/gen"),
            vec![(
                Some("third_party/tools/gen/other.go".to_owned()),
                ImportResolution::Resolved
            )]
        );
    }

    #[test]
    fn test_a_duplicate_module_path_resolves_through_the_module_nearest_the_importer() {
        let modules = GoModules::with(
            &[("example.com/svc", "a"), ("example.com/svc", "b")],
            &known(&["a/store/a.go", "b/store/b.go", "b/cmd/main.go"]),
        );
        assert_eq!(
            modules.resolve("b/cmd/main.go", "example.com/svc/store"),
            vec![(Some("b/store/b.go".to_owned()), ImportResolution::Resolved)]
        );
    }

    #[test]
    fn test_modules_under_testdata_vendor_and_hidden_directories_are_ignored() {
        assert!(!module_directory_allowed("testdata/fake"));
        assert!(!module_directory_allowed("internal/testdata"));
        assert!(!module_directory_allowed("vendor/example.com/x"));
        assert!(!module_directory_allowed("_examples/one"));
        assert!(!module_directory_allowed(".github/tool"));
        assert!(module_directory_allowed(""));
        assert!(module_directory_allowed("services/api"));
    }
}
