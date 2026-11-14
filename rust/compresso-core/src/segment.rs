use std::collections::BTreeSet;

use thiserror::Error;

use crate::graph::{ContextGraph, ContextNode, NodeKind};
use crate::tokens::counter_for;

const MAX_PLAIN_LINES: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SegmentError {
    #[error("node {node_id} contains an unbalanced fenced block")]
    UnbalancedFence { node_id: String },
    #[error("segmentation produced a duplicate node id: {node_id}")]
    DuplicateNodeId { node_id: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Span {
    start: usize,
    end: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TraceRepeatSpan {
    pub(crate) start_line: usize,
    pub(crate) representative_end_line: usize,
    pub(crate) end_line: usize,
    pub(crate) repeats: usize,
}

#[derive(Clone, Copy)]
struct Line<'a> {
    start: usize,
    end: usize,
    text: &'a str,
}

/// Losslessly segments edge-free generic-text ingress before budget packing.
///
/// Graphs with semantic edges or provider turns are already structured and are
/// returned unchanged. Every segmented node preserves byte order, source, kind,
/// and provider token accounting. E5's `compresso_compress` handler can use the
/// engine directly and reconstruct successful text by concatenating node content
/// in graph order; fallback continues to return the caller-owned graph unchanged.
///
/// A protected node is never segmented: splitting would replace its id with
/// derived segment ids, so the caller could no longer find the node it marked
/// as untouchable. It is returned whole, with its id, bytes, and metadata intact.
pub fn segment_graph(input: &ContextGraph) -> Result<ContextGraph, SegmentError> {
    segment_graph_with_protected_kinds(input, &[])
}

/// Segments like [`segment_graph`], additionally treating every node whose kind
/// is listed in `protected_kinds` (the budget's protection set) as protected.
pub(crate) fn segment_graph_with_protected_kinds(
    input: &ContextGraph,
    protected_kinds: &[NodeKind],
) -> Result<ContextGraph, SegmentError> {
    if !is_generic_text_graph(input) {
        return Ok(input.clone());
    }

    let mut nodes = Vec::new();
    for node in &input.nodes {
        if is_protected(node, protected_kinds) {
            nodes.push(node.clone());
        } else {
            nodes.extend(segment_node(node)?);
        }
    }

    let mut ids = BTreeSet::new();
    for node in &nodes {
        if !ids.insert(node.id.as_str()) {
            return Err(SegmentError::DuplicateNodeId {
                node_id: node.id.clone(),
            });
        }
    }

    Ok(ContextGraph {
        schema_version: input.schema_version,
        nodes,
        edges: Vec::new(),
    })
}

pub(crate) fn is_generic_text_graph(input: &ContextGraph) -> bool {
    input.nodes.len() == 1 && input.edges.is_empty() && input.nodes[0].provider_turn.is_none()
}

fn is_protected(node: &ContextNode, protected_kinds: &[NodeKind]) -> bool {
    node.protected || node.kind.requires_protection() || protected_kinds.contains(&node.kind)
}

// Callers must filter protected nodes out first; see `is_protected`.
fn segment_node(node: &ContextNode) -> Result<Vec<ContextNode>, SegmentError> {
    let spans = structural_spans(&node.content).map_err(|()| SegmentError::UnbalancedFence {
        node_id: node.id.clone(),
    })?;
    if spans.len() <= 1 {
        return Ok(vec![node.clone()]);
    }

    let meaningful = spans
        .iter()
        .enumerate()
        .filter(|(_, span)| !node.content[span.start..span.end].trim().is_empty())
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    let first_meaningful = meaningful.first().copied();
    let final_meaningful = meaningful.last().copied();
    let mut error_keys = BTreeSet::new();
    let counter = counter_for(node.token_provider);

    Ok(spans
        .into_iter()
        .enumerate()
        .map(|(index, span)| {
            let content = node.content[span.start..span.end].to_owned();
            let error_anchor = error_key(&content).is_some_and(|key| error_keys.insert(key));
            let structural_anchor =
                Some(index) == first_meaningful || Some(index) == final_meaningful || error_anchor;
            let mut segment = node.clone();
            segment.id = format!("{}::segment-{index:04}", node.id);
            segment.content = content;
            segment.token_count = counter.count(&segment.content);
            segment.protected = structural_anchor;
            if error_anchor {
                segment.priority = 1.0;
            }
            segment
        })
        .collect())
}

fn structural_spans(content: &str) -> Result<Vec<Span>, ()> {
    if content.is_empty() {
        return Ok(vec![Span { start: 0, end: 0 }]);
    }

    let lines = lines(content);
    let contains_fence = lines.iter().any(|line| fence_open(line.text).is_some());
    if contains_fence {
        return paragraph_and_fence_spans(content, &lines);
    }

    if let Some(spans) = json_value_spans(content) {
        return Ok(spans);
    }

    if let Some(spans) = log_spans(content, &lines) {
        return Ok(spans);
    }

    paragraph_and_fence_spans(content, &lines)
}

fn json_value_spans(content: &str) -> Option<Vec<Span>> {
    let mut stream = serde_json::Deserializer::from_str(content).into_iter::<serde_json::Value>();
    let mut start = 0_usize;
    let mut spans = Vec::new();

    while let Some(value) = stream.next() {
        value.ok()?;
        let mut end = stream.byte_offset();
        while content
            .as_bytes()
            .get(end)
            .is_some_and(u8::is_ascii_whitespace)
        {
            end += 1;
        }
        spans.push(Span { start, end });
        start = end;
    }

    if spans.is_empty() || start != content.len() {
        None
    } else {
        Some(spans)
    }
}

fn paragraph_and_fence_spans(content: &str, lines: &[Line<'_>]) -> Result<Vec<Span>, ()> {
    let mut spans = Vec::new();
    let mut segment_start = 0_usize;
    let mut has_nonblank = false;
    let mut index = 0_usize;

    while index < lines.len() {
        if let Some((marker, length)) = fence_open(lines[index].text) {
            if has_nonblank && segment_start < lines[index].start {
                push_plain_chunks(&mut spans, content, segment_start, lines[index].start);
                segment_start = lines[index].start;
            }
            let close = nested_fence_close(lines, index, marker, length).ok_or(())?;
            spans.push(Span {
                start: segment_start,
                end: lines[close].end,
            });
            segment_start = lines[close].end;
            has_nonblank = false;
            index = close + 1;
            continue;
        }

        if lines[index].text.trim().is_empty() && has_nonblank {
            let mut blank_end = index + 1;
            while blank_end < lines.len() && lines[blank_end].text.trim().is_empty() {
                blank_end += 1;
            }
            push_plain_chunks(&mut spans, content, segment_start, lines[blank_end - 1].end);
            segment_start = lines[blank_end - 1].end;
            has_nonblank = false;
            index = blank_end;
            continue;
        }

        has_nonblank |= !lines[index].text.trim().is_empty();
        index += 1;
    }

    if segment_start < content.len() {
        push_plain_chunks(&mut spans, content, segment_start, content.len());
    }
    if spans.is_empty() {
        spans.push(Span {
            start: 0,
            end: content.len(),
        });
    }
    Ok(spans)
}

fn push_plain_chunks(spans: &mut Vec<Span>, content: &str, start: usize, end: usize) {
    let chunk_lines = lines(&content[start..end]);
    if chunk_lines.len() <= MAX_PLAIN_LINES {
        spans.push(classified_span(content, start, end));
        return;
    }

    for chunk in chunk_lines.chunks(MAX_PLAIN_LINES) {
        spans.push(classified_span(
            content,
            start + chunk[0].start,
            start + chunk.last().expect("chunk is non-empty").end,
        ));
    }
}

fn classified_span(content: &str, start: usize, end: usize) -> Span {
    debug_assert!(end <= content.len());
    Span { start, end }
}

fn nested_fence_close(
    lines: &[Line<'_>],
    open: usize,
    marker: u8,
    open_length: usize,
) -> Option<usize> {
    let mut depth = 1_usize;
    for (index, line) in lines.iter().enumerate().skip(open + 1) {
        if fence_close(line.text, marker, open_length) {
            depth -= 1;
            if depth == 0 {
                return Some(index);
            }
        } else if fence_open(line.text).is_some() {
            depth += 1;
        }
    }
    None
}

fn log_spans(content: &str, lines: &[Line<'_>]) -> Option<Vec<Span>> {
    // Repeated multiline traces must remain in one unprotected node so
    // `collapse_logs` can replace whole trace occurrences rather than forcing
    // `pack_budget` to choose among individually segmented error blocks.
    if !repeated_trace_spans(content).is_empty() {
        return Some(vec![Span {
            start: 0,
            end: content.len(),
        }]);
    }

    let starts = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| is_log_start(line.text))
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    if starts.len() < 2 {
        return None;
    }

    let mut spans = Vec::with_capacity(starts.len());
    for (position, start_line) in starts.iter().copied().enumerate() {
        let start = if position == 0 {
            0
        } else {
            lines[start_line].start
        };
        let end = starts
            .get(position + 1)
            .map_or(content.len(), |next| lines[*next].start);
        spans.push(Span { start, end });
    }

    let single_line = spans.iter().all(|span| {
        content[span.start..span.end]
            .lines()
            .filter(|line| !line.trim().is_empty())
            .count()
            == 1
    });
    let same_signature = spans
        .iter()
        .filter_map(|span| first_nonempty_line(&content[span.start..span.end]))
        .map(normalize_log_header)
        .collect::<BTreeSet<_>>()
        .len()
        == 1;
    if single_line && same_signature {
        return Some(vec![Span {
            start: 0,
            end: content.len(),
        }]);
    }

    Some(spans)
}

pub(crate) fn repeated_trace_spans(content: &str) -> Vec<TraceRepeatSpan> {
    let content_lines = lines(content);
    let starts = content_lines
        .iter()
        .enumerate()
        .filter(|(_, line)| is_trace_header(line.text))
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    if starts.len() < 3 {
        return Vec::new();
    }

    let blocks = starts
        .iter()
        .copied()
        .enumerate()
        .map(|(position, start_line)| {
            let end_line = starts
                .get(position + 1)
                .copied()
                .unwrap_or(content_lines.len());
            let signature = trace_signature(&content_lines, start_line, end_line);
            (start_line, end_line, signature)
        })
        .collect::<Vec<_>>();

    let mut spans = Vec::new();
    let mut run_start = 0_usize;
    while run_start < blocks.len() {
        let Some(signature) = blocks[run_start].2.as_ref() else {
            run_start += 1;
            continue;
        };
        let mut run_end = run_start + 1;
        while run_end < blocks.len()
            && blocks[run_end]
                .2
                .as_ref()
                .is_some_and(|next| next == signature)
        {
            run_end += 1;
        }

        let repeats = run_end - run_start;
        if repeats >= 3 {
            spans.push(TraceRepeatSpan {
                start_line: blocks[run_start].0,
                representative_end_line: blocks[run_start].1,
                end_line: blocks[run_end - 1].1,
                repeats,
            });
        }
        run_start = run_end;
    }
    spans
}

fn is_trace_header(line: &str) -> bool {
    timestamp_prefix(line) && line.contains(" ERROR ")
}

fn trace_signature(lines: &[Line<'_>], start: usize, end: usize) -> Option<String> {
    let header = lines.get(start)?.text;
    if !is_trace_header(header) {
        return None;
    }

    let mut signature = normalize_java_worker(&normalize_log_header(header));
    let mut saw_exception = false;
    let mut saw_frame = false;
    for line in lines.get(start + 1..end)? {
        let value = line.text.trim();
        if value.is_empty() {
            continue;
        }

        let is_frame = line
            .text
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_whitespace)
            && value.starts_with("at ");
        let is_exception = !saw_exception
            && !saw_frame
            && (value.contains("Exception:") || value.contains("Error:"));
        if is_frame {
            saw_frame = true;
        } else if is_exception {
            saw_exception = true;
        } else {
            return None;
        }
        signature.push('\n');
        signature.push_str(line.text);
    }

    saw_frame.then_some(signature)
}

fn normalize_java_worker(header: &str) -> String {
    let Some(start) = header.find("[pool-") else {
        return header.to_owned();
    };
    let Some(relative_end) = header[start..].find(']') else {
        return header.to_owned();
    };
    let end = start + relative_end + 1;
    let worker = &header[start + 1..end - 1];
    let Some((pool, thread)) = worker
        .strip_prefix("pool-")
        .and_then(|value| value.split_once("-thread-"))
    else {
        return header.to_owned();
    };
    if pool.is_empty()
        || thread.is_empty()
        || !pool.bytes().all(|byte| byte.is_ascii_digit())
        || !thread.bytes().all(|byte| byte.is_ascii_digit())
    {
        return header.to_owned();
    }

    format!("{}[<java-worker>]{}", &header[..start], &header[end..])
}

fn lines(content: &str) -> Vec<Line<'_>> {
    let mut output = Vec::new();
    let mut start = 0_usize;
    for piece in content.split_inclusive('\n') {
        let end = start + piece.len();
        let without_newline = piece.strip_suffix('\n').unwrap_or(piece);
        let text = without_newline
            .strip_suffix('\r')
            .unwrap_or(without_newline);
        output.push(Line { start, end, text });
        start = end;
    }
    if start < content.len() {
        output.push(Line {
            start,
            end: content.len(),
            text: &content[start..],
        });
    }
    output
}

fn fence_open(line: &str) -> Option<(u8, usize)> {
    let bytes = line.as_bytes();
    let spaces = bytes.iter().take_while(|byte| **byte == b' ').count();
    if spaces > 3 {
        return None;
    }
    let marker = *bytes.get(spaces)?;
    if !matches!(marker, b'`' | b'~') {
        return None;
    }
    let length = bytes[spaces..]
        .iter()
        .take_while(|byte| **byte == marker)
        .count();
    (length >= 3).then_some((marker, length))
}

fn fence_close(line: &str, marker: u8, open_length: usize) -> bool {
    let bytes = line.as_bytes();
    let spaces = bytes.iter().take_while(|byte| **byte == b' ').count();
    if spaces > 3 || bytes.get(spaces) != Some(&marker) {
        return false;
    }
    let length = bytes[spaces..]
        .iter()
        .take_while(|byte| **byte == marker)
        .count();
    length >= open_length && bytes[spaces + length..].iter().all(u8::is_ascii_whitespace)
}

fn is_log_start(line: &str) -> bool {
    let value = line.trim_start();
    timestamp_prefix(value)
        || value.starts_with("error[")
        || value.starts_with("error:")
        || value.starts_with("warning:")
        || value.starts_with("FAIL ")
        || value.starts_with("PASS ")
        || value.starts_with("Test Suites:")
        || value.starts_with("Tests:")
        || value.starts_with("Snapshots:")
        || value.starts_with("Time:")
        || value.starts_with("Compiling ")
        || value.starts_with("Finished ")
        || value.starts_with("$ ")
}

fn timestamp_prefix(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() >= 10
        && bytes[..4].iter().all(u8::is_ascii_digit)
        && bytes[4] == b'-'
        && bytes[7] == b'-'
}

fn normalize_log_header(value: &str) -> String {
    let trimmed = value.trim();
    if timestamp_prefix(trimmed) {
        trimmed
            .find(" ERROR ")
            .or_else(|| trimmed.find(" WARN "))
            .map(|index| &trimmed[index + 1..])
            .or_else(|| {
                trimmed
                    .split_once(char::is_whitespace)
                    .map(|(_, rest)| rest)
            })
            .unwrap_or(trimmed)
            .to_owned()
    } else {
        trimmed.to_owned()
    }
}

fn error_key(content: &str) -> Option<String> {
    for line in content.lines() {
        let value = line.trim();
        let lower = value.to_ascii_lowercase();
        if let Some(index) = value.find(" ERROR ") {
            return Some(value[index + " ERROR ".len()..].to_owned());
        }
        if lower.starts_with("error[")
            || lower.starts_with("error:")
            || value.starts_with("FAIL ")
            || lower.contains("exception:")
        {
            return Some(value.to_owned());
        }
    }
    None
}

fn first_nonempty_line(content: &str) -> Option<&str> {
    content.lines().find(|line| !line.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use serde_json::Map;

    use super::*;
    use crate::graph::{ContextNode, Source};
    use crate::tokens::TokenProvider;

    fn graph(content: &str, kind: NodeKind) -> ContextGraph {
        ContextGraph {
            schema_version: 1,
            nodes: vec![ContextNode {
                id: "input".to_owned(),
                kind,
                content: content.to_owned(),
                source: Source::Client,
                token_count: content.len(),
                token_provider: TokenProvider::Heuristic,
                priority: 0.5,
                protected: kind.requires_protection(),
                recency: None,
                metadata: Map::new(),
                provider_turn: None,
            }],
            edges: Vec::new(),
        }
    }

    fn joined(graph: &ContextGraph) -> String {
        graph
            .nodes
            .iter()
            .map(|node| node.content.as_str())
            .collect()
    }

    #[test]
    fn test_paragraph_fence_and_json_boundaries_are_lossless_and_deterministic() {
        let content = "intro\n\n```json\n{\"inside\":true}\n```\n\n{\"outside\":true}\n\nfinal\n";
        let first = segment_graph(&graph(content, NodeKind::Summary)).unwrap();
        let second = segment_graph(&graph(content, NodeKind::Summary)).unwrap();

        assert_eq!(first, second);
        assert_eq!(joined(&first), content);
        assert_eq!(first.nodes.len(), 4);
        assert_eq!(first.nodes[1].content, "```json\n{\"inside\":true}\n```\n");
        assert_eq!(first.nodes[2].content, "\n{\"outside\":true}\n\n");
        assert!(first.nodes.last().unwrap().protected);
    }

    #[test]
    fn test_log_error_groups_retain_exact_boundaries_and_representative_anchor() {
        let content = "error[E1]: first\n  detail\nwarning: old\n  old detail\nerror[E1]: first\n  detail\nerror: final\n";
        let output = segment_graph(&graph(content, NodeKind::CompilerOutput)).unwrap();

        assert_eq!(joined(&output), content);
        assert_eq!(output.nodes.len(), 4);
        assert!(output.nodes[0].protected);
        assert!(!output.nodes[1].protected);
        assert!(!output.nodes[2].protected);
        assert!(output.nodes[3].protected);
    }

    #[test]
    fn test_repeated_multiline_traces_remain_one_lossless_collapse_unit() {
        let content = concat!(
            "2026-07-21T11:02:14.512Z ERROR FetchError: connection refused\n",
            "    at Client.open (/srv/client.js:10:3)\n",
            "    at main (/srv/main.js:4:1)\n",
            "2026-07-21T11:02:19.518Z ERROR FetchError: connection refused\n",
            "    at Client.open (/srv/client.js:10:3)\n",
            "    at main (/srv/main.js:4:1)\n",
            "2026-07-21T11:02:24.531Z ERROR FetchError: connection refused\n",
            "    at Client.open (/srv/client.js:10:3)\n",
            "    at main (/srv/main.js:4:1)\n",
        );

        let first = segment_graph(&graph(content, NodeKind::ToolResponse)).unwrap();
        let second = segment_graph(&graph(content, NodeKind::ToolResponse)).unwrap();

        assert_eq!(first, second);
        assert_eq!(joined(&first), content);
        assert_eq!(first.nodes.len(), 1);
        assert!(!first.nodes[0].protected);
    }

    #[test]
    fn test_quoted_trace_text_does_not_spoof_a_trace_group() {
        let content = concat!(
            "\"2026-07-21T11:02:14.512Z ERROR FetchError: quoted\"\n",
            "\"    at Client.open (/srv/client.js:10:3)\"\n\n",
            "\"2026-07-21T11:02:19.518Z ERROR FetchError: quoted\"\n",
            "\"    at Client.open (/srv/client.js:10:3)\"\n\n",
            "\"2026-07-21T11:02:24.531Z ERROR FetchError: quoted\"\n",
            "\"    at Client.open (/srv/client.js:10:3)\"\n",
        );

        let output = segment_graph(&graph(content, NodeKind::ToolResponse)).unwrap();

        assert_eq!(joined(&output), content);
        assert!(output.nodes.len() > 1);
    }

    #[test]
    fn test_adjacent_json_values_are_independent_lossless_segments() {
        let content = "{\"first\":1}\n[2,3]\ntrue\n";
        let output = segment_graph(&graph(content, NodeKind::Summary)).unwrap();

        assert_eq!(joined(&output), content);
        assert_eq!(output.nodes.len(), 3);
        assert_eq!(output.nodes[0].content, "{\"first\":1}\n");
        assert_eq!(output.nodes[1].content, "[2,3]\n");
        assert_eq!(output.nodes[2].content, "true\n");
    }

    #[test]
    fn test_large_source_is_chunked_with_first_and_final_anchors() {
        let content = (0..100)
            .map(|index| format!("export const value{index} = {index};\n"))
            .collect::<String>();
        let output = segment_graph(&graph(&content, NodeKind::SourceFile)).unwrap();

        assert_eq!(joined(&output), content);
        assert_eq!(output.nodes.len(), 4);
        assert!(output.nodes[0].protected);
        assert!(output.nodes[3].protected);
    }

    #[test]
    fn test_unbalanced_fence_is_rejected_without_partial_output() {
        let input = graph("before\n\n```rust\nfn main() {}\n", NodeKind::Summary);
        assert_eq!(
            segment_graph(&input),
            Err(SegmentError::UnbalancedFence {
                node_id: "input".to_owned()
            })
        );
    }

    #[test]
    fn test_multi_node_graph_is_already_structured_and_remains_unchanged() {
        let first = graph("first\n\nparagraph", NodeKind::Summary);
        let mut input = first.clone();
        let mut second = first.nodes[0].clone();
        second.id = "second".to_owned();
        input.nodes.push(second);

        assert_eq!(segment_graph(&input), Ok(input.clone()));
    }

    const SPLITTABLE_CONTENTS: [&str; 6] = [
        "a\n\nN",
        "a\r\n\r\nN",
        "before\n\n```rust\nfn main() {}\n```\n\nafter",
        "{\"a\":1}\n{\"b\":2}\n",
        "ERROR first failure\n  detail\nERROR second failure\n  detail\n",
        "before\n\n```rust\nfn main() {}\n",
    ];

    #[test]
    fn test_protected_node_is_never_segmented() {
        for content in SPLITTABLE_CONTENTS {
            let mut input = graph(content, NodeKind::Summary);
            input.nodes[0].protected = true;

            assert_eq!(segment_graph(&input), Ok(input.clone()), "{content:?}");
        }

        let large = "fn generated() {}\n".repeat(2_000);
        let mut input = graph(&large, NodeKind::SourceFile);
        input.nodes[0].protected = true;
        assert_eq!(segment_graph(&input), Ok(input.clone()));
    }

    #[test]
    fn test_inherently_protected_kind_is_never_segmented_even_if_flag_is_false() {
        for content in SPLITTABLE_CONTENTS {
            let mut input = graph(content, NodeKind::SystemInstruction);
            input.nodes[0].protected = false;

            assert_eq!(segment_graph(&input), Ok(input.clone()), "{content:?}");
        }
    }

    #[test]
    fn test_budget_protected_kind_is_never_segmented() {
        let input = graph("a\n\nN", NodeKind::ToolResponse);

        assert_eq!(
            segment_graph_with_protected_kinds(&input, &[NodeKind::ToolResponse]),
            Ok(input.clone())
        );
        let unprotected = segment_graph_with_protected_kinds(&input, &[NodeKind::Summary]).unwrap();
        assert_eq!(unprotected.nodes.len(), 2);
        assert_eq!(joined(&unprotected), "a\n\nN");
    }
}
