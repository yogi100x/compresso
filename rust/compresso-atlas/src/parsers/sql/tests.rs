//! E17-42 (D107): the SQL statement scanner and definition symbols.

use compresso_store::{SymbolKind, SymbolRecord};

use super::{
    Class, FILE_SYMBOL, MAX_SQL_COVERED_BYTES, MAX_SQL_SYMBOLS, Object, SCAN_STEPS,
    SIGNATURE_BYTES, classify, each_statement, extract,
};
use crate::parsers::MAX_SIGNATURE_BYTES;

/// Each statement's line range and first token, lowercased.
fn statements(source: &str) -> Vec<(u32, u32, String)> {
    let mut found = Vec::new();
    each_statement(source, |statement| {
        let first = statement
            .head
            .first()
            .map(|token| source[token.start..token.end].to_ascii_lowercase())
            .unwrap_or_default();
        found.push((statement.start_line, statement.end_line, first));
    });
    found
}

fn firsts(source: &str) -> Vec<String> {
    statements(source)
        .into_iter()
        .map(|(_, _, first)| first)
        .collect()
}

/// The definition symbols, without the file symbol.
fn symbols(source: &str) -> Vec<SymbolRecord> {
    extract(source)
        .symbols
        .into_iter()
        .filter(|symbol| !is_file_symbol(symbol))
        .collect()
}

fn is_file_symbol(symbol: &SymbolRecord) -> bool {
    super::is_file_symbol(symbol)
}

fn qualified_names(source: &str) -> Vec<String> {
    symbols(source)
        .into_iter()
        .map(|symbol| symbol.qualified_name)
        .collect()
}

fn symbol(source: &str, qualified_name: &str) -> SymbolRecord {
    symbols(source)
        .into_iter()
        .find(|symbol| symbol.qualified_name == qualified_name)
        .unwrap_or_else(|| panic!("no {qualified_name} in {:?}", qualified_names(source)))
}

fn line_count(source: &str) -> u32 {
    u32::try_from(source.lines().count()).expect("small")
}

#[test]
fn test_semicolons_in_comments_strings_and_quoted_names_do_not_split() {
    let source = "-- a; comment\nCREATE TABLE a (x text DEFAULT 'it''s; here');\n/* b; */ CREATE TABLE \"we;ird\" (y int);\nSELECT 1;\n";
    assert_eq!(firsts(source), ["create", "create", "select"]);
    assert_eq!(qualified_names(source), ["a", "we;ird"]);
}

#[test]
fn test_nested_block_comments_hide_semicolons() {
    let source = "/* outer /* inner; */ still a comment; */ CREATE TABLE a (x int);\nSELECT 1;\n";
    assert_eq!(firsts(source), ["create", "select"]);
}

#[test]
fn test_dollar_quoted_bodies_hide_semicolons_quotes_and_other_tags() {
    let source = "CREATE FUNCTION f() RETURNS int AS $$ SELECT 1; SELECT ' $$;\nCREATE FUNCTION g() RETURNS void AS $body$\nBEGIN\n  EXECUTE $q$ DROP TABLE x; $q$; -- $$ inside\nEND;\n$body$ LANGUAGE plpgsql;\nSELECT 2;\n";
    let found = statements(source);
    assert_eq!(found.len(), 3, "{found:?}");
    assert_eq!(found[1], (2, 6, "create".to_owned()));
    assert_eq!(qualified_names(source), ["f", "g"]);
}

#[test]
fn test_positional_parameters_and_dollar_in_names_are_not_quotes() {
    let source = "PREPARE p AS SELECT $1;\nCREATE TABLE a$b (x int);\nSELECT 2;\n";
    assert_eq!(firsts(source), ["prepare", "create", "select"]);
    assert_eq!(qualified_names(source), ["a$b"]);
}

#[test]
fn test_backslash_escapes_only_in_e_strings() {
    let source = "SELECT E'a\\'; b';\nSELECT 'c\\';\nSELECT 2;\n";
    assert_eq!(statements(source).len(), 3, "{:?}", statements(source));
    assert_eq!(statements(source)[1].0, 2);
}

#[test]
fn test_begin_atomic_bodies_are_one_statement_and_transactions_are_not_blocks() {
    let source = "BEGIN;\nCREATE FUNCTION f() RETURNS int LANGUAGE sql\nBEGIN ATOMIC\n  SELECT 1;\n  SELECT CASE WHEN true THEN 2 END;\nEND;\nCREATE TABLE t (a int);\nCOMMIT;\nDO $$ BEGIN PERFORM 1; END $$;\nEND;\n";
    let found = statements(source);
    assert_eq!(
        found
            .iter()
            .map(|(start, end, first)| (*start, *end, first.as_str()))
            .collect::<Vec<_>>(),
        [
            (1, 1, "begin"),
            (2, 6, "create"),
            (7, 7, "create"),
            (8, 8, "commit"),
            (9, 9, "do"),
            (10, 10, "end"),
        ]
    );
}

#[test]
fn test_unquoted_trigger_bodies_close_on_their_own_end() {
    let source = "CREATE TRIGGER t AFTER INSERT ON a BEGIN\n  IF 1 THEN SELECT 1; END IF;\n  UPDATE b SET c = 1;\nEND;\nSELECT 2;\n";
    let found = statements(source);
    assert_eq!(found.len(), 2, "{found:?}");
    assert_eq!((found[0].0, found[0].1), (1, 4));
    assert_eq!(symbol(source, "a::t").end_line, 4);
}

#[test]
fn test_semicolons_inside_parentheses_do_not_split() {
    let source = "CREATE RULE r AS ON INSERT TO t DO INSTEAD (\n  INSERT INTO a VALUES (1);\n  INSERT INTO b VALUES (2)\n);\nSELECT 1;\n";
    assert_eq!(firsts(source), ["create", "select"]);
    let rule = symbol(source, "t::r");
    assert_eq!(
        (rule.kind, rule.start_line, rule.end_line),
        (SymbolKind::Method, 1, 4)
    );
}

#[test]
fn test_copy_data_is_skipped_to_its_terminator_line() {
    let source = "COPY public.t (a, b) FROM stdin;\n1\tit's; $$ /* x\n2\t\\N\n\\.\nCREATE TABLE after_copy (x int);\n";
    assert_eq!(firsts(source), ["copy", "create"]);
    let table = symbol(source, "after_copy");
    assert_eq!((table.start_line, table.end_line), (5, 5));
}

#[test]
fn test_psql_meta_commands_end_at_their_newline() {
    let source = "\\connect db\n\\set ON_ERROR_STOP on\n\\echo 'it;s\nCREATE TABLE a (x int);\nSELECT 1 \\gset\nCREATE TABLE b (y int);\n";
    let found = statements(source);
    assert_eq!(
        found
            .iter()
            .map(|(start, _, first)| (*start, first.as_str()))
            .collect::<Vec<_>>(),
        [
            (1, "\\connect db"),
            (2, "\\set on_error_stop on"),
            (3, "\\echo 'it;s"),
            (4, "create"),
            (5, "select"),
            (5, "\\gset"),
            (6, "create"),
        ]
    );
    assert_eq!(qualified_names(source), ["a", "b"]);
}

#[test]
fn test_unterminated_quotes_comments_and_bodies_run_to_the_end_without_failing() {
    for opener in ["'", "E'\\", "\"", "`", "/*", "$tag$", "$$"] {
        let source =
            format!("CREATE TABLE a (x int);\nSELECT {opener} oops;\nCREATE TABLE b (y int);\n");
        let found = statements(&source);
        assert_eq!(qualified_names(&source), ["a"], "{opener}");
        assert!(
            found
                .iter()
                .all(|(start, end, _)| start <= end && *end <= line_count(&source)),
            "{opener}: {found:?}"
        );
    }
    let source = "COPY t FROM stdin;\n1\n2\n";
    assert_eq!(firsts(source), ["copy"]);
}

#[test]
fn test_line_ranges_cover_the_whole_statement_with_its_body() {
    let source = "-- header\n\nCREATE OR REPLACE FUNCTION public.touch()\nRETURNS trigger\nAS $$\nBEGIN\n  NEW.updated_at = now();\n  RETURN NEW;\nEND;\n$$\nLANGUAGE plpgsql;\n";
    let function = symbol(source, "public.touch");
    assert_eq!((function.start_line, function.end_line), (3, 11));
}

#[test]
fn test_crlf_and_a_missing_final_semicolon_keep_lines_in_range() {
    let source = "CREATE TABLE a (x int);\r\nCREATE TABLE b (\r\n  y int\r\n)";
    let found = symbols(source);
    assert_eq!(found.len(), 2);
    assert_eq!((found[1].start_line, found[1].end_line), (2, 4));
    let source = "CREATE TABLE a (x text DEFAULT 'open\n";
    let found = symbols(source);
    assert_eq!((found[0].start_line, found[0].end_line), (1, 1));
}

#[test]
fn test_tables_views_and_their_modifiers() {
    let source = "CREATE TABLE public.users (id int);\nCREATE TEMP TABLE scratch (x int);\nCREATE UNLOGGED TABLE IF NOT EXISTS \"Mixed \"\"Case\"\"\" (x int);\nCREATE TABLE Accounts (id int);\nCREATE TABLE events_2024 PARTITION OF events FOR VALUES FROM ('2024-01-01') TO ('2025-01-01');\nCREATE FOREIGN TABLE remote_t (x int) SERVER s;\nCREATE OR REPLACE VIEW active_users AS SELECT * FROM users;\nCREATE MATERIALIZED VIEW IF NOT EXISTS stats.daily AS SELECT 1;\nCREATE RECURSIVE VIEW tree (id) AS SELECT 1;\n";
    let found = symbols(source);
    assert_eq!(
        found
            .iter()
            .map(|symbol| (
                symbol.kind,
                symbol.name.as_str(),
                symbol.qualified_name.as_str()
            ))
            .collect::<Vec<_>>(),
        [
            (SymbolKind::Struct, "users", "public.users"),
            (SymbolKind::Struct, "scratch", "scratch"),
            (
                SymbolKind::Struct,
                "Mixed \"Case\"",
                "\"Mixed \"\"Case\"\"\""
            ),
            (SymbolKind::Struct, "accounts", "accounts"),
            (SymbolKind::Struct, "events_2024", "events_2024"),
            (SymbolKind::Struct, "remote_t", "remote_t"),
            (SymbolKind::Struct, "active_users", "active_users"),
            (SymbolKind::Struct, "daily", "stats.daily"),
            (SymbolKind::Struct, "tree", "tree"),
        ]
    );
    let signatures = found
        .iter()
        .map(|symbol| symbol.signature.as_deref().unwrap_or_default())
        .collect::<Vec<_>>();
    assert_eq!(signatures[0], "CREATE TABLE public.users");
    assert_eq!(
        signatures[4],
        "CREATE TABLE events_2024 PARTITION OF events"
    );
    assert_eq!(signatures[6], "CREATE OR REPLACE VIEW active_users");
    assert_eq!(
        signatures[7],
        "CREATE MATERIALIZED VIEW IF NOT EXISTS stats.daily"
    );
    assert_eq!(signatures[8], "CREATE RECURSIVE VIEW tree (id)");
}

#[test]
fn test_functions_and_procedures_sign_with_arguments_and_returns() {
    let source = "CREATE OR REPLACE FUNCTION public.f(a integer, b text DEFAULT 'x;y')\n  RETURNS TABLE(id int, name text)\n  LANGUAGE plpgsql SECURITY DEFINER\nAS $$ BEGIN RETURN QUERY SELECT 1, 'a'; END $$;\nCREATE FUNCTION public.f(a integer) RETURNS SETOF users STABLE AS 'SELECT * FROM users' LANGUAGE sql;\nCREATE PROCEDURE p(IN x int) LANGUAGE sql AS $$ SELECT 1 $$;\nCREATE FUNCTION add(a int, b int) RETURNS double precision IMMUTABLE RETURN a + b;\nCREATE AGGREGATE total(int) (SFUNC = add, STYPE = int);\n";
    let found = symbols(source);
    assert_eq!(
        found
            .iter()
            .map(|symbol| (
                symbol.kind,
                symbol.qualified_name.as_str(),
                symbol.signature.as_deref().unwrap_or_default()
            ))
            .collect::<Vec<_>>(),
        [
            (
                SymbolKind::Function,
                "public.f",
                "CREATE OR REPLACE FUNCTION public.f(a integer, b text DEFAULT 'x;y') RETURNS TABLE(id int, name text)"
            ),
            (
                SymbolKind::Function,
                "public.f",
                "CREATE FUNCTION public.f(a integer) RETURNS SETOF users"
            ),
            (SymbolKind::Function, "p", "CREATE PROCEDURE p(IN x int)"),
            (
                SymbolKind::Function,
                "add",
                "CREATE FUNCTION add(a int, b int) RETURNS double precision"
            ),
            (SymbolKind::Function, "total", "CREATE AGGREGATE total(int)"),
        ]
    );
    assert_eq!((found[0].start_line, found[0].end_line), (1, 4));
}

#[test]
fn test_triggers_indexes_policies_and_rules_are_named_by_their_table() {
    let source = "CREATE TRIGGER set_updated BEFORE UPDATE ON public.users FOR EACH ROW EXECUTE FUNCTION touch();\nCREATE CONSTRAINT TRIGGER check_it AFTER INSERT OR UPDATE ON orders DEFERRABLE FOR EACH ROW EXECUTE PROCEDURE check_order();\nCREATE OR REPLACE TRIGGER t2 AFTER DELETE ON ONLY orders FOR EACH STATEMENT EXECUTE FUNCTION f();\nCREATE EVENT TRIGGER guard ON ddl_command_start EXECUTE FUNCTION g();\nCREATE UNIQUE INDEX CONCURRENTLY IF NOT EXISTS users_email ON ONLY public.users USING btree (lower(email));\nCREATE INDEX ON users (created_at);\nCREATE POLICY \"Users read own\" ON public.users FOR SELECT USING (auth.uid() = id);\nCREATE POLICY p ON orders AS RESTRICTIVE USING (true);\n";
    let found = symbols(source);
    assert_eq!(
        found
            .iter()
            .map(|symbol| (
                symbol.kind,
                symbol.name.as_str(),
                symbol.qualified_name.as_str()
            ))
            .collect::<Vec<_>>(),
        [
            (
                SymbolKind::Method,
                "set_updated",
                "public.users::set_updated"
            ),
            (SymbolKind::Method, "check_it", "orders::check_it"),
            (SymbolKind::Method, "t2", "orders::t2"),
            (SymbolKind::Function, "guard", "guard"),
            (
                SymbolKind::Const,
                "users_email",
                "public.users::users_email"
            ),
            (
                SymbolKind::Method,
                "Users read own",
                "public.users::\"Users read own\""
            ),
            (SymbolKind::Method, "p", "orders::p"),
        ]
    );
    assert_eq!(
        found[5].signature.as_deref(),
        Some("CREATE POLICY \"Users read own\" ON public.users FOR SELECT USING (auth.uid() = id)")
    );
}

#[test]
fn test_types_domains_schemas_extensions_sequences_and_publications() {
    let source = "CREATE TYPE status AS ENUM ('active', 'closed');\nCREATE TYPE public.pair AS (a int, b int);\nCREATE TYPE span AS RANGE (subtype = int);\nCREATE DOMAIN email AS text CHECK (VALUE LIKE '%@%');\nCREATE SCHEMA IF NOT EXISTS billing;\nCREATE SCHEMA AUTHORIZATION joe;\nCREATE EXTENSION IF NOT EXISTS \"uuid-ossp\" WITH SCHEMA extensions;\nCREATE SEQUENCE IF NOT EXISTS public.counter START 1;\nCREATE PUBLICATION feed FOR TABLE users;\nCREATE ROLE reader;\nALTER TABLE users ADD COLUMN x int;\n";
    let found = symbols(source);
    assert_eq!(
        found
            .iter()
            .map(|symbol| (symbol.kind, symbol.qualified_name.as_str()))
            .collect::<Vec<_>>(),
        [
            (SymbolKind::Enum, "status"),
            (SymbolKind::Type, "public.pair"),
            (SymbolKind::Type, "span"),
            (SymbolKind::Type, "email"),
            (SymbolKind::Module, "billing"),
            (SymbolKind::Module, "joe"),
            (SymbolKind::Module, "uuid-ossp"),
            (SymbolKind::Const, "public.counter"),
            (SymbolKind::Const, "feed"),
        ]
    );
    assert_eq!(
        found[0].signature.as_deref(),
        Some("CREATE TYPE status AS ENUM ('active', 'closed')")
    );
}

#[test]
fn test_statements_are_classified_and_unnamed_definitions_are_reported() {
    let classes = {
        let source = "CREATE INDEX ON users (x);\nCREATE TABLE (x int);\nCREATE ROLE r;\nALTER TABLE t ADD y int;\nCREATE TRIGGER t BEFORE UPDATE FOR EACH ROW EXECUTE FUNCTION f();\n";
        let mut classes = Vec::new();
        each_statement(source, |statement| {
            classes.push(classify(source, statement))
        });
        classes
    };
    assert_eq!(
        classes,
        [
            Class::Unnamed(Object::Index),
            Class::Unnamed(Object::Table),
            Class::OtherCreate,
            Class::Other,
            Class::Unnamed(Object::Trigger),
        ]
    );
}

#[test]
fn test_empty_overlong_and_control_character_names_make_no_symbol() {
    let long = "n".repeat(2_000);
    let source = format!(
        "CREATE TABLE \"\" (x int);\nCREATE TABLE \"{long}\" (x int);\nCREATE TABLE {long} (x int);\nCREATE TABLE \"a\u{0}b\" (x int);\nCREATE POLICY ok ON t USING ('a\u{0}b' = x);\n"
    );
    let found = symbols(&source);
    assert_eq!(found.len(), 1, "{:?}", qualified_names(&source));
    assert!(
        !found[0]
            .signature
            .as_deref()
            .unwrap_or_default()
            .contains('\0')
    );
}

#[test]
fn test_signatures_are_bounded_and_marked_when_cut() {
    let arguments = (0..2_000)
        .map(|index| format!("a{index} integer"))
        .collect::<Vec<_>>()
        .join(", ");
    let source = format!(
        "CREATE FUNCTION wide({arguments}) RETURNS int AS $$ SELECT 1 $$ LANGUAGE sql;\nCREATE POLICY p ON t USING ('{}' = 'x');\n",
        "é".repeat(5_000)
    );
    for symbol in symbols(&source) {
        let signature = symbol.signature.expect("signature");
        assert!(
            signature.len() <= MAX_SIGNATURE_BYTES,
            "{}",
            signature.len()
        );
        assert!(signature.ends_with('…'), "{}", symbol.qualified_name);
    }
}

/// Hostile input is scanned in steps linear in its size, counted rather
/// than timed: unterminated openers, a megabyte-long line, 100,000
/// statements, deep nesting.
#[test]
fn test_hostile_input_is_scanned_in_linear_steps() {
    let cases = [
        "$a$ ".repeat(100_000),
        (0..100_000)
            .map(|index| format!("$t{index}$ "))
            .collect::<String>(),
        "$$ x ".repeat(100_000),
        "/* ".repeat(100_000),
        "' ".repeat(100_001),
        "E'\\' ".repeat(100_000),
        "\" ".repeat(100_001),
        "(".repeat(300_000),
        "CREATE TABLE a (x int);".repeat(40_000),
        "SELECT 1;\n".repeat(100_000),
        format!("COPY t FROM stdin;\n{}", "x;'$$\n".repeat(100_000)),
        "\\x\n".repeat(100_000),
        "CREATE FUNCTION f() BEGIN CASE END ".repeat(20_000),
        "DO $$ BEGIN CREATE TABLE a (x int); END $$;".repeat(20_000),
        format!("DO $$ {}", "CREATE TABLE a (x int);".repeat(40_000)),
        format!("DO $$ {}", "$x$ ".repeat(100_000)),
    ];
    for source in cases {
        SCAN_STEPS.with(|steps| steps.set(0));
        let mut statements = 0_usize;
        each_statement(&source, |_| statements += 1);
        let steps = SCAN_STEPS.with(std::cell::Cell::get);
        assert!(
            steps <= 4 * source.len() + 16,
            "{} bytes took {steps} steps",
            source.len()
        );
        // Extraction scans a top-level `DO` body a second time, once.
        SCAN_STEPS.with(|steps| steps.set(0));
        let _ = extract(&source);
        let steps = SCAN_STEPS.with(std::cell::Cell::get);
        assert!(
            steps <= 8 * source.len() + 16,
            "extraction of {} bytes took {steps} steps",
            source.len()
        );
    }
}

/// A file's symbols are capped in number and in the line bytes they cover
/// together, so the indexer's per-symbol token count stays bounded on one
/// megabyte-long line of definitions.
#[test]
fn test_symbols_are_capped_in_number_and_covered_bytes() {
    let one_line = "CREATE TABLE a (x int);".repeat(40_000);
    let found = symbols(&one_line);
    assert!(!found.is_empty());
    assert!(
        found.len() * one_line.len() <= MAX_SQL_COVERED_BYTES,
        "{}",
        found.len()
    );

    let many_lines = "CREATE TABLE a (x int);\n".repeat(MAX_SQL_SYMBOLS + 10);
    assert_eq!(extract(&many_lines).symbols.len(), MAX_SQL_SYMBOLS);
}

/// Signature text copied per statement is bounded by the signature cut,
/// however long the statement is.
#[test]
fn test_signatures_copy_a_bounded_prefix() {
    let source = format!("CREATE POLICY p ON t USING ('{}');\n", "x".repeat(900_000));
    SIGNATURE_BYTES.with(|bytes| bytes.set(0));
    let found = symbols(&source);
    assert_eq!(found.len(), 1);
    let copied = SIGNATURE_BYTES.with(std::cell::Cell::get);
    assert!(copied <= MAX_SIGNATURE_BYTES + 64, "{copied}");
}

/// A signature whose statement has more tokens than the head keeps is
/// marked cut even when its kept text is short.
#[test]
fn test_a_signature_cut_by_the_head_token_cap_is_marked() {
    let source = format!(
        "CREATE FUNCTION many({}) RETURNS int AS $$ SELECT 1 $$ LANGUAGE sql;\n",
        vec!["int"; 400].join(",")
    );
    let signature = symbol(&source, "many").signature.expect("signature");
    assert!(signature.len() < MAX_SIGNATURE_BYTES, "{}", signature.len());
    assert!(signature.ends_with('…'), "{signature}");
    // The kept text is the head's, not just its first word (review: the
    // head cut stopped the copy after one token).
    assert!(
        signature.starts_with("CREATE FUNCTION many(int,int,int"),
        "{signature}"
    );
}

/// Each reference as `(caller, callee, kind, line)`.
fn references(source: &str) -> Vec<(String, String, String, u32)> {
    let extraction = extract(source);
    extraction
        .calls
        .iter()
        .map(|call| {
            assert_eq!(call.form, compresso_store::CallForm::Plain);
            (
                extraction.symbols[call.caller].qualified_name.clone(),
                call.callee_name.clone(),
                call.qualifier.clone().unwrap_or_default(),
                call.line,
            )
        })
        .collect()
}

fn reference_set(source: &str) -> Vec<(String, String, String)> {
    references(source)
        .into_iter()
        .map(|(caller, callee, kind, _)| (caller, callee, kind))
        .collect()
}

fn row(caller: &str, callee: &str, kind: &str) -> (String, String, String) {
    (caller.to_owned(), callee.to_owned(), kind.to_owned())
}

/// Stage 2: one `script` module symbol covers a non-empty file, so grep
/// rows and the references of statements that define nothing have an
/// enclosing symbol.
#[test]
fn test_a_file_symbol_covers_the_whole_file() {
    let source = "-- notes\nALTER TABLE users ADD x int;\nCREATE TABLE a (x int);\n";
    let extraction = extract(source);
    let file = &extraction.symbols[0];
    assert!(is_file_symbol(file), "{file:?}");
    assert_eq!((file.start_line, file.end_line), (1, 3));
    assert_eq!(extract("CREATE TABLE a (x int)").symbols[0].end_line, 1);
    assert!(extract("  \n\t\n").symbols.is_empty());
}

#[test]
fn test_definitions_reference_their_table_trigger_function_and_parent() {
    let source = "CREATE INDEX books_title ON public.books (title);\nCREATE POLICY p ON books USING (true);\nCREATE TRIGGER t BEFORE UPDATE ON lending.loans FOR EACH ROW EXECUTE FUNCTION lending.touch();\nCREATE RULE r AS ON INSERT TO books DO NOTHING;\nCREATE TABLE loans (\n  id int,\n  book_id int REFERENCES books (id),\n  owner int REFERENCES public.owners\n);\nCREATE TABLE loans_2026 PARTITION OF loans FOR VALUES IN (2026);\nCREATE VIEW open_loans AS SELECT * FROM loans JOIN books ON true WHERE id IN (SELECT id FROM hidden);\n";
    assert_eq!(
        reference_set(source),
        [
            row("public.books::books_title", "books", "on"),
            row("books::p", "books", "on"),
            row("lending.loans::t", "loans", "on"),
            row("lending.loans::t", "touch", "execute"),
            row("books::r", "books", "on"),
            row("loans", "books", "references"),
            row("loans", "owners", "references"),
            row("loans_2026", "loans", "partition_of"),
            row("open_loans", "loans", "from"),
            row("open_loans", "books", "join"),
        ]
    );
    let lines = references(source)
        .into_iter()
        .filter(|(caller, ..)| caller == "loans")
        .map(|(.., line)| line)
        .collect::<Vec<_>>();
    assert_eq!(lines, [7, 8]);
}

#[test]
fn test_alter_drop_comment_grant_and_revoke_reference_their_objects() {
    let source = "ALTER TABLE IF EXISTS ONLY public.books ADD COLUMN pages int;\nALTER FUNCTION touch() OWNER TO admin;\nALTER POLICY p ON books USING (false);\nDROP TABLE IF EXISTS old_books, public.older CASCADE;\nDROP FUNCTION IF EXISTS touch(int), lending.touch();\nDROP POLICY IF EXISTS p ON public.books;\nDROP INDEX CONCURRENTLY books_title;\nDROP TYPE loan_state;\nCOMMENT ON TABLE books IS 'x';\nCOMMENT ON COLUMN public.books.title IS 'y';\nCOMMENT ON FUNCTION touch() IS 'z';\nGRANT SELECT, INSERT ON TABLE books, loans TO reader;\nGRANT EXECUTE ON FUNCTION touch(int) TO reader;\nREVOKE ALL ON books FROM public;\nGRANT SELECT ON ALL TABLES IN SCHEMA public TO reader;\nGRANT reader TO someone;\n";
    assert_eq!(
        reference_set(source),
        [
            row(FILE_SYMBOL, "books", "alter"),
            row(FILE_SYMBOL, "touch", "alter"),
            row(FILE_SYMBOL, "books", "alter"),
            row(FILE_SYMBOL, "old_books", "drop relation\told_books\t"),
            row(FILE_SYMBOL, "older", "drop relation\tpublic.older\t"),
            row(FILE_SYMBOL, "touch", "drop routine\ttouch\t"),
            row(FILE_SYMBOL, "p", "drop policy\tp\tpublic.books"),
            row(FILE_SYMBOL, "books_title", "drop index\tbooks_title\t"),
            row(FILE_SYMBOL, "loan_state", "drop type\tloan_state\t"),
            row(FILE_SYMBOL, "books", "comment"),
            row(FILE_SYMBOL, "books", "comment"),
            row(FILE_SYMBOL, "touch", "comment"),
            row(FILE_SYMBOL, "books", "grant"),
            row(FILE_SYMBOL, "loans", "grant"),
            row(FILE_SYMBOL, "touch", "grant"),
            row(FILE_SYMBOL, "books", "revoke"),
        ]
    );
}

#[test]
fn test_data_statements_and_top_level_from_and_join_are_best_effort_references() {
    let source = "INSERT INTO public.books (id) SELECT id FROM staging;\nUPDATE ONLY books SET t = 1 FROM extra WHERE a IS DISTINCT FROM b;\nDELETE FROM books USING other;\nTRUNCATE TABLE books, loans;\nCOPY books FROM stdin;\n1\n\\.\nSELECT count(*) FROM generate_series(1, 3), books WHERE x IN (SELECT 1 FROM nested);\nSELECT * FROM ONLY archived LEFT JOIN \"Mixed\" ON true;\nCALL lending.close_all();\nREFRESH MATERIALIZED VIEW CONCURRENTLY open_counts;\n";
    assert_eq!(
        reference_set(source),
        [
            row(FILE_SYMBOL, "books", "insert"),
            row(FILE_SYMBOL, "staging", "from"),
            row(FILE_SYMBOL, "books", "update"),
            row(FILE_SYMBOL, "extra", "from"),
            row(FILE_SYMBOL, "books", "delete"),
            row(FILE_SYMBOL, "books", "truncate"),
            row(FILE_SYMBOL, "loans", "truncate"),
            row(FILE_SYMBOL, "books", "copy"),
            row(FILE_SYMBOL, "archived", "from"),
            row(FILE_SYMBOL, "Mixed", "join"),
            row(FILE_SYMBOL, "close_all", "call"),
            row(FILE_SYMBOL, "open_counts", "refresh"),
        ]
    );
}

/// Function bodies are not read: a reference inside a dollar-quoted body
/// is a stated gap, and so is a `CREATE` inside one.
#[test]
fn test_function_bodies_are_never_read_for_references_or_definitions() {
    let source = "CREATE FUNCTION f() RETURNS void AS $$\nBEGIN\n  INSERT INTO audit VALUES (1);\n  CREATE TABLE made_inside (x int);\nEND $$ LANGUAGE plpgsql;\n";
    assert_eq!(qualified_names(source), ["f"]);
    assert!(references(source).is_empty(), "{:?}", references(source));
}

/// Addition A: a top-level `DO` block's body is scanned once for
/// definitions (the guarded idiom); control flow is not interpreted,
/// nested dollar bodies are opaque, and `EXECUTE` strings are not read.
#[test]
fn test_do_block_definitions_are_symbols_with_their_inner_lines() {
    let source = "DO $$\nBEGIN\n  CREATE TYPE public.loan_state AS ENUM ('open', 'closed');\nEXCEPTION\n  WHEN duplicate_object THEN NULL;\nEND\n$$;\nDO $do$\nBEGIN\n  IF NOT EXISTS (SELECT 1 FROM pg_policies WHERE policyname = 'p') THEN\n    CREATE POLICY p ON books\n      USING (true);\n  END IF;\n  CREATE FUNCTION made() RETURNS int AS $fn$\n    CREATE TABLE never_listed (x int);\n  $fn$ LANGUAGE sql;\n  EXECUTE 'CREATE TABLE never_either (x int)';\n  ALTER TABLE books ADD COLUMN y int;\nEND\n$do$;\nCREATE TABLE after (x int);\n";
    let found = symbols(source)
        .into_iter()
        .map(|symbol| {
            (
                symbol.kind,
                symbol.qualified_name,
                symbol.start_line,
                symbol.end_line,
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        found,
        [
            (SymbolKind::Enum, "public.loan_state".to_owned(), 3, 3),
            (SymbolKind::Method, "books::p".to_owned(), 11, 12),
            (SymbolKind::Function, "made".to_owned(), 14, 16),
            (SymbolKind::Struct, "after".to_owned(), 21, 21),
        ]
    );
    let inner = references(source)
        .into_iter()
        .filter(|(_, callee, ..)| callee == "books")
        .map(|(caller, _, kind, line)| (caller, kind, line))
        .collect::<Vec<_>>();
    assert_eq!(
        inner,
        [
            ("books::p".to_owned(), "on".to_owned(), 11),
            (FILE_SYMBOL.to_owned(), "alter".to_owned(), 18),
        ]
    );
}

/// A `DO` block inside a `DO` block's body is not scanned again, and a
/// `DO` body that never closes still extracts what precedes it.
#[test]
fn test_do_blocks_are_scanned_one_level_deep() {
    let source = "DO $a$ BEGIN DO $b$ BEGIN CREATE TABLE deep (x int); END $b$; CREATE TABLE shallow (x int); END $a$;\nDO $$ BEGIN CREATE TABLE open_ended (x int); ";
    assert_eq!(qualified_names(source), ["shallow", "open_ended"]);
}

/// References are capped per file at the call-site bound, and the cut is
/// reported.
#[test]
fn test_references_are_capped_per_file() {
    let source = "ALTER TABLE books ADD x int;\n".repeat(super::MAX_SQL_REFERENCES + 5);
    let extraction = extract(&source);
    assert_eq!(extraction.calls.len(), super::MAX_SQL_REFERENCES);
    assert!(extraction.calls_truncated);
}

/// Names longer than a call site may store are not references.
#[test]
fn test_overlong_reference_names_are_skipped() {
    let source = format!(
        "ALTER TABLE {} ADD x int;\nALTER TABLE ok ADD y int;\n",
        "n".repeat(300)
    );
    assert_eq!(reference_set(&source), [row(FILE_SYMBOL, "ok", "alter")]);
}

/// Worst-case input at SQL's per-file ceiling (E17-42): millions of
/// one-character statements, one ceiling-long line of definitions, and
/// ceiling-deep nested block comments are scanned and extracted in steps
/// linear in their size, with the symbol and reference caps holding.
/// Counted, not timed.
#[test]
fn test_input_at_the_sql_ceiling_is_bounded() {
    let ceiling = usize::try_from(crate::parsers::languages::SQL_MAX_FILE_BYTES).expect("fits");
    let cases = [
        "a;".repeat(ceiling / 2),
        "CREATE TABLE a (x int);".repeat(ceiling / 23),
        "/* ".repeat(ceiling / 3),
        "ALTER TABLE a ADD x int;\n".repeat(ceiling / 25),
    ];
    for source in cases {
        assert!(source.len() <= ceiling);
        SCAN_STEPS.with(|steps| steps.set(0));
        let extraction = extract(&source);
        let steps = SCAN_STEPS.with(std::cell::Cell::get);
        assert!(steps <= 8 * source.len() + 16, "{steps} steps");
        assert!(extraction.symbols.len() <= MAX_SQL_SYMBOLS);
        assert!(extraction.calls.len() <= super::MAX_SQL_REFERENCES);
    }
}

/// Review S4: a UTF-8 byte order mark does not hide the first definition.
#[test]
fn test_a_byte_order_mark_does_not_hide_the_first_definition() {
    let source = "\u{feff}CREATE TABLE first_table (x int);\nCREATE TABLE second_table (x int);\n";
    assert_eq!(qualified_names(source), ["first_table", "second_table"]);
}

/// Review S5: every cut of a file's references marks the file.
#[test]
fn test_every_reference_cut_marks_the_file() {
    let names = (0..200)
        .map(|index| format!("t{index}"))
        .collect::<Vec<_>>()
        .join(", ");
    let per_statement = format!("DROP TABLE {names};\n");
    assert!(extract(&per_statement).calls_truncated, "per-statement cap");
    let joins = (0..100)
        .map(|index| format!(" JOIN j{index} ON true"))
        .collect::<String>();
    let stream = format!("SELECT * FROM a{joins};\n");
    assert!(extract(&stream).calls_truncated, "stream cap");
    let many = (0..300)
        .map(|index| format!("t{index}"))
        .collect::<Vec<_>>()
        .join(" , ");
    let head = format!("TRUNCATE {many};\n");
    assert!(extract(&head).calls_truncated, "head cap");
    assert!(!extract("DROP TABLE a, b;\n").calls_truncated);
}

/// Review S7: an index without a name still references its table.
#[test]
fn test_an_unnamed_index_references_its_table() {
    assert_eq!(
        reference_set("CREATE INDEX ON users (email);\n"),
        [row(FILE_SYMBOL, "users", "on")]
    );
}

/// Review nit: a trigger's column named `begin` opens no block; a trigger
/// body opens one only after `FOR EACH ROW` or `FOR EACH STATEMENT`.
#[test]
fn test_a_trigger_column_named_begin_opens_no_block() {
    let source = "CREATE TRIGGER x AFTER UPDATE OF begin ON t FOR EACH ROW EXECUTE FUNCTION f();\nCREATE TABLE after_trigger (x int);\n";
    assert_eq!(qualified_names(source), ["t::x", "after_trigger"]);
}

/// Review nit: `\copy` reads data lines only for `FROM STDIN` as words.
#[test]
fn test_copy_meta_command_reads_data_only_from_stdin() {
    let source = "\\copy (SELECT * FROM stdin_log) TO 'f'\nCREATE TABLE after_copy (x int);\n";
    assert_eq!(qualified_names(source), ["after_copy"]);
    let source = "\\copy t FROM STDIN\n1\n\\.\nCREATE TABLE after_data (x int);\n";
    assert_eq!(qualified_names(source), ["after_data"]);
}

/// Review nit: `\\` ends a meta-command; the rest of the line is SQL.
#[test]
fn test_a_meta_command_separator_ends_the_meta_command() {
    let source = "\\set x 1 \\\\ CREATE TABLE a (x int);\n";
    assert_eq!(qualified_names(source), ["a"]);
}

/// Review nit: the `REFERENCES` privilege in a grant names nothing.
#[test]
fn test_the_references_privilege_is_not_a_reference() {
    assert_eq!(
        reference_set("GRANT REFERENCES ON t TO r;\n"),
        [row(FILE_SYMBOL, "t", "grant")]
    );
}

/// Review nit: a qualified name keeps the quotes of a part that needs them,
/// and a `U&"…"` identifier stays one name.
#[test]
fn test_quoted_name_parts_keep_their_quotes_when_needed() {
    let source = "CREATE TABLE \"my.schema\".\"my table\" (x int);\nCREATE TABLE U&\"d\\0061ta\" (x int);\nCREATE TABLE \"Mixed\" (x int);\n";
    assert_eq!(
        qualified_names(source),
        ["\"my.schema\".\"my table\"", "U&\"d\\0061ta\"", "\"Mixed\""]
    );
    assert_eq!(symbols(source)[0].name, "my table");
}

/// Review nit: a table named like the file symbol is an ordinary symbol.
#[test]
fn test_a_table_named_script_is_not_the_file_symbol() {
    let extraction = extract("CREATE TABLE script (x int);\nCREATE SCHEMA script;\n");
    assert_eq!(extraction.symbols.len(), 3);
    assert!(is_file_symbol(&extraction.symbols[0]));
    for symbol in &extraction.symbols[1..] {
        assert!(!is_file_symbol(symbol), "{symbol:?}");
        assert_eq!(symbol.qualified_name, "script");
    }
}

/// Review S1: statements after a down-migration marker are labelled, and
/// their drops are not read as drops of the up section's objects.
#[test]
fn test_down_migration_statements_are_labelled() {
    for (up, down) in [
        ("-- +goose Up", "-- +goose Down"),
        ("-- migrate:up", "-- migrate:down"),
    ] {
        let source = format!(
            "{up}\nCREATE TABLE books (id int);\n{down}\nDROP TABLE books;\nCREATE OR REPLACE FUNCTION f() RETURNS int AS $$ SELECT 1 $$ LANGUAGE sql;\n"
        );
        let found = symbols(&source);
        assert_eq!(
            found[0].signature.as_deref(),
            Some("CREATE TABLE books"),
            "{up}"
        );
        assert!(
            found[1]
                .signature
                .as_deref()
                .is_some_and(|signature| signature.starts_with("(down migration) CREATE")),
            "{found:?}"
        );
        let kinds = references(&source)
            .into_iter()
            .map(|(_, _, kind, _)| kind)
            .collect::<Vec<_>>();
        assert!(
            kinds.iter().all(|kind| !kind.starts_with("drop ")),
            "{kinds:?}"
        );
    }
}

/// A drop records the name as written and, for a member, its table.
#[test]
fn test_drops_record_the_written_name_and_table() {
    let kinds = references(
        "DROP TABLE staging.users;\nDROP FUNCTION f(int, text);\nDROP POLICY p ON public.books;\n",
    )
    .into_iter()
    .map(|(_, callee, kind, _)| (callee, kind))
    .collect::<Vec<_>>();
    assert_eq!(
        kinds,
        [
            (
                "users".to_owned(),
                "drop relation\tstaging.users\t".to_owned()
            ),
            ("f".to_owned(), "drop routine\tf\t".to_owned()),
            ("p".to_owned(), "drop policy\tp\tpublic.books".to_owned()),
        ]
    );
}

/// Review S6: a file cut at its symbol bounds says so on its file symbol.
#[test]
fn test_a_cut_symbol_list_is_reported() {
    let source = "CREATE TABLE a (x int);\n".repeat(MAX_SQL_SYMBOLS + 10);
    let extraction = extract(&source);
    assert!(extraction.symbols_truncated);
    assert!(
        extraction.symbols[0]
            .signature
            .as_deref()
            .is_some_and(|signature| signature.contains("cut")),
        "{:?}",
        extraction.symbols[0]
    );
    assert!(!extract("CREATE TABLE a (x int);\n").symbols_truncated);
}

/// Closing pass 2: a `DO` body inside a down section is down too.
#[test]
fn test_a_do_body_in_a_down_section_is_down() {
    let source = "-- +goose Up\nCREATE TABLE t (id int);\n-- +goose Down\nDO $$ BEGIN DROP TABLE t; END $$;\n";
    let kinds = references(source)
        .into_iter()
        .map(|(_, _, kind, _)| kind)
        .collect::<Vec<_>>();
    assert!(!kinds.is_empty());
    assert!(
        kinds.iter().all(|kind| kind.starts_with("down ")),
        "{kinds:?}"
    );
}

/// Closing pass 3: `\\` inside quotes does not split a meta-command.
#[test]
fn test_a_quoted_separator_does_not_split_a_meta_command() {
    let source = "\\set p 'a\\\\b'\nCREATE TABLE t (id int);\n";
    assert_eq!(qualified_names(source), ["t"]);
}

/// Closing pass 4: a file named `'stdin'` is not standard input.
#[test]
fn test_copy_from_a_file_named_stdin_reads_no_data() {
    let source = "\\copy t FROM 'stdin'\nCREATE TABLE after_copy (x int);\n";
    assert_eq!(qualified_names(source), ["after_copy"]);
}

/// Closing pass 1: a drop name too long for a call site is stored cut and
/// marked, not discarded.
#[test]
fn test_a_long_drop_name_is_stored_cut() {
    let schema = "s".repeat(300);
    let source = format!("DROP TABLE {schema}.users;\n");
    let kinds = references(&source)
        .into_iter()
        .map(|(_, callee, kind, _)| (callee, kind))
        .collect::<Vec<_>>();
    assert_eq!(kinds.len(), 1, "{kinds:?}");
    assert_eq!(kinds[0].0, "users");
    assert!(
        kinds[0].1.starts_with("drop relation\t")
            && kinds[0].1.contains('…')
            && kinds[0].1.len() <= 256,
        "{kinds:?}"
    );
}
