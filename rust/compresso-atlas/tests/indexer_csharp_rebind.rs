//! E17-44 (D109): a run in which a C# namespace appears or disappears
//! re-parses only the C# files whose `using` directives can bind to it, and
//! every incremental run still leaves the store identical to a fresh full
//! index of the same tree.

use std::fs;
use std::path::Path;
use std::sync::Mutex;

use compresso_atlas::{IndexProgress, Indexer};
use compresso_core::{TokenProvider, counter_for};
use compresso_store::{FileLanguage, RepoId, Store};
use tempfile::TempDir;

/// Files no scenario can affect: their own namespaces, their own library,
/// relative, static, alias, and global-free `using` directives of every
/// form. On main every one of them was re-parsed whenever any C# namespace
/// appeared or disappeared.
fn bystanders() -> Vec<(String, String)> {
    let mut files = vec![(
        "far/Lib/Tools.cs".to_owned(),
        "namespace Far.Lib\n{\n    public static class Tools { public static void Run() { } }\n}\n"
            .to_owned(),
    )];
    for index in 0..12 {
        files.push((
            format!("far/Away/N{index}/Thing{index}.cs"),
            format!(
                "using System;\nusing Far.Lib;\nusing static Far.Lib.Tools;\nusing T{index} = Far.Lib.Tools;\n\nnamespace Far.Away.N{index}\n{{\n    using Lib;\n\n    public class Thing{index} {{ void Go() {{ Tools.Run(); }} }}\n}}\n"
            ),
        ));
    }
    files
}

struct Scenario {
    name: &'static str,
    base: &'static [(&'static str, &'static str)],
    add: &'static [(&'static str, &'static str)],
    remove: &'static [&'static str],
}

const UTIL_ROOT: (&str, &str) = (
    "util/Strings.cs",
    "namespace Util\n{\n    public static class Strings { public static string Trim(string s) => s; }\n}\n",
);
const ACME_UTIL: (&str, &str) = (
    "acme/Util/Strings.cs",
    "namespace Acme.Util\n{\n    public static class Strings { public static string Trim(string s) => s; }\n}\n",
);
const PAGE_RELATIVE: (&str, &str) = (
    "web/Page.cs",
    "namespace Acme.Web\n{\n    using Util;\n    using static Util.Strings;\n    using S = Util.Strings;\n\n    public class Page { }\n}\n",
);

const SCENARIOS: &[Scenario] = &[
    // A namespace nested under the one a relative `using` sits in.
    Scenario {
        name: "new namespace nested under an existing one",
        base: &[UTIL_ROOT, PAGE_RELATIVE],
        add: &[(
            "web/Util/Strings.cs",
            "namespace Acme.Web.Util\n{\n    public static class Strings { }\n}\n",
        )],
        remove: &[],
    },
    // `Acme.Web.Util` beside `Acme.Web.Pages`: the last segment matches.
    Scenario {
        name: "sibling namespace with a matching last segment",
        base: &[
            ACME_UTIL,
            (
                "web/Pages/Page.cs",
                "namespace Acme.Web.Pages\n{\n    using Util;\n    using static Util.Strings;\n\n    public class Page { }\n}\n",
            ),
        ],
        add: &[(
            "web/Util/Strings.cs",
            "namespace Acme.Web.Util\n{\n    public static class Strings { }\n}\n",
        )],
        remove: &[],
    },
    // A namespace named like a relative `using` elsewhere, binding nothing.
    Scenario {
        name: "unrelated namespace with a matching last segment",
        base: &[ACME_UTIL, PAGE_RELATIVE],
        add: &[(
            "other/Util/Strings.cs",
            "namespace Other.Util\n{\n    public static class Strings { }\n}\n",
        )],
        remove: &[],
    },
    Scenario {
        name: "namespace removed",
        base: &[
            UTIL_ROOT,
            PAGE_RELATIVE,
            (
                "web/Util/Strings.cs",
                "namespace Acme.Web.Util\n{\n    public static class Strings { }\n}\n",
            ),
        ],
        add: &[],
        remove: &["web/Util/Strings.cs"],
    },
    // A record naming a namespace the repository declares only children
    // of; the child goes.
    Scenario {
        name: "children-only namespace removed",
        base: &[
            UTIL_ROOT,
            PAGE_RELATIVE,
            (
                "web/Util/Json/Writer.cs",
                "namespace Acme.Web.Util.Json\n{\n    public class Writer { }\n}\n",
            ),
        ],
        add: &[],
        remove: &["web/Util/Json/Writer.cs"],
    },
    Scenario {
        name: "children-only namespace added",
        base: &[UTIL_ROOT, PAGE_RELATIVE],
        add: &[(
            "web/Util/Json/Writer.cs",
            "namespace Acme.Web.Util.Json\n{\n    public class Writer { }\n}\n",
        )],
        remove: &[],
    },
    Scenario {
        name: "global using of a namespace removed",
        base: &[
            ACME_UTIL,
            (
                "app/GlobalUsings.cs",
                "global using Acme.Util;\nglobal using static Acme.Util.Strings;\n",
            ),
            (
                "app/App.csproj",
                "<Project Sdk=\"Microsoft.NET.Sdk\"></Project>\n",
            ),
        ],
        add: &[],
        remove: &["acme/Util/Strings.cs"],
    },
    Scenario {
        name: "global using static of a type whose namespace appears",
        base: &[(
            "app/GlobalUsings.cs",
            "global using static Acme.Util.Strings;\n",
        )],
        add: &[ACME_UTIL],
        remove: &[],
    },
    // `using static Acme.Util.Strings.Inner;` reaches `Strings.cs`; then a
    // namespace `Acme.Util.Strings` is declared.
    Scenario {
        name: "using static whose package prefix gets longer",
        base: &[
            ACME_UTIL,
            (
                "app/Main.cs",
                "using static Acme.Util.Strings.Inner;\n\nnamespace App\n{\n    public class Main { }\n}\n",
            ),
        ],
        add: &[(
            "acme/Util/Strings/Inner.cs",
            "namespace Acme.Util.Strings\n{\n    public class Inner { }\n}\n",
        )],
        remove: &[],
    },
    // `using static Util.T;` in `Acme.Web` bound to `Util`'s `T.cs`; a new
    // ancestor namespace `Acme` with a file named `Util.cs` takes it.
    Scenario {
        name: "new ancestor namespace",
        base: &[
            (
                "util/T.cs",
                "namespace Util\n{\n    public static class T { }\n}\n",
            ),
            (
                "web/Page.cs",
                "namespace Acme.Web\n{\n    using static Util.T;\n\n    public class Page { }\n}\n",
            ),
        ],
        add: &[(
            "acme/Util.cs",
            "namespace Acme\n{\n    public static class Util { }\n}\n",
        )],
        remove: &[],
    },
    // An alias written relative to its namespace.
    Scenario {
        name: "relative alias whose namespace appears",
        base: &[
            UTIL_ROOT,
            (
                "web/Page.cs",
                "namespace Acme.Web\n{\n    using S = Util.Strings;\n\n    public class Page { }\n}\n",
            ),
        ],
        add: &[(
            "web/Util/Strings.cs",
            "namespace Acme.Web.Util\n{\n    public static class Strings { }\n}\n",
        )],
        remove: &[],
    },
    // A file-scoped namespace holding the relative `using`.
    Scenario {
        name: "file-scoped namespace with a relative using",
        base: &[
            UTIL_ROOT,
            (
                "web/Page.cs",
                "namespace Acme.Web;\n\nusing Util;\n\npublic class Page { }\n",
            ),
        ],
        add: &[(
            "web/Util/Strings.cs",
            "namespace Acme.Web.Util;\n\npublic static class Strings { }\n",
        )],
        remove: &[],
    },
    // A type file joins a namespace that already existed and takes a
    // relative `using static` that resolved to nothing before.
    Scenario {
        name: "type file joins an existing nested namespace",
        base: &[
            (
                "util/Other.cs",
                "namespace Util\n{\n    public class Other { }\n}\n",
            ),
            (
                "web/Util/Other.cs",
                "namespace Acme.Web.Util\n{\n    public class Other { }\n}\n",
            ),
            (
                "web/Page.cs",
                "namespace Acme.Web\n{\n    using static Util.Strings;\n\n    public class Page { }\n}\n",
            ),
        ],
        add: &[(
            "web/Util/Strings.cs",
            "namespace Acme.Web.Util\n{\n    public static class Strings { }\n}\n",
        )],
        remove: &[],
    },
    // A file moves to another namespace.
    Scenario {
        name: "file moves to a new namespace",
        base: &[
            UTIL_ROOT,
            PAGE_RELATIVE,
            (
                "web/Util/Strings.cs",
                "namespace Acme.Web.Tools\n{\n    public static class Strings { }\n}\n",
            ),
        ],
        add: &[(
            "web/Util/Strings.cs",
            "namespace Acme.Web.Util\n{\n    public static class Strings { }\n}\n",
        )],
        remove: &[],
    },
];

fn write_all<'a>(root: &Path, files: impl IntoIterator<Item = (&'a str, &'a str)>) {
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

/// Indexes `root` and returns the repository with the paths the run parsed.
fn index(store: &Store, root: &Path) -> (RepoId, Vec<String>) {
    let parsed = Mutex::new(Vec::new());
    Indexer::new(store, counter_for(TokenProvider::Heuristic))
        .index(root, |event| {
            if let IndexProgress::Parsing { path } = event {
                parsed
                    .lock()
                    .expect("lock")
                    .push(path.to_string_lossy().replace('\\', "/"));
            }
        })
        .expect("index");
    (repo_of(store, root), parsed.into_inner().expect("lock"))
}

type Snapshot = (
    Vec<(String, String)>,
    Vec<(String, String, Option<String>)>,
    Vec<(String, String)>,
);

fn snapshot(store: &Store, repo: RepoId) -> Snapshot {
    let mut resolved = store
        .resolved_edges(repo)
        .expect("edges")
        .into_iter()
        .map(|edge| (edge.from_path, edge.to_path))
        .collect::<Vec<_>>();
    resolved.sort();
    let mut unresolved = store.unresolved_edge_targets(repo).expect("unresolved");
    unresolved.sort();
    let mut declarations = store
        .module_declarations(repo, &[FileLanguage::CSharp])
        .expect("declarations");
    declarations.sort();
    (resolved, unresolved, declarations)
}

#[test]
fn test_a_namespace_change_reparses_only_usings_that_can_bind_to_it() {
    let bystanders = bystanders();
    for scenario in SCENARIOS {
        let temp = tempfile::tempdir().expect("temporary directory");
        let root = temp.path().join("repo");
        let base = scenario
            .base
            .iter()
            .copied()
            .chain(bystanders.iter().map(|(p, s)| (p.as_str(), s.as_str())));
        write_all(&root, base);
        let store = Store::open(&temp.path().join("compresso.db")).expect("store opens");
        index(&store, &root);

        write_all(&root, scenario.add.iter().copied());
        for path in scenario.remove {
            fs::remove_file(root.join(path)).expect("remove");
        }
        let (repo, parsed) = index(&store, &root);

        let (_fresh_temp, fresh, fresh_repo) = fresh_index_of(&root);
        assert_eq!(
            snapshot(&store, repo),
            snapshot(&fresh, fresh_repo),
            "{}: the incremental run matches a full index",
            scenario.name
        );
        let reparsed_bystanders = parsed
            .iter()
            .filter(|path| bystanders.iter().any(|(bystander, _)| bystander == *path))
            .collect::<Vec<_>>();
        assert!(
            reparsed_bystanders.is_empty(),
            "{}: re-parsed files no change can affect: {reparsed_bystanders:?}",
            scenario.name
        );
    }
}

/// A fresh store over a copy of `root`'s current files.
fn fresh_index_of(root: &Path) -> (TempDir, Store, RepoId) {
    let temp = tempfile::tempdir().expect("temporary directory");
    let copy = temp.path().join("repo");
    copy_tree(root, &copy);
    let store = Store::open(&temp.path().join("compresso.db")).expect("store opens");
    let (repo, _) = index(&store, &copy);
    (temp, store, repo)
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).expect("directory");
    for entry in fs::read_dir(from).expect("read dir") {
        let entry = entry.expect("entry");
        let target = to.join(entry.file_name());
        if entry.file_type().expect("type").is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).expect("copy");
        }
    }
}
