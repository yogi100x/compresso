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
}

impl LanguageSpec {
    /// Whether this language is parsed by a tree-sitter grammar.
    pub const fn has_grammar(&self) -> bool {
        matches!(self.extractor, Extractor::Query { .. })
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
            FileLanguage::Shell => 6,
            FileLanguage::Text => 7,
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

    #[test]
    fn test_only_go_reserves_a_test_file_name() {
        assert!(is_test_file("store/db_test.go"));
        assert!(is_test_file("db_test.go"));
        assert!(!is_test_file("_test.go"));
        assert!(!is_test_file("store/test_utils.go"));
        assert!(!is_test_file("src/db_test.rs"));
        assert!(!is_test_file("web/db_test.ts"));
    }
}
