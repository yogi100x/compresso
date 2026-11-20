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
use compresso_store::{FileLanguage, FileRecord, RepoId, Store, SymbolKind, SymbolRecord};
use regex::{Regex, RegexBuilder};

use crate::AtlasError;
use crate::parsers::languages::CommentSyntax;
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
/// Matching lines per file that [`GrepOptions::select_rows`] chooses from
/// (E17-39, D104); later matches of the file are counted, never listed or
/// named.
pub const GREP_ROW_CANDIDATES: usize = 2_000;
/// Unlisted lines of a listed file named by line (E17-39, D104).
pub const GREP_UNLISTED_LINES_NAMED: usize = 8;

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
    /// Choose which lines of a file to list by usefulness when it has more
    /// than `max_matches_per_file` (E17-39, D104): definitions, then code,
    /// then comment or documentation lines, then lines in a `tests` module,
    /// spread across enclosing symbols, then line order; the rest are named
    /// in [`GrepFile::lines_unlisted`]. Off, the first lines in line order
    /// are listed, as before; files rank the same either way.
    pub select_rows: bool,
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
    /// Usefulness among the file's listed lines, 0 first (E17-39, D104):
    /// a budget cut keeps the lowest ranks. Line order without
    /// [`GrepOptions::select_rows`].
    pub rank: usize,
}

/// One file's matches for one pattern.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GrepFile {
    pub path: String,
    pub language: Option<FileLanguage>,
    /// Every matching line in the file, listed or not.
    pub matches_total: usize,
    /// The listed matching lines, in line order: the first ones, or with
    /// [`GrepOptions::select_rows`] the most useful ones.
    pub lines: Vec<GrepLine>,
    /// The file changed since it was indexed; lines are current, symbols absent.
    pub stale: bool,
    /// Matching lines not listed, the most useful first, at most
    /// [`GREP_UNLISTED_LINES_NAMED`] (E17-39, D104): lines a budget cut
    /// dropped, then, with [`GrepOptions::select_rows`], the next most
    /// useful lines past the cap. Only the text form prints them.
    pub lines_unlisted: Vec<u32>,
    /// A definition is among the file's first `max_matches_per_file`
    /// matching lines: the file ranks as defining a symbol (D87), whichever
    /// lines are listed.
    pub ranks_as_definition: bool,
    /// Text of rows a budget cut dropped before a listed row, with their
    /// context, in line order (E17-39, D104): the text form uses it so
    /// every listed row keeps the context the call asked for.
    pub context_spare: Vec<GrepContextLine>,
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
                // List the file with the most useful lines that fit, in
                // line order, name the lines left out first, then stop.
                let mut partial = GrepFile {
                    lines: Vec::new(),
                    ..file.clone()
                };
                let mut by_rank = file.lines.iter().collect::<Vec<_>>();
                by_rank.sort_by_key(|line| line.rank);
                let mut dropped = Vec::new();
                for line in by_rank {
                    if !dropped.is_empty() {
                        dropped.push(line.line);
                        continue;
                    }
                    let at = partial.lines.partition_point(|kept| kept.line < line.line);
                    partial.lines.insert(at, line.clone());
                    if used + counter.count(&render_file(&partial)) > budget {
                        partial.lines.remove(at);
                        dropped.push(line.line);
                    }
                }
                // A dropped row before a listed one leaves its text and
                // context behind for the listed rows' context.
                let last_listed = partial.lines.last().map_or(0, |line| line.line);
                for line in file
                    .lines
                    .iter()
                    .filter(|line| dropped.contains(&line.line) && line.line < last_listed)
                {
                    partial.context_spare.extend(line.before.iter().cloned());
                    partial.context_spare.push(GrepContextLine {
                        line: line.line,
                        text: line.snippet.clone(),
                    });
                    partial.context_spare.extend(line.after.iter().cloned());
                }
                partial.context_spare.sort_by_key(|line| line.line);
                partial.context_spare.dedup_by_key(|line| line.line);
                dropped.extend(file.lines_unlisted.iter().copied());
                dropped.truncate(GREP_UNLISTED_LINES_NAMED);
                partial.lines_unlisted = dropped;
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
            defines: file.ranks_as_definition,
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
        // Lines earlier patterns chose in this file (E17-39, D104).
        let mut shown = Vec::<u32>::new();
        for (((matcher, scan), scope), pattern) in matchers
            .iter()
            .zip(&mut scans)
            .zip(&scopes)
            .zip(options.patterns)
        {
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
            // With row selection, up to GREP_ROW_CANDIDATES lines are kept
            // to choose from (E17-39, D104).
            let keep = if options.select_rows {
                GREP_ROW_CANDIDATES.max(options.max_matches_per_file)
            } else {
                options.max_matches_per_file
            };
            let (matches_total, kept) = if options.regex {
                matching_lines_by_line(matcher, &source, keep)
            } else {
                matching_lines(matcher, &source, keep)
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
            let enclosing_of =
                |line: u32| symbols.and_then(|symbols| smallest_enclosing_symbol(symbols, line));
            let is_definition =
                |line: u32| enclosing_of(line).is_some_and(|symbol| symbol.start_line == line);
            // The file ranks by its first lines, as before row selection.
            let ranks_as_definition = kept
                .iter()
                .take(options.max_matches_per_file)
                .any(|&(line, ..)| is_definition(line));
            let (kept, ranks, lines_unlisted) = if options.select_rows {
                let selected = select_rows(
                    &Selection {
                        source: &source,
                        language: record.language,
                        symbols: symbols.map(Vec::as_slice),
                        matcher,
                        names_tests: pattern.to_ascii_lowercase().contains("test"),
                        shown: &shown,
                    },
                    kept,
                    options.max_matches_per_file,
                );
                shown.extend(selected.0.iter().map(|&(line, ..)| line));
                selected
            } else {
                let ranks = (0..kept.len()).collect();
                (kept, ranks, Vec::new())
            };
            let contexts = merged_context(&source, &kept, options.context_lines);
            let lines = kept
                .iter()
                .zip(contexts)
                .zip(ranks)
                .map(
                    |((&(line, start, end, match_start), (before, after)), rank)| {
                        let enclosing = enclosing_of(line);
                        GrepLine {
                            line,
                            snippet: snippet(&source, start, end, match_start),
                            symbol: enclosing.map(|symbol| symbol.qualified_name.clone()),
                            definition: enclosing.is_some_and(|symbol| symbol.start_line == line),
                            before,
                            after,
                            rank,
                        }
                    },
                )
                .collect();
            scan.offer(
                GrepFile {
                    path: record.path.clone(),
                    language: record.language,
                    matches_total,
                    lines,
                    stale,
                    lines_unlisted,
                    ranks_as_definition,
                    context_spare: Vec::new(),
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

/// A kept match: `(line, line start, line end, match start)`.
type KeptMatch = (u32, usize, usize, usize);

/// How useful a capped row is (E17-39, D104), most useful first.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
enum RowClass {
    /// The first line of its enclosing indexed symbol.
    Definition,
    Code,
    /// A symbol's first line that matches only outside the symbol's name (a
    /// parameter, receiver, or type): a mention, after body code, so method
    /// signatures do not crowd out use sites.
    Signature,
    /// A line that starts as comment or documentation text.
    Comment,
    /// A line inside an indexed module named `tests`, which the text form
    /// names by line while the pattern lists code (E17-34, D99); not
    /// demoted for a pattern naming tests, which D99 lists.
    TestModule,
    /// A line an earlier pattern of the call chose in this file: the text
    /// form names it under `above` instead of listing it again.
    Repeat,
}

/// What [`select_rows`] reads about one file and pattern.
struct Selection<'a> {
    source: &'a str,
    language: Option<FileLanguage>,
    symbols: Option<&'a [SymbolRecord]>,
    matcher: &'a Regex,
    /// The pattern names tests (D99 lists its test rows).
    names_tests: bool,
    /// Lines earlier patterns of the call chose in this file.
    shown: &'a [u32],
}

/// Row selection (E17-39, D104): ranks `kept` (candidates in line order)
/// and returns the `keep` most useful in line order with each row's rank,
/// plus the next most useful lines, named instead of listed. Rows rank by
/// [`RowClass`]; within a class, rows are spread across enclosing symbols
/// (the first row of each symbol, then the second, ...), then by line. A
/// symbol's first line is a definition only when a match overlaps the
/// symbol's name there. No file is re-parsed: symbols come from the index
/// in one sorted pass and comments from [`comment_rows`].
fn select_rows(
    selection: &Selection<'_>,
    kept: Vec<KeptMatch>,
    keep: usize,
) -> (Vec<KeptMatch>, Vec<usize>, Vec<u32>) {
    let source = selection.source;
    let numbers = kept.iter().map(|&(line, ..)| line).collect::<Vec<_>>();
    let comments = selection.language.map_or_else(
        || vec![false; numbers.len()],
        |language| {
            comment_rows(
                source,
                &crate::parsers::languages::spec(language).comments,
                &numbers,
            )
        },
    );
    let symbols = selection.symbols.unwrap_or_default();
    // `tests` module ranges, found once per file, sorted and merged so each
    // candidate is one binary search.
    let mut ranges = symbols
        .iter()
        .filter(|symbol| symbol.kind == SymbolKind::Module && symbol.name == "tests")
        .map(|symbol| (symbol.start_line, symbol.end_line))
        .collect::<Vec<_>>();
    ranges.sort_unstable();
    let mut tests = Vec::<(u32, u32)>::new();
    for (start, end) in ranges {
        match tests.last_mut() {
            Some(last) if start <= last.1 => last.1 = last.1.max(end),
            _ => tests.push((start, end)),
        }
    }
    let enclosing = enclosing_symbols(symbols, &numbers);
    let candidates = kept
        .iter()
        .zip(&comments)
        .zip(&enclosing)
        .map(|((&(line, start, end, _), &comment), &enclosing)| {
            let in_tests = !selection.names_tests && in_tests_module(&tests, line);
            let class = if selection.shown.contains(&line) {
                RowClass::Repeat
            } else if in_tests {
                RowClass::TestModule
            } else if let Some(symbol) =
                enclosing.filter(|symbol| symbol.start_line == line && !comment)
            {
                if match_on_name(selection.matcher, &source[start..end], &symbol.name) {
                    RowClass::Definition
                } else {
                    RowClass::Signature
                }
            } else if comment {
                RowClass::Comment
            } else {
                RowClass::Code
            };
            (
                line,
                class,
                enclosing.map(|symbol| symbol.qualified_name.as_str()),
            )
        })
        .collect::<Vec<_>>();
    let order = rank_rows(&candidates);
    let mut rank_of = vec![usize::MAX; kept.len()];
    for (rank, &index) in order.iter().enumerate() {
        rank_of[index] = rank;
    }
    let unlisted = order
        .iter()
        .skip(keep)
        .take(GREP_UNLISTED_LINES_NAMED)
        .map(|&index| numbers[index])
        .collect();
    let (chosen, ranks) = kept
        .into_iter()
        .zip(rank_of)
        .filter(|&(_, rank)| rank < keep)
        .unzip();
    (chosen, ranks, unlisted)
}

/// Whether `line` is inside one of `ranges` (sorted, disjoint).
fn in_tests_module(ranges: &[(u32, u32)], line: u32) -> bool {
    let at = ranges.partition_point(|&(start, _)| start <= line);
    at > 0 && ranges[at - 1].1 >= line
}

/// Whether a match of `matcher` in `line` overlaps the first occurrence of
/// `name` there: the line defines that name rather than mentioning a
/// parameter, receiver, or type.
fn match_on_name(matcher: &Regex, line: &str, name: &str) -> bool {
    let line = line.trim_end_matches('\r');
    let Some(at) = (!name.is_empty()).then(|| line.find(name)).flatten() else {
        return false;
    };
    let end = at + name.len();
    matcher
        .find_iter(line)
        .any(|found| found.start() < end && at < found.end())
}

/// The smallest enclosing symbol of each of `lines` (ascending), as
/// [`smallest_enclosing_symbol`] finds it (the smallest span, the earliest
/// symbol on a tie), in one pass over the symbols sorted by first line:
/// each symbol enters and leaves an ordered set once, so the cost is
/// O((symbols + lines) log symbols) however the symbols overlap.
fn enclosing_symbols<'a>(
    symbols: &'a [SymbolRecord],
    lines: &[u32],
) -> Vec<Option<&'a SymbolRecord>> {
    use std::cmp::Reverse;
    use std::collections::{BTreeSet, BinaryHeap};
    let mut by_start = (0..symbols.len()).collect::<Vec<_>>();
    by_start.sort_by_key(|&index| symbols[index].start_line);
    let span = |index: usize| symbols[index].end_line - symbols[index].start_line;
    let mut next = 0;
    let mut active = BTreeSet::<(u32, usize)>::new();
    let mut ends = BinaryHeap::<Reverse<(u32, usize)>>::new();
    lines
        .iter()
        .map(|&line| {
            while let Some(&index) = by_start.get(next) {
                if symbols[index].start_line > line {
                    break;
                }
                // A symbol ending before it starts encloses no line.
                if symbols[index].end_line >= symbols[index].start_line {
                    active.insert((span(index), index));
                    ends.push(Reverse((symbols[index].end_line, index)));
                }
                next += 1;
            }
            while let Some(&Reverse((end, index))) = ends.peek() {
                if end >= line {
                    break;
                }
                ends.pop();
                active.remove(&(span(index), index));
            }
            active.first().map(|&(_, index)| &symbols[index])
        })
        .collect()
}

/// Indexes of `candidates` (`(line, class, enclosing symbol)`, in line
/// order), most useful first: by class, then by how many earlier rows of
/// the same class share the symbol, then by line.
fn rank_rows(candidates: &[(u32, RowClass, Option<&str>)]) -> Vec<usize> {
    let mut seen = std::collections::HashMap::<(RowClass, Option<&str>), usize>::new();
    let mut keys = candidates
        .iter()
        .enumerate()
        .map(|(index, &(line, class, symbol))| {
            let occurrence = seen.entry((class, symbol)).or_default();
            let key = (class, *occurrence, line, index);
            *occurrence += 1;
            key
        })
        .collect::<Vec<_>>();
    keys.sort_unstable();
    keys.into_iter().map(|(.., index)| index).collect()
}

/// Whether each of `lines` (ascending line numbers) starts as comment or
/// documentation text under `syntax` (E17-39, D104), in one pass over
/// `source` that stops at the last line asked for. Only line starts count:
/// a line comment marker or a block opener must be the line's first
/// non-blank text, so a marker in a string or after code never opens a
/// comment, and a block opened after code is not followed. A line inside a
/// block, or that opens one, is a comment unless code follows the block's
/// closer on that line. Anything else is code.
fn comment_rows(source: &str, syntax: &CommentSyntax, lines: &[u32]) -> Vec<bool> {
    let mut flags = Vec::with_capacity(lines.len());
    let Some(&last) = lines.last() else {
        return flags;
    };
    if syntax.line.is_empty() && syntax.block.is_none() {
        return vec![false; lines.len()];
    }
    let mut wanted = lines.iter().peekable();
    let mut in_block = false;
    let mut offset = 0_usize;
    let mut closers = CloserCursor::default();
    for (index, text) in source.split('\n').enumerate() {
        let line_start = offset;
        offset += text.len() + 1;
        let number = u32::try_from(index + 1).unwrap_or(u32::MAX);
        if number > last {
            break;
        }
        let trimmed = text.trim_start();
        // The text after a block's closer, when the block closes here.
        let closes = |rest: &str, close: &str| {
            rest.find(close)
                .map(|at| rest[at + close.len()..].trim().is_empty())
        };
        let comment = match syntax.block {
            Some((_, close)) if in_block => match closes(trimmed, close) {
                Some(only_comment) => {
                    in_block = false;
                    only_comment
                }
                None => true,
            },
            Some((open, close)) if trimmed.starts_with(open) => {
                match closes(&trimmed[open.len()..], close) {
                    Some(only_comment) => only_comment,
                    // A block counts only when its closer follows within
                    // BLOCK_COMMENT_LOOKAHEAD bytes; an opener inside a
                    // multi-line string usually has none, so it stays code.
                    None if closers.follows(source, line_start + text.len(), close) => {
                        in_block = true;
                        true
                    }
                    None => false,
                }
            }
            _ => syntax.line.iter().any(|marker| trimmed.starts_with(marker)),
        };
        while wanted.next_if_eq(&&number).is_some() {
            flags.push(comment);
        }
    }
    flags.resize(lines.len(), false);
    flags
}

/// Bytes after a block comment's opening line searched for its closer.
const BLOCK_COMMENT_LOOKAHEAD: usize = 64 * 1024;

/// Finds block-comment closers for [`comment_rows`] with one forward
/// cursor, so a file is searched at most about once whatever its lines
/// hold (security review: a file of many unclosed openers must not cost
/// lines times the lookahead). Queries come in increasing `from` order.
#[derive(Default)]
struct CloserCursor {
    /// The first closer at or after the last searched start, if found.
    next: Option<usize>,
    /// No closer starts before this offset at or after the last start.
    clear_until: usize,
}

impl CloserCursor {
    /// Whether `close` starts within [`BLOCK_COMMENT_LOOKAHEAD`] bytes of
    /// `from` (the whole closer inside that window).
    fn follows(&mut self, source: &str, from: usize, close: &str) -> bool {
        let bytes = source.as_bytes();
        let from = from.min(bytes.len());
        let window_end = (from + BLOCK_COMMENT_LOOKAHEAD).min(bytes.len());
        if let Some(at) = self.next.filter(|&at| at >= from) {
            return at + close.len() <= window_end;
        }
        // Resume where the last search ended: nothing before it matches.
        let start = from.max(self.clear_until);
        if close.is_empty() || start + close.len() > window_end {
            return false;
        }
        if let Some(offset) = find_bytes(&bytes[start..window_end], close.as_bytes()) {
            self.next = Some(start + offset);
            true
        } else {
            self.next = None;
            // The last `close.len() - 1` bytes may start a closer that runs
            // past this window, so the next search covers them again.
            self.clear_until = window_end + 1 - close.len();
            false
        }
    }
}

/// The first offset of `needle` (not empty) in `haystack`.
fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
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
    use super::*;

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
            select_rows: false,
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

    fn rows(source: &str, syntax: CommentSyntax) -> Vec<bool> {
        let count = u32::try_from(source.lines().count()).expect("small");
        comment_rows(source, &syntax, &(1..=count).collect::<Vec<_>>())
    }

    const C_LIKE: CommentSyntax = CommentSyntax {
        line: &["//"],
        block: Some(("/*", "*/")),
    };

    /// E17-39 (D104): a comment is recognised only from a line start; a
    /// dereference, a marker in a string, or a block opened after code is
    /// code, and a line with code after a block's closer is code.
    #[test]
    fn test_comment_rows_read_only_line_starts_and_err_toward_code() {
        let source = "//! module docs\n/**\n * version docs\n */\nlet glob = \"src/**/*.ts\";\n*total += 1;\nfoo(); /* trailing\nstill code */\n/* one */ code();\n/* one */\n  // indented\ncode(); // tail\n";
        assert_eq!(
            rows(source, C_LIKE),
            [
                true, true, true, true, false, false, false, false, false, true, true, false
            ]
        );
        let python = "# comment\n\"\"\"Docstring.\"\"\"\nx = 1  # tail\n";
        assert_eq!(
            rows(
                python,
                CommentSyntax {
                    line: &["#"],
                    block: None
                }
            ),
            [true, false, false]
        );
        assert_eq!(
            rows("# Heading\ntext\n", CommentSyntax::NONE),
            [false, false]
        );
        // Only the lines asked for are returned, in order.
        assert_eq!(
            comment_rows(source, &C_LIKE, &[3, 5, 99]),
            [true, false, false]
        );
        assert!(comment_rows(source, &C_LIKE, &[]).is_empty());
    }

    /// E17-39 (D104): definitions, then code, then comments, then rows in a
    /// `tests` module; within a class, one row per enclosing symbol per
    /// round, then line order.
    #[test]
    fn test_rank_rows_orders_by_class_then_spreads_across_symbols() {
        let candidates = [
            (1, RowClass::Comment, None),
            (2, RowClass::Comment, None),
            (5, RowClass::Code, Some("a")),
            (6, RowClass::Code, Some("a")),
            (7, RowClass::Code, Some("a")),
            (9, RowClass::Definition, Some("b")),
            (10, RowClass::Code, Some("b")),
            (12, RowClass::TestModule, Some("tests::t")),
            (13, RowClass::Comment, Some("b")),
        ];
        let lines = rank_rows(&candidates)
            .into_iter()
            .map(|index| candidates[index].0)
            .collect::<Vec<_>>();
        assert_eq!(lines, [9, 5, 10, 6, 7, 1, 13, 2, 12]);
    }

    fn line(number: u32, rank: usize) -> GrepLine {
        GrepLine {
            line: number,
            snippet: format!("row {number} with some text"),
            symbol: None,
            definition: false,
            before: Vec::new(),
            after: Vec::new(),
            rank,
        }
    }

    /// E17-39 (D104): a budget cut keeps a file's most useful rows, shown in
    /// line order, and names the rows it drops first.
    #[test]
    fn test_budget_cut_keeps_the_most_useful_rows_and_names_the_rest() {
        let file = GrepFile {
            path: "src/a.rs".to_owned(),
            language: None,
            matches_total: 6,
            lines: vec![line(1, 2), line(4, 0), line(8, 1)],
            stale: false,
            lines_unlisted: vec![2, 3, 5],
            ranks_as_definition: false,
            context_spare: Vec::new(),
        };
        let report = GrepReport {
            results: vec![GrepPatternResult {
                pattern: "row".to_owned(),
                matches_total: 6,
                files_total: 1,
                files: vec![file.clone()],
                files_unlisted: Vec::new(),
            }],
            files_scanned: 1,
            bytes_scanned: 1,
            files_in_scope: 1,
            files_skipped_oversize: 0,
            files_skipped_unreadable: 0,
            stale_files: 0,
            scan_truncated: false,
            output_truncated: true,
            paths_unmatched: Vec::new(),
        };
        let counter = compresso_core::counter_for(compresso_core::TokenProvider::OpenAiO200k);
        let header = counter.count("# row — 6 matching lines in 1 files\n");
        let two = GrepFile {
            lines: vec![line(4, 0), line(8, 1)],
            ..file
        };
        let budget = header + counter.count(&render_file(&two));
        let fitted = report.fit_to_budget(budget, counter);
        let kept = &fitted.results[0].files[0];
        assert_eq!(
            kept.lines.iter().map(|line| line.line).collect::<Vec<_>>(),
            [4, 8]
        );
        assert_eq!(kept.lines_unlisted, [1, 2, 3, 5]);
    }

    /// E17-39 review (item 7): a row a budget cut drops between two listed
    /// rows leaves its own text and context behind, so the listed rows keep
    /// the context the call asked for.
    #[test]
    fn test_budget_cut_keeps_a_dropped_middle_rows_text_for_context() {
        let context = |number: u32| GrepContextLine {
            line: number,
            text: format!("context {number}"),
        };
        let middle = GrepLine {
            before: vec![context(3)],
            after: vec![context(5)],
            ..line(4, 2)
        };
        let file = GrepFile {
            path: "src/a.rs".to_owned(),
            language: None,
            matches_total: 3,
            lines: vec![line(2, 0), middle, line(6, 1)],
            stale: false,
            lines_unlisted: Vec::new(),
            ranks_as_definition: false,
            context_spare: Vec::new(),
        };
        let report = GrepReport {
            results: vec![GrepPatternResult {
                pattern: "row".to_owned(),
                matches_total: 3,
                files_total: 1,
                files: vec![file.clone()],
                files_unlisted: Vec::new(),
            }],
            files_scanned: 1,
            bytes_scanned: 1,
            files_in_scope: 1,
            files_skipped_oversize: 0,
            files_skipped_unreadable: 0,
            stale_files: 0,
            scan_truncated: false,
            output_truncated: false,
            paths_unmatched: Vec::new(),
        };
        let counter = compresso_core::counter_for(compresso_core::TokenProvider::OpenAiO200k);
        let header = counter.count("# row — 3 matching lines in 1 files\n");
        let two = GrepFile {
            lines: vec![line(2, 0), line(6, 1)],
            ..file
        };
        let budget = header + counter.count(&render_file(&two));
        let fitted = report.fit_to_budget(budget, counter);
        let kept = &fitted.results[0].files[0];
        assert_eq!(
            kept.lines.iter().map(|line| line.line).collect::<Vec<_>>(),
            [2, 6]
        );
        assert_eq!(
            kept.context_spare
                .iter()
                .map(|line| line.line)
                .collect::<Vec<_>>(),
            [3, 4, 5]
        );
    }

    /// E17-39 security review: a file of many lines that each open a block
    /// comment and never close one is classified in time linear in its size,
    /// and those lines stay code (no closer follows).
    #[test]
    fn test_comment_rows_stay_linear_on_unclosed_block_openers() {
        let count = 400_000_u32;
        let source = "/*x\n".repeat(count as usize);
        let lines = (1..=count).step_by(997).chain([count]).collect::<Vec<_>>();
        let started = std::time::Instant::now();
        let flags = comment_rows(&source, &C_LIKE, &lines);
        let elapsed = started.elapsed();
        println!("{count} unclosed openers: {elapsed:?}");
        assert!(flags.iter().all(|comment| !comment));
        assert!(elapsed < std::time::Duration::from_secs(2), "{elapsed:?}");
        // A closer far ahead (past the lookahead) still leaves them code; a
        // near one makes the block a comment.
        let near = "/*x\n * y\n */\nz\n";
        assert_eq!(rows(near, C_LIKE), [true, true, true, false]);
    }

    /// E17-39 security review: the enclosing symbols of every candidate are
    /// those `smallest_enclosing_symbol` finds, in time that does not grow
    /// with symbols times candidates when many symbols overlap.
    #[test]
    fn test_enclosing_symbols_match_the_scan_and_stay_fast_when_overlapping() {
        let symbol = |name: &str, start_line: u32, end_line: u32| SymbolRecord {
            name: name.to_owned(),
            qualified_name: name.to_owned(),
            kind: SymbolKind::Function,
            signature: None,
            start_line,
            end_line,
            token_estimate: 1,
        };
        let mut symbols = vec![
            symbol("outer", 1, 100),
            symbol("inner", 10, 20),
            symbol("tie_a", 30, 40),
            symbol("tie_b", 30, 40),
            symbol("late", 90, 120),
        ];
        let lines = [1, 10, 15, 21, 30, 35, 95, 101, 121];
        let swept = enclosing_symbols(&symbols, &lines);
        for (line, found) in lines.iter().zip(swept) {
            assert_eq!(
                found.map(|symbol| symbol.name.as_str()),
                smallest_enclosing_symbol(&symbols, *line).map(|symbol| symbol.name.as_str()),
                "line {line}"
            );
        }
        symbols = (0..50_000)
            .map(|index| symbol(&format!("s{index}"), 1, 1_000_000 - index))
            .collect();
        let lines = (1..=2_000).collect::<Vec<u32>>();
        let started = std::time::Instant::now();
        let swept = enclosing_symbols(&symbols, &lines);
        let elapsed = started.elapsed();
        println!("50,000 overlapping symbols, 2,000 lines: {elapsed:?}");
        assert!(
            swept
                .iter()
                .all(|found| found.is_some_and(|symbol| symbol.name == "s49999"))
        );
        assert!(
            elapsed < std::time::Duration::from_millis(500),
            "{elapsed:?}"
        );
    }
}
