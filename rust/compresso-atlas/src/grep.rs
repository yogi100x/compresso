//! Bounded literal or regular-expression search over indexed file content
//! (D87).
//!
//! Only files the index lists are searched, in index path order, through the
//! same root resolution, secret-path exclusion, and 16 MiB per-file ceiling as
//! every other repository read. Matching uses a finite-automaton engine whose
//! time is linear in the input, so a pathological pattern or a very long line
//! cannot stall the scan. Nothing is executed and nothing is written.

use std::path::Path;

use compresso_core::TokenCounter;
use compresso_store::{FileLanguage, FileRecord, RepoId, Store, SymbolRecord};
use regex::{Regex, RegexBuilder};

use crate::AtlasError;
use crate::queries::{
    QueryError, ResolvedEntry, canonical_query_root, map_path_matches_focus, read_utf8,
    resolve_under_root, smallest_enclosing_symbol, validate_map_focus,
};
use crate::scanner::is_secret_path;

/// Most patterns one search accepts.
pub const GREP_MAX_PATTERNS: usize = 8;
/// Longest accepted pattern, in bytes.
pub const GREP_MAX_PATTERN_BYTES: usize = 512;
/// Longest accepted path filter, in bytes.
pub const GREP_MAX_PATH_FILTER_BYTES: usize = 256;
/// Upper bound for [`GrepOptions::max_files`].
pub const GREP_MAX_FILES_LIMIT: usize = 100;
/// Upper bound for [`GrepOptions::max_matches_per_file`].
pub const GREP_MAX_MATCHES_PER_FILE_LIMIT: usize = 50;
/// Longest snippet, in bytes, shown for one matching line.
pub const GREP_SNIPPET_MAX_BYTES: usize = 160;
/// Indexed files read by one search before it stops.
pub const GREP_SCAN_MAX_FILES: usize = 20_000;
/// Bytes read by one search before it stops.
pub const GREP_SCAN_MAX_BYTES: u64 = 256 * 1024 * 1024;
/// Bytes one regular-expression pattern searches before it stops. A
/// literal runs a substring search; a regular expression runs an automaton
/// whose per-byte cost grows with the pattern, so each one gets its own
/// budget and reaching it marks the scan truncated.
pub const GREP_REGEX_MAX_BYTES_PER_PATTERN: u64 = 64 * 1024 * 1024;
/// Upper bound for [`GrepOptions::context_lines`].
pub const GREP_MAX_CONTEXT_LINES: usize = 10;

/// Compiled program ceiling for a search pattern. Matching is linear-time
/// whatever the pattern, so this bounds memory: it applies to each compiled
/// program (the regex crate builds a forward and a reverse one), and the
/// lazy DFA caches are bounded separately by [`REGEX_DFA_SIZE_LIMIT`], so a
/// pattern holds at most about 8 MiB (2 x 2 MiB + 2 x 2 MiB), 64 MiB for 8.
/// Measured (E17-36, D101): 8 patterns at this limit searching 2.4 MB of
/// text added 22.7 MB of peak resident memory. Raised from 256 KiB, which
/// refused ordinary patterns such as `\D{0,30}` (about 1 MiB compiled).
const REGEX_SIZE_LIMIT: usize = 2 << 20;
/// Compiled program ceiling for a path glob (unchanged).
const GLOB_SIZE_LIMIT: usize = 256 * 1024;
const REGEX_DFA_SIZE_LIMIT: usize = 2 << 20;
const REGEX_NEST_LIMIT: u32 = 32;
/// Bytes of context kept before the match when a long line is cut.
const SNIPPET_LEADING_BYTES: usize = 48;
const ELLIPSIS: &str = "…";

/// Typed rejections of a content-search request.
#[derive(Debug, thiserror::Error)]
pub enum GrepError {
    #[error("at least one pattern is required")]
    NoPatterns,
    #[error("at most {max} patterns are accepted per search")]
    TooManyPatterns { max: usize },
    #[error("pattern {index} is empty")]
    EmptyPattern { index: usize },
    #[error("pattern {index} is longer than {max} bytes")]
    PatternTooLong { index: usize, max: usize },
    #[error("pattern {index} contains a NUL byte or a line break")]
    PatternControlCharacter { index: usize },
    #[error("pattern {index} is not a valid regular expression: {reason}")]
    InvalidPattern { index: usize, reason: String },
    #[error("path filter {filter:?} is invalid: {reason}")]
    InvalidPathFilter {
        filter: String,
        reason: &'static str,
    },
    #[error("{name} must be from 1 through {max}")]
    InvalidLimit { name: &'static str, max: usize },
    #[error("context_lines must be from 0 through {max}")]
    InvalidContext { max: usize },
    #[error("give one scope per pattern ({patterns} patterns, {scopes} scopes)")]
    ScopeCount { patterns: usize, scopes: usize },
}

/// One content search: patterns, matching mode, scope, and output caps.
#[derive(Clone, Copy, Debug)]
pub struct GrepOptions<'a> {
    pub patterns: &'a [String],
    /// Patterns are regular expressions instead of literal text.
    pub regex: bool,
    pub case_insensitive: bool,
    /// A repository-relative file or directory prefix, or a glob with `*`,
    /// `**`, and `?`. A glob without `/` is matched against file names.
    pub path: Option<&'a str>,
    /// Files listed per pattern.
    pub max_files: usize,
    /// Matching lines listed per file.
    pub max_matches_per_file: usize,
    /// Lines shown before and after each listed match (0 through
    /// [`GREP_MAX_CONTEXT_LINES`]); overlapping context is merged.
    pub context_lines: usize,
    /// Per-pattern scopes (E17-36, D101), parsed like `path`: empty when no
    /// pattern has one, else one entry per pattern. A pattern with a scope
    /// searches only the files in scope of both `path` and its own scope.
    pub scopes: &'a [Option<String>],
}

/// One line shown around a match, cut like a snippet.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GrepContextLine {
    pub line: u32,
    pub text: String,
}

/// One matching line.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GrepLine {
    pub line: u32,
    /// The line, cut around the match to at most [`GREP_SNIPPET_MAX_BYTES`]
    /// with `…` marking each cut, and leading indentation removed.
    pub snippet: String,
    /// Qualified name of the smallest indexed symbol containing the line,
    /// when the file still matches its index record.
    pub symbol: Option<String>,
    /// The line is the first line of that symbol: a definition, not a use.
    pub definition: bool,
    /// Context lines before the match not already shown with an earlier
    /// listed match of the same file, in line order.
    pub before: Vec<GrepContextLine>,
    /// Context lines after the match, stopping before the next listed match.
    pub after: Vec<GrepContextLine>,
}

/// One file's matches for one pattern.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GrepFile {
    pub path: String,
    pub language: Option<FileLanguage>,
    /// Every matching line in the file, listed or not.
    pub matches_total: usize,
    /// The first matching lines, in line order.
    pub lines: Vec<GrepLine>,
    /// The file changed since it was indexed; lines are current, symbols absent.
    pub stale: bool,
}

/// Every file one pattern matched, ranked.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GrepPatternResult {
    pub pattern: String,
    /// Matching lines across every scanned file.
    pub matches_total: usize,
    /// Files with at least one match.
    pub files_total: usize,
    /// Ranked: a file defining a symbol on a matching line first, then
    /// non-test before test files, then more matches, then path.
    pub files: Vec<GrepFile>,
    /// Matched files that are not listed, as `(path, matching lines)`: the
    /// [`GREP_UNLISTED_FILES_NAMED`] with the most matches (then by path)
    /// among those past `max_files` or dropped to fit a token budget
    /// (E17-38, D103).
    pub files_unlisted: Vec<(String, usize)>,
}

/// Unlisted files named per pattern (E17-38, D103).
pub const GREP_UNLISTED_FILES_NAMED: usize = 5;

/// Adds `entry` to the named unlisted files, keeping the most matches.
fn name_unlisted(named: &mut Vec<(String, usize)>, entry: (String, usize)) {
    if named.iter().any(|(path, _)| *path == entry.0) {
        return;
    }
    named.push(entry);
    named.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    named.truncate(GREP_UNLISTED_FILES_NAMED);
}

impl GrepPatternResult {
    /// Lines listed across the listed files.
    #[must_use]
    pub fn lines_listed(&self) -> usize {
        self.files.iter().map(|file| file.lines.len()).sum()
    }
}

/// The whole search: per-pattern results plus what the scan read or skipped.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GrepReport {
    pub results: Vec<GrepPatternResult>,
    /// Indexed files that passed the path filter and were read.
    pub files_scanned: usize,
    pub bytes_scanned: u64,
    /// Indexed files that passed the path filter.
    pub files_in_scope: usize,
    /// Files above the 16 MiB read ceiling, not searched.
    pub files_skipped_oversize: usize,
    /// Files missing, not UTF-8, refused by the read gate, or otherwise
    /// unreadable, not searched.
    pub files_skipped_unreadable: usize,
    /// Searched files whose content no longer matches the index.
    pub stale_files: usize,
    /// The file or byte cap, or a regular-expression pattern's byte budget,
    /// stopped the scan: totals are lower bounds.
    pub scan_truncated: bool,
    /// A file or line cap, or a token budget, left matches unlisted.
    pub output_truncated: bool,
    /// The call's path and each pattern's own path that match no indexed
    /// file (E17-38, D103), in call order, each once.
    pub paths_unmatched: Vec<UnmatchedPath>,
}

/// A path filter that matches no indexed file, with the nearest indexed
/// path when one is close: a file or directory of the same name, else the
/// deepest indexed directory sharing its leading directories.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnmatchedPath {
    /// The path as the caller wrote it.
    pub path: String,
    pub nearest: Option<String>,
}

impl GrepReport {
    /// Whether every match in every in-scope file is listed. A file skipped
    /// as oversize or unreadable may hold matches, so it makes the report
    /// incomplete.
    #[must_use]
    pub const fn complete(&self) -> bool {
        !self.scan_truncated
            && !self.output_truncated
            && self.files_skipped_oversize == 0
            && self.files_skipped_unreadable == 0
    }

    /// In-scope files that were not searched.
    #[must_use]
    pub const fn files_skipped(&self) -> usize {
        self.files_skipped_oversize + self.files_skipped_unreadable
    }

    /// Compact text: one header per pattern, one per file, one per line.
    #[must_use]
    pub fn render_text(&self) -> String {
        let mut text = String::new();
        for result in &self.results {
            text.push_str(&format!(
                "# {} — {} matching lines in {} files\n",
                result.pattern, result.matches_total, result.files_total
            ));
            for file in &result.files {
                text.push_str(&render_file(file));
            }
        }
        text
    }

    /// The report cut to fit `budget` tokens of [`GrepReport::render_text`].
    ///
    /// Files are admitted round-robin across patterns in rank order, so one
    /// broad pattern cannot crowd the others out. A file that does not fit
    /// whole is listed with as many of its lines as fit, or not at all.
    #[must_use]
    pub fn fit_to_budget(&self, budget: usize, counter: &dyn TokenCounter) -> Self {
        let mut fitted = self.clone();
        for result in &mut fitted.results {
            result.files.clear();
        }
        let mut used = fitted
            .results
            .iter()
            .map(|result| {
                counter.count(&format!(
                    "# {} — {} matching lines in {} files\n",
                    result.pattern, result.matches_total, result.files_total
                ))
            })
            .sum::<usize>();
        let mut cut = used > budget;
        let rounds = self
            .results
            .iter()
            .map(|result| result.files.len())
            .max()
            .unwrap_or(0);
        'rounds: for rank in 0..rounds {
            for (index, result) in self.results.iter().enumerate() {
                let Some(file) = result.files.get(rank) else {
                    continue;
                };
                if cut {
                    break 'rounds;
                }
                let cost = counter.count(&render_file(file));
                if used + cost <= budget {
                    used += cost;
                    fitted.results[index].files.push(file.clone());
                    continue;
                }
                // List the file with the lines that fit, then stop.
                let mut partial = GrepFile {
                    lines: Vec::new(),
                    ..file.clone()
                };
                for line in &file.lines {
                    partial.lines.push(line.clone());
                    if used + counter.count(&render_file(&partial)) > budget {
                        partial.lines.pop();
                        break;
                    }
                }
                if !partial.lines.is_empty() {
                    used += counter.count(&render_file(&partial));
                    fitted.results[index].files.push(partial);
                }
                cut = true;
            }
        }
        // Files the budget left out are named with the ones past max_files.
        for (fitted, result) in fitted.results.iter_mut().zip(&self.results) {
            for file in result.files.iter().skip(fitted.files.len()) {
                name_unlisted(
                    &mut fitted.files_unlisted,
                    (file.path.clone(), file.matches_total),
                );
            }
        }
        fitted.output_truncated |= cut;
        fitted
    }
}

fn render_file(file: &GrepFile) -> String {
    let mut text = format!("{} ({})\n", file.path, file.matches_total);
    for line in &file.lines {
        for row in line.rendered_rows() {
            text.push_str("  ");
            text.push_str(&row);
            text.push('\n');
        }
    }
    text
}

impl GrepLine {
    /// The match as `line [symbol] snippet` (`[def symbol]` for a
    /// definition), preceded and followed by its context lines as
    /// `line- text`.
    #[must_use]
    pub fn rendered_rows(&self) -> Vec<String> {
        let context = |line: &GrepContextLine| format!("{}- {}", line.line, line.text);
        let mut rows = self.before.iter().map(context).collect::<Vec<_>>();
        rows.push(match &self.symbol {
            Some(symbol) => format!(
                "{} [{}{}] {}",
                self.line,
                if self.definition { "def " } else { "" },
                symbol,
                self.snippet
            ),
            None => format!("{} {}", self.line, self.snippet),
        });
        rows.extend(self.after.iter().map(context));
        rows
    }
}

/// Rejects bad patterns before any file is read and compiles the rest.
fn compile_patterns(options: &GrepOptions<'_>) -> Result<Vec<Regex>, GrepError> {
    if options.patterns.is_empty() {
        return Err(GrepError::NoPatterns);
    }
    if options.patterns.len() > GREP_MAX_PATTERNS {
        return Err(GrepError::TooManyPatterns {
            max: GREP_MAX_PATTERNS,
        });
    }
    options
        .patterns
        .iter()
        .enumerate()
        .map(|(index, pattern)| {
            if pattern.is_empty() {
                return Err(GrepError::EmptyPattern { index });
            }
            if pattern.len() > GREP_MAX_PATTERN_BYTES {
                return Err(GrepError::PatternTooLong {
                    index,
                    max: GREP_MAX_PATTERN_BYTES,
                });
            }
            if pattern.contains(['\0', '\n', '\r']) {
                return Err(GrepError::PatternControlCharacter { index });
            }
            let source = if options.regex {
                pattern.clone()
            } else {
                regex::escape(pattern)
            };
            RegexBuilder::new(&source)
                .case_insensitive(options.case_insensitive)
                .multi_line(true)
                .size_limit(REGEX_SIZE_LIMIT)
                .dfa_size_limit(REGEX_DFA_SIZE_LIMIT)
                .nest_limit(REGEX_NEST_LIMIT)
                .build()
                .map_err(|error| GrepError::InvalidPattern {
                    index,
                    reason: compact_regex_error(&error),
                })
        })
        .collect()
}

/// The regex crate's multi-line diagnostics, flattened to one line.
fn compact_regex_error(error: &regex::Error) -> String {
    match error {
        regex::Error::CompiledTooBig(_) => "the compiled pattern is too large; shorten counted repetitions such as \\w{0,100}, or use ASCII classes such as [^0-9] or [A-Za-z0-9_]".to_owned(),
        other => other
            .to_string()
            .lines()
            .map(str::trim)
            .rfind(|line| !line.is_empty() && !line.chars().all(|c| c == '^' || c == ' '))
            .unwrap_or("syntax error")
            .to_owned(),
    }
}

/// A validated path scope.
pub(crate) enum PathFilter {
    Prefix(String),
    /// Anchored regex over the whole path, or over the file name when
    /// `basename` is set.
    Glob {
        matcher: Regex,
        basename: bool,
    },
}

impl PathFilter {
    pub(crate) fn parse(filter: &str) -> Result<Option<Self>, GrepError> {
        let invalid = |reason| GrepError::InvalidPathFilter {
            filter: filter.to_owned(),
            reason,
        };
        if filter.len() > GREP_MAX_PATH_FILTER_BYTES {
            return Err(invalid("longer than 256 bytes"));
        }
        if filter.contains('\0') || filter.contains('\\') || filter.starts_with('/') {
            return Err(invalid(
                "must be repository-relative, without NUL or backslash",
            ));
        }
        if filter.split('/').any(|part| part == "..") {
            return Err(invalid("must not contain '..'"));
        }
        if !filter.contains(['*', '?']) {
            return match validate_map_focus(filter) {
                Ok(Some(prefix)) => Ok(Some(Self::Prefix(prefix.to_owned()))),
                Ok(None) => Ok(None),
                Err(_) => Err(invalid("must be a lexical repository-relative path")),
            };
        }
        let basename = !filter.contains('/');
        let mut source = String::from("^");
        let mut characters = filter.chars().peekable();
        while let Some(character) = characters.next() {
            match character {
                '*' if characters.peek() == Some(&'*') => {
                    characters.next();
                    if characters.peek() == Some(&'/') {
                        characters.next();
                        source.push_str("(?:.*/)?");
                    } else {
                        source.push_str(".*");
                    }
                }
                '*' => source.push_str("[^/]*"),
                '?' => source.push_str("[^/]"),
                other => source.push_str(&regex::escape(other.encode_utf8(&mut [0; 4]))),
            }
        }
        source.push('$');
        let matcher = RegexBuilder::new(&source)
            .size_limit(GLOB_SIZE_LIMIT)
            .build()
            .map_err(|_| invalid("is not a valid glob"))?;
        Ok(Some(Self::Glob { matcher, basename }))
    }

    pub(crate) fn matches(&self, path: &str) -> bool {
        match self {
            Self::Prefix(prefix) => map_path_matches_focus(path, prefix),
            Self::Glob { matcher, basename } => {
                let subject = if *basename {
                    path.rsplit('/').next().unwrap_or(path)
                } else {
                    path
                };
                matcher.is_match(subject)
            }
        }
    }
}

fn validate_limit(name: &'static str, value: usize, max: usize) -> Result<(), GrepError> {
    if (1..=max).contains(&value) {
        Ok(())
    } else {
        Err(GrepError::InvalidLimit { name, max })
    }
}

/// Validates a search without reading anything: patterns, path filter, and
/// limits. [`grep`] runs the same checks.
pub fn validate_grep(options: &GrepOptions<'_>) -> Result<(), GrepError> {
    compile_patterns(options)?;
    if let Some(path) = options.path {
        PathFilter::parse(path)?;
    }
    validate_limit("max_files", options.max_files, GREP_MAX_FILES_LIMIT)?;
    validate_limit(
        "max_matches_per_file",
        options.max_matches_per_file,
        GREP_MAX_MATCHES_PER_FILE_LIMIT,
    )?;
    validate_context(options.context_lines)?;
    scope_filters(options).map(|_| ())
}

/// The parsed per-pattern scopes, one per pattern (`None` = unscoped).
fn scope_filters(options: &GrepOptions<'_>) -> Result<Vec<Option<PathFilter>>, GrepError> {
    if options.scopes.is_empty() {
        return Ok(options.patterns.iter().map(|_| None).collect());
    }
    if options.scopes.len() != options.patterns.len() {
        return Err(GrepError::ScopeCount {
            patterns: options.patterns.len(),
            scopes: options.scopes.len(),
        });
    }
    options
        .scopes
        .iter()
        .map(|scope| match scope {
            Some(scope) => PathFilter::parse(scope),
            None => Ok(None),
        })
        .collect()
}

/// The call's path and pattern paths that admit no indexed (non-secret)
/// file, each with its nearest indexed path.
fn unmatched_paths(
    options: &GrepOptions<'_>,
    filter: Option<&PathFilter>,
    scopes: &[Option<PathFilter>],
    records: &[FileRecord],
) -> Vec<UnmatchedPath> {
    let candidates = records
        .iter()
        .map(|record| record.path.as_str())
        .filter(|path| !is_secret_path(Path::new(path)))
        .collect::<Vec<_>>();
    let written = std::iter::once(options.path)
        .chain(options.scopes.iter().map(Option::as_deref))
        .zip(std::iter::once(filter).chain(scopes.iter().map(Option::as_ref)));
    let mut unmatched: Vec<UnmatchedPath> = Vec::new();
    for (text, parsed) in written {
        let (Some(text), Some(parsed)) = (text, parsed) else {
            continue;
        };
        if unmatched.iter().any(|entry| entry.path == text)
            || candidates.iter().any(|path| parsed.matches(path))
        {
            continue;
        }
        unmatched.push(UnmatchedPath {
            path: text.to_owned(),
            nearest: nearest_indexed(parsed, &candidates),
        });
    }
    unmatched
}

/// The indexed path nearest a prefix filter that matched nothing: the first
/// (by path) file or directory with the filter's last name, else the deepest
/// directory sharing its leading directories. Globs get none.
fn nearest_indexed(filter: &PathFilter, candidates: &[&str]) -> Option<String> {
    let PathFilter::Prefix(prefix) = filter else {
        return None;
    };
    let wanted = prefix.split('/').collect::<Vec<_>>();
    let name = *wanted.last()?;
    let mut same_name = candidates
        .iter()
        .filter_map(|path| {
            let segments = path.split('/').collect::<Vec<_>>();
            let position = segments.iter().position(|segment| *segment == name)?;
            Some(if position + 1 == segments.len() {
                (*path).to_owned()
            } else {
                format!("{}/", segments[..=position].join("/"))
            })
        })
        .collect::<Vec<_>>();
    same_name.sort();
    if let Some(found) = same_name.into_iter().next() {
        return Some(found);
    }
    candidates
        .iter()
        .map(|path| {
            let directories = path.split('/').collect::<Vec<_>>();
            let directories = &directories[..directories.len().saturating_sub(1)];
            directories
                .iter()
                .zip(&wanted)
                .take_while(|(have, want)| have == want)
                .count()
        })
        .max()
        .filter(|shared| *shared > 0)
        .map(|shared| format!("{}/", wanted[..shared].join("/")))
}

fn validate_context(context_lines: usize) -> Result<(), GrepError> {
    if context_lines <= GREP_MAX_CONTEXT_LINES {
        Ok(())
    } else {
        Err(GrepError::InvalidContext {
            max: GREP_MAX_CONTEXT_LINES,
        })
    }
}

/// Searches the indexed files of `repo` for every pattern.
///
/// Files come from the index in path order. Secret paths are never read;
/// files above the 16 MiB read ceiling or that cannot be read as UTF-8 below
/// the repository root are skipped and counted. A file changed since
/// indexing is searched as it is now and counted as stale; its lines get no
/// enclosing symbol, because the stored outline describes other bytes. The
/// scan stops at [`GREP_SCAN_MAX_FILES`] files or [`GREP_SCAN_MAX_BYTES`]
/// bytes, and a regular-expression pattern stops at
/// [`GREP_REGEX_MAX_BYTES_PER_PATTERN`], never at a deadline, so a search is
/// deterministic.
///
/// Every matching file is counted, but only the files that can still rank
/// within `max_files` keep snippets, context, and a symbol lookup, so memory
/// follows the listed answer rather than the number of matching files.
pub fn grep(
    store: &Store,
    repo: RepoId,
    options: &GrepOptions<'_>,
) -> Result<GrepReport, AtlasError> {
    grep_with_regex_budget(store, repo, options, GREP_REGEX_MAX_BYTES_PER_PATTERN)
}

/// Rank order of a matching file: a file defining a symbol on a listed line
/// first, then non-test before test files, then more matches, then path.
#[derive(Clone, Copy)]
struct RankKey<'a> {
    defines: bool,
    test: bool,
    matches: usize,
    path: &'a str,
}

impl RankKey<'_> {
    fn of(file: &GrepFile) -> RankKey<'_> {
        RankKey {
            defines: file.lines.iter().any(|line| line.definition),
            test: is_test_path(&file.path),
            matches: file.matches_total,
            path: &file.path,
        }
    }

    fn cmp(&self, other: &RankKey<'_>) -> std::cmp::Ordering {
        other
            .defines
            .cmp(&self.defines)
            .then_with(|| self.test.cmp(&other.test))
            .then_with(|| other.matches.cmp(&self.matches))
            .then_with(|| self.path.cmp(other.path))
    }
}

/// One pattern's running answer: totals over every file, and the best
/// `max_files` files kept in rank order.
#[derive(Default)]
struct PatternScan {
    matches_total: usize,
    files_total: usize,
    kept: Vec<GrepFile>,
    /// Matched files not kept, the most matches first (E17-38).
    unlisted: Vec<(String, usize)>,
    regex_bytes_left: u64,
    exhausted: bool,
}

impl PatternScan {
    /// Whether a file with these properties could enter the kept list even
    /// if it turned out to define a symbol on a listed line.
    fn could_rank(&self, max_files: usize, test: bool, matches: usize, path: &str) -> bool {
        self.kept.len() < max_files
            || self.kept.last().is_none_or(|worst| {
                RankKey {
                    defines: true,
                    test,
                    matches,
                    path,
                }
                .cmp(&RankKey::of(worst))
                .is_lt()
            })
    }

    /// Inserts `file` in rank order, keeping at most `max_files`.
    fn offer(&mut self, file: GrepFile, max_files: usize) {
        let key = RankKey::of(&file);
        let position = self
            .kept
            .partition_point(|kept| RankKey::of(kept).cmp(&key).is_lt());
        if position >= max_files {
            name_unlisted(&mut self.unlisted, (file.path, file.matches_total));
            return;
        }
        self.kept.insert(position, file);
        if self.kept.len() > max_files
            && let Some(evicted) = self.kept.pop()
        {
            name_unlisted(&mut self.unlisted, (evicted.path, evicted.matches_total));
        }
    }
}

fn grep_with_regex_budget(
    store: &Store,
    repo: RepoId,
    options: &GrepOptions<'_>,
    regex_bytes_per_pattern: u64,
) -> Result<GrepReport, AtlasError> {
    let matchers = compile_patterns(options)?;
    let filter = match options.path {
        Some(path) => PathFilter::parse(path)?,
        None => None,
    };
    validate_limit("max_files", options.max_files, GREP_MAX_FILES_LIMIT)?;
    validate_limit(
        "max_matches_per_file",
        options.max_matches_per_file,
        GREP_MAX_MATCHES_PER_FILE_LIMIT,
    )?;
    validate_context(options.context_lines)?;
    let scopes = scope_filters(options)?;
    // A file is in scope when the call's filter admits it and at least one
    // pattern may search it.
    let scoped_only = scopes.iter().all(Option::is_some);

    let repository = store.repository(repo)?;
    let root = canonical_query_root(&repository.root)?;
    let records = store.indexed_files(repo)?;
    let mut report = GrepReport {
        results: Vec::new(),
        files_scanned: 0,
        bytes_scanned: 0,
        files_in_scope: 0,
        files_skipped_oversize: 0,
        files_skipped_unreadable: 0,
        stale_files: 0,
        scan_truncated: false,
        output_truncated: false,
        paths_unmatched: Vec::new(),
    };
    report.paths_unmatched = unmatched_paths(options, filter.as_ref(), &scopes, &records);
    let mut scans = matchers
        .iter()
        .map(|_| PatternScan {
            regex_bytes_left: regex_bytes_per_pattern,
            ..PatternScan::default()
        })
        .collect::<Vec<_>>();
    for record in &records {
        if is_secret_path(Path::new(&record.path))
            || filter
                .as_ref()
                .is_some_and(|filter| !filter.matches(&record.path))
            || (scoped_only
                && !scopes
                    .iter()
                    .flatten()
                    .any(|scope| scope.matches(&record.path)))
        {
            continue;
        }
        report.files_in_scope += 1;
        if report.files_scanned == GREP_SCAN_MAX_FILES
            || report.bytes_scanned >= GREP_SCAN_MAX_BYTES
        {
            report.scan_truncated = true;
            continue;
        }
        let Ok(ResolvedEntry::File(resolved)) = resolve_under_root(&root, &record.path) else {
            report.files_skipped_unreadable += 1;
            continue;
        };
        let source = match read_utf8(&resolved) {
            Ok(source) => source,
            Err(QueryError::FileTooLarge { .. }) => {
                report.files_skipped_oversize += 1;
                continue;
            }
            Err(_) => {
                report.files_skipped_unreadable += 1;
                continue;
            }
        };
        report.files_scanned += 1;
        let source_bytes = u64::try_from(source.len()).unwrap_or(u64::MAX);
        report.bytes_scanned = report.bytes_scanned.saturating_add(source_bytes);
        let stale = source_bytes != record.size_bytes
            || blake3::hash(source.as_bytes()).to_hex().as_str() != record.hash;
        if stale {
            report.stale_files += 1;
        }
        let test = is_test_path(&record.path);
        // Snippets and symbols are built while the source is at hand, so a
        // search never holds more than one file's content.
        let mut symbols = None;
        for ((matcher, scan), scope) in matchers.iter().zip(&mut scans).zip(&scopes) {
            if scope
                .as_ref()
                .is_some_and(|scope| !scope.matches(&record.path))
            {
                continue;
            }
            if options.regex {
                if scan.exhausted || scan.regex_bytes_left < source_bytes {
                    scan.exhausted = true;
                    report.scan_truncated = true;
                    continue;
                }
                scan.regex_bytes_left -= source_bytes;
            }
            let (matches_total, kept) = if options.regex {
                matching_lines_by_line(matcher, &source, options.max_matches_per_file)
            } else {
                matching_lines(matcher, &source, options.max_matches_per_file)
            };
            if matches_total == 0 {
                continue;
            }
            scan.matches_total += matches_total;
            scan.files_total += 1;
            if !scan.could_rank(options.max_files, test, matches_total, &record.path) {
                name_unlisted(&mut scan.unlisted, (record.path.clone(), matches_total));
                continue;
            }
            let symbols: Option<&Vec<SymbolRecord>> = if stale {
                None
            } else {
                Some(match &symbols {
                    Some(symbols) => symbols,
                    None => symbols.insert(store.symbols_in_file(repo, &record.path)?),
                })
            };
            let contexts = merged_context(&source, &kept, options.context_lines);
            let lines = kept
                .iter()
                .zip(contexts)
                .map(|(&(line, start, end, match_start), (before, after))| {
                    let enclosing =
                        symbols.and_then(|symbols| smallest_enclosing_symbol(symbols, line));
                    GrepLine {
                        line,
                        snippet: snippet(&source, start, end, match_start),
                        symbol: enclosing.map(|symbol| symbol.qualified_name.clone()),
                        definition: enclosing.is_some_and(|symbol| symbol.start_line == line),
                        before,
                        after,
                    }
                })
                .collect();
            scan.offer(
                GrepFile {
                    path: record.path.clone(),
                    language: record.language,
                    matches_total,
                    lines,
                    stale,
                },
                options.max_files,
            );
        }
    }

    for (pattern, scan) in options.patterns.iter().zip(scans) {
        if scan.files_total > scan.kept.len() {
            report.output_truncated = true;
        }
        if scan
            .kept
            .iter()
            .any(|file| file.lines.len() < file.matches_total)
        {
            report.output_truncated = true;
        }
        report.results.push(GrepPatternResult {
            pattern: pattern.clone(),
            matches_total: scan.matches_total,
            files_total: scan.files_total,
            files: scan.kept,
            files_unlisted: scan.unlisted,
        });
    }
    Ok(report)
}

/// Context for each kept match `(line, line start, line end, match start)`,
/// in line order: up to `count` lines before and after, never repeating a
/// line an earlier match already showed and never running into the next
/// listed match, so the rendered file reads as merged blocks.
fn merged_context(
    source: &str,
    kept: &[(u32, usize, usize, usize)],
    count: usize,
) -> Vec<(Vec<GrepContextLine>, Vec<GrepContextLine>)> {
    if count == 0 {
        return kept.iter().map(|_| (Vec::new(), Vec::new())).collect();
    }
    let bytes = source.as_bytes();
    let line_text = |start: usize, end: usize, line: usize| GrepContextLine {
        line: u32::try_from(line).unwrap_or(u32::MAX),
        text: snippet(source, start, end, start),
    };
    let mut shown_through = 0_usize;
    let mut output = Vec::with_capacity(kept.len());
    for (index, &(line, start, end, _)) in kept.iter().enumerate() {
        let line = line as usize;
        let mut before = Vec::new();
        let mut cursor = start;
        let mut number = line;
        while before.len() < count && cursor > 0 && number - 1 > shown_through {
            // `cursor - 1` is the newline ending the previous line.
            let previous_end = cursor - 1;
            let previous_start = bytes[..previous_end]
                .iter()
                .rposition(|byte| *byte == b'\n')
                .map_or(0, |newline| newline + 1);
            number -= 1;
            before.push(line_text(previous_start, previous_end, number));
            cursor = previous_start;
        }
        before.reverse();
        let next_listed = kept
            .get(index + 1)
            .map_or(usize::MAX, |next| next.0 as usize);
        let mut after = Vec::new();
        let mut cursor = end;
        let mut number = line;
        while after.len() < count && cursor < bytes.len() && number + 1 < next_listed {
            let next_start = cursor + 1;
            if next_start >= bytes.len() {
                break;
            }
            let next_end = bytes[next_start..]
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(bytes.len(), |newline| next_start + newline);
            number += 1;
            after.push(line_text(next_start, next_end, number));
            cursor = next_end;
        }
        shown_through = number.max(line);
        output.push((before, after));
    }
    output
}

/// Counts the lines of `source` that a literal `matcher` matches and keeps
/// the first `keep` of them as (line number, line start, line end, match
/// start). A literal holds no line break, so no match spans two lines.
///
/// One pass: after a match the search resumes at the next line, newlines are
/// counted once between consecutive matches, and a line's start is the last
/// newline in the span already being counted. Every byte is examined a
/// bounded number of times however long a line is or however many matches
/// it holds, and the engine itself runs in linear time.
fn matching_lines(
    matcher: &Regex,
    source: &str,
    keep: usize,
) -> (usize, Vec<(u32, usize, usize, usize)>) {
    let bytes = source.as_bytes();
    let mut total = 0_usize;
    let mut kept = Vec::new();
    let mut position = 0_usize;
    let mut counted_to = 0_usize;
    let mut line_number = 1_usize;
    let mut line_start = 0_usize;
    while position <= bytes.len() {
        let Some(found) = matcher.find_at(source, position) else {
            break;
        };
        let start = found.start();
        // An empty match after the final newline is not on a line.
        if start == bytes.len() && (start == 0 || bytes[start - 1] == b'\n') {
            break;
        }
        for (offset, byte) in bytes[counted_to..start].iter().enumerate() {
            if *byte == b'\n' {
                line_number += 1;
                line_start = counted_to + offset + 1;
            }
        }
        counted_to = start;
        let end = bytes[start..]
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(bytes.len(), |newline| start + newline);
        total += 1;
        if kept.len() < keep {
            kept.push((
                u32::try_from(line_number).unwrap_or(u32::MAX),
                line_start,
                end,
                start,
            ));
        }
        if end == bytes.len() {
            break;
        }
        // The newline ending this line is counted when the next match is.
        position = end + 1;
    }
    (total, kept)
}

/// [`matching_lines`] for a regular expression, which may match across a
/// line break: each line, without its `\n` or a trailing `\r`, is searched
/// on its own, so every byte is searched once and a match never runs on into
/// a later line.
fn matching_lines_by_line(
    matcher: &Regex,
    source: &str,
    keep: usize,
) -> (usize, Vec<(u32, usize, usize, usize)>) {
    let bytes = source.as_bytes();
    let mut total = 0_usize;
    let mut kept = Vec::new();
    let mut line_start = 0_usize;
    let mut line_number = 0_usize;
    while line_start < bytes.len() {
        let end = bytes[line_start..]
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(bytes.len(), |newline| line_start + newline);
        line_number += 1;
        let content_end = if end > line_start && bytes[end - 1] == b'\r' {
            end - 1
        } else {
            end
        };
        if let Some(found) = matcher.find(&source[line_start..content_end]) {
            total += 1;
            if kept.len() < keep {
                kept.push((
                    u32::try_from(line_number).unwrap_or(u32::MAX),
                    line_start,
                    end,
                    line_start + found.start(),
                ));
            }
        }
        line_start = end + 1;
    }
    (total, kept)
}

/// The line `source[start..end]`, without indentation or a trailing `\r`,
/// cut to [`GREP_SNIPPET_MAX_BYTES`] around `match_start`.
fn snippet(source: &str, start: usize, end: usize, match_start: usize) -> String {
    let line = source[start..end].trim_end_matches('\r');
    let indent = line.len() - line.trim_start().len();
    let line_start = start + indent;
    let line_end = start + line.len();
    if line_end.saturating_sub(line_start) <= GREP_SNIPPET_MAX_BYTES {
        return source[line_start..line_end.max(line_start)].to_owned();
    }
    let mut from = match_start
        .saturating_sub(SNIPPET_LEADING_BYTES)
        .max(line_start);
    while !source.is_char_boundary(from) {
        from -= 1;
    }
    let mut to = (from + GREP_SNIPPET_MAX_BYTES).min(line_end);
    while !source.is_char_boundary(to) {
        to -= 1;
    }
    format!(
        "{}{}{}",
        if from > line_start { ELLIPSIS } else { "" },
        &source[from..to],
        if to < line_end { ELLIPSIS } else { "" }
    )
}

/// A path in a test directory or named like a test file.
fn is_test_path(path: &str) -> bool {
    let lowercase = path.to_ascii_lowercase();
    let in_test_directory = lowercase
        .split('/')
        .any(|part| matches!(part, "test" | "tests" | "__tests__" | "testdata"));
    let stem = lowercase
        .rsplit('/')
        .next()
        .and_then(|name| name.split('.').next())
        .unwrap_or_default();
    in_test_directory
        || stem.starts_with("test_")
        || stem.ends_with("_test")
        || lowercase.contains(".test.")
        || lowercase.contains(".spec.")
}

/// The literal parts of a literal (non-regex) pattern that holds an
/// unescaped `|` (E17-38, D103), trimmed, empty parts dropped; `None` when
/// it has no unescaped `|`, holds `||` (a code operator, not a choice), or
/// fewer than two parts remain, or a part is a single character (as in a
/// closure's `|x|`). A backslash before ASCII punctuation is dropped, so
/// `\\|` is a literal `|` and a regex-style `ping\\(` is `ping(`. Models
/// often write `a|b` meaning "a or b" without setting `regex`.
#[must_use]
pub fn literal_alternatives(pattern: &str) -> Option<Vec<String>> {
    if pattern.contains("||") {
        return None;
    }
    let mut parts = vec![String::new()];
    let mut split = false;
    let mut characters = pattern.chars().peekable();
    while let Some(character) = characters.next() {
        let part = parts.last_mut().expect("one part at least");
        if character == '\\'
            && let Some(&next) = characters.peek()
            && next.is_ascii_punctuation()
        {
            characters.next();
            part.push(next);
        } else if character == '|' {
            split = true;
            parts.push(String::new());
        } else {
            part.push(character);
        }
    }
    let parts = parts
        .into_iter()
        .map(|part| part.trim().to_owned())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    (split && parts.len() >= 2 && parts.iter().all(|part| part.chars().count() >= 2))
        .then_some(parts)
}

/// A regular expression that matches any of `parts` as literal text: each
/// part escaped, joined by `|`. No part adds regex syntax.
#[must_use]
pub fn literal_alternation_source(parts: &[String]) -> String {
    parts
        .iter()
        .map(|part| regex::escape(part))
        .collect::<Vec<_>>()
        .join("|")
}

#[cfg(test)]
mod tests {

    #[test]
    fn test_literal_alternatives_split_on_unescaped_pipes_only() {
        let parts = |pattern: &str| literal_alternatives(pattern);
        let owned = |parts: &[&str]| {
            Some(
                parts
                    .iter()
                    .map(|part| (*part).to_owned())
                    .collect::<Vec<_>>(),
            )
        };
        assert_eq!(parts("ab|cd"), owned(&["ab", "cd"]));
        assert_eq!(parts(" ab | cd "), owned(&["ab", "cd"]));
        assert_eq!(parts("xy\\|y|zz"), owned(&["xy|y", "zz"]));
        assert_eq!(
            parts("heartbeat|ping\\(|\\.terminate\\("),
            owned(&["heartbeat", "ping(", ".terminate("])
        );
        assert_eq!(parts("ab\\|cd"), None);
        assert_eq!(parts("ab|"), None);
        assert_eq!(parts("||"), None);
        assert_eq!(parts("a == nil || b"), None);
        assert_eq!(parts("|x| x + 1"), None);
        assert_eq!(parts("\\d+|\\w+"), owned(&["\\d+", "\\w+"]));
        assert_eq!(parts("plain"), None);
        let source = literal_alternation_source(&["a.b".to_owned(), "(c)".to_owned()]);
        assert_eq!(source, "a\\.b|\\(c\\)");
    }
    use super::*;

    fn literal(pattern: &str) -> Regex {
        Regex::new(&regex::escape(pattern)).expect("pattern compiles")
    }

    #[test]
    fn test_regex_byte_budget_stops_a_pattern_and_marks_the_scan_truncated() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let root = temp.path().join("repo");
        std::fs::create_dir_all(&root).expect("root");
        let store = Store::open(&temp.path().join("compresso.db")).expect("store");
        let repo = store.approve_repo(&root).expect("approve");
        let root = store.repository(repo).expect("repository").root;
        let source = "let token = 1;\n";
        for name in ["a.rs", "b.rs", "c.rs"] {
            std::fs::write(root.join(name), source).expect("file");
            store
                .replace_indexed_file(
                    repo,
                    &compresso_store::FileRecord {
                        path: name.to_owned(),
                        hash: blake3::hash(source.as_bytes()).to_hex().to_string(),
                        language: Some(FileLanguage::Rust),
                        size_bytes: source.len() as u64,
                        token_estimate: 4,
                        indexed_at: 10,
                    },
                    &[],
                    &[],
                )
                .expect("index");
        }
        let patterns = vec!["tok.n".to_owned()];
        let options = |regex| GrepOptions {
            patterns: &patterns,
            regex,
            case_insensitive: false,
            path: None,
            max_files: 10,
            max_matches_per_file: 5,
            context_lines: 0,
            scopes: &[],
        };
        let budget = 2 * source.len() as u64;
        let cut = grep_with_regex_budget(&store, repo, &options(true), budget).expect("grep");
        assert_eq!(cut.results[0].files_total, 2);
        assert!(cut.scan_truncated && !cut.complete());
        assert_eq!(
            cut.files_scanned, 3,
            "other patterns could still read the file"
        );

        let whole = grep_with_regex_budget(&store, repo, &options(true), 3 * budget).expect("grep");
        assert_eq!(whole.results[0].files_total, 3);
        assert!(whole.complete());

        // A literal search has no regex budget.
        let patterns_literal = vec!["token".to_owned()];
        let literal = GrepOptions {
            patterns: &patterns_literal,
            ..options(false)
        };
        let report = grep_with_regex_budget(&store, repo, &literal, 1).expect("grep");
        assert_eq!(report.results[0].files_total, 3);
        assert!(report.complete());
    }

    #[test]
    fn test_matching_lines_counts_each_line_once_and_numbers_lines() {
        let source = "alpha beta\nbeta beta\ngamma\nbeta";
        let (total, kept) = matching_lines(&literal("beta"), source, 2);
        assert_eq!(total, 3);
        assert_eq!(kept.iter().map(|entry| entry.0).collect::<Vec<_>>(), [1, 2]);
        let (total, kept) = matching_lines(&literal("beta"), source, 10);
        assert_eq!(total, 3);
        assert_eq!(kept[2].0, 4);
        assert_eq!(&source[kept[2].1..kept[2].2], "beta");
    }

    #[test]
    fn test_snippet_cuts_long_lines_around_the_match_on_char_boundaries() {
        let line = format!("{}needle{}", "é".repeat(200), "x".repeat(400));
        let start = line.find("needle").expect("needle present");
        let cut = snippet(&line, 0, line.len(), start);
        assert!(cut.contains("needle"));
        assert!(cut.starts_with(ELLIPSIS) && cut.ends_with(ELLIPSIS));
        assert!(cut.len() <= GREP_SNIPPET_MAX_BYTES + 2 * ELLIPSIS.len());
    }

    #[test]
    fn test_path_filter_prefix_and_globs() {
        let prefix = PathFilter::parse("src/auth").unwrap().unwrap();
        assert!(prefix.matches("src/auth/login.rs"));
        assert!(!prefix.matches("src/authz.rs"));
        let basename = PathFilter::parse("*.rs").unwrap().unwrap();
        assert!(basename.matches("deep/nested/file.rs"));
        assert!(!basename.matches("file.rsx"));
        let deep = PathFilter::parse("src/**/mod.rs").unwrap().unwrap();
        assert!(deep.matches("src/mod.rs"));
        assert!(deep.matches("src/a/b/mod.rs"));
        assert!(!deep.matches("lib/a/mod.rs"));
        assert!(PathFilter::parse("../etc").is_err());
        assert!(PathFilter::parse("/etc").is_err());
    }
}
