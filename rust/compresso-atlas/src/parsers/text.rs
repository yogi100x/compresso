use compresso_store::{SymbolKind, SymbolRecord};

use super::Extraction;

const MAX_TEXT_SYMBOLS: usize = 256;
const MAX_TEXT_BLOCK_LINES: usize = 40;
const MAX_TEXT_SIGNATURE_BYTES: usize = 1_024;

/// Extracts bounded heading sections or plain-text paragraphs. The input is
/// inert data: no markup directives, includes, links, or code blocks execute.
pub(super) fn extract(source: &str) -> Extraction {
    if source.is_empty() {
        return Extraction {
            symbols: Vec::new(),
            imports: Vec::new(),
            calls: Vec::new(),
            calls_truncated: false,
            symbols_truncated: false,
        };
    }

    let lines = source.lines().collect::<Vec<_>>();
    let headings = lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| heading_text(line).map(|heading| (index, heading)))
        .collect::<Vec<_>>();
    let mut symbols = Vec::new();

    if headings.is_empty() {
        append_plain_paragraphs(&lines, &mut symbols);
    } else {
        if headings[0].0 > 0 {
            append_range(&lines, 0, headings[0].0 - 1, None, &mut symbols);
        }
        for (position, (start, heading)) in headings.iter().enumerate() {
            if symbols.len() == MAX_TEXT_SYMBOLS {
                break;
            }
            let end = headings
                .get(position + 1)
                .map_or(lines.len() - 1, |(next, _)| next.saturating_sub(1));
            append_range(&lines, *start, end, Some(heading), &mut symbols);
        }
    }

    Extraction {
        symbols,
        imports: Vec::new(),
        calls: Vec::new(),
        calls_truncated: false,
        symbols_truncated: false,
    }
}

fn append_plain_paragraphs(lines: &[&str], symbols: &mut Vec<SymbolRecord>) {
    let mut start = None;
    for index in 0..=lines.len() {
        let blank = index == lines.len() || lines[index].trim().is_empty();
        match (start, blank) {
            (None, false) => start = Some(index),
            (Some(block_start), true) => {
                append_range(lines, block_start, index.saturating_sub(1), None, symbols);
                start = None;
                if symbols.len() == MAX_TEXT_SYMBOLS {
                    return;
                }
            }
            _ => {}
        }
    }
}

fn append_range(
    lines: &[&str],
    start: usize,
    end: usize,
    heading: Option<&str>,
    symbols: &mut Vec<SymbolRecord>,
) {
    if start > end || start >= lines.len() {
        return;
    }
    let mut chunk_start = start;
    while chunk_start <= end && symbols.len() < MAX_TEXT_SYMBOLS {
        let chunk_end = end.min(chunk_start.saturating_add(MAX_TEXT_BLOCK_LINES - 1));
        let start_line = line_number(chunk_start);
        let base_name = heading
            .filter(|_| chunk_start == start)
            .map(str::to_owned)
            .unwrap_or_else(|| format!("text@{start_line}"));
        let signature = normalized_signature(&lines[chunk_start..=chunk_end]);
        if !signature.is_empty() {
            symbols.push(SymbolRecord {
                name: base_name.clone(),
                qualified_name: base_name,
                kind: SymbolKind::Module,
                signature: Some(signature),
                start_line,
                end_line: line_number(chunk_end),
                token_estimate: 0,
            });
        }
        chunk_start = chunk_end.saturating_add(1);
    }
}

fn heading_text(line: &str) -> Option<&str> {
    let trimmed = line.trim();
    heading_after_marker(trimmed, '#').or_else(|| heading_after_marker(trimmed, '='))
}

fn heading_after_marker(value: &str, marker: char) -> Option<&str> {
    let marker_count = value
        .chars()
        .take_while(|character| *character == marker)
        .count();
    if !(1..=6).contains(&marker_count) {
        return None;
    }
    let rest = value.get(marker_count..)?;
    if !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let heading = rest.trim().trim_end_matches(marker).trim();
    (!heading.is_empty()).then_some(heading)
}

fn normalized_signature(lines: &[&str]) -> String {
    let normalized = lines
        .iter()
        .flat_map(|line| line.split_whitespace())
        .collect::<Vec<_>>()
        .join(" ");
    truncate_utf8(&normalized, MAX_TEXT_SIGNATURE_BYTES).to_owned()
}

fn truncate_utf8(value: &str, max_bytes: usize) -> &str {
    if value.len() <= max_bytes {
        return value;
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

fn line_number(index: usize) -> u32 {
    u32::try_from(index)
        .unwrap_or(u32::MAX - 1)
        .saturating_add(1)
}
