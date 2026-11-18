use std::collections::BTreeSet;

use compresso_store::{SymbolKind, SymbolRecord};

use super::Extraction;

const MAX_SHELL_FUNCTIONS: usize = 256;
const MAX_SHELL_FUNCTION_LINES: usize = 400;
const MAX_SCRIPT_SIGNATURE_BYTES: usize = 1_024;

/// Extracts a deliberately small structural shell model without invoking a
/// shell, expanding variables, resolving commands, or reading referenced files.
pub(super) fn extract(source: &str) -> Extraction {
    if source.is_empty() {
        return Extraction {
            symbols: Vec::new(),
            imports: Vec::new(),
            calls: Vec::new(),
        };
    }

    let lines = source.lines().collect::<Vec<_>>();
    let mut symbols = vec![SymbolRecord {
        name: "script".to_owned(),
        qualified_name: "script".to_owned(),
        kind: SymbolKind::Module,
        signature: script_signature(&lines),
        start_line: 1,
        end_line: u32::try_from(lines.len()).unwrap_or(u32::MAX),
        token_estimate: 0,
    }];

    for (index, line) in lines.iter().enumerate() {
        if symbols.len() > MAX_SHELL_FUNCTIONS {
            break;
        }
        let Some(name) = function_name(line) else {
            continue;
        };
        let end_index = function_end(&lines, index);
        symbols.push(SymbolRecord {
            name: name.to_owned(),
            qualified_name: name.to_owned(),
            kind: SymbolKind::Function,
            signature: Some(function_signature(line, name)),
            start_line: line_number(index),
            end_line: line_number(end_index),
            token_estimate: 0,
        });
    }

    Extraction {
        symbols,
        imports: Vec::new(),
        calls: Vec::new(),
    }
}

fn script_signature(lines: &[&str]) -> Option<String> {
    let mut parts = Vec::new();
    let mut identifiers = BTreeSet::new();
    for line in lines {
        let trimmed = line.trim();
        if let Some(comment) = trimmed.strip_prefix('#')
            && !trimmed.starts_with("#!")
        {
            let comment = comment.trim();
            if !comment.is_empty() {
                parts.push(comment.to_owned());
            }
        }
        if let Some(identifier) = assignment_name(trimmed) {
            identifiers.insert(identifier.to_owned());
        }
        if let Some(identifier) = function_name(trimmed) {
            identifiers.insert(identifier.to_owned());
        }
    }
    parts.extend(identifiers);
    let normalized = parts.join(" ");
    let signature = truncate_utf8(normalized.trim(), MAX_SCRIPT_SIGNATURE_BYTES);
    (!signature.is_empty()).then_some(signature.to_owned())
}

fn assignment_name(line: &str) -> Option<&str> {
    let mut candidate = line.trim_start();
    while let Some((word, rest)) = split_word(candidate) {
        if matches!(
            word,
            "export" | "readonly" | "declare" | "typeset" | "local"
        ) {
            candidate = rest.trim_start();
            if matches!(word, "declare" | "typeset") {
                while candidate.starts_with('-') {
                    let (_, rest) = split_word(candidate)?;
                    candidate = rest.trim_start();
                }
            }
            continue;
        }
        break;
    }

    let equals = candidate.find('=')?;
    let name = candidate[..equals].trim();
    is_shell_name(name).then_some(name)
}

fn split_word(value: &str) -> Option<(&str, &str)> {
    let end = value.find(char::is_whitespace).unwrap_or(value.len());
    (end > 0).then_some((&value[..end], &value[end..]))
}

fn function_name(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    let declaration = if let Some(rest) = trimmed.strip_prefix("function") {
        if !rest.starts_with(char::is_whitespace) {
            return None;
        }
        rest.trim_start()
    } else {
        trimmed
    };
    let name_end = declaration
        .find(|character: char| character == '(' || character.is_whitespace())
        .unwrap_or(declaration.len());
    let name = &declaration[..name_end];
    if !is_shell_name(name) {
        return None;
    }
    let rest = declaration[name_end..].trim_start();
    let rest = if let Some(rest) = rest.strip_prefix("()") {
        rest.trim_start()
    } else if declaration.as_ptr() == trimmed.as_ptr() {
        return None;
    } else {
        rest
    };
    (rest.starts_with('{') || rest.is_empty()).then_some(name)
}

fn is_shell_name(value: &str) -> bool {
    let mut bytes = value.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn function_signature(line: &str, name: &str) -> String {
    let declaration = line
        .split_once('{')
        .map_or(line.trim(), |(prefix, _)| prefix.trim());
    if declaration.is_empty() {
        format!("{name}()")
    } else {
        format!("{declaration} {{")
    }
}

fn function_end(lines: &[&str], start: usize) -> usize {
    let last = lines
        .len()
        .saturating_sub(1)
        .min(start.saturating_add(MAX_SHELL_FUNCTION_LINES - 1));
    let mut depth = 0_i32;
    let mut saw_open = false;
    for (index, line) in lines.iter().enumerate().take(last + 1).skip(start) {
        let (opens, closes) = brace_counts(line);
        if opens > 0 {
            saw_open = true;
        }
        depth = depth.saturating_add(opens).saturating_sub(closes);
        if saw_open && depth <= 0 {
            return index;
        }
    }
    last
}

fn brace_counts(line: &str) -> (i32, i32) {
    let mut single_quoted = false;
    let mut double_quoted = false;
    let mut escaped = false;
    let mut opens = 0_i32;
    let mut closes = 0_i32;

    for byte in line.bytes() {
        if escaped {
            escaped = false;
            continue;
        }
        if byte == b'\\' && !single_quoted {
            escaped = true;
            continue;
        }
        if byte == b'\'' && !double_quoted {
            single_quoted = !single_quoted;
            continue;
        }
        if byte == b'"' && !single_quoted {
            double_quoted = !double_quoted;
            continue;
        }
        if byte == b'#' && !single_quoted && !double_quoted {
            break;
        }
        if !single_quoted && !double_quoted {
            match byte {
                b'{' => opens += 1,
                b'}' => closes += 1,
                _ => {}
            }
        }
    }
    (opens, closes)
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
