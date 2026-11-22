//! The statement gate: defence in depth in front of the database's own
//! read-only session, never the only control.
//!
//! It accepts a **strict subset** of SQL. After an independent review got
//! past a lexer that tried to follow each dialect, the gate stopped
//! interpreting anything whose reading differs between engines or settings
//! and refuses it instead, naming the construct so the statement can be
//! rewritten: every comment form (`--`, `/* */` including optimizer hints
//! and `/*! */`, MySQL `#`); `$` (dollar quoting, parameters); backslashes;
//! prefixed strings and names (`E''`, `N''`, `X''`, `B''`, `U&''`, `U&""`,
//! `_charset''`, a quote right after a number) and `UESCAPE`; hex and binary
//! number literals; control characters and NUL; and non-ASCII characters
//! anywhere outside a plain `'...'` string (names included). Whitespace is
//! ASCII space, tab, carriage return, and line feed only. A plain string is
//! `'...'` with `''` as its only escape (MySQL also reads `"..."` as one,
//! with `ANSI_QUOTES` pinned off); quoted names are `"..."` (Postgres,
//! SQLite), backticks (MySQL, SQLite), and `[...]` (SQLite), printable ASCII
//! only.
//!
//! On that token stream it then requires exactly one statement (a single
//! optional trailing `;`), a leading `SELECT`, `WITH`, `VALUES`, `TABLE`,
//! `SHOW`, `EXPLAIN`, or `DESCRIBE`, no `ANALYZE` in an explain, no write,
//! DDL, or session keyword anywhere (only the `CREATE` of `SHOW CREATE` is
//! exempt), and no call to a function on a per-engine deny-list. A keyword
//! written as a quoted name is a name, which is how a column literally named
//! `update` can still be selected.
//!
//! The function deny-list is a text list and therefore incomplete by nature:
//! a view, a wrapper function, or a function it does not name can still be
//! reached. It is not a boundary. The controls that hold are the database's
//! read-only session, the refusal of privileged roles, and the deadlines;
//! what remains reachable under them is listed in ADR-0036.

use crate::SqlEngine;

/// Statements may start only with these keywords.
const ALLOWED_LEADING: &[&str] = &[
    "SELECT", "WITH", "VALUES", "TABLE", "SHOW", "EXPLAIN", "DESCRIBE", "DESC",
];

/// Keywords that never appear in a read-only statement this tool runs.
const DENIED_WORDS: &[&str] = &[
    "INSERT",
    "UPDATE",
    "DELETE",
    "MERGE",
    "UPSERT",
    "TRUNCATE",
    "DROP",
    "ALTER",
    "CREATE",
    "GRANT",
    "REVOKE",
    "COPY",
    "CALL",
    "VACUUM",
    "REINDEX",
    "ATTACH",
    "DETACH",
    "OUTFILE",
    "DUMPFILE",
    "LOCK",
    "UNLOCK",
    "HANDLER",
    "PREPARE",
    "EXECUTE",
    "DEALLOCATE",
    "LISTEN",
    "NOTIFY",
    "REFRESH",
    "CLUSTER",
    "CHECKPOINT",
    "DISCARD",
    "SECURITY",
    "IMPORT",
    // SELECT ... INTO creates a table (Postgres) or sets variables or files
    // (MySQL); FOR SHARE / FOR KEY SHARE / LOCK IN SHARE MODE take locks.
    "INTO",
    "SHARE",
];

/// Functions refused in every dialect.
const DENIED_FUNCTIONS_COMMON: &[&str] = &["load_extension"];

/// Postgres functions with effects outside this read-only transaction, or
/// that read server files.
const DENIED_FUNCTIONS_POSTGRES: &[&str] = &[
    "pg_read_file",
    "pg_read_binary_file",
    "pg_ls_dir",
    "pg_stat_file",
    "pg_ls_logdir",
    "pg_ls_waldir",
    "pg_ls_tmpdir",
    "pg_ls_archive_statusdir",
    "lo_import",
    "lo_export",
    "lo_unlink",
    "lo_create",
    "lo_creat",
    "lo_from_bytea",
    "lo_put",
    "lo_truncate",
    "dblink",
    "dblink_exec",
    "dblink_connect",
    "dblink_connect_u",
    "dblink_open",
    "dblink_send_query",
    "dblink_fetch",
    "pg_terminate_backend",
    "pg_cancel_backend",
    "pg_reload_conf",
    "pg_rotate_logfile",
    "pg_promote",
    "pg_switch_wal",
    "pg_create_restore_point",
    "pg_logical_emit_message",
    "pg_create_logical_replication_slot",
    "pg_create_physical_replication_slot",
    "pg_drop_replication_slot",
    "pg_advisory_lock",
    "pg_advisory_lock_shared",
    "pg_advisory_xact_lock",
    "pg_advisory_xact_lock_shared",
    "pg_try_advisory_lock",
    "pg_try_advisory_lock_shared",
    "pg_try_advisory_xact_lock",
    "pg_try_advisory_xact_lock_shared",
    "set_config",
    "nextval",
    "setval",
    "pg_notify",
    "pg_file_write",
    "pg_file_rename",
    "pg_file_unlink",
    "query_to_xml",
    "query_to_xml_and_xmlschema",
    "query_to_xmlschema",
    "cursor_to_xml",
    "table_to_xml",
    "table_to_xmlschema",
    "table_to_xml_and_xmlschema",
    "cursor_to_xmlschema",
    "database_to_xml",
    "database_to_xmlschema",
    "database_to_xml_and_xmlschema",
    "schema_to_xml",
    "schema_to_xmlschema",
    "schema_to_xml_and_xmlschema",
    "ts_stat",
    "ts_rewrite",
];

/// MySQL/MariaDB functions with effects outside this transaction or that read
/// server files.
const DENIED_FUNCTIONS_MYSQL: &[&str] = &[
    "load_file",
    "get_lock",
    "release_lock",
    "release_all_locks",
    "sys_exec",
    "sys_eval",
    "master_pos_wait",
    "source_pos_wait",
];

/// SQLite functions that touch files or extensions (most are absent from the
/// bundled build; refusing them keeps that true if they appear).
const DENIED_FUNCTIONS_SQLITE: &[&str] = &["readfile", "writefile", "edit", "fts3_tokenizer"];

#[derive(Debug, PartialEq, Eq)]
enum Token {
    /// An unquoted word, upper-cased.
    Word(String),
    /// A quoted identifier's text, lower-cased.
    Quoted(String),
    Semicolon,
    OpenParen,
    Other,
}

/// Checks one statement for `engine`. The error says what was refused and
/// never echoes more than one keyword or function name.
pub fn check_statement(sql: &str, engine: SqlEngine) -> Result<(), String> {
    let tokens = lex(sql, engine)?;
    let semicolons = tokens
        .iter()
        .filter(|token| **token == Token::Semicolon)
        .count();
    let first = tokens
        .split(|token| *token == Token::Semicolon)
        .next()
        .unwrap_or_default();
    if semicolons > 1 || (semicolons == 1 && tokens.last() != Some(&Token::Semicolon)) {
        return Err(
            "exactly one statement is allowed; remove everything after the first `;`".to_owned(),
        );
    }
    let leading = first
        .iter()
        .find(|token| **token != Token::OpenParen)
        .ok_or_else(|| "the query is empty".to_owned())?;
    let Token::Word(keyword) = leading else {
        return Err("the statement must start with a keyword".to_owned());
    };
    if !ALLOWED_LEADING.contains(&keyword.as_str()) {
        return Err(format!(
            "only read statements are allowed (SELECT, WITH, VALUES, TABLE, SHOW, EXPLAIN, DESCRIBE); `{keyword}` is refused"
        ));
    }
    if engine == SqlEngine::MySql
        && keyword == "SHOW"
        && first
            .iter()
            .any(|token| matches!(token, Token::Word(word) if word == "WHERE"))
    {
        return Err(
            "SHOW ... WHERE is refused in MySQL: its expressions run outside the statement time limit; use SHOW ... LIKE 'pattern' or query information_schema"
                .to_owned(),
        );
    }
    if matches!(keyword.as_str(), "EXPLAIN" | "DESCRIBE" | "DESC") && explains_with_analyze(first) {
        return Err("EXPLAIN ANALYZE runs the statement; use EXPLAIN without ANALYZE".to_owned());
    }
    // `SHOW CREATE TABLE|VIEW|...` only reads: the one `CREATE` right after a
    // leading SHOW is exempt; every other token, including any WHERE, LIKE,
    // or subquery, is checked as in any statement.
    let leading_index = first
        .iter()
        .position(|token| *token != Token::OpenParen)
        .unwrap_or_default();
    let exempt = (keyword == "SHOW"
        && first.get(leading_index + 1) == Some(&Token::Word("CREATE".to_owned())))
    .then_some(leading_index + 1);
    for (_, token) in first
        .iter()
        .enumerate()
        .filter(|(index, _)| Some(*index) != exempt)
    {
        if let Token::Word(word) = token
            && DENIED_WORDS.contains(&word.as_str())
        {
            let name = word.to_ascii_lowercase();
            return Err(format!(
                "`{word}` is not allowed in a read-only query. If it is a column or table name, quote it: \"{name}\" in Postgres and SQLite, `{name}` in MySQL"
            ));
        }
    }
    for pair in first.windows(2) {
        let name = match &pair[0] {
            Token::Word(word) => word.to_ascii_lowercase(),
            Token::Quoted(quoted) => quoted.clone(),
            _ => continue,
        };
        if pair[1] == Token::OpenParen && denied_function(&name, engine) {
            return Err(format!(
                "the function `{name}` is refused: it reaches outside this read-only query"
            ));
        }
    }
    Ok(())
}

fn denied_function(name: &str, engine: SqlEngine) -> bool {
    let list = match engine {
        SqlEngine::Postgres => DENIED_FUNCTIONS_POSTGRES,
        SqlEngine::MySql => DENIED_FUNCTIONS_MYSQL,
        SqlEngine::Sqlite => DENIED_FUNCTIONS_SQLITE,
    };
    DENIED_FUNCTIONS_COMMON.contains(&name) || list.contains(&name)
}

/// `EXPLAIN ANALYZE ...`, `EXPLAIN ANALYSE ...`, an option list
/// `EXPLAIN (..., ANALYZE ...)`, or the word anywhere else in an EXPLAIN:
/// refusing every unquoted use is simpler than parsing each dialect's
/// option grammar.
fn explains_with_analyze(tokens: &[Token]) -> bool {
    tokens.iter().any(|token| match token {
        Token::Word(word) => word == "ANALYZE" || word == "ANALYSE",
        // A quoted option name is still the option.
        Token::Quoted(name) => name == "analyze" || name == "analyse",
        _ => false,
    })
}

/// The strict-subset lexer. Whitespace is ASCII space, tab, carriage return,
/// and line feed only. Anything whose reading differs between engines or
/// settings is refused, never interpreted: every comment form, `$`,
/// backslashes, prefixed strings (`E''`, `N''`, `X''`, `B''`, `U&''`,
/// `_charset''`, and a quote right after a number), hex and binary number
/// literals, control characters, and non-ASCII characters outside a plain
/// `'...'` string. A plain string is `'...'` with `''` as its only escape.
fn lex(sql: &str, engine: SqlEngine) -> Result<Vec<Token>, String> {
    let chars = sql.chars().collect::<Vec<_>>();
    let mut tokens = Vec::new();
    let mut index = 0;
    let at = |index: usize| chars.get(index).copied();
    while let Some(character) = at(index) {
        match character {
            ' ' | '\t' | '\r' | '\n' => index += 1,
            c if c.is_control() => return Err(control(c)),
            c if !c.is_ascii() => return Err(non_ascii(c)),
            '-' if at(index + 1) == Some('-') => {
                return Err(
                    "comments are refused (`--`); remove the comment and send the statement alone"
                        .to_owned(),
                );
            }
            '/' if at(index + 1) == Some('*') => {
                return Err(
                    "comments are refused (`/* */`, including optimizer hints and `/*! */`); remove the comment and send the statement alone"
                        .to_owned(),
                );
            }
            '#' if engine == SqlEngine::MySql => {
                return Err(
                    "comments are refused (`#`); remove the comment and send the statement alone"
                        .to_owned(),
                );
            }
            '$' => {
                return Err(
                    "`$` is refused (dollar-quoted strings, parameters, and `$` in names); use a plain '...' string"
                        .to_owned(),
                );
            }
            '\\' => return Err(backslash()),
            '\'' => {
                refuse_prefix(&chars, index)?;
                // SQLite never reads a backslash as an escape, so `LIKE ...
                // ESCAPE '\'` is allowed there; Postgres and MySQL may.
                index = plain_string(&chars, index, '\'', engine == SqlEngine::Sqlite)?;
                tokens.push(Token::Other);
            }
            '"' if engine == SqlEngine::MySql => {
                // With ANSI_QUOTES off (the connection pins it) MySQL reads
                // "..." as a string; the same plain rules apply.
                refuse_prefix(&chars, index)?;
                index = plain_string(&chars, index, '"', false)?;
                tokens.push(Token::Other);
            }
            '"' => {
                refuse_prefix(&chars, index)?;
                let end = quoted_identifier(&chars, index, '"')?;
                tokens.push(Token::Quoted(quoted_text(&chars, index, end)));
                index = end;
            }
            '`' if engine != SqlEngine::Postgres => {
                refuse_prefix(&chars, index)?;
                let end = quoted_identifier(&chars, index, '`')?;
                tokens.push(Token::Quoted(quoted_text(&chars, index, end)));
                index = end;
            }
            '[' if engine == SqlEngine::Sqlite => {
                refuse_prefix(&chars, index)?;
                let end = quoted_identifier(&chars, index, ']')?;
                tokens.push(Token::Quoted(quoted_text(&chars, index, end)));
                index = end;
            }
            ';' => {
                tokens.push(Token::Semicolon);
                index += 1;
            }
            '(' => {
                tokens.push(Token::OpenParen);
                index += 1;
            }
            c if c.is_ascii_digit() => {
                if c == '0' && matches!(at(index + 1), Some('x' | 'X' | 'b' | 'B')) {
                    return Err(
                        "hex and binary literals (0x..., 0b...) are refused; write the number in decimal"
                            .to_owned(),
                    );
                }
                index = number_end(&chars, index)?;
                tokens.push(Token::Other);
            }
            c if c.is_ascii_alphabetic() || c == '_' => {
                let start = index;
                while at(index).is_some_and(|c| c.is_ascii_alphanumeric() || c == '_') {
                    index += 1;
                }
                let word = chars[start..index]
                    .iter()
                    .collect::<String>()
                    .to_ascii_uppercase();
                if word == "UESCAPE" {
                    return Err("UESCAPE is refused; write names and strings plainly".to_owned());
                }
                tokens.push(Token::Word(word));
            }
            _ => {
                tokens.push(Token::Other);
                index += 1;
            }
        }
    }
    Ok(tokens)
}

/// The end of a number: digits, an optional `.` and digits, an optional
/// exponent (`e`, an optional sign, digits), and nothing glued after it.
/// Engines split `1FROM` or `1e5pg_sleep` differently, so a letter, digit,
/// `_`, `.`, or quote right after a number is refused.
fn number_end(chars: &[char], start: usize) -> Result<usize, String> {
    let digit = |index: usize| chars.get(index).is_some_and(char::is_ascii_digit);
    let glued = || {
        "put a space after numbers: a letter, `_`, `.`, or quote directly after a number is read differently by different engines"
            .to_owned()
    };
    let mut index = start;
    while digit(index) {
        index += 1;
    }
    if chars.get(index) == Some(&'.') {
        index += 1;
        while digit(index) {
            index += 1;
        }
    }
    if matches!(chars.get(index), Some('e' | 'E')) {
        index += 1;
        if matches!(chars.get(index), Some('+' | '-')) {
            index += 1;
        }
        if !digit(index) {
            return Err(glued());
        }
        while digit(index) {
            index += 1;
        }
    }
    match chars.get(index) {
        Some(next)
            if next.is_ascii_alphanumeric() || matches!(next, '_' | '.' | '\'' | '"' | '`') =>
        {
            Err(glued())
        }
        _ => Ok(index),
    }
}

fn control(character: char) -> String {
    format!(
        "control characters (U+{:04X}, including NUL and tabs other than ASCII whitespace) are refused",
        u32::from(character)
    )
}

fn non_ascii(character: char) -> String {
    format!(
        "non-ASCII characters are allowed only inside a plain '...' string; U+{:04X} was found outside one (in a name, as whitespace, or as an operator)",
        u32::from(character)
    )
}

fn backslash() -> String {
    "backslashes are refused (engines read them differently inside strings); write the text without `\\`".to_owned()
}

/// A quote directly after a letter, digit, `_`, or `&` starts a prefixed
/// string or name (`E'..'`, `N'..'`, `X'..'`, `B'..'`, `U&'..'`,
/// `_utf8mb4'..'`, `1e'..'`) that engines read differently.
fn refuse_prefix(chars: &[char], index: usize) -> Result<(), String> {
    match index.checked_sub(1).map(|previous| chars[previous]) {
        Some(previous) if previous.is_ascii_alphanumeric() || previous == '_' || previous == '&' => {
            Err(
                "prefixed string literals and names (E'', N'', X'', B'', U&'', U&\"\", _charset'') are refused; use a plain '...' string with a space before it"
                    .to_owned(),
            )
        }
        _ => Ok(()),
    }
}

/// A plain string: `''` (or `""`) is the only escape; no backslash and no
/// control character inside; any other character, non-ASCII included, is
/// literal text.
fn plain_string(
    chars: &[char],
    index: usize,
    quote: char,
    literal_backslash: bool,
) -> Result<usize, String> {
    let mut cursor = index + 1;
    while let Some(&character) = chars.get(cursor) {
        if character == quote {
            if chars.get(cursor + 1) == Some(&quote) {
                cursor += 2;
                continue;
            }
            return Ok(cursor + 1);
        }
        if character == '\\' && !literal_backslash {
            return Err(backslash());
        }
        if character.is_control() && !matches!(character, '\t' | '\n' | '\r') {
            return Err(control(character));
        }
        cursor += 1;
    }
    Err("a quoted string is not closed".to_owned())
}

/// A quoted name: printable ASCII only, the closing quote doubled as its
/// only escape (no doubling for `[...]`).
fn quoted_identifier(chars: &[char], index: usize, close: char) -> Result<usize, String> {
    let mut cursor = index + 1;
    while let Some(&character) = chars.get(cursor) {
        if character == close {
            if close != ']' && chars.get(cursor + 1) == Some(&close) {
                cursor += 2;
                continue;
            }
            return Ok(cursor + 1);
        }
        if !character.is_ascii() {
            return Err(non_ascii(character));
        }
        if character.is_ascii_control() {
            return Err(control(character));
        }
        if character == '\\' {
            return Err(backslash());
        }
        cursor += 1;
    }
    Err("a quoted name is not closed".to_owned())
}

/// A quoted identifier's name as the server reads it: the doubled closing
/// quote is one quote character. Lower-cased, so a quoted name that differs
/// from a denied one only in case is refused too (over-refusing is safe).
fn quoted_text(chars: &[char], start: usize, end: usize) -> String {
    let quote = chars[end - 1];
    let doubled = format!("{quote}{quote}");
    chars[start + 1..end - 1]
        .iter()
        .collect::<String>()
        .replace(&doubled, &quote.to_string())
        .to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [SqlEngine; 3] = [SqlEngine::Sqlite, SqlEngine::Postgres, SqlEngine::MySql];

    fn allowed(sql: &str, engine: SqlEngine) -> bool {
        check_statement(sql, engine).is_ok()
    }

    #[test]
    fn test_plain_reads_pass_in_every_dialect() {
        for engine in ALL {
            for sql in [
                "SELECT 1",
                "select * from orders where id = 3;",
                "  \nSELECT a FROM t\t;  ",
                "WITH x AS (SELECT 1 AS n) SELECT n FROM x",
                "VALUES (1), (2)",
                "(SELECT 1) UNION (SELECT 2)",
                "EXPLAIN SELECT * FROM t",
                "SELECT 'a;b', 'it''s' FROM t",
            ] {
                assert!(allowed(sql, engine), "{engine:?}: {sql}");
            }
        }
        assert!(allowed("SELECT \"update\" FROM t", SqlEngine::Postgres));
        assert!(allowed("SELECT `delete` FROM t", SqlEngine::MySql));
        assert!(allowed("SELECT [drop] FROM t", SqlEngine::Sqlite));
        assert!(allowed("SHOW TABLES", SqlEngine::MySql));
        assert!(allowed("DESCRIBE orders", SqlEngine::MySql));
        assert!(allowed(
            "SELECT replace(a, 'x', 'y') FROM t",
            SqlEngine::MySql
        ));
    }

    #[test]
    fn test_stacked_statements_are_refused() {
        for engine in ALL {
            for sql in [
                "SELECT 1; SELECT 2",
                "SELECT 1; DELETE FROM t",
                "SELECT 1;\n SELECT 2",
                ";SELECT 1",
                "SELECT 1;;SELECT 2",
            ] {
                assert!(!allowed(sql, engine), "{engine:?}: {sql}");
            }
        }
        // Comments are refused outright, so nothing can hide behind one.
        assert!(!allowed("SELECT 1--1; DELETE FROM t", SqlEngine::MySql));
        assert!(!allowed("SELECT 1 /* a */", SqlEngine::Postgres));
    }

    #[test]
    fn test_writes_ddl_and_session_statements_are_refused() {
        for engine in ALL {
            for sql in [
                "INSERT INTO t VALUES (1)",
                "UPDATE t SET a = 1",
                "DELETE FROM t",
                "DROP TABLE t",
                "CREATE TABLE x (a int)",
                "ALTER TABLE t ADD b int",
                "TRUNCATE t",
                "SET search_path = x",
                "BEGIN",
                "COMMIT",
                "ROLLBACK",
                "PRAGMA writable_schema = 1",
                "ATTACH DATABASE '/tmp/x' AS x",
                "GRANT SELECT ON t TO u",
                "COPY t TO PROGRAM 'id'",
                "CALL p()",
                "DO $$ BEGIN END $$",
                "LOAD DATA INFILE 'x' INTO TABLE t",
                "WITH d AS (DELETE FROM t RETURNING *) SELECT * FROM d",
                "SELECT * FROM t FOR UPDATE",
                "EXPLAIN ANALYZE SELECT 1",
                "explain analyse select 1",
                "EXPLAIN (FORMAT JSON, ANALYZE) SELECT 1",
                "EXPLAIN (ANALYZE true) DELETE FROM t",
                "DESCRIBE ANALYZE SELECT 1",
            ] {
                assert!(!allowed(sql, engine), "{engine:?}: {sql}");
            }
        }
        assert!(!allowed(
            "SELECT * INTO OUTFILE '/tmp/x' FROM t",
            SqlEngine::MySql
        ));
        assert!(!allowed(
            "SELECT a FROM t INTO DUMPFILE '/tmp/x'",
            SqlEngine::MySql
        ));
    }

    #[test]
    fn test_denied_functions_are_refused_even_quoted() {
        for (sql, engine) in [
            ("SELECT pg_read_file('/etc/passwd')", SqlEngine::Postgres),
            (
                "SELECT \"pg_read_file\"('/etc/passwd')",
                SqlEngine::Postgres,
            ),
            ("SELECT pg_catalog.pg_read_file('x')", SqlEngine::Postgres),
            ("SELECT lo_export(1, '/tmp/x')", SqlEngine::Postgres),
            ("SELECT dblink_exec('db', 'DELETE')", SqlEngine::Postgres),
            ("SELECT pg_terminate_backend(1)", SqlEngine::Postgres),
            ("SELECT set_config('a', 'b', false)", SqlEngine::Postgres),
            ("SELECT pg_advisory_lock(1)", SqlEngine::Postgres),
            ("SELECT nextval('s')", SqlEngine::Postgres),
            ("SELECT LOAD_FILE('/etc/passwd')", SqlEngine::MySql),
            ("SELECT GET_LOCK('x', 1)", SqlEngine::MySql),
            ("SELECT load_extension('x')", SqlEngine::Sqlite),
            ("SELECT writefile('x', 'y')", SqlEngine::Sqlite),
        ] {
            assert!(!allowed(sql, engine), "{engine:?}: {sql}");
        }
        // A column of that name (no call) is fine.
        assert!(allowed("SELECT nextval FROM t", SqlEngine::Postgres));
    }

    #[test]
    fn test_postgres_unicode_escaped_names_and_strings_are_refused() {
        for sql in [
            "SELECT U&\"pg_read_file\"('/etc/passwd')",
            "SELECT u&\"\\0070g_read_file\"('x')",
            "SELECT U&\"!0070g_read_file\" UESCAPE '!' ('x')",
            "SELECT U&'\\0041'",
        ] {
            assert!(!allowed(sql, SqlEngine::Postgres), "{sql}");
        }
        // A doubled quote inside a quoted name is one quote character.
        assert!(!allowed(
            "SELECT \"set_config\"('a', 'b', false)",
            SqlEngine::Postgres
        ));
        assert!(allowed("SELECT \"a\"\"b\" FROM t", SqlEngine::Postgres));
        // A name split from its parenthesis by a comment is still a call.
        assert!(!allowed(
            "SELECT pg_read_file /* x */ ('x')",
            SqlEngine::Postgres
        ));
    }

    #[test]
    fn test_postgres_functions_that_run_query_text_are_refused() {
        for name in [
            "ts_stat",
            "ts_rewrite",
            "query_to_xmlschema",
            "cursor_to_xmlschema",
            "table_to_xmlschema",
            "table_to_xml_and_xmlschema",
            "database_to_xml",
            "database_to_xmlschema",
            "database_to_xml_and_xmlschema",
            "schema_to_xml",
            "schema_to_xmlschema",
            "schema_to_xml_and_xmlschema",
        ] {
            assert!(
                !allowed(&format!("SELECT {name}('x')"), SqlEngine::Postgres),
                "{name}"
            );
        }
    }

    #[test]
    fn test_show_statements_may_name_write_keywords() {
        for sql in [
            "SHOW CREATE TABLE orders",
            "SHOW CREATE VIEW v",
            "SHOW GRANTS",
        ] {
            assert!(allowed(sql, SqlEngine::MySql), "{sql}");
        }
        // Only a leading SHOW is exempt.
        assert!(!allowed(
            "SELECT 1 FROM t WHERE x = 'SHOW' AND create = 1",
            SqlEngine::MySql
        ));
    }

    #[test]
    fn test_show_with_a_filter_is_checked_like_any_statement() {
        for sql in [
            "SHOW TABLES WHERE `Tables_in_app` IN (SELECT 1 INTO OUTFILE '/tmp/x')",
            "SHOW DATABASES WHERE `Database` = LOAD_FILE('/etc/passwd')",
            "SHOW TABLES LIKE 'x' UNION SELECT 1 INTO DUMPFILE '/tmp/y'",
            "SHOW CREATE TABLE t WHERE 1 = (SELECT COUNT(*) FROM u FOR UPDATE)",
            "SHOW COLUMNS FROM t WHERE Field IN (SELECT get_lock('x', 1))",
        ] {
            assert!(!allowed(sql, SqlEngine::MySql), "{sql}");
        }
    }

    #[test]
    fn test_backslashes_are_refused_whatever_the_string_setting() {
        // With standard_conforming_strings off the server would end the
        // first string at `\'` and run the call; the gate refuses every
        // backslash, and the connection also turns the setting on.
        for sql in [
            "SELECT '\\'', 1, '\\''",
            "SELECT '\\'' , pg_terminate_backend(1) --'",
        ] {
            assert!(!allowed(sql, SqlEngine::Postgres), "{sql}");
        }
    }

    #[test]
    fn test_executable_comments_and_unterminated_input_are_refused() {
        assert!(!allowed("SELECT 1 /*! ; DROP TABLE t */", SqlEngine::MySql));
        assert!(!allowed(
            "SELECT 1 /*M! ; DROP TABLE t */",
            SqlEngine::MySql
        ));
        assert!(!allowed("SELECT 1 /*!50000 , 2 */", SqlEngine::MySql));
        for engine in ALL {
            for sql in [
                "SELECT 'open",
                "SELECT 1 /* open",
                "",
                "  -- only a comment",
            ] {
                assert!(!allowed(sql, engine), "{engine:?}: {sql:?}");
            }
        }
        assert!(!allowed("SELECT $x$ open", SqlEngine::Postgres));
        // A backslash does not escape in a standard Postgres string, so the
        // second quote closes it and the `;` is real.
        assert!(!allowed(
            "SELECT 'a\\'; DELETE FROM t; --'",
            SqlEngine::Postgres
        ));
    }
}
