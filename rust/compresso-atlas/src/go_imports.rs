//! Resolution of Go import paths to repository files (E17-37, D102).
//!
//! A Go import names a package, which is a directory. Each `go.mod` in the
//! repository maps its `module` path to its directory; an import path equal
//! to a module path or below it (`example.com/svc/store` under
//! `example.com/svc`) names the matching directory, and resolves to every
//! indexed `.go` file directly in it except `_test.go` files, one edge each.
//! Any other import (the standard library, a third-party module) is
//! external. Not handled: `replace` directives, `go.work` workspaces beyond
//! the modules their `go.mod` files declare, vendored packages, build
//! constraints that exclude a file, and a package whose `package` clause
//! differs from its directory name (the files are still found by directory).

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use crate::parsers::ImportResolution;
use crate::rust_imports::{EXTERNAL, NOT_INDEXED};

/// Largest `go.mod` read for its module path; a real one is far smaller.
const MAX_GO_MOD_BYTES: u64 = 256 * 1024;

/// The Go modules found in one repository, built once per index run.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct GoModules {
    /// Module path to the repository directory of its `go.mod` (`""` for
    /// the root). A module path declared twice keeps the first directory.
    modules: BTreeMap<String, String>,
}

impl GoModules {
    /// Reads the `go.mod` of every directory that holds an indexed `.go`
    /// file or is an ancestor of one.
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
        let mut modules = BTreeMap::new();
        for directory in directories {
            if let Some(module) = read_go_mod(root, &directory)
                .as_deref()
                .and_then(module_path)
            {
                modules.entry(module).or_insert(directory);
            }
        }
        Self { modules }
    }

    #[cfg(test)]
    fn with(modules: &[(&str, &str)]) -> Self {
        Self {
            modules: modules
                .iter()
                .map(|(module, directory)| ((*module).to_owned(), (*directory).to_owned()))
                .collect(),
        }
    }

    /// The files an import path names: each is one resolved edge. An
    /// external import, or one whose package has no indexed file, is a
    /// single unresolved record.
    pub(crate) fn resolve(
        &self,
        importer: &str,
        raw: &str,
        known_paths: &BTreeSet<String>,
    ) -> Vec<(Option<String>, ImportResolution)> {
        let Some(directory) = self.package_directory(raw) else {
            return vec![(None, ImportResolution::Unresolved(EXTERNAL.to_owned()))];
        };
        let prefix = if directory.is_empty() {
            String::new()
        } else {
            format!("{directory}/")
        };
        let files = known_paths
            .range(prefix.clone()..)
            .take_while(|path| path.starts_with(&prefix))
            .filter(|path| {
                let name = &path[prefix.len()..];
                !name.contains('/')
                    && name.ends_with(".go")
                    && !name.ends_with("_test.go")
                    && path.as_str() != importer
            })
            .map(|path| (Some(path.clone()), ImportResolution::Resolved))
            .collect::<Vec<_>>();
        if files.is_empty() {
            vec![(None, ImportResolution::Unresolved(NOT_INDEXED.to_owned()))]
        } else {
            files
        }
    }

    /// The repository directory of the package `raw` names, through the
    /// longest module path that is `raw` or a prefix of it.
    fn package_directory(&self, raw: &str) -> Option<String> {
        let (module, directory) = self
            .modules
            .iter()
            .filter(|(module, _)| {
                raw == module.as_str()
                    || raw
                        .strip_prefix(module.as_str())
                        .is_some_and(|rest| rest.starts_with('/'))
            })
            .max_by_key(|(module, _)| module.len())?;
        let rest = raw[module.len()..].trim_start_matches('/');
        if rest.split('/').any(|part| matches!(part, "" | "." | "..")) && !rest.is_empty() {
            return None;
        }
        Some(match (directory.is_empty(), rest.is_empty()) {
            (_, true) => directory.clone(),
            (true, false) => rest.to_owned(),
            (false, false) => format!("{directory}/{rest}"),
        })
    }
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

fn read_go_mod(root: &Path, directory: &str) -> Option<String> {
    use std::io::Read as _;

    let relative = Path::new(directory).join("go.mod");
    if crate::scanner::is_scanner_excluded(root, &relative) {
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
        let modules = GoModules::with(&[("example.com/svc", "")]);
        let paths = known(&[
            "cmd/main.go",
            "store/db.go",
            "store/db_test.go",
            "store/open.go",
            "store/sub/inner.go",
            "store/README.md",
        ]);
        assert_eq!(
            modules.resolve("cmd/main.go", "example.com/svc/store", &paths),
            vec![
                (Some("store/db.go".to_owned()), ImportResolution::Resolved),
                (Some("store/open.go".to_owned()), ImportResolution::Resolved),
            ]
        );
    }

    #[test]
    fn test_external_and_unindexed_imports_stay_single_unresolved_records() {
        let modules = GoModules::with(&[("example.com/svc", "services/api")]);
        let paths = known(&["services/api/main.go", "services/api/store/db.go"]);
        assert_eq!(
            modules.resolve("services/api/main.go", "fmt", &paths),
            vec![(None, ImportResolution::Unresolved(EXTERNAL.to_owned()))]
        );
        assert_eq!(
            modules.resolve("services/api/main.go", "example.com/svcx/store", &paths),
            vec![(None, ImportResolution::Unresolved(EXTERNAL.to_owned()))]
        );
        assert_eq!(
            modules.resolve("services/api/main.go", "example.com/svc/store", &paths),
            vec![(
                Some("services/api/store/db.go".to_owned()),
                ImportResolution::Resolved
            )]
        );
        assert_eq!(
            modules.resolve("services/api/main.go", "example.com/svc/missing", &paths),
            vec![(None, ImportResolution::Unresolved(NOT_INDEXED.to_owned()))]
        );
        assert_eq!(
            modules.resolve("services/api/main.go", "example.com/svc/../x", &paths),
            vec![(None, ImportResolution::Unresolved(EXTERNAL.to_owned()))]
        );
    }

    #[test]
    fn test_the_longest_module_path_wins() {
        let modules =
            GoModules::with(&[("example.com/svc", ""), ("example.com/svc/tools", "tools")]);
        let paths = known(&["tools/gen/gen.go", "tools/tools/gen/other.go"]);
        assert_eq!(
            modules.resolve("main.go", "example.com/svc/tools/gen", &paths),
            vec![(
                Some("tools/gen/gen.go".to_owned()),
                ImportResolution::Resolved
            )]
        );
    }
}
