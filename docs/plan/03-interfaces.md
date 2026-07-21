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
}
pub enum NodeKind { SystemInstruction, UserInstruction, AssistantResponse, ToolDefinition, ToolRequest, ToolResponse, SourceFile, CodeSymbol, SearchResult, CompilerOutput, TestOutput, RepoMap, Summary, Decision, MemoryFact }
pub enum Source { Client, Repository { path: String }, Tool { name: String }, Synthetic }
pub struct ContextEdge { pub from: String, pub to: String, pub kind: EdgeKind, pub weight: f32 }
pub enum EdgeKind { Imports, Calls, Defines, References, Tests, Supersedes, ProducedByTool, RespondsTo, DependsOn, SameErrorGroup }

impl ContextGraph {
    pub fn validate(&self) -> Result<(), GraphError>;      // unique ids, edge endpoints, protection invariants (02-schemas §1)
    pub fn total_tokens(&self) -> usize;
    pub fn from_text(content: &str, kind: NodeKind, counter: &dyn TokenCounter) -> Self; // single-node convenience
}
```

### 2.2 `tokens` module

```rust
pub enum TokenProvider { OpenAiO200k, OpenAiCl100k, AnthropicCalibrated, Heuristic }
pub trait TokenCounter: Send + Sync {
    fn provider(&self) -> TokenProvider;
    fn count(&self, text: &str) -> usize;
}
pub fn counter_for(provider: TokenProvider) -> &'static dyn TokenCounter; // lazily-initialized singletons
// AnthropicCalibrated = O200k count × CALIBRATION (const, derived from fixtures/provider-payloads/anthropic-counts.json; documented in ADR-0017)
```

### 2.3 `strategy` module

```rust
pub struct Estimate { pub tokens_saved: usize, pub confidence: f32 }
pub struct StrategyOutput { pub graph: ContextGraph, pub notes: Vec<String> }

pub trait CompressionStrategy: Send + Sync {
    fn id(&self) -> &'static str;                          // "dedup_exact", "collapse_logs", "compact_json", "pack_budget"
    fn supports(&self, input: &ContextGraph) -> bool;
    fn estimate(&self, input: &ContextGraph) -> Estimate;
    fn execute(&self, input: &ContextGraph, budget: &CompressionBudget) -> Result<StrategyOutput, StrategyError>;
}

pub struct StrategyRegistry { /* ordered Vec<Box<dyn CompressionStrategy>> */ }
impl StrategyRegistry {
    pub fn safe_default(mode: Mode) -> Self;               // safe: [dedup_exact, collapse_logs, compact_json, pack_budget]; passthrough: []
    pub fn strategies(&self) -> &[Box<dyn CompressionStrategy>];
}
```

Slice strategies (each its own file under `strategies/`): `dedup_exact` (identical unprotected node contents → keep first, replace rest with reference stub), `collapse_logs` (≥3 consecutive lines matching after timestamp/hex/uuid normalization → `<line> [×N repeats]`), `compact_json` (tool_response nodes with valid JSON: minify + truncate long string values/arrays with elision markers, structure preserved), `pack_budget` (drop lowest `priority/token` unprotected nodes until under `max_tokens`; never partial-truncate code).

### 2.4 `engine` module

```rust
pub enum Mode { Safe, Balanced, Aggressive, Passthrough }  // slice implements Safe + Passthrough
pub struct CompressionBudget { pub max_tokens: usize, pub max_latency_ms: u64, pub allowed_lossiness: Lossiness, pub protected_kinds: Vec<NodeKind> }
pub enum Lossiness { None, Structural, Summarizing }
pub struct CompressionResult { pub output: ContextGraph, pub before_tokens: usize, pub after_tokens: usize, pub strategy_results: Vec<StrategyResult>, pub safety_checks: Vec<SafetyCheck>, pub fallback_used: bool, pub trace: Trace }

pub struct Engine { /* registry, counter, safety */ }
impl Engine {
    pub fn new(mode: Mode, counter: &'static dyn TokenCounter) -> Self;
    pub fn compress(&self, input: ContextGraph, budget: &CompressionBudget) -> CompressionResult;
    // NEVER returns Err: any failure => fallback_used=true, output=input unchanged
}
```

Pipeline (build-doc §10.3): validate → mark protected → run strategies in registry order, each within remaining latency budget → safety-check the cumulative result → on any failure restore original graph, set `fallback_used` + `fallback_reason`.

### 2.5 `safety` module

```rust
pub struct SafetyCheck { pub name: String, pub passed: bool, pub detail: String }
pub fn run_all(original: &ContextGraph, candidate: &ContextGraph, budget: &CompressionBudget, elapsed_ms: u64) -> Vec<SafetyCheck>;
```

Checks (all must pass; build-doc §10.6): `protected_unchanged` (byte-equal content for every protected node), `no_inflation` (after ≤ before unless a note explains), `json_still_valid` (nodes that were valid JSON still are), `fences_balanced` (``` count parity preserved per node), `refs_valid` (file:line references in output exist in input), `latency_ok`, `schema_valid` (output revalidates).

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
pub struct Store { /* rusqlite Connection, WAL */ }
impl Store {
    pub fn open(path: &Path) -> Result<Store, StoreError>;   // runs pending migrations transactionally
    pub fn open_default() -> Result<Store, StoreError>;      // platform data dir (01-stack §5)
    // repositories
    pub fn approve_repo(&self, canonical_root: &Path) -> Result<RepoId>;
    pub fn repo_by_root(&self, canonical_root: &Path) -> Result<Option<RepoRecord>>;
    // files & symbols (used only by atlas)
    pub fn upsert_file(&self, repo: RepoId, rec: &FileRecord) -> Result<FileId>;
    pub fn file_hashes(&self, repo: RepoId) -> Result<HashMap<String, String>>;  // path -> blake3
    pub fn replace_symbols(&self, file: FileId, symbols: &[SymbolRecord], edges: &[EdgeRecord]) -> Result<()>; // single transaction
    pub fn delete_files(&self, repo: RepoId, paths: &[String]) -> Result<()>;
    pub fn search_symbols(&self, repo: RepoId, query: &str, limit: usize) -> Result<Vec<SymbolHit>>; // FTS5, bm25-ranked
    pub fn symbols_in_file(&self, repo: RepoId, path: &str) -> Result<Vec<SymbolRecord>>;
    pub fn symbol_by_name(&self, repo: RepoId, qualified: &str) -> Result<Option<(FileRecord, SymbolRecord)>>;
    // sessions & operations
    pub fn start_session(&self, client: &str, transport: &str) -> Result<SessionId>;
    pub fn record_operation(&self, op: &OperationRecord) -> Result<()>;   // validates trace_json against schema
    pub fn stats(&self, since: Option<i64>) -> Result<StatsSummary>;
    // receipts
    pub fn save_receipt(&self, r: &ReceiptRecord) -> Result<()>;
    pub fn receipts(&self, client: Option<&str>) -> Result<Vec<ReceiptRecord>>;
}
```

Boundary rule: only `compresso-store` contains SQL. Atlas/MCP/CLI call these methods.

## 4. compresso-atlas

```rust
// scanner.rs
pub struct ScanResult { pub added: Vec<PathBuf>, pub changed: Vec<PathBuf>, pub removed: Vec<String>, pub skipped: Vec<(PathBuf, SkipReason)> }
pub fn scan(store: &Store, repo: RepoId, root: &Path) -> Result<ScanResult, AtlasError>;
// respects .gitignore + built-in exclusions (binaries, >max_file_bytes, node_modules/target/dist/.git, secret-file patterns: .env*, *.pem, id_rsa*)

// parsers.rs
pub struct ParserRegistry;
impl ParserRegistry {
    pub fn for_path(path: &Path) -> Option<Language>;      // by extension: rs, ts, tsx, js, jsx, mjs, cjs, py
    pub fn extract(lang: Language, source: &str) -> Result<Extraction, ParseError>;
}
pub struct Extraction { pub symbols: Vec<SymbolRecord>, pub imports: Vec<String> }
// tree-sitter queries per language live in queries/<lang>.scm; symbol kinds normalized to store vocabulary

// indexer.rs
pub struct Indexer<'a> { /* store, registry, counter */ }
impl Indexer<'_> {
    pub fn index(&self, root: &Path, progress: impl FnMut(IndexProgress)) -> Result<IndexReport, AtlasError>;
    // pipeline: canonicalize+approve root -> scan -> parse ONLY added/changed -> replace_symbols per file transaction -> delete removed -> stamp index_versions
}
pub struct IndexReport { pub files_indexed: usize, pub files_skipped: usize, pub symbols: usize, pub duration_ms: u64, pub stale: bool }

// queries.rs — all read-only, all bounded by token budget
pub fn repo_map(store: &Store, repo: RepoId, budget_tokens: usize) -> Result<RepoMap>;      // dir tree + top symbols per file, PageRank-ish by edge in-degree
pub fn outline(store: &Store, repo: RepoId, path: &str) -> Result<Outline>;                  // symbol skeleton with line ranges + token estimates
pub fn unfold(store: &Store, repo: RepoId, path: &str, symbol: &str) -> Result<UnfoldResult>; // exact body via stored line range, read from disk, hash-checked (stale -> flag + reindex hint)
pub fn search(store: &Store, repo: RepoId, query: &str, limit: usize) -> Result<Vec<SearchHit>>;
pub fn read_bounded(root: &Path, path: &str, range: Option<(u32, u32)>, budget_tokens: usize, counter: &dyn TokenCounter) -> Result<BoundedRead>; // path canonicalized, MUST be under approved root (symlink-resolved)
pub fn impact(store: &Store, repo: RepoId, path: &str) -> Result<ImpactReport>;              // reverse-edge closure, depth 2
pub fn plan_context(store: &Store, repo: RepoId, task: &str, budget_tokens: usize) -> Result<ContextPlan>; // search-driven pack: map + outlines + top symbols within budget
```

Security boundary: every path from external input passes `canonicalize()` and `starts_with(approved_root)` after symlink resolution; violations return `AtlasError::OutsideRoot` (tested).

## 5. compresso-mcp

```rust
pub async fn serve_stdio(deps: McpDeps) -> anyhow::Result<()>;   // rmcp ServerHandler; blocks until client disconnect
pub struct McpDeps { pub store: Store, pub engine: Engine, pub config: Config }
// One rmcp #[tool] handler per tool in 04-mcp-and-integrations.md §1; handlers call atlas/engine, record operations, and are pure request/response (no background tasks).
// Server instructions string: <=200 words, factual, lists tools, never instructs client to prefer Compresso over its own policy (build-doc §13.4).
```

## 6. compresso-integrations

```rust
pub trait Integration {
    fn client_id(&self) -> &'static str;                 // "claude-code" | "codex" | "cursor"
    fn detect(&self) -> Detection;                       // Installed { version, config_path } | NotFound | Unsupported { reason }
    fn plan_install(&self, ctx: &InstallCtx) -> Result<InstallPlan>;   // exact key-level diff, human-readable
    fn install(&self, plan: &InstallPlan) -> Result<InstallReceipt>;   // backup first; atomic write (tmp+rename); only Compresso-owned keys
    fn verify(&self) -> VerificationReport;              // config parses + compresso server entry resolves to existing binary
    fn uninstall(&self, receipt: &InstallReceipt) -> Result<RestoreReport>; // remove owned keys only; restore semantics per adapter spec
}
pub struct InstallCtx { pub compresso_bin: PathBuf, pub scope: Scope }  // Scope::User | Scope::Project(PathBuf)
```

Per-client config details in `04-mcp-and-integrations.md` §2. Claude Desktop: explicitly no adapter (D5).

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
PUT  /v1/config          -> validated partial update (atomic file write)
GET  /v1/integrations    -> detection + receipt status per client
POST /v1/integrations/:client/install    -> body { approve: true, plan_hash } (two-step: plan then approve)
POST /v1/integrations/:client/uninstall
```

## 8. Cloud API contract (Phase 6, spec now; `schemas/cloud-api/v1.json`)

Fastify, JSON, versioned prefix `/v1`. Auth: Better Auth session cookies (web) + device JWT (CLI/desktop, RS256, 15-min access / rotating refresh bound to device keypair).

```text
POST /v1/device/authorize      -> device-code flow start { device_code, user_code, verify_url }
POST /v1/device/token          -> poll for tokens
GET  /v1/me                    -> user + orgs + plan
GET  /v1/entitlements          -> signed entitlement bundle (JWS, 30-day offline validity; local caches; grace per build-doc §18.1)
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
