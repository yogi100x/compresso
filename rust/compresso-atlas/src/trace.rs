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

use compresso_store::{CallForm, IndexedSymbol, RepoId, Store, StoredCallSite, SymbolKind};

use crate::AtlasError;
use crate::queries::QueryError;
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
    /// Symbols at `max_depth` were found but not expanded: deeper callers,
    /// callees, or a longer path may exist.
    pub depth_limited: bool,
    /// No bound was hit and no ambiguous or ownerless `self` call was met,
    /// so a missing caller or path is not hiding behind one.
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
    let current = store
        .latest_index_version(repo)?
        .is_some_and(|version| version.parser_version == crate::indexer::DEFAULT_PARSER_VERSION);
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
    let mut walk = Walk::new(max_depth, options);
    let result = match (from, to) {
        (Some(from), Some(to)) => walk.paths(&mut resolver, &from, &to)?,
        (Some(from), None) => walk.callees(&mut resolver, &from)?,
        (None, Some(to)) => walk.callers(&mut resolver, &to)?,
        (None, None) => unreachable!("validated: at least one end is named"),
    };
    Ok(walk.report(result, max_depth))
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
        .symbols_named(repo, name)?
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
fn owner_base(qualified: &str) -> Option<String> {
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

/// The module name a file provides: its stem, or its directory for
/// `mod.rs`, `__init__.py`, `index.*`, `lib.rs`, and `main.rs`, or the crate
/// directory for `src/lib.rs`. `-` reads as `_`.
fn module_name(path: &str) -> String {
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
    match (call.form, call.qualifier.as_deref()) {
        (CallForm::Plain, _) => call.callee_name.clone(),
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
    Ambiguous(Vec<i64>),
    Unresolved(&'static str),
}

/// Resolves stored call sites against current symbols and import edges,
/// caching name lookups and the import map for one trace.
struct Resolver<'a> {
    store: &'a Store,
    repo: RepoId,
    by_name: HashMap<String, Vec<IndexedSymbol>>,
    imports: Option<HashMap<String, HashSet<String>>>,
    symbols: HashMap<i64, IndexedSymbol>,
}

impl<'a> Resolver<'a> {
    fn new(store: &'a Store, repo: RepoId) -> Self {
        Self {
            store,
            repo,
            by_name: HashMap::new(),
            imports: None,
            symbols: HashMap::new(),
        }
    }

    fn named(&mut self, name: &str) -> Result<&[IndexedSymbol], AtlasError> {
        if !self.by_name.contains_key(name) {
            let found = self
                .store
                .symbols_named(self.repo, name)?
                .into_iter()
                .filter(|symbol| !is_secret_path(Path::new(&symbol.path)))
                .collect::<Vec<_>>();
            for symbol in &found {
                self.symbols.insert(symbol.id, symbol.clone());
            }
            self.by_name.insert(name.to_owned(), found);
        }
        Ok(self.by_name.get(name).map_or(&[], Vec::as_slice))
    }

    fn imports_of(&mut self, path: &str) -> Result<HashSet<String>, AtlasError> {
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
        Ok(self
            .imports
            .as_ref()
            .and_then(|imports| imports.get(path))
            .cloned()
            .unwrap_or_default())
    }

    fn symbol(&self, id: i64) -> Option<&IndexedSymbol> {
        self.symbols.get(&id)
    }

    fn remember(&mut self, symbol: &IndexedSymbol) {
        self.symbols.insert(symbol.id, symbol.clone());
    }

    fn resolve(&mut self, call: &StoredCallSite) -> Result<Resolution, AtlasError> {
        let rust = call.from_path.ends_with(".rs");
        let caller_owner = owner_base(&call.caller_qualified_name);
        let candidates = self.named(&call.callee_name)?.to_vec();
        let methods = |candidates: &[IndexedSymbol]| {
            candidates
                .iter()
                .filter(|candidate| candidate.symbol.kind == SymbolKind::Method)
                .cloned()
                .collect::<Vec<_>>()
        };
        let mut name_only = call.form == CallForm::Receiver;
        let filtered = match call.form {
            CallForm::Plain => candidates
                .iter()
                .filter(|candidate| {
                    matches!(
                        candidate.symbol.kind,
                        SymbolKind::Function | SymbolKind::Class | SymbolKind::Struct
                    )
                })
                .cloned()
                .collect::<Vec<_>>(),
            CallForm::SelfRef => {
                let Some(owner) = caller_owner else {
                    return Ok(Resolution::Unresolved("no_owner"));
                };
                candidates
                    .iter()
                    .filter(|candidate| {
                        candidate.symbol.kind == SymbolKind::Method
                            && owner_base(&candidate.symbol.qualified_name).as_deref()
                                == Some(owner.as_str())
                    })
                    .cloned()
                    .collect()
            }
            CallForm::Path => {
                let qualifier = call.qualifier.as_deref().unwrap_or_default();
                let last = qualifier
                    .rsplit("::")
                    .next()
                    .and_then(|segment| segment.rsplit('.').next())
                    .unwrap_or(qualifier);
                let filtered = if matches!(last, "crate" | "super" | "self") {
                    candidates
                        .iter()
                        .filter(|candidate| candidate.symbol.kind == SymbolKind::Function)
                        .cloned()
                        .collect::<Vec<_>>()
                } else {
                    candidates
                        .iter()
                        .filter(|candidate| {
                            owner_base(&candidate.symbol.qualified_name).as_deref() == Some(last)
                                || (matches!(
                                    candidate.symbol.kind,
                                    SymbolKind::Function | SymbolKind::Class | SymbolKind::Struct
                                ) && module_name(&candidate.path) == last.replace('-', "_"))
                        })
                        .cloned()
                        .collect()
                };
                if filtered.is_empty() {
                    if rust {
                        return Ok(Resolution::Unresolved("external_path"));
                    }
                    // A Python or script variable, not a module or type.
                    name_only = true;
                    methods(&candidates)
                } else {
                    filtered
                }
            }
            CallForm::Receiver => methods(&candidates),
        };
        if filtered.is_empty() {
            return Ok(Resolution::Unresolved("not_found"));
        }

        let same_file = filtered
            .iter()
            .filter(|candidate| candidate.path == call.from_path)
            .map(|candidate| candidate.id)
            .collect::<Vec<_>>();
        if let Some(resolution) = tier(&same_file, CallResolution::SameFile) {
            return Ok(resolution);
        }
        let imports = self.imports_of(&call.from_path)?;
        let imported = filtered
            .iter()
            .filter(|candidate| imports.contains(&candidate.path))
            .map(|candidate| candidate.id)
            .collect::<Vec<_>>();
        if let Some(resolution) = tier(&imported, CallResolution::Import) {
            return Ok(resolution);
        }
        let all = filtered
            .iter()
            .map(|candidate| candidate.id)
            .collect::<Vec<_>>();
        let rule = if name_only {
            CallResolution::NameOnly
        } else {
            CallResolution::UniqueName
        };
        Ok(tier(&all, rule).unwrap_or(Resolution::Unresolved("not_found")))
    }
}

fn tier(ids: &[i64], rule: CallResolution) -> Option<Resolution> {
    match ids {
        [] => None,
        [id] => Some(Resolution::Resolved(*id, rule)),
        _ => Some(Resolution::Ambiguous(ids.to_vec())),
    }
}

/// One resolved call from `caller` to `callee`.
#[derive(Clone, Debug)]
struct Link {
    caller: i64,
    callee: i64,
    site: TraceCallSite,
    rule: CallResolution,
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
}

impl Walk {
    fn new(max_depth: u8, options: TraceOptions) -> Self {
        Self {
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

    /// Resolved calls out of `callers`, one link per caller and callee.
    fn links_from(
        &mut self,
        resolver: &mut Resolver<'_>,
        callers: &[i64],
    ) -> Result<Vec<Link>, AtlasError> {
        let calls = resolver.store.call_sites_from(resolver.repo, callers)?;
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
            for call in resolver.store.call_sites_named(resolver.repo, &name)? {
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
            if !next.is_empty() && depth == self.max_depth {
                // Symbols at the depth limit were found but not expanded.
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
                }];
                for link in chain {
                    if let Some(symbol) = resolver.symbol(link.callee) {
                        hops.push(TraceHop {
                            symbol: trace_symbol(symbol),
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
