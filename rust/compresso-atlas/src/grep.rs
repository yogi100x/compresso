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
use compresso_store::{FileLanguage, RepoId, Store, SymbolRecord};
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

/// Compiled program ceiling: rejects patterns whose automaton would be large.
const REGEX_SIZE_LIMIT: usize = 1 << 20;
const REGEX_DFA_SIZE_LIMIT: usize = 4 << 20;
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
    /// Files missing, not UTF-8, or otherwise unreadable, not searched.
    pub files_skipped_unreadable: usize,
    /// Searched files whose content no longer matches the index.
    pub stale_files: usize,
    /// The file or byte cap stopped the scan: totals are lower bounds.
    pub scan_truncated: bool,
    /// A file or line cap, or a token budget, left matches unlisted.
    pub output_truncated: bool,
}

impl GrepReport {
    /// Whether every match in every in-scope file is listed.
    #[must_use]
    pub const fn complete(&self) -> bool {
        !self.scan_truncated && !self.output_truncated
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
        fitted.output_truncated |= cut;
        fitted
    }
}

fn render_file(file: &GrepFile) -> String {
    let mut text = format!("{} ({})\n", file.path, file.matches_total);
    for line in &file.lines {
        match &line.symbol {
            Some(symbol) => text.push_str(&format!(
                "  {} [{}{}] {}\n",
                line.line,
                if line.definition { "def " } else { "" },
                symbol,
                line.snippet
            )),
            None => text.push_str(&format!("  {} {}\n", line.line, line.snippet)),
        }
    }
    text
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
        regex::Error::CompiledTooBig(_) => "the compiled pattern is too large".to_owned(),
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
enum PathFilter {
    Prefix(String),
    /// Anchored regex over the whole path, or over the file name when
    /// `basename` is set.
    Glob {
        matcher: Regex,
        basename: bool,
    },
}

impl PathFilter {
    fn parse(filter: &str) -> Result<Option<Self>, GrepError> {
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
            .size_limit(REGEX_SIZE_LIMIT)
            .build()
            .map_err(|_| invalid("is not a valid glob"))?;
        Ok(Some(Self::Glob { matcher, basename }))
    }

    fn matches(&self, path: &str) -> bool {
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
    )
}

/// A file's matches for one pattern before ranking.
struct FileHits {
    record_index: usize,
    matches_total: usize,
    /// (line number, line start, line end, match start) of the first lines.
    kept: Vec<(u32, usize, usize, usize)>,
}

/// Searches the indexed files of `repo` for every pattern.
///
/// Files come from the index in path order. Secret paths are never read;
/// files above the 16 MiB read ceiling or that cannot be read as UTF-8 below
/// the repository root are skipped and counted. A file changed since
/// indexing is searched as it is now and counted as stale; its lines get no
/// enclosing symbol, because the stored outline describes other bytes. The
/// scan stops at [`GREP_SCAN_MAX_FILES`] files or [`GREP_SCAN_MAX_BYTES`]
/// bytes, never at a deadline, so a search is deterministic.
pub fn grep(
    store: &Store,
    repo: RepoId,
    options: &GrepOptions<'_>,
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
    };
    let mut per_pattern: Vec<Vec<FileHits>> = matchers.iter().map(|_| Vec::new()).collect();
    // Sources of files with listed lines, kept until snippets are built.
    let mut sources = Vec::<(usize, String, bool)>::new();
    for (record_index, record) in records.iter().enumerate() {
        if is_secret_path(Path::new(&record.path))
            || filter
                .as_ref()
                .is_some_and(|filter| !filter.matches(&record.path))
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
        report.bytes_scanned = report
            .bytes_scanned
            .saturating_add(u64::try_from(source.len()).unwrap_or(u64::MAX));
        let mut listed = false;
        for (matcher, hits) in matchers.iter().zip(&mut per_pattern) {
            let (matches_total, kept) =
                matching_lines(matcher, &source, options.max_matches_per_file);
            if matches_total > 0 {
                listed = true;
                hits.push(FileHits {
                    record_index,
                    matches_total,
                    kept,
                });
            }
        }
        let stale = u64::try_from(source.len()).ok() != Some(record.size_bytes)
            || blake3::hash(source.as_bytes()).to_hex().as_str() != record.hash;
        if stale {
            report.stale_files += 1;
        }
        if listed {
            sources.push((record_index, source, stale));
        }
    }

    let mut symbols_cache = Vec::<(usize, Vec<SymbolRecord>)>::new();
    for (pattern, hits) in options.patterns.iter().zip(per_pattern) {
        let matches_total = hits.iter().map(|hit| hit.matches_total).sum();
        let files_total = hits.len();
        let mut files = Vec::with_capacity(hits.len());
        for hit in hits {
            let record = &records[hit.record_index];
            let Some((_, source, stale)) = sources
                .iter()
                .find(|(index, _, _)| *index == hit.record_index)
            else {
                continue;
            };
            let symbols = if *stale {
                None
            } else {
                let position = match symbols_cache
                    .iter()
                    .position(|(index, _)| *index == hit.record_index)
                {
                    Some(position) => position,
                    None => {
                        symbols_cache
                            .push((hit.record_index, store.symbols_in_file(repo, &record.path)?));
                        symbols_cache.len() - 1
                    }
                };
                Some(&symbols_cache[position].1)
            };
            let lines = hit
                .kept
                .iter()
                .map(|&(line, start, end, match_start)| {
                    let enclosing =
                        symbols.and_then(|symbols| smallest_enclosing_symbol(symbols, line));
                    GrepLine {
                        line,
                        snippet: snippet(source, start, end, match_start),
                        symbol: enclosing.map(|symbol| symbol.qualified_name.clone()),
                        definition: enclosing.is_some_and(|symbol| symbol.start_line == line),
                    }
                })
                .collect::<Vec<_>>();
            files.push(GrepFile {
                path: record.path.clone(),
                language: record.language,
                matches_total: hit.matches_total,
                lines,
                stale: *stale,
            });
        }
        files.sort_by(|left, right| {
            let defines = |file: &GrepFile| file.lines.iter().any(|line| line.definition);
            defines(right)
                .cmp(&defines(left))
                .then_with(|| is_test_path(&left.path).cmp(&is_test_path(&right.path)))
                .then_with(|| right.matches_total.cmp(&left.matches_total))
                .then_with(|| left.path.cmp(&right.path))
        });
        if files.len() > options.max_files {
            files.truncate(options.max_files);
            report.output_truncated = true;
        }
        if files
            .iter()
            .any(|file| file.lines.len() < file.matches_total)
        {
            report.output_truncated = true;
        }
        report.results.push(GrepPatternResult {
            pattern: pattern.clone(),
            matches_total,
            files_total,
            files,
        });
    }
    Ok(report)
}

/// Counts the lines of `source` that `matcher` matches and keeps the first
/// `keep` of them as (line number, line start, line end, match start).
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

#[cfg(test)]
mod tests {
    use super::*;

    fn literal(pattern: &str) -> Regex {
        Regex::new(&regex::escape(pattern)).expect("pattern compiles")
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
