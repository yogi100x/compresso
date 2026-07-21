use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;

use compresso_core::{TokenProvider, counter_for};
use compresso_store::{FileLanguage, RepoId, Store, SymbolRecord};

use crate::AtlasError;

const TOP_SYMBOLS_PER_FILE: usize = 3;

/// A deterministic, token-budgeted structural summary of an indexed repository.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RepoMap {
    pub rendered: String,
    pub files_total: usize,
    pub files_shown: usize,
    pub truncated: bool,
    pub tokens_used: usize,
    pub token_provider: TokenProvider,
}

/// The complete stored symbol skeleton for one indexed file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Outline {
    pub path: String,
    pub language: Option<FileLanguage>,
    pub symbols: Vec<SymbolRecord>,
}

/// Builds a directory tree with the most important indexed files selected first.
///
/// File importance is the number of distinct indexed files with resolved incoming
/// edges. Ties and final tree presentation are repository-relative path ordered.
pub fn repo_map(store: &Store, repo: RepoId, budget_tokens: usize) -> Result<RepoMap, AtlasError> {
    let counter = counter_for(TokenProvider::OpenAiO200k);
    let files = store.indexed_files(repo)?;
    let mut importers = BTreeMap::<String, BTreeSet<String>>::new();
    for edge in store.resolved_edges(repo)? {
        importers
            .entry(edge.to_path)
            .or_default()
            .insert(edge.from_path);
    }

    let mut ranked = files
        .iter()
        .map(|file| {
            let in_degree = importers.get(&file.path).map_or(0, BTreeSet::len);
            let symbols = store.symbols_in_file(repo, &file.path)?;
            Ok((file.path.clone(), in_degree, symbols))
        })
        .collect::<Result<Vec<_>, compresso_store::StoreError>>()?;
    ranked.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));

    let mut selected = BTreeMap::<String, Vec<String>>::new();
    let mut budget_truncated_symbols = false;
    for (path, _, symbols) in ranked {
        let top_symbols = symbols
            .into_iter()
            .take(TOP_SYMBOLS_PER_FILE)
            .map(|symbol| symbol.qualified_name)
            .collect::<Vec<_>>();
        let mut accepted = None;
        for symbol_count in (0..=top_symbols.len()).rev() {
            let mut trial = selected.clone();
            trial.insert(path.clone(), top_symbols[..symbol_count].to_vec());
            let rendered = render_directory_tree(&trial);
            if counter.count(&rendered) <= budget_tokens {
                accepted = Some((trial, symbol_count < top_symbols.len()));
                break;
            }
        }
        let Some((trial, truncated_symbols)) = accepted else {
            break;
        };
        selected = trial;
        budget_truncated_symbols |= truncated_symbols;
    }

    let rendered = render_directory_tree(&selected);
    let tokens_used = counter.count(&rendered);
    let files_total = files.len();
    let files_shown = selected.len();
    Ok(RepoMap {
        rendered,
        files_total,
        files_shown,
        truncated: files_shown < files_total || budget_truncated_symbols,
        tokens_used,
        token_provider: counter.provider(),
    })
}

/// Returns every stored symbol for `path` in deterministic source order.
pub fn outline(store: &Store, repo: RepoId, path: &str) -> Result<Outline, AtlasError> {
    let Some(file) = store.indexed_file(repo, path)? else {
        return Err(AtlasError::FileNotIndexed {
            repo,
            path: path.to_owned(),
        });
    };
    let symbols = store.symbols_in_file(repo, path)?;
    Ok(Outline {
        path: file.path,
        language: file.language,
        symbols,
    })
}

#[derive(Default)]
struct DirectoryNode {
    directories: BTreeMap<String, DirectoryNode>,
    files: BTreeMap<String, Vec<String>>,
}

fn render_directory_tree(files: &BTreeMap<String, Vec<String>>) -> String {
    let mut root = DirectoryNode::default();
    for (path, symbols) in files {
        let mut components = path.split('/').peekable();
        let mut directory = &mut root;
        while let Some(component) = components.next() {
            if components.peek().is_none() {
                directory
                    .files
                    .insert(component.to_owned(), symbols.clone());
            } else {
                directory = directory
                    .directories
                    .entry(component.to_owned())
                    .or_default();
            }
        }
    }

    let mut rendered = String::new();
    render_directory(&root, 0, &mut rendered);
    rendered
}

fn render_directory(directory: &DirectoryNode, depth: usize, output: &mut String) {
    let indent = "  ".repeat(depth);
    for (name, symbols) in &directory.files {
        let _ = write!(output, "{indent}{name}");
        if !symbols.is_empty() {
            let _ = write!(output, "  — {}", symbols.join(", "));
        }
        output.push('\n');
    }
    for (name, child) in &directory.directories {
        let _ = writeln!(output, "{indent}{name}/");
        render_directory(child, depth + 1, output);
    }
}
