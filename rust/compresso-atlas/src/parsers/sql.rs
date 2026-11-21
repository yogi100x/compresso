//! SQL definitions through a hand-written statement scanner (E17-42, D107).
//!
//! One forward pass over the bytes splits a file into statements the way
//! `psql` does: a `;` ends a statement unless it sits inside a `--` comment,
//! a nested `/* */` comment, a `'…'` string (`''` doubles a quote; a
//! backslash escapes only in an `E'…'` string), a `"…"` or `` `…` ``
//! identifier, a `$$…$$` or `$tag$…$tag$` body, parentheses, or a
//! `BEGIN … END` block of a `CREATE [OR REPLACE] FUNCTION | PROCEDURE |
//! TRIGGER` statement (`BEGIN ATOMIC` bodies; `CASE … END` nests inside one).
//! A psql meta-command (`\connect db`) ends at its newline, and the data
//! lines after `COPY … FROM stdin;` are skipped up to the `\.` line.
//!
//! Nothing recurses on the input and nothing is scanned again, except the
//! body of a top-level `DO` statement, which extraction scans once more for
//! the definitions inside it: an unterminated string, identifier, comment, or
//! body runs to the end of the file. No dialect can fail a file: a statement
//! the classifier does not recognise is simply not a symbol. The scanner's
//! input is bounded by SQL's per-file ceiling
//! ([`super::languages::SQL_MAX_FILE_BYTES`]).

use compresso_store::{SymbolKind, SymbolRecord};

use compresso_store::{CallForm, CallSiteRecord};

use super::{Extraction, MAX_SIGNATURE_BYTES, marked_signature};
use references::{Found, Stream};

mod references;

/// Definitions kept per file; later definitions of a larger file are not
/// symbols.
pub(crate) const MAX_SQL_SYMBOLS: usize = 4_096;
/// Bytes of whole lines all symbols of a file may cover together. The
/// indexer counts the tokens of every symbol's lines, so thousands of
/// statements on one megabyte-long line would otherwise be counted once per
/// statement; ordinary files cover each line once, far below this.
pub(crate) const MAX_SQL_COVERED_BYTES: usize = 16 * 1024 * 1024;
/// Significant tokens kept from the start of each statement for
/// classification and signatures; later tokens are only counted.
const MAX_HEAD_TOKENS: usize = 256;
/// Longest name, qualified or not, kept as a symbol.
const MAX_NAME_BYTES: usize = 1_024;
/// Signature text built before the bound cuts it (see
/// [`super::signature_prefix`]).
const SIGNATURE_KEPT_BYTES: usize = MAX_SIGNATURE_BYTES + 8;

#[cfg(test)]
thread_local! {
    /// Bytes the scanner stepped over or searched, on this thread (tests
    /// only): pins that scanning is linear without timing it.
    static SCAN_STEPS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// Bytes copied into signatures, on this thread (tests only).
    static SIGNATURE_BYTES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn count_steps(bytes: usize) {
    #[cfg(test)]
    SCAN_STEPS.with(|steps| steps.set(steps.get().saturating_add(bytes)));
    #[cfg(not(test))]
    let _ = bytes;
}

fn count_signature_bytes(bytes: usize) {
    #[cfg(test)]
    SIGNATURE_BYTES.with(|copied| copied.set(copied.get().saturating_add(bytes)));
    #[cfg(not(test))]
    let _ = bytes;
}

/// What a significant token is.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TokenKind {
    /// An unquoted keyword or identifier (`$` allowed after the first byte).
    Word,
    /// A `"…"` or `` `…` `` identifier.
    Quoted,
    /// A `'…'` string (an `E` prefix is the word before it).
    String,
    /// A `$tag$…$tag$` body.
    Dollar,
    /// A number or a `$1` parameter.
    Number,
    /// One punctuation byte.
    Punct(u8),
    /// A psql meta-command line.
    Meta,
}

/// A significant token: never whitespace or comment text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Token {
    pub(crate) kind: TokenKind,
    pub(crate) start: usize,
    pub(crate) end: usize,
    /// False when a string, identifier, or body runs unclosed to the end of
    /// the file.
    pub(crate) closed: bool,
    /// The line the token starts on.
    pub(crate) line: u32,
}

/// Text the scanner passes over without tokens.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Region {
    LineComment,
    BlockComment { closed: bool },
    CopyData { closed: bool },
}

/// Receives the scan.
pub(crate) trait Sink {
    /// A significant token of the current statement, starting on `line`.
    fn token(&mut self, token: Token, line: u32);
    /// The current statement ends at byte `end` on `line`; `terminated`
    /// when a `;` or a newline (meta-command) ended it, not the file's end.
    fn end(&mut self, end: usize, line: u32, terminated: bool);
    /// Bytes passed over as comment text or COPY data.
    fn skipped(&mut self, _region: Region, _start: usize, _end: usize) {}
}

/// Scans `source` once, front to back.
pub(crate) fn scan(source: &str, sink: &mut impl Sink) {
    Scanner {
        source,
        bytes: source.as_bytes(),
        // A UTF-8 byte order mark is not text.
        at: if source.starts_with('\u{feff}') { 3 } else { 0 },
        line: 1,
        open: false,
        last_end: 0,
        last_line: 1,
        last_word_is_e: false,
        paren: 0,
        block: Block::default(),
        copy: CopyState::None,
    }
    .run(sink);
}

/// `BEGIN … END` tracking of a routine or trigger header, after psql: the
/// first letters of the statement's first four words say whether it is
/// `CREATE [OR REPLACE] FUNCTION | PROCEDURE | TRIGGER`.
#[derive(Clone, Copy, Debug, Default)]
struct Block {
    words: u32,
    letters: [u8; 4],
    depth: u32,
    /// An `END` was read inside a block; the next word decides whether it
    /// closes one (`END`, `END CASE`) or an unmatched opener (`END IF`).
    pending_end: bool,
    /// The previous word was `EACH`.
    after_each: bool,
    /// A trigger header has reached its `ON table` (or `FOR EACH ROW |
    /// STATEMENT`), after which a `BEGIN` opens its body: a column named
    /// `begin` in `UPDATE OF begin` comes before it and opens none.
    trigger_body: bool,
}

impl Block {
    fn is_routine(&self) -> bool {
        let [first, second, third, fourth] = self.letters;
        first == b'c'
            && (matches!(second, b'f' | b'p' | b't')
                || (second == b'o' && third == b'r' && matches!(fourth, b'f' | b'p' | b't')))
    }

    fn is_trigger(&self) -> bool {
        let [_, second, third, fourth] = self.letters;
        second == b't' || (second == b'o' && third == b'r' && fourth == b't')
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CopyState {
    None,
    Copy,
    From,
    Stdin,
}

struct Scanner<'s> {
    source: &'s str,
    bytes: &'s [u8],
    at: usize,
    line: u32,
    /// The current statement has a token.
    open: bool,
    last_end: usize,
    last_line: u32,
    last_word_is_e: bool,
    paren: u32,
    block: Block,
    copy: CopyState,
}

impl Scanner<'_> {
    fn run(mut self, sink: &mut impl Sink) {
        while self.at < self.bytes.len() {
            count_steps(1);
            let byte = self.bytes[self.at];
            match byte {
                b'\n' => {
                    self.line = self.line.saturating_add(1);
                    self.at += 1;
                }
                b' ' | b'\t' | b'\r' | 0x0b | 0x0c => self.at += 1,
                b'-' if self.peek(1) == Some(b'-') => self.line_comment(sink),
                b'/' if self.peek(1) == Some(b'*') => self.block_comment(sink),
                b'\\' => self.meta_command(sink),
                b'\'' => self.string(sink),
                b'"' | b'`' => self.quoted(byte, sink),
                b'$' => self.dollar(sink),
                b';' => self.semicolon(sink),
                b'0'..=b'9' => self.number(sink),
                _ if is_word_start(byte) => self.word(sink),
                _ => self.punct(byte, sink),
            }
        }
        if self.open {
            let (end, line) = (self.last_end, self.last_line);
            self.finish(sink, end, line, false);
        }
    }

    fn peek(&self, offset: usize) -> Option<u8> {
        self.bytes.get(self.at + offset).copied()
    }

    /// Moves to `to`, counting the newlines passed.
    fn skip_to(&mut self, to: usize) {
        let passed = &self.bytes[self.at..to];
        count_steps(passed.len());
        let newlines = passed.iter().filter(|byte| **byte == b'\n').count();
        self.line = self
            .line
            .saturating_add(u32::try_from(newlines).unwrap_or(u32::MAX));
        self.at = to;
    }

    /// The index of the next `byte` at or after `from`, or the end.
    fn find_byte(&self, from: usize, byte: u8) -> usize {
        let found = self.bytes[from..]
            .iter()
            .position(|candidate| *candidate == byte)
            .map_or(self.bytes.len(), |offset| from + offset);
        count_steps(found - from);
        found
    }

    fn emit(
        &mut self,
        kind: TokenKind,
        start: usize,
        closed: bool,
        start_line: u32,
        sink: &mut impl Sink,
    ) {
        let end = self.at;
        if kind != TokenKind::Word {
            self.resolve_pending_end();
            self.last_word_is_e = false;
        }
        self.open = true;
        self.last_end = end;
        self.last_line = if end > start && self.bytes[end - 1] == b'\n' {
            self.line.saturating_sub(1).max(start_line)
        } else {
            self.line
        };
        sink.token(
            Token {
                kind,
                start,
                end,
                closed,
                line: start_line,
            },
            start_line,
        );
    }

    fn finish(&mut self, sink: &mut impl Sink, end: usize, line: u32, terminated: bool) {
        sink.end(end, line, terminated);
        self.open = false;
        self.paren = 0;
        self.block = Block::default();
        self.copy = CopyState::None;
        self.last_word_is_e = false;
    }

    fn line_comment(&mut self, sink: &mut impl Sink) {
        let start = self.at;
        let end = self.find_byte(start, b'\n');
        self.skip_to(end);
        sink.skipped(Region::LineComment, start, end);
    }

    /// A `/* */` comment; Postgres nests them.
    fn block_comment(&mut self, sink: &mut impl Sink) {
        let start = self.at;
        let mut depth = 1_u32;
        let mut index = start + 2;
        while index < self.bytes.len() {
            match (self.bytes[index], self.bytes.get(index + 1)) {
                (b'/', Some(b'*')) => {
                    depth = depth.saturating_add(1);
                    index += 2;
                }
                (b'*', Some(b'/')) => {
                    depth -= 1;
                    index += 2;
                    if depth == 0 {
                        break;
                    }
                }
                _ => index += 1,
            }
        }
        self.skip_to(index);
        sink.skipped(Region::BlockComment { closed: depth == 0 }, start, index);
    }

    /// A psql meta-command: it ends the open statement and runs to its
    /// newline; `\copy … from stdin` is followed by data lines.
    fn meta_command(&mut self, sink: &mut impl Sink) {
        if self.open {
            let (end, line) = (self.last_end, self.last_line);
            self.finish(sink, end, line, false);
        }
        let start = self.at;
        let line = self.line;
        let newline = self.find_byte(start, b'\n');
        // `\\` outside quotes separates a meta-command from SQL on the
        // rest of its line.
        let separator =
            meta_separator(&self.bytes[start + 1..newline]).map(|offset| start + 1 + offset);
        let mut end = separator.unwrap_or(newline);
        while end > start + 1 && matches!(self.bytes[end - 1], b'\r' | b' ' | b'\t') {
            end -= 1;
        }
        self.skip_to(end);
        self.emit(TokenKind::Meta, start, true, line, sink);
        let copy = separator.is_none() && copies_from_stdin(&self.source[start..end]);
        if let Some(separator) = separator {
            self.skip_to(separator + 2);
            self.finish(sink, end, line, true);
            return;
        }
        self.skip_to(newline);
        self.finish(sink, end, line, true);
        if copy {
            self.copy_data(sink);
        }
    }

    /// A `'…'` string: `''` is a quote; in an `E'…'` string a backslash
    /// escapes the next byte.
    fn string(&mut self, sink: &mut impl Sink) {
        let start = self.at;
        let line = self.line;
        let escapes = self.open && self.last_word_is_e && self.last_end == start;
        let mut index = start + 1;
        let mut closed = false;
        while index < self.bytes.len() {
            match self.bytes[index] {
                b'\\' if escapes => index = (index + 2).min(self.bytes.len()),
                b'\'' => {
                    if self.bytes.get(index + 1) == Some(&b'\'') {
                        index += 2;
                    } else {
                        index += 1;
                        closed = true;
                        break;
                    }
                }
                _ => index += 1,
            }
        }
        // An escape before a multi-byte character can step inside it; the
        // closing quote (or the end) is always a character boundary.
        self.skip_to(index);
        self.emit(TokenKind::String, start, closed, line, sink);
    }

    /// A `"…"` or `` `…` `` identifier; a doubled quote is a quote.
    fn quoted(&mut self, quote: u8, sink: &mut impl Sink) {
        let start = self.at;
        let line = self.line;
        let mut index = start + 1;
        let mut closed = false;
        while index < self.bytes.len() {
            if self.bytes[index] == quote {
                if self.bytes.get(index + 1) == Some(&quote) {
                    index += 2;
                    continue;
                }
                index += 1;
                closed = true;
                break;
            }
            index += 1;
        }
        self.skip_to(index);
        self.emit(TokenKind::Quoted, start, closed, line, sink);
    }

    /// `$$`, `$tag$` (a body up to the same delimiter), `$1`, or a lone `$`.
    fn dollar(&mut self, sink: &mut impl Sink) {
        let start = self.at;
        let line = self.line;
        let mut tag_end = start + 1;
        while tag_end < self.bytes.len() && is_tag_byte(self.bytes[tag_end], tag_end == start + 1) {
            tag_end += 1;
        }
        if self.bytes.get(tag_end) == Some(&b'$') {
            let delimiter = &self.source[start..=tag_end];
            let body = tag_end + 1;
            let (end, closed) = match self.source[body..].find(delimiter) {
                Some(offset) => (body + offset + delimiter.len(), true),
                None => (self.bytes.len(), false),
            };
            count_steps(end - body);
            self.skip_to(end);
            self.emit(TokenKind::Dollar, start, closed, line, sink);
        } else if self.peek(1).is_some_and(|byte| byte.is_ascii_digit()) {
            let mut end = start + 1;
            while end < self.bytes.len() && self.bytes[end].is_ascii_digit() {
                end += 1;
            }
            self.skip_to(end);
            self.emit(TokenKind::Number, start, true, line, sink);
        } else {
            self.punct(b'$', sink);
        }
    }

    fn semicolon(&mut self, sink: &mut impl Sink) {
        self.resolve_pending_end();
        if self.paren > 0 || self.block.depth > 0 {
            self.punct(b';', sink);
            return;
        }
        self.skip_to(self.at + 1);
        if self.open {
            let copy = self.copy == CopyState::Stdin;
            let (end, line) = (self.at, self.line);
            self.finish(sink, end, line, true);
            if copy {
                self.copy_data(sink);
            }
        }
    }

    /// The data lines after `COPY … FROM stdin;`, up to a `\.` line; the rest
    /// of the `COPY` line is not data.
    fn copy_data(&mut self, sink: &mut impl Sink) {
        let start = self.at;
        let mut line_start = self.find_byte(start, b'\n');
        loop {
            if line_start >= self.bytes.len() {
                self.skip_to(self.bytes.len());
                sink.skipped(Region::CopyData { closed: false }, start, self.at);
                return;
            }
            line_start += 1;
            let line_end = self.find_byte(line_start, b'\n');
            let mut content = &self.bytes[line_start..line_end];
            while let [rest @ .., b'\r' | b' ' | b'\t'] = content {
                content = rest;
            }
            if content == b"\\." {
                self.skip_to(line_end);
                sink.skipped(Region::CopyData { closed: true }, start, line_end);
                return;
            }
            line_start = line_end;
        }
    }

    fn number(&mut self, sink: &mut impl Sink) {
        let start = self.at;
        let line = self.line;
        let mut end = start + 1;
        while end < self.bytes.len()
            && (self.bytes[end].is_ascii_alphanumeric() || matches!(self.bytes[end], b'_' | b'.'))
        {
            end += 1;
        }
        self.skip_to(end);
        self.emit(TokenKind::Number, start, true, line, sink);
    }

    fn word(&mut self, sink: &mut impl Sink) {
        let start = self.at;
        let line = self.line;
        let mut end = start + 1;
        while end < self.bytes.len() && is_word_byte(self.bytes[end]) {
            end += 1;
        }
        self.skip_to(end);
        let source = self.source;
        let word = &source[start..end];
        self.track_word(word);
        self.emit(TokenKind::Word, start, true, line, sink);
        self.last_word_is_e = word.eq_ignore_ascii_case("e");
    }

    fn punct(&mut self, byte: u8, sink: &mut impl Sink) {
        let start = self.at;
        let line = self.line;
        match byte {
            b'(' => self.paren = self.paren.saturating_add(1),
            b')' => self.paren = self.paren.saturating_sub(1),
            _ => {}
        }
        self.skip_to(start + 1);
        self.emit(TokenKind::Punct(byte), start, true, line, sink);
    }

    fn resolve_pending_end(&mut self) {
        if self.block.pending_end {
            self.block.pending_end = false;
            self.block.depth = self.block.depth.saturating_sub(1);
        }
    }

    /// `COPY … FROM stdin` and routine `BEGIN … END` tracking.
    fn track_word(&mut self, word: &str) {
        let is = |keyword: &str| word.eq_ignore_ascii_case(keyword);
        self.copy = match self.copy {
            CopyState::None if !self.open && is("copy") => CopyState::Copy,
            CopyState::Copy | CopyState::From if is("from") => CopyState::From,
            CopyState::From if is("stdin") => CopyState::Stdin,
            CopyState::From => CopyState::Copy,
            state => state,
        };
        if self.block.pending_end {
            self.block.pending_end = false;
            if ["if", "loop", "while", "repeat", "for"]
                .iter()
                .any(|keyword| is(keyword))
            {
                return;
            }
            self.block.depth = self.block.depth.saturating_sub(1);
            if is("case") {
                return;
            }
        }
        let index = self.block.words as usize;
        self.block.words = self.block.words.saturating_add(1);
        if index < self.block.letters.len() {
            self.block.letters[index] = [
                ("create", b'c'),
                ("or", b'o'),
                ("replace", b'r'),
                ("function", b'f'),
                ("procedure", b'p'),
                ("trigger", b't'),
            ]
            .iter()
            .find(|(keyword, _)| is(keyword))
            .map_or(0, |(_, letter)| *letter);
        }
        if (self.block.after_each && (is("row") || is("statement"))) || is("on") {
            self.block.trigger_body = true;
        }
        self.block.after_each = is("each");
        let ready = !self.block.is_trigger() || self.block.trigger_body;
        if self.paren == 0 && self.block.is_routine() && ready {
            // `CASE` nests only inside a block, as in psql.
            if is("begin") || (is("case") && self.block.depth > 0) {
                self.block.depth = self.block.depth.saturating_add(1);
            } else if is("end") && self.block.depth > 0 {
                self.block.pending_end = true;
            }
        }
    }
}

const fn is_word_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_' || byte >= 0x80
}

const fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$') || byte >= 0x80
}

const fn is_tag_byte(byte: u8, first: bool) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_' || byte >= 0x80 || (!first && byte.is_ascii_digit())
}

/// The offset of the first `\\` outside single and double quotes.
fn meta_separator(line: &[u8]) -> Option<usize> {
    let mut quote = None;
    for index in 0..line.len() {
        match (quote, line[index]) {
            (None, b'\'' | b'"') => quote = Some(line[index]),
            (Some(open), byte) if byte == open => quote = None,
            (None, b'\\') if line.get(index + 1) == Some(&b'\\') => return Some(index),
            _ => {}
        }
    }
    None
}

/// A `\copy` meta-command whose words include `FROM STDIN`, with `STDIN`
/// unquoted (`'stdin'` names a file).
fn copies_from_stdin(text: &str) -> bool {
    let mut words = text
        .split(|character: char| {
            character.is_whitespace() || matches!(character, '(' | ')' | ',' | ';')
        })
        .filter(|word| !word.is_empty());
    if !words
        .next()
        .is_some_and(|first| first.eq_ignore_ascii_case("\\copy"))
    {
        return false;
    }
    let mut previous_from = false;
    for word in words {
        if previous_from && word.eq_ignore_ascii_case("stdin") {
            return true;
        }
        previous_from = word.eq_ignore_ascii_case("from");
    }
    false
}

/// One statement: its byte and line range and its first tokens.
#[derive(Clone, Debug)]
pub(crate) struct Statement {
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) start_line: u32,
    pub(crate) end_line: u32,
    /// The first [`MAX_HEAD_TOKENS`] tokens.
    pub(crate) head: Vec<Token>,
    /// Every token of the statement, counted.
    pub(crate) tokens: usize,
    /// A `;` (or a meta-command's newline) ended the statement.
    pub(crate) terminated: bool,
    /// No token runs unclosed to the end of the file.
    pub(crate) closed: bool,
    /// References read from every token of the statement (`REFERENCES x`
    /// anywhere; `FROM x` and `JOIN x` outside parentheses).
    pub(crate) found: Vec<Found>,
    /// The token stream had more references than are kept.
    pub(crate) found_cut: bool,
    /// The statement follows a down-migration marker (`-- +goose Down`,
    /// `-- migrate:down`) and precedes the next up marker.
    pub(crate) down: bool,
}

impl Statement {
    const fn new() -> Self {
        Self {
            start: 0,
            end: 0,
            start_line: 1,
            end_line: 1,
            head: Vec::new(),
            tokens: 0,
            terminated: false,
            closed: true,
            found: Vec::new(),
            found_cut: false,
            down: false,
        }
    }
}

struct Statements<'s, F> {
    source: &'s str,
    statement: Statement,
    stream: Stream,
    /// A down-migration marker was read and no up marker since.
    down: bool,
    each: F,
}

impl<F: FnMut(&Statement)> Sink for Statements<'_, F> {
    fn token(&mut self, token: Token, line: u32) {
        let statement = &mut self.statement;
        if statement.tokens == 0 {
            statement.start = token.start;
            statement.start_line = line;
            statement.down = self.down;
            self.stream = Stream::new(self.source, token);
        }
        self.stream.feed(self.source, token, &mut statement.found);
        if statement.head.len() < MAX_HEAD_TOKENS {
            statement.head.push(token);
        }
        statement.tokens += 1;
        statement.closed &= token.closed;
    }

    fn end(&mut self, end: usize, line: u32, terminated: bool) {
        let statement = &mut self.statement;
        statement.end = end;
        statement.end_line = line.max(statement.start_line);
        statement.terminated = terminated;
        self.stream.finish(&mut statement.found, false);
        statement.found_cut = self.stream.cut;
        (self.each)(statement);
        statement.head.clear();
        statement.found.clear();
        statement.tokens = 0;
        statement.closed = true;
    }

    fn skipped(&mut self, region: Region, start: usize, end: usize) {
        if region == Region::LineComment {
            let comment = self.source[start..end].trim();
            if ["-- +goose down", "-- migrate:down"]
                .iter()
                .any(|marker| comment.eq_ignore_ascii_case(marker))
            {
                self.down = true;
            } else if ["-- +goose up", "-- migrate:up"]
                .iter()
                .any(|marker| comment.eq_ignore_ascii_case(marker))
            {
                self.down = false;
            }
        }
    }
}

/// Calls `each` with every statement of `source`, in order.
pub(crate) fn each_statement(source: &str, each: impl FnMut(&Statement)) {
    each_statement_from(source, false, each);
}

/// [`each_statement`] starting in a down-migration section when `down`
/// (a `DO` body inside one).
fn each_statement_from(source: &str, down: bool, each: impl FnMut(&Statement)) {
    let mut sink = Statements {
        source,
        statement: Statement::new(),
        stream: Stream::default(),
        down,
        each,
    };
    scan(source, &mut sink);
}

/// Extracts one symbol per recognised definition statement (and per
/// definition inside a top-level `DO` block's body), a `script` symbol for
/// the whole file, and the statements' references as call sites. Never
/// fails.
pub(super) fn extract(source: &str) -> Extraction {
    let mut builder = Builder {
        line_starts: line_starts(source),
        length: source.len(),
        symbols: Vec::new(),
        calls: Vec::new(),
        seen_calls: std::collections::HashSet::new(),
        covered: 0,
        full: false,
        calls_truncated: false,
    };
    if !source.trim().is_empty() {
        let last_line = builder.line_starts.len() - usize::from(source.ends_with('\n'));
        let end_line = u32::try_from(last_line.max(1)).unwrap_or(u32::MAX);
        builder.push_symbol(SymbolRecord {
            name: FILE_SYMBOL.to_owned(),
            qualified_name: FILE_SYMBOL.to_owned(),
            kind: SymbolKind::Module,
            signature: None,
            start_line: 1,
            end_line,
            token_estimate: 0,
        });
    }
    each_statement(source, |statement| {
        if let Some((body, offset)) = do_body(source, statement) {
            each_statement_from(body, statement.down, |inner| {
                if let Some(view) = without_control_words(body, inner) {
                    builder.statement(body, &view, offset);
                }
            });
        } else {
            builder.statement(source, statement, 0);
        }
    });
    let symbols_truncated = builder.full;
    if symbols_truncated && let Some(file) = builder.symbols.first_mut() {
        file.signature = Some(format!(
            "[symbols cut: definitions past the first {MAX_SQL_SYMBOLS} symbols or {} MiB of their lines are not symbols]",
            MAX_SQL_COVERED_BYTES / (1024 * 1024)
        ));
    }
    Extraction {
        symbols: builder.symbols,
        imports: Vec::new(),
        calls: builder.calls,
        calls_truncated: builder.calls_truncated,
        symbols_truncated,
    }
}

/// The name of the symbol that covers a whole SQL file and holds the
/// references of statements that define nothing. No SQL name is stored
/// like it: a name part with a space or parenthesis keeps its quotes.
pub(crate) const FILE_SYMBOL: &str = "(sql file)";

/// Whether `symbol` is a SQL file's own symbol (a module whose qualified
/// name no definition can have).
pub(crate) fn is_file_symbol(symbol: &SymbolRecord) -> bool {
    symbol.kind == SymbolKind::Module && symbol.qualified_name == FILE_SYMBOL
}

/// References (call-site rows) kept per SQL file. Four times the call bound
/// of other languages ([`super::MAX_CALLS_PER_FILE`]): one schema dump can
/// hold thousands of `ALTER`, `COMMENT`, and `ON` references, and each row
/// is a few dozen bytes; past it the cut is marked as for any language.
pub(crate) const MAX_SQL_REFERENCES: usize = 16_384;

/// References kept per statement.
const MAX_STATEMENT_REFERENCES: usize = 64;
/// Longest callee name or reference kind a call site may store.
const MAX_REFERENCE_BYTES: usize = 256;

struct Builder {
    line_starts: Vec<usize>,
    length: usize,
    symbols: Vec<SymbolRecord>,
    calls: Vec<CallSiteRecord>,
    seen_calls: std::collections::HashSet<(usize, String, String, u32)>,
    covered: usize,
    full: bool,
    calls_truncated: bool,
}

impl Builder {
    /// Adds a symbol within the per-file bounds; its index, or `None` once
    /// they are reached.
    fn push_symbol(&mut self, symbol: SymbolRecord) -> Option<usize> {
        if self.full {
            return None;
        }
        let bytes = covered_bytes(
            &self.line_starts,
            self.length,
            symbol.start_line,
            symbol.end_line,
        );
        if self.symbols.len() == MAX_SQL_SYMBOLS
            || self.covered.saturating_add(bytes) > MAX_SQL_COVERED_BYTES
        {
            self.full = true;
            return None;
        }
        self.covered += bytes;
        self.symbols.push(symbol);
        Some(self.symbols.len() - 1)
    }

    /// One statement, `offset` lines below the text it was scanned in.
    fn statement(&mut self, source: &str, statement: &Statement, offset: u32) {
        let class = classify(source, statement);
        let mut caller = 0;
        // A down migration's definitions are listed with a label that keeps
        // them out of the redefinition groups (D107).
        let label = |signature: &Option<String>| match signature {
            Some(signature) if statement.down => Some(format!("(down migration) {signature}")),
            signature => signature.clone(),
        };
        if let Class::Definition(definition) = &class
            && let Some(index) = self.push_symbol(SymbolRecord {
                name: definition.name.clone(),
                qualified_name: definition.qualified_name.clone(),
                kind: definition.object.kind(),
                signature: label(&definition.signature),
                start_line: statement.start_line.saturating_add(offset),
                end_line: statement.end_line.saturating_add(offset),
                token_estimate: 0,
            })
        {
            caller = index;
        }
        if self.symbols.is_empty() {
            return;
        }
        let (mut found, ran_out) = references::head_references(source, &statement.head, &class);
        // Every cut of a file's references marks the file, as for any
        // language: the head ran out while the statement went on, the token
        // stream kept fewer than it met, or the per-statement bound.
        if (ran_out && statement.tokens > statement.head.len()) || statement.found_cut {
            self.calls_truncated = true;
        }
        for reference in &statement.found {
            if !found.iter().any(|kept| kept.name == reference.name) {
                found.push(reference.clone());
            }
        }
        if found.len() > MAX_STATEMENT_REFERENCES {
            found.truncate(MAX_STATEMENT_REFERENCES);
            self.calls_truncated = true;
        }
        for mut reference in found {
            if statement.down {
                reference.kind = format!("down {}", reference.kind);
            }
            if reference.name.len() > MAX_REFERENCE_BYTES
                || reference.kind.len() > MAX_REFERENCE_BYTES
            {
                // Too long for a call site to store: a cut, marked.
                self.calls_truncated = true;
                continue;
            }
            let line = reference.line.saturating_add(offset);
            if !self.seen_calls.insert((
                caller,
                reference.name.clone(),
                reference.kind.clone(),
                line,
            )) {
                continue;
            }
            if self.calls.len() == MAX_SQL_REFERENCES {
                self.calls_truncated = true;
                return;
            }
            self.calls.push(CallSiteRecord {
                caller,
                callee_name: reference.name,
                qualifier: Some(reference.kind),
                form: CallForm::Plain,
                line,
            });
        }
    }
}

/// The body of a top-level `DO` statement and the lines above it: its first
/// dollar-quoted token, without the delimiters.
fn do_body<'s>(source: &'s str, statement: &Statement) -> Option<(&'s str, u32)> {
    let first = statement.head.first()?;
    if first.kind != TokenKind::Word || !source[first.start..first.end].eq_ignore_ascii_case("do") {
        return None;
    }
    let body = statement
        .head
        .iter()
        .find(|token| token.kind == TokenKind::Dollar)?;
    let text = &source[body.start..body.end];
    let tag = text[1..].find('$')? + 2;
    let end = if body.closed && text.len() >= 2 * tag {
        text.len() - tag
    } else {
        text.len()
    };
    Some((&text[tag..end], body.line.saturating_sub(1)))
}

/// A statement of a `DO` body without the PL/pgSQL control words before its
/// own text (`BEGIN`, `ELSE`, `LOOP`, `EXCEPTION`, and an `IF`, `ELSIF`,
/// `WHEN`, `WHILE`, or `FOR` condition up to its `THEN` or `LOOP`). Control
/// flow is not interpreted: a `CREATE` under an `IF` is read as it stands.
fn without_control_words(source: &str, statement: &Statement) -> Option<Statement> {
    let head = &statement.head;
    let word = |index: usize| {
        head.get(index)
            .filter(|token| token.kind == TokenKind::Word)
            .map(|token| &source[token.start..token.end])
    };
    let is = |index: usize, words: &[&str]| {
        word(index).is_some_and(|text| {
            words
                .iter()
                .any(|candidate| text.eq_ignore_ascii_case(candidate))
        })
    };
    let mut skip = 0;
    loop {
        if is(skip, &["begin", "else", "loop", "exception"]) {
            skip += 1;
        } else if is(skip, &["if", "elsif", "when", "while", "for"]) {
            let mut depth = 0_u32;
            let mut found = None;
            for (index, token) in head.iter().enumerate().skip(skip + 1) {
                match token.kind {
                    TokenKind::Punct(b'(') => depth = depth.saturating_add(1),
                    TokenKind::Punct(b')') => depth = depth.saturating_sub(1),
                    TokenKind::Word if depth == 0 && is(index, &["then", "loop"]) => {
                        found = Some(index + 1);
                        break;
                    }
                    _ => {}
                }
            }
            skip = found?;
        } else {
            break;
        }
    }
    if skip == 0 {
        return Some(statement.clone());
    }
    let first = head.get(skip)?;
    Some(Statement {
        start: first.start,
        start_line: first.line,
        head: head[skip..].to_vec(),
        tokens: statement.tokens - skip,
        found: statement.found.clone(),
        ..statement.clone()
    })
}

fn line_starts(source: &str) -> Vec<usize> {
    let mut starts = vec![0];
    starts.extend(
        source
            .bytes()
            .enumerate()
            .filter(|(_, byte)| *byte == b'\n')
            .map(|(index, _)| index + 1),
    );
    starts
}

/// The bytes of lines `start_line..=end_line`, as the indexer reads them for
/// a symbol's token estimate.
fn covered_bytes(line_starts: &[usize], length: usize, start_line: u32, end_line: u32) -> usize {
    let start = line_starts
        .get((start_line as usize).saturating_sub(1))
        .copied()
        .unwrap_or(length);
    let end = line_starts
        .get(end_line as usize)
        .copied()
        .unwrap_or(length);
    end.saturating_sub(start)
}

/// A recognised definition statement's object type.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) enum Object {
    Table,
    ForeignTable,
    View,
    MaterializedView,
    Function,
    Procedure,
    Aggregate,
    Trigger,
    EventTrigger,
    Index,
    Enum,
    Type,
    Domain,
    Policy,
    Rule,
    Schema,
    Extension,
    Sequence,
    Publication,
}

impl Object {
    /// The store kind (the store's ten kinds are fixed; no migration).
    pub(crate) const fn kind(self) -> SymbolKind {
        match self {
            Self::Table | Self::ForeignTable | Self::View | Self::MaterializedView => {
                SymbolKind::Struct
            }
            Self::Function | Self::Procedure | Self::Aggregate | Self::EventTrigger => {
                SymbolKind::Function
            }
            Self::Trigger | Self::Policy | Self::Rule => SymbolKind::Method,
            Self::Index | Self::Sequence | Self::Publication => SymbolKind::Const,
            Self::Enum => SymbolKind::Enum,
            Self::Type | Self::Domain => SymbolKind::Type,
            Self::Schema | Self::Extension => SymbolKind::Module,
        }
    }
}

/// A recognised definition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Definition {
    pub(crate) object: Object,
    pub(crate) name: String,
    pub(crate) qualified_name: String,
    pub(crate) signature: Option<String>,
    /// The bare name of the table a trigger, index, policy, or rule is
    /// named by, and the line it is written on.
    pub(crate) table: Option<(String, u32)>,
}

/// What a statement is, as far as symbols go.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Class {
    Definition(Definition),
    /// A recognised `CREATE` whose name (or table) could not be read.
    Unnamed(Object),
    /// A `CREATE` of an object type not recognised.
    OtherCreate,
    /// Not a `CREATE` statement.
    Other,
}

/// Words that end a routine's `RETURNS` clause.
const ROUTINE_CLAUSE_WORDS: &[&str] = &[
    "as",
    "language",
    "begin",
    "return",
    "immutable",
    "stable",
    "volatile",
    "security",
    "set",
    "strict",
    "called",
    "parallel",
    "cost",
    "rows",
    "support",
    "leakproof",
    "not",
    "window",
    "transform",
    "external",
    "deterministic",
    "comment",
    "reads",
    "modifies",
    "contains",
    "no",
    "sql",
];

/// Reads a statement's head as a definition.
pub(crate) fn classify(source: &str, statement: &Statement) -> Class {
    let mut head = Head {
        source,
        tokens: &statement.head,
        at: 0,
    };
    if !head.eat("create") {
        return Class::Other;
    }
    let _ = head.eat_all(&["or", "replace"]) || head.eat_all(&["or", "alter"]);
    let (mut materialized, mut foreign, mut event) = (false, false, false);
    let object = loop {
        let Some(word) = head.keyword(head.at) else {
            return Class::OtherCreate;
        };
        head.at += 1;
        match word.as_str() {
            "temp" | "temporary" | "unlogged" | "global" | "local" | "unique" | "recursive"
            | "constraint" | "trusted" | "procedural" => {}
            "materialized" => materialized = true,
            "foreign" => foreign = true,
            "event" => event = true,
            "table" if foreign => break Object::ForeignTable,
            "table" => break Object::Table,
            "view" if materialized => break Object::MaterializedView,
            "view" => break Object::View,
            "function" => break Object::Function,
            "procedure" => break Object::Procedure,
            "aggregate" => break Object::Aggregate,
            "trigger" if event => break Object::EventTrigger,
            "trigger" => break Object::Trigger,
            "index" => break Object::Index,
            "type" => break Object::Type,
            "domain" => break Object::Domain,
            "policy" => break Object::Policy,
            "rule" => break Object::Rule,
            "schema" => break Object::Schema,
            "extension" => break Object::Extension,
            "sequence" => break Object::Sequence,
            "publication" => break Object::Publication,
            _ => return Class::OtherCreate,
        }
    };
    if object == Object::Index {
        head.eat("concurrently");
    }
    head.eat_all(&["if", "not", "exists"]);
    if object == Object::Schema {
        head.eat("authorization");
    }
    if object == Object::Index && head.is("on") {
        return Class::Unnamed(object);
    }
    let Some((name, written)) = head.name_written() else {
        return Class::Unnamed(object);
    };
    let whole = head.tokens.len();
    let mut object = object;
    let mut table = None;
    let mut table_line = 0;
    let signature_end = match object {
        Object::Table | Object::ForeignTable => head.find_top_level(head.at, |token, text| {
            token.kind == TokenKind::Punct(b'(') || is_word_in(token, text, &["as", "for", "with"])
        }),
        Object::View | Object::MaterializedView => head.find_top_level(head.at, |token, text| {
            is_word_in(token, text, &["as", "with"])
        }),
        Object::Function | Object::Procedure | Object::Aggregate => {
            let mut end = head.at;
            if head.is_punct(end, b'(') {
                end = head.after_group(end);
            }
            if head
                .word_at(end)
                .is_some_and(|word| word.eq_ignore_ascii_case("returns"))
            {
                end = head.find_top_level(end + 1, |token, text| {
                    matches!(token.kind, TokenKind::String | TokenKind::Dollar)
                        || is_word_in(token, text, ROUTINE_CLAUSE_WORDS)
                });
            }
            end
        }
        Object::Trigger => {
            let on = head.find_top_level(head.at, |token, text| is_word_in(token, text, &["on"]));
            head.at = on + 1;
            head.eat("only");
            table_line = head.line_at(head.at);
            table = Some(head.name_written());
            whole
        }
        Object::Index | Object::Policy => {
            if head.eat("on") {
                head.eat("only");
                table_line = head.line_at(head.at);
                table = Some(head.name_written());
            } else {
                table = Some(None);
            }
            whole
        }
        Object::Rule => {
            let to = head.find_top_level(head.at, |token, text| is_word_in(token, text, &["to"]));
            head.at = to + 1;
            table_line = head.line_at(head.at);
            table = Some(head.name_written());
            whole
        }
        Object::Type => {
            if head.is("as")
                && head
                    .word_at(head.at + 1)
                    .is_some_and(|word| word.eq_ignore_ascii_case("enum"))
            {
                object = Object::Enum;
            }
            whole
        }
        _ => whole,
    };
    let bare = name.last().cloned().unwrap_or_default();
    let (qualified_name, table) = match table {
        Some(Some((table, table_written))) => (
            format!("{table_written}::{}", render(&bare)),
            table.last().map(|last| (last.clone(), table_line)),
        ),
        Some(None) => return Class::Unnamed(object),
        None => (written, None),
    };
    if qualified_name.len() > MAX_NAME_BYTES {
        return Class::Unnamed(object);
    }
    let cut_by_head =
        signature_end >= statement.head.len() && statement.tokens > statement.head.len();
    Class::Definition(Definition {
        object,
        name: bare,
        qualified_name,
        table,
        signature: signature(
            source,
            &statement.head[..signature_end.min(statement.head.len())],
            cut_by_head,
        ),
    })
}

fn is_word_in(token: Token, text: &str, words: &[&str]) -> bool {
    token.kind == TokenKind::Word && words.iter().any(|word| text.eq_ignore_ascii_case(word))
}

/// The tokens' text, comments dropped and every run of whitespace one
/// space, cut at [`MAX_SIGNATURE_BYTES`] and marked `…` when cut.
fn signature(source: &str, tokens: &[Token], cut_by_head: bool) -> Option<String> {
    let mut text = String::new();
    let mut cut = false;
    let mut previous_end = None;
    for token in tokens {
        if previous_end.is_some_and(|end| end < token.start) && !text.ends_with(' ') {
            text.push(' ');
        }
        previous_end = Some(token.end);
        let room = SIGNATURE_KEPT_BYTES.saturating_sub(text.len());
        let mut piece = &source[token.start..token.end];
        if piece.len() > room {
            let mut end = room;
            while !piece.is_char_boundary(end) {
                end -= 1;
            }
            piece = &piece[..end];
            cut = true;
        }
        count_signature_bytes(piece.len());
        for character in piece.chars() {
            if character.is_whitespace() || character.is_control() {
                if !text.ends_with(' ') {
                    text.push(' ');
                }
            } else {
                text.push(character);
            }
        }
        if cut {
            break;
        }
    }
    marked_signature(&text, cut || cut_by_head)
}

/// A cursor over a statement's head tokens.
struct Head<'a> {
    source: &'a str,
    tokens: &'a [Token],
    at: usize,
}

impl<'a> Head<'a> {
    fn word_at(&self, index: usize) -> Option<&'a str> {
        self.tokens
            .get(index)
            .filter(|token| token.kind == TokenKind::Word)
            .map(|token| &self.source[token.start..token.end])
    }

    /// The word at `index` in lowercase, when it is short enough to be a
    /// keyword.
    fn keyword(&self, index: usize) -> Option<String> {
        self.word_at(index)
            .filter(|word| word.len() <= 16)
            .map(str::to_ascii_lowercase)
    }

    fn is(&self, word: &str) -> bool {
        self.word_at(self.at)
            .is_some_and(|candidate| candidate.eq_ignore_ascii_case(word))
    }

    fn eat(&mut self, word: &str) -> bool {
        let matched = self.is(word);
        if matched {
            self.at += 1;
        }
        matched
    }

    fn eat_all(&mut self, words: &[&str]) -> bool {
        let matched = words.iter().enumerate().all(|(offset, word)| {
            self.word_at(self.at + offset)
                .is_some_and(|candidate| candidate.eq_ignore_ascii_case(word))
        });
        if matched {
            self.at += words.len();
        }
        matched
    }

    fn is_punct(&self, index: usize, byte: u8) -> bool {
        self.tokens
            .get(index)
            .is_some_and(|token| token.kind == TokenKind::Punct(byte))
    }

    /// The line of the token at `index`, or 0 past the head.
    fn line_at(&self, index: usize) -> u32 {
        self.tokens.get(index).map_or(0, |token| token.line)
    }

    /// One name part at `index`: the unquoted name, its written form, and
    /// how many tokens it takes (`U&"…"` takes three).
    fn part(&self, index: usize) -> Option<(String, String, usize)> {
        let token = *self.tokens.get(index)?;
        if token.kind == TokenKind::Word
            && self.source[token.start..token.end].eq_ignore_ascii_case("u")
            && let (Some(ampersand), Some(quoted)) =
                (self.tokens.get(index + 1), self.tokens.get(index + 2))
            && ampersand.kind == TokenKind::Punct(b'&')
            && quoted.kind == TokenKind::Quoted
            && ampersand.start == token.end
            && quoted.start == ampersand.end
        {
            // Unicode escapes are kept as written, not decoded.
            let name = identifier(self.source, *quoted)?;
            let written = &self.source[token.start..quoted.end];
            return Some((name, written.to_owned(), 3));
        }
        let name = identifier(self.source, token)?;
        let written = render(&name);
        Some((name, written, 1))
    }

    /// A dotted name of up to four parts, unquoted.
    fn name(&mut self) -> Option<Vec<String>> {
        self.name_written().map(|(parts, _)| parts)
    }

    /// A dotted name of up to four parts, unquoted, and as it is shown:
    /// parts joined by `.`, a part that needs quotes keeping them.
    fn name_written(&mut self) -> Option<(Vec<String>, String)> {
        let (first, written, width) = self.part(self.at)?;
        let mut parts = vec![first];
        let mut shown = written;
        self.at += width;
        while parts.len() < 4 && self.is_punct(self.at, b'.') {
            let Some((part, written, width)) = self.part(self.at + 1) else {
                break;
            };
            parts.push(part);
            shown.push('.');
            shown.push_str(&written);
            self.at += 1 + width;
        }
        (shown.len() <= MAX_NAME_BYTES).then_some((parts, shown))
    }

    /// The first token from `from` outside the parentheses opened from there
    /// for which `stop` holds, or the end of the head.
    fn find_top_level(&self, from: usize, stop: impl Fn(Token, &str) -> bool) -> usize {
        let mut depth = 0_u32;
        for (index, token) in self.tokens.iter().enumerate().skip(from) {
            if depth == 0 && stop(*token, &self.source[token.start..token.end]) {
                return index;
            }
            match token.kind {
                TokenKind::Punct(b'(') => depth = depth.saturating_add(1),
                TokenKind::Punct(b')') => depth = depth.saturating_sub(1),
                _ => {}
            }
        }
        self.tokens.len()
    }

    /// The index after the `)` closing the `(` at `open`.
    fn after_group(&self, open: usize) -> usize {
        let mut depth = 0_u32;
        for (index, token) in self.tokens.iter().enumerate().skip(open) {
            match token.kind {
                TokenKind::Punct(b'(') => depth = depth.saturating_add(1),
                TokenKind::Punct(b')') => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return index + 1;
                    }
                }
                _ => {}
            }
        }
        self.tokens.len()
    }
}

/// A name part as shown in a qualified name: quoted (with `"` doubled)
/// when it holds a character that would make it read as several parts or
/// as a folded name (`.`, `:`, whitespace, an uppercase letter, a quote, a
/// parenthesis).
fn render(part: &str) -> String {
    let needs_quotes = part.chars().any(|character| {
        matches!(character, '.' | ':' | '"' | '(' | ')')
            || character.is_whitespace()
            || character.is_uppercase()
    });
    if needs_quotes {
        format!("\"{}\"", part.replace('"', "\"\""))
    } else {
        part.to_owned()
    }
}

/// An identifier: an unquoted word folded to lowercase (as Postgres folds
/// it) or a quoted identifier unquoted.
fn identifier(source: &str, token: Token) -> Option<String> {
    let text = &source[token.start..token.end];
    if text.len() > MAX_NAME_BYTES + 2 {
        return None;
    }
    let name = match token.kind {
        TokenKind::Word => text.to_ascii_lowercase(),
        TokenKind::Quoted => {
            let quote = &text[..1];
            let inner = &text[1..];
            let inner = if token.closed && !inner.is_empty() {
                &inner[..inner.len() - 1]
            } else {
                inner
            };
            inner.replace(&quote.repeat(2), quote)
        }
        _ => return None,
    };
    (!name.is_empty() && !name.chars().any(char::is_control)).then_some(name)
}

/// The family a definition's object belongs to, for grouping redefinitions
/// and matching `DROP` statements (D107): tables and views are relations;
/// functions, procedures, and aggregates are routines; enums, other types,
/// and domains are types.
pub(crate) const fn family(object: Object) -> &'static str {
    match object {
        Object::Table | Object::ForeignTable | Object::View | Object::MaterializedView => {
            "relation"
        }
        Object::Function | Object::Procedure | Object::Aggregate => "routine",
        Object::Trigger => "trigger",
        Object::EventTrigger => "event_trigger",
        Object::Index => "index",
        Object::Enum | Object::Type | Object::Domain => "type",
        Object::Policy => "policy",
        Object::Rule => "rule",
        Object::Schema => "schema",
        Object::Extension => "extension",
        Object::Sequence => "sequence",
        Object::Publication => "publication",
    }
}

/// The family of a stored SQL definition, read back from its signature
/// (which starts with the statement's `CREATE` words).
pub(crate) fn family_of_signature(signature: &str) -> Option<&'static str> {
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

#[cfg(test)]
mod audit;
#[cfg(test)]
mod tests;
