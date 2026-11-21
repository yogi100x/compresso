//! SQL across the index (E17-42, D107): migration files are indexed as
//! language `sql` with one symbol per definition, search finds a definition
//! by its bare or qualified name, unfold returns the whole statement, the
//! index stays bounded on hostile files, and an older store re-indexes.

use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

use compresso_atlas::{
    GrepOptions, ImpactOptions, Indexer, SearchOptions, TraceOptions, TraceResult, UnfoldResult,
    grep, impact_with_options, other_sql_definitions, outline, search, search_with_diagnostics,
    sql_definitions, trace, unfold,
};
use compresso_core::{TokenCounter, TokenProvider, counter_for};
use compresso_store::{FileLanguage, RepoId, Store, SymbolKind, SymbolSelector};

const MIGRATION: &str = include_str!("fixtures/lang/sample.sql");

fn write_all(root: &Path, files: &[(&str, &str)]) {
    for (path, source) in files {
        let absolute = root.join(path);
        fs::create_dir_all(absolute.parent().expect("parent")).expect("directory");
        fs::write(absolute, source).expect("file");
    }
}

fn repo_of(store: &Store, root: &Path) -> RepoId {
    let canonical = fs::canonicalize(root).expect("root canonicalizes");
    store
        .repo_by_root(&canonical)
        .expect("lookup")
        .expect("repository")
        .id
}

#[test]
fn test_sql_files_are_indexed_with_language_and_definition_symbols() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let root = temp.path().join("repo");
    write_all(
        &root,
        &[
            ("db/migrations/20260101000000_init.sql", MIGRATION),
            (
                "db/migrations/20260102000000_more.sql",
                "ALTER TABLE public.books ADD COLUMN pages int;\nCREATE INDEX books_pages ON public.books (pages);\n",
            ),
        ],
    );
    let store = Store::open(&temp.path().join("compresso.db")).expect("store opens");
    Indexer::new(&store, counter_for(TokenProvider::Heuristic))
        .index(&root, |_| {})
        .expect("index");
    let repo = repo_of(&store, &root);
    let file = store
        .indexed_file(repo, "db/migrations/20260101000000_init.sql")
        .expect("lookup")
        .expect("file");
    assert_eq!(file.language, Some(FileLanguage::Sql));

    let first = outline(&store, repo, "db/migrations/20260101000000_init.sql").expect("outline");
    // The file's `script` symbol, then one per definition.
    assert_eq!(first.symbols.len(), 18);
    assert_eq!(first.symbols[0].qualified_name, "(sql file)");
    let second = outline(&store, repo, "db/migrations/20260102000000_more.sql").expect("outline");
    assert_eq!(
        second
            .symbols
            .iter()
            .map(|symbol| (symbol.kind, symbol.qualified_name.as_str()))
            .collect::<Vec<_>>(),
        [
            (SymbolKind::Module, "(sql file)"),
            (SymbolKind::Const, "public.books::books_pages")
        ]
    );

    for query in ["books", "public.books"] {
        let hits = search(&store, repo, query, 20).expect("search");
        assert!(
            hits.iter()
                .any(|hit| hit.symbol.qualified_name == "public.books"),
            "{query}: {hits:?}"
        );
    }

    let selector = SymbolSelector {
        path: "db/migrations/20260101000000_init.sql".to_owned(),
        qualified_name: "lending.touch_loan".to_owned(),
        kind: None,
        signature_hash: None,
        start_line_hint: None,
    };
    let UnfoldResult::Found { content, .. } = unfold(&store, repo, &selector).expect("unfold")
    else {
        panic!("one definition");
    };
    assert!(content.starts_with("CREATE OR REPLACE FUNCTION lending.touch_loan()"));
    assert!(content.trim_end().ends_with("$fn$;"), "{content}");
}

/// Counts the bytes the indexer hands the token counter.
struct CountingCounter(AtomicUsize);

impl TokenCounter for CountingCounter {
    fn provider(&self) -> TokenProvider {
        TokenProvider::Heuristic
    }

    fn count(&self, text: &str) -> usize {
        self.0.fetch_add(text.len(), Ordering::Relaxed);
        text.len() / 4
    }
}

/// Thousands of definitions on one long line are indexed with a bounded
/// token count: the covered-bytes cap keeps the per-symbol count from
/// reading the line once per statement. Counted, not timed.
#[test]
fn test_definitions_on_one_long_line_are_counted_in_bounded_bytes() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let root = temp.path().join("repo");
    let line = "CREATE TABLE a (x int);".repeat(40_000);
    write_all(&root, &[("dump.sql", &line)]);
    let store = Store::open(&temp.path().join("compresso.db")).expect("store opens");
    let counter = CountingCounter(AtomicUsize::new(0));
    Indexer::new(&store, &counter)
        .index(&root, |_| {})
        .expect("index");
    let repo = repo_of(&store, &root);
    let symbols = store.symbols_in_file(repo, "dump.sql").expect("symbols");
    assert!(!symbols.is_empty());
    let counted = counter.0.load(Ordering::Relaxed);
    assert!(counted <= 20 * 1024 * 1024, "{counted} bytes counted");
}

#[test]
fn test_a_store_indexed_before_sql_support_reindexes_sql_files() {
    const OLD_VERSION: &str =
        "0.1.0+atlas-shell-text-v1+rust-imports-v2+java-csharp-v1+go-v1+calls-v1";
    let temp = tempfile::tempdir().expect("temporary directory");
    let root = temp.path().join("repo");
    write_all(
        &root,
        &[("db/0001.sql", "CREATE TABLE public.users (id int);\n")],
    );
    let store = Store::open(&temp.path().join("compresso.db")).expect("store opens");
    let counter = counter_for(TokenProvider::Heuristic);
    Indexer::new(&store, counter)
        .with_parser_version(OLD_VERSION)
        .index(&root, |_| {})
        .expect("old index");
    let repo = repo_of(&store, &root);
    // What a parser without SQL left behind: no language, no symbols.
    let mut record = store
        .indexed_file(repo, "db/0001.sql")
        .expect("lookup")
        .expect("file");
    record.language = None;
    store
        .replace_indexed_file(repo, &record, &[], &[])
        .expect("old rows");
    Indexer::new(&store, counter)
        .index(&root, |_| {})
        .expect("new index");
    let version = store
        .latest_index_version(repo)
        .expect("version")
        .expect("stamped");
    assert!(
        version
            .parser_version
            .contains("+sql-v1+java-csharp-v1+go-v1+calls-v1"),
        "{version:?}"
    );
    let file = store
        .indexed_file(repo, "db/0001.sql")
        .expect("lookup")
        .expect("file");
    assert_eq!(file.language, Some(FileLanguage::Sql));
    let symbols = store.symbols_in_file(repo, "db/0001.sql").expect("symbols");
    assert!(
        symbols
            .iter()
            .any(|symbol| symbol.qualified_name == "public.users"),
        "{symbols:?}"
    );
}

fn index_repo(files: &[(&str, &str)]) -> (tempfile::TempDir, Store, RepoId) {
    let temp = tempfile::tempdir().expect("temporary directory");
    let root = temp.path().join("repo");
    write_all(&root, files);
    let store = Store::open(&temp.path().join("compresso.db")).expect("store opens");
    Indexer::new(&store, counter_for(TokenProvider::Heuristic))
        .index(&root, |_| {})
        .expect("index");
    let repo = repo_of(&store, &root);
    (temp, store, repo)
}

const MIGRATIONS: [(&str, &str); 4] = [
    (
        "db/migrations/0001_init.sql",
        "CREATE TABLE public.books (id int PRIMARY KEY);\nCREATE POLICY books_read ON public.books USING (true);\nCREATE INDEX books_id ON books (id);\nCREATE FUNCTION touch_row() RETURNS trigger AS $$ BEGIN RETURN NEW; END $$ LANGUAGE plpgsql;\nCREATE TRIGGER books_touch BEFORE UPDATE ON books FOR EACH ROW EXECUTE FUNCTION touch_row();\n",
    ),
    (
        "db/migrations/0002_more.sql",
        "ALTER TABLE books ADD COLUMN title text;\nCREATE OR REPLACE FUNCTION public.touch_row() RETURNS trigger AS $$ BEGIN RETURN NEW; END $$ LANGUAGE plpgsql;\nCREATE POLICY books_read ON books USING (false);\nINSERT INTO books (id) VALUES (1);\n",
    ),
    (
        "db/migrations/0003_drop.sql",
        "DROP FUNCTION IF EXISTS touch_row();\nDROP POLICY books_read ON books;\n",
    ),
    (
        "db/migrations/0004_again.sql",
        "CREATE FUNCTION touch_row() RETURNS trigger AS $$ BEGIN RETURN NEW; END $$ LANGUAGE plpgsql;\n",
    ),
];

/// `trace --to <table>` lists the statements that touch it: definitions
/// named by the table and the file symbol of statements that define nothing.
#[test]
fn test_trace_to_a_table_lists_the_statements_that_touch_it() {
    let (_temp, store, repo) = index_repo(&MIGRATIONS);
    let report = trace(&store, repo, None, Some("books"), TraceOptions::default()).expect("trace");
    let TraceResult::Callers(callers) = report.result else {
        panic!("callers: {:?}", report.result);
    };
    let mut sites = callers
        .iter()
        .map(|caller| {
            (
                caller.symbol.qualified_name.clone(),
                caller.call_site.path.clone(),
                caller.call_site.line,
            )
        })
        .collect::<Vec<_>>();
    sites.sort();
    let expected = [
        ("books::books_touch", "db/migrations/0001_init.sql", 5),
        ("books::books_id", "db/migrations/0001_init.sql", 3),
        ("books::books_read", "db/migrations/0002_more.sql", 3),
        ("public.books::books_read", "db/migrations/0001_init.sql", 2),
        // One call site is kept per caller (D90): the file's first
        // statement naming the table.
        ("(sql file)", "db/migrations/0002_more.sql", 1),
    ];
    for (name, path, line) in expected {
        assert!(
            sites.contains(&(name.to_owned(), path.to_owned(), line)),
            "{name} {path}:{line} in {sites:?}"
        );
    }
}

/// Search lists a SQL name's definitions in path order, last first, with
/// the later drops that name them, and says when spellings differ rather
/// than asserting one object.
#[test]
fn test_search_lists_redefinitions_in_path_order_with_drops_and_spellings() {
    let (_temp, store, repo) = index_repo(&MIGRATIONS);
    let report = search_with_diagnostics(
        &store,
        repo,
        "touch_row",
        SearchOptions {
            limit: 20,
            path: None,
        },
    )
    .expect("search");
    let functions = report
        .hits
        .iter()
        .filter(|hit| hit.symbol.name == "touch_row")
        .map(|hit| hit.path.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        functions,
        [
            "db/migrations/0004_again.sql",
            "db/migrations/0002_more.sql",
            "db/migrations/0001_init.sql",
        ]
    );
    let diagnostic = report
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code.as_str() == "sql_definitions")
        .expect("redefinition diagnostic");
    assert_eq!(
        diagnostic.message,
        "SQL routine `touch_row`: 3 definitions in path order, last first: db/migrations/0004_again.sql:1 touch_row; db/migrations/0002_more.sql:2 public.touch_row (a later DROP statement names touch_row at db/migrations/0003_drop.sql:1); db/migrations/0001_init.sql:4 touch_row (a later DROP statement names touch_row at db/migrations/0003_drop.sql:1). Written as public.touch_row, touch_row; not shown to be one object."
    );

    let policy = store
        .symbols_in_file(repo, "db/migrations/0002_more.sql")
        .expect("symbols")
        .into_iter()
        .find(|symbol| symbol.name == "books_read")
        .expect("policy");
    let group = sql_definitions(&store, repo, &policy)
        .expect("group")
        .expect("a SQL definition");
    assert_eq!(
        (group.family, group.table.as_deref()),
        ("policy", Some("books"))
    );
    assert_eq!(
        group
            .definitions
            .iter()
            .map(|definition| (
                definition.symbol.qualified_name.as_str(),
                !definition.later_drops.is_empty()
            ))
            .collect::<Vec<_>>(),
        [
            ("books::books_read", true),
            ("public.books::books_read", true)
        ]
    );
    assert_eq!(
        group.spellings,
        ["books::books_read", "public.books::books_read"]
    );
}

/// A non-SQL search is unchanged: no reordering, no SQL diagnostic.
#[test]
fn test_search_without_sql_hits_is_unchanged() {
    let (_temp, store, repo) = index_repo(&[("src/lib.rs", "pub fn touch_row() {}\n")]);
    let report = search_with_diagnostics(
        &store,
        repo,
        "touch_row",
        SearchOptions {
            limit: 20,
            path: None,
        },
    )
    .expect("search");
    assert_eq!(report.hits.len(), 1);
    assert!(report.diagnostics.is_empty());
}

/// Unfold of one definition names the others in path order, last first.
#[test]
fn test_unfold_of_a_sql_definition_names_its_other_definitions() {
    let (_temp, store, repo) = index_repo(&MIGRATIONS);
    let symbol = store
        .symbols_in_file(repo, "db/migrations/0001_init.sql")
        .expect("symbols")
        .into_iter()
        .find(|symbol| symbol.name == "touch_row")
        .expect("function");
    let (others, total) =
        other_sql_definitions(&store, repo, "db/migrations/0001_init.sql", &symbol)
            .expect("others");
    assert_eq!(total, 2);
    assert_eq!(
        others
            .iter()
            .map(|other| (other.path.as_str(), other.start_line))
            .collect::<Vec<_>>(),
        [
            ("db/migrations/0004_again.sql", 1),
            ("db/migrations/0002_more.sql", 2)
        ]
    );
}

/// SQL has no imports: the import impact of a migration is never complete.
#[test]
fn test_impact_of_a_sql_file_is_never_complete() {
    let (_temp, store, repo) = index_repo(&MIGRATIONS);
    let report = impact_with_options(
        &store,
        repo,
        "db/migrations/0001_init.sql",
        ImpactOptions::default(),
    )
    .expect("impact");
    assert!(report.dependents.is_empty());
    assert!(!report.complete);
}

/// Grep rows in a SQL file carry their enclosing symbol: the definition
/// around a line, or the file's `script` symbol.
#[test]
fn test_grep_rows_in_sql_files_name_their_enclosing_symbol() {
    let (_temp, store, repo) = index_repo(&MIGRATIONS);
    let patterns = ["books".to_owned()];
    let report = grep(
        &store,
        repo,
        &GrepOptions {
            patterns: &patterns,
            regex: false,
            case_insensitive: false,
            path: Some("db/migrations/0002_more.sql"),
            max_files: 5,
            max_matches_per_file: 10,
            context_lines: 0,
            scopes: &[],
            select_rows: false,
        },
    )
    .expect("grep");
    let rows = report.results[0].files[0]
        .lines
        .iter()
        .map(|line| (line.line, line.symbol.clone().unwrap_or_default()))
        .collect::<Vec<_>>();
    assert_eq!(
        rows,
        [
            (1, "(sql file)".to_owned()),
            (3, "books::books_read".to_owned()),
            (4, "(sql file)".to_owned())
        ]
    );
}

/// E17-42 (owner decision 2026-11-21): SQL files up to 4 MiB are indexed;
/// every other language keeps the 1,000,000-byte ceiling, and a SQL file
/// above 4 MiB is skipped. A large SQL file is searchable by symbol,
/// outlined, unfolded, grepped, and read in windows.
#[test]
fn test_sql_files_up_to_their_own_ceiling_are_indexed_and_served() {
    let mut big = String::from("CREATE TABLE big_table (x int);\n");
    let mut index = 0;
    while big.len() <= 1_500_000 {
        big.push_str(&format!(
            "CREATE INDEX big_index_{index} ON big_table (x);\n"
        ));
        index += 1;
    }
    let too_big = "SELECT 1;\n".repeat(450_000);
    let rust = format!("pub fn big() {{}}\n{}", "// padding\n".repeat(140_000));
    let (temp, store, repo) = index_repo(&[
        ("db/big.sql", &big),
        ("db/too_big.sql", &too_big),
        ("src/big.rs", &rust),
    ]);
    assert!(too_big.len() > 4 * 1024 * 1024 && rust.len() > 1_000_000);
    assert_eq!(
        store
            .indexed_file(repo, "db/big.sql")
            .expect("lookup")
            .map(|file| file.language),
        Some(Some(FileLanguage::Sql))
    );
    for skipped in ["db/too_big.sql", "src/big.rs"] {
        assert!(
            store.indexed_file(repo, skipped).expect("lookup").is_none(),
            "{skipped}"
        );
    }
    let hits = search(&store, repo, "big_table", 5).expect("search");
    assert!(hits.iter().any(|hit| hit.path == "db/big.sql"));
    let listed = outline(&store, repo, "db/big.sql").expect("outline");
    // More definitions than the per-file symbol cap: the first 4,096
    // symbols (the file symbol among them) are kept.
    assert!(index > 4_096);
    assert_eq!(listed.symbols.len(), 4_096);
    let selector = SymbolSelector {
        path: "db/big.sql".to_owned(),
        qualified_name: "big_table::big_index_7".to_owned(),
        kind: None,
        signature_hash: None,
        start_line_hint: None,
    };
    let UnfoldResult::Found { content, .. } = unfold(&store, repo, &selector).expect("unfold")
    else {
        panic!("found");
    };
    assert_eq!(content, "CREATE INDEX big_index_7 ON big_table (x);\n");
    let patterns = ["big_index_100".to_owned()];
    let found = grep(
        &store,
        repo,
        &GrepOptions {
            patterns: &patterns,
            regex: false,
            case_insensitive: false,
            path: None,
            max_files: 5,
            max_matches_per_file: 5,
            context_lines: 0,
            scopes: &[],
            select_rows: false,
        },
    )
    .expect("grep");
    assert_eq!(found.results[0].files[0].path, "db/big.sql");
    // The read gate takes the canonical repository root, as the store
    // records it (a temporary directory may sit behind a symlink).
    let root = fs::canonicalize(temp.path().join("repo")).expect("canonical root");
    let window = compresso_atlas::read_bounded(
        &root,
        "db/big.sql",
        Some((1, 3)),
        2_000,
        counter_for(TokenProvider::Heuristic),
    )
    .expect("read");
    assert!(window.content.starts_with("CREATE TABLE big_table"));
}

/// Review: with more definitions than the search limit, the window still
/// shows the last ones in path order, not the first the ranking kept.
#[test]
fn test_search_shows_the_last_definitions_in_path_order_when_the_limit_holds_fewer() {
    let function =
        "CREATE OR REPLACE FUNCTION touch_row() RETURNS int AS $$ SELECT 1 $$ LANGUAGE sql;\n";
    let (_temp, store, repo) = index_repo(&[
        ("db/0001.sql", function),
        ("db/0002.sql", function),
        ("db/0003.sql", function),
        ("db/0004.sql", function),
        ("db/0005.sql", function),
    ]);
    let report = search_with_diagnostics(
        &store,
        repo,
        "touch_row",
        SearchOptions {
            limit: 2,
            path: None,
        },
    )
    .expect("search");
    let paths = report
        .hits
        .iter()
        .map(|hit| hit.path.as_str())
        .collect::<Vec<_>>();
    assert_eq!(paths, ["db/0005.sql", "db/0004.sql"]);
}

/// Review: `DROP INDEX x` names no table, and index names are unique per
/// schema, so it labels the index defined on a table.
#[test]
fn test_a_later_drop_index_labels_the_index() {
    let (_temp, store, repo) = index_repo(&[
        ("db/0001.sql", "CREATE INDEX books_id ON books (id);\n"),
        ("db/0002.sql", "DROP INDEX IF EXISTS books_id;\n"),
    ]);
    let index = store
        .symbols_in_file(repo, "db/0001.sql")
        .expect("symbols")
        .into_iter()
        .find(|symbol| symbol.name == "books_id")
        .expect("index");
    let group = sql_definitions(&store, repo, &index)
        .expect("group")
        .expect("a SQL definition");
    let drop = group.definitions[0]
        .later_drops
        .first()
        .cloned()
        .expect("listed");
    assert_eq!((drop.path.as_str(), drop.line), ("db/0002.sql", 1));
}

/// Review: `trace --to public.books` finds the table search shows by that
/// name.
#[test]
fn test_trace_finds_a_schema_qualified_sql_name() {
    let (_temp, store, repo) = index_repo(&MIGRATIONS);
    let report = trace(
        &store,
        repo,
        None,
        Some("public.books"),
        TraceOptions::default(),
    )
    .expect("trace");
    assert!(
        matches!(report.result, TraceResult::Callers(ref callers) if !callers.is_empty()),
        "{:?}",
        report.result
    );
}

/// Review: the repository map lists a SQL file's definitions, not its
/// whole-file `script` symbol.
#[test]
fn test_the_map_lists_sql_definitions_not_the_file_symbol() {
    let (_temp, store, repo) = index_repo(&MIGRATIONS);
    let map = compresso_atlas::repo_map(&store, repo, 4_000).expect("map");
    assert!(map.rendered.contains("books"), "{}", map.rendered);
    assert!(!map.rendered.contains("(sql file)"), "{}", map.rendered);
}

fn sql_message(
    store: &Store,
    repo: RepoId,
    query: &str,
    path: Option<&str>,
) -> (Vec<String>, String) {
    let report = search_with_diagnostics(store, repo, query, SearchOptions { limit: 20, path })
        .expect("search");
    let paths = report
        .hits
        .iter()
        .filter(|hit| hit.symbol.name == query)
        .map(|hit| hit.path.clone())
        .collect();
    let message = report
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code.as_str() == "sql_definitions")
        .map(|diagnostic| diagnostic.message.clone())
        .unwrap_or_default();
    (paths, message)
}

/// Review M1: a path scope keeps definitions outside it out of the hits and
/// the listing, and the listing counts them.
#[test]
fn test_sql_definitions_respect_the_search_path_scope() {
    let table = "CREATE TABLE users (id int);\n";
    let (_temp, store, repo) = index_repo(&[("db/a/001.sql", table), ("db/b/002.sql", table)]);
    let (paths, message) = sql_message(&store, repo, "users", Some("db/a"));
    assert_eq!(paths, ["db/a/001.sql"]);
    assert!(!message.contains("db/b"), "{message}");
    assert!(
        message.contains("1 more outside the path scope"),
        "{message}"
    );
}

/// Review S2: order compares digit runs as numbers, is worded as path
/// order, and says when definitions sit in several directories.
#[test]
fn test_sql_definitions_are_in_numeric_path_order_and_name_their_directories() {
    let function =
        "CREATE OR REPLACE FUNCTION touch_row() RETURNS int AS $$ SELECT 1 $$ LANGUAGE sql;\n";
    let (_temp, store, repo) = index_repo(&[
        ("db/V2__a.sql", function),
        ("db/V10__b.sql", function),
        ("schema/dump.sql", function),
    ]);
    let (paths, message) = sql_message(&store, repo, "touch_row", None);
    assert_eq!(paths, ["schema/dump.sql", "db/V10__b.sql", "db/V2__a.sql"]);
    assert!(message.contains("in path order, last first"), "{message}");
    assert!(message.contains("2 directories"), "{message}");
    assert!(!message.contains("latest"), "{message}");
}

/// Closing pass 1: a later `DROP` of the same family and name is stated as
/// read (the name as the drop writes it, where), with no claim that it
/// names the same object, and listed even outside a search path scope.
#[test]
fn test_later_drops_are_stated_as_read() {
    let (_temp, store, repo) = index_repo(&[
        (
            "db/a/0001.sql",
            "CREATE TABLE public.users (id int);\nCREATE FUNCTION f(a int) RETURNS int AS $$ SELECT 1 $$ LANGUAGE sql;\n",
        ),
        (
            "db/b/0002.sql",
            "DROP TABLE staging.users;\nDROP FUNCTION f();\nDROP TABLE users;\nDROP TABLE \"Users\";\nDROP TABLE x.users;\nDROP TABLE y.users;\n",
        ),
    ]);
    let drops = |name: &str| {
        let symbol = store
            .symbols_in_file(repo, "db/a/0001.sql")
            .expect("symbols")
            .into_iter()
            .find(|symbol| symbol.name == name)
            .expect("definition");
        let definition = sql_definitions(&store, repo, &symbol)
            .expect("group")
            .expect("grouped")
            .definitions
            .remove(0);
        (
            definition
                .later_drops
                .iter()
                .map(|drop| (drop.written.clone(), drop.line))
                .collect::<Vec<_>>(),
            definition.later_drops_total,
        )
    };
    assert_eq!(
        drops("users"),
        (
            vec![
                ("staging.users".to_owned(), 1),
                ("users".to_owned(), 3),
                ("x.users".to_owned(), 5)
            ],
            4
        )
    );
    assert_eq!(drops("f"), (vec![("f".to_owned(), 2)], 1));
    let (_, message) = sql_message(&store, repo, "users", Some("db/a"));
    assert!(
        message.contains("a later DROP statement names staging.users at db/b/0002.sql:1"),
        "{message}"
    );
    assert!(message.contains("1 more"), "{message}");
    assert!(!message.contains("dropped later"), "{message}");
}

/// Review S1: a down migration's definitions and drops stay out of the up
/// definitions' grouping and labels.
#[test]
fn test_down_migrations_stay_out_of_groups_and_drop_labels() {
    let (_temp, store, repo) = index_repo(&[(
        "db/0001.sql",
        "-- +goose Up\nCREATE TABLE books (id int);\n-- +goose Down\nDROP TABLE books;\nCREATE TABLE books (id int, legacy int);\n",
    )]);
    let up = store
        .symbols_in_file(repo, "db/0001.sql")
        .expect("symbols")
        .into_iter()
        .find(|symbol| symbol.name == "books")
        .expect("up definition");
    let group = sql_definitions(&store, repo, &up)
        .expect("group")
        .expect("grouped");
    assert_eq!(group.definitions.len(), 1);
    assert!(group.definitions[0].later_drops.is_empty());
}
