//! A counts-only audit of the SQL scanner over a list of files (E17-42).
//!
//! Ignored by default. Run with `COMPRESSO_SQL_AUDIT_LIST` naming a file of
//! absolute paths, one per line; it prints counts and fixed keyword labels
//! only, never file names or file content, so it can be run over private
//! repositories. It compares the scanner's `CREATE` definitions with an
//! independent line-start regular expression and attributes every
//! difference to a cause.

use std::collections::BTreeMap;

use super::{
    Class, Object, Region, Sink, Token, TokenKind, classify, each_statement, extract, scan,
};

const LEADING: &[&str] = &[
    "alter",
    "drop",
    "insert",
    "update",
    "delete",
    "select",
    "with",
    "grant",
    "revoke",
    "comment",
    "begin",
    "commit",
    "rollback",
    "end",
    "set",
    "reset",
    "do",
    "copy",
    "create",
    "truncate",
    "analyze",
    "vacuum",
    "lock",
    "savepoint",
    "release",
    "notify",
    "listen",
    "refresh",
    "reindex",
    "cluster",
    "call",
    "perform",
    "execute",
    "prepare",
    "deallocate",
    "declare",
    "explain",
    "security",
    "import",
    "values",
    "show",
    "start",
    "discard",
    "load",
    "abort",
    "checkpoint",
    "merge",
    "return",
    "if",
    "raise",
];

const CREATE_OBJECTS: &[&str] = &[
    "role",
    "user",
    "group",
    "cast",
    "operator",
    "collation",
    "conversion",
    "language",
    "server",
    "database",
    "tablespace",
    "subscription",
    "statistics",
    "transform",
    "access",
    "text",
    "default",
    "or",
    "unique",
    "temp",
    "temporary",
];

fn family(object: Object) -> &'static str {
    match object {
        Object::Table | Object::ForeignTable => "table",
        Object::View | Object::MaterializedView => "view",
        Object::Function => "function",
        Object::Procedure => "procedure",
        Object::Aggregate => "aggregate",
        Object::Trigger | Object::EventTrigger => "trigger",
        Object::Index => "index",
        Object::Enum | Object::Type => "type",
        Object::Domain => "domain",
        Object::Policy => "policy",
        Object::Rule => "rule",
        Object::Schema => "schema",
        Object::Extension => "extension",
        Object::Sequence => "sequence",
        Object::Publication => "publication",
    }
}

/// The regex family of a stored definition, read from its signature.
fn regex_family(signature: &str) -> Option<&'static str> {
    let mut found = None;
    each_statement(signature, |statement| {
        if found.is_none()
            && let Class::Definition(definition) = classify(signature, statement)
        {
            found = Some(family(definition.object));
        }
    });
    found
}

fn label(word: &str, allowed: &[&str]) -> String {
    let lower = word.to_ascii_lowercase();
    if allowed.contains(&lower.as_str()) {
        lower
    } else {
        "other".to_owned()
    }
}

#[derive(Default)]
struct Regions {
    /// Non-overlapping `(start, end, label)` in order.
    spans: Vec<(usize, usize, &'static str)>,
    unclosed: BTreeMap<&'static str, usize>,
}

impl Sink for Regions {
    fn token(&mut self, token: Token, _line: u32) {
        let label = match token.kind {
            TokenKind::Dollar => "dollar body",
            TokenKind::String => "string",
            TokenKind::Quoted => "quoted identifier",
            TokenKind::Meta => "psql meta-command",
            _ => return,
        };
        if !token.closed {
            *self.unclosed.entry(label).or_default() += 1;
        }
        self.spans.push((token.start, token.end, label));
    }

    fn end(&mut self, _end: usize, _line: u32, _terminated: bool) {}

    fn skipped(&mut self, region: Region, start: usize, end: usize) {
        let label = match region {
            Region::LineComment => "line comment",
            Region::BlockComment { closed } => {
                if !closed {
                    *self.unclosed.entry("block comment").or_default() += 1;
                }
                "block comment"
            }
            Region::CopyData { closed } => {
                if !closed {
                    *self.unclosed.entry("copy data").or_default() += 1;
                }
                "copy data"
            }
        };
        self.spans.push((start, end, label));
    }
}

fn containing(spans: &[(usize, usize, &'static str)], offset: usize) -> Option<&'static str> {
    let index = spans.partition_point(|(start, _, _)| *start <= offset);
    let (start, end, label) = *spans.get(index.checked_sub(1)?)?;
    (start <= offset && offset < end).then_some(label)
}

#[derive(Default)]
struct Totals {
    counts: BTreeMap<String, usize>,
}

impl Totals {
    fn add(&mut self, key: impl Into<String>, amount: usize) {
        *self.counts.entry(key.into()).or_default() += amount;
    }

    fn print(&self) {
        for (key, count) in &self.counts {
            println!("{key}\t{count}");
        }
    }
}

#[test]
#[ignore = "audit: needs COMPRESSO_SQL_AUDIT_LIST"]
fn test_audit_sql_files_from_a_list() {
    let Some(list) = std::env::var_os("COMPRESSO_SQL_AUDIT_LIST") else {
        return;
    };
    let pattern = regex::Regex::new(
        r"(?im)^[ \t]*(create)\s+(?:or\s+replace\s+)?(?:\w+\s+){0,2}?(table|view|function|procedure|aggregate|trigger|index|type|domain|policy|rule|schema|extension|sequence|publication)\b",
    )
    .expect("pattern");
    let mut totals = Totals::default();
    let paths = std::fs::read_to_string(list).expect("list");
    for path in paths.lines().filter(|line| !line.is_empty()) {
        let Ok(source) = std::fs::read_to_string(path) else {
            totals.add("files unreadable as UTF-8", 1);
            continue;
        };
        totals.add("files", 1);
        totals.add("bytes", source.len());
        totals.add("lines", source.lines().count());

        let mut regions = Regions::default();
        scan(&source, &mut regions);
        for (kind, count) in &regions.unclosed {
            totals.add(format!("self-check: unclosed {kind}"), *count);
        }
        if !regions.unclosed.is_empty() {
            totals.add("self-check: files with an unclosed construct", 1);
        }

        let mut starts = BTreeMap::new();
        let mut ranges = Vec::new();
        let mut definitions = 0_usize;
        let mut unterminated = Vec::new();
        let mut last_start = 0;
        each_statement(&source, |statement| {
            totals.add("statements", 1);
            let lines = statement.end_line - statement.start_line + 1;
            let has_body = statement
                .head
                .iter()
                .any(|token| token.kind == TokenKind::Dollar);
            let first_word = statement.head.first().map_or("(none)".to_owned(), |token| {
                if token.kind == TokenKind::Word {
                    label(&source[token.start..token.end], LEADING)
                } else {
                    "(non-word)".to_owned()
                }
            });
            if lines > 200 {
                let body = if has_body {
                    "with a dollar body"
                } else {
                    "no dollar body"
                };
                totals.add(
                    format!("self-check: statements over 200 lines ({body}, {first_word})"),
                    1,
                );
            }
            if !statement.terminated {
                let span = match lines {
                    1 => "1 line",
                    2..=10 => "2-10 lines",
                    _ => "over 10 lines",
                };
                unterminated.push((statement.start, format!("{first_word}, {span}")));
            }
            if !statement.closed {
                totals.add("self-check: statements with an unclosed token", 1);
            }
            let first = statement.head.first().copied();
            let class = classify(&source, statement);
            match &class {
                Class::Definition(definition) => {
                    definitions += 1;
                    totals.add(format!("definition: {:?}", definition.object), 1);
                }
                Class::Unnamed(object) => totals.add(format!("unnamed: {object:?}"), 1),
                Class::OtherCreate => {
                    let word = statement
                        .head
                        .get(1)
                        .filter(|token| token.kind == TokenKind::Word)
                        .map_or("(non-word)".to_owned(), |token| {
                            label(&source[token.start..token.end], CREATE_OBJECTS)
                        });
                    totals.add(format!("unclassified create: {word}"), 1);
                }
                Class::Other => {
                    let word = match first {
                        Some(token) if token.kind == TokenKind::Word => {
                            label(&source[token.start..token.end], LEADING)
                        }
                        Some(token) if token.kind == TokenKind::Meta => {
                            "psql meta-command".to_owned()
                        }
                        _ => "(non-word)".to_owned(),
                    };
                    totals.add(format!("other statement: {word}"), 1);
                }
            }
            let container = match &class {
                Class::Definition(definition) => format!("{first_word} {:?}", definition.object),
                _ => first_word,
            };
            ranges.push((statement.start, statement.end, container, lines));
            starts.insert(statement.start, class);
            last_start = statement.start;
        });
        for (start, what) in unterminated {
            let cause = if start == last_start {
                format!(
                    "ended by end of file ({what}, file ends with ';': {})",
                    source.trim_end().ends_with(';')
                )
            } else {
                format!("ended by a psql meta-command ({what})")
            };
            totals.add(format!("self-check: statements {cause}"), 1);
        }
        let extraction = extract(&source);
        for call in &extraction.calls {
            let kind = call.qualifier.as_deref().unwrap_or_default();
            let kind = kind.split('\t').next().unwrap_or(kind);
            let caller = if extraction.symbols[call.caller].qualified_name == super::FILE_SYMBOL {
                "file"
            } else {
                "definition"
            };
            totals.add(format!("reference: {kind} (from {caller})"), 1);
        }
        if extraction.calls_truncated {
            totals.add("files whose references hit the call-site cap", 1);
        }
        let kept = extraction.symbols;
        for symbol in &kept {
            totals.add(format!("symbol kind: {:?}", symbol.kind), 1);
        }
        if kept.len().saturating_sub(1) < definitions {
            totals.add("files whose definitions hit a symbol cap", 1);
        }

        // The regex against the symbols extraction keeps (top-level
        // statements and top-level `DO` bodies), by line and object family.
        let line_of = |offset: usize| {
            source[..offset]
                .bytes()
                .filter(|byte| *byte == b'\n')
                .count()
                + 1
        };
        let mut by_line = BTreeMap::<(usize, &'static str), usize>::new();
        for symbol in kept.iter().skip(1) {
            if let Some(family) = symbol.signature.as_deref().and_then(regex_family) {
                *by_line
                    .entry((symbol.start_line as usize, family))
                    .or_default() += 1;
            } else {
                let signature = symbol.signature.as_deref().unwrap_or_default();
                let quotes = signature.matches('\'').count() % 2;
                totals.add(
                    format!(
                        "self-check: signature does not classify ({:?}, cut: {}, odd quotes: {}, dollar: {})",
                        symbol.kind,
                        signature.ends_with('…'),
                        quotes,
                        signature.contains('$')
                    ),
                    1,
                );
            }
        }
        for captures in pattern.captures_iter(&source) {
            let offset = captures.get(1).expect("create").start();
            let word = captures
                .get(2)
                .expect("object")
                .as_str()
                .to_ascii_lowercase();
            totals.add(format!("regex: {word}"), 1);
            let line = line_of(offset);
            if let Some((_, count)) = by_line
                .iter_mut()
                .find(|((at, family), count)| *at == line && *family == word && **count > 0)
            {
                *count -= 1;
                continue;
            }
            let containing_statement = ranges
                .iter()
                .find(|(start, end, _, _)| *start <= offset && offset < *end);
            let cause = match (containing(&regions.spans, offset), containing_statement) {
                (Some("dollar body"), Some((_, _, first, _))) if first == "do" => {
                    "inside a DO body, not read as a definition".to_owned()
                }
                (Some("dollar body"), _) => "inside a routine body".to_owned(),
                (Some(region), _) => format!("inside a {region}"),
                (None, Some((start, _, first, lines))) if *start < offset => {
                    format!("inside another statement ({first}, {lines} lines)")
                }
                (None, Some(_)) => "scanner: not a recognised definition".to_owned(),
                (None, None) => "elsewhere".to_owned(),
            };
            totals.add(format!("regex only ({word}): {cause}"), 1);
        }
        for ((_, family), count) in by_line {
            if count > 0 {
                totals.add(format!("scanner only ({family})"), count);
            }
        }
    }
    totals.print();
}
