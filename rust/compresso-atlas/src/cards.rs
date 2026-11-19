//! Deterministic per-directory module cards (E17-26, D91).
//!
//! A card summarizes one directory of indexed files for orientation: a
//! purpose line taken from a module or file doc comment, the top public
//! symbols with their kinds, entry-point files, internal dependencies and
//! dependents (other directories, from resolved import edges), and file and
//! symbol counts.
//!
//! Cards are built only from the index plus, for the purpose line, a bounded
//! read of the first [`PURPOSE_HEAD_BYTES`] of at most
//! [`PURPOSE_CANDIDATES`] files per card. That read goes through the same
//! root containment and secret-path checks as bounded reads and is skipped
//! for any file that no longer matches its index record, so a stale file
//! never contributes text. No model is called and the same index always
//! yields the same cards.
//!
//! The store keeps no visibility column, so "public" is a per-language
//! signature heuristic (see [`is_public`]).

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read as _;
use std::path::Path;

use compresso_core::{TokenCounter, TokenProvider, counter_for};
use compresso_store::{
    EdgeKind, FileLanguage, FileRecord, RepoId, Store, SymbolKind, SymbolRecord,
};

use crate::AtlasError;
use crate::parsers::languages::{self, ModuleDoc, Visibility};
use crate::queries::{
    ResolvedEntry, canonical_query_root, file_matches_index, map_path_matches_focus,
    resolve_under_root, validate_map_focus,
};
use crate::scanner::is_secret_path;

/// Cards returned when the caller does not choose a count.
pub const DEFAULT_MAX_CARDS: usize = 12;
/// Hard ceiling on cards per request.
pub const MAX_CARDS: usize = 50;
const SYMBOLS_PER_CARD: usize = 6;
const LINKS_PER_CARD: usize = 6;
const ENTRY_POINTS_PER_CARD: usize = 4;
/// Bytes read from the start of a file when looking for its doc comment.
const PURPOSE_HEAD_BYTES: u64 = 4 * 1024;
const PURPOSE_MAX_CHARS: usize = 160;
/// Files per card whose head may be read for a purpose line.
const PURPOSE_CANDIDATES: usize = 3;

/// Caller bounds for [`module_cards`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CardOptions<'a> {
    /// Repository-relative directory prefix; only cards at or below it are built.
    pub path: Option<&'a str>,
    /// Maximum cards returned, clamped to `1..=MAX_CARDS`.
    pub max_cards: usize,
    /// Hard token budget for all rendered cards together.
    pub budget_tokens: usize,
}

/// One public symbol shown on a card.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CardSymbol {
    pub name: String,
    pub kind: SymbolKind,
    /// File name within the card's directory.
    pub file: String,
    pub line: u32,
}

/// A deterministic summary of one directory's directly contained files.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModuleCard {
    /// Repository-relative directory; `.` for the repository root.
    pub path: String,
    /// First sentence of a module or file doc comment, when one exists.
    pub purpose: Option<String>,
    pub files: usize,
    pub symbols: usize,
    /// Entry-point file names, at most four.
    pub entry_points: Vec<String>,
    /// Top public symbols, at most six.
    pub public_symbols: Vec<CardSymbol>,
    /// Directories this one imports from, most-used first, at most six.
    pub depends_on: Vec<String>,
    pub depends_on_total: usize,
    /// Directories that import from this one, most-dependent first, at most six.
    pub used_by: Vec<String>,
    pub used_by_total: usize,
    /// Compact text form; the token budget is enforced on this text.
    pub rendered: String,
    pub tokens: usize,
}

/// Bounded card set for one repository or subtree.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModuleCards {
    pub cards: Vec<ModuleCard>,
    /// Directories in scope that would have a card without the bounds.
    pub cards_total: usize,
    pub tokens_used: usize,
    pub token_provider: TokenProvider,
    /// True when the card cap or token budget left a card out.
    pub truncated: bool,
}

#[derive(Default)]
struct Directory {
    files: Vec<FileRecord>,
    symbols: Vec<(String, SymbolRecord, Option<FileLanguage>)>,
    entry_points: BTreeSet<String>,
    depends_on: BTreeMap<String, usize>,
    used_by: BTreeMap<String, usize>,
}

/// Builds bounded, deterministic cards for the directories of indexed files.
///
/// Cards are ranked by how many other directories import from them, then by
/// symbol count, then by path. `root` must be the repository's canonical
/// root; it is used only for the bounded purpose-line read.
pub fn module_cards(
    store: &Store,
    repo: RepoId,
    root: &Path,
    options: CardOptions<'_>,
) -> Result<ModuleCards, AtlasError> {
    let prefix = options.path.map(validate_map_focus).transpose()?.flatten();
    let max_cards = options.max_cards.clamp(1, MAX_CARDS);
    let counter = counter_for(TokenProvider::OpenAiO200k);
    let root = canonical_query_root(root)?;

    let mut directories = BTreeMap::<String, Directory>::new();
    let mut file_directory = BTreeMap::<String, String>::new();
    for file in store.indexed_files(repo)? {
        if is_secret_path(Path::new(&file.path)) {
            continue;
        }
        let directory = directory_of(&file.path);
        file_directory.insert(file.path.clone(), directory.clone());
        let entry = directories.entry(directory).or_default();
        let file_name = file_name_of(&file.path).to_owned();
        let symbols = store.symbols_in_file(repo, &file.path)?;
        if languages::is_entry_file(&file_name)
            || symbols.iter().any(|symbol| {
                symbol.kind == SymbolKind::Function
                    && symbol.name == "main"
                    && symbol.qualified_name == "main"
            })
        {
            entry.entry_points.insert(file_name.clone());
        }
        for symbol in symbols {
            entry
                .symbols
                .push((file_name.clone(), symbol, file.language));
        }
        entry.files.push(file);
    }
    for edge in store.resolved_edges(repo)? {
        if edge.kind != EdgeKind::Imports {
            continue;
        }
        let (Some(from), Some(to)) = (
            file_directory.get(&edge.from_path),
            file_directory.get(&edge.to_path),
        ) else {
            // Either end is a secret path or no longer indexed.
            continue;
        };
        if from == to {
            continue;
        }
        let (from, to) = (from.clone(), to.clone());
        if let Some(directory) = directories.get_mut(&from) {
            *directory.depends_on.entry(to.clone()).or_default() += 1;
        }
        if let Some(directory) = directories.get_mut(&to) {
            *directory.used_by.entry(from).or_default() += 1;
        }
    }
    let file_in_degree = file_in_degrees(store, repo, &file_directory)?;

    let mut ranked = directories
        .into_iter()
        .filter(|(path, _)| prefix.is_none_or(|prefix| directory_in_scope(path, prefix)))
        .collect::<Vec<_>>();
    ranked.sort_by(|(left_path, left), (right_path, right)| {
        right
            .used_by
            .len()
            .cmp(&left.used_by.len())
            .then_with(|| right.symbols.len().cmp(&left.symbols.len()))
            .then_with(|| left_path.cmp(right_path))
    });
    let cards_total = ranked.len();

    let mut cards = Vec::new();
    let mut tokens_used = 0_usize;
    let mut truncated = false;
    for (path, directory) in ranked {
        if cards.len() == max_cards {
            truncated = true;
            break;
        }
        let card = build_card(&root, &path, directory, &file_in_degree, counter);
        // A separating newline between cards counts toward the budget.
        let cost = card.tokens + usize::from(!cards.is_empty());
        if tokens_used.saturating_add(cost) > options.budget_tokens {
            truncated = true;
            break;
        }
        tokens_used += cost;
        cards.push(card);
    }
    Ok(ModuleCards {
        cards,
        cards_total,
        tokens_used,
        token_provider: counter.provider(),
        truncated,
    })
}

/// Number of distinct files outside each file's directory that import it.
fn file_in_degrees(
    store: &Store,
    repo: RepoId,
    file_directory: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, usize>, AtlasError> {
    let mut importers = BTreeMap::<String, BTreeSet<String>>::new();
    for edge in store.resolved_edges(repo)? {
        if edge.kind == EdgeKind::Imports
            && file_directory.contains_key(&edge.from_path)
            && file_directory.contains_key(&edge.to_path)
        {
            importers
                .entry(edge.to_path)
                .or_default()
                .insert(edge.from_path);
        }
    }
    Ok(importers
        .into_iter()
        .map(|(path, from)| (path, from.len()))
        .collect())
}

fn build_card(
    root: &Path,
    path: &str,
    directory: Directory,
    file_in_degree: &BTreeMap<String, usize>,
    counter: &dyn TokenCounter,
) -> ModuleCard {
    let Directory {
        files,
        symbols,
        entry_points,
        depends_on,
        used_by,
    } = directory;
    let symbol_count = symbols.len();
    let degree_of = |file_name: &str| {
        file_in_degree
            .get(&join_path(path, file_name))
            .copied()
            .unwrap_or(0)
    };

    let mut public = symbols
        .into_iter()
        .filter(|(_, symbol, language)| is_public(symbol, *language))
        .collect::<Vec<_>>();
    public.sort_by(|(left_file, left, _), (right_file, right, _)| {
        kind_rank(left.kind)
            .cmp(&kind_rank(right.kind))
            .then_with(|| degree_of(right_file).cmp(&degree_of(left_file)))
            .then_with(|| left_file.cmp(right_file))
            .then_with(|| left.start_line.cmp(&right.start_line))
            .then_with(|| left.qualified_name.cmp(&right.qualified_name))
    });
    let public_symbols = public
        .into_iter()
        .take(SYMBOLS_PER_CARD)
        .map(|(file, symbol, _)| CardSymbol {
            name: symbol.qualified_name,
            kind: symbol.kind,
            file,
            line: symbol.start_line,
        })
        .collect::<Vec<_>>();

    let purpose = purpose_candidates(path, &files, &entry_points, &degree_of)
        .into_iter()
        .find_map(|file| purpose_of(root, file));
    let (depends_on, depends_on_total) = top_links(depends_on);
    let (used_by, used_by_total) = top_links(used_by);
    let mut card = ModuleCard {
        path: if path.is_empty() {
            ".".to_owned()
        } else {
            path.to_owned()
        },
        purpose,
        files: files.len(),
        symbols: symbol_count,
        entry_points: entry_points
            .into_iter()
            .take(ENTRY_POINTS_PER_CARD)
            .collect(),
        public_symbols,
        depends_on,
        depends_on_total,
        used_by,
        used_by_total,
        rendered: String::new(),
        tokens: 0,
    };
    card.rendered = render_card(&card);
    card.tokens = counter.count(&card.rendered);
    card
}

/// Entry points first, then the most-imported files, then path order.
fn purpose_candidates<'a>(
    path: &str,
    files: &'a [FileRecord],
    entry_points: &BTreeSet<String>,
    degree_of: &dyn Fn(&str) -> usize,
) -> Vec<&'a FileRecord> {
    let mut candidates = files
        .iter()
        .filter(|file| {
            file.language
                .is_some_and(|language| language != FileLanguage::Text)
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        let left_name = file_name_of(&left.path);
        let right_name = file_name_of(&right.path);
        entry_points
            .contains(right_name)
            .cmp(&entry_points.contains(left_name))
            .then_with(|| degree_of(right_name).cmp(&degree_of(left_name)))
            .then_with(|| left.path.cmp(&right.path))
    });
    debug_assert!(
        candidates
            .iter()
            .all(|file| directory_of(&file.path) == path)
    );
    candidates.truncate(PURPOSE_CANDIDATES);
    candidates
}

/// Reads the head of one in-root, non-secret, index-current file and extracts
/// the first sentence of its leading doc comment. Any failure yields `None`.
fn purpose_of(root: &Path, file: &FileRecord) -> Option<String> {
    let Ok(ResolvedEntry::File(resolved)) = resolve_under_root(root, &file.path) else {
        return None;
    };
    if !file_matches_index(&resolved, file).ok()? {
        return None;
    }
    let mut head = Vec::new();
    resolved
        .open()
        .ok()?
        .take(PURPOSE_HEAD_BYTES)
        .read_to_end(&mut head)
        .ok()?;
    let text = String::from_utf8_lossy(&head);
    let doc = match languages::spec(file.language?).module_doc {
        ModuleDoc::Rust => rust_module_doc(&text),
        ModuleDoc::Python => python_module_doc(&text),
        ModuleDoc::Script => script_module_doc(&text),
        ModuleDoc::Shell => shell_module_doc(&text),
        ModuleDoc::Go => go_package_doc(&text),
        ModuleDoc::None => None,
    }?;
    first_sentence(&doc)
}

/// Leading `//!` lines or a leading `/*! ... */` block, after attributes.
fn rust_module_doc(text: &str) -> Option<String> {
    let mut lines = text
        .lines()
        .map(str::trim)
        .skip_while(|line| line.is_empty() || line.starts_with("#!["));
    let first = lines.next()?;
    if let Some(rest) = first.strip_prefix("//!") {
        let mut doc = vec![rest.trim().to_owned()];
        doc.extend(
            lines
                .map_while(|line| line.strip_prefix("//!"))
                .map(|line| line.trim().to_owned()),
        );
        return Some(doc.join(" "));
    }
    let start = text.find("/*!")?;
    text[..start].trim().is_empty().then_some(())?;
    let body = &text[start + 3..];
    Some(block_comment_text(&body[..body.find("*/")?]))
}

/// A module docstring: the first statement, when it is a string literal.
fn python_module_doc(text: &str) -> Option<String> {
    let mut rest = text;
    loop {
        let trimmed = rest.trim_start();
        if trimmed.starts_with('#') {
            rest = trimmed.split_once('\n').map_or("", |(_, after)| after);
        } else {
            rest = trimmed;
            break;
        }
    }
    let rest = rest
        .strip_prefix(['r', 'R', 'u', 'U'])
        .filter(|after| after.starts_with(['"', '\'']))
        .unwrap_or(rest);
    for quote in ["\"\"\"", "'''", "\"", "'"] {
        if let Some(body) = rest.strip_prefix(quote) {
            let end = body.find(quote)?;
            return Some(body[..end].to_owned());
        }
    }
    None
}

/// A leading `/** ... */` block after an optional shebang; license headers
/// are not purposes.
fn script_module_doc(text: &str) -> Option<String> {
    let text = if text.starts_with("#!") {
        text.split_once('\n').map_or("", |(_, rest)| rest)
    } else {
        text
    };
    let trimmed = text.trim_start();
    let body = trimmed.strip_prefix("/**")?;
    let doc = block_comment_text(&body[..body.find("*/")?]);
    let lowered = doc.to_ascii_lowercase();
    (!lowered.contains("copyright") && !lowered.contains("license")).then_some(doc)
}

/// Leading `#` comment lines after the shebang.
fn shell_module_doc(text: &str) -> Option<String> {
    let doc = text
        .lines()
        .skip_while(|line| line.starts_with("#!"))
        .map(str::trim)
        .skip_while(|line| line.is_empty())
        .map_while(|line| line.strip_prefix('#'))
        .map(str::trim)
        .collect::<Vec<_>>()
        .join(" ");
    let lowered = doc.to_ascii_lowercase();
    (!doc.is_empty() && !lowered.contains("copyright") && !lowered.contains("license"))
        .then_some(doc)
}

/// The comment directly above the `package` clause: `//` lines or one
/// `/* */` block with no blank line between it and `package`. Build
/// constraints (`//go:build`, `// +build`) and license headers are not
/// purposes.
fn go_package_doc(text: &str) -> Option<String> {
    let mut comment = Vec::new();
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        let line = line.trim();
        if line.starts_with("package ") || line == "package" {
            break;
        }
        if line.is_empty() {
            comment.clear();
        } else if let Some(rest) = line.strip_prefix("//") {
            if rest.starts_with("go:") || rest.trim_start().starts_with("+build") {
                comment.clear();
            } else {
                comment.push(rest.trim().to_owned());
            }
        } else if let Some(rest) = line.strip_prefix("/*") {
            comment.clear();
            let mut block = rest.to_owned();
            while !block.contains("*/") {
                block.push('\n');
                block.push_str(lines.next()?);
            }
            let (body, after) = block.split_once("*/")?;
            if !after.trim().is_empty() {
                return None;
            }
            comment.push(block_comment_text(body));
        } else {
            return None;
        }
    }
    let doc = comment.join(" ");
    let lowered = doc.to_ascii_lowercase();
    (!doc.trim().is_empty() && !lowered.contains("copyright") && !lowered.contains("license"))
        .then_some(doc)
}

fn block_comment_text(body: &str) -> String {
    body.lines()
        .map(|line| line.trim().trim_start_matches('*').trim())
        .filter(|line| !line.starts_with('@'))
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_owned()
}

/// Collapses whitespace, drops Markdown heading marks and control characters,
/// keeps the first sentence, and caps it at [`PURPOSE_MAX_CHARS`].
fn first_sentence(doc: &str) -> Option<String> {
    let collapsed = doc
        .split_whitespace()
        .filter(|word| !word.chars().all(|character| character == '#'))
        .collect::<Vec<_>>()
        .join(" ");
    let cleaned = collapsed
        .chars()
        .filter(|character| !character.is_control())
        .collect::<String>();
    let sentence = cleaned
        .match_indices(". ")
        .next()
        .map_or(cleaned.as_str(), |(index, _)| &cleaned[..=index])
        .trim();
    if sentence.is_empty() {
        return None;
    }
    if sentence.chars().count() <= PURPOSE_MAX_CHARS {
        return Some(sentence.to_owned());
    }
    let mut capped = sentence
        .chars()
        .take(PURPOSE_MAX_CHARS - 3)
        .collect::<String>();
    capped.push_str("...");
    Some(capped)
}

/// Signature-based visibility: the store records no visibility column.
///
/// Rust: a top-level item whose signature starts with `pub`. TypeScript and
/// JavaScript: a top-level `export`. Python: a top-level name without a
/// leading underscore. Shell: every top-level function. Go: a top-level name
/// that starts with an uppercase letter. Text headings are never public
/// symbols.
fn is_public(symbol: &SymbolRecord, language: Option<FileLanguage>) -> bool {
    let top_level = symbol.qualified_name == symbol.name;
    let signature = symbol.signature.as_deref().unwrap_or_default();
    let Some(language) = language else {
        return false;
    };
    match languages::spec(language).visibility {
        Visibility::PubKeyword => {
            top_level && (signature.starts_with("pub ") || signature.starts_with("pub("))
        }
        Visibility::Export => top_level && signature.starts_with("export"),
        Visibility::NoLeadingUnderscore => top_level && !symbol.name.starts_with('_'),
        Visibility::TopLevel => top_level,
        Visibility::Capitalized => {
            top_level && symbol.name.chars().next().is_some_and(char::is_uppercase)
        }
        Visibility::Never => false,
    }
}

const fn kind_rank(kind: SymbolKind) -> u8 {
    match kind {
        SymbolKind::Trait | SymbolKind::Interface => 0,
        SymbolKind::Struct | SymbolKind::Class | SymbolKind::Enum | SymbolKind::Type => 1,
        SymbolKind::Function => 2,
        SymbolKind::Module => 3,
        SymbolKind::Const => 4,
        SymbolKind::Method => 5,
    }
}

const fn kind_name(kind: SymbolKind) -> &'static str {
    match kind {
        SymbolKind::Function => "function",
        SymbolKind::Method => "method",
        SymbolKind::Struct => "struct",
        SymbolKind::Class => "class",
        SymbolKind::Enum => "enum",
        SymbolKind::Trait => "trait",
        SymbolKind::Interface => "interface",
        SymbolKind::Const => "const",
        SymbolKind::Type => "type",
        SymbolKind::Module => "module",
    }
}

/// Most-linked first, then path order; returns the capped list and the total.
fn top_links(links: BTreeMap<String, usize>) -> (Vec<String>, usize) {
    let total = links.len();
    let mut ranked = links.into_iter().collect::<Vec<_>>();
    ranked.sort_by(|(left, left_count), (right, right_count)| {
        right_count.cmp(left_count).then_with(|| left.cmp(right))
    });
    (
        ranked
            .into_iter()
            .take(LINKS_PER_CARD)
            .map(|(path, _)| {
                if path.is_empty() {
                    ".".to_owned()
                } else {
                    path
                }
            })
            .collect(),
        total,
    )
}

fn render_card(card: &ModuleCard) -> String {
    let mut lines = vec![format!(
        "{}/ ({} files, {} symbols)",
        card.path.trim_end_matches('/'),
        card.files,
        card.symbols
    )];
    if let Some(purpose) = &card.purpose {
        lines.push(format!("  purpose: {purpose}"));
    }
    if !card.entry_points.is_empty() {
        lines.push(format!("  entry: {}", card.entry_points.join(", ")));
    }
    if !card.public_symbols.is_empty() {
        let symbols = card
            .public_symbols
            .iter()
            .map(|symbol| {
                format!(
                    "{} ({} {}:{})",
                    symbol.name,
                    kind_name(symbol.kind),
                    symbol.file,
                    symbol.line
                )
            })
            .collect::<Vec<_>>();
        lines.push(format!("  public: {}", symbols.join(", ")));
    }
    if !card.depends_on.is_empty() {
        lines.push(format!(
            "  uses: {}{}",
            card.depends_on.join(", "),
            more(card.depends_on_total, card.depends_on.len())
        ));
    }
    if !card.used_by.is_empty() {
        lines.push(format!(
            "  used by: {}{}",
            card.used_by.join(", "),
            more(card.used_by_total, card.used_by.len())
        ));
    }
    lines.join("\n")
}

fn more(total: usize, shown: usize) -> String {
    if total > shown {
        format!(" (+{} more)", total - shown)
    } else {
        String::new()
    }
}

fn directory_of(path: &str) -> String {
    path.rsplit_once('/')
        .map_or(String::new(), |(directory, _)| directory.to_owned())
}

fn file_name_of(path: &str) -> &str {
    path.rsplit_once('/').map_or(path, |(_, name)| name)
}

fn join_path(directory: &str, file_name: &str) -> String {
    if directory.is_empty() {
        file_name.to_owned()
    } else {
        format!("{directory}/{file_name}")
    }
}

fn directory_in_scope(directory: &str, prefix: &str) -> bool {
    map_path_matches_focus(directory, prefix)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rust_module_doc_reads_inner_line_and_block_comments() {
        assert_eq!(
            rust_module_doc("#![forbid(unsafe_code)]\n//! Session recovery.\n//! More.\nuse x;\n")
                .as_deref(),
            Some("Session recovery. More.")
        );
        assert_eq!(
            rust_module_doc("/*! Block\n * purpose. */\nfn x() {}\n").as_deref(),
            Some("Block purpose.")
        );
        assert_eq!(rust_module_doc("use x;\n//! late\n"), None);
        assert_eq!(rust_module_doc("// plain comment\n"), None);
    }

    #[test]
    fn test_python_script_and_shell_docs() {
        assert_eq!(
            python_module_doc("#!/usr/bin/env python\n# coding: utf-8\n\"\"\"Recover sessions.\n\nDetails.\"\"\"\n")
                .as_deref(),
            Some("Recover sessions.\n\nDetails.")
        );
        assert_eq!(python_module_doc("import os\n\"\"\"late\"\"\""), None);
        assert_eq!(
            script_module_doc("/**\n * Router setup.\n * @module router\n */\nexport {}\n")
                .as_deref(),
            Some("Router setup.")
        );
        assert_eq!(script_module_doc("/** Copyright 2026 */\n"), None);
        assert_eq!(
            shell_module_doc("#!/bin/sh\n# Builds the bundle.\nset -e\n").as_deref(),
            Some("Builds the bundle.")
        );
    }

    #[test]
    fn test_first_sentence_is_single_line_and_capped() {
        assert_eq!(
            first_sentence("# Title\n\nDoes one thing. Then another.").as_deref(),
            Some("Title Does one thing.")
        );
        let long = "word ".repeat(100);
        let capped = first_sentence(&long).expect("sentence");
        assert_eq!(capped.chars().count(), PURPOSE_MAX_CHARS);
        assert!(capped.ends_with("..."));
        assert_eq!(first_sentence("  \n "), None);
        assert_eq!(
            first_sentence("a\u{0007}b").as_deref(),
            Some("ab"),
            "control characters are dropped"
        );
    }

    #[test]
    fn test_visibility_heuristic_per_language() {
        let symbol = |name: &str, qualified: &str, signature: &str| SymbolRecord {
            name: name.to_owned(),
            qualified_name: qualified.to_owned(),
            kind: SymbolKind::Function,
            signature: Some(signature.to_owned()),
            start_line: 1,
            end_line: 1,
            token_estimate: 1,
        };
        let rust = Some(FileLanguage::Rust);
        assert!(is_public(&symbol("run", "run", "pub fn run()"), rust));
        assert!(is_public(
            &symbol("run", "run", "pub(crate) fn run()"),
            rust
        ));
        assert!(!is_public(&symbol("run", "run", "fn run()"), rust));
        assert!(!is_public(
            &symbol("start", "S::start", "pub fn start()"),
            rust
        ));
        let ts = Some(FileLanguage::TypeScript);
        assert!(is_public(&symbol("f", "f", "export function f()"), ts));
        assert!(!is_public(&symbol("f", "f", "function f()"), ts));
        let py = Some(FileLanguage::Python);
        assert!(is_public(&symbol("load", "load", "def load()"), py));
        assert!(!is_public(&symbol("_load", "_load", "def _load()"), py));
        assert!(!is_public(
            &symbol("h", "h", "# h"),
            Some(FileLanguage::Text)
        ));
    }

    #[test]
    fn test_go_exported_names_are_capitalized_top_level_symbols() {
        let symbol = |name: &str, qualified: &str| SymbolRecord {
            name: name.to_owned(),
            qualified_name: qualified.to_owned(),
            kind: SymbolKind::Function,
            signature: Some(format!("func {name}()")),
            start_line: 1,
            end_line: 1,
            token_estimate: 1,
        };
        let go = Some(FileLanguage::Go);
        assert!(is_public(&symbol("Open", "Open"), go));
        assert!(!is_public(&symbol("open", "open"), go));
        assert!(!is_public(&symbol("_Open", "_Open"), go));
        assert!(!is_public(&symbol("Close", "Store::Close"), go));
    }

    #[test]
    fn test_go_package_doc_is_the_comment_above_the_package_clause() {
        assert_eq!(
            go_package_doc("// Package store keeps jobs.\n// More.\npackage store\n").as_deref(),
            Some("Package store keeps jobs. More.")
        );
        assert_eq!(
            go_package_doc("//go:build linux\n\n// Package p does x.\npackage p\n").as_deref(),
            Some("Package p does x.")
        );
        assert_eq!(
            go_package_doc("/*\nPackage p reads input.\n*/\npackage p\n").as_deref(),
            Some("Package p reads input.")
        );
        assert_eq!(
            go_package_doc(
                "// Copyright 2026 Example.\n// Use of this source is governed by a license.\npackage p\n"
            ),
            None
        );
        assert_eq!(go_package_doc("// Detached.\n\npackage p\n"), None);
        assert_eq!(go_package_doc("package p\n// Late.\n"), None);
    }
}
