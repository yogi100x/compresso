//! The language table (E17-37, D102): one entry per indexed language.
//!
//! Each entry names the file extensions that select the language, how a file
//! is parsed (a tree-sitter grammar plus its `queries/<lang>.scm` and symbol
//! processor, or a hand-written extractor), and the per-language rules other
//! modules apply: import resolution, the call-resolution family, the module
//! doc comment read for module cards, the visibility heuristic, entry-point
//! file names, and a smoke sample `compresso doctor` parses. Adding a language
//! is one entry here, a `FileLanguage` variant in the store, a query file, and
//! whatever hooks it cannot share with an existing language.

use compresso_store::FileLanguage;

use super::calls::CallSyntax;
use super::{Candidate, Extraction};

/// A hand-written declaration walk over a parsed tree: pushes symbols and
/// imports, and for each symbol the byte range of its node when its recorded
/// lines are the node's lines (call attribution, as for query extractors).
pub(crate) type Walk = for<'tree> fn(
    tree_sitter::Node<'tree>,
    &str,
    &mut Extraction,
    &mut Vec<Option<(usize, usize)>>,
);

/// The packages or namespaces a parsed file declares (Java, C#).
pub(crate) type Declarations = for<'tree> fn(tree_sitter::Node<'tree>, &str) -> Vec<String>;

/// How the files of one language are turned into symbols, imports, and calls.
#[derive(Clone, Copy)]
pub(crate) enum Extractor {
    /// A tree-sitter grammar with an extraction query.
    Query {
        grammar: fn() -> tree_sitter::Language,
        query: &'static str,
        calls: CallSyntax,
        process: for<'tree> fn(Candidate<'tree>, &str, &mut Extraction),
    },
    /// A tree-sitter grammar walked by hand: one iterative pass over the
    /// declaration levels with an owner stack (Java, C#), no query.
    Walk {
        grammar: fn() -> tree_sitter::Language,
        calls: CallSyntax,
        walk: Walk,
    },
    /// A hand-written extractor that cannot fail.
    Custom(fn(&str) -> Extraction),
}

/// How a language's import strings are resolved to repository files.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ImportStyle {
    /// No imports are extracted.
    None,
    /// `use` paths through the crate graph.
    Rust,
    /// Dotted modules relative to the importer or a package root.
    Python,
    /// Relative specifiers with script extensions and `index` files.
    Script,
    /// Package paths under a `go.mod` module path, to every non-test `.go`
    /// file of that package directory.
    Go,
    /// `a.b.C` to the files declaring package `a.b` whose file name is
    /// `C.java`; `a.b.*` to every file declaring package `a.b`.
    Java,
    /// `using N;` to every file declaring namespace `N`.
    CSharp,
}

/// The leading comment or docstring read as a file's purpose line.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ModuleDoc {
    None,
    /// `//!` lines or a `/*! */` block.
    Rust,
    /// A leading string literal.
    Python,
    /// A leading `/** */` block that is not a license header.
    Script,
    /// Leading `#` lines after the shebang.
    Shell,
    /// The comment block directly above the `package` clause.
    Go,
    /// The Javadoc block above the `package` clause, or else above the first
    /// type declaration.
    Java,
    /// The `///` XML documentation above the first type declaration.
    CSharp,
}

/// The signature heuristic that marks a top-level symbol public; the store
/// keeps no visibility column.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Visibility {
    Never,
    /// The signature starts with `pub`.
    PubKeyword,
    /// The signature starts with `export`.
    Export,
    /// The name has no leading underscore.
    NoLeadingUnderscore,
    /// Every top-level symbol.
    TopLevel,
    /// The name starts with an uppercase letter (Go export rule).
    Capitalized,
    /// The signature's modifiers include `public` (Java, C#).
    PublicModifier,
}

/// Which rule set resolves a language's calls at query time (`trace`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CallRules {
    /// Candidates by name narrowed by call form, then the caller's file, an
    /// imported file, or the repository (D90).
    ByName,
    /// Go: a package is a directory (D102).
    GoPackages,
    /// Java and C#: types and methods live in declared packages or
    /// namespaces, and visibility modifiers decide what another file can
    /// reach (D105).
    DeclaredPackages(DeclaredRules),
}

/// Where Java and C# differ in what a call can reach (D105).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct DeclaredRules {
    /// A member without an access modifier is private (C#), not visible to
    /// its package (Java); a top-level type without one is `internal`.
    pub(crate) members_default_private: bool,
    /// Code sees the types of every enclosing namespace and of the global
    /// namespace (C#), not only of its own package (Java).
    pub(crate) enclosing_namespaces: bool,
    /// A type may be declared in parts across files (`partial`, C#).
    pub(crate) partial_types: bool,
}

/// How a line is recognised as comment or documentation text when grep
/// chooses which capped rows to list (E17-39, D104). Only line starts are
/// read: a line comment marker or a block opener counts only as the first
/// non-blank text of a line, so a marker inside a string or after code never
/// makes a row a comment. Anything not recognised is code.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CommentSyntax {
    /// Line comment markers (`//`, `#`).
    pub line: &'static [&'static str],
    /// A block comment's opener and closer (`/*`, `*/`).
    pub block: Option<(&'static str, &'static str)>,
}

impl CommentSyntax {
    /// No comment syntax: every line is code.
    pub const NONE: Self = Self {
        line: &[],
        block: None,
    };
    const C_LIKE: Self = Self {
        line: &["//"],
        block: Some(("/*", "*/")),
    };
    const HASH: Self = Self {
        line: &["#"],
        block: None,
    };
}

/// One indexed language.
pub struct LanguageSpec {
    pub language: FileLanguage,
    /// Display name, used in parser errors and diagnostics.
    pub name: &'static str,
    /// Lowercase file extensions without the dot, matched exactly.
    pub extensions: &'static [&'static str],
    /// A tiny source the grammar must parse, for `compresso doctor`.
    pub smoke_sample: &'static str,
    pub(crate) extractor: Extractor,
    pub(crate) imports: ImportStyle,
    /// Calls resolve only between languages of the same family; `None`
    /// records no calls.
    pub(crate) call_family: Option<u8>,
    pub(crate) module_doc: ModuleDoc,
    pub(crate) visibility: Visibility,
    /// File names that mark a directory's entry points in module cards.
    pub(crate) entry_files: &'static [&'static str],
    /// A file-name suffix the language reserves for test code
    /// (`_test.go`); such files are test code wherever they sit.
    pub test_file_suffix: Option<&'static str>,
    /// A package is a directory (Go): `pkg.F()` names the files of directory
    /// `pkg`, and a call reaches other files of its own directory without an
    /// import.
    pub(crate) package_is_directory: bool,
    /// The call-resolution rule set.
    pub(crate) call_rules: CallRules,
    /// A static method of this name marks its file as an entry point
    /// (`main` in Java, `Main` in C#).
    pub(crate) entry_method: Option<&'static str>,
    /// Reads the declared packages or namespaces from a parsed file, for the
    /// import map built before a run (Java, C#).
    pub(crate) declarations: Option<Declarations>,
    /// Comment syntax for grep row selection (E17-39, D104). Python
    /// docstrings, Markdown, and other text count as code.
    pub comments: CommentSyntax,
}

impl LanguageSpec {
    /// Whether this language is parsed by a tree-sitter grammar.
    pub const fn has_grammar(&self) -> bool {
        matches!(
            self.extractor,
            Extractor::Query { .. } | Extractor::Walk { .. }
        )
    }
}

fn rust_grammar() -> tree_sitter::Language {
    tree_sitter_rust::LANGUAGE.into()
}

fn typescript_grammar() -> tree_sitter::Language {
    tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()
}

fn tsx_grammar() -> tree_sitter::Language {
    tree_sitter_typescript::LANGUAGE_TSX.into()
}

fn javascript_grammar() -> tree_sitter::Language {
    tree_sitter_javascript::LANGUAGE.into()
}

fn python_grammar() -> tree_sitter::Language {
    tree_sitter_python::LANGUAGE.into()
}

fn go_grammar() -> tree_sitter::Language {
    tree_sitter_go::LANGUAGE.into()
}

fn java_grammar() -> tree_sitter::Language {
    tree_sitter_java::LANGUAGE.into()
}

fn csharp_grammar() -> tree_sitter::Language {
    tree_sitter_c_sharp::LANGUAGE.into()
}

/// Every indexed language. Extensions are unique across entries.
pub static LANGUAGES: &[LanguageSpec] = &[
    LanguageSpec {
        language: FileLanguage::Rust,
        name: "Rust",
        extensions: &["rs"],
        smoke_sample: "fn check() {}",
        extractor: Extractor::Query {
            grammar: rust_grammar,
            query: include_str!("../../queries/rust.scm"),
            calls: CallSyntax::Rust,
            process: super::process_rust_candidate,
        },
        imports: ImportStyle::Rust,
        call_family: Some(0),
        module_doc: ModuleDoc::Rust,
        visibility: Visibility::PubKeyword,
        entry_files: &["lib.rs", "main.rs", "mod.rs"],
        test_file_suffix: None,
        package_is_directory: false,
        call_rules: CallRules::ByName,
        entry_method: None,
        declarations: None,
        comments: CommentSyntax::C_LIKE,
    },
    LanguageSpec {
        language: FileLanguage::TypeScript,
        name: "TypeScript",
        extensions: &["ts"],
        smoke_sample: "export function check(): void {}",
        extractor: Extractor::Query {
            grammar: typescript_grammar,
            query: include_str!("../../queries/typescript.scm"),
            calls: CallSyntax::Script,
            process: super::process_typescript_candidate,
        },
        imports: ImportStyle::Script,
        call_family: Some(2),
        module_doc: ModuleDoc::Script,
        visibility: Visibility::Export,
        entry_files: &["index.ts", "main.ts"],
        test_file_suffix: None,
        package_is_directory: false,
        call_rules: CallRules::ByName,
        entry_method: None,
        declarations: None,
        comments: CommentSyntax::C_LIKE,
    },
    LanguageSpec {
        language: FileLanguage::Tsx,
        name: "TSX",
        extensions: &["tsx"],
        smoke_sample: "export const Check = () => <div />;",
        extractor: Extractor::Query {
            grammar: tsx_grammar,
            query: include_str!("../../queries/tsx.scm"),
            calls: CallSyntax::Script,
            process: super::process_typescript_candidate,
        },
        imports: ImportStyle::Script,
        call_family: Some(2),
        module_doc: ModuleDoc::Script,
        visibility: Visibility::Export,
        entry_files: &["index.tsx"],
        test_file_suffix: None,
        package_is_directory: false,
        call_rules: CallRules::ByName,
        entry_method: None,
        declarations: None,
        comments: CommentSyntax::C_LIKE,
    },
    LanguageSpec {
        language: FileLanguage::JavaScript,
        name: "JavaScript",
        extensions: &["js", "jsx", "mjs", "cjs"],
        smoke_sample: "export function check() {}",
        extractor: Extractor::Query {
            grammar: javascript_grammar,
            query: include_str!("../../queries/javascript.scm"),
            calls: CallSyntax::Script,
            process: super::process_javascript_candidate,
        },
        imports: ImportStyle::Script,
        call_family: Some(2),
        module_doc: ModuleDoc::Script,
        visibility: Visibility::Export,
        entry_files: &["index.js", "index.mjs", "main.js"],
        test_file_suffix: None,
        package_is_directory: false,
        call_rules: CallRules::ByName,
        entry_method: None,
        declarations: None,
        comments: CommentSyntax::C_LIKE,
    },
    LanguageSpec {
        language: FileLanguage::Python,
        name: "Python",
        extensions: &["py"],
        smoke_sample: "def check():\n    pass\n",
        extractor: Extractor::Query {
            grammar: python_grammar,
            query: include_str!("../../queries/python.scm"),
            calls: CallSyntax::Python,
            process: super::process_python_candidate,
        },
        imports: ImportStyle::Python,
        call_family: Some(1),
        module_doc: ModuleDoc::Python,
        visibility: Visibility::NoLeadingUnderscore,
        entry_files: &["__init__.py", "__main__.py", "main.py"],
        test_file_suffix: None,
        package_is_directory: false,
        call_rules: CallRules::ByName,
        entry_method: None,
        declarations: None,
        comments: CommentSyntax::HASH,
    },
    LanguageSpec {
        language: FileLanguage::Go,
        name: "Go",
        extensions: &["go"],
        smoke_sample: "package check\n\nfunc Check() {}\n",
        extractor: Extractor::Query {
            grammar: go_grammar,
            query: include_str!("../../queries/go.scm"),
            calls: CallSyntax::Go,
            process: super::go::process_candidate,
        },
        imports: ImportStyle::Go,
        call_family: Some(3),
        module_doc: ModuleDoc::Go,
        visibility: Visibility::Capitalized,
        entry_files: &["main.go", "doc.go"],
        test_file_suffix: Some("_test.go"),
        package_is_directory: true,
        call_rules: CallRules::GoPackages,
        entry_method: None,
        declarations: None,
        comments: CommentSyntax::C_LIKE,
    },
    LanguageSpec {
        language: FileLanguage::Java,
        name: "Java",
        extensions: &["java"],
        smoke_sample: "package check;\n\nclass Check { void check() {} }\n",
        extractor: Extractor::Walk {
            grammar: java_grammar,
            calls: CallSyntax::Java,
            walk: super::java::walk,
        },
        imports: ImportStyle::Java,
        call_family: Some(4),
        module_doc: ModuleDoc::Java,
        visibility: Visibility::PublicModifier,
        entry_files: &["package-info.java"],
        test_file_suffix: None,
        package_is_directory: false,
        call_rules: CallRules::DeclaredPackages(DeclaredRules {
            members_default_private: false,
            enclosing_namespaces: false,
            partial_types: false,
        }),
        entry_method: Some("main"),
        declarations: Some(super::java::declarations),
        comments: CommentSyntax::C_LIKE,
    },
    LanguageSpec {
        language: FileLanguage::CSharp,
        name: "C#",
        extensions: &["cs"],
        smoke_sample: "namespace Check;\n\nclass Check { void Run() {} }\n",
        extractor: Extractor::Walk {
            grammar: csharp_grammar,
            calls: CallSyntax::CSharp,
            walk: super::csharp::walk,
        },
        imports: ImportStyle::CSharp,
        call_family: Some(5),
        module_doc: ModuleDoc::CSharp,
        visibility: Visibility::PublicModifier,
        entry_files: &["Program.cs"],
        test_file_suffix: None,
        package_is_directory: false,
        call_rules: CallRules::DeclaredPackages(DeclaredRules {
            members_default_private: true,
            enclosing_namespaces: true,
            partial_types: true,
        }),
        entry_method: Some("Main"),
        declarations: Some(super::csharp::declarations),
        comments: CommentSyntax::C_LIKE,
    },
    LanguageSpec {
        language: FileLanguage::Shell,
        name: "Shell",
        extensions: &["sh", "bash", "zsh"],
        smoke_sample: "check() { :; }\n",
        extractor: Extractor::Custom(super::shell::extract),
        imports: ImportStyle::None,
        call_family: None,
        module_doc: ModuleDoc::Shell,
        visibility: Visibility::TopLevel,
        entry_files: &[],
        test_file_suffix: None,
        package_is_directory: false,
        call_rules: CallRules::ByName,
        entry_method: None,
        declarations: None,
        comments: CommentSyntax::HASH,
    },
    LanguageSpec {
        language: FileLanguage::Text,
        name: "Text",
        extensions: &["md", "markdown", "txt", "rst", "adoc"],
        smoke_sample: "# Check\n",
        extractor: Extractor::Custom(super::text::extract),
        imports: ImportStyle::None,
        call_family: None,
        module_doc: ModuleDoc::None,
        visibility: Visibility::Never,
        entry_files: &[],
        test_file_suffix: None,
        package_is_directory: false,
        call_rules: CallRules::ByName,
        entry_method: None,
        declarations: None,
        comments: CommentSyntax::NONE,
    },
];

/// The table entry of `language`.
pub fn spec(language: FileLanguage) -> &'static LanguageSpec {
    LANGUAGES
        .iter()
        .find(|spec| spec.language == language)
        .unwrap_or_else(|| unreachable!("every FileLanguage has a table entry"))
}

/// The language a file extension selects, matched exactly (case-sensitive,
/// as before the table existed).
pub fn for_extension(extension: &str) -> Option<&'static LanguageSpec> {
    LANGUAGES
        .iter()
        .find(|spec| spec.extensions.contains(&extension))
}

/// Whether a repository path is a file its language reserves for tests
/// (`store/db_test.go`), judged by the file name alone.
pub fn is_test_file(path: &str) -> bool {
    let file_name = path.rsplit('/').next().unwrap_or(path);
    LANGUAGES.iter().any(|spec| {
        spec.test_file_suffix
            .is_some_and(|suffix| file_name.len() > suffix.len() && file_name.ends_with(suffix))
    })
}

/// Whether the language of a repository path treats its directory as its
/// package.
pub(crate) fn package_is_directory(path: &str) -> bool {
    path.rsplit_once('.')
        .and_then(|(_, extension)| for_extension(extension))
        .is_some_and(|spec| spec.package_is_directory)
}

/// The call-resolution rule set of a repository path's language.
pub(crate) fn call_rules(path: &str) -> CallRules {
    path.rsplit_once('.')
        .and_then(|(_, extension)| for_extension(extension))
        .map_or(CallRules::ByName, |spec| spec.call_rules)
}

/// The languages whose files declare packages or namespaces (Java, C#).
pub(crate) fn declaring_languages() -> Vec<FileLanguage> {
    LANGUAGES
        .iter()
        .filter(|spec| spec.declarations.is_some())
        .map(|spec| spec.language)
        .collect()
}

/// Whether `language` declares packages or namespaces, stored as `module`
/// symbols.
pub(crate) fn declares_packages(language: Option<FileLanguage>) -> bool {
    language.is_some_and(|language| spec(language).declarations.is_some())
}

/// Whether `file_name` is an entry-point file name of any language.
pub(crate) fn is_entry_file(file_name: &str) -> bool {
    LANGUAGES
        .iter()
        .any(|spec| spec.entry_files.contains(&file_name))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Adding a `FileLanguage` variant fails to compile here until it is
    /// listed, and then fails this test until it has a table entry.
    const fn position(language: FileLanguage) -> usize {
        match language {
            FileLanguage::Rust => 0,
            FileLanguage::TypeScript => 1,
            FileLanguage::Tsx => 2,
            FileLanguage::JavaScript => 3,
            FileLanguage::Python => 4,
            FileLanguage::Go => 5,
            FileLanguage::Java => 6,
            FileLanguage::CSharp => 7,
            FileLanguage::Shell => 8,
            FileLanguage::Text => 9,
        }
    }

    #[test]
    fn test_every_file_language_has_exactly_one_table_entry() {
        let all = [
            FileLanguage::Rust,
            FileLanguage::TypeScript,
            FileLanguage::Tsx,
            FileLanguage::JavaScript,
            FileLanguage::Python,
            FileLanguage::Go,
            FileLanguage::Java,
            FileLanguage::CSharp,
            FileLanguage::Shell,
            FileLanguage::Text,
        ];
        assert_eq!(all.len(), LANGUAGES.len());
        for (index, language) in all.into_iter().enumerate() {
            assert_eq!(position(language), index);
            assert_eq!(
                LANGUAGES
                    .iter()
                    .filter(|spec| spec.language == language)
                    .count(),
                1,
                "{language:?}"
            );
            assert_eq!(spec(language).language, language);
        }
    }

    /// E17-39 (D104): every language states its comment syntax; text
    /// files have none, so a Markdown `# Heading` row is never a comment.
    #[test]
    fn test_each_language_states_its_comment_syntax() {
        for language in [
            FileLanguage::Rust,
            FileLanguage::TypeScript,
            FileLanguage::Tsx,
            FileLanguage::JavaScript,
            FileLanguage::Go,
            FileLanguage::Java,
            FileLanguage::CSharp,
        ] {
            assert_eq!(spec(language).comments.line, &["//"], "{language:?}");
            assert_eq!(spec(language).comments.block, Some(("/*", "*/")));
        }
        for language in [FileLanguage::Python, FileLanguage::Shell] {
            assert_eq!(spec(language).comments.line, &["#"], "{language:?}");
            assert_eq!(spec(language).comments.block, None);
        }
        assert_eq!(spec(FileLanguage::Text).comments, CommentSyntax::NONE);
    }

    #[test]
    fn test_only_go_reserves_a_test_file_name() {
        assert!(is_test_file("store/db_test.go"));
        assert!(is_test_file("db_test.go"));
        assert!(!is_test_file("_test.go"));
        assert!(!is_test_file("store/test_utils.go"));
        assert!(!is_test_file("src/db_test.rs"));
        assert!(!is_test_file("web/db_test.ts"));
        // E17-40 (D105): Java and C# reserve no file name for tests; their
        // tests are test code through D99's directory rule only.
        assert!(!is_test_file("src/test/java/a/StoreTest.java"));
        assert!(!is_test_file("Acme.Tests/StoreTests.cs"));
    }

    /// E17-40 (D105): Java and C# resolve calls by declared packages, never
    /// by Go's directory rule, and each is its own call family.
    #[test]
    fn test_java_and_csharp_use_declared_package_rules_and_their_own_families() {
        for (language, path) in [
            (FileLanguage::Java, "src/a/Store.java"),
            (FileLanguage::CSharp, "src/Store.cs"),
        ] {
            let spec = spec(language);
            assert!(!spec.package_is_directory, "{language:?}");
            assert!(!package_is_directory(path));
            assert!(matches!(call_rules(path), CallRules::DeclaredPackages(_)));
            assert!(spec.declarations.is_some());
            assert!(spec.has_grammar());
        }
        assert_eq!(call_rules("main.go"), CallRules::GoPackages);
        assert_eq!(call_rules("src/lib.rs"), CallRules::ByName);
        let families = LANGUAGES
            .iter()
            .filter_map(|spec| spec.call_family)
            .collect::<Vec<_>>();
        let java = spec(FileLanguage::Java)
            .call_family
            .expect("Java records calls");
        let csharp = spec(FileLanguage::CSharp)
            .call_family
            .expect("C# records calls");
        assert_eq!(families.iter().filter(|family| **family == java).count(), 1);
        assert_eq!(
            families.iter().filter(|family| **family == csharp).count(),
            1
        );
    }
}
