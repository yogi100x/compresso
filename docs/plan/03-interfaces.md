# 03 — Rust Module Interfaces, Ownership Boundaries, API Contracts

Signatures below are the contract between tickets. Implementers may add private helpers freely; changing a public signature requires updating this doc + dependent tickets.

## 1. Crate ownership boundaries

| Crate | Owner | Public surface | May depend on |
|-------|-------|----------------|----------------|
| compresso-core | R1 | graph, tokens, strategy, engine, safety, trace, config | (external only) |
| compresso-store | R2 | Store, migrations, DAOs | core (types only) |
| compresso-atlas | R2 | Scanner, ParserRegistry, Indexer, queries | store, core::tokens |
| compresso-mcp | R2 | serve_stdio(), ToolRouter | core, atlas, store |
| compresso-integrations | D1 | Integration trait + adapters | (external only) |
| compresso-cli | D1 | bin only, no lib | all above |
| compresso-eval | R1/PQ | FixtureSet, EvalRunner, Gates | core, atlas |
| compresso-model-protocol/providers/router | R1/R2 | model contracts, drivers, routing | explicit E13 graph |
| compresso-search/edit/recall/knowledge/review | R1/R2 | E15 local capabilities | explicit local graph |
| compresso-sandbox | I1 | OS isolation | OS APIs only |
| compresso-browser/terminal | R2 | sandboxed workflows | sandbox only |

## 2. compresso-core

### 2.1 `graph` module

```rust
pub struct ContextGraph { pub schema_version: u32, pub nodes: Vec<ContextNode>, pub edges: Vec<ContextEdge> }
pub struct ContextNode {
    pub id: String,
    pub kind: NodeKind,
    pub content: String,
    pub source: Source,
    pub token_count: usize,
    pub token_provider: TokenProvider,
    pub priority: f32,          // 0.0..=1.0
    pub protected: bool,
    pub recency: Option<u64>,
    pub metadata: serde_json::Map<String, serde_json::Value>,
    pub provider_turn: Option<ProviderTurn>,
}
pub struct ProviderTurn {
    pub provider: ProviderId,
    pub turn_id: ProviderTurnId,
    pub tool_call_id: ToolCallId,
    pub opaque_state_hash: Option<String>,
}
pub struct ProviderId(/* non-empty string */);
pub struct ProviderTurnId(/* non-empty string */);
pub struct ToolCallId(/* non-empty string */);
pub enum NodeKind { SystemInstruction, UserInstruction, AssistantResponse, AssistantReasoning, ToolDefinition, ToolRequest, ToolResponse, SourceFile, CodeSymbol, SearchResult, CompilerOutput, TestOutput, RepoMap, Summary, Decision, MemoryFact }
pub enum Source { Client, Repository { path: String }, Tool { name: String }, Synthetic }
pub struct ContextEdge { pub from: String, pub to: String, pub kind: EdgeKind, pub weight: f32 }
pub enum EdgeKind { Imports, Calls, Defines, References, Tests, Supersedes, ProducedByTool, RespondsTo, DependsOn, SameErrorGroup, DuplicateOf }

impl ContextGraph {
    pub fn validate(&self) -> Result<(), GraphError>;      // ids, edges, protection, token recount, provider turns, duplicate refs
    pub fn total_tokens(&self) -> usize;
    pub fn from_text(content: &str, kind: NodeKind, counter: &dyn TokenCounter) -> Self; // single-node convenience
    pub fn recount_changed(&mut self, changed: &BTreeSet<String>, counter: &dyn TokenCounter);
    pub fn validate_token_counts(&self, counter: &dyn TokenCounter) -> Result<(), GraphError>;
}

// E2-13 generic-text boundary. Only a single-node, edge-free, non-provider
// graph is segmented; existing conversation/repository graphs remain intact.
pub fn segment_graph(input: &ContextGraph) -> Result<ContextGraph, SegmentError>;
```

### 2.2 `tokens` module

```rust
pub enum TokenProvider { OpenAiO200k, OpenAiCl100k, AnthropicCalibrated, Heuristic }
pub trait TokenCounter: Send + Sync {
    fn provider(&self) -> TokenProvider;
    fn count(&self, text: &str) -> usize;
}
pub fn counter_for(provider: TokenProvider) -> &'static dyn TokenCounter; // eagerly initialized during process startup singletons
// AnthropicCalibrated = O200k count × CALIBRATION. Until the calibration ticket (E2-02a) lands measured values in
// fixtures/provider-payloads/anthropic-counts/anthropic-counts.json, CALIBRATION = 1.15 (documented interim default, ADR-0017);
// the ±10% accuracy assertion only activates once measured counts are committed.
```

### 2.3 `strategy` module

```rust
pub struct Estimate { pub tokens_saved: usize, pub confidence: f32 }
pub struct StrategyOutput { pub graph: ContextGraph, pub notes: Vec<String>, pub removals: Vec<RemovalRecord> }
pub struct RemovalRecord {
    pub strategy_id: String, pub node_id: String, pub kind: RemovalKind,
    pub locator: Option<String>, pub original_hash: String,
    pub replacement_hash: Option<String>, pub before_tokens: usize,
    pub after_tokens: usize, pub units_removed: Option<usize>,
}

pub trait CompressionStrategy: Send + Sync {
    fn id(&self) -> &'static str;                          // "dedup_exact", "collapse_logs", "compact_json", "pack_budget"
    fn supports(&self, input: &ContextGraph) -> bool;
    fn estimate(&self, input: &ContextGraph) -> Estimate;
    fn execute(&self, input: &ContextGraph, budget: &CompressionBudget) -> Result<StrategyOutput, StrategyError>;
    fn execute_if_supported(&self, input: &ContextGraph, budget: &CompressionBudget) -> StrategyExecution;
}

pub enum StrategyExecution {
    Legacy,                                               // default: engine preserves supports → estimate → execute behavior
    NotSupported,
    Executed { estimate: Estimate, output: Result<StrategyOutput, StrategyError> },
}

pub struct StrategyRegistry { /* ordered Vec<Box<dyn CompressionStrategy>> */ }
impl StrategyRegistry {
    pub fn safe_default(mode: Mode) -> Self;               // safe: [dedup_exact, collapse_logs, compact_json, pack_budget]; passthrough: []
    pub fn strategies(&self) -> &[Box<dyn CompressionStrategy>];
}
```

Slice strategies: `dedup_exact` creates a typed stub plus `DuplicateReference.locator` while preserving the existing edge set; `collapse_logs` and `compact_json` remove only enumerated units; `pack_budget` drops lowest `priority/token` unprotected atomic groups. It never partial-truncates code, provider turns, tool request/response pairs, or dedup referent groups. Every strategy returns changed node ids; the engine recounts those nodes and updates totals by delta. Strategies must not invoke whole-graph tokenization. The combined execution hook is backward-compatible: its default `Legacy` result retains the three public calls, while a costly strategy may override it to derive `supports`, the exact estimate, and the exact output from one immutable plan.

**Lossiness taxonomy (D39)** — safe mode is deterministic and rule-based, NOT lossless, and is never described as lossless in user-facing copy. Mapping enforced by `supports()`:

| `allowed_lossiness` | Strategies permitted |
|---|---|
| `None` | `dedup_exact` only (duplicates replaced by reference stubs naming the surviving node) |
| `Structural` (safe-mode default) | + `collapse_logs`, `compact_json`, `pack_budget` — removal/truncation of whole units (lines, values, nodes), each enumerated in the trace |
| `Summarizing` | + model-based summarization (aggressive mode, post-slice) |

Every removal or truncation appends a typed `RemovalRecord`. Notes are explanatory only. Records identify a non-overlapping node/locator, hashes, and before/after token counts. `removals_enumerated` recomputes affected spans and requires the sum of record deltas to equal the graph delta exactly (D51).

### 2.4 `engine` module

```rust
pub enum Mode { Safe, Balanced, Aggressive, Passthrough }  // slice implements Safe + Passthrough
pub struct CompressionBudget { pub max_tokens: usize, pub max_latency_ms: u64, pub allowed_lossiness: Lossiness, pub protected_kinds: Vec<NodeKind> }
pub enum Lossiness { None, Structural, Summarizing }
pub struct CompressionResult { pub output: ContextGraph, pub before_tokens: usize, pub after_tokens: usize, pub strategy_results: Vec<StrategyResult>, pub safety_checks: Vec<SafetyCheck>, pub fallback_used: bool, pub trace: Trace }

pub struct Engine { /* registry, counter, safety */ }
impl Engine {
    pub fn new(mode: Mode, counter: &'static dyn TokenCounter) -> Self;
    pub fn with_provider_validator(self, validator: Arc<dyn ProviderConversationValidator>) -> Self;
    pub fn compress(&self, input: ContextGraph, budget: &CompressionBudget) -> CompressionResult;
    // NEVER returns Err: any failure => fallback_used=true, output=input unchanged
}

pub trait ProviderConversationValidator: Send + Sync {
    fn validate_pre_compression(&self, graph: &ContextGraph) -> Result<(), ProviderConversationError>;
    fn validate_post_compression(&self, original: &ContextGraph, candidate: &ContextGraph) -> Result<(), ProviderConversationError>;
}
```

Pipeline: validate and recount ingress → mark protected/provider-atomic groups → run strategies with differential token accounting and remaining latency budget → safety-check → on failure restore the original graph. Non-empty input may never produce empty output. Provider-shaped input runs its profile conversation validator before serialization; failure is byte-identical passthrough.

### 2.5 `safety` module

```rust
pub struct SafetyCheck { pub name: String, pub passed: bool, pub detail: String }
// Compatibility wrapper: uses an empty removal ledger. Identity candidates may
// pass removals_enumerated; any non-identity graph delta fails that check.
pub fn run_all(original: &ContextGraph, candidate: &ContextGraph, budget: &CompressionBudget, elapsed_ms: u64) -> Vec<SafetyCheck>;
// Engine path: receives the cumulative typed ledger from applied strategies.
pub fn run_all_with_removals(original: &ContextGraph, candidate: &ContextGraph, budget: &CompressionBudget, elapsed_ms: u64, removals: &[RemovalRecord]) -> Vec<SafetyCheck>;
```

Checks: `protected_unchanged`, `no_inflation`, `json_still_valid`, `fences_balanced`, `refs_valid`, `provider_conversation_valid`, `minimum_output`, `token_counts_valid`, `latency_ok`, `schema_valid`, `removals_enumerated`. `refs_valid` covers file references and dedup referents; `minimum_output` requires a non-empty candidate for non-empty input; provider validation enforces complete turns/tool pairs and immutable reasoning; token validation recounts changed nodes and totals.

### 2.6 `config` module

```rust
pub struct Config { pub compression: CompressionCfg, pub index: IndexCfg, pub privacy: PrivacyCfg, pub mcp: McpCfg }  // mirrors build-doc §17 TOML
pub struct EffectiveValue<T> { pub value: T, pub source: ConfigSource }   // Default|File|Env|Flag
impl Config {
    pub fn load() -> Result<Config, ConfigError>;          // default < file < env < flags
    pub fn explain() -> Vec<(String, String, ConfigSource)>; // key, value, source — backs `compresso config explain`
}
```

## 3. compresso-store

```rust
pub struct Store { /* Arc<Mutex<rusqlite::Connection>>, WAL */ }
pub type RepoId = i64;
pub type FileId = i64;
pub struct RepoRecord { pub id: RepoId, pub root: PathBuf, pub approved_at: i64, pub parser_version: String, pub schema_version: u32, pub last_indexed_at: Option<i64> }
pub struct IndexVersionRecord { pub parser_version: String, pub schema_version: u32, pub created_at: i64 }
pub enum FileLanguage { Rust, TypeScript, Tsx, JavaScript, Python, Shell, Text }
pub struct FileRecord { pub path: String, pub hash: String, pub language: Option<FileLanguage>, pub size_bytes: u64, pub token_estimate: usize, pub indexed_at: i64 }
pub enum SymbolKind { Function, Method, Struct, Class, Enum, Trait, Interface, Const, Type, Module }
pub struct SymbolRecord { pub name: String, pub qualified_name: String, pub kind: SymbolKind, pub signature: Option<String>, pub start_line: u32, pub end_line: u32, pub token_estimate: usize }
pub enum EdgeKind { Imports, References, Tests }
pub enum EdgeResolution { Resolved, Ambiguous(Vec<String>), Unresolved(String) }
pub struct EdgeRecord { pub to_path: String, pub kind: EdgeKind, pub resolution: EdgeResolution }
pub struct ResolvedEdgeRecord { pub from_path: String, pub to_path: String, pub kind: EdgeKind }
pub struct SymbolHit { pub file: FileRecord, pub symbol: SymbolRecord, pub rank: f64 }
// D59: selector ownership stays in Store to avoid a Store -> Atlas dependency. Lookup returns all candidates.
pub struct SymbolSelector { pub path: String, pub qualified_name: String, pub kind: Option<SymbolKind>, pub signature_hash: Option<String>, pub start_line_hint: Option<u32> }
// Concurrency contract (D41, ADR-0004): Store is Send + Sync and cheaply cloneable (Arc). ONE process, ONE
// connection guarded by a Mutex: every method acquires the lock, so writes are serialized by construction and
// reads never observe partial transactions. busy_timeout = 5s guards against a second process (e.g. CLI while
// MCP serves). Async callers (rmcp handlers) wrap calls in tokio::task::spawn_blocking — Store itself exposes
// only blocking methods. If profiling ever shows lock contention, the escape hatch is a dedicated writer thread
// with a channel, behind the same API — callers never change.
impl Store {
    pub fn open(path: &Path) -> Result<Store, StoreError>;   // runs pending migrations transactionally
    pub fn open_default() -> Result<Store, StoreError>;      // platform data dir (01-stack §5)
    // repositories
    pub fn approve_repo(&self, canonical_root: &Path) -> Result<RepoId>;
    pub fn repo_by_root(&self, canonical_root: &Path) -> Result<Option<RepoRecord>>;
    pub fn repository(&self, repo: RepoId) -> Result<RepoRecord>;
    pub fn approved_repositories(&self) -> Result<Vec<RepoRecord>>; // canonical-root order
    pub fn prepare_repo_removal(&self, canonical_root: &Path) -> Result<Option<RepoRemovalPreview>>; // exact record + Store revision
    pub fn remove_repo_if_unchanged(&self, preview: &RepoRemovalPreview) -> Result<RepoRecord>; // fails closed after any intervening Store write
    pub fn remove_repo(&self, canonical_root: &Path) -> Result<Option<RepoRecord>>; // exact stored root; cascades index metadata, never source files
    // files & symbols (used only by atlas)
    pub fn upsert_file(&self, repo: RepoId, rec: &FileRecord) -> Result<FileId>;
    pub fn replace_indexed_file(&self, repo: RepoId, rec: &FileRecord, symbols: &[SymbolRecord], edges: &[EdgeRecord]) -> Result<FileId>; // one per-file transaction
    pub fn replace_indexed_file_with_calls(&self, repo: RepoId, rec: &FileRecord, symbols: &[SymbolRecord], edges: &[EdgeRecord], calls: &[CallSiteRecord]) -> Result<FileId>; // same transaction also replaces the file's call_sites (D89); replace_indexed_file == calls: &[]
    pub fn indexed_symbols_named(&self, repo: RepoId, name: &str) -> Result<Vec<IndexedSymbol>>; // call-graph resolution (D89/D90): every exact-name definition, path/line order; IndexedSymbol { id, path, language, symbol }
    pub fn symbols_named(&self, repo: RepoId, name: &str, limit: usize) -> Result<Vec<SymbolHit>>; // context planner (D88): bounded exact-name lookup; not the call-graph lookup above
    pub fn symbols_by_ids(&self, repo: RepoId, ids: &[i64]) -> Result<Vec<IndexedSymbol>>; // other repositories' ids skipped
    pub fn call_sites_from(&self, repo: RepoId, caller_ids: &[i64]) -> Result<Vec<StoredCallSite>>; // calls inside these callers
    pub fn call_sites_named(&self, repo: RepoId, callee_name: &str) -> Result<Vec<StoredCallSite>>; // every call naming callee_name
    pub fn call_site_count(&self, repo: RepoId) -> Result<usize>;
    pub fn file_hashes(&self, repo: RepoId) -> Result<HashMap<String, String>>;  // path -> blake3
    pub fn indexed_files(&self, repo: RepoId) -> Result<Vec<FileRecord>>; // deterministic path order
    pub fn indexed_file(&self, repo: RepoId, path: &str) -> Result<Option<FileRecord>>;
    pub fn replace_symbols(&self, file: FileId, symbols: &[SymbolRecord], edges: &[EdgeRecord]) -> Result<()>; // single transaction
    pub fn delete_files(&self, repo: RepoId, paths: &[String]) -> Result<()>;
    pub fn search_symbols(&self, repo: RepoId, query: &str, limit: usize) -> Result<Vec<SymbolHit>>; // FTS5, bm25-ranked; whole query is one literal contiguous phrase
    pub fn search_symbols_all_terms(&self, repo: RepoId, query: &str, limit: usize) -> Result<Vec<SymbolHit>>; // multi-word: every whitespace-separated term (any order/case, edge punctuation stripped, terms under 3 chars dropped); exact-phrase hits first, then all-terms hits, each bm25-ordered; one word == search_symbols
    pub fn search_symbols_excluding_language(&self, repo: RepoId, query: &str, limit: usize, excluded: FileLanguage) -> Result<Vec<SymbolHit>>; // exclusion occurs in SQL before bm25 limit
    pub fn symbols_in_file(&self, repo: RepoId, path: &str) -> Result<Vec<SymbolRecord>>;
    pub fn resolved_edges(&self, repo: RepoId) -> Result<Vec<ResolvedEdgeRecord>>; // deterministic, resolved rows only
    pub fn unresolved_edge_sources(&self, repo: RepoId) -> Result<Vec<String>>; // deterministic sources to reconsider when paths expand
    pub fn symbols_by_identity(&self, repo: RepoId, selector: &SymbolSelector) -> Result<Vec<(FileRecord, SymbolRecord)>>; // never arbitrary first
    pub fn latest_index_version(&self, repo: RepoId) -> Result<Option<IndexVersionRecord>>;
    pub fn stamp_index_version(&self, repo: RepoId, parser_version: &str, schema_version: u32) -> Result<()>;
    // sessions & operations
    pub fn start_session(&self, client: &str, transport: &str) -> Result<SessionId>;
    pub fn record_operation(&self, op: &OperationRecord) -> Result<()>;   // validates trace_json against schema
    pub fn stats(&self, since: Option<i64>) -> Result<StatsSummary>;
    // receipts
    pub fn save_receipt(&self, r: &ReceiptRecord) -> Result<()>;
    pub fn receipts(&self, client: Option<&str>) -> Result<Vec<ReceiptRecord>>;
    pub fn total_changes(&self) -> Result<u64>; // connection-local read-only acceptance proof
}
```

Boundary rule: only `compresso-store` contains SQL. Atlas/MCP/CLI call these methods.

## 4. compresso-atlas

```rust
// scanner.rs
pub struct ScanResult { pub added: Vec<PathBuf>, pub changed: Vec<PathBuf>, pub removed: Vec<String>, pub skipped: Vec<(PathBuf, SkipReason)> }
pub struct ScanOptions { pub max_file_bytes: u64 }
pub fn scan(store: &Store, repo: RepoId, root: &Path) -> Result<ScanResult, AtlasError>; // safe default: max_file_bytes = 1_000_000
pub fn scan_with_options(store: &Store, repo: RepoId, root: &Path, options: ScanOptions) -> Result<ScanResult, AtlasError>;
pub struct Concurrency { /* jobs clamped to 1..=available CPUs; memory_budget_bytes (default 64 MiB) */ } // Concurrency::default(), ::serial(), ::bounded(jobs, memory_budget_bytes)
pub fn scan_with_concurrency(store: &Store, repo: RepoId, root: &Path, options: ScanOptions, concurrency: Concurrency) -> Result<ScanResult, AtlasError>; // identical ScanResult at every concurrency; scan/scan_with_options use Concurrency::default()
// respects .gitignore + built-in exclusions (binaries, >max_file_bytes, node_modules/target/dist/.git, secret-file patterns: .env*, *.pem, id_rsa*)
// ignored and built-in excluded directories are reported once in skipped and pruned before descent; descendants are intentionally not enumerated

// parsers.rs
pub type Language = compresso_store::FileLanguage;        // Store owns the canonical persisted language vocabulary
pub struct ParserRegistry;
impl ParserRegistry {
    pub fn for_path(path: &Path) -> Option<Language>;      // code: rs, ts, tsx, js, jsx, mjs, cjs, py; shell: sh, bash, zsh; inert text: md, markdown, txt, rst, adoc
    pub fn extract(lang: Language, source: &str) -> Result<Extraction, ParseError>;
}
pub enum ImportKind { Use, Import, ExportFrom, Require, FromImport }
pub struct ImportRecord { pub raw: String, pub kind: ImportKind, pub resolved_path: Option<String>, pub resolution: ImportResolution }
pub enum ImportResolution { Resolved, Ambiguous(Vec<String>), Unresolved(String) }
pub struct Extraction { pub symbols: Vec<SymbolRecord>, pub imports: Vec<ImportRecord>, pub calls: Vec<CallSiteRecord> } // calls: D89; CallSiteRecord { caller /* index into symbols */, callee_name, qualifier, form: CallForm /* Plain|Path|SelfRef|Receiver|Reference */, line }; at most MAX_CALLS_PER_FILE (4,096) kept and MAX_CALL_NODES_PER_FILE (65,536) examined per file; pub calls_truncated: bool
// code languages use pinned tree-sitter queries in queries/<lang>.scm; shell and inert text use bounded, non-executing structural scanners; all kinds normalize to store vocabulary

// indexer.rs
pub struct Indexer<'a> { /* store, registry, counter */ }
impl Indexer<'_> {
    pub fn with_concurrency(self, concurrency: Concurrency) -> Self; // workers read/hash/parse; Store writes stay on the calling thread in candidate order
    pub fn index(&self, root: &Path, progress: impl FnMut(IndexProgress)) -> Result<IndexReport, AtlasError>;
    // pipeline: canonicalize+approve root -> scan -> parse ONLY added/changed -> replace_indexed_file per file transaction -> delete removed -> stamp index_versions
}
pub enum IndexProgress { Parsing { path: PathBuf }, Removed { path: String } }
pub struct IndexReport { pub files_indexed: usize, pub files_removed: usize, pub files_skipped: usize, pub symbols: usize, pub imports_resolved: usize, pub imports_unresolved: usize, pub call_sites: usize, pub call_site_files_truncated: usize, pub call_site_files_rejected: usize /* E17-29: call sites the store would refuse; the file is written without them */, pub duration_ms: u64, pub stale: bool }

// queries.rs — all read-only, all bounded by token budget (the one write path is edit.rs, §14)
pub struct RepoMap { pub rendered: String, pub files_total: usize, pub files_shown: usize, pub truncated: bool, pub tokens_used: usize, pub token_provider: TokenProvider }
pub struct Outline { pub path: String, pub language: Option<FileLanguage>, pub symbols: Vec<SymbolRecord> }
pub struct SymbolIdentity { pub path: String, pub qualified_name: String, pub kind: SymbolKind, pub signature_hash: Option<String>, pub start_line: u32, pub end_line: u32 }
pub enum UnfoldResult { Found { path: String, symbol: SymbolRecord, content: String, stale: bool, reindex_hint: Option<String> }, Ambiguous { candidates: Vec<SymbolIdentity>, stale: bool, reindex_hint: Option<String> }, NotFound { path: String, qualified_name: String, stale: bool, reindex_hint: String } }
pub struct SearchHit { pub path: String, pub symbol: SymbolRecord, pub score: f64 }
pub struct BoundedRead { pub path: String, pub content: String, pub start_line: u32, pub end_line: u32, pub selected_end_line: u32, pub file_lines_total: u32, pub range_clamped: bool, pub file_tokens_total: usize, pub file_tokens_estimated: bool /* true above 1,000,000 bytes: the total is a byte-length estimate (D82) */, pub tokens_used: usize, pub token_provider: TokenProvider, pub truncated: bool, pub elision: Option<String> }
pub fn repo_map(store: &Store, repo: RepoId, budget_tokens: usize) -> Result<RepoMap>;      // dir tree + top symbols per file, PageRank-ish by edge in-degree
pub fn outline(store: &Store, repo: RepoId, path: &str) -> Result<Outline>;                  // symbol skeleton with line ranges + token estimates
pub fn unfold(store: &Store, repo: RepoId, selector: &SymbolSelector) -> Result<UnfoldResult>; // multiple matches return typed Ambiguous candidates
pub fn search(store: &Store, repo: RepoId, query: &str, limit: usize) -> Result<Vec<SearchHit>>; // Store::search_symbols_all_terms semantics; plan_context keeps literal per-concept queries
// search.rs — diagnostics are informational data on a successful result, never errors
pub struct SearchOptions<'a> { pub limit: usize, pub path: Option<&'a str> } // path: repo-relative file or directory scope, applied before the hit limit
pub enum SearchDiagnosticCode { PathNotFound, QueryTooShort, PathScopeWindowCapped } // as_str(): path_not_found, query_too_short, path_scope_window_capped
pub struct SearchDiagnostic { pub code: SearchDiagnosticCode, pub message: String }
pub struct SearchReport { pub hits: Vec<SearchHit>, pub diagnostics: Vec<SearchDiagnostic> }
pub fn validate_search_query(query: &str) -> Result<(), QueryError>; // QueryError::InvalidSearchQuery { position /* byte offset */, reason }
pub fn search_with_diagnostics(store: &Store, repo: RepoId, query: &str, options: SearchOptions<'_>) -> Result<SearchReport>; // errors only for InvalidSearchQuery / InvalidSearchPath
// grep.rs (E17-22, D87) — bounded content search over indexed files; linear-time `regex` engine; same read gates as read_bounded
pub struct GrepOptions<'a> { pub patterns: &'a [String], pub regex: bool, pub case_insensitive: bool, pub path: Option<&'a str>, pub max_files: usize, pub max_matches_per_file: usize, pub context_lines: usize /* 0..=10, E17-29 */ } // path: prefix or glob (*, **, ?); GrepLine gains before/after: Vec<GrepContextLine { line, text }>
pub fn validate_grep(options: &GrepOptions<'_>) -> Result<(), GrepError>; // GrepError: NoPatterns, TooManyPatterns, EmptyPattern, PatternTooLong, PatternControlCharacter, InvalidPattern, InvalidPathFilter, InvalidLimit, InvalidContext (AtlasError::Grep)
pub fn grep(store: &Store, repo: RepoId, options: &GrepOptions<'_>) -> Result<GrepReport>; // GrepReport { results: Vec<GrepPatternResult>, files_scanned, bytes_scanned, files_in_scope, files_skipped_oversize, files_skipped_unreadable, stale_files, scan_truncated, output_truncated }; fit_to_budget(budget, counter), render_text(), complete() /* false when files were skipped */; regex patterns stop at GREP_REGEX_MAX_BYTES_PER_PATTERN (64 MiB) and set scan_truncated
pub fn read_bounded(root: &Path, path: &str, range: Option<(u32, u32)>, budget_tokens: usize, counter: &dyn TokenCounter) -> Result<BoundedRead>; // path canonicalized, MUST be under approved root; files over MAX_READ_FILE_BYTES (16 MiB) fail with QueryError::FileTooLarge before loading (D77) (symlink-resolved); a secret canonical target or any symbolic-link component below the root reads as NotFound, and on Unix the opened handle must be the inode the gate approved (04 §1.5a, E17-29)
pub fn read_bounded_ranges(root: &Path, requests: &[ReadRequest<'_>], budget_tokens: usize, counter: &dyn TokenCounter) -> Result<Vec<Result<BoundedRead>>>; // E17-29: ReadRequest { path, start_line: Option<u32>, end_line: Option<u32> }, 1..=READ_MAX_RANGES (8); validate_read_requests rejects the whole list (QueryError::InvalidReadRanges); each range gated on its own; budget shared in order, equal share of what is left
pub fn impact(store: &Store, repo: RepoId, path: &str) -> Result<ImpactReport>;              // reverse-edge closure, depth 2
// trace.rs (D90) — call sites resolved at query time (ADR-0033); reads only
pub fn trace(store: &Store, repo: RepoId, from: Option<&str>, to: Option<&str>, options: TraceOptions) -> Result<TraceReport>; // both: shortest paths; from: callees; to: callers. TraceOptions { max_depth: Option<u8> /* 1..=8; None = 6 path, 1 neighbors */, max_paths /* 1..=10 */, max_nodes /* 1..=20000 */, max_results /* 1..=500 */ }; QueryError::InvalidTraceOptions on bad bounds; at most TRACE_MAX_CALL_SITE_ROWS (100,000) call-site rows read per trace, reaching it sets truncated (E17-29); CallGraphMissing unless the stamped parser version carries `+calls-v1`, which with_parser_version appends
// TraceHop and TraceNeighbor carry `reference: bool` (a function passed as a call argument); TraceNeighbor also `cycle`
pub enum TraceResult { Paths(Vec<Vec<TraceHop>>), NoPath, Callees(Vec<TraceNeighbor>), Callers(Vec<TraceNeighbor>), Ambiguous { end, candidates, total }, NotFound { end, name }, CallGraphMissing }
pub struct TraceReport { pub result: TraceResult, pub total: usize, pub nodes_visited: usize, pub depth_reached: u8, pub max_depth: u8, pub truncated: bool, pub depth_limited: bool /* path search, or explicit max_depth */, pub complete: bool /* among recorded calls and references */, pub unfollowed: Vec<UnfollowedCall>, pub unfollowed_total: usize }
pub fn plan_context(store: &Store, repo: RepoId, task: &str, budget_tokens: usize) -> Result<ContextPlan>; // search-driven pack: focused map + file-diverse evidence excerpts within budget
```

Security boundary: every path from external input passes `canonicalize()` and `starts_with(approved_root)` after symlink resolution; violations return `AtlasError::OutsideRoot` (tested).

## 5. compresso-mcp

```rust
pub async fn serve_stdio(deps: McpDeps) -> anyhow::Result<()>;   // binds cwd's longest approved ancestor or remains repository-unbound
pub async fn serve_stdio_for_root(deps: McpDeps, root: PathBuf) -> anyhow::Result<()>; // exact approved canonical root or actionable startup failure
pub struct McpDeps { pub store: Store, pub engine: Engine, pub config: Config }
// One rmcp #[tool] handler per tool in 04-mcp-and-integrations.md §1; handlers call atlas/engine, record operations, and are pure request/response (no background tasks). Repository tools can only use the immutable session root; compression/stats/explain remain available in an unbound session.
// Server instructions string (default profile): <=200 words, factual, lists tools, never instructs client to prefer Compresso over its own policy (build-doc §13.4), because it reaches every installed client without opt-in; compresso_grep is listed neutrally as "text search of indexed files with use sites" (D87). Opt-in surfaces (the lean profile's generated instructions, D92, §12; the exploration agent, D93) may direct the model to explore with Compresso "instead of shell search"; a tool description may state what the tool replaces (instruction-wording rule as amended 2026-11-19 in D92, 04 §1.2).
```

## 6. compresso-integrations

```rust
pub trait Integration {
    fn client_id(&self) -> &'static str;                 // "claude-code" | "codex" | "cursor"
    fn detect(&self) -> Detection;                       // Installed { version, config_path } | NotFound | Unsupported { reason }
    fn plan_install(&self, ctx: &InstallCtx) -> Result<InstallPlan>;   // secret-safe exact key-level diff; exact target stays private
    fn install(&self, plan: &InstallPlan) -> Result<InstallReceipt>;   // backup first; atomic write (tmp+rename); only Compresso-owned keys
    fn verify(&self) -> VerificationReport;              // config parses + compresso server entry resolves to existing binary
    fn uninstall(&self, receipt: &InstallReceipt) -> Result<RestoreReport>; // remove owned keys only; restore semantics per adapter spec
}
pub struct InstallCtx { pub compresso_bin: PathBuf, pub scope: Scope, pub session_recall: bool }  // normalized absolute lexical launcher path; Scope::User | Scope::Project(PathBuf); recall over the client's own transcripts (D75)
```

Per-client config details in `04-mcp-and-integrations.md` §2. User-scoped entries use `mcp serve`; project-scoped Claude Code/Cursor entries pin `--root CANONICAL_ROOT`. With `session_recall` (the CLI default; `--no-recall` clears it) Claude Code and Codex entries end in `--recall-source <own client>`; Cursor ignores it. Claude Desktop: explicitly no adapter (D5).

PB-03 CLI orchestration adds deterministic `compresso repos list`, exact and
confirmable `compresso repos remove ROOT`, and `compresso setup CLIENT ROOT`.
Repository removal binds confirmation to the exact previewed record and Store
revision. Setup approves one exact config path, full-file base/result hashes,
secret-safe key-level diff, diff hash, backup path, and owned-key effect. Exact
target bytes stay private inside the manager and remain bound by `result_hash`;
they are never serialized into CLI or assistant-visible preview output. Before
indexing setup converts that still-valid plan into an opaque, manager-bound,
one-use authorization. For Claude Code and Codex setup also previews and,
after the MCP entry is applied, installs the post-compaction routing-reminder
hook in the client's user-level hook file as a second plan with its own
receipt (D76; `--no-compaction-hook` opts out), and removes by its receipt a
hook an earlier build wrote into the repository's own hook file. A hook step that cannot be planned or applied is reported as `skipped`
or `failed` with fix text and never changes setup's exit status.
Because indexing may outlive the plan TTL, setup renews only `issued_at`,
`expires_at`, and the derived plan hash afterward; every approved material
field, including the private target, remains byte-identical. The strict setup apply path refuses any pending
recovery journal instead of replaying it, and reports the approved, effective,
and active-receipt plan identities without calling an idempotent receipt newly
applied. It also revalidates the repository, project config target, and launcher
before apply. Dry-run, decline, and JSON without `--yes` open no Store and write
nothing. The public launcher may provide `COMPRESSO_LAUNCHER_PATH`; the CLI
validates its executable target but records the normalized, stable absolute
lexical shim so package upgrades need no config rewrite.

## 7. Local API contract (supervisor — E8, spec now)

Authenticated localhost HTTP + UDS (`schemas/local-api/v1.json`). Auth: bearer token from `~/.local/state/compresso/api-token` (0600, generated on first start).

```text
GET  /v1/health          -> { status, version, components: [{name, status}] }
GET  /v1/sessions        -> paginated session list
GET  /v1/operations?session=&since=&limit=  -> traces (no raw content)
GET  /v1/stats?since=    -> StatsSummary (savings vocabulary, 02-schemas §3)
GET  /v1/repos           -> approved repositories + index status
POST /v1/repos           -> { root } approve + index
DELETE /v1/repos/:id     -> remove from index
GET  /v1/config          -> effective config with sources
POST /v1/config/plan     -> { plan, unified_diff, plan_hash, base_hash, expires_at }
POST /v1/config/apply    -> { approve: true, plan_hash, base_hash }; reject drift/expiry, then atomic write
GET  /v1/integrations    -> detection + receipt status per client
POST /v1/integrations/:client/plan       -> { plan, unified_diff, plan_hash, base_hash, backup_path, expires_at }
POST /v1/integrations/:client/install    -> { approve: true, plan_hash, base_hash }; reject target drift
POST /v1/integrations/:client/uninstall/plan  -> { plan, unified_diff, plan_hash, base_hash, backup_path, expires_at }
POST /v1/integrations/:client/uninstall/apply -> { approve: true, plan_hash, base_hash }; restore receipt-owned keys only
```

## 8. Cloud API contract (Phase 6, spec now; `schemas/cloud-api/v1.json`)

Fastify, JSON, versioned prefix `/v1`. Auth: Better Auth session cookies (web) + device JWT (CLI/desktop, RS256, 15-min access / rotating refresh bound to device keypair).

```text
POST /v1/device/authorize      -> device-code flow start { device_code, user_code, verify_url }
POST /v1/device/token          -> poll for tokens
GET  /v1/me                    -> user + orgs + plan
GET  /v1/entitlements          -> signed entitlement bundle (JWS; device-bound 21-day machine lease per D65, entitled enterprise orgs may issue 30/60/90-day offline leases; local caches; grace per build-doc §18.1)
POST /v1/devices               -> register { name, platform, public_key }
DELETE /v1/devices/:id         -> revoke
GET  /v1/orgs/:id/policies     -> policy_set JSON (signed)
PUT  /v1/orgs/:id/policies     -> admin-only
GET  /v1/releases/manifest?channel=stable&platform=darwin-arm64  -> signed release manifest (also served from CDN)
POST /v1/webhooks/stripe       -> subscription lifecycle (signature-verified)
POST /v1/events                -> opt-in aggregate batches ONLY; validated against telemetry allowlist schema; NOT called by slice builds (D6)
GET  /v1/flags                 -> feature flags for device
```

Error shape everywhere: `{ error: { code, message, request_id } }`. Entitlement failures return degraded-but-valid bundles — never a shape that blocks local operation (build-doc §4).

## 9. Local entitlement verification (spec now; E10-local tickets)

```rust
// compresso-agent (or core::entitlements until the supervisor exists)
pub struct EntitlementVerifier { /* embedded ed25519 public keys, keyed by kid */ }
impl EntitlementVerifier {
    pub fn verify(&self, bundle_jws: &str, now: u64) -> Result<Entitlements, EntitlementError>;
    // JWS (EdDSA/ed25519), header kid selects among EMBEDDED_KEYS (previous + current + next).
    // Lease claims bind account, device_id, device public-key hash, plan/features, lease_class,
    // issued_at, refresh_after, expires_at, lease_id, and schema_version. Standard is +14d/+21d.
    // Enterprise offline leases are server-authorized per org/device for exactly 30, 60, or 90 days
    // and set refresh_after to seven days before expiry; 90 days is the absolute protocol maximum.
    // At refresh_after the lease remains valid but reports renewal_due; at expires_at paid local
    // features become local_only(). An active operation is never interrupted.
}
pub struct EntitlementCache<'a> { /* store-backed, entitlement_cache table (02-schemas §4) */ }
pub trait CloudClient: Send + Sync {
    fn begin_device_authorization(&self) -> Result<DeviceAuthorization, AccountError>;
    fn poll_device_token(&self, device_code: &SecretString) -> Result<DevicePoll, AccountError>;
    fn begin_entitlement_challenge(&self, access: &AccessToken) -> Result<LeaseChallenge, AccountError>;
    fn fetch_entitlements(&self, access: &AccessToken, proof: &DeviceProof) -> Result<String, AccountError>;
    fn revoke_device(&self, access: &AccessToken) -> Result<(), AccountError>;
}
pub struct AccountClient { /* HTTPS CloudClient, bounded polling, refresh rotation, keychain */ }
// CLI: compresso account login|status|logout. Logout always clears local credentials and revokes when reachable.
impl EntitlementCache<'_> {
    pub fn current(&self, now: u64) -> Entitlements;          // NEVER errors: missing/invalid/expired lease => Entitlements::local_only()
    pub fn refresh(&self, client: &dyn CloudClient) -> Result<(), EntitlementError>; // only when due, explicitly requested, or recovering
}
// Device proof: server nonce signed by the device private key; server verifies possession before renewal.
// Device credentials (refresh token, device private key): OS keychain via the `keyring` crate — never on disk.
// Scheduled network policy: no automatic entitlement request before refresh_after. Attempt at day 14;
// after failure retry at most once per 24h with bounded jitter until day 21. Explicit login/status/refresh may contact sooner.
// Renewal payload is allowlisted to device_id, device-key proof, entitlement/client version, lease_id, and nonce:
// never prompts, source, repository metadata, paths, operation history, or usage statistics.
// The client never accepts local config, environment variables, CLI flags, cache columns, or wall-clock edits
// as authority for plan/features/lease_class/refresh_after/expires_at; only signed claims are authoritative locally.
// Cloud authorization never trusts this local result: every paid cloud request is independently authorized from
// current server subscription, org membership, seat, device, revocation, and capability state.
// Key rotation: releases carry previous+current+next keys. Server overlap covers the maximum 90-day
// enterprise-offline lease plus the supported-release window.
// Time validation permits ±5 minutes only for nbf/iat; refresh/expiry use monotonic elapsed time anchored at fetched_at.
// Hard rule (owner constraint 4): every failure path degrades to local/free features; nothing here can block
// compression, MCP, or the underlying assistant.
```


## 10. E15 capability interfaces

Authoritative E15 state machines are in `09-advanced-capabilities.md`. Public surfaces are `CombinedSearch`, `EditPlanner`/`EditApplier`, `SqlInspector`/`SqlExecutor`, `RecallIndex`, `KnowledgeStore`, `ReviewOrchestrator`, and `ClientCapabilityNegotiator`. Read and apply interfaces are separate types; apply requires an unexpired content-hashed plan and `ApprovalGrant` issued by the permission engine.

## 11. E17 interfaces (delivered 2026-11-14)

Public surfaces added by E17. Every one is local: none opens a network connection, and none uploads anything.

- **`compresso-core::pricing`** (E17-03, ADR-0029): `PriceTable::embedded() / parse() / lookup(model) / cost_basis(tokens_saved, model)`, `ModelPrice`, `PriceTableError`, and `is_plain_model_id` (printable ASCII, no spaces, at most `MAX_MODEL_ID_BYTES` = 128). Savings are priced at `SAVINGS_RATE_APPLIED = "input"` only. The table is `rust/compresso-core/data/price-table-v1.json`, validated by `schemas/price-table/v1.json`.
- **`compresso-core::transcript`** (E17-03): `scan_transcript(text)`, the streaming `TranscriptScanner::{new, push_line, finish}`, `TranscriptScan` (including `model_ids_rejected`), `TranscriptCall`, `TranscriptCallKind`, `COMPRESSO_TOOL_NAMES`, `MAX_TRANSCRIPT_TOOL_CALLS`, and `UNKNOWN_MODEL`. Scanning retains no transcript text.
- **`compresso-core::stdio`** (E17-05): `StreamError::{classify, is_closed, code}`, `is_closed_pipe`, `ClosedPipeGuard<W>`, `write_diagnostic_line`, and `report_diagnostic`. Closed pipes are typed errors and never panic.
- **`compresso-core::background_spawn`** (E17-05): `CREATE_NO_WINDOW`, `background_creation_flags()`, and `suppress_console_window(set_flags)`. Every child-process launch routes through it. The only permitted launch site is `compresso-cli::commands::mcp_probe` (ADR-0028).
- **`compresso-recall`** (E17-02, new crate, `bounded-local`): `recall(&RecallRequest) -> Result<RecallReport, RecallError>`, plus `RecallRequest`, `RecallScope`, `RecallLimits`, `RecallHit`, `SourceReport`, `SourceStatus`, `RecallSource`, `SourceSupport`, `redact`, and `Redaction`. It reads transcripts read-only, keeps no index, and redacts before matching. E15-14's indexed recall extends this crate instead of creating a new one.
- **`compresso-atlas::search`** (E17-07): `search_with_diagnostics(SearchOptions) -> SearchReport`, `SearchDiagnostic`, `SearchDiagnosticCode`, `validate_search_query`, and `SEARCH_MIN_QUERY_CHARS`. Multi-word queries match every term, with exact-phrase hits ranked first.
- **`compresso-atlas::grep`** (E17-22, D87): `grep(&GrepOptions) -> GrepReport`, `validate_grep`, `GrepError`, `GrepFile`, `GrepLine`, `GrepPatternResult`, and the `GREP_MAX_*` bounds. Served as the `compresso_grep` MCP tool and the `compresso grep` CLI command (04 §1.4a).
- **`compresso-store`** (E17-23, D88): `Store::symbols_named(repo, name, limit) -> Vec<SymbolHit>`, an exact, case-sensitive bare-name lookup through the `symbols(name)` index, path and line ordered, used by the context planner to resolve identifiers a task writes verbatim. It is distinct from `Store::indexed_symbols_named(repo, name) -> Vec<IndexedSymbol>` (E17-24, D89), the unbounded lookup `trace` uses to resolve call sites.
- **`compresso-mcp`**: `tool_names()` (the served tool list, used by the drift-lint inventory) and `reminder::routing_reminder()` with `ROUTING_REMINDER_MAX_BYTES` = 640 (E17-01). The `compresso_recall` tool is served only when the server starts with `--recall-source`.
- **`compresso-cli::surface`** (E17-08): `surface_inventory() -> serde_json::Value` (`SURFACE_SCHEMA_VERSION` = 1). It generates the CLI/MCP/config inventory consumed by `scripts/doc-drift-lint.ts`.
- **`compresso-eval::campaign`** (E17-10): `CampaignRunner`, `CampaignPolicy`, `CampaignOutcome`, `StartMode`, `Checkpoint`, `TaskState`, `SuiteAdapter`, `SuiteDescriptor`, `TaskSpec`, `TaskContext`, `AdapterKind`, `resolve_adapter`, `FixtureSuiteAdapter`, and `CampaignReport`, exposed by the separate `compresso-eval-campaign` binary. Adapters for external suites are opt-in and not built in.

## 12. E17-26..28 interfaces (owner-confirmed 2026-11-19)

Opt-in surfaces for lower fixed overhead, tool substitution, and orientation (D91-D93). Defaults are unchanged; none adds a network or process capability.

- **`compresso-mcp::profile`** (E17-27, D92): `McpToolProfile::{Full (default), Lean}` with `parse("full"|"lean")` and `name()`; `lean_tool_allowlist()` (the `LEAN_TOOLS` rows: context, repo_map, search, grep, trace, outline, unfold, read, recall); `McpServeOptions { root, recall, tools, results }` (`results` added by E17-31, §13) with `serve_stdio_with_options` / `serve_io_with_options` (the older `serve_*` entry points keep the full profile); `advertised_surface(profile, recall_enabled) -> (Vec<Tool>, String)` returns the exact advertised tools and instructions. `tool_names()` is unchanged and still lists every registered tool. CLI: `compresso mcp serve --tools full|lean`, combinable with `--root` and `--recall-source`.
- **`compresso-atlas::cards`** (E17-26, D91): `module_cards(store, repo, root, CardOptions { path, max_cards, budget_tokens }) -> Result<ModuleCards, AtlasError>`; `ModuleCards { cards, cards_total, tokens_used, token_provider, truncated }`; `ModuleCard { path, purpose, files, symbols, entry_points, public_symbols: Vec<CardSymbol>, depends_on, depends_on_total, used_by, used_by_total, rendered, tokens }`; `CardSymbol { name, kind, file, line }`; `DEFAULT_MAX_CARDS` = 12, `MAX_CARDS` = 50. Read-only, deterministic, no model call; `path` is validated like `repo_map_focused`'s focus. `repo_map` and `repo_map_focused` are unchanged. MCP `compresso_repo_map` gains opt-in `cards` and `path`; CLI `compresso map` gains `--cards`, `--path`, and `--max-cards` (04 §1.3).
- **`compresso-integrations::explore_agent`** (E17-28, D93, ADR-0034): `ExploreAgentIntegration::new(user_home, compresso_tools)` implementing `Integration` (client `claude-code`), plus `config_path(scope)`, `render()`, and `EXPLORE_AGENT_NAME` = `compresso-explorer`. `ConfigFormat::Markdown` is a whole-file owned format whose empty uninstall target deletes the file; `UNMODIFIED_FILE_REMOVED_BACKUP_SENTINEL`; `IntegrationError::UserFilePresent { path }` (CLI code `user_file_present`). CLI: `--explore-agent` on `integrations install|uninstall|status|verify` (04 §2.6). The CLI passes `compresso_mcp::lean_tool_allowlist()` minus `compresso_recall` as the agent's Compresso tools.

## 15. E17-33 interfaces (D98)

Result-size bounds for the compact text form. No public type or signature changes; the JSON form and the full profile's default results are unchanged.

- **`compresso-mcp` read window** (crate-private, `tools::atlas_read`): in the text form, `compresso_read` holds each range to `TEXT_READ_RANGE_LINES` (80; a range at most `TEXT_READ_SLACK_LINES` (10) longer is served whole) and each call to `TEXT_READ_CALL_LINES` (200) across its ranges, sharing the call's lines evenly among long ranges. A windowed range's internal payload carries `window { lines, from, to, symbols, symbols_more }` (symbols as `name LINE`, at most `TEXT_WINDOW_SYMBOLS` (8), chosen top-level first, from `outline_current` only when the file is not stale), `resolved_range.end_line` stays the requested end clamped to the file, and `truncated` is true. When the token budget cuts inside the window, no `window` is added and `elision` covers the whole requested range with its exact overage. `text::render` turns `window` into one trailing `[lines A-B not shown: ...; continue with start_line A]` line.
- **`compresso-mcp` grep rows** (crate-private, `tools::atlas_grep`): in the text form, file rows are rebuilt from the fitted `GrepReport` so a match row names its enclosing symbol only when it differs from the previous match row's in that file (`[-]` for none after a named row; definitions always named). `GrepLine::rendered_rows`, `GrepReport::render_text`, and the JSON rows are unchanged. Text-form defaults: `max_matches_per_file` 3 and `max_tokens` 1,000 when the call omits them.
- **`compresso-mcp::profile`**: new lean descriptions for `compresso_grep` and `compresso_read` (04 §1.2.1). **`compresso-integrations::explore_agent`**: one added instruction line (04 §2.6).
- **`compresso-cli` example `replay_results`** (dev tool, not shipped): `cargo run -p compresso-cli --example replay_results -- --bin PATH --root REPO --calls FILE [--tools lean|full] [--results text|json] [--only TOOL,...] [--dump DIR]` replays a JSON array of `{tool, arguments}` or a stream-JSON transcript's Compresso calls against `compresso mcp serve` and prints per-tool calls, errors, and characters as JSON. It inherits the caller's environment, so `HOME` must point at a scratch store that indexes the repository.

## 14. E17-32 interfaces (D97, ADR-0035)

One opt-in write path. Every other interface in this document stays read-only; no network or process capability is added.

- **`compresso-atlas::edit`**: `apply_edits(store, root, &EditRequest { operations, dry_run, protected }, counter) -> Result<EditReport, EditError>`. `EditOperation::{Replace { path, old, new, expected_count, expected_hash }, Rename { old, new, path, expected_count }, Insert { path, anchor, text, position: InsertPosition::{Before, After}, expected_hash }}`. `EditReport { dry_run, files: Vec<FileChange>, operations: Vec<OperationOutcome>, diff, diff_truncated, bytes_written, index: IndexState::{NotRun, Fresh, Stale} }`; `FileChange { path, sites, lines, stale, line_ending, bom, bytes_before, bytes_after, hash_before, hash_after, verified }`; `RenameOutcome { scope, files_searched, files_skipped, remaining_whole_total, remaining_whole, remaining_substring_total, remaining_substring, new_name_preexisting }`. `EditError::{Invalid, NotIndexed, OutsideRoot, Refused { path, reason: Refusal }, CountMismatch, NoMatches, HashMismatch, Bound { bound: EditBound, .. }, Changed, Read, WriteFailed { path, restored, unrestored, .. }, Store, Atlas}`; only `WriteFailed` can follow a write. Identifier helpers `is_identifier_char`, `is_identifier`, `identifier_occurrences`; bounds `EDIT_MAX_*`. Targets pass the shared read gate (`resolve_under_root`) plus the edit gates of ADR-0035; writes are atomic per file with re-verification and rollback; a successful write re-indexes through `Indexer::index`.
- **`compresso-store`**: `Store::database_path() -> Option<PathBuf>`; the ledger classifies `compresso_edit` as its own operation kind (no trace, no retrieval metrics, no savings; no migration).
- **`compresso-mcp`**: `McpServeOptions.edits` (`mcp serve --edits`); `advertised_surface_with(profile, recall_enabled, edits_enabled)` (`advertised_surface` is the `edits_enabled = false` case); `tool_names()` lists `compresso_edit` last; `compresso_mcp::edit::{report_payload, fit, edit_failure_message, parse_operations_value, protected_paths, render_text, TOOL_NAME}` shared with the CLI.
- **`compresso-cli`**: `compresso edit` (`EditArgs`), and `--edits` on `integrations install` (Claude Code server entry, or with `--explore-agent` the agent's tool list).
- **`compresso-integrations`**: `ClaudeCodeIntegration::with_edits(bool)`, `ExploreAgentIntegration::with_edits(bool)`, `server_enables_edits(args)`; adapter-shaped arguments are the base, optionally `--edits`, optionally the recall pair, in that order.

## 13. E17-31 interfaces (D96)

Compact text tool results for the lean profile. The full profile's JSON results are unchanged byte for byte; no new network or process capability.

- **`compresso-mcp::profile`**: `McpResultFormat::{Json, Text}` with `parse("json"|"text")` and `name()`; `McpToolProfile::default_results()` (`Full` -> `Json`, `Lean` -> `Text`). `McpServeOptions.results: Option<McpResultFormat>`; `None` takes the profile default. CLI: `compresso mcp serve --results json|text`, combinable with `--tools`, `--root`, and `--recall-source`. No installer or default path writes `--results`.
- **`compresso-mcp::tools::text`** (crate-private): `render(tool, &payload) -> Option<String>` renders a tool's internal JSON payload as compact text for context, repo_map (with cards), search, grep, trace, outline, unfold, read (single, gate, and `ranges`), and recall; every other tool (compress, stats, explain, impact) returns `None` and sends JSON in both forms. Tools keep building the same payload, so operation recording, stats, and explain are unchanged.
- **Budget**: tools fit list results with `common::cap_rows` (one rendered row per line) instead of `cap_values` when the form is text; grep and context measure their rendered text in their fitting loops. The dispatcher re-runs a tool with a larger token reserve when the rendered text exceeds the call's `max_tokens` (at most four attempts), then cuts at a line boundary with `[output cut to max_tokens]` as a last resort, so returned text never exceeds `max_tokens`. The ledger's `tokens_served` counts the text actually sent.
