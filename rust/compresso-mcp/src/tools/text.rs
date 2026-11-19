//! Compact text form of tool results (E17-31).
//!
//! Each renderer reads the same internal JSON payload the JSON form sends, so
//! both forms come from one result and the text carries the same facts:
//!
//! - repository text (file content, symbol bodies, grep rows, packs) is
//!   emitted raw, never escaped;
//! - one short header per file, range, or pattern, with no repeated keys;
//! - fields at their default or empty value are omitted, as are operation
//!   ids, token counters, providers, and schema versions;
//! - any non-default state a model must act on (truncation, totals beyond
//!   what is shown, staleness, per-item errors, next actions) is one terse
//!   trailing `[...]` line.
//!
//! Range headers start with `@@ ` so raw source cannot be mistaken for one.

use serde_json::Value;

/// Whether `tool` has a text form. The others (compress, stats, explain,
/// impact) send JSON in every profile.
pub(crate) fn supports(tool: &str) -> bool {
    matches!(
        tool,
        "compresso_repo_map"
            | "compresso_search"
            | "compresso_grep"
            | "compresso_read"
            | "compresso_outline"
            | "compresso_unfold"
            | "compresso_context"
            | "compresso_trace"
            | "compresso_recall"
            | "compresso_edit"
    )
}

/// The text form of `tool`'s successful `payload`, or `None` when the tool
/// has no text form.
pub(crate) fn render(tool: &str, payload: &Value) -> Option<String> {
    let mut out = Out::default();
    match tool {
        "compresso_repo_map" => repo_map(payload, &mut out),
        "compresso_search" => search(payload, &mut out),
        "compresso_grep" => grep(payload, &mut out),
        "compresso_read" => read(payload, &mut out),
        "compresso_outline" => outline(payload, &mut out),
        "compresso_unfold" => unfold(payload, &mut out),
        "compresso_context" => context(payload, &mut out),
        "compresso_trace" => trace(payload, &mut out),
        "compresso_recall" => recall(payload, &mut out),
        "compresso_edit" => edit(payload, &mut out),
        _ => return None,
    }
    Some(out.finish(payload))
}

/// Body lines plus trailing notes.
#[derive(Default)]
struct Out {
    body: String,
    notes: Vec<String>,
}

impl Out {
    fn line(&mut self, line: impl AsRef<str>) {
        self.body.push_str(line.as_ref());
        self.body.push('\n');
    }

    /// Raw repository text, newline-terminated so the next header starts on
    /// its own line.
    fn raw(&mut self, text: &str) {
        self.body.push_str(text);
        if !text.is_empty() && !text.ends_with('\n') {
            self.body.push('\n');
        }
    }

    fn note(&mut self, note: impl Into<String>) {
        self.notes.push(note.into());
    }

    /// Ends a section: its notes print now, before the next section's
    /// content (E17-36, D101).
    fn flush_notes(&mut self) {
        for note in std::mem::take(&mut self.notes) {
            self.line(format!("[{note}]"));
        }
    }

    /// The stale-index hint is common to every repository tool and closes
    /// the result.
    fn finish(mut self, payload: &Value) -> String {
        if let Some(hint) = string(payload, "reindex_hint") {
            self.notes.push(hint.to_owned());
        }
        for note in self.notes {
            self.body.push('[');
            self.body.push_str(&note);
            self.body.push_str("]\n");
        }
        self.body
    }
}

fn string<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

fn number(value: &Value, key: &str) -> u64 {
    value.get(key).and_then(Value::as_u64).unwrap_or_default()
}

fn flag(value: &Value, key: &str) -> bool {
    value.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn array<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    value
        .get(key)
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

/// `12` for a one-line span, else `12-40`.
fn lines(start: u64, end: u64) -> String {
    if start == end {
        start.to_string()
    } else {
        format!("{start}-{end}")
    }
}

/// A symbol as a model reads it: its signature when that already names it,
/// otherwise `qualified_name: signature`, or `kind qualified_name` when no
/// signature is indexed. The kind is implied by a signature, so it is shown
/// only without one.
fn symbol_label(qualified_name: &str, kind: &str, signature: Option<&str>) -> String {
    let signature = signature
        .map(|signature| signature.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|signature| !signature.is_empty());
    match signature {
        Some(signature)
            if !qualified_name.contains("::")
                && !qualified_name.contains('.')
                && signature.contains(qualified_name) =>
        {
            signature
        }
        Some(signature) => format!("{qualified_name}: {signature}"),
        None => format!("{kind} {qualified_name}"),
    }
}

/// One outline symbol: `START-END label`.
pub(crate) fn outline_row(symbol: &Value) -> String {
    format!(
        "{} {}",
        lines(number(symbol, "start_line"), number(symbol, "end_line")),
        symbol_label(
            string(symbol, "qualified_name").unwrap_or_default(),
            string(symbol, "kind").unwrap_or_default(),
            string(symbol, "signature"),
        )
    )
}

/// One search hit: `path:START-END label`.
pub(crate) fn search_row(hit: &Value) -> String {
    format!(
        "{}:{} {}",
        string(hit, "path").unwrap_or_default(),
        lines(number(hit, "start_line"), number(hit, "end_line")),
        symbol_label(
            string(hit, "qualified_name").unwrap_or_default(),
            string(hit, "kind").unwrap_or_default(),
            string(hit, "signature"),
        )
    )
}

/// One dropped search hit's location: `path:START-END`.
pub(crate) fn location_row(location: &Value) -> String {
    format!(
        "{}:{}",
        string(location, "path").unwrap_or_default(),
        lines(number(location, "start_line"), number(location, "end_line"))
    )
}

/// One ambiguous unfold candidate: `START-END kind qualified_name`. The
/// start line is what `start_line_hint` takes to pick it.
pub(crate) fn candidate_row(candidate: &Value) -> String {
    format!(
        "{} {} {}",
        lines(
            number(candidate, "start_line"),
            number(candidate, "end_line")
        ),
        string(candidate, "kind").unwrap_or_default(),
        string(candidate, "qualified_name").unwrap_or_default()
    )
}

/// A trace symbol: `path:LINE label`.
fn trace_symbol(symbol: &Value) -> String {
    format!(
        "{} {}",
        string(symbol, "defined_at").unwrap_or_default(),
        symbol_label(
            string(symbol, "symbol").unwrap_or_default(),
            string(symbol, "kind").unwrap_or_default(),
            string(symbol, "signature"),
        )
    )
}

/// How a trace edge was found: `[call SITE RESOLUTION, via X, cycle]`. A
/// `reference` edge passes the function as a value rather than calling it.
fn trace_edge(entry: &Value) -> String {
    let mut edge = format!(
        "[{} {} {}",
        if flag(entry, "reference") {
            "ref"
        } else {
            "call"
        },
        string(entry, "call_site").unwrap_or_default(),
        string(entry, "resolution").unwrap_or_default()
    );
    if let Some(via) = string(entry, "via") {
        edge.push_str(", via ");
        edge.push_str(via);
    }
    if flag(entry, "cycle") {
        edge.push_str(", cycle");
    }
    edge.push(']');
    edge
}

/// One caller or callee, indented by depth.
pub(crate) fn neighbor_row(entry: &Value) -> String {
    format!(
        "{}{} {}",
        "  ".repeat(usize::try_from(number(entry, "depth").saturating_sub(1)).unwrap_or(0)),
        trace_symbol(entry),
        trace_edge(entry)
    )
}

/// One call path: its first symbol, then one `-> ` hop per call.
pub(crate) fn path_rows(path: &Value) -> String {
    path.as_array()
        .map_or(&[][..], Vec::as_slice)
        .iter()
        .map(|hop| {
            if hop.get("call_site").and_then(Value::as_str).is_some() {
                format!("-> {} {}", trace_symbol(hop), trace_edge(hop))
            } else {
                trace_symbol(hop)
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// An ambiguity candidate for a trace end.
pub(crate) fn trace_candidate_row(candidate: &Value) -> String {
    trace_symbol(candidate)
}

/// A call the trace could not follow.
pub(crate) fn unfollowed_row(call: &Value) -> String {
    let mut row = format!(
        "not followed ({}): {} in {} at {}",
        string(call, "status").unwrap_or_default(),
        string(call, "call").unwrap_or_default(),
        string(call, "caller").unwrap_or_default(),
        string(call, "call_site").unwrap_or_default()
    );
    let candidates = array(call, "candidates")
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    if !candidates.is_empty() {
        row.push_str("; candidates: ");
        row.push_str(&candidates.join(", "));
    }
    row
}

fn repo_map(payload: &Value, out: &mut Out) {
    out.raw(string(payload, "map").unwrap_or_default());
    let (shown, total) = (
        number(payload, "files_shown"),
        number(payload, "files_total"),
    );
    if flag(payload, "truncated") || shown < total {
        out.note(format!(
            "map: {shown} of {total} files shown; pass focus or a smaller path to see more"
        ));
    }
    if let Some(cards) = payload.get("cards").and_then(Value::as_array) {
        for card in cards {
            out.line("");
            out.raw(card.as_str().unwrap_or_default());
        }
        let (shown, total) = (
            number(payload, "cards_shown"),
            number(payload, "cards_total"),
        );
        if flag(payload, "cards_truncated") || shown < total {
            out.note(format!(
                "cards: {shown} of {total} shown; pass path to zoom into one subtree"
            ));
        }
    }
}

fn search(payload: &Value, out: &mut Out) {
    let hits = array(payload, "hits");
    for hit in hits {
        out.line(search_row(hit));
    }
    for diagnostic in array(payload, "diagnostics") {
        out.note(format!(
            "{}: {}",
            string(diagnostic, "code").unwrap_or_default(),
            string(diagnostic, "message").unwrap_or_default()
        ));
    }
    if let Some(suggestion) = string(payload, "suggestion") {
        out.note(suggestion);
    }
    let total = number(payload, "hits_total");
    if flag(payload, "truncated") || (hits.len() as u64) < total {
        let locations = array(payload, "truncated_hits")
            .iter()
            .map(location_row)
            .collect::<Vec<_>>();
        let mut note = format!("{} of {total} hits shown", hits.len());
        if !locations.is_empty() {
            note.push_str("; more at ");
            note.push_str(&locations.join(", "));
        }
        let omitted = number(payload, "truncated_hits_omitted");
        if omitted > 0 {
            note.push_str(&format!("; {omitted} more not listed"));
        }
        out.note(note);
    }
}

fn grep(payload: &Value, out: &mut Out) {
    let mut unlisted = 0;
    let (mut named_tests, mut named_above) = (false, false);
    for result in array(payload, "results") {
        let mut pattern = string(result, "pattern").unwrap_or_default().to_owned();
        // A pattern's own scope (E17-36, D101), as the caller wrote it.
        if let Some(scope) = string(result, "path") {
            pattern.push_str(" in ");
            pattern.push_str(scope);
        }
        let matches = number(result, "matches_total");
        if matches == 0 {
            out.line(format!("# {pattern} — no matches"));
            continue;
        }
        out.line(format!(
            "# {pattern} — {matches} lines in {} files",
            number(result, "files_total")
        ));
        unlisted += number(result, "matches_total").saturating_sub(number(result, "lines_listed"));
        for file in array(result, "files") {
            // Lines named instead of listed (E17-34, D99) are numbers the
            // server wrote, never repository text.
            let named = [("tests", &mut named_tests), ("above", &mut named_above)]
                .into_iter()
                .filter_map(|(key, seen)| {
                    let lines = array(file, key)
                        .iter()
                        .filter_map(Value::as_u64)
                        .map(|line| line.to_string())
                        .collect::<Vec<_>>();
                    *seen |= !lines.is_empty();
                    (!lines.is_empty()).then(|| format!("; {key}: {}", lines.join(", ")))
                })
                .collect::<String>();
            out.line(format!(
                "{} ({}{}{named})",
                header_path(string(file, "path").unwrap_or_default()),
                number(file, "matches"),
                if flag(file, "stale") {
                    ", changed since indexed: no symbols"
                } else {
                    ""
                }
            ));
            // Rows start with their line number (`LINE [symbol] text` for a
            // match, `LINE- text` for context), so they need no indent.
            for row in array(file, "lines") {
                out.line(row.as_str().unwrap_or_default());
            }
        }
    }
    let clamped = array(payload, "clamped")
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    if !clamped.is_empty() {
        out.note(format!("limits clamped: {}", clamped.join(", ")));
    }
    // A pattern whose own scope was rejected (E17-36, D101): its section
    // is the error; the caller wrote the pattern and the scope.
    for failed in array(payload, "patterns_failed") {
        let error = failed.get("error").unwrap_or(&Value::Null);
        out.line(format!(
            "# {} in {} — [{}] {}",
            string(failed, "pattern").unwrap_or_default(),
            string(failed, "path").unwrap_or_default(),
            string(error, "code").unwrap_or_default(),
            string(error, "message").unwrap_or_default()
        ));
    }
    if named_tests {
        out.note(
            "tests: test-code lines, unlisted while code matches; read them or grep a test path",
        );
    }
    if named_above {
        out.note("above: listed under an earlier pattern");
    }
    let scan = payload.get("scan").unwrap_or(&Value::Null);
    let skipped = number(scan, "files_skipped_oversize") + number(scan, "files_skipped_unreadable");
    if flag(scan, "scan_truncated") {
        out.note(
            "scan stopped at its file or byte cap: totals are lower bounds; narrow with path instead of repeating the search",
        );
    } else if skipped > 0 {
        out.note(format!(
            "{skipped} files in scope not searched (over the size ceiling or unreadable): totals may be low"
        ));
    }
    if unlisted > 0 {
        out.note(format!(
            "{unlisted} matching lines counted but not listed; narrow with path or a more specific pattern rather than raising limits"
        ));
    }
    // A packed call's reads (E17-36, D101) follow every grep section and
    // its notes, so raw file content never precedes a grep header.
    if let Some(ranges) = payload.get("ranges").and_then(Value::as_array) {
        out.flush_notes();
        for range in ranges {
            read_range(range, out);
        }
    }
}

/// A grep file header's path with the characters that could imitate the
/// header's own syntax replaced by `?` (E17-34, D99): control characters,
/// `;`, and a `(` after a space. A path starting with `# ` or `@@ ` would
/// imitate a pattern or read section header, so that first character is
/// replaced too (E17-36, D101). Ordinary paths print unchanged.
fn header_path(path: &str) -> String {
    let mut previous = '\0';
    let section = path.starts_with("# ") || path.starts_with("@@ ");
    path.chars()
        .enumerate()
        .map(|(index, character)| {
            let safe = match character {
                _ if index == 0 && section => '?',
                ';' => '?',
                '(' if previous == ' ' => '?',
                character if character.is_control() => '?',
                character => character,
            };
            previous = character;
            safe
        })
        .collect()
}

/// One bounded read: `@@ path:START-END of TOTAL`, the raw content, and a
/// continuation note when the budget cut it.
fn read_range(range: &Value, out: &mut Out) {
    let path = string(range, "path").unwrap_or_default();
    if let Some(error) = range.get("error") {
        out.line(format!(
            "@@ {path} [{}] {}",
            string(error, "code").unwrap_or_default(),
            string(error, "message").unwrap_or_default()
        ));
        return;
    }
    let shown = range.get("range").unwrap_or(&Value::Null);
    let (start, end) = (number(shown, "start_line"), number(shown, "end_line"));
    out.line(format!(
        "@@ {path}:{start}-{end} of {}",
        number(range, "file_lines_total")
    ));
    let content = string(range, "content").unwrap_or_default();
    out.raw(content);
    if let Some(window) = range.get("window") {
        out.line(window_note(window));
        return;
    }
    if let Some(elision) = string(range, "elision") {
        // A cut inside a line repeats that line on the next read.
        let next = if content.is_empty() || content.ends_with('\n') {
            end + u64::from(!content.is_empty())
        } else {
            end
        };
        let elision = elision.trim_start_matches('[').trim_end_matches(']');
        out.line(format!("[{elision}; continue with start_line {next}]"));
    }
}

/// The one note for a read held to its line window (E17-33): which lines
/// were not served, the indexed symbols that start there, and where to
/// continue.
fn window_note(window: &Value) -> String {
    let (from, to) = (number(window, "from"), number(window, "to"));
    let mut note = format!(
        "[lines {from}-{to} not shown: past the {}-line read window",
        number(window, "lines")
    );
    let symbols = array(window, "symbols")
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    if !symbols.is_empty() {
        note.push_str("; starts there: ");
        note.push_str(&symbols.join(", "));
        let more = number(window, "symbols_more");
        if more > 0 {
            note.push_str(&format!(", +{more} more"));
        }
    }
    note.push_str(&format!("; continue with start_line {from}]"));
    note
}

fn read(payload: &Value, out: &mut Out) {
    if let Some(ranges) = payload.get("ranges").and_then(Value::as_array) {
        for range in ranges {
            read_range(range, out);
        }
        return;
    }
    if let Some(gate) = payload.get("gate") {
        out.line(format!(
            "@@ {} {}: {} tokens; recommended max_tokens {}",
            string(payload, "path").unwrap_or_default(),
            if flag(gate, "worth_reading") {
                "worth reading"
            } else {
                "not worth reading"
            },
            number(gate, "file_tokens_total"),
            number(gate, "recommended_budget")
        ));
        out.line(string(gate, "reason").unwrap_or_default());
        out.line(format!(
            "cheaper: {}",
            string(gate, "cheaper_alternative").unwrap_or_default()
        ));
        return;
    }
    read_range(payload, out);
}

fn outline(payload: &Value, out: &mut Out) {
    let mut header = string(payload, "path").unwrap_or_default().to_owned();
    if let Some(language) = string(payload, "language") {
        header.push_str(&format!(" ({language})"));
    }
    out.line(header);
    for symbol in array(payload, "symbols") {
        out.line(outline_row(symbol));
    }
    if let Some(elision) = string(payload, "elision") {
        out.note(format!(
            "{}; read a line range or raise max_tokens for the rest",
            elision.trim_start_matches('[').trim_end_matches(']')
        ));
    }
}

fn unfold(payload: &Value, out: &mut Out) {
    let path = string(payload, "path").unwrap_or_default();
    let name = string(payload, "qualified_name").unwrap_or_default();
    match string(payload, "status") {
        Some("found") => {
            let symbol = payload.get("symbol").unwrap_or(&Value::Null);
            out.line(format!(
                "@@ {path}:{} {}",
                lines(number(symbol, "start_line"), number(symbol, "end_line")),
                symbol_label(
                    string(symbol, "qualified_name").unwrap_or(name),
                    string(symbol, "kind").unwrap_or_default(),
                    None,
                )
            ));
            out.raw(string(payload, "content").unwrap_or_default());
        }
        Some("ambiguous") => {
            out.line(format!(
                "{name} is ambiguous in {path}; pass start_line_hint (or kind) to pick one:"
            ));
            for candidate in array(payload, "candidates") {
                out.line(candidate_row(candidate));
            }
            if let Some(elision) = string(payload, "elision") {
                out.note(elision.trim_start_matches('[').trim_end_matches(']'));
            }
        }
        _ => out.line(format!("{name} not found in {path}")),
    }
}

fn context(payload: &Value, out: &mut Out) {
    let mut status = format!(
        "[{}; next_action: {}",
        string(payload, "context_sufficiency").unwrap_or_default(),
        string(payload, "next_action").unwrap_or_default()
    );
    let limit = number(payload, "targeted_follow_up_limit");
    if limit > 0 {
        status.push_str(&format!("; at most {limit} follow-up calls"));
    }
    status.push(']');
    out.line(status);
    out.raw(string(payload, "content_pack").unwrap_or_default());
    if string(payload, "context_sufficiency") != Some("task_pack_ready") {
        let evidence = payload.get("evidence_coverage").unwrap_or(&Value::Null);
        let task = payload.get("task_coverage").unwrap_or(&Value::Null);
        let mut coverage = format!(
            "evidence files {}/{} of {} candidates; task concepts {}/{}",
            number(evidence, "files_included"),
            number(evidence, "files_required"),
            number(evidence, "candidate_files"),
            number(task, "concepts_covered"),
            number(task, "concepts_total")
        );
        let uncovered = array(task, "uncovered")
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>();
        if !uncovered.is_empty() {
            coverage.push_str("; uncovered: ");
            coverage.push_str(&uncovered.join(", "));
        }
        // Task parts are named only when some are uncovered, so a fully
        // covered task adds nothing to the state line. A single-part task
        // whose uncovered terms are already listed above is not repeated.
        let parts_total = number(task, "facets_total");
        let parts_covered = number(task, "facets_covered");
        let already_named = parts_total == 1 && !uncovered.is_empty();
        if parts_covered < parts_total && !already_named {
            coverage.push_str(&format!("; task parts {parts_covered}/{parts_total}"));
            let parts = array(task, "uncovered_facets")
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>();
            if !parts.is_empty() {
                coverage.push_str("; uncovered parts: ");
                coverage.push_str(&parts.join(" | "));
            }
        }
        out.note(coverage);
    }
    if let Some(note) = string(payload, "retrieval_note") {
        // The JSON note points at a field the text form does not carry; the
        // state line above already names the uncovered parts.
        out.note(note.replace(" (see task_coverage.uncovered_facets)", ""));
    }
    if flag(payload, "truncated") {
        out.note("pack cut to its token budget");
    }
    if flag(payload, "budget_clamped") {
        out.note(format!(
            "max_tokens clamped to {}",
            number(payload, "budget_ceiling")
        ));
    }
}

fn edit_locations(kind: &str, locations: &[Value], total: u64, out: &mut Out) {
    for location in locations {
        out.line(format!(
            "  {kind} {}:{}: {}",
            string(location, "path").unwrap_or_default(),
            number(location, "line"),
            string(location, "text").unwrap_or_default()
        ));
    }
    let listed = locations.len() as u64;
    if total > listed {
        out.line(format!("  +{} more {kind}", total - listed));
    }
}

/// `edited 8 sites in 1 file`, one `@@ path` row per file, one summary row
/// per rename with its leftovers, then the dry-run diff.
fn edit(payload: &Value, out: &mut Out) {
    let dry_run = flag(payload, "dry_run");
    let files = number(payload, "files_total");
    // The header carries every safety total, so no cut can hide them.
    let mut header = format!(
        "{} {} site(s) in {files} file(s)",
        if dry_run { "would edit" } else { "edited" },
        number(payload, "sites_total")
    );
    let incomplete = number(payload, "incomplete_operations");
    if incomplete > 0 {
        header.push_str(&format!("; INCOMPLETE: {incomplete} rename(s)"));
        let skipped = number(payload, "files_skipped_total");
        if skipped > 0 {
            header.push_str(&format!(", {skipped} skipped file(s) hold the old name"));
        }
        if payload.get("index_refreshed") == Some(&Value::Bool(false)) {
            header.push_str(", the index could not be refreshed, so new files may be missed");
        }
        let outside = number(payload, "outside_scope_whole_total");
        if outside > 0 {
            header.push_str(&format!(
                ", {outside} whole match(es) of an old name remain outside a rename's path scope"
            ));
        }
    }
    for (key, label) in [
        (
            "remaining_whole_total",
            "whole match(es) of an old name remain",
        ),
        (
            "remaining_substring_total",
            "substring match(es) inside longer names",
        ),
        (
            "remaining_adjacent_total",
            "adjacent match(es) to check by hand",
        ),
        ("unverified_files", "file(s) NOT verified on disk"),
    ] {
        let count = number(payload, key);
        if count > 0 {
            header.push_str(&format!("; {count} {label}"));
        }
    }
    out.line(header);
    for file in array(payload, "files") {
        let lines = array(file, "lines")
            .iter()
            .filter_map(Value::as_u64)
            .map(|line| line.to_string())
            .collect::<Vec<_>>();
        let mut row = format!(
            "@@ {}: {} site(s)",
            string(file, "path").unwrap_or_default(),
            number(file, "sites")
        );
        if !lines.is_empty() {
            row.push_str(" at line ");
            row.push_str(&lines.join(", "));
            if (lines.len() as u64) < number(file, "sites") {
                row.push_str(", ...");
            }
        }
        if flag(file, "stale") {
            row.push_str("; index was stale, matched current content");
        }
        if string(file, "line_ending") == Some("crlf") {
            row.push_str("; crlf kept");
        }
        if flag(file, "bom") {
            row.push_str("; bom kept");
        }
        if !dry_run && !flag(file, "verified") {
            row.push_str("; NOT verified on disk");
        }
        out.line(row);
    }
    for operation in array(payload, "operations") {
        if string(operation, "op") != Some("rename") {
            continue;
        }
        let mut row = format!(
            "rename {} -> {}: {} site(s); searched {} file(s)",
            string(operation, "old").unwrap_or_default(),
            string(operation, "new").unwrap_or_default(),
            number(operation, "sites"),
            number(operation, "files_searched")
        );
        if let Some(scope) = string(operation, "scope") {
            row.push_str(&format!(" under {scope}"));
        }
        let scoped = operation.get("outside_scope_whole_total").is_some();
        row.push_str(&format!(
            "; remaining{}: {} whole, {} substring, {} adjacent",
            if scoped { " in scope" } else { "" },
            number(operation, "remaining_whole_total"),
            number(operation, "remaining_substring_total"),
            number(operation, "remaining_adjacent_total")
        ));
        // A scoped rename (E17-36, D101) also states what it left outside.
        let outside = number(operation, "outside_scope_whole_total");
        if scoped {
            row.push_str(&format!(
                "; {}{outside} whole outside scope, not renamed",
                if flag(operation, "outside_scope_searched") {
                    ""
                } else {
                    "at least "
                }
            ));
        }
        if operation.get("complete") == Some(&Value::Bool(false)) {
            let skipped = number(operation, "files_skipped_total") > 0;
            row.push_str(match (skipped, outside > 0) {
                (true, true) => "; INCOMPLETE: skipped files hold the old name, and it remains outside the scope",
                (false, true) => "; INCOMPLETE: the old name remains outside the scope",
                _ => "; INCOMPLETE: skipped files hold the old name",
            });
        }
        out.line(row);
        edit_locations(
            "whole",
            array(operation, "remaining_whole"),
            number(operation, "remaining_whole_total"),
            out,
        );
        edit_locations(
            "outside scope",
            array(operation, "outside_scope_whole"),
            outside,
            out,
        );
        edit_locations(
            "substring",
            array(operation, "remaining_substring"),
            number(operation, "remaining_substring_total"),
            out,
        );
        // Next to a string prefix, escape, or sigil: left as is, check by hand.
        edit_locations(
            "adjacent",
            array(operation, "remaining_adjacent"),
            number(operation, "remaining_adjacent_total"),
            out,
        );
        for skipped in array(operation, "files_skipped") {
            out.line(format!(
                "  skipped {} ({})",
                string(skipped, "path").unwrap_or_default(),
                string(skipped, "reason").unwrap_or_default()
            ));
        }
        let preexisting = number(operation, "new_name_preexisting");
        if preexisting > 0 {
            out.note(format!(
                "'{}' already occurred {preexisting} time(s) before the rename",
                string(operation, "new").unwrap_or_default()
            ));
        }
    }
    if let Some(diff) = string(payload, "diff").filter(|diff| !diff.is_empty()) {
        out.raw(diff);
    }
    if dry_run {
        out.note("dry run: nothing written");
    }
    if string(payload, "index") == Some("stale") {
        out.note("edit written but the index is stale; run `compresso index`");
    }
    if flag(payload, "diff_truncated") {
        out.note("diff cut");
    }
    if flag(payload, "truncated") {
        out.note("report cut to max_tokens; totals are exact");
    }
}

fn trace(payload: &Value, out: &mut Out) {
    let from = string(payload, "from");
    let to = string(payload, "to");
    let (key, row): (&str, fn(&Value) -> String) = match string(payload, "status") {
        Some("callers") => {
            out.line(format!("callers of {}:", to.unwrap_or_default()));
            ("callers", neighbor_row)
        }
        Some("callees") => {
            out.line(format!("callees of {}:", from.unwrap_or_default()));
            ("callees", neighbor_row)
        }
        Some("paths") => ("paths", path_rows),
        Some("no_path") => {
            out.line(format!(
                "no call path from {} to {} within max_depth {}",
                from.unwrap_or_default(),
                to.unwrap_or_default(),
                number(payload, "max_depth")
            ));
            ("paths", path_rows)
        }
        Some("ambiguous") => {
            out.line(format!(
                "{} '{}' matches {} symbols; qualify it as path:name:",
                string(payload, "end").unwrap_or_default(),
                if string(payload, "end") == Some("from") {
                    from.unwrap_or_default()
                } else {
                    to.unwrap_or_default()
                },
                number(payload, "candidates_total")
            ));
            ("candidates", trace_candidate_row)
        }
        Some("not_found") => {
            out.line(format!(
                "{} symbol not found: {}",
                string(payload, "end").unwrap_or_default(),
                string(payload, "name").unwrap_or_default()
            ));
            ("candidates", trace_candidate_row)
        }
        _ => ("paths", path_rows),
    };
    let items = array(payload, key);
    for (index, item) in items.iter().enumerate() {
        if key == "paths" {
            out.line(format!("path {}:", index + 1));
        }
        out.line(row(item));
    }
    let unfollowed = array(payload, "unfollowed");
    for call in unfollowed {
        out.line(unfollowed_row(call));
    }
    let total = number(payload, "total");
    if key != "candidates" && (items.len() as u64) < total {
        out.note(format!(
            "{} of {total} shown; raise max_results or max_tokens, or narrow from/to",
            items.len()
        ));
    }
    let unfollowed_total = number(payload, "unfollowed_total");
    if (unfollowed.len() as u64) < unfollowed_total {
        out.note(format!(
            "{} more calls not followed",
            unfollowed_total - unfollowed.len() as u64
        ));
    }
    if flag(payload, "depth_limited") {
        out.note(format!(
            "stopped at max_depth {}; raise it to go further",
            number(payload, "max_depth")
        ));
    }
    if !flag(payload, "complete") && !flag(payload, "depth_limited") {
        out.note("incomplete: a traversal bound stopped the walk");
    }
}

fn recall(payload: &Value, out: &mut Out) {
    out.line(format!(
        "[{}]",
        string(payload, "notice").unwrap_or_default()
    ));
    for hit in array(payload, "hits") {
        out.line(format!(
            "@@ {} {} {} {} {}:{}",
            string(hit, "source").unwrap_or_default(),
            string(hit, "session_id").unwrap_or_default(),
            string(hit, "timestamp").unwrap_or_default(),
            string(hit, "role").unwrap_or_default(),
            string(hit, "transcript").unwrap_or_default(),
            number(hit, "line")
        ));
        out.raw(string(hit, "snippet").unwrap_or_default());
    }
    for source in array(payload, "sources") {
        let status = string(source, "status").unwrap_or_default();
        let unrecognized = number(source, "files_unrecognized");
        let detail = string(source, "detail");
        if status != "searched" || unrecognized > 0 || detail.is_some() {
            let mut note = format!(
                "{}: {status}, {} files scanned",
                string(source, "source").unwrap_or_default(),
                number(source, "files_scanned")
            );
            if unrecognized > 0 {
                note.push_str(&format!(", {unrecognized} unrecognized"));
            }
            if let Some(detail) = detail {
                note.push_str(&format!(" ({detail})"));
            }
            out.note(note);
        }
    }
    if flag(payload, "truncated") {
        out.note("more hits exist; raise limit or refine the query");
    }
    let redactions = number(payload, "redactions");
    if redactions > 0 {
        out.note(format!("{redactions} secrets redacted"));
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn test_symbol_label_keeps_owner_and_drops_redundant_kind() {
        assert_eq!(
            symbol_label("open", "function", Some("pub fn open(name: &str)")),
            "pub fn open(name: &str)"
        );
        assert_eq!(
            symbol_label(
                "Session::open",
                "method",
                Some("pub fn open(\n    name: &str,\n)")
            ),
            "Session::open: pub fn open( name: &str, )"
        );
        assert_eq!(symbol_label("Session", "struct", None), "struct Session");
        assert_eq!(symbol_label("x", "const", Some("  ")), "const x");
    }

    #[test]
    fn test_read_continuation_follows_a_whole_line_and_repeats_a_cut_line() {
        let range = |content: &str| {
            json!({
                "path": "a.rs",
                "content": content,
                "range": { "start_line": 3, "end_line": 4 },
                "file_lines_total": 9,
                "elision": "[lines 4-9 elided: 50 tokens over budget]"
            })
        };
        let whole = render("compresso_read", &range("c\nd\n")).expect("text form");
        assert_eq!(
            whole,
            "@@ a.rs:3-4 of 9\nc\nd\n[lines 4-9 elided: 50 tokens over budget; continue with start_line 5]\n"
        );
        let cut = render("compresso_read", &range("c\nd")).expect("text form");
        assert!(cut.ends_with("continue with start_line 4]\n"), "{cut}");
    }

    #[test]
    fn test_default_state_adds_no_trailing_lines() {
        let payload = json!({
            "path": "a.rs",
            "content": "x\n",
            "range": { "start_line": 1, "end_line": 1 },
            "file_lines_total": 1,
            "elision": null,
            "truncated": false,
            "stale": false,
            "reindex_hint": null,
            "operation_id": "01ABC",
            "tokens_used": 3,
            "token_provider": "openai_o200k",
            "schema_version": 1
        });
        assert_eq!(
            render("compresso_read", &payload).expect("text form"),
            "@@ a.rs:1-1 of 1\nx\n"
        );
        assert!(render("compresso_stats", &payload).is_none());
    }

    fn context_payload(covered: u64, total: u64, uncovered: &[&str]) -> Value {
        json!({
            "context_sufficiency": "targeted_follow_up_needed",
            "next_action": "targeted_follow_up",
            "targeted_follow_up_limit": 2,
            "content_pack": "# pack\n",
            "evidence_coverage": { "files_included": 1, "files_required": 1, "candidate_files": 1 },
            "task_coverage": {
                "concepts_covered": 2, "concepts_total": 2, "uncovered": [],
                "facets_covered": covered, "facets_total": total, "uncovered_facets": uncovered
            },
            "retrieval_note": "The evidence found does not cover every part of the task (see task_coverage.uncovered_facets). Answer only the covered parts.",
            "truncated": false
        })
    }

    #[test]
    fn test_context_names_uncovered_task_parts_in_the_state_line() {
        let text = render(
            "compresso_context",
            &context_payload(1, 3, &["retry backoff", "store migration"]),
        )
        .expect("text form");
        assert!(
            text.contains(
                "task concepts 2/2; task parts 1/3; uncovered parts: retry backoff | store migration]"
            ),
            "{text}"
        );
        assert!(!text.contains("task_coverage."), "{text}");
        assert!(
            text.contains("does not cover every part of the task. Answer"),
            "{text}"
        );
    }

    #[test]
    fn test_context_omits_task_parts_when_all_are_covered() {
        let text = render("compresso_context", &context_payload(2, 2, &[])).expect("text form");
        assert!(!text.contains("task parts"), "{text}");
    }

    #[test]
    fn test_context_names_a_single_uncovered_part_only_when_its_terms_are_not_listed() {
        // Concepts all covered, part uncovered: the part must be named.
        let text =
            render("compresso_context", &context_payload(0, 1, &["retry"])).expect("text form");
        assert!(
            text.contains("task parts 0/1; uncovered parts: retry]"),
            "{text}"
        );
        // The uncovered concepts already name the part's terms.
        let mut payload = context_payload(0, 1, &["retry"]);
        payload["task_coverage"]["uncovered"] = json!(["retry"]);
        let text = render("compresso_context", &payload).expect("text form");
        assert!(!text.contains("task parts"), "{text}");
    }

    #[test]
    fn test_edit_header_names_the_real_reason_a_rename_is_incomplete() {
        let payload = json!({
            "dry_run": false, "files_total": 1, "sites_total": 2,
            "incomplete_operations": 1, "files_skipped_total": 0,
            "index_refreshed": false, "remaining_substring_total": 3,
            "files": [], "operations": []
        });
        let text = render("compresso_edit", &payload).expect("text form");
        let header = text.lines().next().expect("header");
        assert!(header.contains("index could not be refreshed"), "{header}");
        assert!(!header.contains("skipped file(s) hold"), "{header}");
        assert!(header.contains("3 substring match(es)"), "{header}");
    }
}
