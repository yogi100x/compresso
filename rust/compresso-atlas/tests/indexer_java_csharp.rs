//! Java and C# across the index (E17-40, D105): imports resolve through
//! declared packages and namespaces, calls resolve by the languages'
//! visibility and scope rules, module cards read their docs, incremental
//! runs re-resolve importers, and an older store re-indexes.

use std::fs;
use std::path::Path;

use compresso_atlas::{
    CallResolution, CardOptions, ImpactOptions, Indexer, TraceOptions, TraceResult, impact,
    impact_with_options, module_cards, repo_map, trace,
};
use compresso_core::{TokenProvider, counter_for};
use compresso_store::{EdgeKind, FileLanguage, RepoId, Store, SymbolKind};
use tempfile::TempDir;

const JAVA: [(&str, &str); 9] = [
    (
        "src/main/java/com/acme/store/JobStore.java",
        "package com.acme.store;

import com.acme.util.Strings;
import com.acme.util.*;
import java.util.Collections;
import java.util.List;

public class JobStore extends BaseStore {
    public void load(String key) {
        validate(key);
        this.flush();
        Strings.trim(key);
        Keys.normalize(key);
        Collections.sort(null);
        System.out.println(key);
        Hidden.secret();
        Fixtures.make();
        Missing.call();
    }

    private void validate(String key) {}

    @Override
    void flush() {
        super.flush();
    }

    public static void main(String[] args) {
        new JobStore().load(args[0]);
    }
}
",
    ),
    (
        "src/main/java/com/acme/store/BaseStore.java",
        "package com.acme.store;

abstract class BaseStore {
    void flush() {}
}
",
    ),
    (
        "src/main/java/com/acme/store/Keys.java",
        "package com.acme.store;

public class Keys {
    public static String normalize(String key) { return key; }
}
",
    ),
    (
        "src/main/java/com/acme/store/Hidden.java",
        "package com.acme.store;

class Hidden {
    private static void secret() {}
}
",
    ),
    (
        "src/main/java/com/acme/util/package-info.java",
        "/**
 * String helpers shared by the job store. Not thread-safe.
 */
package com.acme.util;
",
    ),
    (
        "src/main/java/com/acme/util/Strings.java",
        "package com.acme.util;

public final class Strings {
    public static String trim(String text) { return text; }
    static String pad(String text) { return text; }
}
",
    ),
    (
        "legacy/Sorter.java",
        "package com.acme.util;

public class Sorter {
    public static void sort(Object items) {}
    public void println(String text) {}
    public static void make() {}
}
",
    ),
    (
        "src/test/java/com/acme/store/Fixtures.java",
        "package com.acme.store;

class Fixtures {
    static JobStore make() { return new JobStore(); }
}
",
    ),
    (
        "src/test/java/com/acme/store/JobStoreTest.java",
        "package com.acme.store;

class JobStoreTest {
    void loads() {
        Fixtures.make().load(\"a\");
        Keys.normalize(\"x\");
    }
}
",
    ),
];

const CSHARP: [(&str, &str); 6] = [
    (
        "src/Acme.Jobs/JobStore.cs",
        "using Acme.Util;
using System;

namespace Acme.Jobs
{
    public partial class JobStore : StoreBase
    {
        public void Load(string key)
        {
            Validate(key);
            this.Flush();
            Strings.Trim(key);
            Keys.Normalize(key);
            Keys.Hidden();
            Console.WriteLine(key);
            base.Dispose();
        }

        public void Flush() { }

        public void Dispose() { }
    }
}
",
    ),
    (
        "src/Acme.Jobs/JobStore.Validation.cs",
        "namespace Acme.Jobs
{
    public partial class JobStore
    {
        private void Validate(string key) { }
    }

    public class StoreBase
    {
        public void Dispose() { }
    }
}
",
    ),
    (
        "src/Acme.Jobs/Keys.cs",
        "namespace Acme.Jobs;

public static class Keys
{
    public static string Normalize(string key) => key;

    static void Hidden() { }
}
",
    ),
    (
        "lib/Util/Strings.cs",
        "namespace Acme.Util
{
    /// <summary>String helpers shared by the job store.</summary>
    public static class Strings
    {
        public static string Trim(string text) => text;
    }
}
",
    ),
    (
        "lib/Util/More.cs",
        "namespace Acme.Util
{
    internal class More { }
}
",
    ),
    (
        "lib/Logging/Logger.cs",
        "namespace Acme.Logging
{
    public class Logger
    {
        public void WriteLine(string text) { }
    }
}
",
    ),
];

fn index_files(files: &[(&str, &str)]) -> (TempDir, Store, RepoId) {
    let temp = tempfile::tempdir().expect("temporary directory");
    let root = temp.path().join("repo");
    write_all(&root, files);
    let store = Store::open(&temp.path().join("compresso.db")).expect("store opens");
    Indexer::new(&store, counter_for(TokenProvider::Heuristic))
        .index(&root, |_| {})
        .expect("index");
    let repo = repo_of(&store, &root);
    (temp, store, repo)
}

fn write_all(root: &Path, files: &[(&str, &str)]) {
    for (path, source) in files {
        let absolute = root.join(path);
        fs::create_dir_all(absolute.parent().expect("parent")).expect("directory");
        fs::write(absolute, source).expect("file");
    }
}

fn repo_of(store: &Store, root: &Path) -> RepoId {
    let canonical = fs::canonicalize(root).expect("root canonicalizes");
    store
        .repo_by_root(&canonical)
        .expect("lookup")
        .expect("repository")
        .id
}

fn reindex(store: &Store, temp: &TempDir) {
    Indexer::new(store, counter_for(TokenProvider::Heuristic))
        .index(&temp.path().join("repo"), |_| {})
        .expect("reindex");
}

fn edges_from(store: &Store, repo: RepoId, from: &str) -> Vec<String> {
    let mut edges = store
        .resolved_edges(repo)
        .expect("edges")
        .into_iter()
        .filter(|edge| edge.from_path == from)
        .map(|edge| edge.to_path)
        .collect::<Vec<_>>();
    edges.sort();
    edges
}

/// Direct dependents with how they are linked: `imports` (an import edge
/// or an import of the package), `references` (another file of the same
/// package or namespace), and the package that links them, if any.
fn linked_dependents(
    store: &Store,
    repo: RepoId,
    path: &str,
) -> Vec<(String, &'static str, Option<String>)> {
    impact_with_options(
        store,
        repo,
        path,
        ImpactOptions {
            max_depth: 1,
            max_results: 500,
        },
    )
    .expect("impact")
    .dependents
    .into_iter()
    .map(|entry| {
        let kind = match entry.edge_kinds.first() {
            Some(EdgeKind::References) => "references",
            _ => "imports",
        };
        (entry.path, kind, entry.package)
    })
    .collect()
}

fn importers_of(store: &Store, repo: RepoId, path: &str) -> Vec<String> {
    linked_dependents(store, repo, path)
        .into_iter()
        .filter(|(_, kind, _)| *kind == "imports")
        .map(|(path, _, _)| path)
        .collect()
}

fn package_records_from(store: &Store, repo: RepoId, from: &str) -> Vec<String> {
    store
        .unresolved_edge_targets_with_detail(repo, compresso_store::EDGE_DETAIL_PACKAGE)
        .expect("records")
        .into_iter()
        .filter(|(path, _)| path == from)
        .map(|(_, raw)| raw)
        .collect()
}

fn callees_of(store: &Store, repo: RepoId, from: &str) -> Vec<(String, CallResolution)> {
    let report = trace(store, repo, Some(from), None, TraceOptions::default()).expect("trace");
    match report.result {
        TraceResult::Callees(callees) => callees
            .into_iter()
            .map(|callee| (callee.symbol.qualified_name, callee.resolution))
            .collect(),
        TraceResult::NoPath => Vec::new(),
        other => panic!("expected callees, got {other:?}"),
    }
}

#[test]
fn test_java_and_csharp_files_are_indexed_with_language_and_symbols() {
    let (_temp, store, repo) = index_files(&JAVA);
    let file = store
        .indexed_file(repo, "legacy/Sorter.java")
        .expect("lookup")
        .expect("file");
    assert_eq!(file.language, Some(FileLanguage::Java));
    let symbols = store
        .symbols_in_file(repo, "legacy/Sorter.java")
        .expect("symbols")
        .into_iter()
        .map(|symbol| (symbol.qualified_name, symbol.kind))
        .collect::<Vec<_>>();
    assert_eq!(
        symbols,
        [
            ("com.acme.util".to_owned(), SymbolKind::Module),
            ("Sorter".to_owned(), SymbolKind::Class),
            ("Sorter::sort".to_owned(), SymbolKind::Method),
            ("Sorter::println".to_owned(), SymbolKind::Method),
            ("Sorter::make".to_owned(), SymbolKind::Method),
        ]
    );
    let (_temp, store, repo) = index_files(&CSHARP);
    let file = store
        .indexed_file(repo, "lib/Util/Strings.cs")
        .expect("lookup")
        .expect("file");
    assert_eq!(file.language, Some(FileLanguage::CSharp));
}

/// Owner decision 2026-11-21: a wildcard import is one record naming its
/// package, and `impact` finds the package's files when it reads it, by
/// declaration, not directory (`legacy/Sorter.java` declares
/// `com.acme.util`); `a.b.C` is one edge to `C.java`; files of the same
/// package depend on each other without an import (`references`); a
/// `package-info.java` declares no type and gains no dependent.
#[test]
fn test_java_imports_are_package_records_and_type_edges_expanded_by_impact() {
    let (_temp, store, repo) = index_files(&JAVA);
    let job_store = "src/main/java/com/acme/store/JobStore.java";
    assert_eq!(
        edges_from(&store, repo, job_store),
        ["src/main/java/com/acme/util/Strings.java"]
    );
    assert_eq!(
        package_records_from(&store, repo, job_store),
        ["com.acme.util.*"]
    );
    assert_eq!(
        linked_dependents(&store, repo, "legacy/Sorter.java"),
        [
            (
                job_store.to_owned(),
                "imports",
                Some("com.acme.util".to_owned())
            ),
            (
                "src/main/java/com/acme/util/Strings.java".to_owned(),
                "references",
                Some("com.acme.util".to_owned())
            ),
        ]
    );
    assert!(
        linked_dependents(
            &store,
            repo,
            "src/main/java/com/acme/util/package-info.java"
        )
        .is_empty()
    );
    let report = impact(&store, repo, "legacy/Sorter.java").expect("impact");
    assert!(report.complete, "package records hide no dependent");
    assert_eq!(report.total, report.dependents.len());
    let external = store
        .external_edges(repo)
        .expect("external")
        .into_iter()
        .filter(|(from, _)| from == job_store)
        .map(|(_, raw)| raw)
        .collect::<Vec<_>>();
    assert_eq!(external, ["java.util.Collections", "java.util.List"]);
}

/// A `using` is one namespace record; `impact` lists its importers and the
/// files of the same namespace (C# also sees enclosing namespaces), so a
/// same-namespace caller with no `using` is a dependent (review M4).
#[test]
fn test_csharp_usings_are_namespace_records_and_impact_lists_namespace_mates() {
    let (_temp, store, repo) = index_files(&CSHARP);
    assert!(edges_from(&store, repo, "src/Acme.Jobs/JobStore.cs").is_empty());
    assert_eq!(
        package_records_from(&store, repo, "src/Acme.Jobs/JobStore.cs"),
        ["Acme.Util"],
        "System is external"
    );
    assert_eq!(
        importers_of(&store, repo, "lib/Util/More.cs"),
        ["src/Acme.Jobs/JobStore.cs"]
    );
    let keys = linked_dependents(&store, repo, "src/Acme.Jobs/Keys.cs");
    assert!(
        keys.contains(&(
            "src/Acme.Jobs/JobStore.cs".to_owned(),
            "references",
            Some("Acme.Jobs".to_owned())
        )),
        "{keys:?}"
    );
    assert!(
        impact(&store, repo, "src/Acme.Jobs/Keys.cs")
            .expect("impact")
            .complete
    );
}

/// `impact` bounds what it lists, not what it counts: totals are exact.
#[test]
fn test_impact_over_a_large_namespace_reports_exact_totals() {
    let mut files = vec![(
        "app/Main.cs".to_owned(),
        "using Big;\n\nnamespace App;\n\npublic class Main { }\n".to_owned(),
    )];
    for index in 0..600 {
        files.push((
            format!("big/F{index}.cs"),
            format!("namespace Big;\n\npublic class F{index} {{ }}\n"),
        ));
    }
    let borrowed = files
        .iter()
        .map(|(path, source)| (path.as_str(), source.as_str()))
        .collect::<Vec<_>>();
    let (_temp, store, repo) = index_files(&borrowed);
    assert_eq!(package_records_from(&store, repo, "app/Main.cs"), ["Big"]);
    let report = impact_with_options(
        &store,
        repo,
        "big/F0.cs",
        ImpactOptions {
            max_depth: 1,
            max_results: 50,
        },
    )
    .expect("impact");
    assert_eq!(report.total, 600, "599 namespace mates and one importer");
    assert_eq!(report.dependents.len(), 50);
    assert!(report.truncated && !report.complete);
    assert_eq!(
        report.dependents[0].path, "app/Main.cs",
        "importers sort first"
    );
}

/// Unqualified, `this.`, type-qualified, and same-package calls resolve by
/// Java's rules; `super.` reaches the declared base type, never the caller's
/// own type; library calls stay external even where a repository method
/// shares the name; a private method is out of reach from another file;
/// test code is reached only from test code.
#[test]
fn test_java_calls_resolve_by_scope_and_visibility() {
    let (_temp, store, repo) = index_files(&JAVA);
    let mut found = callees_of(&store, repo, "JobStore::load");
    found.sort_by(|left, right| left.0.cmp(&right.0));
    assert_eq!(
        found,
        [
            ("JobStore::flush".to_owned(), CallResolution::SameFile),
            ("JobStore::validate".to_owned(), CallResolution::SameFile),
            ("Keys::normalize".to_owned(), CallResolution::SamePackage),
            ("Strings::trim".to_owned(), CallResolution::Import),
        ],
        "Collections.sort and System.out.println are library calls; \
         Hidden.secret is private; Fixtures is test code; Missing is unknown"
    );
    assert_eq!(
        callees_of(&store, repo, "JobStore::flush"),
        [("BaseStore::flush".to_owned(), CallResolution::SamePackage)],
        "super.flush() reaches the base type, never JobStore::flush"
    );
    let mut from_test = callees_of(&store, repo, "JobStoreTest::loads");
    from_test.sort_by(|left, right| left.0.cmp(&right.0));
    assert_eq!(
        from_test,
        [
            ("Fixtures::make".to_owned(), CallResolution::SamePackage),
            // `Fixtures.make().load(..)`: a receiver call, by package.
            ("JobStore::load".to_owned(), CallResolution::SamePackage),
            ("Keys::normalize".to_owned(), CallResolution::SamePackage),
        ]
    );
    assert!(
        callees_of(&store, repo, "Sorter::make").is_empty(),
        "nothing calls out of an empty body"
    );
}

#[test]
fn test_csharp_calls_resolve_by_scope_partial_types_and_visibility() {
    let (_temp, store, repo) = index_files(&CSHARP);
    let mut found = callees_of(&store, repo, "JobStore::Load");
    found.sort_by(|left, right| left.0.cmp(&right.0));
    assert_eq!(
        found,
        [
            ("JobStore::Flush".to_owned(), CallResolution::SameFile),
            ("JobStore::Validate".to_owned(), CallResolution::SamePackage),
            ("Keys::Normalize".to_owned(), CallResolution::SamePackage),
            ("StoreBase::Dispose".to_owned(), CallResolution::SamePackage),
            ("Strings::Trim".to_owned(), CallResolution::Import),
        ],
        "Console.WriteLine is a library call, not Logger::WriteLine; \
         Keys.Hidden is private; base.Dispose is never JobStore::Dispose"
    );
}

/// A variable's method call has an unknown receiver type: it falls back to
/// a method of that name, always labelled `name_only`; a qualifier that is
/// an external import or a library type never reaches a repository method.
#[test]
fn test_java_library_look_alikes_stay_external_and_variables_are_name_only() {
    let (_temp, store, repo) = index_files(&[
        (
            "app/Main.java",
            "package app;

import java.util.Collections;

class Main {
    void run(Sorter sorter) {
        Collections.sort(null);
        java.util.Objects.equals(null, null);
        String.valueOf(1);
        sorter.sort(null);
    }
}
",
        ),
        (
            "app/Sorter.java",
            "package app;

public class Sorter {
    public void sort(Object items) {}
    public boolean equals(Object other) { return true; }
    public static String valueOf(int x) { return \"\"; }
}
",
        ),
    ]);
    assert_eq!(
        callees_of(&store, repo, "Main::run"),
        [("Sorter::sort".to_owned(), CallResolution::NameOnly)]
    );
}

/// A C# `using` of an external namespace names the namespace, not its last
/// part: a repository type that shares that last part still resolves.
#[test]
fn test_an_external_csharp_namespace_never_shadows_a_repository_type() {
    let (_temp, store, repo) = index_files(&[
        (
            "app/Main.cs",
            "using Vendor.Configuration;\n\nnamespace App;\n\npublic class Main\n{\n    public void Run() { Configuration.Load(); }\n}\n",
        ),
        (
            "app/Configuration.cs",
            "namespace App;\n\npublic static class Configuration { public static void Load() { } }\n",
        ),
    ]);
    assert_eq!(
        callees_of(&store, repo, "Main::Run"),
        [(
            "Configuration::Load".to_owned(),
            CallResolution::SamePackage
        )]
    );
}

/// The repository map lists a Java or C# file's types and members, not the
/// package or namespace symbol every file of the package repeats.
#[test]
fn test_the_map_lists_types_not_package_or_namespace_symbols() {
    let (_temp, store, repo) = index_files(&[
        (
            "a/Store.java",
            "package com.acme.store;\n\npublic class Store { void save() {} }\n",
        ),
        (
            "b/Keys.cs",
            "namespace Acme.Keys\n{\n    public class Keys { void Make() { } }\n}\n",
        ),
    ]);
    let map = repo_map(&store, repo, 2_000).expect("map");
    assert!(
        map.rendered.contains("Store.java  — Store, Store::save"),
        "{}",
        map.rendered
    );
    assert!(
        map.rendered.contains("Keys.cs  — Keys, Keys::Make"),
        "{}",
        map.rendered
    );
    assert!(!map.rendered.contains("com.acme.store"), "{}", map.rendered);
    assert!(!map.rendered.contains("Acme.Keys"), "{}", map.rendered);
}

/// Review M2(a): interface members without a modifier are public in both
/// languages, so callers in other packages reach them.
#[test]
fn test_interface_members_without_modifiers_are_public() {
    let (_temp, store, repo) = index_files(&[
        (
            "a/Codec.java",
            "package a;\n\npublic interface Codec {\n    static Codec of() { return null; }\n    void encode();\n}\n",
        ),
        (
            "b/Use.java",
            "package b;\n\nimport a.Codec;\n\nclass Use {\n    void run(Codec codec) {\n        Codec.of();\n        codec.encode();\n    }\n}\n",
        ),
        (
            "A/IJobStore.cs",
            "namespace A;\n\npublic interface IJobStore\n{\n    void Save();\n}\n",
        ),
        (
            "B/Runner.cs",
            "using A;\n\nnamespace B;\n\npublic class Runner\n{\n    public void Run(IJobStore store) { store.Save(); }\n}\n",
        ),
    ]);
    let mut java = callees_of(&store, repo, "Use::run");
    java.sort_by(|left, right| left.0.cmp(&right.0));
    assert_eq!(
        java,
        [
            ("Codec::encode".to_owned(), CallResolution::NameOnly),
            ("Codec::of".to_owned(), CallResolution::Import),
        ]
    );
    assert_eq!(
        callees_of(&store, repo, "Runner::Run"),
        [("IJobStore::Save".to_owned(), CallResolution::NameOnly)]
    );
}

/// Review M2(b), M2(c), M3: a Java subclass in another package reaches its
/// base's `protected` members; inherited methods are found up the declared
/// chain (a grandparent's); and an owner is matched by its full path, so
/// `Foo::Builder` never reaches `Bar::Builder::reset`.
#[test]
fn test_inherited_and_protected_members_resolve_through_the_supertype_chain() {
    let (_temp, store, repo) = index_files(&[
        (
            "a/GrandBase.java",
            "package a;\n\npublic class GrandBase {\n    public void reset() {}\n}\n",
        ),
        (
            "a/Base.java",
            "package a;\n\npublic class Base extends GrandBase {\n    protected void helper() {}\n}\n",
        ),
        (
            "b/Child.java",
            "package b;\n\nimport a.Base;\n\nclass Child extends Base {\n    void run() {\n        helper();\n        reset();\n    }\n}\n",
        ),
        (
            "c/Foo.java",
            "package c;\n\nclass Foo {\n    static class Builder extends Reset {\n        void build() { reset(); }\n    }\n}\n",
        ),
        (
            "c/Bar.java",
            "package c;\n\nclass Bar {\n    static class Builder {\n        void reset() {}\n    }\n}\n",
        ),
        (
            "c/Reset.java",
            "package c;\n\nclass Reset {\n    void reset() {}\n}\n",
        ),
    ]);
    let mut found = callees_of(&store, repo, "Child::run");
    found.sort_by(|left, right| left.0.cmp(&right.0));
    assert_eq!(
        found,
        [
            ("Base::helper".to_owned(), CallResolution::Import),
            ("GrandBase::reset".to_owned(), CallResolution::UniqueName),
        ]
    );
    assert_eq!(
        callees_of(&store, repo, "Foo::Builder::build"),
        [("Reset::reset".to_owned(), CallResolution::SamePackage)]
    );
}

/// Review M2: a call the rules cannot place, with repository methods of
/// that name and a supertype outside the repository, makes the answer
/// incomplete instead of silently complete.
#[test]
fn test_a_call_that_may_be_inherited_from_a_library_type_is_unplaced() {
    let (_temp, store, repo) = index_files(&[
        (
            "a/Items.java",
            "package a;\n\nimport java.util.ArrayList;\n\nclass Items extends ArrayList<String> {\n    void fill() { add(\"x\"); }\n}\n",
        ),
        (
            "a/Cart.java",
            "package a;\n\nclass Cart {\n    void add(String item) {}\n}\n",
        ),
    ]);
    let report = trace(
        &store,
        repo,
        Some("Items::fill"),
        None,
        TraceOptions::default(),
    )
    .expect("trace");
    assert!(!report.complete);
    assert_eq!(report.unfollowed.len(), 1);
    assert_eq!(report.unfollowed[0].status, "unplaced");
}

/// Review S3: a capitalized qualifier that is a member of the caller's type
/// (a C# property, a Java constant field) is a value of unknown type, not
/// a library type: its call falls back to `name_only`.
#[test]
fn test_capitalized_member_qualifiers_are_values_not_library_types() {
    let (_temp, store, repo) = index_files(&[
        (
            "A/Cart.cs",
            "namespace A;\n\npublic class Cart\n{\n    public Ledger Items { get; }\n    public void Fill() { Items.Post(1); }\n}\n",
        ),
        (
            "A/Ledger.cs",
            "namespace A;\n\npublic class Ledger\n{\n    public void Post(int x) { }\n}\n",
        ),
        (
            "a/Job.java",
            "package a;\n\nclass Job {\n    static final Runner INSTANCE = new Runner();\n    void go() { INSTANCE.start(); }\n}\n",
        ),
        (
            "a/Runner.java",
            "package a;\n\nclass Runner {\n    void start() {}\n}\n",
        ),
    ]);
    assert_eq!(
        callees_of(&store, repo, "Cart::Fill"),
        [("Ledger::Post".to_owned(), CallResolution::NameOnly)]
    );
    assert_eq!(
        callees_of(&store, repo, "Job::go"),
        [("Runner::start".to_owned(), CallResolution::NameOnly)]
    );
}

/// Review S4: an annotation longer than the signature bound never hides
/// `public` from the visibility rules.
#[test]
fn test_a_long_annotation_never_hides_the_public_modifier() {
    let long = "x".repeat(2_000);
    let util = format!(
        "package a;\n\npublic class Util {{\n    @Doc(\"{long}\")\n    public static void util() {{}}\n}}\n"
    );
    let (_temp, store, repo) = index_files(&[
        ("a/Util.java", util.as_str()),
        (
            "b/Use.java",
            "package b;\n\nimport a.Util;\n\nclass Use {\n    void run() { Util.util(); }\n}\n",
        ),
    ]);
    assert_eq!(
        callees_of(&store, repo, "Use::run"),
        [("Util::util".to_owned(), CallResolution::Import)]
    );
    let signature = store
        .symbols_in_file(repo, "a/Util.java")
        .expect("symbols")
        .into_iter()
        .find(|symbol| symbol.qualified_name == "Util::util")
        .and_then(|symbol| symbol.signature)
        .expect("signature");
    assert!(
        signature.starts_with("public static void util()"),
        "{signature}"
    );
}

#[test]
fn test_java_and_csharp_calls_never_cross_languages() {
    let (_temp, store, repo) = index_files(&[
        (
            "a/Main.java",
            "package a;\n\nclass Main {\n    void run(Object x) { x.Save(); helper(); }\n}\n",
        ),
        (
            "b/Repo.cs",
            "namespace B;\n\npublic class Repo { public void Save() { } public void helper() { } }\n",
        ),
    ]);
    assert!(callees_of(&store, repo, "Main::run").is_empty());
}

/// Overloads are one symbol each; a call to an overloaded name is listed as
/// ambiguous with every overload rather than resolved to one.
#[test]
fn test_overloaded_calls_are_listed_as_ambiguous() {
    let (_temp, store, repo) = index_files(&[(
        "a/Shapes.java",
        "package a;\n\nclass Shapes {\n    void draw() {}\n    void draw(int size) {}\n    void run() { draw(); }\n}\n",
    )]);
    let report = trace(
        &store,
        repo,
        Some("Shapes::run"),
        None,
        TraceOptions::default(),
    )
    .expect("trace");
    assert!(!report.complete);
    assert_eq!(report.unfollowed.len(), 1);
    assert_eq!(report.unfollowed[0].status, "ambiguous");
    assert_eq!(report.unfollowed[0].candidates.len(), 2);
    let ambiguous = trace(
        &store,
        repo,
        None,
        Some("Shapes::draw"),
        TraceOptions::default(),
    )
    .expect("trace");
    assert!(
        matches!(ambiguous.result, TraceResult::Ambiguous { total: 2, .. }),
        "{:?}",
        ambiguous.result
    );
}

#[test]
fn test_java_and_csharp_module_cards_read_docs_public_types_and_entry_points() {
    let (temp, store, repo) = index_files(&JAVA);
    let root = fs::canonicalize(temp.path().join("repo")).expect("root canonicalizes");
    let cards = module_cards(
        &store,
        repo,
        &root,
        CardOptions {
            path: None,
            max_cards: 20,
            budget_tokens: 8_000,
        },
    )
    .expect("cards");
    let util = cards
        .cards
        .iter()
        .find(|card| card.path == "src/main/java/com/acme/util")
        .expect("util card");
    assert_eq!(
        util.purpose.as_deref(),
        Some("String helpers shared by the job store.")
    );
    let public = util
        .public_symbols
        .iter()
        .map(|symbol| symbol.name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        public,
        ["Strings"],
        "package-private and member names are not listed"
    );
    let store_card = cards
        .cards
        .iter()
        .find(|card| card.path == "src/main/java/com/acme/store")
        .expect("store card");
    assert!(
        store_card
            .entry_points
            .iter()
            .any(|entry| entry == "JobStore.java"),
        "{:?}",
        store_card.entry_points
    );
    let store_public = store_card
        .public_symbols
        .iter()
        .map(|symbol| symbol.name.as_str())
        .collect::<Vec<_>>();
    assert!(store_public.contains(&"JobStore") && store_public.contains(&"Keys"));
    assert!(!store_public.contains(&"BaseStore") && !store_public.contains(&"Hidden"));

    let (temp, store, repo) = index_files(&CSHARP);
    let root = fs::canonicalize(temp.path().join("repo")).expect("root canonicalizes");
    let cards = module_cards(
        &store,
        repo,
        &root,
        CardOptions {
            path: Some("lib/Util"),
            max_cards: 4,
            budget_tokens: 4_000,
        },
    )
    .expect("cards");
    let util = cards
        .cards
        .iter()
        .find(|card| card.path == "lib/Util")
        .expect("util card");
    assert_eq!(
        util.purpose.as_deref(),
        Some("String helpers shared by the job store.")
    );
    let public = util
        .public_symbols
        .iter()
        .map(|symbol| symbol.name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(public, ["Strings"], "internal types are not listed");
}

#[test]
fn test_a_csharp_main_or_program_file_is_an_entry_point() {
    let (temp, store, repo) = index_files(&[
        ("app/Program.cs", "Console.WriteLine(\"hi\");\n"),
        (
            "tool/Runner.cs",
            "namespace Tool;\n\npublic static class Runner\n{\n    public static void Main(string[] args) { }\n}\n",
        ),
    ]);
    let root = fs::canonicalize(temp.path().join("repo")).expect("root canonicalizes");
    let cards = module_cards(
        &store,
        repo,
        &root,
        CardOptions {
            path: None,
            max_cards: 10,
            budget_tokens: 4_000,
        },
    )
    .expect("cards");
    let entries = cards
        .cards
        .iter()
        .flat_map(|card| card.entry_points.iter().map(String::as_str))
        .collect::<Vec<_>>();
    assert!(entries.contains(&"Program.cs"), "{entries:?}");
    assert!(entries.contains(&"Runner.cs"), "{entries:?}");
}

/// A file added to a package or namespace is reached by the files that
/// already import it, though they did not change.
#[test]
fn test_a_file_added_to_an_imported_package_or_namespace_gains_its_importers() {
    let (temp, store, repo) = index_files(&JAVA);
    let root = temp.path().join("repo");
    write_all(
        &root,
        &[(
            "src/main/java/com/acme/util/Dates.java",
            "package com.acme.util;\n\npublic class Dates {}\n",
        )],
    );
    reindex(&store, &temp);
    assert_eq!(
        importers_of(&store, repo, "src/main/java/com/acme/util/Dates.java"),
        ["src/main/java/com/acme/store/JobStore.java"]
    );

    let (temp, store, repo) = index_files(&CSHARP);
    let root = temp.path().join("repo");
    write_all(
        &root,
        &[(
            "other/Dates.cs",
            "namespace Acme.Util;\n\npublic class Dates { }\n",
        )],
    );
    reindex(&store, &temp);
    assert_eq!(
        importers_of(&store, repo, "other/Dates.cs"),
        ["src/Acme.Jobs/JobStore.cs"]
    );
}

/// Adding a file to a package changes no stored row of its importers: the
/// package record is expanded when read.
#[test]
fn test_a_file_added_to_a_package_reaches_its_wildcard_importers_without_reparsing_them() {
    let (temp, store, repo) = index_files(&[
        (
            "app/Main.java",
            "package app;\n\nimport lib.*;\nimport java.util.List;\n\nclass Main {}\n",
        ),
        (
            "app/Other.java",
            "package app;\n\nimport java.util.Map;\n\nclass Other {}\n",
        ),
        ("lib/One.java", "package lib;\n\npublic class One {}\n"),
    ]);
    write_all(
        &temp.path().join("repo"),
        &[("lib/Two.java", "package lib;\n\npublic class Two {}\n")],
    );
    let report = Indexer::new(&store, counter_for(TokenProvider::Heuristic))
        .index(&temp.path().join("repo"), |_| {})
        .expect("reindex");
    assert_eq!(
        report.files_indexed, 1,
        "only the new file is parsed (review S1)"
    );
    assert_eq!(
        importers_of(&store, repo, "lib/Two.java"),
        ["app/Main.java"]
    );
}

/// A namespace of any size is one record: the importer is reached from
/// every file of it, and calls into it resolve as imports.
#[test]
fn test_a_large_namespace_import_is_one_record_and_still_traced() {
    let mut files = vec![(
        "app/Main.cs".to_owned(),
        "using Big;\n\nnamespace App;\n\npublic class Main\n{\n    public void Run() { Tools.Make(); }\n}\n"
            .to_owned(),
    )];
    files.push((
        "big/Tools.cs".to_owned(),
        "namespace Big;\n\npublic static class Tools { public static void Make() { } }\n"
            .to_owned(),
    ));
    for index in 1..500 {
        files.push((
            format!("big/F{index}.cs"),
            format!("namespace Big;\n\npublic class F{index} {{ }}\n"),
        ));
    }
    let borrowed = files
        .iter()
        .map(|(path, source)| (path.as_str(), source.as_str()))
        .collect::<Vec<_>>();
    let (_temp, store, repo) = index_files(&borrowed);
    assert!(edges_from(&store, repo, "app/Main.cs").is_empty());
    assert_eq!(package_records_from(&store, repo, "app/Main.cs"), ["Big"]);
    assert_eq!(importers_of(&store, repo, "big/F499.cs"), ["app/Main.cs"]);
    assert_eq!(
        callees_of(&store, repo, "Main::Run"),
        [("Tools::Make".to_owned(), CallResolution::Import)]
    );
}

/// A changed file that moves into an imported namespace, with no file added
/// or removed, is reached by its importers; one that moves out loses them;
/// a namespace that disappears turns its imports external, and one that
/// appears turns an external import into a namespace record.
#[test]
fn test_a_changed_file_that_moves_package_re_resolves_its_importers() {
    let (temp, store, repo) = index_files(&CSHARP);
    let root = temp.path().join("repo");
    write_all(
        &root,
        &[(
            "lib/Logging/Logger.cs",
            "namespace Acme.Util\n{\n    public class Logger { }\n}\n",
        )],
    );
    reindex(&store, &temp);
    assert_eq!(
        importers_of(&store, repo, "lib/Logging/Logger.cs"),
        ["src/Acme.Jobs/JobStore.cs"]
    );
    write_all(
        &root,
        &[(
            "lib/Util/More.cs",
            "namespace Acme.Elsewhere\n{\n    internal class More { }\n}\n",
        )],
    );
    reindex(&store, &temp);
    assert!(importers_of(&store, repo, "lib/Util/More.cs").is_empty());

    // Every file leaves `Acme.Util`: the import becomes external, then a
    // namespace record again when a file declares it once more.
    for path in ["lib/Util/Strings.cs", "lib/Logging/Logger.cs"] {
        write_all(
            &root,
            &[(path, "namespace Acme.Gone\n{\n    public class X { }\n}\n")],
        );
    }
    reindex(&store, &temp);
    assert!(package_records_from(&store, repo, "src/Acme.Jobs/JobStore.cs").is_empty());
    write_all(
        &root,
        &[(
            "lib/Util/More.cs",
            "namespace Acme.Util\n{\n    internal class More { }\n}\n",
        )],
    );
    reindex(&store, &temp);
    assert_eq!(
        package_records_from(&store, repo, "src/Acme.Jobs/JobStore.cs"),
        ["Acme.Util"]
    );

    let (temp, store, repo) = index_files(&JAVA);
    let root = temp.path().join("repo");
    write_all(
        &root,
        &[(
            "src/main/java/com/acme/store/Keys.java",
            "package com.acme.util;\n\npublic class Keys {}\n",
        )],
    );
    reindex(&store, &temp);
    assert_eq!(
        importers_of(&store, repo, "src/main/java/com/acme/store/Keys.java"),
        ["src/main/java/com/acme/store/JobStore.java"],
        "now in com.acme.util, which JobStore imports with a wildcard"
    );
    // A type import that was not indexed resolves once its file appears.
    write_all(
        &root,
        &[(
            "src/main/java/com/acme/app/Uses.java",
            "package com.acme.app;\n\nimport com.acme.util.Later;\n\nclass Uses {}\n",
        )],
    );
    reindex(&store, &temp);
    assert!(edges_from(&store, repo, "src/main/java/com/acme/app/Uses.java").is_empty());
    write_all(
        &root,
        &[(
            "src/main/java/com/acme/util/Later.java",
            "package com.acme.util;\n\npublic class Later {}\n",
        )],
    );
    reindex(&store, &temp);
    assert_eq!(
        edges_from(&store, repo, "src/main/java/com/acme/app/Uses.java"),
        ["src/main/java/com/acme/util/Later.java"]
    );
}

#[test]
fn test_a_removed_file_leaves_its_importers_edges() {
    let (temp, store, repo) = index_files(&JAVA);
    fs::remove_file(
        temp.path()
            .join("repo/src/main/java/com/acme/util/Strings.java"),
    )
    .expect("remove");
    reindex(&store, &temp);
    assert!(edges_from(&store, repo, "src/main/java/com/acme/store/JobStore.java").is_empty());
    assert_eq!(
        package_records_from(&store, repo, "src/main/java/com/acme/store/JobStore.java"),
        ["com.acme.util.*"]
    );
}

/// A file the scanner excludes declares nothing: its package never draws
/// edges.
#[test]
fn test_an_excluded_java_file_declares_no_package() {
    let (_temp, store, repo) = index_files(&[
        (".gitignore", "generated/\n"),
        (
            "app/Main.java",
            "package app;\n\nimport gen.api.Client;\n\nclass Main {}\n",
        ),
        (
            "generated/Client.java",
            "package gen.api;\n\npublic class Client {}\n",
        ),
    ]);
    assert!(edges_from(&store, repo, "app/Main.java").is_empty());
    assert!(
        store
            .indexed_file(repo, "generated/Client.java")
            .expect("lookup")
            .is_none()
    );
}

#[test]
fn test_a_store_indexed_before_java_and_csharp_support_reindexes_their_files() {
    const OLD_VERSION: &str = "0.1.0+atlas-shell-text-v1+rust-imports-v2+go-v1+calls-v1";
    let temp = tempfile::tempdir().expect("temporary directory");
    let root = temp.path().join("repo");
    write_all(
        &root,
        &[
            (
                "a/Main.java",
                "package a;\n\nclass Main { void run() {} }\n",
            ),
            (
                "b/Main.cs",
                "namespace B;\n\nclass Main { void Run() { } }\n",
            ),
        ],
    );
    let store = Store::open(&temp.path().join("compresso.db")).expect("store opens");
    let counter = counter_for(TokenProvider::Heuristic);
    Indexer::new(&store, counter)
        .with_parser_version(OLD_VERSION)
        .index(&root, |_| {})
        .expect("old index");
    let repo = repo_of(&store, &root);
    // What a parser without Java and C# left behind: no language, no symbols.
    for path in ["a/Main.java", "b/Main.cs"] {
        let mut record = store
            .indexed_file(repo, path)
            .expect("lookup")
            .expect("file");
        record.language = None;
        store
            .replace_indexed_file(repo, &record, &[], &[])
            .expect("old rows");
    }
    Indexer::new(&store, counter)
        .index(&root, |_| {})
        .expect("new index");
    let version = store
        .latest_index_version(repo)
        .expect("version")
        .expect("stamped");
    assert!(
        version
            .parser_version
            .contains("+java-csharp-v1+go-v1+calls-v1"),
        "{version:?}"
    );
    for (path, language, method) in [
        ("a/Main.java", FileLanguage::Java, "Main::run"),
        ("b/Main.cs", FileLanguage::CSharp, "Main::Run"),
    ] {
        let file = store
            .indexed_file(repo, path)
            .expect("lookup")
            .expect("file");
        assert_eq!(file.language, Some(language));
        let symbols = store.symbols_in_file(repo, path).expect("symbols");
        assert!(
            symbols.iter().any(|symbol| symbol.qualified_name == method),
            "{symbols:?}"
        );
    }
}
