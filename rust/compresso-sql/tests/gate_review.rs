//! Statements an independent review got past the first statement gate
//! (E17-50 review, 2026-11-22). The gate now accepts only a strict subset of
//! SQL: anything whose lexing differs between engines or settings is refused
//! rather than interpreted, and the refusal names the construct.

use compresso_sql::SqlEngine::{self, MySql, Postgres, Sqlite};
use compresso_sql::gate::check_statement;

fn refused(sql: &str, engine: SqlEngine, construct: &str) {
    let error = check_statement(sql, engine).expect_err(sql);
    assert!(
        error.contains(construct),
        "{engine:?}: {sql:?} refused for another reason: {error}"
    );
}

#[test]
fn test_unicode_whitespace_after_dashes_hides_nothing_in_mysql() {
    for sql in [
        "SELECT 1 --\u{a0}, GET_LOCK('x', 10), SLEEP(30) FROM (SELECT 1 AS `\u{a0}`) t",
        "SELECT 1 --\u{a0} INTO OUTFILE '/tmp/x'",
        "SELECT * FROM t --\u{85} FOR UPDATE",
        "SELECT 1 --\u{a0}; DELETE FROM t",
    ] {
        refused(sql, MySql, "comment");
    }
}

#[test]
fn test_postgres_dollar_quotes_with_any_tag_are_refused() {
    for sql in [
        "SELECT $\u{20ac}$'$\u{20ac}$, pg_terminate_backend(pid), set_config('statement_timeout','0',false) FROM pg_stat_activity --'",
        "SELECT $\u{a0}$'$\u{a0}$, dblink_exec('x','DELETE FROM t') --'",
        "SELECT $\u{20ac}$ pg_sleep $\u{20ac}$",
        "SELECT $$a$$",
        "SELECT $tag$a$tag$",
    ] {
        refused(sql, Postgres, "$");
    }
}

#[test]
fn test_optimizer_hints_and_every_comment_form_are_refused() {
    for (sql, engine) in [
        (
            "SELECT /*+ MAX_EXECUTION_TIME(3600000) */ SLEEP(3600)",
            MySql,
        ),
        (
            "SELECT /*+ SET_VAR(max_execution_time=0) */ BENCHMARK(1e12, MD5('a'))",
            MySql,
        ),
        ("SELECT 1 /*! , 2 */", MySql),
        ("SELECT 1 # note", MySql),
        ("SELECT 1 -- note", MySql),
        ("SELECT 1 /* /* */ , pg_sleep(1) */", Postgres),
        ("SELECT 1 -- note", Postgres),
        ("SELECT 1 /* note */", Sqlite),
    ] {
        refused(sql, engine, "comment");
    }
}

#[test]
fn test_prefixed_and_escaped_strings_are_refused() {
    for (sql, engine) in [
        ("SELECT 1e'\\'' , pg_terminate_backend(1) --'", Postgres),
        ("SELECT E'a'", Postgres),
        ("SELECT N'a'", MySql),
        ("SELECT X'00'", MySql),
        ("SELECT _utf8mb4'a'", MySql),
        ("SELECT B'1'", Postgres),
    ] {
        refused(sql, engine, "prefixed string");
    }
    refused("SELECT U&'\\0041'", Postgres, "prefixed string");
    refused("SELECT U&\"x\"", Postgres, "prefixed");
    for (sql, engine) in [
        ("SELECT '\u{4e2d}\\' , GET_LOCK('x',1), '", MySql),
        ("SELECT 'a\\'b'", MySql),
        ("SELECT 'a\\b'", Postgres),
    ] {
        refused(sql, engine, "backslash");
    }
    refused("SELECT 0x41", MySql, "hex");
}

#[test]
fn test_non_ascii_and_control_characters_outside_strings_are_refused() {
    refused("SELECT pg_read_file\u{a0}('x')", Postgres, "non-ASCII");
    refused("SELECT \"caf\u{e9}\" FROM t", Postgres, "non-ASCII");
    refused("SELECT `\u{a0}` FROM t", MySql, "non-ASCII");
    refused("SELECT\u{2003}1", Sqlite, "non-ASCII");
    refused("SELECT 1\0; DELETE FROM t", Sqlite, "control");
    refused("SELECT 'a\u{7}'", MySql, "control");
    refused("SELECT\u{b}1", Postgres, "control");
}

#[test]
fn test_plain_sql_still_passes() {
    for engine in [Sqlite, Postgres, MySql] {
        for sql in [
            "SELECT 1",
            "select id, name from orders where note = 'it''s caf\u{e9}' order by id;",
            "WITH x AS (SELECT 1 AS n) SELECT n FROM x",
            "SELECT count(*) FROM t WHERE a <> 2 AND b >= 1.5e3",
        ] {
            assert!(check_statement(sql, engine).is_ok(), "{engine:?}: {sql}");
        }
    }
    assert!(check_statement("SELECT \"order\" FROM t", Postgres).is_ok());
    assert!(check_statement("SELECT `order` FROM t", MySql).is_ok());
    assert!(check_statement("SELECT @a := 1", MySql).is_ok());
    assert!(check_statement("SHOW CREATE TABLE orders", MySql).is_ok());
}
