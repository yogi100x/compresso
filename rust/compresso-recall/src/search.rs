//! Bounded, index-free search over local transcripts.

use std::cmp::Ordering;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::parse::{ParsedLine, parse_line};
use crate::redact::redact;
use crate::sources::{RecallSource, SourceSupport, discover};

/// Largest result count a caller may request.
pub const MAX_RESULTS: usize = 50;
const MAX_QUERY_TERMS: usize = 16;
const MAX_TERM_CHARS: usize = 64;
const SNIPPET_CHARS: usize = 320;
const SNIPPET_LEAD_CHARS: usize = 80;
/// Leading non-empty lines probed before a file is declared unrecognized.
const PROBE_LINES: usize = 5;
const DEADLINE_CHECK_INTERVAL: usize = 128;

/// Hard bounds applied to one recall call.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RecallLimits {
    /// Maximum hits returned, from 1 through [`MAX_RESULTS`].
    pub max_results: usize,
    /// Maximum newest transcript files read per source.
    pub max_files: usize,
    /// Maximum bytes read from one transcript file.
    pub max_file_bytes: u64,
    /// Lines longer than this are skipped without being parsed.
    pub max_line_bytes: usize,
    /// Wall-clock budget; the search stops and reports truncation when spent.
    pub deadline: Duration,
}

impl Default for RecallLimits {
    fn default() -> Self {
        Self {
            max_results: 8,
            max_files: 400,
            max_file_bytes: 32 * 1024 * 1024,
            max_line_bytes: 1024 * 1024,
            deadline: Duration::from_millis(1_500),
        }
    }
}

/// Which sessions a recall call may read.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RecallScope {
    /// Only sessions whose recorded working directory is inside this root.
    Repository(PathBuf),
    /// Every session of the enabled sources, regardless of working directory.
    AllRepositories,
}

/// One recall call. `sources` is the explicit per-source opt-in.
#[derive(Clone, Debug)]
pub struct RecallRequest<'a> {
    pub query: &'a str,
    pub sources: &'a [RecallSource],
    pub scope: RecallScope,
    pub home_dir: &'a Path,
    pub limits: RecallLimits,
}

/// Why a recall call was refused before reading anything.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum RecallError {
    #[error("recall is disabled until at least one session source is enabled")]
    NoSourcesEnabled,
    #[error("recall query must contain at least one search term")]
    EmptyQuery,
    #[error("recall result limit must be from 1 through {MAX_RESULTS}")]
    InvalidLimit,
}

/// One cited, redacted match.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecallHit {
    pub source: RecallSource,
    pub session_id: Option<String>,
    /// Transcript path, shown relative to the home directory when possible.
    pub transcript: String,
    /// One-based line number inside the transcript.
    pub line: usize,
    pub role: &'static str,
    pub timestamp: Option<String>,
    pub cwd: Option<String>,
    /// Redacted, whitespace-collapsed excerpt of at most 320 characters.
    pub snippet: String,
    pub matched_terms: usize,
    /// Secrets redacted in the matched event before it was searched.
    pub redactions: usize,
}

/// How one requested source was handled.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceStatus {
    Searched,
    /// The source has no transcript directory under the home directory.
    Missing,
    /// The source's transcript format is unverified, so nothing was read.
    Unsupported,
}

impl SourceStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Searched => "searched",
            Self::Missing => "missing",
            Self::Unsupported => "unsupported",
        }
    }
}

/// Per-source accounting for one recall call.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceReport {
    pub source: RecallSource,
    pub status: SourceStatus,
    pub files_scanned: usize,
    /// Files skipped because their leading lines matched no verified shape.
    pub files_unrecognized: usize,
    pub detail: Option<&'static str>,
}

/// Result of one recall call.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecallReport {
    pub hits: Vec<RecallHit>,
    pub sources: Vec<SourceReport>,
    /// True when a file, byte, or time bound stopped the search early.
    pub truncated: bool,
    /// Secrets redacted across the returned hits.
    pub redactions: usize,
}

/// Searches the enabled sources directly from disk and returns bounded hits.
pub fn recall(request: &RecallRequest<'_>) -> Result<RecallReport, RecallError> {
    if request.sources.is_empty() {
        return Err(RecallError::NoSourcesEnabled);
    }
    if !(1..=MAX_RESULTS).contains(&request.limits.max_results) {
        return Err(RecallError::InvalidLimit);
    }
    let query = Query::parse(request.query).ok_or(RecallError::EmptyQuery)?;
    let scope = Scope::new(&request.scope);
    let started = Instant::now();

    let mut sources = request.sources.to_vec();
    sources.sort_unstable();
    sources.dedup();

    let mut search = Search {
        query: &query,
        scope: &scope,
        home_dir: request.home_dir,
        limits: request.limits,
        started,
        hits: Vec::new(),
        truncated: false,
    };
    let mut reports = Vec::with_capacity(sources.len());
    for source in sources {
        reports.push(search.search_source(source));
    }

    let Search {
        mut hits,
        truncated,
        ..
    } = search;
    hits.sort_by(compare_hits);
    hits.truncate(request.limits.max_results);
    let redactions = hits.iter().map(|hit| hit.redactions).sum();
    Ok(RecallReport {
        hits,
        sources: reports,
        truncated,
        redactions,
    })
}

struct Query {
    terms: Vec<String>,
    phrase: Option<String>,
    /// Terms are plain enough that a raw-line byte search cannot miss them.
    raw_prefilter: bool,
}

impl Query {
    fn parse(query: &str) -> Option<Self> {
        let mut terms = Vec::new();
        for word in query.split_whitespace() {
            let term = word
                .to_lowercase()
                .chars()
                .take(MAX_TERM_CHARS)
                .collect::<String>();
            if !terms.contains(&term) {
                terms.push(term);
            }
            if terms.len() == MAX_QUERY_TERMS {
                break;
            }
        }
        if terms.is_empty() {
            return None;
        }
        let phrase = (terms.len() > 1).then(|| terms.join(" "));
        let raw_prefilter = terms.iter().all(|term| {
            term.bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
        });
        Some(Self {
            terms,
            phrase,
            raw_prefilter,
        })
    }

    fn may_match_raw(&self, line: &[u8]) -> bool {
        if !self.raw_prefilter {
            return true;
        }
        let lowered = line.to_ascii_lowercase();
        self.terms
            .iter()
            .any(|term| contains_bytes(&lowered, term.as_bytes()))
    }
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

struct Scope {
    roots: Option<Vec<PathBuf>>,
}

impl Scope {
    fn new(scope: &RecallScope) -> Self {
        let roots = match scope {
            RecallScope::AllRepositories => None,
            RecallScope::Repository(root) => {
                let mut roots = vec![root.clone()];
                if let Ok(canonical) = fs::canonicalize(root)
                    && canonical != *root
                {
                    roots.push(canonical);
                }
                Some(roots)
            }
        };
        Self { roots }
    }

    /// A session without a recorded working directory is never in a
    /// repository scope.
    fn admits(&self, cwd: Option<&str>) -> bool {
        match (&self.roots, cwd) {
            (None, _) => true,
            (Some(_), None) => false,
            (Some(roots), Some(cwd)) => roots.iter().any(|root| Path::new(cwd).starts_with(root)),
        }
    }
}

struct Search<'a> {
    query: &'a Query,
    scope: &'a Scope,
    home_dir: &'a Path,
    limits: RecallLimits,
    started: Instant,
    hits: Vec<RecallHit>,
    truncated: bool,
}

enum FileOutcome {
    Scanned,
    Unrecognized,
    Unreadable,
}

impl Search<'_> {
    fn out_of_time(&mut self) -> bool {
        let spent = self.started.elapsed() >= self.limits.deadline;
        if spent {
            self.truncated = true;
        }
        spent
    }

    fn search_source(&mut self, source: RecallSource) -> SourceReport {
        if let SourceSupport::Unsupported { reason } = source.support() {
            return SourceReport {
                source,
                status: SourceStatus::Unsupported,
                files_scanned: 0,
                files_unrecognized: 0,
                detail: Some(reason),
            };
        }
        let discovery = discover(source, self.home_dir, self.limits.max_files);
        let mut report = SourceReport {
            source,
            status: if discovery.root_exists {
                SourceStatus::Searched
            } else {
                SourceStatus::Missing
            },
            files_scanned: 0,
            files_unrecognized: 0,
            detail: None,
        };
        if discovery.capped {
            self.truncated = true;
        }
        for path in &discovery.files {
            if self.out_of_time() {
                break;
            }
            match self.search_file(source, path) {
                FileOutcome::Scanned => report.files_scanned += 1,
                FileOutcome::Unrecognized => report.files_unrecognized += 1,
                FileOutcome::Unreadable => {}
            }
        }
        report
    }

    fn search_file(&mut self, source: RecallSource, path: &Path) -> FileOutcome {
        // Transcripts are opened read-only and are never created or modified.
        let Ok(file) = File::open(path) else {
            return FileOutcome::Unreadable;
        };
        if file
            .metadata()
            .is_ok_and(|metadata| metadata.len() > self.limits.max_file_bytes)
        {
            self.truncated = true;
        }
        let mut reader = BufReader::new(file.take(self.limits.max_file_bytes));
        let transcript = display_path(self.home_dir, path);
        let mut cwd: Option<String> = None;
        let mut session_id: Option<String> = None;
        let mut buffer = Vec::new();
        let mut line_number = 0;
        let mut probed = 0;
        let mut recognized = 0;

        loop {
            buffer.clear();
            match reader.read_until(b'\n', &mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            line_number += 1;
            if line_number % DEADLINE_CHECK_INTERVAL == 0 && self.out_of_time() {
                break;
            }
            if buffer.len() > self.limits.max_line_bytes {
                continue;
            }
            let probing = probed < PROBE_LINES;
            if !probing
                && !self.query.may_match_raw(&buffer)
                && !contains_bytes(&buffer, b"\"session_meta\"")
            {
                continue;
            }
            let Ok(line) = std::str::from_utf8(&buffer) else {
                continue;
            };
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let parsed = parse_line(source, line);
            if probing {
                probed += 1;
            }
            let ParsedLine::Recognized { context, event } = parsed else {
                if probing && probed == PROBE_LINES && recognized == 0 {
                    return FileOutcome::Unrecognized;
                }
                continue;
            };
            recognized += 1;
            if context.cwd.is_some() {
                cwd = context.cwd;
            }
            if context.session_id.is_some() {
                session_id = context.session_id;
            }
            // A Codex session records its working directory once, so an
            // out-of-scope session needs no further reading.
            if source == RecallSource::Codex && cwd.is_some() && !self.scope.admits(cwd.as_deref())
            {
                break;
            }
            let Some(event) = event else {
                continue;
            };
            if !self.scope.admits(cwd.as_deref()) {
                continue;
            }
            let redaction = redact(&event.text);
            let normalized = redaction
                .text
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            let lowered = normalized.to_lowercase();
            let Some((matched_terms, first_match)) = self.query.score(&lowered) else {
                continue;
            };
            self.hits.push(RecallHit {
                source,
                session_id: session_id.clone(),
                transcript: transcript.clone(),
                line: line_number,
                role: event.role,
                timestamp: event.timestamp,
                cwd: cwd.clone(),
                snippet: snippet(&normalized, lowered[..first_match].chars().count()),
                matched_terms: matched_terms + usize::from(self.query.matches_phrase(&lowered)),
                redactions: redaction.count,
            });
            self.compact_hits();
        }
        if recognized == 0 && probed > 0 {
            FileOutcome::Unrecognized
        } else {
            FileOutcome::Scanned
        }
    }

    /// Keeps memory proportional to the result limit, not the history size.
    fn compact_hits(&mut self) {
        let ceiling = self.limits.max_results.saturating_mul(4).saturating_add(64);
        if self.hits.len() > ceiling {
            self.hits.sort_by(compare_hits);
            self.hits.truncate(self.limits.max_results);
        }
    }
}

impl Query {
    /// Returns the number of distinct matched terms and the first match offset.
    fn score(&self, lowered: &str) -> Option<(usize, usize)> {
        let mut matched = 0;
        let mut first = usize::MAX;
        for term in &self.terms {
            if let Some(offset) = lowered.find(term.as_str()) {
                matched += 1;
                first = first.min(offset);
            }
        }
        (matched > 0).then_some((matched, first))
    }

    fn matches_phrase(&self, lowered: &str) -> bool {
        self.phrase
            .as_deref()
            .is_some_and(|phrase| lowered.contains(phrase))
    }
}

fn compare_hits(left: &RecallHit, right: &RecallHit) -> Ordering {
    right
        .matched_terms
        .cmp(&left.matched_terms)
        .then_with(|| right.timestamp.cmp(&left.timestamp))
        .then_with(|| left.source.cmp(&right.source))
        .then_with(|| left.transcript.cmp(&right.transcript))
        .then_with(|| left.line.cmp(&right.line))
}

fn snippet(normalized: &str, match_char: usize) -> String {
    let total = normalized.chars().count();
    let start = match_char.saturating_sub(SNIPPET_LEAD_CHARS);
    let body = normalized
        .chars()
        .skip(start)
        .take(SNIPPET_CHARS)
        .collect::<String>();
    let mut snippet = String::with_capacity(body.len() + 8);
    if start > 0 {
        snippet.push('…');
    }
    snippet.push_str(&body);
    if start + SNIPPET_CHARS < total {
        snippet.push('…');
    }
    snippet
}

fn display_path(home_dir: &Path, path: &Path) -> String {
    match path.strip_prefix(home_dir) {
        Ok(relative) => format!("~/{}", relative.display()),
        Err(_) => path.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_query_terms_are_lowercased_deduplicated_and_capped() {
        let query = Query::parse("  Cache CACHE  Miss ").expect("query should parse");
        assert_eq!(query.terms, vec!["cache".to_owned(), "miss".to_owned()]);
        assert_eq!(query.phrase.as_deref(), Some("cache miss"));
        assert!(query.raw_prefilter);

        let many = (0..40).map(|index| format!("t{index}")).collect::<Vec<_>>();
        let query = Query::parse(&many.join(" ")).expect("query should parse");
        assert_eq!(query.terms.len(), MAX_QUERY_TERMS);

        assert!(Query::parse(" \t\n").is_none());
        assert!(
            !Query::parse("\"quoted\"")
                .expect("query should parse")
                .raw_prefilter
        );
    }

    #[test]
    fn test_snippet_is_bounded_and_marks_elision() {
        let text = "a".repeat(200) + " needle " + &"b".repeat(600);

        let excerpt = snippet(&text, 201);

        assert!(excerpt.starts_with('…') && excerpt.ends_with('…'));
        assert!(excerpt.contains("needle"));
        assert_eq!(excerpt.chars().count(), SNIPPET_CHARS + 2);
        assert_eq!(snippet("short text", 0), "short text");
    }

    #[test]
    fn test_scope_requires_a_recorded_working_directory_inside_the_root() {
        let scope = Scope::new(&RecallScope::Repository(PathBuf::from("/work/repo")));

        assert!(scope.admits(Some("/work/repo")));
        assert!(scope.admits(Some("/work/repo/crates/core")));
        assert!(!scope.admits(Some("/work/repo-other")));
        assert!(!scope.admits(Some("/work")));
        assert!(!scope.admits(None));
        assert!(Scope::new(&RecallScope::AllRepositories).admits(None));
    }
}
