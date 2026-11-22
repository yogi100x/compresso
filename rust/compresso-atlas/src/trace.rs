//! Call-path tracing over stored call sites (D90, ADR-0033).
//!
//! `trace` answers "how does A reach B" (shortest call paths), "what does A
//! call" (callees), and "who calls B" (callers) in one bounded query. Call
//! sites are stored as written; each one is resolved here against the current
//! symbols and import edges with a conservative name rule, and only resolved
//! calls are followed. Ambiguous and unresolved calls met on the way are
//! reported, never guessed.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;
use std::rc::Rc;

use compresso_store::{
    CallForm, FileLanguage, IndexedSymbol, RepoId, Store, StoredCallSite, SymbolKind,
};

use crate::AtlasError;
use crate::declared_imports::GLOBAL_IMPORT_PREFIX;
use crate::parsers::ParserRegistry;
use crate::parsers::calls::GO_SHADOWED_QUALIFIER;
use crate::parsers::go_package_local_name;
use crate::parsers::languages::{
    CallRules, DeclaredRules, ImportStyle, call_rules, declaring_languages, is_test_file,
    package_is_directory,
};
use crate::parsers::signature_modifiers as modifiers;
use crate::queries::QueryError;
use crate::queries::{global_using_scope, in_scope, project_directories};
use crate::scanner::is_secret_path;

/// Deepest call path or neighbour expansion a trace follows.
pub const TRACE_MAX_DEPTH: u8 = 8;
/// Default depth for a path search between two symbols.
pub const TRACE_DEFAULT_PATH_DEPTH: u8 = 6;
/// Default depth for callers or callees of one symbol.
pub const TRACE_DEFAULT_NEIGHBOR_DEPTH: u8 = 1;
const TRACE_MAX_PATHS: usize = 10;
const TRACE_MAX_NODES: usize = 20_000;
const TRACE_MAX_RESULTS: usize = 500;
/// Unresolved or ambiguous calls listed in a report; the rest are counted.
const TRACE_UNRESOLVED_LISTED: usize = 10;
/// Candidates listed for an ambiguous name or call.
const TRACE_CANDIDATES_LISTED: usize = 10;
const SIGNATURE_MAX_CHARS: usize = 160;
/// Partial paths examined while enumerating shortest paths.
const TRACE_PATH_ENUMERATION_STEPS: usize = 10_000;
/// Stored call-site rows one trace reads, across every depth, name, and
/// direction. `max_nodes` bounds the symbols found; this bounds the work
/// spent finding them (a popular name can have many call sites that resolve
/// elsewhere). Reaching it sets `truncated`.
pub const TRACE_MAX_CALL_SITE_ROWS: usize = 100_000;

/// Explicit bounds for one trace. `max_depth: None` uses
/// [`TRACE_DEFAULT_PATH_DEPTH`] for a path search and
/// [`TRACE_DEFAULT_NEIGHBOR_DEPTH`] otherwise.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TraceOptions {
    pub max_depth: Option<u8>,
    pub max_paths: usize,
    pub max_nodes: usize,
    pub max_results: usize,
}

impl Default for TraceOptions {
    fn default() -> Self {
        Self {
            max_depth: None,
            max_paths: 3,
            max_nodes: 2_000,
            max_results: 50,
        }
    }
}

/// Which end of the trace a lookup outcome concerns.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TraceEnd {
    From,
    To,
}

impl TraceEnd {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::From => "from",
            Self::To => "to",
        }
    }
}

/// The rule that resolved a call to its callee.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CallResolution {
    /// The only candidate in the caller's file.
    SameFile,
    /// The only candidate in another file of the caller's package: its
    /// directory in Go, its declared package in Java, and its namespace (or
    /// an enclosing one) in C#, which code reaches without importing it.
    SamePackage,
    /// The only candidate in a file the caller's file imports.
    Import,
    /// The only candidate in the repository.
    UniqueName,
    /// A method call on an expression of unknown type whose name is unique
    /// in the repository; it may be a library method of the same name.
    NameOnly,
}

impl CallResolution {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SameFile => "same_file",
            Self::SamePackage => "same_package",
            Self::Import => "import",
            Self::UniqueName => "unique_name",
            Self::NameOnly => "name_only",
        }
    }
}

/// One symbol on a trace.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TraceSymbol {
    pub path: String,
    pub qualified_name: String,
    pub kind: SymbolKind,
    pub start_line: u32,
    pub end_line: u32,
    /// First line of the declaration, at most 160 characters.
    pub signature: Option<String>,
}

/// Where a call is written.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TraceCallSite {
    pub path: String,
    pub line: u32,
}

/// One step of a call path: the symbol reached and the call that reached it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TraceHop {
    pub symbol: TraceSymbol,
    /// The call in the previous hop's body; `None` for the first hop.
    pub call_site: Option<TraceCallSite>,
    pub resolution: Option<CallResolution>,
    /// The previous hop names this symbol as a call argument (a function
    /// passed as a value) rather than calling it there.
    pub reference: bool,
}

/// One caller or callee found by a single-ended trace.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TraceNeighbor {
    pub depth: u8,
    pub symbol: TraceSymbol,
    /// Qualified name of the adjacent symbol one step closer to the start.
    pub via: String,
    /// The call that links `symbol` and `via` (written in the caller).
    pub call_site: TraceCallSite,
    pub resolution: CallResolution,
    /// The link is a function passed as a call argument, not a call.
    pub reference: bool,
    /// The call leads back to the start symbol (recursion or a cycle).
    pub cycle: bool,
}

/// A call met during the trace that was not followed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnfollowedCall {
    /// Qualified name of the symbol containing the call.
    pub caller: String,
    /// The call as written: `name`, `Qualifier::name`, `self.name`, or `.name`.
    pub callee: String,
    pub call_site: TraceCallSite,
    /// `ambiguous`, or `no_owner` for a `self` call outside any type.
    /// Library calls (no repository symbol of that name, or a Rust path
    /// naming no repository type or module) are not reported.
    pub status: &'static str,
    /// `path:qualified_name` of the candidates, for an ambiguous call.
    pub candidates: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TraceResult {
    /// Shortest call paths from `from` to `to`, each starting at `from`.
    Paths(Vec<Vec<TraceHop>>),
    /// No resolved call path within the bounds.
    NoPath,
    /// Callees of `from`, breadth first.
    Callees(Vec<TraceNeighbor>),
    /// Callers of `to`, breadth first.
    Callers(Vec<TraceNeighbor>),
    /// A name matched more than one symbol; nothing was traced.
    Ambiguous {
        end: TraceEnd,
        candidates: Vec<TraceSymbol>,
        total: usize,
    },
    /// A name matched no indexed symbol.
    NotFound { end: TraceEnd, name: String },
    /// The index was built by a parser without call sites; re-index first.
    CallGraphMissing,
}

/// A bounded trace outcome with completeness metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TraceReport {
    pub result: TraceResult,
    /// Callers or callees found before `max_results` cut the list; for
    /// paths, the number of shortest paths found before `max_paths`.
    pub total: usize,
    pub nodes_visited: usize,
    pub depth_reached: u8,
    pub max_depth: u8,
    /// A node, result, or path bound cut the answer short; asking again with
    /// the same bounds returns the same answer.
    pub truncated: bool,
    /// Symbols at `max_depth` were found but not expanded: a longer path, or
    /// (when `max_depth` was given explicitly) deeper callers or callees, may
    /// exist.
    pub depth_limited: bool,
    /// Among recorded direct calls, no bound was hit and no ambiguous or
    /// ownerless `self` call was met. Uses that are not recorded calls
    /// (a function passed as a value, a call at module level, a call through
    /// a trait object or callback) are outside this claim.
    pub complete: bool,
    pub unfollowed: Vec<UnfollowedCall>,
    pub unfollowed_total: usize,
}

/// Traces calls between `from` and `to`, or callees of `from` alone, or
/// callers of `to` alone.
///
/// A symbol is named `name`, `Owner::name` (or `Owner.name`), optionally
/// prefixed by `path:` (a repository-relative file path or its suffix).
/// Reads only; executes nothing.
pub fn trace(
    store: &Store,
    repo: RepoId,
    from: Option<&str>,
    to: Option<&str>,
    options: TraceOptions,
) -> Result<TraceReport, AtlasError> {
    let mut report =
        trace_with_row_budget(store, repo, from, to, options, TRACE_MAX_CALL_SITE_ROWS)?;
    // A resolved import whose target is not committed yet (D106) hides the
    // calls a reference through it would resolve to, so they are not found.
    if report.complete && store.edge_resolution_counts(repo)?.pending > 0 {
        report.complete = false;
    }
    Ok(report)
}

/// [`trace`] with an explicit call-site row budget.
fn trace_with_row_budget(
    store: &Store,
    repo: RepoId,
    from: Option<&str>,
    to: Option<&str>,
    options: TraceOptions,
    call_site_rows: usize,
) -> Result<TraceReport, AtlasError> {
    let max_depth = validate_options(from, to, options)?;
    let empty = |result| TraceReport {
        result,
        total: 0,
        nodes_visited: 0,
        depth_reached: 0,
        max_depth,
        truncated: false,
        depth_limited: false,
        complete: true,
        unfollowed: Vec::new(),
        unfollowed_total: 0,
    };
    // Any index stamped by a parser that records call sites carries the
    // marker, whatever its exact version.
    let current = store.latest_index_version(repo)?.is_some_and(|version| {
        version
            .parser_version
            .contains(crate::indexer::CALL_GRAPH_MARKER)
    });
    if !current {
        return Ok(TraceReport {
            complete: false,
            ..empty(TraceResult::CallGraphMissing)
        });
    }

    let mut resolver = Resolver::new(store, repo);
    let mut ends = Vec::new();
    for (end, spec) in [(TraceEnd::From, from), (TraceEnd::To, to)] {
        let Some(spec) = spec else {
            ends.push(None);
            continue;
        };
        match lookup(store, repo, spec)? {
            Lookup::Found(symbol) => ends.push(Some(symbol)),
            Lookup::NotFound(name) => return Ok(empty(TraceResult::NotFound { end, name })),
            Lookup::Ambiguous(candidates) => {
                let total = candidates.len();
                return Ok(TraceReport {
                    total,
                    ..empty(TraceResult::Ambiguous {
                        end,
                        total: candidates.len(),
                        candidates: candidates
                            .iter()
                            .take(TRACE_CANDIDATES_LISTED)
                            .map(trace_symbol)
                            .collect(),
                    })
                });
            }
        }
    }
    let (from, to) = (ends[0].take(), ends[1].take());
    let mut walk = Walk::new(max_depth, options, call_site_rows);
    let result = match (from, to) {
        (Some(from), Some(to)) => walk.paths(&mut resolver, &from, &to)?,
        (Some(from), None) => walk.callees(&mut resolver, &from)?,
        (None, Some(to)) => walk.callers(&mut resolver, &to)?,
        (None, None) => unreachable!("validated: at least one end is named"),
    };
    let mut report = walk.report(result, max_depth);
    // A file declaring more packages than are read can hide a same-package
    // or imported candidate (Java, C#): the answer cannot be complete.
    if resolver.packages_limited {
        report.complete = false;
    }
    Ok(report)
}

fn validate_options(
    from: Option<&str>,
    to: Option<&str>,
    options: TraceOptions,
) -> Result<u8, AtlasError> {
    let invalid = |reason| Err(QueryError::InvalidTraceOptions { reason }.into());
    if from.is_none() && to.is_none() {
        return invalid("name at least one of from and to");
    }
    if [from, to]
        .into_iter()
        .flatten()
        .any(|spec| spec.trim().is_empty() || spec.len() > 512 || spec.contains('\0'))
    {
        return invalid("a symbol name must be 1 to 512 bytes without NUL");
    }
    let default = if from.is_some() && to.is_some() {
        TRACE_DEFAULT_PATH_DEPTH
    } else {
        TRACE_DEFAULT_NEIGHBOR_DEPTH
    };
    let max_depth = options.max_depth.unwrap_or(default);
    if !(1..=TRACE_MAX_DEPTH).contains(&max_depth) {
        return invalid("max_depth must be between 1 and 8");
    }
    if !(1..=TRACE_MAX_PATHS).contains(&options.max_paths) {
        return invalid("max_paths must be between 1 and 10");
    }
    if !(1..=TRACE_MAX_NODES).contains(&options.max_nodes) {
        return invalid("max_nodes must be between 1 and 20000");
    }
    if !(1..=TRACE_MAX_RESULTS).contains(&options.max_results) {
        return invalid("max_results must be between 1 and 500");
    }
    Ok(max_depth)
}

enum Lookup {
    Found(IndexedSymbol),
    NotFound(String),
    Ambiguous(Vec<IndexedSymbol>),
}

fn lookup(store: &Store, repo: RepoId, spec: &str) -> Result<Lookup, AtlasError> {
    let spec = spec.trim();
    let (path, symbol) = split_path(spec);
    let qualified = if symbol.contains("::") {
        symbol.to_owned()
    } else {
        symbol.replace('.', "::")
    };
    let name = qualified.rsplit("::").next().unwrap_or(&qualified).trim();
    if name.is_empty() {
        return Ok(Lookup::NotFound(spec.to_owned()));
    }
    let mut candidates = store
        .indexed_symbols_named(repo, name)?
        .into_iter()
        .filter(|candidate| !is_secret_path(Path::new(&candidate.path)))
        .filter(|candidate| {
            path.is_none_or(|path| {
                candidate.path == path || candidate.path.ends_with(&format!("/{path}"))
            })
        })
        .collect::<Vec<_>>();
    if qualified.contains("::") {
        let wanted = owner_base(&qualified);
        candidates.retain(|candidate| {
            candidate.symbol.qualified_name == qualified
                || (owner_base(&candidate.symbol.qualified_name) == wanted && wanted.is_some())
                // A SQL name is stored as written, `public.users` (E17-42).
                || (candidate.language == Some(FileLanguage::Sql)
                    && candidate.symbol.qualified_name == symbol)
        });
    }
    if candidates.len() > 1
        && candidates
            .iter()
            .any(|candidate| is_callable(candidate.symbol.kind))
    {
        candidates.retain(|candidate| is_callable(candidate.symbol.kind));
    }
    Ok(match candidates.len() {
        0 => Lookup::NotFound(spec.to_owned()),
        1 => Lookup::Found(candidates.remove(0)),
        _ => Lookup::Ambiguous(candidates),
    })
}

/// Splits `path:Symbol` on a single colon; `::` never splits.
fn split_path(spec: &str) -> (Option<&str>, &str) {
    let bytes = spec.as_bytes();
    for (index, byte) in bytes.iter().enumerate() {
        if *byte != b':' {
            continue;
        }
        let before = index.checked_sub(1).map(|previous| bytes[previous]);
        let after = bytes.get(index + 1).copied();
        if before != Some(b':') && after != Some(b':') {
            let path = spec[..index].trim();
            return ((!path.is_empty()).then_some(path), spec[index + 1..].trim());
        }
    }
    (None, spec)
}

const fn is_callable(kind: SymbolKind) -> bool {
    matches!(
        kind,
        SymbolKind::Function | SymbolKind::Method | SymbolKind::Class | SymbolKind::Struct
    )
}

/// The last owner segment of a qualified name, without generic arguments:
/// `Store<T>::open` gives `Store`; a name without an owner gives `None`.
pub(crate) fn owner_base(qualified: &str) -> Option<String> {
    let (owner, _) = qualified.rsplit_once("::")?;
    let owner = strip_generics(owner);
    let base = owner.rsplit("::").next().unwrap_or(&owner).trim();
    (!base.is_empty()).then(|| base.to_owned())
}

fn strip_generics(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut depth = 0_u32;
    for character in text.chars() {
        match character {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            _ if depth == 0 => output.push(character),
            _ => {}
        }
    }
    output
}

pub(crate) fn parent_directory(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(parent, _)| parent)
}

/// The module name a file provides: its stem, or its directory for
/// `mod.rs`, `__init__.py`, `index.*`, `lib.rs`, and `main.rs`, or the crate
/// directory for `src/lib.rs`. A Go file provides its directory's name, and
/// a Go file at the repository root provides none (its package name is not
/// indexed), so no qualifier names it. `-` reads as `_`.
fn module_name(path: &str) -> String {
    if package_is_directory(path) {
        // A Go package is its directory; `store.Open()` names `store/*.go`.
        return path
            .rsplit_once('/')
            .map_or_else(String::new, |(parent, _)| {
                parent
                    .rsplit('/')
                    .next()
                    .unwrap_or(parent)
                    .replace('-', "_")
            });
    }
    let mut segments = path.rsplit('/');
    let file = segments.next().unwrap_or(path);
    let stem = file.split('.').next().unwrap_or(file);
    let name = if matches!(stem, "mod" | "__init__" | "index" | "lib" | "main") {
        match segments.next() {
            Some("src") => segments.next().unwrap_or(stem),
            Some(directory) => directory,
            None => stem,
        }
    } else {
        stem
    };
    name.replace('-', "_")
}

fn trace_symbol(symbol: &IndexedSymbol) -> TraceSymbol {
    TraceSymbol {
        path: symbol.path.clone(),
        qualified_name: symbol.symbol.qualified_name.clone(),
        kind: symbol.symbol.kind,
        start_line: symbol.symbol.start_line,
        end_line: symbol.symbol.end_line,
        signature: symbol.symbol.signature.as_deref().and_then(one_line),
    }
}

fn one_line(signature: &str) -> Option<String> {
    let line = signature
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with('@') && !line.starts_with("#["))?;
    let mut output = line.chars().take(SIGNATURE_MAX_CHARS).collect::<String>();
    if line.chars().count() > SIGNATURE_MAX_CHARS {
        output.push_str("...");
    }
    Some(output)
}

fn written_call(call: &StoredCallSite) -> String {
    if let Some(rest) = call
        .qualifier
        .as_deref()
        .and_then(|qualifier| qualifier.strip_prefix(GO_SHADOWED_QUALIFIER))
    {
        // A shadowed Go call (E17-46): as written, `name` or `s.name`.
        return match rest.strip_prefix('.') {
            Some(receiver) => format!("{receiver}.{}", call.callee_name),
            None => call.callee_name.clone(),
        };
    }
    match (call.form, call.qualifier.as_deref()) {
        (CallForm::Plain | CallForm::Reference, _) => call.callee_name.clone(),
        (CallForm::SelfRef, Some("Self")) => format!("Self::{}", call.callee_name),
        (CallForm::SelfRef, Some(owner)) => format!("{owner}.{}", call.callee_name),
        (CallForm::Path, Some(qualifier)) if call.from_path.ends_with(".rs") => {
            format!("{qualifier}::{}", call.callee_name)
        }
        (CallForm::Path, Some(qualifier)) => format!("{qualifier}.{}", call.callee_name),
        _ => format!(".{}", call.callee_name),
    }
}

#[derive(Clone, Debug)]
enum Resolution {
    Resolved(i64, CallResolution),
    /// Shared, so a cached ambiguous answer is cloned without its ids.
    Ambiguous(Rc<[i64]>),
    Unresolved(&'static str),
}

/// What one call shape resolves against, independent of the calling file:
/// either an answer that needs no file (`no_owner`, `external_path`,
/// `not_found`) or the candidates left after the form's filter, grouped by
/// path so the same-file and import tiers are lookups, not scans.
enum Filtered {
    Early(Resolution),
    Candidates {
        /// Candidate ids in path and line order.
        all: Vec<i64>,
        by_path: HashMap<String, Vec<i64>>,
        name_only: bool,
    },
}

/// The parts of a call that decide its candidates (the calling file's
/// language family, not the file itself).
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct ShapeKey {
    name: String,
    family: Option<u8>,
    form: CallForm,
    qualifier: Option<String>,
    caller_owner: Option<String>,
    /// Java and C# only: the caller's owner types, outermost first
    /// (`Outer::Inner`).
    caller_owners: Option<String>,
}

/// The parts of a call that decide its resolution: the shape plus the file.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct CallKey {
    from_path: String,
    shape: ShapeKey,
}

/// Resolves stored call sites against current symbols and import edges for
/// one trace. Symbols are loaded once per name and held by id; a call's
/// candidates are filtered once per call shape, and its resolution is
/// cached per calling file, so resolving many call sites costs about one
/// lookup each, however many symbols share the name.
struct Resolver<'a> {
    store: &'a Store,
    repo: RepoId,
    by_name: HashMap<String, Vec<i64>>,
    imports: Option<HashMap<String, HashSet<String>>>,
    /// Go only: local names of each file's external imports.
    external_names: Option<HashMap<String, HashSet<String>>>,
    /// Java and C# only: the simple names (last part) of each file's
    /// external imports.
    declared_external_names: Option<HashMap<String, HashSet<String>>>,
    /// The directories of indexed `.csproj` files.
    projects: Option<BTreeSet<String>>,
    /// Java and C# only: the packages or namespaces each file declares.
    file_packages: Option<HashMap<String, Vec<String>>>,
    /// C# `global using` records: (language, namespace) every file of the
    /// language imports.
    global_imports: Vec<(&'static str, String, String)>,
    /// External `global using` names with the directory they reach.
    global_external: Vec<(String, String)>,
    /// A file declared more packages than are read.
    packages_limited: bool,
    /// Every declared Java or C# package or namespace name.
    known_packages: HashSet<(&'static str, String)>,
    /// Java and C# only: per file, the packages or namespaces it imports
    /// whole (one record each, owner decision 2026-11-21).
    package_imports: Option<HashMap<String, HashSet<String>>>,
    no_names: HashSet<String>,
    symbols: HashMap<i64, IndexedSymbol>,
    shapes: HashMap<ShapeKey, Rc<Filtered>>,
    resolved: HashMap<CallKey, Resolution>,
}

impl<'a> Resolver<'a> {
    fn new(store: &'a Store, repo: RepoId) -> Self {
        Self {
            store,
            repo,
            by_name: HashMap::new(),
            imports: None,
            external_names: None,
            declared_external_names: None,
            projects: None,
            file_packages: None,
            packages_limited: false,
            global_imports: Vec::new(),
            global_external: Vec::new(),
            known_packages: HashSet::new(),
            package_imports: None,
            no_names: HashSet::new(),
            symbols: HashMap::new(),
            shapes: HashMap::new(),
            resolved: HashMap::new(),
        }
    }

    /// Ids of the repository's non-secret symbols named `name`, in path and
    /// line order; each symbol is loaded once and kept by id.
    fn named(&mut self, name: &str) -> Result<&[i64], AtlasError> {
        if !self.by_name.contains_key(name) {
            let mut ids = Vec::new();
            for symbol in self.store.indexed_symbols_named(self.repo, name)? {
                if is_secret_path(Path::new(&symbol.path)) {
                    continue;
                }
                ids.push(symbol.id);
                self.symbols.insert(symbol.id, symbol);
            }
            self.by_name.insert(name.to_owned(), ids);
        }
        Ok(self.by_name.get(name).map_or(&[], Vec::as_slice))
    }

    fn load_imports(&mut self) -> Result<(), AtlasError> {
        if self.imports.is_none() {
            let mut imports = HashMap::<String, HashSet<String>>::new();
            for edge in self.store.resolved_edges(self.repo)? {
                imports
                    .entry(edge.from_path)
                    .or_default()
                    .insert(edge.to_path);
            }
            self.imports = Some(imports);
        }
        Ok(())
    }

    fn symbol(&self, id: i64) -> Option<&IndexedSymbol> {
        self.symbols.get(&id)
    }

    fn remember(&mut self, symbol: &IndexedSymbol) {
        self.symbols.insert(symbol.id, symbol.clone());
    }

    fn resolve(&mut self, call: &StoredCallSite) -> Result<Resolution, AtlasError> {
        // A call never crosses languages: Rust, Python, Go, Java, C#, and the
        // script family (JavaScript, TypeScript, TSX) resolve only within
        // themselves.
        let shape = ShapeKey {
            name: call.callee_name.clone(),
            family: language_family(ParserRegistry::for_path(Path::new(&call.from_path))),
            form: call.form,
            qualifier: call.qualifier.clone(),
            caller_owner: owner_base(&call.caller_qualified_name),
            caller_owners: matches!(call_rules(&call.from_path), CallRules::DeclaredPackages(_))
                .then(|| {
                    call.caller_qualified_name
                        .rsplit_once("::")
                        .map(|(owners, _)| owners.to_owned())
                })
                .flatten(),
        };
        let key = CallKey {
            from_path: call.from_path.clone(),
            shape,
        };
        if let Some(resolution) = self.resolved.get(&key) {
            return Ok(resolution.clone());
        }
        let resolution = self.resolve_uncached(&key)?;
        self.resolved.insert(key, resolution.clone());
        Ok(resolution)
    }

    fn resolve_uncached(&mut self, key: &CallKey) -> Result<Resolution, AtlasError> {
        match call_rules(&key.from_path) {
            CallRules::GoPackages => return self.resolve_go(key),
            CallRules::DeclaredPackages(rules) => return self.resolve_declared(key, rules),
            CallRules::ByName => {}
        }
        let filtered = self.filtered(&key.shape)?;
        let (all, by_path, name_only) = match filtered.as_ref() {
            Filtered::Early(resolution) => return Ok(resolution.clone()),
            Filtered::Candidates {
                all,
                by_path,
                name_only,
            } => (all, by_path, *name_only),
        };
        let form = key.shape.form;
        let same_file = by_path.get(&key.from_path).map_or(&[][..], Vec::as_slice);
        if let Some(resolution) = tier(same_file, CallResolution::SameFile) {
            return Ok(silent_reference_ambiguity(form, resolution));
        }
        self.load_imports()?;
        let mut imported_paths = self
            .imports
            .as_ref()
            .and_then(|imports| imports.get(&key.from_path))
            .into_iter()
            .flatten()
            .filter(|path| by_path.contains_key(*path))
            .collect::<Vec<_>>();
        // Path order, as the candidates themselves are ordered.
        imported_paths.sort();
        let imported = imported_paths
            .into_iter()
            .flat_map(|path| by_path[path].iter().copied())
            .collect::<Vec<_>>();
        if let Some(resolution) = tier(&imported, CallResolution::Import) {
            return Ok(silent_reference_ambiguity(form, resolution));
        }
        if form == CallForm::Reference {
            // Most bare arguments are variables: a name counts as a function
            // reference only when one function of that name is in the
            // caller's file or a file it imports; anything else is silent.
            return Ok(Resolution::Unresolved("not_found"));
        }
        let rule = if name_only {
            CallResolution::NameOnly
        } else {
            CallResolution::UniqueName
        };
        Ok(tier(all, rule).unwrap_or(Resolution::Unresolved("not_found")))
    }

    /// Go's rules (D102). A package is a directory: plain, reference, and
    /// self calls reach only the caller's file and then its package (test
    /// files only from test files). A path call on a qualifier that names an
    /// external import is a library call; one that names a package directory
    /// reaches that package's exported names (never the caller's own
    /// directory), first in imported files; any other qualifier is a
    /// variable, and its call falls back to methods of that name anywhere,
    /// labelled `name_only`. Receiver calls reach methods by file, package,
    /// import, then `name_only`.
    fn resolve_go(&mut self, key: &CallKey) -> Result<Resolution, AtlasError> {
        let shape = &key.shape;
        // E17-46 (D111): the calling function binds the name (or the
        // receiver name) itself, so the call may reach a value, not the
        // package's declaration: reported, never followed.
        if shape.form == CallForm::Path
            && shape
                .qualifier
                .as_deref()
                .is_some_and(|qualifier| qualifier.starts_with(GO_SHADOWED_QUALIFIER))
        {
            return Ok(Resolution::Unresolved("unplaced"));
        }
        let from = key.from_path.as_str();
        let caller_directory = parent_directory(from);
        let caller_is_test = is_test_file(from);
        let ids = self.named(&shape.name)?.to_vec();
        let candidates = ids
            .iter()
            .filter_map(|id| self.symbols.get(id))
            .filter(|candidate| {
                shape.family.is_some()
                    && language_family(candidate.language) == shape.family
                    && (caller_is_test || !is_test_file(&candidate.path))
            })
            .cloned()
            .collect::<Vec<_>>();
        let pick = |predicate: &dyn Fn(&IndexedSymbol) -> bool| {
            candidates
                .iter()
                .filter(|candidate| predicate(candidate))
                .map(|candidate| candidate.id)
                .collect::<Vec<_>>()
        };
        let form = shape.form;
        let ids = match form {
            CallForm::Plain => pick(&|candidate| {
                matches!(
                    candidate.symbol.kind,
                    SymbolKind::Function | SymbolKind::Class | SymbolKind::Struct
                )
            }),
            CallForm::Reference => pick(&|candidate| candidate.symbol.kind == SymbolKind::Function),
            CallForm::SelfRef => {
                let Some(owner) = shape.caller_owner.as_deref() else {
                    return Ok(Resolution::Unresolved("no_owner"));
                };
                pick(&|candidate| {
                    candidate.symbol.kind == SymbolKind::Method
                        && owner_base(&candidate.symbol.qualified_name).as_deref() == Some(owner)
                })
            }
            CallForm::Receiver => {
                let methods = pick(&|candidate| candidate.symbol.kind == SymbolKind::Method);
                let local = go_tiers(&methods, &candidates, from, caller_directory, form);
                if !matches!(local, Resolution::Unresolved(_)) {
                    return Ok(local);
                }
                if let Some(resolution) = self.imported_tier(from, &methods, &candidates)? {
                    return Ok(resolution);
                }
                return Ok(tier(&methods, CallResolution::NameOnly)
                    .unwrap_or(Resolution::Unresolved("not_found")));
            }
            CallForm::Path => return self.resolve_go_path(key, &candidates),
        };
        Ok(go_tiers(&ids, &candidates, from, caller_directory, form))
    }

    /// The candidates among `ids` in files `from` imports, as one tier.
    fn imported_tier(
        &mut self,
        from: &str,
        ids: &[i64],
        candidates: &[IndexedSymbol],
    ) -> Result<Option<Resolution>, AtlasError> {
        self.load_imports()?;
        let imported_paths = self
            .imports
            .as_ref()
            .and_then(|imports| imports.get(from))
            .cloned()
            .unwrap_or_default();
        let imported = ids
            .iter()
            .copied()
            .filter(|id| {
                candidates
                    .iter()
                    .find(|candidate| candidate.id == *id)
                    .is_some_and(|candidate| imported_paths.contains(&candidate.path))
            })
            .collect::<Vec<_>>();
        Ok(tier(&imported, CallResolution::Import))
    }

    fn resolve_go_path(
        &mut self,
        key: &CallKey,
        candidates: &[IndexedSymbol],
    ) -> Result<Resolution, AtlasError> {
        let from = key.from_path.as_str();
        let caller_directory = parent_directory(from);
        let qualifier = key.shape.qualifier.as_deref().unwrap_or_default();
        self.load_imports()?;
        let imports_package = self
            .imports
            .as_ref()
            .and_then(|imports| imports.get(from))
            .is_some_and(|paths| paths.iter().any(|path| module_name(path) == qualifier));
        if !imports_package && self.go_external_names(from)?.contains(qualifier) {
            return Ok(Resolution::Unresolved("external_path"));
        }
        let package = candidates
            .iter()
            .filter(|candidate| {
                (matches!(
                    candidate.symbol.kind,
                    SymbolKind::Function | SymbolKind::Class | SymbolKind::Struct
                ) && parent_directory(&candidate.path) != caller_directory
                    && module_name(&candidate.path) == qualifier
                    && candidate
                        .symbol
                        .name
                        .chars()
                        .next()
                        .is_some_and(char::is_uppercase))
                    || owner_base(&candidate.symbol.qualified_name).as_deref() == Some(qualifier)
            })
            .map(|candidate| candidate.id)
            .collect::<Vec<_>>();
        if !package.is_empty() {
            if let Some(resolution) = self.imported_tier(from, &package, candidates)? {
                return Ok(resolution);
            }
            return Ok(tier(&package, CallResolution::UniqueName)
                .unwrap_or(Resolution::Unresolved("not_found")));
        }
        // A variable: a method of that name anywhere, always `name_only`.
        let methods = candidates
            .iter()
            .filter(|candidate| candidate.symbol.kind == SymbolKind::Method)
            .map(|candidate| candidate.id)
            .collect::<Vec<_>>();
        Ok(tier(&methods, CallResolution::NameOnly).unwrap_or(Resolution::Unresolved("not_found")))
    }

    /// The local names (`fmt`, `yaml` for `gopkg.in/yaml.v3`) of the imports
    /// `from` records as external, loaded once per trace.
    fn go_external_names(&mut self, from: &str) -> Result<&HashSet<String>, AtlasError> {
        if self.external_names.is_none() {
            let mut names = HashMap::<String, HashSet<String>>::new();
            for (path, import) in self.store.external_edges(self.repo)? {
                if package_is_directory(&path) {
                    names
                        .entry(path)
                        .or_default()
                        .insert(go_package_local_name(&import));
                }
            }
            self.external_names = Some(names);
        }
        Ok(self
            .external_names
            .as_ref()
            .and_then(|names| names.get(from))
            .unwrap_or(&self.no_names))
    }

    /// Java and C# rules (D105). Candidates are the same language's symbols
    /// of the call's name that the caller may reach: another file's
    /// `private` member never; a Java member without `public` only from its
    /// own package, and a `protected` one also from a subclass; a C# member
    /// without an access modifier only from its own (possibly partial)
    /// type; an interface member without one from anywhere (it is public);
    /// test code only from test code (a file under a `test`, `tests`,
    /// `__tests__`, or `testdata` directory). Tiers run in order and the
    /// first non-empty one answers: the caller's file (`same_file`), its
    /// package or namespace (`same_package`; in C# also an enclosing
    /// namespace), then files it imports (`import`, an import of the file or
    /// of its whole package). Unqualified and `this.` calls reach methods of
    /// the innermost own type (matched by its full owner path) that has the
    /// name, then of the supertypes its declaration names, level by level up
    /// the declared chain; unqualified calls also reach types (`new T()`)
    /// and, through imports, static methods. `super.` and `base.` calls
    /// reach only the supertype chain, never the caller's own type. A path
    /// call `Q.m()` reaches methods of type `Q` in scope, or of a
    /// package-qualified type (`unique_name`); a qualifier that is an
    /// external import is a library call (`external_path`); one that is a
    /// field or property of the caller's types is a value, and so is any
    /// other qualifier that is not a type: its call falls back to the only
    /// reachable method of that name anywhere, always `name_only`; a
    /// capitalized qualifier that names no repository type (`System.out`,
    /// `String`) is a library call. Receiver calls reach methods by file,
    /// package, import, then `name_only`. Several partial declarations of
    /// one type count as its first declaration; overloads stay ambiguous. A
    /// call these rules cannot place while repository methods of its name
    /// exist and the caller's supertype chain leaves the repository (or a
    /// repository type it names is out of scope) is `unplaced`: listed and
    /// counted against `complete`, never dropped silently.
    fn resolve_declared(
        &mut self,
        key: &CallKey,
        rules: DeclaredRules,
    ) -> Result<Resolution, AtlasError> {
        let shape = &key.shape;
        let from = key.from_path.as_str();
        let Some(family) = shape.family else {
            return Ok(Resolution::Unresolved("not_found"));
        };
        let owners = shape
            .caller_owners
            .as_deref()
            .map(|owners| owners.split("::").map(str::to_owned).collect::<Vec<_>>())
            .unwrap_or_default();
        // The caller's own types by full owner path, innermost first.
        let own_paths = (1..=owners.len())
            .rev()
            .map(|end| owners[..end].join("::"))
            .collect::<Vec<_>>();
        // Everything that needs the store is loaded first.
        let ids = self.named(&shape.name)?.to_vec();
        self.load_imports()?;
        self.load_file_packages()?;
        self.load_package_imports()?;
        let form = shape.form;
        let qualifier = shape.qualifier.as_deref().unwrap_or_default();
        let chain = match form {
            CallForm::Plain | CallForm::SelfRef | CallForm::Path => {
                self.supertype_chain(from, &owners, family, rules)?
            }
            _ => SupertypeChain::default(),
        };
        let interface_members = self.interface_members(&ids, family)?;
        let mut external = false;
        let mut type_segment_is_repository_type = None;
        let mut qualifier_is_member = false;
        if form == CallForm::Path {
            let names = self.declared_external_names(from)?.clone();
            let global = self
                .global_external
                .iter()
                .filter(|(_, scope)| in_scope(from, scope))
                .map(|(name, _)| name.as_str())
                .collect::<HashSet<_>>();
            // Any dotted prefix: `Collections`, `System.Console`.
            external = qualifier
                .match_indices('.')
                .map(|(dot, _)| &qualifier[..dot])
                .chain([qualifier])
                .any(|prefix| names.contains(prefix) || global.contains(prefix));
            let first = qualifier.split('.').next().unwrap_or(qualifier);
            let mut owners_seen = own_paths.clone();
            owners_seen.extend(chain.levels.iter().flatten().cloned());
            qualifier_is_member = self.is_member_of(first, &owners_seen, family)?;
            if let Some(segment) = qualifier
                .split('.')
                .find(|segment| segment.chars().next().is_some_and(char::is_uppercase))
            {
                type_segment_is_repository_type = Some(self.is_repository_type(segment, family)?);
            }
        }

        let caller_is_test = is_test_code(from);
        let packages = self.file_packages.as_ref().expect("loaded above");
        let known_packages = &self.known_packages;
        let caller_language =
            ParserRegistry::for_path(Path::new(from)).map_or("", FileLanguage::as_str);
        let scope = DeclaredScope {
            from,
            rules,
            from_packages: packages_of(packages, from),
            imports: self.imports.as_ref().and_then(|imports| imports.get(from)),
            package_imports: self
                .package_imports
                .as_ref()
                .and_then(|imported| imported.get(from)),
            packages,
            owners: &owners,
            interface_members: &interface_members,
            global_imports: &self.global_imports,
            language: ParserRegistry::for_path(Path::new(from)).map_or("", FileLanguage::as_str),
        };
        let named = ids
            .iter()
            .filter_map(|id| self.symbols.get(id))
            .filter(|candidate| language_family(candidate.language) == Some(family))
            .collect::<Vec<_>>();
        let any_method = named
            .iter()
            .any(|candidate| candidate.symbol.kind == SymbolKind::Method);
        let base = named
            .iter()
            .copied()
            .filter(|candidate| caller_is_test || !is_test_code(&candidate.path))
            .collect::<Vec<_>>();
        let candidates = base
            .iter()
            .copied()
            .filter(|candidate| scope.reachable(candidate, false))
            .collect::<Vec<_>>();
        let is_method = |candidate: &&IndexedSymbol| candidate.symbol.kind == SymbolKind::Method;
        let owner_path = |candidate: &IndexedSymbol| {
            candidate
                .symbol
                .qualified_name
                .rsplit_once("::")
                .map(|(owner, _)| owner.to_owned())
        };
        let owned_by = |candidate: &&IndexedSymbol, owner: &str| {
            candidate.symbol.kind == SymbolKind::Method
                && owner_path(candidate).as_deref() == Some(owner)
        };
        // Inherited: the nearest level of the declared chain that has the
        // name, reachable as a subclass reaches its bases.
        let inherited = || {
            for level in &chain.levels {
                let found = base
                    .iter()
                    .copied()
                    .filter(|candidate| {
                        level.iter().any(|owner| owned_by(candidate, owner))
                            && scope.reachable(candidate, true)
                    })
                    .collect::<Vec<_>>();
                if !found.is_empty() {
                    return scope.located(&found);
                }
            }
            None
        };
        let unplaced = |placed: Option<Resolution>| match placed {
            Some(resolution) => resolution,
            None if any_method && !chain.complete => Resolution::Unresolved("unplaced"),
            None => Resolution::Unresolved("not_found"),
        };
        let resolution = match (form, qualifier) {
            (CallForm::SelfRef, "super" | "base") => unplaced(inherited()),
            (CallForm::Plain | CallForm::SelfRef, _) => {
                // The innermost own type that declares the name.
                let own = own_paths
                    .iter()
                    .map(|owner| {
                        candidates
                            .iter()
                            .copied()
                            .filter(|candidate| owned_by(candidate, owner))
                            .collect::<Vec<_>>()
                    })
                    .find(|methods| !methods.is_empty())
                    .unwrap_or_default();
                let mut local = own;
                if form == CallForm::Plain {
                    local.extend(candidates.iter().copied().filter(|candidate| {
                        matches!(
                            candidate.symbol.kind,
                            SymbolKind::Class | SymbolKind::Struct
                        )
                    }));
                }
                unplaced(
                    scope
                        .tiers(&local, form == CallForm::Plain)
                        .or_else(inherited)
                        .or_else(|| {
                            if form != CallForm::Plain {
                                return None;
                            }
                            // A statically imported method.
                            let statics = candidates
                                .iter()
                                .copied()
                                .filter(|candidate| {
                                    is_method(candidate)
                                        && modifiers(candidate.symbol.signature.as_deref())
                                            .contains(&"static")
                                        && scope.imported(candidate)
                                })
                                .collect::<Vec<_>>();
                            declared_tier(&statics, CallResolution::Import)
                        }),
                )
            }
            (CallForm::Path, _) => {
                if external {
                    return Ok(Resolution::Unresolved("external_path"));
                }
                // A package-qualified type (`com.acme.util.Strings`): the
                // longest declared package prefix names the package, the
                // rest the type; stored owner paths never hold the package.
                let package_split = qualifier
                    .match_indices('.')
                    .map(|(dot, _)| dot)
                    .rev()
                    .find(|dot| {
                        known_packages.contains(&(caller_language, qualifier[..*dot].to_owned()))
                    })
                    .map(|dot| (&qualifier[..dot], &qualifier[dot + 1..]));
                let type_path = package_split
                    .map_or(qualifier, |(_, rest)| rest)
                    .replace('.', "::");
                // A package-qualified type matches only that type of that
                // package; any other qualifier may name a nested type.
                let names_type = |candidate: &&IndexedSymbol| {
                    candidate.symbol.kind == SymbolKind::Method
                        && match package_split {
                            Some((package, _)) => {
                                owner_path(candidate).as_deref() == Some(type_path.as_str())
                                    && packages_of(packages, &candidate.path).contains(&package)
                            }
                            None => owner_path(candidate).is_some_and(|owner| {
                                owner == type_path || owner.ends_with(&format!("::{type_path}"))
                            }),
                        }
                };
                let typed = candidates
                    .iter()
                    .copied()
                    .filter(names_type)
                    .collect::<Vec<_>>();
                // Methods of a reachable repository type outside every tier.
                let any_type_method = !typed.is_empty();
                // A type whose methods are private or test code only is out
                // of reach by rule, not unplaced.
                let test_type_method = named.iter().any(names_type);
                let variable = || {
                    declared_tier(
                        &candidates
                            .iter()
                            .copied()
                            .filter(is_method)
                            .collect::<Vec<_>>(),
                        CallResolution::NameOnly,
                    )
                };
                if let Some(resolution) = scope.tiers(&typed, true) {
                    resolution
                } else if let Some((package, _)) = package_split
                    && let Some(resolution) = declared_tier(
                        &typed
                            .iter()
                            .copied()
                            .filter(|candidate| {
                                packages_of(packages, &candidate.path).contains(&package)
                            })
                            .collect::<Vec<_>>(),
                        CallResolution::UniqueName,
                    )
                {
                    // A package-qualified type: `com.acme.Strings.trim()`.
                    resolution
                } else if qualifier_is_member {
                    // A field or property of the caller's types: a value.
                    variable().unwrap_or(Resolution::Unresolved("not_found"))
                } else if any_type_method {
                    // A repository type the caller cannot see from here.
                    Resolution::Unresolved("unplaced")
                } else if test_type_method {
                    Resolution::Unresolved("not_found")
                } else if type_segment_is_repository_type == Some(false) {
                    // `System.out`, `String`, `java.util.Objects`: a type
                    // that is not the repository's, unless it may be a member
                    // inherited from outside the repository.
                    if any_method && !chain.complete {
                        Resolution::Unresolved("unplaced")
                    } else {
                        Resolution::Unresolved("external_path")
                    }
                } else {
                    // A variable or field: a method of that name anywhere.
                    variable().unwrap_or(Resolution::Unresolved("not_found"))
                }
            }
            (CallForm::Receiver, _) => {
                let methods = candidates
                    .iter()
                    .copied()
                    .filter(is_method)
                    .collect::<Vec<_>>();
                scope
                    .tiers(&methods, true)
                    .or_else(|| declared_tier(&methods, CallResolution::NameOnly))
                    .unwrap_or(Resolution::Unresolved("not_found"))
            }
            (CallForm::Reference, _) => Resolution::Unresolved("not_found"),
        };
        Ok(resolution)
    }

    /// The ids among `ids` of members declared in an interface (no access
    /// modifier needed: they are public), each owner looked up once.
    fn interface_members(&mut self, ids: &[i64], family: u8) -> Result<HashSet<i64>, AtlasError> {
        let mut owners = Vec::new();
        for id in ids {
            let Some(candidate) = self.symbols.get(id) else {
                continue;
            };
            if language_family(candidate.language) != Some(family) {
                continue;
            }
            // Only a member without an access modifier needs its owner's kind.
            let has_access = modifiers(candidate.symbol.signature.as_deref())
                .iter()
                .any(|word| {
                    matches!(
                        *word,
                        "public" | "private" | "protected" | "internal" | "file"
                    )
                });
            if has_access {
                continue;
            }
            if let Some((owner, _)) = candidate.symbol.qualified_name.rsplit_once("::") {
                owners.push((*id, candidate.path.clone(), owner.to_owned()));
            }
        }
        let mut members = HashSet::new();
        let mut known = HashMap::<(String, String), bool>::new();
        for (id, path, owner) in owners {
            let key = (path, owner);
            let is_interface = if let Some(found) = known.get(&key) {
                *found
            } else {
                let simple = key.1.rsplit("::").next().unwrap_or(&key.1).to_owned();
                let found = self.named(&simple)?.to_vec().iter().any(|owner_id| {
                    self.symbols.get(owner_id).is_some_and(|symbol| {
                        symbol.path == key.0
                            && symbol.symbol.qualified_name == key.1
                            && symbol.symbol.kind == SymbolKind::Interface
                    })
                });
                known.insert(key.clone(), found);
                found
            };
            if is_interface {
                members.insert(id);
            }
        }
        Ok(members)
    }

    /// Whether `name` is a field or property (`const`) of one of `owners`.
    fn is_member_of(
        &mut self,
        name: &str,
        owners: &[String],
        family: u8,
    ) -> Result<bool, AtlasError> {
        if owners.is_empty() {
            return Ok(false);
        }
        let ids = self.named(name)?.to_vec();
        Ok(ids
            .iter()
            .filter_map(|id| self.symbols.get(id))
            .any(|symbol| {
                language_family(symbol.language) == Some(family)
                    && symbol.symbol.kind == SymbolKind::Const
                    && symbol
                        .symbol
                        .qualified_name
                        .rsplit_once("::")
                        .is_some_and(|(owner, _)| owners.iter().any(|known| known == owner))
            }))
    }

    /// The packages or namespaces of every Java and C# file, once per trace.
    fn load_file_packages(&mut self) -> Result<(), AtlasError> {
        if self.file_packages.is_none() {
            let mut packages = HashMap::<String, Vec<String>>::new();
            for (path, name) in self
                .store
                .module_declarations(self.repo, &declaring_languages())?
            {
                // A hostile file declaring many namespaces keeps the first
                // ones, so package checks stay bounded (review S5).
                let language = ParserRegistry::for_path(Path::new(&path));
                let names = packages.entry(path).or_default();
                if names.len() < MAX_PACKAGES_PER_FILE {
                    if let Some(language) = language {
                        self.known_packages
                            .insert((language.as_str(), name.clone()));
                    }
                    names.push(name);
                } else {
                    self.packages_limited = true;
                }
            }
            self.file_packages = Some(packages);
        }
        Ok(())
    }

    /// The whole packages and namespaces each Java or C# file imports, one
    /// stored record each (`a.b` for `a.b.*`, `N` for `using N;`), once per
    /// trace.
    fn load_package_imports(&mut self) -> Result<(), AtlasError> {
        if self.package_imports.is_none() {
            self.load_projects()?;
            let mut capped = HashMap::<String, HashSet<String>>::new();
            for (path, raw) in self.store.unresolved_edge_targets_with_detail(
                self.repo,
                compresso_store::EDGE_DETAIL_PACKAGE,
            )? {
                if matches!(call_rules(&path), CallRules::DeclaredPackages(_)) {
                    // A `global using` reaches the files of its project.
                    if let Some(name) = raw.strip_prefix(GLOBAL_IMPORT_PREFIX) {
                        if let Some(language) = ParserRegistry::for_path(Path::new(&path)) {
                            let scope =
                                global_using_scope(&path, self.projects.as_ref().expect("loaded"));
                            self.global_imports
                                .push((language.as_str(), name.to_owned(), scope));
                        }
                        continue;
                    }
                    let name = raw.strip_suffix(".*").unwrap_or(&raw).to_owned();
                    capped.entry(path).or_default().insert(name);
                }
            }
            self.package_imports = Some(capped);
        }
        Ok(())
    }

    /// The directories of indexed `.csproj` files, once per trace.
    fn load_projects(&mut self) -> Result<(), AtlasError> {
        if self.projects.is_none() {
            self.projects = Some(project_directories(
                self.store
                    .indexed_files(self.repo)?
                    .into_iter()
                    .map(|file| file.path),
            ));
        }
        Ok(())
    }

    /// The names of the imports `from` records as external: whole, and for
    /// a Java type import by its last part (`java.util.Collections` and
    /// `Collections`; `System` for `using System;`), loaded once per trace.
    fn declared_external_names(&mut self, from: &str) -> Result<&HashSet<String>, AtlasError> {
        if self.declared_external_names.is_none() {
            self.load_projects()?;
            let mut names = HashMap::<String, HashSet<String>>::new();
            for (path, import) in self.store.external_edges(self.repo)? {
                if !matches!(call_rules(&path), CallRules::DeclaredPackages(_))
                    || import.ends_with(".*")
                {
                    continue;
                }
                // A Java import names a type, known by its last part; a C#
                // `using` names a namespace, known only whole (an alias or
                // `using static` type is already rewritten or capitalized).
                let names_a_type =
                    ParserRegistry::for_path(Path::new(&path)).is_some_and(|language| {
                        crate::parsers::languages::spec(language).imports == ImportStyle::Java
                    });
                // A `global using` of an external namespace holds for the
                // files of its project.
                if let Some(name) = import.strip_prefix(GLOBAL_IMPORT_PREFIX) {
                    let scope = global_using_scope(&path, self.projects.as_ref().expect("loaded"));
                    self.global_external.push((name.to_owned(), scope));
                    continue;
                }
                let entry = names.entry(path).or_default();
                if names_a_type {
                    entry.insert(import.rsplit('.').next().unwrap_or(&import).to_owned());
                }
                entry.insert(import);
            }
            self.declared_external_names = Some(names);
        }
        Ok(self
            .declared_external_names
            .as_ref()
            .and_then(|names| names.get(from))
            .unwrap_or(&self.no_names))
    }

    /// Whether `name` is a type of the repository in this call family.
    fn is_repository_type(&mut self, name: &str, family: u8) -> Result<bool, AtlasError> {
        let ids = self.named(name)?.to_vec();
        Ok(ids
            .iter()
            .filter_map(|id| self.symbols.get(id))
            .any(|candidate| {
                language_family(candidate.language) == Some(family)
                    && matches!(
                        candidate.symbol.kind,
                        SymbolKind::Class
                            | SymbolKind::Struct
                            | SymbolKind::Interface
                            | SymbolKind::Enum
                            | SymbolKind::Type
                    )
            }))
    }

    /// The declared supertype chain of the caller's innermost type, level
    /// by level (review M2): the types its declarations in the caller's
    /// file (and, for a partial C# type, its other declarations) extend or
    /// implement, then theirs, as repository types by qualified name. The
    /// chain is complete only when every named supertype is a repository
    /// type whose own list was read whole, within
    /// [`MAX_SUPERTYPE_LEVELS`] levels and [`MAX_SUPERTYPES`] types.
    fn supertype_chain(
        &mut self,
        from: &str,
        owners: &[String],
        family: u8,
        rules: DeclaredRules,
    ) -> Result<SupertypeChain, AtlasError> {
        let mut chain = SupertypeChain::default();
        let Some(innermost) = owners.last() else {
            return Ok(chain);
        };
        let own = owners.join("::");
        let mut names = Vec::new();
        for id in self.named(innermost)?.to_vec() {
            let Some(candidate) = self.symbols.get(&id) else {
                continue;
            };
            let same_place = candidate.path == from
                || (rules.partial_types
                    && modifiers(candidate.symbol.signature.as_deref()).contains(&"partial"));
            if language_family(candidate.language) == Some(family)
                && candidate.symbol.qualified_name == own
                && same_place
                && is_type_kind(candidate.symbol.kind)
            {
                chain.note_signature(candidate.symbol.signature.as_deref(), &mut names);
            }
        }
        let mut visited = HashSet::from([own]);
        for _ in 0..MAX_SUPERTYPE_LEVELS {
            if names.is_empty() {
                return Ok(chain);
            }
            let mut level = Vec::new();
            let mut next = Vec::new();
            for name in std::mem::take(&mut names) {
                let types = self
                    .named(&name)?
                    .to_vec()
                    .into_iter()
                    .filter_map(|id| self.symbols.get(&id))
                    .filter(|symbol| {
                        language_family(symbol.language) == Some(family)
                            && is_type_kind(symbol.symbol.kind)
                    })
                    .map(|symbol| {
                        (
                            symbol.symbol.qualified_name.clone(),
                            symbol.symbol.signature.clone(),
                        )
                    })
                    .collect::<Vec<_>>();
                if types.is_empty() {
                    // A library type: what it declares is unknown.
                    chain.complete = false;
                }
                for (qualified, signature) in types {
                    if visited.len() == MAX_SUPERTYPES {
                        chain.complete = false;
                        break;
                    }
                    if visited.insert(qualified.clone()) {
                        level.push(qualified);
                        chain.note_signature(signature.as_deref(), &mut next);
                    }
                }
            }
            if !level.is_empty() {
                chain.levels.push(level);
            }
            names = next;
        }
        if !names.is_empty() {
            chain.complete = false;
        }
        Ok(chain)
    }

    /// The candidates a call shape may reach, computed once per shape.
    fn filtered(&mut self, shape: &ShapeKey) -> Result<Rc<Filtered>, AtlasError> {
        if let Some(filtered) = self.shapes.get(shape) {
            return Ok(Rc::clone(filtered));
        }
        let ids = self.named(&shape.name)?.to_vec();
        let candidates = ids
            .iter()
            .filter_map(|id| self.symbols.get(id))
            .filter(|candidate| {
                shape.family.is_some() && language_family(candidate.language) == shape.family
            })
            .collect::<Vec<_>>();
        let filtered = Rc::new(filter_shape(shape, &candidates));
        self.shapes.insert(shape.clone(), Rc::clone(&filtered));
        Ok(filtered)
    }
}

/// Applies a call form's candidate rule to the same-family symbols of its
/// name (path and line ordered).
fn filter_shape(shape: &ShapeKey, candidates: &[&IndexedSymbol]) -> Filtered {
    let rust = shape.family == Some(0);
    let keep = |predicate: &dyn Fn(&IndexedSymbol) -> bool| {
        candidates
            .iter()
            .copied()
            .filter(|candidate| predicate(candidate))
            .collect::<Vec<_>>()
    };
    let is_method = |candidate: &IndexedSymbol| candidate.symbol.kind == SymbolKind::Method;
    let mut name_only = shape.form == CallForm::Receiver;
    let filtered = match shape.form {
        CallForm::Plain => keep(&|candidate| {
            matches!(
                candidate.symbol.kind,
                SymbolKind::Function | SymbolKind::Class | SymbolKind::Struct
            )
        }),
        CallForm::SelfRef => {
            let Some(owner) = shape.caller_owner.as_deref() else {
                return Filtered::Early(Resolution::Unresolved("no_owner"));
            };
            keep(&|candidate| {
                candidate.symbol.kind == SymbolKind::Method
                    && owner_base(&candidate.symbol.qualified_name).as_deref() == Some(owner)
            })
        }
        CallForm::Path => {
            let qualifier = shape.qualifier.as_deref().unwrap_or_default();
            let last = qualifier
                .rsplit("::")
                .next()
                .and_then(|segment| segment.rsplit('.').next())
                .unwrap_or(qualifier);
            let filtered = if matches!(last, "crate" | "super" | "self") {
                keep(&|candidate| candidate.symbol.kind == SymbolKind::Function)
            } else {
                let module = last.replace('-', "_");
                keep(&|candidate| {
                    owner_base(&candidate.symbol.qualified_name).as_deref() == Some(last)
                        || (matches!(
                            candidate.symbol.kind,
                            SymbolKind::Function | SymbolKind::Class | SymbolKind::Struct
                        ) && module_name(&candidate.path) == module)
                })
            };
            if filtered.is_empty() {
                if rust {
                    return Filtered::Early(Resolution::Unresolved("external_path"));
                }
                // A Python or script variable, not a module or type.
                name_only = true;
                keep(&is_method)
            } else {
                filtered
            }
        }
        CallForm::Receiver => keep(&is_method),
        CallForm::Reference => keep(&|candidate| candidate.symbol.kind == SymbolKind::Function),
    };
    if filtered.is_empty() {
        return Filtered::Early(Resolution::Unresolved("not_found"));
    }
    let mut by_path = HashMap::<String, Vec<i64>>::new();
    for candidate in &filtered {
        by_path
            .entry(candidate.path.clone())
            .or_default()
            .push(candidate.id);
    }
    Filtered::Candidates {
        all: filtered.iter().map(|candidate| candidate.id).collect(),
        by_path,
        name_only,
    }
}

fn silent_reference_ambiguity(form: CallForm, resolution: Resolution) -> Resolution {
    match resolution {
        Resolution::Ambiguous(_) if form == CallForm::Reference => {
            Resolution::Unresolved("not_found")
        }
        resolution => resolution,
    }
}

fn language_family(language: Option<FileLanguage>) -> Option<u8> {
    crate::parsers::languages::spec(language?).call_family
}

/// Same file, then same package; nothing else is in scope.
fn go_tiers(
    ids: &[i64],
    candidates: &[IndexedSymbol],
    from: &str,
    caller_directory: &str,
    form: CallForm,
) -> Resolution {
    let path_of = |id: &i64| {
        candidates
            .iter()
            .find(|candidate| candidate.id == *id)
            .map(|candidate| candidate.path.as_str())
    };
    let same_file = ids
        .iter()
        .copied()
        .filter(|id| path_of(id) == Some(from))
        .collect::<Vec<_>>();
    if let Some(resolution) = tier(&same_file, CallResolution::SameFile) {
        return silent_reference_ambiguity(form, resolution);
    }
    let same_package = ids
        .iter()
        .copied()
        .filter(|id| {
            path_of(id)
                .is_some_and(|path| path != from && parent_directory(path) == caller_directory)
        })
        .collect::<Vec<_>>();
    if let Some(resolution) = tier(&same_package, CallResolution::SamePackage) {
        return silent_reference_ambiguity(form, resolution);
    }
    Resolution::Unresolved("not_found")
}

fn tier(ids: &[i64], rule: CallResolution) -> Option<Resolution> {
    match ids {
        [] => None,
        [id] => Some(Resolution::Resolved(*id, rule)),
        _ => Some(Resolution::Ambiguous(Rc::from(ids))),
    }
}

/// Most packages or namespaces of one file that package checks read.
const MAX_PACKAGES_PER_FILE: usize = 64;
/// Most levels of a declared supertype chain followed (D105).
const MAX_SUPERTYPE_LEVELS: usize = 8;
/// Most types a declared supertype chain holds, the caller's own included.
const MAX_SUPERTYPES: usize = 64;

/// The supertypes of a Java or C# caller's type, nearest level first.
struct SupertypeChain {
    levels: Vec<Vec<String>>,
    /// Every named supertype was a repository type read whole, within the
    /// bounds.
    complete: bool,
}

impl Default for SupertypeChain {
    fn default() -> Self {
        Self {
            levels: Vec::new(),
            complete: true,
        }
    }
}

impl SupertypeChain {
    /// Adds the supertypes a declaration's signature names to `names`; a
    /// signature cut at its bound may have named more.
    fn note_signature(&mut self, signature: Option<&str>, names: &mut Vec<String>) {
        if signature.is_some_and(|text| text.ends_with('…')) {
            self.complete = false;
        }
        for name in declared_supertypes(signature) {
            if !names.contains(&name) {
                names.push(name);
            }
        }
    }
}

const fn is_type_kind(kind: SymbolKind) -> bool {
    matches!(
        kind,
        SymbolKind::Class | SymbolKind::Struct | SymbolKind::Interface | SymbolKind::Enum
    )
}

/// What a Java or C# caller can see: its file, packages or namespaces,
/// imported files, and own types (D105).
struct DeclaredScope<'a> {
    from: &'a str,
    rules: DeclaredRules,
    from_packages: Vec<&'a str>,
    imports: Option<&'a HashSet<String>>,
    /// Packages or namespaces the caller imports whole.
    package_imports: Option<&'a HashSet<String>>,
    packages: &'a HashMap<String, Vec<String>>,
    /// The caller's owner types, outermost first.
    owners: &'a [String],
    /// Candidates declared in an interface: public without a modifier.
    interface_members: &'a HashSet<i64>,
    /// Namespaces every file of a language imports (`global using`).
    global_imports: &'a [(&'static str, String, String)],
    /// The caller's language name.
    language: &'static str,
}

impl DeclaredScope<'_> {
    /// Whether `candidate` lives in the caller's package (Java) or in its
    /// namespace or an enclosing one (C#); files declaring none share the
    /// default package or global namespace, which C# sees from everywhere.
    fn same_package(&self, candidate: &IndexedSymbol) -> bool {
        packages_of(self.packages, &candidate.path)
            .iter()
            .any(|theirs| {
                self.from_packages.iter().any(|ours| {
                    ours == theirs
                        || (self.rules.enclosing_namespaces
                            && (theirs.is_empty()
                                || ours
                                    .strip_prefix(*theirs)
                                    .is_some_and(|rest| rest.starts_with('.'))))
                })
            })
    }

    /// A file the caller imports: an edge to it, or a package or
    /// namespace it declares that the caller imports whole.
    fn imported(&self, candidate: &IndexedSymbol) -> bool {
        self.imports
            .is_some_and(|imports| imports.contains(&candidate.path))
            || self.global_imports.iter().any(|(language, name, scope)| {
                *language == self.language
                    && in_scope(self.from, scope)
                    && packages_of(self.packages, &candidate.path)
                        .iter()
                        .any(|package| {
                            !package.is_empty() && {
                                // `global using N;`, or `global using static N.T;`
                                // naming the file `T.cs`.
                                let stem =
                                    candidate.path.rsplit('/').next().unwrap_or(&candidate.path);
                                let stem = stem.split('.').next().unwrap_or(stem);
                                name == package || *name == format!("{package}.{stem}")
                            }
                        })
            })
            || self.package_imports.is_some_and(|capped| {
                packages_of(self.packages, &candidate.path)
                    .iter()
                    .any(|package| !package.is_empty() && capped.contains(*package))
            })
    }

    /// Whether the caller may reach `candidate` by its access modifiers;
    /// `via_inheritance` when the caller's type inherits it (a Java
    /// `protected` member is reachable from a subclass anywhere).
    fn reachable(&self, candidate: &IndexedSymbol, via_inheritance: bool) -> bool {
        if candidate.path == self.from {
            return true;
        }
        let modifiers = modifiers(candidate.symbol.signature.as_deref());
        let access = modifiers.iter().copied().find(|word| {
            matches!(
                *word,
                "public" | "private" | "protected" | "internal" | "file"
            )
        });
        if access.is_none() && self.interface_members.contains(&candidate.id) {
            return true;
        }
        if !self.rules.members_default_private {
            return match access {
                Some("private") => false,
                Some("public") => true,
                Some("protected") => via_inheritance || self.same_package(candidate),
                // Package-private: the same package.
                _ => self.same_package(candidate),
            };
        }
        let same_type = || {
            owner_base(&candidate.symbol.qualified_name)
                .is_some_and(|owner| self.owners.contains(&owner))
                && self.same_package(candidate)
        };
        match access {
            Some("public" | "internal" | "protected") => true,
            Some("file") => false,
            Some("private") => same_type(),
            // A member without a modifier is private; a top-level type is
            // internal.
            _ if candidate.symbol.qualified_name.contains("::") => same_type(),
            _ => true,
        }
    }

    /// One tier of candidates found by a known relation (inheritance),
    /// each labelled by where it lives: the caller's file, package, an
    /// imported file, or elsewhere (`unique_name`).
    fn located(&self, candidates: &[&IndexedSymbol]) -> Option<Resolution> {
        let label = |candidate: &IndexedSymbol| {
            if candidate.path == self.from {
                CallResolution::SameFile
            } else if self.same_package(candidate) {
                CallResolution::SamePackage
            } else if self.imported(candidate) {
                CallResolution::Import
            } else {
                CallResolution::UniqueName
            }
        };
        match declared_tier(candidates, CallResolution::UniqueName)? {
            Resolution::Resolved(id, _) => {
                let chosen = candidates.iter().find(|candidate| candidate.id == id)?;
                Some(Resolution::Resolved(id, label(chosen)))
            }
            other => Some(other),
        }
    }

    /// Same file, then same package, then (when `imports`) imported files.
    fn tiers(&self, candidates: &[&IndexedSymbol], imports: bool) -> Option<Resolution> {
        let pick = |predicate: &dyn Fn(&IndexedSymbol) -> bool| {
            candidates
                .iter()
                .copied()
                .filter(|candidate| predicate(candidate))
                .collect::<Vec<_>>()
        };
        declared_tier(
            &pick(&|candidate| candidate.path == self.from),
            CallResolution::SameFile,
        )
        .or_else(|| {
            declared_tier(
                &pick(&|candidate| candidate.path != self.from && self.same_package(candidate)),
                CallResolution::SamePackage,
            )
        })
        .or_else(|| {
            if !imports {
                return None;
            }
            declared_tier(
                &pick(&|candidate| candidate.path != self.from && self.imported(candidate)),
                CallResolution::Import,
            )
        })
    }
}

/// One tier of Java or C# candidates. Partial declarations of one type
/// count as one: the first, in path and line order.
fn declared_tier(candidates: &[&IndexedSymbol], rule: CallResolution) -> Option<Resolution> {
    let first = candidates.first()?;
    let one_partial_type = candidates.len() > 1
        && candidates.iter().all(|candidate| {
            candidate.symbol.qualified_name == first.symbol.qualified_name
                && candidate.symbol.kind == first.symbol.kind
                && matches!(
                    candidate.symbol.kind,
                    SymbolKind::Class | SymbolKind::Struct | SymbolKind::Interface
                )
                && modifiers(candidate.symbol.signature.as_deref()).contains(&"partial")
        });
    if one_partial_type {
        return Some(Resolution::Resolved(first.id, rule));
    }
    let ids = candidates
        .iter()
        .map(|candidate| candidate.id)
        .collect::<Vec<_>>();
    tier(&ids, rule)
}

fn packages_of<'a>(packages: &'a HashMap<String, Vec<String>>, path: &str) -> Vec<&'a str> {
    match packages.get(path) {
        Some(names) if !names.is_empty() => names.iter().map(String::as_str).collect(),
        _ => vec![""],
    }
}

/// A Java or C# file under a test directory (D99's rule), or a file name a
/// language reserves for tests.
fn is_test_code(path: &str) -> bool {
    is_test_file(path)
        || path
            .rsplit('/')
            .skip(1)
            .any(|part| matches!(part, "test" | "tests" | "__tests__" | "testdata"))
}

/// The simple names of the types a Java or C# type declaration extends or
/// implements: Java `extends A implements B, C`, C# `: A, IB` (before any
/// `where`); type arguments, record parameters, attributes, and namespaces
/// are dropped.
fn declared_supertypes(signature: Option<&str>) -> Vec<String> {
    let signature = signature.unwrap_or_default();
    // Without generic arguments or parenthesized groups.
    let mut flat = String::with_capacity(signature.len());
    let mut depth = 0_usize;
    for character in signature.chars() {
        match character {
            '<' | '(' | '[' => depth += 1,
            '>' | ')' | ']' => depth = depth.saturating_sub(1),
            _ if depth == 0 => flat.push(character),
            _ => {}
        }
    }
    let list = if let Some((_, after)) = flat.split_once(':') {
        after
            .split(" where ")
            .next()
            .unwrap_or(after)
            .replace(',', " ")
    } else {
        let words = flat
            .split(|character: char| character.is_whitespace() || character == ',')
            .collect::<Vec<_>>();
        let mut names = Vec::new();
        let mut collecting = false;
        for word in words {
            match word {
                "extends" | "implements" => collecting = true,
                "permits" => collecting = false,
                "" => {}
                name if collecting => names.push(name),
                _ => {}
            }
        }
        names.join(" ")
    };
    list.split_whitespace()
        .map(|name| name.rsplit('.').next().unwrap_or(name))
        .filter(|name| {
            !name.is_empty()
                && name
                    .chars()
                    .all(|character| character == '_' || character.is_alphanumeric())
        })
        .map(str::to_owned)
        .collect()
}

/// One resolved call from `caller` to `callee`.
#[derive(Clone, Debug)]
struct Link {
    caller: i64,
    callee: i64,
    site: TraceCallSite,
    rule: CallResolution,
    reference: bool,
}

enum ResultKind {
    NoPath,
    Other,
}

struct Walk {
    max_depth: u8,
    options: TraceOptions,
    visited: usize,
    depth_reached: u8,
    truncated: bool,
    depth_limited: bool,
    incomplete: bool,
    total: usize,
    unfollowed: Vec<UnfollowedCall>,
    unfollowed_total: usize,
    /// Call-site rows this trace may still read.
    rows_left: usize,
}

impl Walk {
    fn new(max_depth: u8, options: TraceOptions, call_site_rows: usize) -> Self {
        Self {
            rows_left: call_site_rows,
            max_depth,
            options,
            visited: 1,
            depth_reached: 0,
            truncated: false,
            depth_limited: false,
            incomplete: false,
            total: 0,
            unfollowed: Vec::new(),
            unfollowed_total: 0,
        }
    }

    fn report(self, result: TraceResult, max_depth: u8) -> TraceReport {
        let result_kind = if matches!(result, TraceResult::NoPath) {
            ResultKind::NoPath
        } else {
            ResultKind::Other
        };
        let mut unfollowed = self.unfollowed;
        // Ambiguous calls first: they are the ones that may hide an answer.
        unfollowed.sort_by_key(|call| {
            (
                call.status != "ambiguous",
                call.call_site.path.clone(),
                call.call_site.line,
            )
        });
        unfollowed.truncate(TRACE_UNRESOLVED_LISTED);
        TraceReport {
            result,
            total: self.total,
            nodes_visited: self.visited,
            depth_reached: self.depth_reached,
            max_depth,
            truncated: self.truncated,
            depth_limited: self.depth_limited,
            complete: !(self.truncated
                || self.incomplete
                || (self.depth_limited && matches!(result_kind, ResultKind::NoPath))),
            unfollowed,
            unfollowed_total: self.unfollowed_total,
        }
    }

    fn note_unfollowed(
        &mut self,
        resolver: &Resolver<'_>,
        call: &StoredCallSite,
        resolution: &Resolution,
    ) {
        let (status, candidates) = match resolution {
            Resolution::Resolved(..) => return,
            Resolution::Ambiguous(ids) => (
                "ambiguous",
                ids.iter()
                    .take(TRACE_CANDIDATES_LISTED)
                    .filter_map(|id| resolver.symbol(*id))
                    .map(|symbol| format!("{}:{}", symbol.path, symbol.symbol.qualified_name))
                    .collect(),
            ),
            Resolution::Unresolved(reason) => (*reason, Vec::new()),
        };
        // A name with no repository symbol, or a Rust path naming no
        // repository type or module, is a library call: it cannot lead to a
        // repository symbol, so it is neither reported nor incomplete.
        if matches!(status, "not_found" | "external_path") {
            return;
        }
        self.incomplete = true;
        self.unfollowed_total += 1;
        // Bounded memory: only a prefix is kept, then sorted and cut again.
        if self.unfollowed.len() < TRACE_UNRESOLVED_LISTED * 8 {
            self.unfollowed.push(UnfollowedCall {
                caller: call.caller_qualified_name.clone(),
                callee: written_call(call),
                call_site: TraceCallSite {
                    path: call.from_path.clone(),
                    line: call.line,
                },
                status,
                candidates,
            });
        }
    }

    /// Charges `rows` (fetched with a limit one above the budget left)
    /// against the trace's row budget, cutting and marking the trace
    /// truncated when they exceed it.
    fn charge_rows(&mut self, mut rows: Vec<StoredCallSite>) -> Vec<StoredCallSite> {
        if rows.len() > self.rows_left {
            rows.truncate(self.rows_left);
            self.truncated = true;
        }
        self.rows_left -= rows.len();
        rows
    }

    /// Resolved calls out of `callers`, one link per caller and callee.
    fn links_from(
        &mut self,
        resolver: &mut Resolver<'_>,
        callers: &[i64],
    ) -> Result<Vec<Link>, AtlasError> {
        let calls = resolver.store.call_sites_from(
            resolver.repo,
            callers,
            self.rows_left.saturating_add(1),
        )?;
        let calls = self.charge_rows(calls);
        let mut links = Vec::new();
        let mut seen = HashSet::new();
        for call in calls {
            if is_secret_path(Path::new(&call.from_path)) {
                continue;
            }
            let resolution = resolver.resolve(&call)?;
            let Resolution::Resolved(callee, rule) = resolution else {
                self.note_unfollowed(resolver, &call, &resolution);
                continue;
            };
            if seen.insert((call.caller_symbol_id, callee)) {
                links.push(Link {
                    caller: call.caller_symbol_id,
                    callee,
                    site: TraceCallSite {
                        path: call.from_path,
                        line: call.line,
                    },
                    rule,
                    reference: call.form == CallForm::Reference,
                });
            }
        }
        links.sort_by(|left, right| {
            (left.caller, &left.site.path, left.site.line, left.callee).cmp(&(
                right.caller,
                &right.site.path,
                right.site.line,
                right.callee,
            ))
        });
        Ok(links)
    }

    /// Resolved calls into `callees`, one link per caller and callee.
    fn links_to(
        &mut self,
        resolver: &mut Resolver<'_>,
        callees: &[i64],
    ) -> Result<Vec<Link>, AtlasError> {
        let wanted = callees.iter().copied().collect::<HashSet<_>>();
        let names = callees
            .iter()
            .filter_map(|id| resolver.symbol(*id))
            .map(|symbol| symbol.symbol.name.clone())
            .collect::<BTreeSet<_>>();
        let mut links = Vec::new();
        let mut seen = HashSet::new();
        for name in names {
            let calls = resolver.store.call_sites_named(
                resolver.repo,
                &name,
                self.rows_left.saturating_add(1),
            )?;
            for call in self.charge_rows(calls) {
                if is_secret_path(Path::new(&call.from_path)) {
                    continue;
                }
                let resolution = resolver.resolve(&call)?;
                match &resolution {
                    Resolution::Resolved(callee, rule) if wanted.contains(callee) => {
                        if seen.insert((call.caller_symbol_id, *callee)) {
                            links.push(Link {
                                caller: call.caller_symbol_id,
                                callee: *callee,
                                site: TraceCallSite {
                                    path: call.from_path.clone(),
                                    line: call.line,
                                },
                                rule: *rule,
                                reference: call.form == CallForm::Reference,
                            });
                        }
                    }
                    Resolution::Resolved(..) => {}
                    Resolution::Ambiguous(ids) if ids.iter().any(|id| wanted.contains(id)) => {
                        self.note_unfollowed(resolver, &call, &resolution);
                    }
                    Resolution::Ambiguous(_) => {}
                    Resolution::Unresolved(_) => {
                        self.note_unfollowed(resolver, &call, &resolution);
                    }
                }
            }
        }
        links.sort_by(|left, right| {
            (&left.site.path, left.site.line, left.caller, left.callee).cmp(&(
                &right.site.path,
                right.site.line,
                right.caller,
                right.callee,
            ))
        });
        Ok(links)
    }

    fn load(&self, resolver: &mut Resolver<'_>, ids: &[i64]) -> Result<(), AtlasError> {
        let missing = ids
            .iter()
            .copied()
            .filter(|id| resolver.symbol(*id).is_none())
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            for symbol in resolver.store.symbols_by_ids(resolver.repo, &missing)? {
                resolver.remember(&symbol);
            }
        }
        Ok(())
    }

    fn callees(
        &mut self,
        resolver: &mut Resolver<'_>,
        from: &IndexedSymbol,
    ) -> Result<TraceResult, AtlasError> {
        resolver.remember(from);
        self.neighbors(resolver, from, true)
            .map(TraceResult::Callees)
    }

    fn callers(
        &mut self,
        resolver: &mut Resolver<'_>,
        to: &IndexedSymbol,
    ) -> Result<TraceResult, AtlasError> {
        resolver.remember(to);
        self.neighbors(resolver, to, false)
            .map(TraceResult::Callers)
    }

    fn neighbors(
        &mut self,
        resolver: &mut Resolver<'_>,
        start: &IndexedSymbol,
        forward: bool,
    ) -> Result<Vec<TraceNeighbor>, AtlasError> {
        let mut seen = HashSet::from([start.id]);
        let mut frontier = vec![start.id];
        let mut entries = Vec::new();
        for depth in 1..=self.max_depth {
            if frontier.is_empty() {
                break;
            }
            let links = if forward {
                self.links_from(resolver, &frontier)?
            } else {
                // Callers are found by the frontier's names.
                self.load(resolver, &frontier)?;
                self.links_to(resolver, &frontier)?
            };
            let mut next = Vec::new();
            for link in links {
                let (near, far) = if forward {
                    (link.caller, link.callee)
                } else {
                    (link.callee, link.caller)
                };
                let cycle = far == start.id;
                if !cycle && !seen.insert(far) {
                    continue;
                }
                if !cycle {
                    if self.visited >= self.options.max_nodes {
                        self.truncated = true;
                        break;
                    }
                    self.visited += 1;
                    next.push(far);
                }
                self.total += 1;
                self.depth_reached = depth;
                entries.push((depth, near, far, link, cycle));
            }
            if !next.is_empty() && depth == self.max_depth && self.options.max_depth.is_some() {
                // Symbols at an explicit depth limit were found but not
                // expanded. At the default depth the caller asked for direct
                // neighbours only, so that is the whole answer.
                self.depth_limited = true;
            }
            frontier = next;
            if self.truncated && self.visited >= self.options.max_nodes {
                break;
            }
        }
        if entries.len() > self.options.max_results {
            self.truncated = true;
            entries.truncate(self.options.max_results);
        }
        let ids = entries
            .iter()
            .flat_map(|(_, near, far, _, _)| [*near, *far])
            .collect::<Vec<_>>();
        self.load(resolver, &ids)?;
        Ok(entries
            .into_iter()
            .filter_map(|(depth, near, far, link, cycle)| {
                let symbol = resolver.symbol(far)?;
                let via = resolver.symbol(near)?;
                Some(TraceNeighbor {
                    depth,
                    symbol: trace_symbol(symbol),
                    via: via.symbol.qualified_name.clone(),
                    call_site: link.site,
                    resolution: link.rule,
                    reference: link.reference,
                    cycle,
                })
            })
            .collect())
    }

    fn paths(
        &mut self,
        resolver: &mut Resolver<'_>,
        from: &IndexedSymbol,
        to: &IndexedSymbol,
    ) -> Result<TraceResult, AtlasError> {
        resolver.remember(from);
        resolver.remember(to);
        let mut depth_of = HashMap::from([(from.id, 0_u8)]);
        let mut predecessors = BTreeMap::<i64, Vec<Link>>::new();
        let mut goal = Vec::<Link>::new();
        let mut frontier = vec![from.id];
        for depth in 1..=self.max_depth {
            if frontier.is_empty() {
                break;
            }
            self.depth_reached = depth;
            let mut next = Vec::new();
            for link in self.links_from(resolver, &frontier)? {
                if link.callee == to.id {
                    goal.push(link);
                    continue;
                }
                match depth_of.get(&link.callee) {
                    None => {
                        if self.visited >= self.options.max_nodes {
                            self.truncated = true;
                            continue;
                        }
                        self.visited += 1;
                        depth_of.insert(link.callee, depth);
                        next.push(link.callee);
                        predecessors.entry(link.callee).or_default().push(link);
                    }
                    Some(existing) if *existing == depth => {
                        predecessors.entry(link.callee).or_default().push(link);
                    }
                    Some(_) => {}
                }
            }
            if !goal.is_empty() {
                break;
            }
            frontier = next;
            if depth == self.max_depth && !frontier.is_empty() {
                self.depth_limited = true;
            }
        }
        if goal.is_empty() {
            return Ok(TraceResult::NoPath);
        }

        // Every shortest path, enumerated backwards in deterministic order.
        let mut chains = Vec::<Vec<Link>>::new();
        let mut found = 0_usize;
        // Path enumeration is bounded too: shortest-path DAGs can hold
        // exponentially many paths.
        let mut steps = 0_usize;
        'goals: for last in goal {
            let mut stack = vec![(last.caller, vec![last])];
            while let Some((node, chain)) = stack.pop() {
                steps += 1;
                if steps > TRACE_PATH_ENUMERATION_STEPS {
                    self.truncated = true;
                    break 'goals;
                }
                if node == from.id {
                    found += 1;
                    if chains.len() < self.options.max_paths {
                        let mut chain = chain;
                        chain.reverse();
                        chains.push(chain);
                    }
                    continue;
                }
                for link in predecessors.get(&node).into_iter().flatten().rev() {
                    let mut extended = chain.clone();
                    extended.push(link.clone());
                    stack.push((link.caller, extended));
                }
            }
        }
        self.total = found;
        if found > chains.len() {
            self.truncated = true;
        }
        let ids = chains
            .iter()
            .flatten()
            .flat_map(|link| [link.caller, link.callee])
            .collect::<Vec<_>>();
        self.load(resolver, &ids)?;
        let paths = chains
            .into_iter()
            .map(|chain| {
                let mut hops = vec![TraceHop {
                    symbol: trace_symbol(from),
                    call_site: None,
                    resolution: None,
                    reference: false,
                }];
                for link in chain {
                    if let Some(symbol) = resolver.symbol(link.callee) {
                        hops.push(TraceHop {
                            symbol: trace_symbol(symbol),
                            reference: link.reference,
                            call_site: Some(link.site),
                            resolution: Some(link.rule),
                        });
                    }
                }
                hops
            })
            .collect();
        Ok(TraceResult::Paths(paths))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_call_site_row_budget_truncates_a_trace_deterministically() {
        use compresso_core::{TokenProvider, counter_for};

        let temp = tempfile::tempdir().expect("temporary directory");
        let root = temp.path().join("repo");
        std::fs::create_dir_all(&root).expect("root");
        let mut source = String::from("def handle():\n    pass\n");
        for caller in 0..30 {
            source.push_str(&format!("def c{caller:02}():\n    handle()\n"));
        }
        std::fs::write(root.join("app.py"), source).expect("module");
        let store = Store::open(&temp.path().join("compresso.db")).expect("store");
        crate::Indexer::new(&store, counter_for(TokenProvider::Heuristic))
            .index(&root, |_| {})
            .expect("index");
        let repo = store
            .repo_by_root(&std::fs::canonicalize(&root).expect("canonical"))
            .expect("lookup")
            .expect("repository")
            .id;
        let run = |rows| {
            trace_with_row_budget(
                &store,
                repo,
                None,
                Some("handle"),
                TraceOptions::default(),
                rows,
            )
            .expect("trace")
        };

        let full = run(TRACE_MAX_CALL_SITE_ROWS);
        assert!(full.complete && !full.truncated);
        assert_eq!(full.total, 30);

        let cut = run(10);
        assert!(cut.truncated && !cut.complete);
        let TraceResult::Callers(callers) = &cut.result else {
            panic!("expected callers, got {:?}", cut.result);
        };
        let names = callers
            .iter()
            .map(|caller| caller.symbol.qualified_name.clone())
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            (0..10).map(|i| format!("c{i:02}")).collect::<Vec<_>>()
        );
        assert_eq!(run(10), cut);

        // A budget the rows exactly fill is not a cut.
        assert!(!run(30).truncated);
    }

    /// E17-40 (D105): `super.`/`base.` calls reach the types a declaration
    /// extends or implements, read from its signature.
    #[test]
    fn test_declared_supertypes_read_java_and_csharp_signatures() {
        assert_eq!(
            declared_supertypes(Some(
                "@Entity(name = \"a:b\")\npublic class JobStore<T extends Job> extends a.BaseStore<T> implements Store, java.io.Closeable"
            )),
            ["BaseStore", "Store", "Closeable"]
        );
        assert_eq!(
            declared_supertypes(Some("public sealed interface Shape permits Circle, Square")),
            Vec::<String>::new()
        );
        assert_eq!(
            declared_supertypes(Some("record Point(int x) implements Comparable<Point>")),
            ["Comparable"]
        );
        assert_eq!(
            declared_supertypes(Some(
                "[Obsolete(\"a:b\")] public partial class JobStore<T> : Acme.StoreBase, IStore where T : Job"
            )),
            ["StoreBase", "IStore"]
        );
        assert_eq!(
            declared_supertypes(Some("public record Entry(string Key) : Base(Key)")),
            ["Base"]
        );
        assert!(declared_supertypes(Some("class Plain")).is_empty());
    }

    #[test]
    fn test_split_path_ignores_double_colons() {
        assert_eq!(split_path("Store::open"), (None, "Store::open"));
        assert_eq!(
            split_path("src/lib.rs:Store::open"),
            (Some("src/lib.rs"), "Store::open")
        );
        assert_eq!(split_path("a.py:run"), (Some("a.py"), "run"));
    }

    #[test]
    fn test_owner_base_and_module_name() {
        assert_eq!(owner_base("Store<T>::open").as_deref(), Some("Store"));
        assert_eq!(owner_base("run"), None);
        assert_eq!(
            module_name("rust/compresso-store/src/lib.rs"),
            "compresso_store"
        );
        assert_eq!(module_name("pkg/util/__init__.py"), "util");
        assert_eq!(module_name("src/parsers/mod.rs"), "parsers");
        assert_eq!(module_name("web/api.ts"), "api");
        assert_eq!(module_name("internal/store/db.go"), "store");
        // A root-level Go file names no package: `errors.New()` never
        // reaches a root `errors.go`.
        assert_eq!(module_name("main.go"), "");
        assert_eq!(module_name("errors.go"), "");
    }

    #[test]
    fn test_one_line_signature_skips_attributes_and_caps_length() {
        assert_eq!(
            one_line("#[inline]\npub fn run(\n    x: u8,\n)").as_deref(),
            Some("pub fn run(")
        );
        let long = format!("fn {}()", "a".repeat(300));
        assert!(
            one_line(&long).is_some_and(|line| line.chars().count() == SIGNATURE_MAX_CHARS + 3)
        );
    }
}
