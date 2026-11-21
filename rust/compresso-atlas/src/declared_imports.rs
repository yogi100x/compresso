//! Resolution of Java imports and C# `using` directives (E17-40, D105).
//!
//! Java and C# name packages and namespaces in each file's declarations,
//! not by directory, so the map is built from what files declare: the
//! declarations stored for files this run does not re-parse, and those read
//! before the run for the files it does. Only scanned, indexed paths take
//! part, so a file the scanner excludes declares nothing.
//!
//! Owner decision 2026-11-21: an import of a whole package or namespace
//! (Java `a.b.*`, C# `using N;`) is stored as one record naming it (detail
//! [`EDGE_DETAIL_PACKAGE`]); its files are found when a query reads the
//! record, so no file list is copied at index time and the stored rows do
//! not grow with the package. An import of one type (Java `a.b.C`, a static
//! import `a.b.C.member`, a nested type `a.b.C.Inner` or `a.b.C.Inner.*`;
//! C# `using static N.T;` or an alias of a type) is one edge to the files of
//! the longest declared package prefix named after the type that follows it
//! (`C.java`; for C#, `T.cs` or `T.Part.cs`, a convention). A name is
//! `not_indexed` when a declared package of its language is a dotted prefix
//! of it, and external otherwise (a namespace of which the repository
//! declares only children, `using System;` beside `System.Polyfill`, is
//! external). Java and C# never resolve into each other.

use std::collections::{BTreeMap, HashMap};

use compresso_store::{EDGE_DETAIL_PACKAGE, FileLanguage};

use crate::parsers::languages::{ImportStyle, spec};
use crate::parsers::{ImportContext, ImportResolution};
use crate::rust_imports::{EXTERNAL, NOT_INDEXED};

type Key = (&'static str, String);

/// Marks a stored C# `global using` record, which applies to every C# file
/// of the repository: `global:Acme.Util`.
pub(crate) const GLOBAL_IMPORT_PREFIX: &str = "global:";

/// Declared packages and namespaces of one repository, built once per run.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct DeclaredPackages {
    /// (language, package or namespace) to how many files declare it.
    packages: HashMap<Key, usize>,
    /// (language, package, file name up to its first `.`) to those files,
    /// sorted: the files one type import can name.
    types: HashMap<(&'static str, String, String), Vec<String>>,
}

impl DeclaredPackages {
    /// Builds the map from `(path, language, declared name)` rows.
    pub(crate) fn new(
        declarations: impl IntoIterator<Item = (String, FileLanguage, String)>,
    ) -> Self {
        let mut packages = HashMap::<Key, usize>::new();
        let mut types = BTreeMap::<(&'static str, String, String), Vec<String>>::new();
        for (path, language, name) in declarations {
            *packages
                .entry((language.as_str(), name.clone()))
                .or_default() += 1;
            let file = path.rsplit('/').next().unwrap_or(&path);
            let stem = file.split('.').next().unwrap_or(file).to_owned();
            types
                .entry((language.as_str(), name, stem))
                .or_default()
                .push(path);
        }
        let types = types
            .into_iter()
            .map(|(key, mut paths)| {
                paths.sort();
                paths.dedup();
                (key, paths)
            })
            .collect();
        Self { packages, types }
    }

    /// One package record, the type files an import names (never the
    /// importer itself), or one unresolved record.
    pub(crate) fn resolve(
        &self,
        importer: &str,
        language: FileLanguage,
        raw: &str,
    ) -> Vec<(Option<String>, ImportResolution)> {
        let unresolved =
            |detail: &str| vec![(None, ImportResolution::Unresolved(detail.to_owned()))];
        let whole = match spec(language).imports {
            ImportStyle::Java => raw.strip_suffix(".*"),
            ImportStyle::CSharp => Some(raw),
            _ => return unresolved(EXTERNAL),
        };
        if let Some(package) = whole
            && self.is_package(language, package)
        {
            return unresolved(EDGE_DETAIL_PACKAGE);
        }
        let name = raw.strip_suffix(".*").unwrap_or(raw);
        let Some((package, kind)) = self.longest_package_prefix(language, name) else {
            return unresolved(EXTERNAL);
        };
        let files = self
            .types
            .get(&(language.as_str(), package.to_owned(), kind.to_owned()))
            .into_iter()
            .flatten()
            .filter(|path| path.as_str() != importer)
            .map(|path| (Some(path.clone()), ImportResolution::Resolved))
            .collect::<Vec<_>>();
        if files.is_empty() {
            unresolved(NOT_INDEXED)
        } else {
            files
        }
    }

    /// [`Self::resolve`] for an import written in `context`, with the name
    /// stored for it: a C# `using` inside a namespace names a namespace or
    /// type relative to the enclosing namespaces first, innermost first,
    /// and is stored by the full name it resolved to (as written when none
    /// matched); a `global using` is stored with [`GLOBAL_IMPORT_PREFIX`],
    /// as it applies to every C# file.
    pub(crate) fn resolve_in(
        &self,
        importer: &str,
        language: FileLanguage,
        raw: &str,
        context: &ImportContext,
    ) -> (String, Vec<(Option<String>, ImportResolution)>) {
        match context {
            ImportContext::Namespace(namespace)
                if spec(language).imports == ImportStyle::CSharp =>
            {
                let mut outer = Some(namespace.as_str());
                while let Some(enclosing) = outer.filter(|enclosing| !enclosing.is_empty()) {
                    let full = format!("{enclosing}.{raw}");
                    let found = self.resolve(importer, language, &full);
                    let matched = found.iter().any(|(path, resolution)| {
                        path.is_some()
                            || *resolution
                                == ImportResolution::Unresolved(EDGE_DETAIL_PACKAGE.to_owned())
                    });
                    if matched {
                        return (full, found);
                    }
                    outer = enclosing.rfind('.').map(|dot| &enclosing[..dot]);
                }
                (raw.to_owned(), self.resolve(importer, language, raw))
            }
            ImportContext::Global => (
                format!("{GLOBAL_IMPORT_PREFIX}{raw}"),
                self.resolve(importer, language, raw),
            ),
            _ => (raw.to_owned(), self.resolve(importer, language, raw)),
        }
    }

    fn is_package(&self, language: FileLanguage, name: &str) -> bool {
        self.packages
            .contains_key(&(language.as_str(), name.to_owned()))
    }

    /// The longest declared package that is a proper dotted prefix of
    /// `name`, with the segment that follows it (the type).
    fn longest_package_prefix<'a>(
        &self,
        language: FileLanguage,
        name: &'a str,
    ) -> Option<(&'a str, &'a str)> {
        let mut end = name.len();
        while let Some(dot) = name[..end].rfind('.') {
            let package = &name[..dot];
            if self.is_package(language, package) {
                let rest = &name[dot + 1..];
                return Some((package, rest.split('.').next().unwrap_or(rest)));
            }
            end = dot;
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use compresso_store::EDGE_DETAIL_PACKAGE;

    fn map(rows: &[(&str, FileLanguage, &str)]) -> DeclaredPackages {
        DeclaredPackages::new(
            rows.iter()
                .map(|(path, language, name)| ((*path).to_owned(), *language, (*name).to_owned())),
        )
    }

    fn resolved(paths: &[&str]) -> Vec<(Option<String>, ImportResolution)> {
        paths
            .iter()
            .map(|path| (Some((*path).to_owned()), ImportResolution::Resolved))
            .collect()
    }

    fn unresolved(detail: &str) -> Vec<(Option<String>, ImportResolution)> {
        vec![(None, ImportResolution::Unresolved(detail.to_owned()))]
    }

    /// E17-40 (D105, owner decision 2026-11-21): a wildcard import is one
    /// record naming its package; a single-type import is one edge to the
    /// type's file; nested and static names reach the outer type's file.
    #[test]
    fn test_java_imports_resolve_to_one_package_record_or_one_type_file() {
        let java = FileLanguage::Java;
        let packages = map(&[
            ("src/a/b/C.java", java, "a.b"),
            ("src/a/b/D.java", java, "a.b"),
            ("legacy/E.java", java, "a.b"),
            ("src/a/b/package-info.java", java, "a.b"),
            ("src/a/b/c/F.java", java, "a.b.c"),
        ]);
        assert_eq!(
            packages.resolve("x/M.java", java, "a.b.C"),
            resolved(&["src/a/b/C.java"])
        );
        assert_eq!(
            packages.resolve("x/M.java", java, "a.b.*"),
            unresolved(EDGE_DETAIL_PACKAGE)
        );
        assert_eq!(
            packages.resolve("x/M.java", java, "a.b.C.member"),
            resolved(&["src/a/b/C.java"])
        );
        // `import a.b.C.*`, `import static a.b.C.Inner.*`, and
        // `import a.b.C.Inner.*` name the nested types of `C`.
        for raw in ["a.b.C.*", "a.b.C.Inner.*", "a.b.C.Inner"] {
            assert_eq!(
                packages.resolve("x/M.java", java, raw),
                resolved(&["src/a/b/C.java"]),
                "{raw}"
            );
        }
        assert_eq!(
            packages.resolve("x/M.java", java, "a.b.c.F"),
            resolved(&["src/a/b/c/F.java"])
        );
        assert_eq!(
            packages.resolve("x/M.java", java, "a.b.Missing"),
            unresolved(NOT_INDEXED)
        );
        assert_eq!(
            packages.resolve("x/M.java", java, "a.Other"),
            unresolved(EXTERNAL)
        );
        assert_eq!(
            packages.resolve("x/M.java", java, "java.util.List"),
            unresolved(EXTERNAL)
        );
        assert_eq!(
            packages.resolve("x/M.java", java, "java.util.*"),
            unresolved(EXTERNAL)
        );
        assert_eq!(
            packages.resolve("src/a/b/C.java", java, "a.b.C"),
            unresolved(NOT_INDEXED)
        );
    }

    #[test]
    fn test_csharp_usings_are_one_namespace_record_and_types_reach_their_file() {
        let csharp = FileLanguage::CSharp;
        let packages = map(&[
            ("lib/Strings.cs", csharp, "Acme.Util"),
            ("other/More.cs", csharp, "Acme.Util"),
            ("lib/Json/Writer.cs", csharp, "Acme.Util.Json"),
            ("src/Store.Part.cs", csharp, "Acme.Jobs"),
            ("polyfill/Range.cs", csharp, "System.Polyfill"),
        ]);
        assert_eq!(
            packages.resolve("app/Main.cs", csharp, "Acme.Util"),
            unresolved(EDGE_DETAIL_PACKAGE)
        );
        assert_eq!(
            packages.resolve("app/Main.cs", csharp, "Acme.Jobs.Store"),
            resolved(&["src/Store.Part.cs"])
        );
        // `using static N.Outer.Inner;` reaches the outer type's file.
        assert_eq!(
            packages.resolve("app/Main.cs", csharp, "Acme.Jobs.Store.Inner"),
            resolved(&["src/Store.Part.cs"])
        );
        // A namespace the repository only declares children of is not the
        // repository's: `using System;` beside a polyfill stays external.
        assert_eq!(
            packages.resolve("app/Main.cs", csharp, "System"),
            unresolved(EXTERNAL)
        );
        assert_eq!(
            packages.resolve("app/Main.cs", csharp, "Acme"),
            unresolved(EXTERNAL)
        );
        assert_eq!(
            packages.resolve("app/Main.cs", csharp, "System.Text"),
            unresolved(EXTERNAL)
        );
    }

    #[test]
    fn test_java_and_csharp_never_resolve_into_each_other() {
        let packages = map(&[
            ("a/C.java", FileLanguage::Java, "a"),
            ("b/C.cs", FileLanguage::CSharp, "b"),
        ]);
        assert_eq!(
            packages.resolve("x.cs", FileLanguage::CSharp, "a"),
            unresolved(EXTERNAL)
        );
        assert_eq!(
            packages.resolve("x.java", FileLanguage::Java, "b.C"),
            unresolved(EXTERNAL)
        );
    }
}
