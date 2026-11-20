//! Resolution of Java imports and C# `using` directives to repository files
//! (E17-40, D105).
//!
//! Java and C# name packages and namespaces in each file's declarations,
//! not by directory, so the map is built from what files declare: the
//! declarations stored for files this run does not re-parse, and those read
//! before the run for the files it does. Only scanned, indexed paths take
//! part, so a file the scanner excludes declares nothing.
//!
//! Java: `a.b.*` names every file declaring package `a.b`; `a.b.C` (and a
//! static import `a.b.C.member`, or a nested type `a.b.C.Inner`) names the
//! files of the longest declared package prefix whose file name is `C.java`
//! (the language requires a public top-level type to live in a file of its
//! name). C#: `using N;` names every file declaring namespace `N`; a
//! `using static N.T;` or alias of a type names the files of `N` whose file
//! name starts with `T` (`T.cs`, `T.Part.cs`; a convention, not a rule of
//! the language). A name that matches no file is `not_indexed` when a
//! repository package or namespace of its language is the name, a dotted
//! prefix of it, or starts with it, and external otherwise. Java and C#
//! never resolve into each other.

use std::collections::{BTreeMap, BTreeSet};

use compresso_store::FileLanguage;

use crate::parsers::ImportResolution;
use crate::rust_imports::{EXTERNAL, NOT_INDEXED};

/// Declared packages and namespaces of one repository, built once per run.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct DeclaredPackages {
    /// (language, package or namespace) to the files declaring it, sorted.
    packages: BTreeMap<(&'static str, String), Vec<String>>,
}

impl DeclaredPackages {
    /// Builds the map from `(path, language, declared name)` rows.
    pub(crate) fn new(
        declarations: impl IntoIterator<Item = (String, FileLanguage, String)>,
    ) -> Self {
        let mut packages = BTreeMap::<(&'static str, String), BTreeSet<String>>::new();
        for (path, language, name) in declarations {
            packages
                .entry((language.as_str(), name))
                .or_default()
                .insert(path);
        }
        Self {
            packages: packages
                .into_iter()
                .map(|(key, paths)| (key, paths.into_iter().collect()))
                .collect(),
        }
    }

    /// The files an import names, each one resolved edge (never the
    /// importer itself), or one unresolved record.
    pub(crate) fn resolve(
        &self,
        importer: &str,
        language: FileLanguage,
        raw: &str,
    ) -> Vec<(Option<String>, ImportResolution)> {
        let files = match language {
            FileLanguage::Java => self.resolve_java(language, raw),
            FileLanguage::CSharp => self.resolve_csharp(language, raw),
            _ => None,
        };
        let Some(files) = files else {
            let detail = if self.shares_a_prefix(language, raw) {
                NOT_INDEXED
            } else {
                EXTERNAL
            };
            return vec![(None, ImportResolution::Unresolved(detail.to_owned()))];
        };
        let files = files
            .into_iter()
            .filter(|path| path != importer)
            .map(|path| (Some(path), ImportResolution::Resolved))
            .collect::<Vec<_>>();
        if files.is_empty() {
            vec![(None, ImportResolution::Unresolved(NOT_INDEXED.to_owned()))]
        } else {
            files
        }
    }

    fn files(&self, language: FileLanguage, package: &str) -> Option<&Vec<String>> {
        self.packages.get(&(language.as_str(), package.to_owned()))
    }

    fn resolve_java(&self, language: FileLanguage, raw: &str) -> Option<Vec<String>> {
        if let Some(package) = raw.strip_suffix(".*") {
            if let Some(files) = self.files(language, package) {
                return Some(files.clone());
            }
            // `a.b.C.*` imports the nested types of `C`.
            return self.type_files(language, package);
        }
        // The longest declared package prefix, then the type after it.
        let mut end = raw.len();
        while let Some(dot) = raw[..end].rfind('.') {
            let (package, rest) = (&raw[..dot], &raw[dot + 1..]);
            if self.files(language, package).is_some() {
                let kind = rest.split('.').next().unwrap_or(rest);
                return self.files_named(language, package, kind);
            }
            end = dot;
        }
        None
    }

    fn resolve_csharp(&self, language: FileLanguage, raw: &str) -> Option<Vec<String>> {
        if let Some(files) = self.files(language, raw) {
            return Some(files.clone());
        }
        self.type_files(language, raw)
    }

    /// `pkg.Type`: the files of `pkg` named after `Type`.
    fn type_files(&self, language: FileLanguage, name: &str) -> Option<Vec<String>> {
        let (package, kind) = name.rsplit_once('.')?;
        self.files(language, package)?;
        self.files_named(language, package, kind)
    }

    fn files_named(
        &self,
        language: FileLanguage,
        package: &str,
        kind: &str,
    ) -> Option<Vec<String>> {
        let files = self
            .files(language, package)?
            .iter()
            .filter(|path| {
                let file = path.rsplit('/').next().unwrap_or(path);
                file.split('.').next() == Some(kind)
            })
            .cloned()
            .collect::<Vec<_>>();
        Some(files)
    }

    /// Whether a repository package or namespace of `language` is `raw`, a
    /// dotted prefix of it, or has `raw` as a dotted prefix: such a name is
    /// the repository's, not a library's, even when no file matches.
    fn shares_a_prefix(&self, language: FileLanguage, raw: &str) -> bool {
        let raw = raw.strip_suffix(".*").unwrap_or(raw);
        let mut end = raw.len();
        loop {
            if self.files(language, &raw[..end]).is_some() {
                return true;
            }
            match raw[..end].rfind('.') {
                Some(dot) => end = dot,
                None => break,
            }
        }
        let below = format!("{raw}.");
        self.packages
            .range((language.as_str(), below.clone())..)
            .next()
            .is_some_and(|((found, name), _)| {
                *found == language.as_str() && name.starts_with(&below)
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn test_java_imports_resolve_by_package_and_file_name() {
        let java = FileLanguage::Java;
        let packages = map(&[
            ("src/a/b/C.java", java, "a.b"),
            ("src/a/b/D.java", java, "a.b"),
            ("legacy/E.java", java, "a.b"),
            ("src/a/b/c/F.java", java, "a.b.c"),
        ]);
        assert_eq!(
            packages.resolve("x/M.java", java, "a.b.C"),
            resolved(&["src/a/b/C.java"])
        );
        assert_eq!(
            packages.resolve("x/M.java", java, "a.b.*"),
            resolved(&["legacy/E.java", "src/a/b/C.java", "src/a/b/D.java"])
        );
        // A static import and a nested type name the outer type's file.
        assert_eq!(
            packages.resolve("x/M.java", java, "a.b.C.member"),
            resolved(&["src/a/b/C.java"])
        );
        assert_eq!(
            packages.resolve("x/M.java", java, "a.b.C.*"),
            resolved(&["src/a/b/C.java"])
        );
        assert_eq!(
            packages.resolve("x/M.java", java, "a.b.c.F"),
            resolved(&["src/a/b/c/F.java"])
        );
        assert_eq!(
            packages.resolve("x/M.java", java, "a.b.Missing"),
            unresolved(NOT_INDEXED)
        );
        assert_eq!(
            packages.resolve("x/M.java", java, "a"),
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
        // A file never imports itself.
        assert_eq!(
            packages.resolve("src/a/b/C.java", java, "a.b.C"),
            unresolved(NOT_INDEXED)
        );
    }

    #[test]
    fn test_csharp_usings_resolve_to_every_file_of_the_namespace() {
        let csharp = FileLanguage::CSharp;
        let packages = map(&[
            ("lib/Strings.cs", csharp, "Acme.Util"),
            ("other/More.cs", csharp, "Acme.Util"),
            ("lib/Json/Writer.cs", csharp, "Acme.Util.Json"),
            ("src/Store.Part.cs", csharp, "Acme.Jobs"),
        ]);
        assert_eq!(
            packages.resolve("app/Main.cs", csharp, "Acme.Util"),
            resolved(&["lib/Strings.cs", "other/More.cs"])
        );
        assert_eq!(
            packages.resolve("app/Main.cs", csharp, "Acme.Jobs.Store"),
            resolved(&["src/Store.Part.cs"])
        );
        assert_eq!(
            packages.resolve("app/Main.cs", csharp, "Acme"),
            unresolved(NOT_INDEXED)
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
