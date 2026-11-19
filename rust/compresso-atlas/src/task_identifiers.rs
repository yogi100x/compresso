//! Identifiers and numeric literals written verbatim in a task (D88).
//!
//! The context planner lowercases and splits the task into words, which
//! loses the one strong signal a task can carry: a name copied from the code.
//! These helpers read the raw task text instead.

use std::path::Path;

use compresso_store::{FileLanguage, RepoId, Store};

use crate::AtlasError;
use crate::queries::{SearchHit, search_hits};
use crate::scanner::is_secret_path;

/// A name defined by more symbols than this is too common to lead the pack.
const MAX_LEADING_DEFINITIONS: usize = 3;
/// Symbols taken from one file a task names by path.
const SYMBOLS_PER_NAMED_PATH: usize = 2;
/// Files one path mention may resolve to by suffix.
const FILES_PER_NAMED_PATH: usize = 2;

/// How strongly a symbol is named by the task: a definition of a verbatim
/// identifier leads, then a symbol of a named file.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum NamedRank {
    Path = 1,
    Definition = 2,
}

/// Symbols the task names verbatim, with how they are named, in task order.
///
/// An identifier resolves through the exact-name index, not full-text
/// ranking, so its definition is found however many other symbols mention it.
/// A name defined more than three times is common, not a pointer, and names
/// nothing. A path resolves to the indexed file with that exact path, or else
/// to the one or two whose path ends with it (more name nothing), and names
/// their first two symbols.
/// Secret paths, and text files when `exclude_text` is set, are never named.
pub(crate) fn named_symbols(
    store: &Store,
    repo: RepoId,
    task: &str,
    exclude_text: bool,
) -> Result<Vec<(SearchHit, NamedRank)>, AtlasError> {
    let mut named = Vec::new();
    let mut indexed_paths = None;
    for identifier in task_identifiers(task) {
        match identifier {
            TaskIdentifier::Symbol(name) => {
                let hits =
                    search_hits(store.symbols_named(repo, &name, MAX_LEADING_DEFINITIONS + 1)?)?
                        .into_iter()
                        .filter(|hit| !(exclude_text && hit.language == Some(FileLanguage::Text)))
                        .collect::<Vec<_>>();
                if hits.len() <= MAX_LEADING_DEFINITIONS {
                    named.extend(hits.into_iter().map(|hit| (hit, NamedRank::Definition)));
                }
            }
            TaskIdentifier::Path(mention) => {
                let paths = indexed_paths.get_or_insert_with(Vec::new);
                if paths.is_empty() {
                    *paths = store.indexed_files(repo)?;
                }
                let suffix = format!("/{mention}");
                let files = match paths.iter().find(|record| record.path == mention) {
                    Some(exact) => vec![exact],
                    None => paths
                        .iter()
                        .filter(|record| record.path.ends_with(&suffix))
                        .take(FILES_PER_NAMED_PATH + 1)
                        .collect(),
                };
                // A bare `mod.rs` that many files end with points at none.
                if files.len() > FILES_PER_NAMED_PATH {
                    continue;
                }
                for record in files {
                    if is_secret_path(Path::new(&record.path))
                        || (exclude_text && record.language == Some(FileLanguage::Text))
                    {
                        continue;
                    }
                    for symbol in store
                        .symbols_in_file(repo, &record.path)?
                        .into_iter()
                        .take(SYMBOLS_PER_NAMED_PATH)
                    {
                        named.push((
                            SearchHit {
                                path: record.path.clone(),
                                language: record.language,
                                symbol,
                                score: 0.0,
                            },
                            NamedRank::Path,
                        ));
                    }
                }
            }
        }
    }
    Ok(named)
}

/// Most identifiers taken from one task.
pub(crate) const MAX_TASK_IDENTIFIERS: usize = 6;
/// Most numeric literals taken from one task.
pub(crate) const MAX_TASK_NUMBERS: usize = 2;

/// How a task spells a code name.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum TaskIdentifier {
    /// `ContextPlan`, `plan_context`, `MAX_READ_FILE_BYTES`: the bare name,
    /// the last segment of a qualified name such as `QueryError::FileTooLarge`.
    Symbol(String),
    /// `src/queries.rs`, `queries.rs`: a repository path or file name.
    Path(String),
}

/// File extensions that make a dotted word a file name.
const PATH_EXTENSIONS: [&str; 15] = [
    "rs", "ts", "tsx", "js", "jsx", "mjs", "py", "go", "sh", "md", "toml", "json", "yaml", "yml",
    "sql",
];

/// Identifiers in task order, without repeats, at most
/// [`MAX_TASK_IDENTIFIERS`].
///
/// A symbol identifier is a word that is `CamelCase` (a capital after a
/// lowercase letter: `ContextPlan`, `GraphQL`, `HTTPServer` is not), contains
/// an inner `_` between letters or digits (`plan_context`, `MAX_BYTES`), or is
/// qualified with `::`. A path contains `/` or ends in a known source
/// extension. Plain words, all-capital words without `_`, and numbers are not
/// identifiers.
pub(crate) fn task_identifiers(task: &str) -> Vec<TaskIdentifier> {
    let mut identifiers = Vec::new();
    for raw in task.split(|character: char| {
        !(character.is_alphanumeric() || matches!(character, '_' | ':' | '/' | '.' | '-'))
    }) {
        if identifiers.len() == MAX_TASK_IDENTIFIERS {
            break;
        }
        if raw.contains("..") {
            continue;
        }
        let word = raw.trim_matches(|character: char| matches!(character, ':' | '.' | '-' | '/'));
        let Some(identifier) = classify(word) else {
            continue;
        };
        if !identifiers.contains(&identifier) {
            identifiers.push(identifier);
        }
    }
    identifiers
}

fn classify(word: &str) -> Option<TaskIdentifier> {
    if word.is_empty() || word.contains("..") {
        return None;
    }
    let is_path = word.contains('/')
        || word.rsplit_once('.').is_some_and(|(stem, extension)| {
            !stem.is_empty() && PATH_EXTENSIONS.contains(&extension)
        });
    if is_path {
        let valid = word
            .split('/')
            .all(|part| !part.is_empty() && part.chars().any(char::is_alphanumeric));
        return valid.then(|| TaskIdentifier::Path(word.to_owned()));
    }
    let name = word.rsplit("::").next().unwrap_or(word);
    let qualified = word.contains("::");
    let identifier_shaped = name.chars().count() >= 3
        && name.starts_with(|first: char| first.is_alphabetic() || first == '_')
        && name
            .chars()
            .all(|character| character.is_alphanumeric() || character == '_');
    if !identifier_shaped {
        return None;
    }
    let camel = name
        .chars()
        .zip(name.chars().skip(1))
        .any(|(previous, current)| previous.is_lowercase() && current.is_uppercase());
    let inner_underscore = name
        .trim_matches('_')
        .split('_')
        .filter(|part| !part.is_empty())
        .count()
        >= 2;
    (camel || inner_underscore || qualified).then(|| TaskIdentifier::Symbol(name.to_owned()))
}

/// Integer literals of at least two digits written in the task, in task
/// order, without repeats, at most [`MAX_TASK_NUMBERS`]. A number inside a
/// word (`E17`, `v2`, `o200k`) or a decimal (`0.25`) is not a literal.
pub(crate) fn task_numbers(task: &str) -> Vec<String> {
    let mut numbers = Vec::<String>::new();
    for word in task
        .split(|character: char| !(character.is_alphanumeric() || matches!(character, '_' | '.')))
    {
        if numbers.len() == MAX_TASK_NUMBERS {
            break;
        }
        let word = word.trim_end_matches('.');
        if word.len() >= 2
            && word.bytes().all(|byte| byte.is_ascii_digit())
            && !numbers.iter().any(|known| known == word)
        {
            numbers.push(word.to_owned());
        }
    }
    numbers
}

/// Whether `line` contains `number` as a whole numeric literal: not part of a
/// longer number (`1320`, `3200`, `320.5`, `0.320`) or of an identifier
/// (`x320`, `320px` is accepted as a unit suffix only after a digit run ends).
pub(crate) fn line_has_number(line: &str, number: &str) -> bool {
    let bytes = line.as_bytes();
    line.match_indices(number).any(|(start, _)| {
        let end = start + number.len();
        let before_ok = start == 0 || {
            let previous = bytes[start - 1];
            !(previous.is_ascii_alphanumeric() || previous == b'_' || previous == b'.')
        };
        let after_ok = end == bytes.len() || {
            let next = bytes[end];
            !(next.is_ascii_digit()
                || next == b'_'
                || (next == b'.' && bytes.get(end + 1).is_some_and(u8::is_ascii_digit)))
        };
        before_ok && after_ok
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn symbols(task: &str) -> Vec<String> {
        task_identifiers(task)
            .into_iter()
            .map(|identifier| match identifier {
                TaskIdentifier::Symbol(name) => name,
                TaskIdentifier::Path(path) => format!("path:{path}"),
            })
            .collect()
    }

    #[test]
    fn test_task_identifiers_keep_code_shaped_words_only() {
        assert_eq!(
            symbols(
                "Why does ContextPlan drop evidence when plan_context hits MAX_READ_FILE_BYTES \
                 in src/queries.rs? See QueryError::FileTooLarge and queries.rs."
            ),
            [
                "ContextPlan",
                "plan_context",
                "MAX_READ_FILE_BYTES",
                "path:src/queries.rs",
                "FileTooLarge",
                "path:queries.rs"
            ]
        );
        assert!(symbols("Where do we handle SIGTERM for a graceful shutdown?").is_empty());
        assert!(symbols("How is the retry backoff delay worked out, e.g. 0.25 or E17?").is_empty());
        assert!(symbols("../etc/passwd").is_empty());
        assert_eq!(
            symbols("a b c d e f g h One_a Two_b Three_c Four_d Five_e Six_f Seven_g").len(),
            6
        );
    }

    #[test]
    fn test_go_file_names_are_paths() {
        assert_eq!(
            symbols("Why does server.go retry when store/db.go fails?"),
            ["path:server.go", "path:store/db.go"]
        );
    }

    #[test]
    fn test_task_numbers_take_standalone_integers_of_two_or_more_digits() {
        assert_eq!(
            task_numbers("Why is 320 the cap, not 8 or 0.25 or E17, and when does 64 apply? 320"),
            ["320", "64"]
        );
    }

    #[test]
    fn test_line_has_number_respects_digit_boundaries() {
        assert!(line_has_number("if size > 320 * 1024 {", "320"));
        assert!(line_has_number("width: 320px;", "320"));
        assert!(line_has_number("[320]", "320"));
        assert!(!line_has_number("let total = 1320;", "320"));
        assert!(!line_has_number("let total = 3200;", "320"));
        assert!(!line_has_number("let x320 = 0.320;", "320"));
        assert!(!line_has_number("let ratio = 320.5;", "320"));
        assert!(line_has_number("end of sentence 320.", "320"));
    }
}
