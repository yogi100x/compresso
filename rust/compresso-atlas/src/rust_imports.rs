//! Resolution of Rust `use` paths to repository files.
//!
//! A path is resolved the way the module system reads it: `crate::`, `self::`
//! and `super::` are followed from the importing file, a first segment that
//! names another crate in the repository is followed into that crate, and the
//! deepest module file on the path wins. A path that ends inside a module
//! file (`crate::Error`, `other_crate::Store`) resolves to that file.
//!
//! Nesting inside a file is not known here, so `super::` written in an inline
//! module (`mod tests { use super::*; }`) is read as the file's parent module.
//! That can add a dependency edge that is not there; it never hides one.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use crate::parsers::ImportResolution;

/// Detail recorded for an import of the standard library or of a dependency
/// the importing crate declares. Such an import cannot change which
/// repository files depend on each other.
pub(crate) const EXTERNAL: &str = compresso_store::EDGE_DETAIL_EXTERNAL;
/// Detail recorded for an import that names the importing file itself.
pub(crate) const SAME_FILE: &str = compresso_store::EDGE_DETAIL_SAME_FILE;
/// Detail recorded when nothing in the index matches.
pub(crate) const NOT_INDEXED: &str = "not_indexed";

/// Largest `Cargo.toml` read for crate names; a real manifest is far smaller.
const MAX_MANIFEST_BYTES: u64 = 256 * 1024;

const STANDARD_CRATES: [&str; 5] = ["std", "core", "alloc", "proc_macro", "test"];

/// The Rust crates found in one repository, built once per index run.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct RustWorkspace {
    /// Crate name as written in a `use` path, to the source directory of
    /// every crate with that name. A repository can hold the same crate name
    /// more than once, for example in a fixture workspace.
    pub(crate) crates: BTreeMap<String, Vec<String>>,
    /// Source directory of a crate, to the dependency names its manifest
    /// declares (as written in a `use` path).
    pub(crate) dependencies: BTreeMap<String, BTreeSet<String>>,
    /// Source directory of a crate, to the directory of the Cargo workspace
    /// it belongs to (its own directory when it has none). A `use` path can
    /// only name a repository crate from the same workspace.
    pub(crate) workspace_roots: BTreeMap<String, String>,
}

impl RustWorkspace {
    /// Finds every crate source directory among the indexed paths and reads
    /// the crate name and declared dependencies from its `Cargo.toml`.
    ///
    /// A directory with `src/lib.rs` is an importable crate. One with only
    /// `src/main.rs` contributes its dependency list. A missing or unreadable
    /// manifest falls back to the directory name and no dependencies.
    pub(crate) fn discover(root: &Path, known_paths: &BTreeSet<String>) -> Self {
        let mut workspace = Self::default();
        let mut manifests = BTreeMap::<String, Manifest>::new();
        let mut manifest_at = |directory: &str| -> Manifest {
            manifests
                .entry(directory.to_owned())
                .or_insert_with(|| {
                    read_manifest(root, directory)
                        .map(|text| Manifest::parse(&text))
                        .unwrap_or_default()
                })
                .clone()
        };
        for path in known_paths {
            let (source_dir, is_library) = if let Some(dir) = path.strip_suffix("/lib.rs") {
                (dir, true)
            } else if let Some(dir) = path.strip_suffix("/main.rs") {
                (dir, false)
            } else {
                continue;
            };
            if source_dir != "src" && !source_dir.ends_with("/src") {
                continue;
            }
            let crate_dir = parent_path(source_dir);
            let manifest = manifest_at(crate_dir);
            let mut workspace_root = crate_dir;
            let mut ancestor = Some(crate_dir);
            while let Some(directory) = ancestor {
                if manifest_at(directory).is_workspace {
                    workspace_root = directory;
                    break;
                }
                ancestor = (!directory.is_empty()).then(|| parent_path(directory));
            }
            workspace
                .workspace_roots
                .insert(source_dir.to_owned(), workspace_root.to_owned());
            if is_library {
                let name = manifest
                    .library_name
                    .or(manifest.package_name)
                    .or_else(|| crate_dir.rsplit('/').next().map(str::to_owned))
                    .filter(|name| !name.is_empty());
                if let Some(name) = name {
                    workspace
                        .crates
                        .entry(use_name(&name))
                        .or_default()
                        .push(source_dir.to_owned());
                }
            }
            workspace
                .dependencies
                .entry(source_dir.to_owned())
                .or_default()
                .extend(manifest.dependencies);
        }
        workspace
    }

    /// The source directory of the crate that contains `importer`.
    fn crate_source_of(&self, importer: &str) -> Option<&str> {
        self.dependencies
            .keys()
            .filter(|source_dir| {
                let crate_dir = parent_path(source_dir);
                crate_dir.is_empty() || importer.starts_with(&format!("{crate_dir}/"))
            })
            .max_by_key(|source_dir| source_dir.len())
            .map(String::as_str)
    }

    /// Whether the crate that contains `importer` declares `name` as a
    /// dependency.
    fn declares(&self, importer: &str, name: &str) -> bool {
        self.crate_source_of(importer)
            .and_then(|source_dir| self.dependencies.get(source_dir))
            .is_some_and(|dependencies| dependencies.contains(name))
    }

    /// The repository crates named `name` that `importer` can refer to:
    /// those in its own Cargo workspace, nearest first by shared path.
    fn reachable_crates(&self, importer: &str, name: &str) -> Vec<&str> {
        let Some(sources) = self.crates.get(name) else {
            return Vec::new();
        };
        let importer_root = self
            .crate_source_of(importer)
            .and_then(|source_dir| self.workspace_roots.get(source_dir));
        let same_workspace = sources
            .iter()
            .filter(|source| {
                match (importer_root, self.workspace_roots.get(*source)) {
                    (Some(importer_root), Some(root)) => importer_root == root,
                    // Unknown membership cannot rule a crate out.
                    _ => true,
                }
            })
            .cloned()
            .collect::<Vec<_>>();
        let nearest = nearest(importer, &same_workspace);
        sources
            .iter()
            .map(String::as_str)
            .filter(|source| nearest.contains(source))
            .collect()
    }
}

/// Reads `<crate_dir>/Cargo.toml` under the rules the scanner applies to any
/// file: a regular file (never a symbolic link) that the scanner's exclusions
/// allow, read through a size cap. Anything else counts as no manifest.
fn read_manifest(root: &Path, crate_dir: &str) -> Option<String> {
    use std::io::Read as _;

    let relative = Path::new(crate_dir).join("Cargo.toml");
    if crate::scanner::is_scanner_excluded(root, &relative) {
        return None;
    }
    let path = root.join(&relative);
    let metadata = fs::symlink_metadata(&path).ok()?;
    if !metadata.file_type().is_file() || metadata.len() > MAX_MANIFEST_BYTES {
        return None;
    }
    let mut text = String::new();
    fs::File::open(&path)
        .ok()?
        .take(MAX_MANIFEST_BYTES + 1)
        .read_to_string(&mut text)
        .ok()?;
    (text.len() as u64 <= MAX_MANIFEST_BYTES).then_some(text)
}

/// The parts of a `Cargo.toml` that name crates. Read line by line: only
/// plain `key = value` lines and table headers matter here.
#[derive(Clone, Default)]
struct Manifest {
    is_workspace: bool,
    package_name: Option<String>,
    library_name: Option<String>,
    dependencies: BTreeSet<String>,
}

impl Manifest {
    fn parse(text: &str) -> Self {
        let mut manifest = Self::default();
        let mut section = String::new();
        for line in text.lines() {
            let line = line.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            if let Some(header) = line.strip_prefix('[') {
                section = header
                    .trim_matches(|c| c == '[' || c == ']')
                    .trim()
                    .to_owned();
                if section == "workspace" || section.starts_with("workspace.") {
                    manifest.is_workspace = true;
                }
                if let Some(name) = dependency_table_entry(&section) {
                    manifest.dependencies.insert(use_name(name));
                }
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let key = key.trim();
            if is_dependency_table(&section) {
                let name = key.split('.').next().unwrap_or(key).trim_matches('"');
                if !name.is_empty() {
                    manifest.dependencies.insert(use_name(name));
                }
            } else if key == "name" {
                let value = value.trim().trim_matches('"').to_owned();
                match section.as_str() {
                    "package" => manifest.package_name = Some(value),
                    "lib" => manifest.library_name = Some(value),
                    _ => {}
                }
            }
        }
        manifest
    }
}

const DEPENDENCY_TABLES: [&str; 3] = ["dependencies", "dev-dependencies", "build-dependencies"];

fn is_dependency_table(section: &str) -> bool {
    DEPENDENCY_TABLES
        .iter()
        .any(|table| section == *table || section.ends_with(&format!(".{table}")))
}

/// The dependency named by a `[dependencies.<name>]` style header.
fn dependency_table_entry(section: &str) -> Option<&str> {
    DEPENDENCY_TABLES.iter().find_map(|table| {
        let marker = format!("{table}.");
        let (before, name) = section.rsplit_once(marker.as_str())?;
        (before.is_empty() || before.ends_with('.')).then_some(name.trim_matches('"'))
    })
}

/// A crate or dependency name as it appears in a `use` path.
fn use_name(name: &str) -> String {
    name.replace('-', "_")
}

/// A module seen from the file system: where its child modules live and
/// which file can define it.
struct Module {
    children_dir: String,
    files: Vec<String>,
}

impl Module {
    fn crate_root(source_root: &str, importer: &str, known_paths: &BTreeSet<String>) -> Self {
        let library = format!("{source_root}/lib.rs");
        let binary = format!("{source_root}/main.rs");
        // A crate with both files keeps its modules in the library; only the
        // binary root itself means `main.rs` when it writes `crate::`.
        let files = if importer == binary || !known_paths.contains(&library) {
            vec![binary]
        } else {
            vec![library]
        };
        Self {
            children_dir: source_root.to_owned(),
            files,
        }
    }

    fn of_file(importer: &str) -> Self {
        let file_name = importer.rsplit('/').next().unwrap_or(importer);
        let children_dir = if matches!(file_name, "lib.rs" | "main.rs" | "mod.rs") {
            parent_path(importer).to_owned()
        } else {
            importer.strip_suffix(".rs").unwrap_or(importer).to_owned()
        };
        Self {
            children_dir,
            files: vec![importer.to_owned()],
        }
    }

    fn parent(
        &self,
        source_root: &str,
        importer: &str,
        known_paths: &BTreeSet<String>,
    ) -> Option<Self> {
        if self.children_dir == source_root || self.children_dir.is_empty() {
            return None;
        }
        let parent_dir = parent_path(&self.children_dir);
        if parent_dir == source_root {
            return Some(Self::crate_root(source_root, importer, known_paths));
        }
        Some(Self {
            children_dir: parent_dir.to_owned(),
            files: vec![format!("{parent_dir}.rs"), format!("{parent_dir}/mod.rs")],
        })
    }

    /// Files a path of `segments` below this module can refer to.
    fn targets(&self, segments: &[&str], known_paths: &BTreeSet<String>) -> Vec<String> {
        let deepest = deepest_module_files(&self.children_dir, segments, known_paths);
        if !deepest.is_empty() {
            return deepest;
        }
        // No module file on the path. A single remaining segment names an
        // item of this module; a longer path may run through a module that
        // was not indexed, so it stays unresolved.
        if segments.len() > 1 {
            return Vec::new();
        }
        self.files
            .iter()
            .filter(|file| known_paths.contains(*file))
            .cloned()
            .collect()
    }
}

/// The indexed module files for the longest prefix of `segments` under
/// `base`. The deepest module is the one that defines the imported name.
fn deepest_module_files(
    base: &str,
    segments: &[&str],
    known_paths: &BTreeSet<String>,
) -> Vec<String> {
    for length in (1..=segments.len()).rev() {
        let stem = if base.is_empty() {
            segments[..length].join("/")
        } else {
            format!("{base}/{}", segments[..length].join("/"))
        };
        let found = [format!("{stem}.rs"), format!("{stem}/mod.rs")]
            .into_iter()
            .filter(|candidate| known_paths.contains(candidate))
            .collect::<Vec<_>>();
        if !found.is_empty() {
            return found;
        }
    }
    Vec::new()
}

/// Resolves one `use` path written in `importer`.
pub(crate) fn resolve(
    importer: &str,
    raw: &str,
    known_paths: &BTreeSet<String>,
    workspace: &RustWorkspace,
) -> (Option<String>, ImportResolution) {
    let unresolved = |detail: &str| (None, ImportResolution::Unresolved(detail.to_owned()));
    let mut segments = raw
        .split("::")
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();
    if matches!(segments.last(), Some(&"*" | &"self")) && segments.len() > 1 {
        segments.pop();
    }
    let Some(first) = segments.first().copied() else {
        return unresolved(NOT_INDEXED);
    };
    let source_root = source_root(importer);

    let targets = match first {
        "crate" => Module::crate_root(&source_root, importer, known_paths)
            .targets(&segments[1..], known_paths),
        "self" => Module::of_file(importer).targets(&segments[1..], known_paths),
        "super" => {
            let mut module = Module::of_file(importer);
            let mut rest = segments.as_slice();
            while rest.first() == Some(&"super") {
                // `super` cannot leave the crate root. Reaching it with one
                // left over means the path was written in an inline module
                // of this file, whose parent is the file's own module.
                if let Some(parent) = module.parent(&source_root, importer, known_paths) {
                    module = parent;
                }
                rest = &rest[1..];
            }
            module.targets(rest, known_paths)
        }
        name => {
            let crate_sources = workspace.reachable_crates(importer, name);
            if !crate_sources.is_empty() {
                let mut found = BTreeSet::new();
                for crate_source in crate_sources {
                    found.extend(
                        Module::crate_root(crate_source, importer, known_paths)
                            .targets(&segments[1..], known_paths),
                    );
                }
                found.into_iter().collect()
            } else {
                // Without a keyword the path names a child module of the
                // importing module or, in the oldest edition, a module at the
                // crate root. Both readings are kept when both match.
                let own_module = Module::of_file(importer);
                let mut found = BTreeSet::new();
                for base in [own_module.children_dir.as_str(), source_root.as_str()] {
                    found.extend(deepest_module_files(base, &segments, known_paths));
                }
                if found.is_empty()
                    && (STANDARD_CRATES.contains(&name) || workspace.declares(importer, name))
                {
                    return unresolved(EXTERNAL);
                }
                found.into_iter().collect()
            }
        }
    };

    match targets.as_slice() {
        [] => unresolved(NOT_INDEXED),
        [only] if only == importer => unresolved(SAME_FILE),
        [only] => (Some(only.clone()), ImportResolution::Resolved),
        _ => {
            let mut candidates = targets;
            candidates.sort_unstable();
            (None, ImportResolution::Ambiguous(candidates))
        }
    }
}

/// The crate sources that share the longest leading path with `importer`.
/// A crate name used twice in one repository means the copy in the
/// importer's own workspace; a tie keeps every candidate.
fn nearest<'a>(importer: &str, crate_sources: &'a [String]) -> Vec<&'a str> {
    let shared = |source: &str| {
        importer
            .split('/')
            .zip(source.split('/'))
            .take_while(|(left, right)| left == right)
            .count()
    };
    let best = crate_sources
        .iter()
        .map(|source| shared(source))
        .max()
        .unwrap_or(0);
    crate_sources
        .iter()
        .filter(|source| shared(source) == best)
        .map(String::as_str)
        .collect()
}

/// The directory that holds the crate root for `importer`: the nearest `src`
/// directory, or the file's own directory for tests, benches, and examples.
fn source_root(importer: &str) -> String {
    let components = importer.split('/').collect::<Vec<_>>();
    components
        .iter()
        .rposition(|component| *component == "src")
        .map_or_else(
            || parent_path(importer).to_owned(),
            |index| components[..=index].join("/"),
        )
}

fn parent_path(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(parent, _)| parent)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn known() -> BTreeSet<String> {
        [
            "crates/store/src/lib.rs",
            "crates/store/src/ops.rs",
            "crates/store/src/ops/detail.rs",
            "crates/app/src/lib.rs",
            "crates/app/src/commands.rs",
            "crates/app/src/commands/run.rs",
            "crates/app/src/util/mod.rs",
            "crates/app/src/util/text.rs",
            "crates/app/tests/smoke.rs",
        ]
        .map(str::to_owned)
        .into()
    }

    fn workspace() -> RustWorkspace {
        RustWorkspace {
            crates: BTreeMap::from([
                ("my_store".to_owned(), vec!["crates/store/src".to_owned()]),
                ("app".to_owned(), vec!["crates/app/src".to_owned()]),
            ]),
            dependencies: BTreeMap::from([(
                "crates/app/src".to_owned(),
                BTreeSet::from(["serde".to_owned(), "my_store".to_owned()]),
            )]),
            workspace_roots: BTreeMap::new(),
        }
    }

    fn resolved(importer: &str, raw: &str) -> (Option<String>, ImportResolution) {
        resolve(importer, raw, &known(), &workspace())
    }

    #[test]
    fn test_rust_imports_resolve_to_the_file_that_defines_the_path() {
        let cases = [
            // A sibling crate, at its root and inside its modules.
            (
                "crates/app/src/commands/run.rs",
                "my_store::Store",
                "crates/store/src/lib.rs",
            ),
            (
                "crates/app/src/commands/run.rs",
                "my_store::ops::Row",
                "crates/store/src/ops.rs",
            ),
            // The deepest module on the path wins over its parents.
            (
                "crates/app/src/commands/run.rs",
                "my_store::ops::detail::Thing",
                "crates/store/src/ops/detail.rs",
            ),
            (
                "crates/app/tests/smoke.rs",
                "app::commands::run::go",
                "crates/app/src/commands/run.rs",
            ),
            // An item defined in the crate root or in the parent module.
            (
                "crates/app/src/commands.rs",
                "crate::AppError",
                "crates/app/src/lib.rs",
            ),
            (
                "crates/app/src/commands/run.rs",
                "super::Shared",
                "crates/app/src/commands.rs",
            ),
            (
                "crates/app/src/commands.rs",
                "super::Top",
                "crates/app/src/lib.rs",
            ),
            (
                "crates/app/src/util/text.rs",
                "super::helper",
                "crates/app/src/util/mod.rs",
            ),
            (
                "crates/app/src/util/text.rs",
                "super::super::commands::run::go",
                "crates/app/src/commands/run.rs",
            ),
            // Child modules of a file that is not `mod.rs`.
            (
                "crates/app/src/commands.rs",
                "self::run::go",
                "crates/app/src/commands/run.rs",
            ),
            (
                "crates/app/src/commands.rs",
                "crate::util::text::trim",
                "crates/app/src/util/text.rs",
            ),
            // A path relative to the importing module, without a keyword.
            (
                "crates/app/src/lib.rs",
                "commands::run::go",
                "crates/app/src/commands/run.rs",
            ),
            (
                "crates/app/src/commands.rs",
                "crate::util::*",
                "crates/app/src/util/mod.rs",
            ),
        ];

        for (importer, raw, expected) in cases {
            assert_eq!(
                resolved(importer, raw),
                (Some(expected.to_owned()), ImportResolution::Resolved),
                "{raw} in {importer}"
            );
        }
    }

    #[test]
    fn test_keywordless_path_reaches_a_child_module_of_the_importing_file() {
        // `commands.rs` declares `mod run;`, whose file is `commands/run.rs`.
        assert_eq!(
            resolved("crates/app/src/commands.rs", "run::go"),
            (
                Some("crates/app/src/commands/run.rs".to_owned()),
                ImportResolution::Resolved
            )
        );
    }

    #[test]
    fn test_crate_name_used_twice_resolves_to_the_nearest_crate() {
        let known: BTreeSet<String> = [
            "rust/store/src/lib.rs",
            "rust/app/src/lib.rs",
            "fixtures/sample/crates/store/src/lib.rs",
            "fixtures/sample/crates/app/src/lib.rs",
            "elsewhere/tool/src/lib.rs",
        ]
        .map(str::to_owned)
        .into();
        let workspace = RustWorkspace {
            crates: BTreeMap::from([(
                "store".to_owned(),
                vec![
                    "fixtures/sample/crates/store/src".to_owned(),
                    "rust/store/src".to_owned(),
                ],
            )]),
            dependencies: BTreeMap::new(),
            workspace_roots: BTreeMap::new(),
        };
        let resolve_from = |importer: &str| resolve(importer, "store::Store", &known, &workspace);

        assert_eq!(
            resolve_from("rust/app/src/lib.rs"),
            (
                Some("rust/store/src/lib.rs".to_owned()),
                ImportResolution::Resolved
            )
        );
        assert_eq!(
            resolve_from("fixtures/sample/crates/app/src/lib.rs"),
            (
                Some("fixtures/sample/crates/store/src/lib.rs".to_owned()),
                ImportResolution::Resolved
            )
        );
        // Equally far from both: the import could mean either crate.
        assert_eq!(
            resolve_from("elsewhere/tool/src/lib.rs"),
            (
                None,
                ImportResolution::Ambiguous(vec![
                    "fixtures/sample/crates/store/src/lib.rs".to_owned(),
                    "rust/store/src/lib.rs".to_owned(),
                ])
            )
        );
    }

    #[test]
    fn test_same_named_crate_in_another_workspace_is_not_the_import_target() {
        // A fixture workspace ships a stub crate named like a real
        // dependency. The application's `tokio::…` is the registry crate.
        let known: BTreeSet<String> = [
            "rust/app/src/lib.rs",
            "fixtures/case/deps/tokio/src/lib.rs",
            "fixtures/case/crates/user/src/lib.rs",
        ]
        .map(str::to_owned)
        .into();
        let workspace = RustWorkspace {
            crates: BTreeMap::from([(
                "tokio".to_owned(),
                vec!["fixtures/case/deps/tokio/src".to_owned()],
            )]),
            dependencies: BTreeMap::from([
                (
                    "rust/app/src".to_owned(),
                    BTreeSet::from(["tokio".to_owned()]),
                ),
                ("fixtures/case/deps/tokio/src".to_owned(), BTreeSet::new()),
                (
                    "fixtures/case/crates/user/src".to_owned(),
                    BTreeSet::from(["tokio".to_owned()]),
                ),
            ]),
            workspace_roots: BTreeMap::from([
                ("rust/app/src".to_owned(), String::new()),
                (
                    "fixtures/case/deps/tokio/src".to_owned(),
                    "fixtures/case".to_owned(),
                ),
                (
                    "fixtures/case/crates/user/src".to_owned(),
                    "fixtures/case".to_owned(),
                ),
            ]),
        };

        assert_eq!(
            resolve(
                "rust/app/src/lib.rs",
                "tokio::io::AsyncRead",
                &known,
                &workspace
            ),
            (None, ImportResolution::Unresolved(EXTERNAL.to_owned()))
        );
        assert_eq!(
            resolve(
                "fixtures/case/crates/user/src/lib.rs",
                "tokio::Runtime",
                &known,
                &workspace
            ),
            (
                Some("fixtures/case/deps/tokio/src/lib.rs".to_owned()),
                ImportResolution::Resolved
            )
        );
    }

    #[test]
    fn test_standard_library_and_declared_dependencies_are_external() {
        for raw in [
            "std::collections::HashMap",
            "core::fmt",
            "alloc::vec::Vec",
            "serde::Serialize",
        ] {
            assert_eq!(
                resolved("crates/app/src/commands.rs", raw),
                (None, ImportResolution::Unresolved(EXTERNAL.to_owned())),
                "{raw}"
            );
        }
    }

    #[test]
    fn test_unknown_first_segment_stays_unresolved_rather_than_external() {
        // Nothing declares this name, so it may be a module that was not
        // indexed; claiming it is external could hide a dependent.
        for (importer, raw) in [
            ("crates/app/src/commands.rs", "mystery::Thing"),
            ("crates/store/src/ops.rs", "serde::Serialize"),
            ("crates/app/src/commands.rs", "crate::missing::Thing"),
        ] {
            let (path, resolution) = resolved(importer, raw);
            assert_eq!(path, None, "{raw}");
            assert_eq!(
                resolution,
                ImportResolution::Unresolved(NOT_INDEXED.to_owned()),
                "{raw} in {importer}"
            );
        }
    }

    #[test]
    fn test_crate_path_that_names_no_module_resolves_to_the_crate_root() {
        // `crate::missing::Thing` above is unresolved because `missing` could
        // be a module; a single segment after `crate` is an item of the root.
        assert_eq!(
            resolved("crates/app/src/commands.rs", "crate::Thing"),
            (
                Some("crates/app/src/lib.rs".to_owned()),
                ImportResolution::Resolved
            )
        );
    }

    #[test]
    fn test_import_of_the_importing_file_is_marked_same_file() {
        assert_eq!(
            resolved("crates/app/src/commands.rs", "self::Local"),
            (None, ImportResolution::Unresolved(SAME_FILE.to_owned()))
        );
        assert_eq!(
            resolved("crates/app/src/lib.rs", "crate::AppError"),
            (None, ImportResolution::Unresolved(SAME_FILE.to_owned()))
        );
        // `super` cannot leave the crate root, so in a root file it was
        // written inside an inline module and names that same file.
        assert_eq!(
            resolved("crates/app/src/lib.rs", "super::*"),
            (None, ImportResolution::Unresolved(SAME_FILE.to_owned()))
        );
        assert_eq!(
            resolved("crates/app/src/lib.rs", "super::commands::run::go"),
            (
                Some("crates/app/src/commands/run.rs".to_owned()),
                ImportResolution::Resolved
            )
        );
    }

    #[test]
    fn test_keywordless_path_matching_two_modules_stays_ambiguous() {
        // `util::Thing` in `commands.rs` is its child module in current
        // editions and the crate-root module in the oldest one.
        let mut known = known();
        known.insert("crates/app/src/commands/util.rs".to_owned());

        let (path, resolution) = resolve(
            "crates/app/src/commands.rs",
            "util::Thing",
            &known,
            &workspace(),
        );

        assert_eq!(path, None);
        assert_eq!(
            resolution,
            ImportResolution::Ambiguous(vec![
                "crates/app/src/commands/util.rs".to_owned(),
                "crates/app/src/util/mod.rs".to_owned(),
            ])
        );
    }

    #[test]
    fn test_keywordless_path_does_not_guess_a_sibling_file() {
        // A sibling of a non-root file is not in scope without `super::`.
        let mut known = known();
        known.insert("crates/app/src/commands/helper.rs".to_owned());

        let (path, resolution) = resolve(
            "crates/app/src/commands/run.rs",
            "helper::Thing",
            &known,
            &workspace(),
        );

        assert_eq!(path, None);
        assert_eq!(
            resolution,
            ImportResolution::Unresolved(NOT_INDEXED.to_owned())
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_workspace_discovery_ignores_manifests_the_scanner_would_not_read() {
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let root = directory.path().join("repo");
        let outside = directory.path().join("outside.toml");
        fs::write(&outside, "[package]\nname = \"leaked-name\"\n")
            .expect("outside manifest should be written");
        for name in ["linked", "huge", "ignored"] {
            fs::create_dir_all(root.join(name).join("src")).expect("crate should be created");
        }
        std::os::unix::fs::symlink(&outside, root.join("linked/Cargo.toml"))
            .expect("symlink should be created");
        let mut oversized = String::from("[package]\nname = \"huge-name\"\n");
        oversized.push_str(&"# padding\n".repeat(MAX_MANIFEST_BYTES as usize / 10 + 1));
        fs::write(root.join("huge/Cargo.toml"), oversized)
            .expect("oversized manifest should be written");
        fs::write(root.join(".gitignore"), "ignored/Cargo.toml\n")
            .expect("ignore file should be written");
        fs::write(
            root.join("ignored/Cargo.toml"),
            "[package]\nname = \"ignored-name\"\n",
        )
        .expect("ignored manifest should be written");
        let known = ["linked/src/lib.rs", "huge/src/lib.rs", "ignored/src/lib.rs"]
            .map(str::to_owned)
            .into();

        let workspace = RustWorkspace::discover(&root, &known);

        // Each crate falls back to its directory name; no manifest was read.
        assert_eq!(
            workspace
                .crates
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["huge", "ignored", "linked"]
        );
    }

    #[test]
    fn test_workspace_discovery_reads_crate_names_and_dependencies() {
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let root = directory.path();
        fs::create_dir_all(root.join("crates/store/src")).expect("store should be created");
        fs::create_dir_all(root.join("crates/app/src")).expect("app should be created");
        fs::create_dir_all(root.join("tools/plain/src")).expect("plain should be created");
        fs::write(
            root.join("crates/store/Cargo.toml"),
            "[package]\nname = \"my-store\" # hyphenated\nversion = \"0.1.0\"\n\n[dependencies]\nrusqlite = { version = \"0.41\" }\n",
        )
        .expect("store manifest should be written");
        fs::write(
            root.join("crates/app/Cargo.toml"),
            "[package]\nname = \"app-cli\"\n\n[lib]\nname = \"app\"\npath = \"src/lib.rs\"\n\n[dependencies]\nmy-store.workspace = true\nserde = \"1\"\n\n[dependencies.clap]\nversion = \"4\"\n\n[dev-dependencies]\ntempfile = \"3\"\n\n[features]\nname = [\"not-a-crate-name\"]\n",
        )
        .expect("app manifest should be written");
        fs::write(
            root.join("crates/Cargo.toml"),
            "[workspace]\nmembers = [\"store\", \"app\"]\n\n[workspace.dependencies]\nserde = \"1\"\n",
        )
        .expect("workspace manifest should be written");
        let known = [
            "crates/store/src/lib.rs",
            "crates/app/src/lib.rs",
            "crates/app/src/main.rs",
            "tools/plain/src/lib.rs",
        ]
        .map(str::to_owned)
        .into();

        let workspace = RustWorkspace::discover(root, &known);

        assert_eq!(
            workspace.crates,
            BTreeMap::from([
                ("app".to_owned(), vec!["crates/app/src".to_owned()]),
                ("my_store".to_owned(), vec!["crates/store/src".to_owned()]),
                // No manifest: the directory name is the best available name.
                ("plain".to_owned(), vec!["tools/plain/src".to_owned()]),
            ])
        );
        assert_eq!(
            workspace.dependencies.get("crates/app/src"),
            Some(&BTreeSet::from([
                "clap".to_owned(),
                "my_store".to_owned(),
                "serde".to_owned(),
                "tempfile".to_owned(),
            ]))
        );
        assert_eq!(
            workspace.dependencies.get("crates/store/src"),
            Some(&BTreeSet::from(["rusqlite".to_owned()]))
        );
        // `crates/` holds a workspace manifest; `tools/plain` stands alone.
        assert_eq!(
            workspace.workspace_roots,
            BTreeMap::from([
                ("crates/app/src".to_owned(), "crates".to_owned()),
                ("crates/store/src".to_owned(), "crates".to_owned()),
                ("tools/plain/src".to_owned(), "tools/plain".to_owned()),
            ])
        );
    }
}
