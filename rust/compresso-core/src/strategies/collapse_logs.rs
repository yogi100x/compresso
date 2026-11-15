use std::collections::BTreeSet;

use crate::graph::{ContextGraph, NodeKind};
use crate::safety::duplicate_group_node_ids;
use crate::segment::repeated_trace_spans;
use crate::strategy::{
    CompressionBudget, CompressionStrategy, Estimate, RemovalKind, RemovalRecord, StrategyError,
    StrategyExecution, StrategyOutput,
};
use crate::tokens::counter_for;

pub struct CollapseLogs;

struct CollapsePlan {
    changes: Vec<NodeChange>,
    notes: Vec<String>,
    tokens_saved: usize,
}

struct NodeChange {
    index: usize,
    content: String,
    token_count: usize,
    protect_representative: bool,
    removals: Vec<RemovalRecord>,
}

struct CandidateSpan {
    start_line: usize,
    end_line: usize,
    locator_start_line: usize,
    locator_end_line: usize,
    replacement: String,
    units_removed: usize,
    net_lines_removed: usize,
    protect_representative: bool,
}

struct Line<'a> {
    text: &'a str,
    ending: &'a str,
}

impl CompressionStrategy for CollapseLogs {
    fn id(&self) -> &'static str {
        "collapse_logs"
    }

    fn supports(&self, input: &ContextGraph) -> bool {
        !collapse_plan(input, &[]).changes.is_empty()
    }

    fn estimate(&self, input: &ContextGraph) -> Estimate {
        let plan = collapse_plan(input, &[]);
        estimate_for_plan(&plan)
    }

    fn execute(
        &self,
        input: &ContextGraph,
        budget: &CompressionBudget,
    ) -> Result<StrategyOutput, StrategyError> {
        let plan = collapse_plan(input, &budget.protected_kinds);
        Ok(execute_plan(input, plan))
    }

    fn execute_if_supported(
        &self,
        input: &ContextGraph,
        budget: &CompressionBudget,
    ) -> StrategyExecution {
        let plan = collapse_plan(input, &budget.protected_kinds);
        if plan.changes.is_empty() {
            StrategyExecution::NotSupported
        } else {
            let estimate = estimate_for_plan(&plan);
            StrategyExecution::Executed {
                estimate,
                output: Ok(execute_plan(input, plan)),
            }
        }
    }
}

fn estimate_for_plan(plan: &CollapsePlan) -> Estimate {
    if plan.changes.is_empty() {
        Estimate::default()
    } else {
        Estimate {
            tokens_saved: plan.tokens_saved,
            confidence: 1.0,
        }
    }
}

fn execute_plan(input: &ContextGraph, plan: CollapsePlan) -> StrategyOutput {
    let mut graph = input.clone();
    let mut removals = Vec::new();
    let mut changed_node_ids = BTreeSet::new();

    for change in plan.changes {
        let node = &mut graph.nodes[change.index];
        changed_node_ids.insert(node.id.clone());
        node.content = change.content;
        node.token_count = change.token_count;
        // A collapsed multiline trace is now the sole root-cause representative.
        // Protect it before later strategies (notably budget packing) execute.
        node.protected |= change.protect_representative;
        removals.extend(change.removals);
    }

    StrategyOutput {
        graph,
        changed_node_ids,
        notes: plan.notes,
        removals,
    }
}

fn collapse_plan(input: &ContextGraph, protected_kinds: &[NodeKind]) -> CollapsePlan {
    let mut changes = Vec::new();
    let mut notes = Vec::new();
    let mut tokens_saved = 0_usize;
    let duplicate_group_nodes = duplicate_group_node_ids(input);

    for (index, node) in input.nodes.iter().enumerate() {
        if node.protected || node.kind.requires_protection() || protected_kinds.contains(&node.kind)
        {
            notes.push(format!("skipped node: {} is protected", node.id));
            continue;
        }
        if duplicate_group_nodes.contains(node.id.as_str()) {
            notes.push(format!(
                "skipped node: {} belongs to a duplicate referent group",
                node.id
            ));
            continue;
        }
        if matches!(node.kind, NodeKind::SourceFile | NodeKind::CodeSymbol) {
            notes.push(format!("skipped node: {} is code content", node.id));
            continue;
        }

        let spans = candidate_spans(&node.content);
        if spans.is_empty() {
            notes.push(format!(
                "skipped node: {} has no runs of 3 or more",
                node.id
            ));
            continue;
        }

        let counter = counter_for(node.token_provider);
        let mut content = node.content.clone();
        let mut current_tokens = node.token_count;
        let mut removed_prior_lines = 0_usize;
        let mut node_removals = Vec::new();
        let mut protect_representative = false;

        for span in spans {
            let start_line = span.start_line - removed_prior_lines;
            let end_line = span.end_line - removed_prior_lines;
            let lines = split_lines(&content);
            let candidate = replace_lines(&lines, start_line, end_line, &span.replacement);
            let after_tokens = counter.count(&candidate);
            let original_start = span.locator_start_line + 1;
            let original_end = span.locator_end_line;

            if after_tokens >= current_tokens {
                notes.push(format!(
                    "skipped node: {} lines {original_start}-{original_end}; no token reduction",
                    node.id
                ));
                continue;
            }

            node_removals.push(RemovalRecord {
                strategy_id: "collapse_logs".to_owned(),
                node_id: node.id.clone(),
                kind: RemovalKind::CollapsedLines,
                locator: Some(format!("lines:{original_start}-{original_end}")),
                original_hash: content_hash(&content),
                replacement_hash: Some(content_hash(&candidate)),
                before_tokens: current_tokens,
                after_tokens,
                units_removed: Some(span.units_removed),
            });
            notes.push(format!(
                "collapsed node: {} lines {original_start}-{original_end}; removed {} lines",
                node.id, span.units_removed
            ));
            tokens_saved = tokens_saved.saturating_add(current_tokens - after_tokens);
            content = candidate;
            current_tokens = after_tokens;
            removed_prior_lines += span.net_lines_removed;
            protect_representative |= span.protect_representative;
        }

        if !node_removals.is_empty() {
            changes.push(NodeChange {
                index,
                content,
                token_count: current_tokens,
                protect_representative,
                removals: node_removals,
            });
        }
    }

    CollapsePlan {
        changes,
        notes,
        tokens_saved,
    }
}

fn candidate_spans(content: &str) -> Vec<CandidateSpan> {
    let lines = split_lines(content);
    let trace_spans = repeated_trace_spans(content);
    let mut spans = trace_spans
        .iter()
        .map(|span| {
            let mut replacement =
                render_lines(&lines[span.start_line..span.representative_end_line]);
            replacement.push_str(&format!(
                "[×{} repeats]{}",
                span.repeats,
                lines[span.end_line - 1].ending
            ));
            let units_removed = span.end_line - span.representative_end_line;
            CandidateSpan {
                start_line: span.start_line,
                end_line: span.end_line,
                locator_start_line: span.representative_end_line,
                locator_end_line: span.end_line,
                replacement,
                units_removed: units_removed - 1,
                net_lines_removed: units_removed - 1,
                protect_representative: true,
            }
        })
        .collect::<Vec<_>>();
    let mut run_start = 0_usize;

    while run_start < lines.len() {
        let normalized = normalize_line(lines[run_start].text);
        let mut run_end = run_start + 1;
        while run_end < lines.len() && normalize_line(lines[run_end].text) == normalized {
            run_end += 1;
        }

        let run_length = run_end - run_start;
        let overlaps_trace = trace_spans
            .iter()
            .any(|span| run_start < span.end_line && run_end > span.start_line);
        if run_length >= 3 && !overlaps_trace {
            spans.push(CandidateSpan {
                start_line: run_start,
                end_line: run_end,
                locator_start_line: run_start,
                locator_end_line: run_end,
                replacement: format!(
                    "{} [×{run_length} repeats]{}",
                    lines[run_start].text,
                    lines[run_end - 1].ending
                ),
                units_removed: run_length - 1,
                net_lines_removed: run_length - 1,
                protect_representative: false,
            });
        }
        run_start = run_end;
    }

    spans.sort_by_key(|span| span.start_line);
    spans
}

fn render_lines(lines: &[Line<'_>]) -> String {
    let mut rendered = String::new();
    for line in lines {
        rendered.push_str(line.text);
        rendered.push_str(line.ending);
    }
    rendered
}

fn split_lines(content: &str) -> Vec<Line<'_>> {
    let bytes = content.as_bytes();
    let mut lines = Vec::new();
    let mut start = 0_usize;

    while start < bytes.len() {
        if let Some(relative_newline) = bytes[start..].iter().position(|byte| *byte == b'\n') {
            let newline = start + relative_newline;
            let (text_end, ending) = if newline > start && bytes[newline - 1] == b'\r' {
                (newline - 1, "\r\n")
            } else {
                (newline, "\n")
            };
            lines.push(Line {
                text: &content[start..text_end],
                ending,
            });
            start = newline + 1;
        } else {
            lines.push(Line {
                text: &content[start..],
                ending: "",
            });
            break;
        }
    }

    lines
}

fn replace_lines(
    lines: &[Line<'_>],
    start_line: usize,
    end_line: usize,
    replacement: &str,
) -> String {
    let mut output = String::new();
    for line in &lines[..start_line] {
        output.push_str(line.text);
        output.push_str(line.ending);
    }
    output.push_str(replacement);
    for line in &lines[end_line..] {
        output.push_str(line.text);
        output.push_str(line.ending);
    }
    output
}

fn normalize_line(line: &str) -> String {
    let bytes = line.as_bytes();
    let mut normalized = String::with_capacity(line.len());
    let mut copied_until = 0_usize;
    let mut cursor = 0_usize;

    while cursor < bytes.len() {
        let matched = iso8601_len(bytes, cursor)
            .map(|length| (length, "<iso8601>"))
            .or_else(|| uuid_len(bytes, cursor).map(|length| (length, "<uuid>")))
            .or_else(|| hex_token_len(bytes, cursor).map(|length| (length, "<hex>")))
            .or_else(|| duration_len(bytes, cursor).map(|length| (length, "<duration>")));

        if let Some((length, marker)) = matched {
            normalized.push_str(&line[copied_until..cursor]);
            normalized.push_str(marker);
            cursor += length;
            copied_until = cursor;
        } else if let Some(character) = line[cursor..].chars().next() {
            cursor += character.len_utf8();
        } else {
            break;
        }
    }

    normalized.push_str(&line[copied_until..]);
    normalized
}

fn iso8601_len(bytes: &[u8], start: usize) -> Option<usize> {
    if !is_boundary_before(bytes, start) || start.checked_add(19)? > bytes.len() {
        return None;
    }
    let fixed_digits = [(0, 4), (5, 2), (8, 2), (11, 2), (14, 2), (17, 2)];
    if fixed_digits
        .iter()
        .any(|(offset, length)| !are_digits(bytes, start + offset, *length))
        || bytes[start + 4] != b'-'
        || bytes[start + 7] != b'-'
        || !matches!(bytes[start + 10], b'T' | b't' | b' ')
        || bytes[start + 13] != b':'
        || bytes[start + 16] != b':'
    {
        return None;
    }

    let mut end = start + 19;
    if bytes.get(end) == Some(&b'.') {
        end += 1;
        let fraction_start = end;
        while bytes.get(end).is_some_and(u8::is_ascii_digit) {
            end += 1;
        }
        if end == fraction_start {
            return None;
        }
    }

    if matches!(bytes.get(end), Some(b'Z' | b'z')) {
        end += 1;
    } else if matches!(bytes.get(end), Some(b'+' | b'-')) {
        if !are_digits(bytes, end + 1, 2)
            || bytes.get(end + 3) != Some(&b':')
            || !are_digits(bytes, end + 4, 2)
        {
            return None;
        }
        end += 6;
    }

    is_boundary_after(bytes, end).then_some(end - start)
}

fn uuid_len(bytes: &[u8], start: usize) -> Option<usize> {
    const UUID_LENGTH: usize = 36;
    let end = start.checked_add(UUID_LENGTH)?;
    if !is_boundary_before(bytes, start) || end > bytes.len() || !is_boundary_after(bytes, end) {
        return None;
    }

    for offset in 0..UUID_LENGTH {
        let byte = bytes[start + offset];
        if matches!(offset, 8 | 13 | 18 | 23) {
            if byte != b'-' {
                return None;
            }
        } else if !byte.is_ascii_hexdigit() {
            return None;
        }
    }
    Some(UUID_LENGTH)
}

/// Matches an elapsed time such as `3.01s`, `950ms`, or `120µs`.
///
/// Only sub-minute time units are recognised, written directly after the
/// number, so memory sizes (`512m`), versions (`1.2.3`), and identifiers
/// (`build1s`) keep their exact text.
fn duration_len(bytes: &[u8], start: usize) -> Option<usize> {
    const UNITS: [&[u8]; 5] = [b"ms", "µs".as_bytes(), b"us", b"ns", b"s"];

    if !is_boundary_before(bytes, start) || (start > 0 && bytes[start - 1] == b'.') {
        return None;
    }
    let mut end = start;
    while bytes.get(end).is_some_and(u8::is_ascii_digit) {
        end += 1;
    }
    if end == start {
        return None;
    }
    if bytes.get(end) == Some(&b'.') {
        let fraction_start = end + 1;
        let mut fraction_end = fraction_start;
        while bytes.get(fraction_end).is_some_and(u8::is_ascii_digit) {
            fraction_end += 1;
        }
        if fraction_end == fraction_start {
            return None;
        }
        end = fraction_end;
    }
    let unit = UNITS.iter().find(|unit| bytes[end..].starts_with(unit))?;
    let end = end + unit.len();
    is_boundary_after(bytes, end).then_some(end - start)
}

fn hex_token_len(bytes: &[u8], start: usize) -> Option<usize> {
    if !is_boundary_before(bytes, start) {
        return None;
    }

    if bytes.get(start) == Some(&b'0') && matches!(bytes.get(start + 1), Some(b'x' | b'X')) {
        let mut end = start + 2;
        while bytes.get(end).is_some_and(u8::is_ascii_hexdigit) {
            end += 1;
        }
        return ((end - start - 2) >= 8 && is_boundary_after(bytes, end)).then_some(end - start);
    }

    let mut end = start;
    while bytes.get(end).is_some_and(|byte| is_token_byte(*byte)) {
        end += 1;
    }
    let token = bytes.get(start..end)?;
    (token.len() >= 8 && token.iter().all(u8::is_ascii_hexdigit)).then_some(token.len())
}

fn are_digits(bytes: &[u8], start: usize, length: usize) -> bool {
    bytes
        .get(start..start.saturating_add(length))
        .is_some_and(|value| value.iter().all(u8::is_ascii_digit))
}

fn is_boundary_before(bytes: &[u8], start: usize) -> bool {
    start == 0 || !is_token_byte(bytes[start - 1])
}

fn is_boundary_after(bytes: &[u8], end: usize) -> bool {
    bytes.get(end).is_none_or(|byte| !is_token_byte(*byte))
}

fn is_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn content_hash(content: &str) -> String {
    blake3::hash(content.as_bytes()).to_hex().to_string()
}

#[cfg(test)]
mod tests {
    use serde_json::Map;

    use super::*;
    use crate::graph::{ContextEdge, ContextGraph, ContextNode, EdgeKind, NodeKind, Source};
    use crate::strategy::{
        CompressionBudget, CompressionStrategy, Estimate, Lossiness, Mode, RemovalKind,
        StrategyRegistry,
    };
    use crate::tokens::{TokenProvider, counter_for};

    const FIVE_HUNDRED_LINES: &str =
        include_str!("../../../../fixtures/logs/repeated-npm-install/input.txt");

    fn node(id: &str, kind: NodeKind, content: &str, protected: bool) -> ContextNode {
        let counter = counter_for(TokenProvider::OpenAiO200k);
        ContextNode {
            id: id.to_owned(),
            kind,
            content: content.to_owned(),
            source: Source::Synthetic,
            token_count: counter.count(content),
            token_provider: counter.provider(),
            priority: 0.5,
            protected,
            recency: None,
            metadata: Map::new(),
            provider_turn: None,
        }
    }

    fn graph_with(node: ContextNode) -> ContextGraph {
        ContextGraph {
            schema_version: 1,
            nodes: vec![node],
            edges: Vec::new(),
        }
    }

    fn budget() -> CompressionBudget {
        CompressionBudget {
            max_tokens: 2_000,
            max_latency_ms: 200,
            allowed_lossiness: Lossiness::Structural,
            protected_kinds: Vec::new(),
        }
    }

    #[test]
    fn test_collapse_500_timestamped_lines() {
        let mut graph = graph_with(node(
            "log-1",
            NodeKind::ToolResponse,
            FIVE_HUNDRED_LINES,
            false,
        ));
        graph.edges.push(ContextEdge {
            from: "log-1".to_owned(),
            to: "log-1".to_owned(),
            kind: EdgeKind::References,
            weight: 1.0,
        });
        let original_edges = graph.edges.clone();

        let output = CollapseLogs
            .execute(&graph, &budget())
            .expect("timestamped log run should collapse");
        let collapsed = &output.graph.nodes[0];
        let first_raw_line = FIVE_HUNDRED_LINES
            .lines()
            .next()
            .expect("fixture should contain a first line");

        assert!(collapsed.content.lines().count() <= 5);
        assert!(
            collapsed
                .content
                .starts_with(&format!("{first_raw_line} [×500 repeats]"))
        );
        assert!(collapsed.content.contains("[×500 repeats]"));
        assert!(collapsed.content.ends_with('\n'));
        assert!(
            output
                .notes
                .iter()
                .any(|note| note.contains("collapsed") && note.contains("499"))
        );
        assert_eq!(output.graph.edges, original_edges);
        assert_eq!(output.graph.validate(), Ok(()));
        assert_eq!(
            output.changed_node_ids,
            BTreeSet::from(["log-1".to_owned()])
        );

        assert_eq!(output.removals.len(), 1);
        let record = &output.removals[0];
        assert_eq!(record.strategy_id, "collapse_logs");
        assert_eq!(record.node_id, "log-1");
        assert_eq!(record.kind, RemovalKind::CollapsedLines);
        assert_eq!(record.locator.as_deref(), Some("lines:1-500"));
        assert_eq!(record.units_removed, Some(499));
        assert_eq!(record.before_tokens, graph.nodes[0].token_count);
        assert_eq!(record.after_tokens, collapsed.token_count);
        assert_eq!(record.original_hash, content_hash(FIVE_HUNDRED_LINES));
        let replacement_hash = content_hash(&collapsed.content);
        assert_eq!(
            record.replacement_hash.as_deref(),
            Some(replacement_hash.as_str())
        );
        assert_eq!(
            record.before_tokens - record.after_tokens,
            graph.total_tokens() - output.graph.total_tokens()
        );

        let estimate = CollapseLogs.estimate(&graph);
        assert_eq!(
            estimate.tokens_saved,
            graph.total_tokens() - output.graph.total_tokens()
        );
        assert_eq!(estimate.confidence, 1.0);
    }

    #[test]
    fn test_combined_execution_matches_public_estimate_and_output() {
        let graph = graph_with(node(
            "log-1",
            NodeKind::ToolResponse,
            FIVE_HUNDRED_LINES,
            false,
        ));
        let expected_estimate = CollapseLogs.estimate(&graph);
        let expected_output = CollapseLogs
            .execute(&graph, &budget())
            .expect("standalone collapse should execute");

        let StrategyExecution::Executed { estimate, output } =
            CollapseLogs.execute_if_supported(&graph, &budget())
        else {
            panic!("the committed log fixture should use combined execution");
        };

        assert_eq!(estimate, expected_estimate);
        assert_eq!(
            output.expect("combined collapse should execute"),
            expected_output
        );
    }

    #[test]
    fn test_two_repeats_untouched() {
        let content =
            "2026-07-21T09:00:00Z request completed\n2026-07-21T09:00:01Z request completed\n";
        let graph = graph_with(node("log", NodeKind::ToolResponse, content, false));

        assert!(!CollapseLogs.supports(&graph));
        assert_eq!(
            CollapseLogs.execute_if_supported(&graph, &budget()),
            StrategyExecution::NotSupported
        );
        let output = CollapseLogs
            .execute(&graph, &budget())
            .expect("two repeats should be a no-op");
        assert_eq!(output.graph, graph);
        assert!(output.removals.is_empty());
        assert!(output.notes.iter().any(|note| note.contains("skipped")));
    }

    #[test]
    fn test_uuid_and_hex_normalization() {
        let content = (0_u64..10)
            .map(|value| {
                format!(
                    "request {:016x} mask=0x{:08x} done (id=550e8400-e29b-41d4-a716-{value:012x})",
                    0x3f2a_9c1d_4b5e_6f70_u64 + value,
                    0xdead_0000_u64 + value,
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        let graph = graph_with(node("log", NodeKind::ToolResponse, &content, false));

        let output = CollapseLogs
            .execute(&graph, &budget())
            .expect("UUID and hexadecimal differences should normalize");

        assert_eq!(output.graph.nodes[0].content.lines().count(), 1);
        assert!(output.graph.nodes[0].content.contains("[×10 repeats]"));
        assert!(!output.graph.nodes[0].content.ends_with('\n'));
        assert_eq!(output.removals[0].units_removed, Some(9));
    }

    #[test]
    fn test_multiline_node_and_java_trace_groups_collapse_with_typed_removals() {
        let node_first = concat!(
            "2026-07-21T11:02:14.512Z ERROR FetchError: connection refused\n",
            "    at Client.open (/srv/client.js:10:3)\n",
            "    at main (/srv/main.js:4:1)\n",
        );
        let java_first = concat!(
            "2026-07-21 11:03:02.117 ERROR [pool-3-thread-7] InvoiceWorker - failed\n",
            "java.lang.NullPointerException: customer is null\n",
            "    at com.acme.InvoiceWorker.applyTax(InvoiceWorker.java:214)\n",
        );
        let content = format!(
            "{node_first}{}{}{java_first}{}{}",
            concat!(
                "2026-07-21T11:02:19.518Z ERROR FetchError: connection refused\n",
                "    at Client.open (/srv/client.js:10:3)\n",
                "    at main (/srv/main.js:4:1)\n",
            ),
            concat!(
                "2026-07-21T11:02:24.531Z ERROR FetchError: connection refused\n",
                "    at Client.open (/srv/client.js:10:3)\n",
                "    at main (/srv/main.js:4:1)\n",
            ),
            concat!(
                "2026-07-21 11:03:07.204 ERROR [pool-3-thread-2] InvoiceWorker - failed\n",
                "java.lang.NullPointerException: customer is null\n",
                "    at com.acme.InvoiceWorker.applyTax(InvoiceWorker.java:214)\n",
            ),
            concat!(
                "2026-07-21 11:03:12.290 ERROR [pool-3-thread-4] InvoiceWorker - failed\n",
                "java.lang.NullPointerException: customer is null\n",
                "    at com.acme.InvoiceWorker.applyTax(InvoiceWorker.java:214)\n",
            ),
        );
        let graph = graph_with(node("log", NodeKind::ToolResponse, &content, false));

        let output = CollapseLogs
            .execute(&graph, &budget())
            .expect("multiline traces should collapse");
        let collapsed = &output.graph.nodes[0].content;

        assert!(output.graph.nodes[0].protected);
        assert!(collapsed.starts_with(node_first));
        assert!(collapsed.contains(java_first));
        assert_eq!(collapsed.matches("[×3 repeats]").count(), 2);
        assert!(collapsed.contains("FetchError: connection refused"));
        assert!(collapsed.contains("InvoiceWorker.applyTax(InvoiceWorker.java:214)"));
        assert_eq!(output.removals.len(), 2);
        assert_eq!(output.removals[0].kind, RemovalKind::CollapsedLines);
        assert_eq!(output.removals[0].locator.as_deref(), Some("lines:4-9"));
        assert_eq!(output.removals[0].units_removed, Some(5));
        assert_eq!(output.removals[1].locator.as_deref(), Some("lines:13-18"));
        assert_eq!(output.removals[1].units_removed, Some(5));
        assert_eq!(
            output.removals[0].after_tokens,
            output.removals[1].before_tokens
        );
        assert_eq!(
            output.removals[0].replacement_hash.as_deref(),
            Some(output.removals[1].original_hash.as_str())
        );
        let recorded_delta = output
            .removals
            .iter()
            .map(|record| record.before_tokens - record.after_tokens)
            .sum::<usize>();
        assert_eq!(
            recorded_delta,
            graph.total_tokens() - output.graph.total_tokens()
        );
    }

    #[test]
    fn test_trace_grouping_does_not_merge_distinct_root_causes_or_quoted_text() {
        let content = concat!(
            "2026-07-21T11:02:14.512Z ERROR FetchError: first root\n",
            "    at Client.open (/srv/client.js:10:3)\n",
            "2026-07-21T11:02:19.518Z ERROR FetchError: second root\n",
            "    at Client.open (/srv/client.js:10:3)\n",
            "2026-07-21T11:02:24.531Z ERROR FetchError: third root\n",
            "    at Client.open (/srv/client.js:10:3)\n",
            "\"2026-07-21T11:03:14.512Z ERROR FetchError: quoted\"\n",
            "\"    at Client.open (/srv/client.js:10:3)\"\n",
            "\"2026-07-21T11:03:19.518Z ERROR FetchError: quoted\"\n",
            "\"    at Client.open (/srv/client.js:10:3)\"\n",
            "\"2026-07-21T11:03:24.531Z ERROR FetchError: quoted\"\n",
            "\"    at Client.open (/srv/client.js:10:3)\"\n",
        );
        let graph = graph_with(node("log", NodeKind::ToolResponse, content, false));

        assert!(!CollapseLogs.supports(&graph));
        let output = CollapseLogs
            .execute(&graph, &budget())
            .expect("distinct and quoted traces should remain unchanged");
        assert_eq!(output.graph, graph);
        assert!(output.removals.is_empty());
    }

    #[test]
    fn test_protected_log_untouched() {
        let graph = graph_with(node(
            "protected-log",
            NodeKind::ToolResponse,
            FIVE_HUNDRED_LINES,
            true,
        ));

        assert!(!CollapseLogs.supports(&graph));
        let output = CollapseLogs
            .execute(&graph, &budget())
            .expect("protected log should be skipped");
        assert_eq!(output.graph, graph);
        assert!(output.notes.iter().any(|note| note.contains("skipped")));
    }

    #[test]
    fn test_inherently_protected_and_code_kinds_untouched() {
        for kind in [
            NodeKind::SystemInstruction,
            NodeKind::SourceFile,
            NodeKind::CodeSymbol,
        ] {
            let graph = graph_with(node("node", kind, FIVE_HUNDRED_LINES, false));
            assert!(!CollapseLogs.supports(&graph));
            let output = CollapseLogs
                .execute(&graph, &budget())
                .expect("protected and code kinds should be skipped");
            assert_eq!(output.graph, graph);
        }
    }

    #[test]
    fn test_interleaved_runs() {
        let content = ["A", "A", "A", "A", "A", "B", "A", "A", "A", "A"].join("\n");
        let graph = graph_with(node("log", NodeKind::ToolResponse, &content, false));

        let output = CollapseLogs
            .execute(&graph, &budget())
            .expect("interleaved runs should collapse independently");

        assert_eq!(
            output.graph.nodes[0].content,
            "A [×5 repeats]\nB\nA [×4 repeats]"
        );
        assert_eq!(output.removals.len(), 2);
        assert_eq!(output.removals[0].locator.as_deref(), Some("lines:1-5"));
        assert_eq!(output.removals[1].locator.as_deref(), Some("lines:7-10"));
        assert_eq!(
            output.removals[0].after_tokens,
            output.removals[1].before_tokens
        );
        assert_eq!(
            output.removals[0].replacement_hash.as_deref(),
            Some(output.removals[1].original_hash.as_str())
        );
        let recorded_delta = output
            .removals
            .iter()
            .map(|record| record.before_tokens - record.after_tokens)
            .sum::<usize>();
        assert_eq!(
            recorded_delta,
            graph.total_tokens() - output.graph.total_tokens()
        );
    }

    #[test]
    fn test_unprofitable_short_run_is_untouched() {
        let content = "ok\nok\nok";
        let graph = graph_with(node("log", NodeKind::ToolResponse, content, false));

        assert!(!CollapseLogs.supports(&graph));
        assert_eq!(CollapseLogs.estimate(&graph), Estimate::default());
        let output = CollapseLogs
            .execute(&graph, &budget())
            .expect("unprofitable collapse should be skipped");
        assert_eq!(output.graph, graph);
        assert!(output.notes.iter().any(|note| note.contains("skipped")));
    }

    #[test]
    fn test_lines_differing_only_in_a_duration_collapse() {
        let durations = ["3.01s", "4.2s", "950ms", "12s", "0.5ms", "120µs", "7ns"];
        let content = durations
            .iter()
            .map(|duration| format!("    Finished dev [unoptimized] target(s) in {duration}"))
            .collect::<Vec<_>>()
            .join("\n");
        let graph = graph_with(node("log", NodeKind::ToolResponse, &content, false));

        let output = CollapseLogs
            .execute(&graph, &budget())
            .expect("duration differences should normalize");

        let collapsed = &output.graph.nodes[0].content;
        assert_eq!(collapsed.lines().count(), 1);
        assert!(collapsed.contains("[×7 repeats]"));
        assert!(
            collapsed.starts_with("    Finished dev [unoptimized] target(s) in 3.01s"),
            "the first line is kept verbatim as the representative: {collapsed}"
        );
    }

    #[test]
    fn test_sizes_versions_and_identifiers_are_not_durations() {
        for content in [
            "heap limit 512m\nheap limit 256m\nheap limit 128m",
            "resolved pkg 1.2.3\nresolved pkg 1.2.4\nresolved pkg 1.2.5",
            "stage build1s\nstage build2s\nstage build3s",
            "shard 10.0.0.5s\nshard 10.0.0.6s\nshard 10.0.0.7s",
            "waited 5 s\nwaited 6 s\nwaited 7 s",
            "column 3sx\ncolumn 4sx\ncolumn 5sx",
        ] {
            let graph = graph_with(node("log", NodeKind::ToolResponse, content, false));

            let output = CollapseLogs
                .execute(&graph, &budget())
                .expect("non-duration fields must remain exact");

            assert_eq!(output.graph, graph, "collapsed: {content:?}");
        }
    }

    #[test]
    fn test_non_normalized_fields_remain_distinct() {
        let content = "request code=1234567\nrequest code=7654321\nrequest code=abcdefg";
        let graph = graph_with(node("log", NodeKind::ToolResponse, content, false));

        assert!(!CollapseLogs.supports(&graph));
        let output = CollapseLogs
            .execute(&graph, &budget())
            .expect("short and non-hex fields must remain exact");
        assert_eq!(output.graph, graph);
    }

    #[test]
    fn test_line_ending_of_collapsed_span_is_preserved() {
        let line = "a sufficiently long repeated log payload that saves tokens";
        let content = format!("{line}\r\n{line}\r\n{line}");
        let graph = graph_with(node("log", NodeKind::ToolResponse, &content, false));

        let output = CollapseLogs
            .execute(&graph, &budget())
            .expect("long CRLF run should collapse");

        assert_eq!(
            output.graph.nodes[0].content,
            format!("{line} [×3 repeats]")
        );
        assert!(!output.graph.nodes[0].content.ends_with('\n'));
    }

    #[test]
    fn test_safe_registry_uses_real_collapse_strategy() {
        let graph = graph_with(node(
            "log",
            NodeKind::ToolResponse,
            FIVE_HUNDRED_LINES,
            false,
        ));
        let registry = StrategyRegistry::safe_default(Mode::Safe);
        let collapse = &registry.strategies()[1];

        assert_eq!(collapse.id(), "collapse_logs");
        assert!(collapse.supports(&graph));
        assert!(collapse.execute(&graph, &budget()).is_ok());
    }

    #[test]
    fn test_budget_protected_kind_log_untouched() {
        let graph = graph_with(node(
            "budget-protected-log",
            NodeKind::ToolResponse,
            FIVE_HUNDRED_LINES,
            false,
        ));
        let mut protected_budget = budget();
        protected_budget
            .protected_kinds
            .push(NodeKind::ToolResponse);

        assert!(CollapseLogs.execute(&graph, &budget()).unwrap().graph != graph);
        let output = CollapseLogs.execute(&graph, &protected_budget).unwrap();
        assert_eq!(output.graph, graph);
        assert!(output.removals.is_empty());
        assert!(matches!(
            CollapseLogs.execute_if_supported(&graph, &protected_budget),
            StrategyExecution::NotSupported
        ));
    }
}
