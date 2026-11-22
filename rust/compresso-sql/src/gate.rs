//! The statement gate: defence in depth in front of the database's own
//! read-only session, never the only control.
//!
//! A small per-dialect lexer skips strings, quoted identifiers, and comments
//! and then requires:
//!
//! - exactly one statement: nothing but whitespace and comments after a
//!   single trailing `;`, and no unterminated string or comment;
//! - a leading keyword from a short allow-list (`SELECT`, `WITH`, `VALUES`,
//!   `TABLE`, `SHOW`, `EXPLAIN`/`DESCRIBE` without `ANALYZE`);
//! - no write or DDL keyword anywhere (so a data-modifying `WITH` or a
//!   `SELECT ... INTO OUTFILE` is refused before it reaches the server);
//! - no call to a function on the per-engine deny-list (server file access,
//!   other-session control, cross-database links, session-level locks).
//!
//! MySQL executable comments (`/*! ... */`, MariaDB `/*M! ... */`) run their
//! contents, so they are refused rather than skipped. A keyword written as a
//! quoted identifier is an identifier, not a keyword, which is how a column
//! literally named `update` can still be selected. Postgres Unicode-escaped
//! names and strings (`U&"..."`, `UESCAPE`) are refused because this lexer
//! does not decode them.
//!
//! The function deny-list is a text list and therefore incomplete by nature:
//! a view, a wrapper function, or a function it does not name can still be
//! reached. It is not a boundary. The controls that hold are the database's
//! read-only session, the refusal of privileged roles, and the deadlines;
//! what remains reachable under them is listed in ADR-0036.
//!
//! MySQL is lexed under the default quoting rules; the connection removes
//! `ANSI_QUOTES`, `NO_BACKSLASH_ESCAPES`, and `ANSI` from the session's
//! `sql_mode` and confirms it before any statement reaches this gate.

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
    if matches!(keyword.as_str(), "EXPLAIN" | "DESCRIBE" | "DESC") && explains_with_analyze(first) {
        return Err("EXPLAIN ANALYZE runs the statement; use EXPLAIN without ANALYZE".to_owned());
    }
    for token in first {
        if let Token::Word(word) = token
            && DENIED_WORDS.contains(&word.as_str())
        {
            return Err(format!(
                "`{word}` is not allowed in a read-only query (quote an identifier with that name)"
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
    tokens
        .iter()
        .any(|token| matches!(token, Token::Word(word) if word == "ANALYZE" || word == "ANALYSE"))
}

fn lex(sql: &str, engine: SqlEngine) -> Result<Vec<Token>, String> {
    let chars = sql.chars().collect::<Vec<_>>();
    let mut tokens = Vec::new();
    let mut index = 0;
    let at = |index: usize| chars.get(index).copied();
    while let Some(character) = at(index) {
        match character {
            c if c.is_whitespace() => index += 1,
            '-' if at(index + 1) == Some('-')
                && (engine != SqlEngine::MySql
                    || at(index + 2)
                        .is_none_or(|next| next.is_whitespace() || next.is_control())) =>
            {
                index = skip_line(&chars, index);
            }
            '#' if engine == SqlEngine::MySql => index = skip_line(&chars, index),
            '/' if at(index + 1) == Some('*') => {
                if engine == SqlEngine::MySql
                    && (at(index + 2) == Some('!')
                        || (at(index + 2) == Some('M') && at(index + 3) == Some('!')))
                {
                    return Err(
                        "MySQL executable comments (/*! ... */) are refused because the server runs their contents".to_owned(),
                    );
                }
                index = skip_block_comment(&chars, index, engine == SqlEngine::Postgres)?;
            }
            '\'' => {
                let backslashes = engine == SqlEngine::MySql
                    || (engine == SqlEngine::Postgres
                        && index > 0
                        && matches!(at(index - 1), Some('e' | 'E'))
                        && !at(index.wrapping_sub(2)).is_some_and(is_word_char));
                index = skip_quoted(&chars, index, '\'', backslashes)?;
                tokens.push(Token::Other);
            }
            '"' => {
                let end = skip_quoted(&chars, index, '"', engine == SqlEngine::MySql)?;
                // MySQL treats "..." as a string by default; elsewhere it is
                // an identifier.
                if engine == SqlEngine::MySql {
                    tokens.push(Token::Other);
                } else {
                    tokens.push(Token::Quoted(quoted_text(&chars, index, end)));
                }
                index = end;
            }
            '`' if engine != SqlEngine::Postgres => {
                let end = skip_quoted(&chars, index, '`', false)?;
                tokens.push(Token::Quoted(quoted_text(&chars, index, end)));
                index = end;
            }
            '[' if engine == SqlEngine::Sqlite => {
                let end = chars[index + 1..]
                    .iter()
                    .position(|c| *c == ']')
                    .map(|offset| index + offset + 2)
                    .ok_or_else(|| "an identifier in [ ] is not closed".to_owned())?;
                tokens.push(Token::Quoted(quoted_text(&chars, index, end)));
                index = end;
            }
            '$' if engine == SqlEngine::Postgres => {
                index = skip_dollar(&chars, index, &mut tokens)?;
            }
            ';' => {
                tokens.push(Token::Semicolon);
                index += 1;
            }
            '(' => {
                tokens.push(Token::OpenParen);
                index += 1;
            }
            c if is_word_start(c) => {
                let start = index;
                while at(index).is_some_and(is_word_char) {
                    index += 1;
                }
                let word = chars[start..index]
                    .iter()
                    .collect::<String>()
                    .to_ascii_uppercase();
                // `U&"..."` and `U&'...'` spell names and strings with escapes
                // this lexer does not decode, so a denied name could hide in
                // one: refuse them outright.
                if engine == SqlEngine::Postgres
                    && (word == "UESCAPE"
                        || (word == "U"
                            && at(index) == Some('&')
                            && matches!(at(index + 1), Some('"' | '\''))))
                {
                    return Err(
                        "Unicode-escaped names and strings (U&\"...\") are refused; write the name plainly"
                            .to_owned(),
                    );
                }
                // `E'...'` strings: the quote is lexed next with escapes.
                tokens.push(Token::Word(word));
            }
            _ => {
                tokens.push(Token::Other);
                index += 1;
            }
        }
    }
    // `E'...'` leaves a Word("E") before its string; it is never a keyword.
    tokens.retain(|token| !matches!(token, Token::Word(word) if word == "E"));
    Ok(tokens)
}

fn is_word_start(character: char) -> bool {
    character.is_alphabetic() || character == '_'
}

fn is_word_char(character: char) -> bool {
    character.is_alphanumeric() || character == '_' || character == '$'
}

fn skip_line(chars: &[char], index: usize) -> usize {
    chars[index..]
        .iter()
        .position(|c| *c == '\n')
        .map_or(chars.len(), |offset| index + offset + 1)
}

fn skip_block_comment(chars: &[char], index: usize, nested: bool) -> Result<usize, String> {
    let mut depth = 0usize;
    let mut cursor = index;
    while cursor < chars.len() {
        if chars[cursor] == '/' && chars.get(cursor + 1) == Some(&'*') {
            if depth == 0 || nested {
                depth += 1;
            }
            cursor += 2;
        } else if chars[cursor] == '*' && chars.get(cursor + 1) == Some(&'/') {
            depth -= 1;
            cursor += 2;
            if depth == 0 {
                return Ok(cursor);
            }
        } else {
            cursor += 1;
        }
    }
    Err("a /* comment is not closed".to_owned())
}

/// Skips a quoted run starting at `index`; a doubled quote is an escaped
/// quote, and with `backslashes` a backslash escapes the next character.
fn skip_quoted(
    chars: &[char],
    index: usize,
    quote: char,
    backslashes: bool,
) -> Result<usize, String> {
    let mut cursor = index + 1;
    while cursor < chars.len() {
        let character = chars[cursor];
        if backslashes && character == '\\' {
            cursor += 2;
        } else if character == quote {
            if chars.get(cursor + 1) == Some(&quote) {
                cursor += 2;
            } else {
                return Ok(cursor + 1);
            }
        } else {
            cursor += 1;
        }
    }
    Err("a quoted string or identifier is not closed".to_owned())
}

/// A quoted identifier's name as the server reads it: the doubled closing
/// quote is one quote character. Lower-cased, so a quoted name that differs
/// from a denied one only in case is refused too (over-refusing is safe).
fn quoted_text(chars: &[char], start: usize, end: usize) -> String {
    let quote = chars[start];
    let doubled = format!("{quote}{quote}");
    chars[start + 1..end.saturating_sub(1).max(start + 1)]
        .iter()
        .collect::<String>()
        .replace(&doubled, &quote.to_string())
        .to_lowercase()
}

/// A Postgres `$tag$ ... $tag$` string, or a `$1` parameter.
fn skip_dollar(chars: &[char], index: usize, tokens: &mut Vec<Token>) -> Result<usize, String> {
    let mut cursor = index + 1;
    if chars.get(cursor).is_some_and(char::is_ascii_digit) {
        while chars.get(cursor).is_some_and(char::is_ascii_digit) {
            cursor += 1;
        }
        tokens.push(Token::Other);
        return Ok(cursor);
    }
    while chars
        .get(cursor)
        .is_some_and(|c| c.is_alphanumeric() || *c == '_')
    {
        cursor += 1;
    }
    if chars.get(cursor) != Some(&'$') {
        tokens.push(Token::Other);
        return Ok(index + 1);
    }
    let tag = &chars[index..=cursor];
    let body = cursor + 1;
    let mut search = body;
    while search + tag.len() <= chars.len() {
        if &chars[search..search + tag.len()] == tag {
            tokens.push(Token::Other);
            return Ok(search + tag.len());
        }
        search += 1;
    }
    Err("a $$-quoted string is not closed".to_owned())
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
                "  -- note\nSELECT a FROM t /* trailing */ ;  -- done",
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
        assert!(allowed("SELECT $$a;b$$, $tag$x$tag$", SqlEngine::Postgres));
        assert!(allowed("SELECT E'it\\'s; still one'", SqlEngine::Postgres));
        assert!(allowed("SELECT 'a\\';b'", SqlEngine::MySql));
        assert!(allowed(
            "SELECT 1 /*+ MAX_EXECUTION_TIME(10) */",
            SqlEngine::MySql
        ));
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
                "SELECT 1;\n/* c */ SELECT 2",
                ";SELECT 1",
                "SELECT 1;;SELECT 2",
            ] {
                assert!(!allowed(sql, engine), "{engine:?}: {sql}");
            }
        }
        // MySQL `--x` is not a comment, so the `;` after it is real.
        assert!(!allowed("SELECT 1--1; DELETE FROM t", SqlEngine::MySql));
        assert!(!allowed("SELECT 1 # c\n; SELECT 2", SqlEngine::MySql));
        // Postgres block comments nest.
        assert!(!allowed(
            "SELECT 1 /* a /* b */ ; */ ; SELECT 2",
            SqlEngine::Postgres
        ));
        assert!(allowed("SELECT 1 /* a /* b */ ; */", SqlEngine::Postgres));
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
