//! SQL redefinitions in path order, last first (E17-42, D107).
//!
//! Migrations define the same object again and again (`CREATE OR REPLACE
//! FUNCTION` in many files). A group of definitions is every SQL symbol with
//! the same family (relation, routine, type, policy, ...; read from the
//! signature's `CREATE` words), the same bare name, and, for a trigger,
//! index, policy, or rule, the same table bare name. No search path is
//! inferred: `public.users` and `users` fall in one group, each is shown as
//! written, and a group whose spellings differ says so instead of claiming
//! one object.
//!
//! Order is repository path order, then line, with digit runs inside a
//! path compared as numbers (`V10__` after `V2__`). It is path order, not
//! history: a group whose definitions sit in different directories says
//! so, since order across directories means nothing. Definitions after a
//! down-migration marker carry a `(down migration)` signature label and are
//! in no group.
//!
//! A `DROP` statement later in path order that names the same family and
//! bare name (and table bare name, for a trigger, policy, or rule) is
//! listed with the definition as read: "a later DROP statement names
//! <name as the drop writes it> at path:line". No claim is made that it
//! names the same object: schemas and argument lists are not compared.
//! Drops are listed whether or not they sit inside a search path scope.

use std::cmp::Ordering;

use compresso_store::{FileLanguage, RepoId, Store, SymbolRecord};

use crate::parsers::sql::{family_of_signature, is_file_symbol};
use crate::queries::{SearchHit, SymbolIdentity, map_path_matches_focus, symbol_identity};
use crate::scanner::AtlasError;
use crate::search::{SearchDiagnostic, SearchDiagnosticCode};

/// SQL groups annotated per search.
const MAX_SEARCH_GROUPS: usize = 8;
/// Definitions named in one diagnostic.
const MAX_LISTED_DEFINITIONS: usize = 10;
/// Call-site rows read per name when looking for drops.
const MAX_DROPS: usize = 10_000;
/// Later drops listed per definition.
pub const MAX_LISTED_DROPS: usize = 3;
/// Other definitions an unfold lists.
pub const MAX_OTHER_DEFINITIONS: usize = 20;

/// A later `DROP` statement naming a definition's family and bare name.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LaterDrop {
    pub path: String,
    pub line: u32,
    /// The name as the drop writes it (cut and marked `…` when too long to
    /// store).
    pub written: String,
}

/// One definition of a group.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SqlDefinition {
    pub path: String,
    pub symbol: SymbolRecord,
    /// The first [`MAX_LISTED_DROPS`] later `DROP` statements naming it,
    /// in path order.
    pub later_drops: Vec<LaterDrop>,
    /// Every later `DROP` statement naming it.
    pub later_drops_total: usize,
}

/// Every definition of one SQL object name, last in path order first.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SqlDefinitions {
    pub family: &'static str,
    pub name: String,
    /// The table bare name, for members.
    pub table: Option<String>,
    pub definitions: Vec<SqlDefinition>,
    /// The distinct qualified names, sorted; more than one means the group
    /// mixes spellings or schemas.
    pub spellings: Vec<String>,
    /// Directories the definitions sit in.
    pub directories: usize,
    /// Definitions outside the search path scope, not listed.
    pub outside_scope: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct GroupKey {
    family: &'static str,
    name: String,
    table: Option<String>,
}

/// Compares repository paths with digit runs as numbers.
pub(crate) fn path_order(left: &str, right: &str) -> Ordering {
    let (mut left, mut right) = (left.as_bytes(), right.as_bytes());
    loop {
        match (left.first(), right.first()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(a), Some(b)) if a.is_ascii_digit() && b.is_ascii_digit() => {
                let run = |bytes: &[u8]| {
                    bytes
                        .iter()
                        .take_while(|byte| byte.is_ascii_digit())
                        .count()
                };
                let (left_run, right_run) = (run(left), run(right));
                let trim = |digits: &[u8]| {
                    let zeros = digits.iter().take_while(|byte| **byte == b'0').count();
                    digits[zeros..].to_vec()
                };
                let (a_digits, b_digits) = (trim(&left[..left_run]), trim(&right[..right_run]));
                let order = a_digits
                    .len()
                    .cmp(&b_digits.len())
                    .then_with(|| a_digits.cmp(&b_digits))
                    .then_with(|| left_run.cmp(&right_run));
                if order != Ordering::Equal {
                    return order;
                }
                left = &left[left_run..];
                right = &right[right_run..];
            }
            (Some(a), Some(b)) => {
                if a != b {
                    return a.cmp(b);
                }
                left = &left[1..];
                right = &right[1..];
            }
        }
    }
}

/// `(path, line)` in path order.
fn position_order(left: (&str, u32), right: (&str, u32)) -> Ordering {
    path_order(left.0, right.0).then(left.1.cmp(&right.1))
}

fn table_of(qualified_name: &str) -> Option<String> {
    let (table, _) = qualified_name.rsplit_once("::")?;
    Some(unquote(table.rsplit('.').next().unwrap_or(table)))
}

fn unquote(part: &str) -> String {
    part.strip_prefix('"')
        .and_then(|inner| inner.strip_suffix('"'))
        .map_or_else(|| part.to_owned(), |inner| inner.replace("\"\"", "\""))
}

fn key_of(symbol: &SymbolRecord) -> Option<GroupKey> {
    if is_file_symbol(symbol) {
        return None;
    }
    Some(GroupKey {
        family: family_of_signature(symbol.signature.as_deref()?)?,
        name: symbol.name.clone(),
        table: table_of(&symbol.qualified_name),
    })
}

/// Every SQL definition in the group of `symbol`, last in path order
/// first; `None` when `symbol` is not a grouped SQL definition.
pub fn sql_definitions(
    store: &Store,
    repo: RepoId,
    symbol: &SymbolRecord,
) -> Result<Option<SqlDefinitions>, AtlasError> {
    let Some(key) = key_of(symbol) else {
        return Ok(None);
    };
    group(store, repo, &key, None).map(Some)
}

struct Drop {
    path: String,
    line: u32,
    written: String,
}

fn group(
    store: &Store,
    repo: RepoId,
    key: &GroupKey,
    scope: Option<&str>,
) -> Result<SqlDefinitions, AtlasError> {
    let in_scope = |path: &str| scope.is_none_or(|scope| map_path_matches_focus(path, scope));
    let mut outside_scope = 0;
    let mut definitions = Vec::new();
    for candidate in store.indexed_symbols_named(repo, &key.name)? {
        if candidate.language != Some(FileLanguage::Sql)
            || key_of(&candidate.symbol).as_ref() != Some(key)
        {
            continue;
        }
        if !in_scope(&candidate.path) {
            outside_scope += 1;
            continue;
        }
        definitions.push(SqlDefinition {
            path: candidate.path,
            symbol: candidate.symbol,
            later_drops: Vec::new(),
            later_drops_total: 0,
        });
    }
    definitions.sort_by(|left, right| {
        position_order(
            (&right.path, right.symbol.start_line),
            (&left.path, left.symbol.start_line),
        )
    });
    // Drops are read in every path, scope or not: a drop elsewhere is
    // still a fact about this name.
    let mut drops = store
        .call_sites_named(repo, &key.name, MAX_DROPS)?
        .into_iter()
        .filter_map(|call| {
            let rest = call.qualifier.as_deref()?.strip_prefix("drop ")?;
            let mut fields = rest.split('\t');
            let family = fields.next()?;
            let written = fields.next()?.to_owned();
            let table = fields.next().unwrap_or_default().to_owned();
            let member = key.table.is_some() && key.family != "index";
            let same_table = !member || table_of(&format!("{table}::x")) == key.table;
            (family == key.family && same_table).then_some(Drop {
                path: call.from_path,
                line: call.line,
                written,
            })
        })
        .collect::<Vec<_>>();
    drops.sort_by(|left, right| position_order((&left.path, left.line), (&right.path, right.line)));
    for definition in &mut definitions {
        let later = drops
            .iter()
            .filter(|drop| {
                position_order(
                    (&drop.path, drop.line),
                    (&definition.path, definition.symbol.end_line),
                ) == Ordering::Greater
            })
            .collect::<Vec<_>>();
        definition.later_drops_total = later.len();
        definition.later_drops = later
            .into_iter()
            .take(MAX_LISTED_DROPS)
            .map(|drop| LaterDrop {
                path: drop.path.clone(),
                line: drop.line,
                written: drop.written.clone(),
            })
            .collect();
    }
    let mut spellings = definitions
        .iter()
        .map(|definition| definition.symbol.qualified_name.clone())
        .collect::<Vec<_>>();
    spellings.sort();
    spellings.dedup();
    let mut directories = definitions
        .iter()
        .map(|definition| {
            definition
                .path
                .rsplit_once('/')
                .map_or("", |(directory, _)| directory)
        })
        .collect::<Vec<_>>();
    directories.sort_unstable();
    directories.dedup();
    Ok(SqlDefinitions {
        family: key.family,
        name: key.name.clone(),
        table: key.table.clone(),
        directories: directories.len(),
        definitions,
        spellings,
        outside_scope,
    })
}

/// The other definitions of a SQL symbol's group, last in path order
/// first, for `unfold` (at most [`MAX_OTHER_DEFINITIONS`]), and how many
/// there are; none for any other symbol.
pub fn other_sql_definitions(
    store: &Store,
    repo: RepoId,
    path: &str,
    symbol: &SymbolRecord,
) -> Result<(Vec<SymbolIdentity>, usize), AtlasError> {
    if !path.ends_with(".sql") {
        return Ok((Vec::new(), 0));
    }
    let Some(group) = sql_definitions(store, repo, symbol)? else {
        return Ok((Vec::new(), 0));
    };
    let others = group
        .definitions
        .iter()
        .filter(|definition| {
            !(definition.path == path
                && definition.symbol.start_line == symbol.start_line
                && definition.symbol.qualified_name == symbol.qualified_name)
        })
        .collect::<Vec<_>>();
    let total = others.len();
    Ok((
        others
            .into_iter()
            .take(MAX_OTHER_DEFINITIONS)
            .map(|definition| symbol_identity(&definition.path, &definition.symbol))
            .collect(),
        total,
    ))
}

/// Puts the SQL hits of one group together, last in path order first,
/// where the group's best-ranked hit was (replaced by the group's last
/// definitions in scope when the window holds earlier ones), and adds one
/// diagnostic per group with more than one definition or a later `DROP`
/// (at most [`MAX_SEARCH_GROUPS`] groups). Only definitions inside the
/// path scope are shown; those outside it are counted.
pub(crate) fn annotate_search(
    store: &Store,
    repo: RepoId,
    scope: Option<&str>,
    hits: &mut Vec<SearchHit>,
    diagnostics: &mut Vec<SearchDiagnostic>,
) -> Result<(), AtlasError> {
    let keys = hits
        .iter()
        .map(|hit| {
            (hit.language == Some(FileLanguage::Sql))
                .then(|| key_of(&hit.symbol))
                .flatten()
        })
        .collect::<Vec<_>>();
    let mut groups = Vec::<GroupKey>::new();
    for key in keys.iter().flatten() {
        if !groups.contains(key) && groups.len() < MAX_SEARCH_GROUPS {
            groups.push(key.clone());
        }
    }
    if groups.is_empty() {
        return Ok(());
    }
    let loaded = groups
        .iter()
        .map(|key| group(store, repo, key, scope))
        .collect::<Result<Vec<_>, _>>()?;
    let mut placed = vec![false; hits.len()];
    let mut ordered = Vec::with_capacity(hits.len());
    for index in 0..hits.len() {
        if placed[index] {
            continue;
        }
        placed[index] = true;
        let Some(position) = keys[index]
            .as_ref()
            .and_then(|key| groups.iter().position(|group| group == key))
        else {
            ordered.push(hits[index].clone());
            continue;
        };
        // The group's hits in this window are replaced by as many of its
        // last definitions: a window cut by the limit may hold only the
        // first ones.
        let members = (index..hits.len())
            .filter(|other| keys[*other].as_ref() == Some(&groups[position]))
            .collect::<Vec<_>>();
        for member in &members {
            placed[*member] = true;
        }
        let score = hits[index].score;
        let last = &loaded[position].definitions;
        if last.len() < members.len() {
            ordered.extend(members.iter().map(|member| hits[*member].clone()));
            continue;
        }
        ordered.extend(last.iter().take(members.len()).map(|definition| SearchHit {
            path: definition.path.clone(),
            language: Some(FileLanguage::Sql),
            symbol: definition.symbol.clone(),
            score,
        }));
    }
    *hits = ordered;
    for group in &loaded {
        let dropped = group
            .definitions
            .iter()
            .any(|definition| definition.later_drops_total > 0);
        if group.definitions.len() > 1 || dropped || group.outside_scope > 0 {
            diagnostics.push(SearchDiagnostic {
                code: SearchDiagnosticCode::SqlDefinitions,
                message: message(group),
            });
        }
    }
    Ok(())
}

fn message(group: &SqlDefinitions) -> String {
    let mut text = format!("SQL {} `{}`", group.family, group.name);
    if let Some(table) = &group.table {
        text.push_str(&format!(" on `{table}`"));
    }
    let count = group.definitions.len();
    text.push_str(&format!(
        ": {count} definition{} in path order, last first: ",
        if count == 1 { "" } else { "s" }
    ));
    let listed = group
        .definitions
        .iter()
        .take(MAX_LISTED_DEFINITIONS)
        .map(|definition| {
            let mut row = format!(
                "{}:{} {}",
                definition.path, definition.symbol.start_line, definition.symbol.qualified_name
            );
            if definition.later_drops_total > 0 {
                let mut drops = definition
                    .later_drops
                    .iter()
                    .map(|drop| {
                        format!(
                            "a later DROP statement names {} at {}:{}",
                            drop.written, drop.path, drop.line
                        )
                    })
                    .collect::<Vec<_>>();
                let more = definition.later_drops_total - definition.later_drops.len();
                if more > 0 {
                    drops.push(format!("{more} more"));
                }
                row.push_str(&format!(" ({})", drops.join("; ")));
            }
            row
        })
        .collect::<Vec<_>>();
    text.push_str(&listed.join("; "));
    if count > MAX_LISTED_DEFINITIONS {
        text.push_str(&format!("; {} more", count - MAX_LISTED_DEFINITIONS));
    }
    text.push('.');
    if group.directories > 1 {
        text.push_str(&format!(
            " They sit in {} directories; path order across directories is not history.",
            group.directories
        ));
    }
    if group.spellings.len() > 1 {
        text.push_str(&format!(
            " Written as {}; not shown to be one object.",
            group.spellings.join(", ")
        ));
    }
    if group.outside_scope > 0 {
        text.push_str(&format!(
            " {} more outside the path scope, not listed.",
            group.outside_scope
        ));
    }
    text
}

#[cfg(test)]
mod tests {
    use std::cmp::Ordering;

    use super::path_order;

    #[test]
    fn test_path_order_compares_digit_runs_as_numbers() {
        assert_eq!(path_order("db/V2__a.sql", "db/V10__b.sql"), Ordering::Less);
        assert_eq!(path_order("db/0001_a.sql", "db/0002_a.sql"), Ordering::Less);
        assert_eq!(path_order("db/a.sql", "db/a.sql"), Ordering::Equal);
        assert_eq!(path_order("db/a/9.sql", "db/b/1.sql"), Ordering::Less);
        assert_eq!(path_order("db/01.sql", "db/1.sql"), Ordering::Greater);
    }
}
