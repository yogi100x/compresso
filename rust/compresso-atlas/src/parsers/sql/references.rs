//! SQL references (E17-42, D107): the tables, views, and routines a
//! statement names, stored as call sites (D89) so `trace --to <name>`
//! answers "what touches this table".
//!
//! Each reference is a plain call from the statement's definition symbol,
//! or from the file's `script` symbol for a statement that defines nothing,
//! to the bare name (lowercased unless quoted). The call site's qualifier
//! holds the reference kind (`alter`, `drop relation`, `from`, ...), which no
//! resolution rule reads for a plain call. Read from the leading words of a
//! statement (`ALTER TABLE x`, `DROP …`, `COMMENT ON …`, `GRANT … ON x`,
//! `INSERT INTO x`, `UPDATE x`, `DELETE FROM x`, `TRUNCATE x`, `COPY x`,
//! `REFRESH MATERIALIZED VIEW x`, `CALL f`, and a definition's `ON` table,
//! trigger function, or `PARTITION OF` parent) and, over every token, from
//! `REFERENCES x` anywhere and `FROM x` / `JOIN x` outside parentheses
//! (best effort: the first name only; a name followed by `(` is a function).
//! Dollar-quoted bodies are single tokens, so nothing inside a function body
//! is read.

use super::{Class, Definition, Head, Object, Token, TokenKind, identifier, is_word_in};

/// References kept per statement from the token stream.
const MAX_STREAM_REFERENCES: usize = 64;
/// Longest reference kind a call site stores (its qualifier).
const MAX_KIND_BYTES: usize = 256;
/// Longest member table kept in a drop's kind.
const MAX_DROP_TABLE_BYTES: usize = 96;

/// `text` cut to at most `bytes` bytes at a character boundary and marked
/// `…` when cut.
fn cut(text: &str, bytes: usize) -> String {
    if text.len() <= bytes {
        return text.to_owned();
    }
    let mut end = bytes.saturating_sub('…'.len_utf8());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

/// A name a statement references.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Found {
    pub(crate) name: String,
    /// The reference kind, stored as the call site's qualifier.
    pub(crate) kind: String,
    pub(crate) line: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Trigger {
    References,
    From,
    Join,
}

impl Trigger {
    const fn kind(self) -> &'static str {
        match self {
            Self::References => "references",
            Self::From => "from",
            Self::Join => "join",
        }
    }
}

#[derive(Debug)]
struct Pending {
    trigger: Trigger,
    /// The last part read so far: a reference is stored by its bare name.
    name: String,
    line: u32,
    awaiting_part: bool,
}

/// Reads `REFERENCES`, `FROM`, and `JOIN` references from a statement's
/// tokens as they are scanned; constant state per statement.
#[derive(Debug, Default)]
pub(crate) struct Stream {
    depth: u32,
    from_allowed: bool,
    references_allowed: bool,
    /// More references were met than are kept.
    pub(crate) cut: bool,
    after_distinct: bool,
    expect: Option<Trigger>,
    pending: Option<Pending>,
}

impl Stream {
    /// A stream for a statement starting with `first`: `GRANT`, `REVOKE`,
    /// and `COPY` use `FROM` for a role or a source, not a table.
    pub(crate) fn new(source: &str, first: Token) -> Self {
        let leading = &source[first.start..first.end];
        let leads = |words: &[&str]| {
            first.kind == TokenKind::Word
                && words.iter().any(|word| leading.eq_ignore_ascii_case(word))
        };
        Self {
            from_allowed: !leads(&["grant", "revoke", "copy"]),
            // In a grant, `REFERENCES` is a privilege.
            references_allowed: !leads(&["grant", "revoke"]),
            ..Self::default()
        }
    }

    pub(crate) fn feed(&mut self, source: &str, token: Token, found: &mut Vec<Found>) {
        if let Some(pending) = self.pending.as_mut() {
            if pending.awaiting_part {
                pending.awaiting_part = false;
                if let Some(part) = identifier(source, token) {
                    pending.name = part;
                    return;
                }
                self.finish(found, false);
            } else if token.kind == TokenKind::Punct(b'.') {
                pending.awaiting_part = true;
                return;
            } else {
                self.finish(found, token.kind == TokenKind::Punct(b'('));
            }
        }
        let text = &source[token.start..token.end];
        let is = |word: &str| token.kind == TokenKind::Word && text.eq_ignore_ascii_case(word);
        if let Some(trigger) = self.expect.take() {
            if is("only") || is("lateral") {
                self.expect = Some(trigger);
                return;
            }
            if !(is("select") || is("values") || is("stdin") || is("stdout"))
                && let Some(name) = identifier(source, token)
            {
                self.pending = Some(Pending {
                    trigger,
                    name,
                    line: token.line,
                    awaiting_part: false,
                });
                return;
            }
        }
        match token.kind {
            TokenKind::Punct(b'(') => self.depth = self.depth.saturating_add(1),
            TokenKind::Punct(b')') => self.depth = self.depth.saturating_sub(1),
            TokenKind::Word => {
                if is("references") && self.references_allowed {
                    self.expect = Some(Trigger::References);
                } else if self.depth == 0 && is("from") && self.from_allowed && !self.after_distinct
                {
                    self.expect = Some(Trigger::From);
                } else if self.depth == 0 && is("join") {
                    self.expect = Some(Trigger::Join);
                }
            }
            _ => {}
        }
        self.after_distinct = is("distinct");
    }

    /// Stores the pending name; `call` when a `(` follows it, which makes a
    /// `FROM` or `JOIN` name a function, not a table.
    pub(crate) fn finish(&mut self, found: &mut Vec<Found>, call: bool) {
        let Some(pending) = self.pending.take() else {
            return;
        };
        if (call && pending.trigger != Trigger::References)
            || found.iter().any(|kept| kept.name == pending.name)
        {
            return;
        }
        if found.len() >= MAX_STREAM_REFERENCES {
            self.cut = true;
            return;
        }
        found.push(Found {
            name: pending.name,
            kind: pending.trigger.kind().to_owned(),
            line: pending.line,
        });
    }
}

/// The references a statement's leading words name, and whether reading
/// them reached the end of the head (so a longer statement may name more).
pub(crate) fn head_references(source: &str, head: &[Token], class: &Class) -> (Vec<Found>, bool) {
    let mut reader = Reader {
        head: Head {
            source,
            tokens: head,
            at: 0,
        },
        found: Vec::new(),
    };
    match class {
        Class::Definition(definition) => reader.definition(definition),
        Class::Other => reader.other(),
        Class::Unnamed(Object::Index) => {
            // `CREATE INDEX ON t (…)`: no name, but a table.
            let head = &mut reader.head;
            head.at = head.find_top_level(0, |token, text| is_word_in(token, text, &["on"])) + 1;
            head.eat("only");
            reader.name("on");
        }
        Class::Unnamed(_) | Class::OtherCreate => {}
    }
    let ran_out = reader.head.at >= head.len();
    (reader.found, ran_out)
}

struct Reader<'a> {
    head: Head<'a>,
    found: Vec<Found>,
}

impl Reader<'_> {
    fn push(&mut self, name: String, kind: String, line: u32) {
        if !self.found.iter().any(|kept| kept.name == name) {
            self.found.push(Found { name, kind, line });
        }
    }

    /// The name at the cursor, by its last part; false when there is none.
    fn name(&mut self, kind: &str) -> bool {
        let line = self.head.line_at(self.head.at);
        let Some(mut parts) = self.head.name() else {
            return false;
        };
        let name = parts.pop().unwrap_or_default();
        self.push(name, kind.to_owned(), line);
        true
    }

    fn definition(&mut self, definition: &Definition) {
        if let Some((table, line)) = &definition.table {
            self.push(table.clone(), "on".to_owned(), *line);
        }
        let head = &mut self.head;
        match definition.object {
            Object::Trigger => {
                head.at =
                    head.find_top_level(0, |token, text| is_word_in(token, text, &["execute"])) + 1;
                if head.eat("function") || head.eat("procedure") {
                    self.name("execute");
                }
            }
            Object::Table | Object::ForeignTable => {
                head.at = head
                    .find_top_level(0, |token, text| is_word_in(token, text, &["partition"]))
                    + 1;
                if head.eat("of") {
                    self.name("partition_of");
                }
            }
            _ => {}
        }
    }

    fn other(&mut self) {
        let Some(first) = self.head.keyword(0) else {
            return;
        };
        self.head.at = 1;
        match first.as_str() {
            "alter" => self.alter(),
            "drop" => self.drop(),
            "comment" => self.comment(),
            "grant" | "revoke" => self.grant(&first),
            "insert" => {
                if self.head.eat("into") {
                    self.name("insert");
                }
            }
            "update" => {
                self.head.eat("only");
                self.name("update");
            }
            "delete" => {
                if self.head.eat("from") {
                    self.head.eat("only");
                    self.name("delete");
                }
            }
            "truncate" => {
                self.head.eat("table");
                self.list("truncate");
            }
            "copy" => {
                self.name("copy");
            }
            "refresh" => {
                self.head.eat("materialized");
                self.head.eat("view");
                self.head.eat("concurrently");
                self.name("refresh");
            }
            "call" => {
                self.name("call");
            }
            _ => {}
        }
    }

    /// The object word after `ALTER`, `DROP`, or `COMMENT ON` as a family.
    fn object_family(&mut self) -> Option<&'static str> {
        let mut event = false;
        loop {
            let word = self.head.keyword(self.head.at)?;
            self.head.at += 1;
            let family = match word.as_str() {
                "materialized" | "foreign" => continue,
                "event" => {
                    event = true;
                    continue;
                }
                "table" | "view" => "relation",
                "function" | "procedure" | "aggregate" | "routine" => "routine",
                "trigger" if event => "event_trigger",
                "trigger" => "trigger",
                "index" => "index",
                "type" | "domain" => "type",
                "policy" => "policy",
                "rule" => "rule",
                "schema" => "schema",
                "extension" => "extension",
                "sequence" => "sequence",
                "publication" => "publication",
                _ => return None,
            };
            return Some(family);
        }
    }

    /// Names separated by commas, each optionally followed by an argument
    /// list, up to `TO` or `FROM` (a grant's grantees).
    fn list(&mut self, kind: &str) {
        loop {
            if self.head.is("to") || self.head.is("from") {
                return;
            }
            self.head.eat("only");
            if !self.name(kind) {
                return;
            }
            if self.head.is_punct(self.head.at, b'(') {
                self.head.at = self.head.after_group(self.head.at);
            }
            if !self.head.is_punct(self.head.at, b',') {
                return;
            }
            self.head.at += 1;
        }
    }

    fn alter(&mut self) {
        let Some(family) = self.object_family() else {
            return;
        };
        self.head.eat_all(&["if", "exists"]);
        self.head.eat("only");
        match family {
            "relation" | "routine" => {
                self.name("alter");
            }
            "policy" | "trigger" | "rule" if self.head.name().is_some() && self.head.eat("on") => {
                self.head.eat("only");
                self.name("alter");
            }
            _ => {}
        }
    }

    /// `DROP <object> [IF EXISTS] name [(args)], … [ON table]`: each name is
    /// stored with its kind as `drop <family>`, then tab-separated, the name
    /// as written and the table as written for a trigger, policy, or rule.
    /// A written name too long for a call site is cut and marked `…`, so
    /// the drop is still recorded.
    fn drop(&mut self) {
        let Some(family) = self.object_family() else {
            return;
        };
        self.head.eat("concurrently");
        self.head.eat_all(&["if", "exists"]);
        let mut names = Vec::new();
        loop {
            let line = self.head.line_at(self.head.at);
            let Some((mut parts, written)) = self.head.name_written() else {
                break;
            };
            if self.head.is_punct(self.head.at, b'(') {
                self.head.at = self.head.after_group(self.head.at);
            }
            names.push((parts.pop().unwrap_or_default(), written, line));
            if !self.head.is_punct(self.head.at, b',') {
                break;
            }
            self.head.at += 1;
        }
        let mut table = String::new();
        if matches!(family, "policy" | "trigger" | "rule") && self.head.eat("on") {
            self.head.eat("only");
            if let Some((_, written)) = self.head.name_written() {
                table = cut(&written, MAX_DROP_TABLE_BYTES);
            }
        }
        let prefix = format!("drop {family}\t");
        let room = MAX_KIND_BYTES - prefix.len() - 1 - table.len();
        for (name, written, line) in names {
            let kind = format!("{prefix}{}\t{table}", cut(&written, room));
            self.push(name, kind, line);
        }
    }

    fn comment(&mut self) {
        if !self.head.eat("on") {
            return;
        }
        if self.head.eat("column") {
            let line = self.head.line_at(self.head.at);
            if let Some(mut parts) = self.head.name()
                && parts.len() >= 2
            {
                parts.pop();
                let table = parts.pop().unwrap_or_default();
                self.push(table, "comment".to_owned(), line);
            }
            return;
        }
        match self.object_family() {
            Some("relation" | "routine") => {
                self.name("comment");
            }
            Some("policy" | "trigger" | "rule")
                if self.head.name().is_some() && self.head.eat("on") =>
            {
                self.name("comment");
            }
            _ => {}
        }
    }

    fn grant(&mut self, kind: &str) {
        self.head.at = self
            .head
            .find_top_level(1, |token, text| is_word_in(token, text, &["on"]))
            + 1;
        if self.head.is("all") {
            return;
        }
        // Only tables, views, and routines are listed; `ON TABLE` may be
        // left out.
        match self.head.keyword(self.head.at).as_deref() {
            Some("table" | "function" | "procedure" | "routine") => self.head.at += 1,
            Some(
                "sequence" | "schema" | "database" | "domain" | "type" | "language" | "tablespace"
                | "foreign" | "large",
            ) => return,
            _ => {}
        }
        self.list(kind);
    }
}
