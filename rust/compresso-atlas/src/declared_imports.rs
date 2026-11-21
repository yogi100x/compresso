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

use std::collections::{BTreeMap, BTreeSet, HashMap};

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
    /// The same names, ordered, for prefix lookups.
    names: std::collections::BTreeSet<Key>,
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
        let names = packages.keys().cloned().collect();
        Self {
            packages,
            names,
            types,
        }
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
                    if Self::matched(&found) {
                        return (full, found);
                    }
                    // A namespace the repository declares only through its
                    // children: one record naming it, holding no file.
                    if self.has_children(language, &full) {
                        return (
                            full,
                            vec![(
                                None,
                                ImportResolution::Unresolved(EDGE_DETAIL_PACKAGE.to_owned()),
                            )],
                        );
                    }
                    outer = enclosing.rfind('.').map(|dot| &enclosing[..dot]);
                }
                (raw.to_owned(), self.resolve(importer, language, raw))
            }
            // A `global using` (also `global using static N.T;`) is one
            // record naming what it imports, read by queries for the files
            // of its project.
            ImportContext::Global => {
                let found = self.resolve(importer, language, raw);
                let stored = format!("{GLOBAL_IMPORT_PREFIX}{raw}");
                if Self::matched(&found) {
                    (
                        stored,
                        vec![(
                            None,
                            ImportResolution::Unresolved(EDGE_DETAIL_PACKAGE.to_owned()),
                        )],
                    )
                } else {
                    (stored, found)
                }
            }
            _ => (raw.to_owned(), self.resolve(importer, language, raw)),
        }
    }

    /// A package record or a resolved file.
    fn matched(found: &[(Option<String>, ImportResolution)]) -> bool {
        found.iter().any(|(path, resolution)| {
            path.is_some()
                || *resolution == ImportResolution::Unresolved(EDGE_DETAIL_PACKAGE.to_owned())
        })
    }

    /// Whether a declared package of `language` lies below `name`.
    fn has_children(&self, language: FileLanguage, name: &str) -> bool {
        let below = format!("{name}.");
        self.names
            .range((language.as_str(), below.clone())..)
            .next()
            .is_some_and(|(found, next)| *found == language.as_str() && next.starts_with(&below))
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

/// One stored import record of a C# file, as the store keeps it: a name (the
/// full name the `using` resolved to, or the name as written, `global:`
/// marked), a file it resolved to (the name is not kept), or a record whose
/// written name cannot be recovered (a cut list).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StoredImport<'a> {
    Name(&'a str),
    /// The resolved file, with the namespaces it declared when the record
    /// was written.
    File {
        path: &'a str,
        namespaces: &'a [String],
    },
    Unknown,
}

/// What one run changes in the C# namespace map (E17-44, D109).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct NamespaceChanges {
    /// Namespaces declared before the run and not after it, or after and
    /// not before.
    pub(crate) appeared_or_gone: BTreeSet<String>,
    /// Namespaces declared both before and after the run.
    pub(crate) kept: std::collections::HashSet<String>,
    /// `(namespace, file name up to its first .)` of every file that joined
    /// or left a namespace: the type files one `using static` or alias can
    /// name.
    pub(crate) type_files: BTreeSet<(String, String)>,
    /// The sets above as hash lookups, so one lookup costs the depth of the
    /// name, not the size of the change.
    index: ChangeIndex,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct ChangeIndex {
    /// `appeared_or_gone`.
    changed: std::collections::HashSet<String>,
    /// Every proper dotted prefix of a changed namespace.
    above_changed: std::collections::HashSet<String>,
    /// `namespace.TypeFile` of every type file.
    type_names: std::collections::HashSet<String>,
    /// Every proper dotted prefix of a type name.
    above_type_names: std::collections::HashSet<String>,
}

/// The proper dotted prefixes of `name`, longest first.
fn proper_prefixes(name: &str) -> impl Iterator<Item = &str> {
    let mut end = name.len();
    std::iter::from_fn(move || {
        let dot = name[..end].rfind('.')?;
        end = dot;
        Some(&name[..dot])
    })
}

impl NamespaceChanges {
    pub(crate) fn new(
        appeared_or_gone: BTreeSet<String>,
        kept: std::collections::HashSet<String>,
        type_files: BTreeSet<(String, String)>,
    ) -> Self {
        let type_names = type_files
            .iter()
            .map(|(namespace, stem)| format!("{namespace}.{stem}"))
            .collect::<std::collections::HashSet<_>>();
        let index = ChangeIndex {
            changed: appeared_or_gone.iter().cloned().collect(),
            above_changed: appeared_or_gone
                .iter()
                .flat_map(|name| proper_prefixes(name).map(str::to_owned))
                .collect(),
            above_type_names: type_names
                .iter()
                .flat_map(|name| proper_prefixes(name).map(str::to_owned))
                .collect(),
            type_names,
        };
        Self {
            appeared_or_gone,
            kept,
            type_files,
            index,
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.appeared_or_gone.is_empty() && self.type_files.is_empty()
    }

    /// Whether a C# file declaring `namespaces`, whose stored import records
    /// are `imports`, can resolve differently after these changes, so it
    /// must be re-parsed (E17-44, D109). A `using` inside namespace `E` is
    /// looked up as `E.W`, then as `W` relative to each enclosing namespace,
    /// then as written (`resolve_in`); the store keeps only what it resolved
    /// to. So every way the stored name can be written relative to the
    /// file's namespaces gives the full names it could have been looked up
    /// as, and the file is re-parsed when one of them can resolve
    /// differently: a changed namespace equal to it, a dotted prefix of it
    /// (the package a type import falls in), or below it (a namespace
    /// declared only through its children), unless a namespace kept across
    /// the run decides the lookup either way; or a type file joining or
    /// leaving the namespace the name falls in. A record whose written name
    /// cannot be bounded is always re-parsed.
    pub(crate) fn may_rebind(&self, namespaces: &[String], imports: &[StoredImport<'_>]) -> bool {
        if self.is_empty() {
            return false;
        }
        // Every namespace a `using` of the file can sit in or be relative
        // to: the declared namespaces and their dotted prefixes.
        let mut enclosing = std::collections::BTreeSet::<&str>::new();
        for namespace in namespaces {
            let mut end = namespace.len();
            loop {
                enclosing.insert(&namespace[..end]);
                match namespace[..end].rfind('.') {
                    Some(dot) => end = dot,
                    None => break,
                }
            }
        }
        for import in imports {
            // The stored name, or for a resolved file every
            // `namespace.TypeFile` it can have been reached by, followed by
            // a part that is not known (`open`).
            let known = match *import {
                StoredImport::Name(name) => {
                    vec![(
                        name.strip_prefix(GLOBAL_IMPORT_PREFIX)
                            .unwrap_or(name)
                            .to_owned(),
                        false,
                    )]
                }
                StoredImport::File { path, namespaces } => {
                    let file = path.rsplit('/').next().unwrap_or(path);
                    let stem = file.split('.').next().unwrap_or(file);
                    if namespaces.is_empty() {
                        return true;
                    }
                    namespaces
                        .iter()
                        .map(|namespace| (format!("{namespace}.{stem}"), true))
                        .collect()
                }
                StoredImport::Unknown => return true,
            };
            for (stored, open) in known {
                // The name as written: the stored name, or what follows an
                // enclosing namespace it starts with.
                let mut written = vec![stored.as_str()];
                for outer in &enclosing {
                    if open && is_prefix_or_equal(&stored, outer) {
                        // Written relative to a namespace below the known
                        // part: the written name is not known.
                        return true;
                    }
                    if let Some(rest) = stored
                        .strip_prefix(outer)
                        .and_then(|rest| rest.strip_prefix('.'))
                    {
                        written.push(rest);
                    }
                }
                for name in written {
                    let looked_up = std::iter::once(name.to_owned())
                        .chain(enclosing.iter().map(|outer| format!("{outer}.{name}")));
                    for full in looked_up {
                        if self.changes_lookup(&full, open) {
                            return true;
                        }
                    }
                }
            }
        }
        false
    }

    /// Whether `full` (followed by an unknown part when `open`) can resolve
    /// differently after these changes.
    fn changes_lookup(&self, full: &str, open: bool) -> bool {
        let found = self.changes_lookup_indexed(full, open);
        #[cfg(test)]
        assert_eq!(
            found,
            self.changes_lookup_by_scan(full, open),
            "{full} (open: {open})"
        );
        found
    }

    fn changes_lookup_indexed(&self, full: &str, open: bool) -> bool {
        let index = &self.index;
        #[cfg(test)]
        LOOKUP_STEPS.with(|steps| steps.set(steps.get() + 1));
        // A changed namespace or a type name equal to `full`.
        if index.changed.contains(full) || index.type_names.contains(full) {
            return true;
        }
        // One that is a dotted prefix of `full`: a type name always; a
        // changed namespace unless a namespace kept across the run lies
        // between them (`full` itself included), which then decides the
        // longest declared prefix either way.
        let mut kept_between = self.kept.contains(full);
        for prefix in proper_prefixes(full) {
            #[cfg(test)]
            LOOKUP_STEPS.with(|steps| steps.set(steps.get() + 1));
            if index.type_names.contains(prefix)
                || (index.changed.contains(prefix) && (open || !kept_between))
            {
                return true;
            }
            kept_between |= self.kept.contains(prefix);
        }
        // One below `full`, unless `full` itself is kept; for an open name,
        // also a type name below it.
        (index.above_changed.contains(full) && (open || !self.kept.contains(full)))
            || (open && index.above_type_names.contains(full))
    }

    /// The scan `changes_lookup` replaced, kept as its test oracle.
    #[cfg(test)]
    fn changes_lookup_by_scan(&self, full: &str, open: bool) -> bool {
        for changed in &self.appeared_or_gone {
            if open {
                if is_prefix_or_equal(changed, full) || is_prefix_or_equal(full, changed) {
                    return true;
                }
                continue;
            }
            if changed == full {
                return true;
            }
            // The longest declared prefix of `full` may now be `changed`,
            // unless a namespace kept across the run lies between them.
            if is_proper_prefix(changed, full) && !self.kept_between(changed, full) {
                return true;
            }
            // `full` may now be or stop being declared only through its
            // children, unless `full` itself is declared throughout.
            if is_proper_prefix(full, changed) && !self.kept.contains(full) {
                return true;
            }
        }
        self.type_files.iter().any(|(namespace, stem)| {
            let type_name = format!("{namespace}.{stem}");
            is_prefix_or_equal(&type_name, full) || (open && is_prefix_or_equal(full, &type_name))
        })
    }

    /// A kept namespace that `changed` is a proper dotted prefix of and that
    /// is itself `full` or a dotted prefix of it.
    #[cfg(test)]
    fn kept_between(&self, changed: &str, full: &str) -> bool {
        let mut end = full.len();
        while end > changed.len() {
            if self.kept.contains(&full[..end]) {
                return true;
            }
            match full[..end].rfind('.') {
                Some(dot) => end = dot,
                None => return false,
            }
        }
        false
    }
}

#[cfg(test)]
thread_local! {
    /// Steps `changes_lookup` took on this thread (tests only): pins that a
    /// lookup costs the depth of the name, not the size of the change.
    static LOOKUP_STEPS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// `prefix` is `name` or a dotted prefix of it.
fn is_prefix_or_equal(prefix: &str, name: &str) -> bool {
    prefix == name || is_proper_prefix(prefix, name)
}

/// `prefix` is a dotted prefix of `name` shorter than it.
fn is_proper_prefix(prefix: &str, name: &str) -> bool {
    name.len() > prefix.len() && name.starts_with(prefix) && name.as_bytes()[prefix.len()] == b'.'
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

    /// E17-44 (D109): records whose written names cannot be bounded (a cut
    /// list, a file whose namespaces are not known, a file below one of the
    /// importer's namespaces) always re-parse; nothing re-parses when no
    /// namespace or type file changed.
    #[test]
    fn test_may_rebind_is_conservative_where_the_written_name_is_unknown() {
        let changes = NamespaceChanges::new(
            BTreeSet::from(["Far.Away".to_owned()]),
            std::collections::HashSet::new(),
            BTreeSet::new(),
        );
        let mine = ["Acme.Web".to_owned()];
        let util = ["Acme.Util".to_owned()];
        assert!(changes.may_rebind(&mine, &[StoredImport::Unknown]));
        assert!(changes.may_rebind(
            &mine,
            &[StoredImport::File {
                path: "gone/Strings.cs",
                namespaces: &[],
            }]
        ));
        // `using static Web.Strings;` inside `Acme.Web.Strings.Tests` can
        // be written relative to a namespace below the type it reached.
        let web = ["Acme.Web".to_owned()];
        assert!(changes.may_rebind(
            &["Acme.Web.Strings.Tests".to_owned()],
            &[StoredImport::File {
                path: "web/Strings.cs",
                namespaces: &web,
            }]
        ));
        assert!(!changes.may_rebind(
            &mine,
            &[
                StoredImport::Name("Acme.Util"),
                StoredImport::Name("global:System"),
                StoredImport::File {
                    path: "util/Strings.cs",
                    namespaces: &util,
                },
            ]
        ));
        assert!(!NamespaceChanges::default().may_rebind(&mine, &[StoredImport::Unknown]));
    }

    /// E17-44 (D109): one importer's check costs the depth of its names, not
    /// the size of the change: a pull adding thousands of files in hundreds
    /// of namespaces must not make every importer scan them all (a first
    /// cut did, and adding 500 files to existing namespaces of a 5,000-file
    /// repository took 5 s instead of 0.6 s). Counted, not timed.
    #[test]
    fn test_may_rebind_cost_does_not_grow_with_the_change() {
        let changes = NamespaceChanges::new(
            (0..10_000).map(|index| format!("Far.N{index}")).collect(),
            (0..1_000).map(|index| format!("Near.K{index}")).collect(),
            (0..10_000)
                .map(|index| (format!("Near.K{}", index % 1_000), format!("T{index}")))
                .collect(),
        );
        let mine = ["Acme.Web.Pages".to_owned()];
        let util = ["Acme.Util".to_owned()];
        let imports = [
            StoredImport::Name("Acme.Util"),
            StoredImport::Name("global:System.Text"),
            StoredImport::File {
                path: "util/Strings.cs",
                namespaces: &util,
            },
        ];
        LOOKUP_STEPS.with(|steps| steps.set(0));
        assert!(!changes.may_rebind(&mine, &imports));
        let steps = LOOKUP_STEPS.with(std::cell::Cell::get);
        assert!(steps <= 256, "{steps} lookup steps");
    }

    /// A small deterministic generator (no dependency).
    struct Lcg(u64);

    impl Lcg {
        fn below(&mut self, bound: usize) -> usize {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            ((self.0 >> 33) % bound as u64) as usize
        }

        fn name(&mut self, segments: &[&str], max_depth: usize) -> String {
            let depth = 1 + self.below(max_depth);
            (0..depth)
                .map(|_| segments[self.below(segments.len())])
                .collect::<Vec<_>>()
                .join(".")
        }
    }

    /// `(path, declared namespaces)` of each library file.
    type Library = Vec<(String, Vec<String>)>;

    /// What the importer's `using` directives store against `library`
    /// (files besides the importer, which declares `importer.0`).
    fn stored_records(
        library: &Library,
        importer: &(Vec<String>, Vec<(String, ImportContext)>),
    ) -> Vec<(String, Option<String>, ImportResolution)> {
        let csharp = FileLanguage::CSharp;
        let mut rows = library
            .iter()
            .flat_map(|(path, namespaces)| {
                namespaces
                    .iter()
                    .map(move |namespace| (path.clone(), csharp, namespace.clone()))
            })
            .collect::<Vec<_>>();
        for namespace in &importer.0 {
            rows.push(("imp/Importer.cs".to_owned(), csharp, namespace.clone()));
        }
        let packages = DeclaredPackages::new(rows);
        let mut records = Vec::new();
        for (raw, context) in &importer.1 {
            let (stored, targets) = packages.resolve_in("imp/Importer.cs", csharp, raw, context);
            for (path, resolution) in targets {
                records.push((stored.clone(), path, resolution));
            }
        }
        records.sort_by(|left, right| format!("{left:?}").cmp(&format!("{right:?}")));
        records
    }

    /// E17-44 (D109): over a small universe of namespaces, type files, and
    /// `using` forms (plain, static or alias by name, global, relative in a
    /// block or nested namespace), one to three changes per case (a file
    /// added, removed, or moved; files and the importer declaring one or two
    /// namespaces), every change that alters what the importer stores is
    /// selected by `may_rebind`, which sees only what the store keeps; the
    /// indexed lookup agrees with the scan it replaced on every name it is
    /// asked about; and most unaffected importers are left alone.
    #[test]
    fn test_may_rebind_selects_every_importer_whose_records_change() {
        let segments = ["A", "B", "U"];
        let stems = ["A", "B", "U", "T"];
        let mut random = Lcg(0x5eed);
        let (mut changed, mut selected_unchanged, mut unselected) = (0, 0, 0);
        for case in 0..60_000 {
            let namespaces_of = |random: &mut Lcg| {
                (0..1 + usize::from(random.below(4) == 0))
                    .map(|_| random.name(&segments, 3))
                    .collect::<Vec<_>>()
            };
            let mut before = Library::new();
            for index in 0..random.below(6) {
                let stem = stems[random.below(stems.len())];
                before.push((format!("lib{index}/{stem}.cs"), namespaces_of(&mut random)));
            }
            let mut after = before.clone();
            for change in 0..1 + random.below(3) {
                match random.below(3) {
                    0 => {
                        let stem = stems[random.below(stems.len())];
                        after.push((format!("new{change}/{stem}.cs"), namespaces_of(&mut random)));
                    }
                    1 if !after.is_empty() => {
                        after.remove(random.below(after.len()));
                    }
                    _ if !after.is_empty() => {
                        let at = random.below(after.len());
                        after[at].1 = namespaces_of(&mut random);
                    }
                    _ => {}
                }
            }
            if before == after {
                continue;
            }
            let own = if random.below(4) > 0 {
                namespaces_of(&mut random)
            } else {
                Vec::new()
            };
            let usings = (0..1 + random.below(3))
                .map(|_| {
                    let raw = random.name(&stems, 3);
                    let context = match (random.below(3), own.first()) {
                        (0, _) => ImportContext::Global,
                        (1, Some(_)) => {
                            ImportContext::Namespace(own[random.below(own.len())].clone())
                        }
                        _ => ImportContext::None,
                    };
                    (raw, context)
                })
                .collect::<Vec<_>>();
            let importer = (own.clone(), usings);
            let old = stored_records(&before, &importer);
            let new = stored_records(&after, &importer);

            let declared = |library: &Library| {
                library
                    .iter()
                    .flat_map(|(_, namespaces)| namespaces.iter().cloned())
                    .chain(own.iter().cloned())
                    .collect::<std::collections::HashSet<_>>()
            };
            let (names_before, names_after) = (declared(&before), declared(&after));
            let stem = |path: &str| {
                path.rsplit('/')
                    .next()
                    .unwrap()
                    .split('.')
                    .next()
                    .unwrap()
                    .to_owned()
            };
            // As the indexer notes them: every file added, removed, or
            // declaring other namespaces, with what it declared before and
            // declares now.
            let mut type_files = BTreeSet::new();
            let paths = before
                .iter()
                .chain(&after)
                .map(|(path, _)| path.clone())
                .collect::<BTreeSet<_>>();
            for path in paths {
                let then = before.iter().find(|(p, _)| *p == path).map(|(_, n)| n);
                let now = after.iter().find(|(p, _)| *p == path).map(|(_, n)| n);
                if then != now {
                    for namespace in then.into_iter().chain(now).flatten() {
                        type_files.insert((namespace.clone(), stem(&path)));
                    }
                }
            }
            let changes = NamespaceChanges::new(
                names_before
                    .symmetric_difference(&names_after)
                    .cloned()
                    .collect(),
                names_before.intersection(&names_after).cloned().collect(),
                type_files,
            );
            let file_namespaces = before.iter().cloned().collect::<BTreeMap<_, _>>();
            let stored = old
                .iter()
                .map(|(name, path, _)| match path {
                    Some(path) => StoredImport::File {
                        path,
                        namespaces: &file_namespaces[path],
                    },
                    None => StoredImport::Name(name),
                })
                .collect::<Vec<_>>();
            let namespaces = own.clone();
            let selected = changes.may_rebind(&namespaces, &stored);
            if old != new {
                changed += 1;
                assert!(
                    selected,
                    "case {case}: {importer:?}\nbefore {before:?}\nafter {after:?}\n{old:?}\n{new:?}"
                );
            } else if selected {
                selected_unchanged += 1;
            } else {
                unselected += 1;
            }
        }
        assert!(changed > 1_000, "{changed} changed cases");
        assert!(
            unselected > selected_unchanged,
            "{unselected} left alone, {selected_unchanged} re-parsed needlessly"
        );
    }
}
