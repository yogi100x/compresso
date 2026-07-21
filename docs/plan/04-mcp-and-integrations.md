# 04 — MCP Tool Schemas and Client Integrations

Scope: the ten read-only MCP tools shipped in the first slice (D1, D7) and the three client integrations (D4). Claude Desktop is explicitly excluded (D5). Memory tools (`compresso_remember`/`recall`/`forget`) and write-capable tools (`compresso_edit`/`batch_edit`) are later phases and not specified here. Local-only stats per D6; no tool ever transmits data off the device.

## 1. MCP tool surface

### 1.1 Conventions shared by every tool

- **Transport**: stdio (`compresso mcp serve`), rmcp `ServerHandler` per 03-interfaces §5. One handler per tool; pure request/response, no background tasks, no shell execution anywhere in the tool path.
- **Repository resolution**: tools that operate on a repository accept an optional `root` (absolute path). Resolution order when omitted: (1) canonicalized server working directory, matched by longest prefix against approved repository roots; (2) if exactly one repository is approved, that repository; (3) otherwise `not_indexed` error. Every externally supplied path is canonicalized (symlinks resolved) and must satisfy `starts_with(approved_root)` (03-interfaces §4); violations return `outside_root`.
- **Token budgets**: every tool that returns repository or text content takes `max_tokens` (name chosen for Tetris compatibility, D7). Bounds are uniform: minimum 100, maximum 100000; per-tool defaults below. Output is truncated to fit the budget with an explicit elision marker and `truncated: true` in the result. Token counts are produced by the real tokenizer stack (D14) and always carry a `token_provider` tag.
- **Operation recording**: every handler calls `Store::record_operation` (03-interfaces §3). Compression operations (`compresso_compress`) record the full compression-trace v1 `trace_json` (02-schemas §2); retrieval operations (map, search, read, outline, unfold, context, impact, stats, explain) record `trace_json = NULL` with `tokens_served` set to the tokens returned (02-schemas §4). Traces never contain raw content. The `operation_id` (ulid) is included in every successful result so `compresso_explain` can retrieve it.
- **Error behavior**: errors are returned as MCP tool results with `isError: true` and a single text content block — never protocol-level failures, never a crashed server. The text starts with a stable code in brackets, then an instructive human sentence. Codes: `not_indexed`, `outside_root`, `not_found`, `invalid_arguments`, `internal`. (Staleness is never an error code — stale results are served with `stale: true` plus a reindex hint.) Unknown/unindexed repository always produces:

```text
[not_indexed] Repository '/path/to/repo' is not indexed by Compresso. Run `compresso index` in that directory, then retry.
```

- **Fail-open**: `compresso_compress` never errors on compression failure; it returns the original text with `fallback_used: true` and a `fallback_reason` (D38). `internal` is reserved for defects (e.g. corrupt store) and still returns `isError` tool results, not crashes.
- **Result envelope**: all results are a single JSON text content block. Common fields: `schema_version` (1), `operation_id`, `token_provider`, `tokens_used` (tokens of the returned payload), `truncated`.

### 1.2 Server instructions string

Shipped verbatim as the rmcp server `instructions` (build-doc §13.4: brief, factual, never overrides client policy). 137 words:

> Compresso provides token-efficient, read-only access to indexed repositories, plus local text compression. Tools: compresso_repo_map (ranked repository overview), compresso_search (indexed symbol search), compresso_outline (symbol skeleton of one file), compresso_unfold (exact body of one symbol), compresso_read (token-budgeted file read; gate:true returns a worth-reading verdict and cost estimate without content), compresso_context (task-scoped context pack), compresso_impact (reverse-dependency report for a file), compresso_compress (compress arbitrary text, returns before/after token counts), compresso_stats (local savings summary), compresso_explain (trace for a past operation). Every tool bounds its output by a max_tokens budget, executes no shell commands, and works entirely locally; no content leaves this machine. For navigation, outline plus unfold is usually cheaper than a full file read. If a tool reports a repository is not indexed, run `compresso index` in that repository and retry.

### 1.3 compresso_repo_map

**Description (final copy)**: Returns a token-budgeted map of an indexed repository: directory tree plus the most structurally important symbols per file, ranked by edge in-degree. Use this first when orienting in an unfamiliar repository; it is the cheapest way to decide where to look next. Read-only; requires the repository to have been indexed with `compresso index`.

**inputSchema**:

```json
{
  "type": "object",
  "properties": {
    "root": { "type": "string", "description": "Absolute path to the repository root. Optional; defaults to the server working directory's approved repository." },
    "focus": { "type": "string", "description": "Repo-relative path prefix to bias ranking toward (e.g. 'src/auth')." },
    "max_tokens": { "type": "integer", "minimum": 100, "maximum": 100000, "default": 2000 }
  },
  "required": []
}
```

**Result shape**:

```json
{
  "schema_version": 1,
  "operation_id": "01J2X3ABCDEF0123456789ABCD",
  "root": "/Users/dev/project",
  "map": "src/\n  session.rs  — SessionManager, SessionManager::start, TokenBucket\n  auth/\n    login.ts  — loginHandler, validateCredentials\n  ...",
  "files_total": 412,
  "files_shown": 87,
  "stale": false,
  "tokens_used": 1874,
  "token_provider": "openai_o200k",
  "truncated": true
}
```

**Errors**: `not_indexed` (with `compresso index` instruction), `outside_root`, `invalid_arguments`. `stale: true` (not an error) when file hashes have drifted since last index; the map is still served with a reindex hint appended.

**Tetris compatibility**: functional counterpart of `tetris_repo_map`; same purpose, `max_tokens` budget name retained.

### 1.4 compresso_search

**Description (final copy)**: Searches the symbol index of a repository (names, qualified names, signatures; FTS5, bm25-ranked) and returns compact hits with file, line range, and signature. Cheaper and more precise than grep for "where is X defined/used" questions. Read-only; requires an indexed repository.

**inputSchema**:

```json
{
  "type": "object",
  "properties": {
    "root": { "type": "string", "description": "Absolute repository root. Optional." },
    "query": { "type": "string", "minLength": 1, "description": "Search text, e.g. 'SessionManager' or 'start session'." },
    "limit": { "type": "integer", "minimum": 1, "maximum": 100, "default": 20 },
    "max_tokens": { "type": "integer", "minimum": 100, "maximum": 100000, "default": 2000 }
  },
  "required": ["query"]
}
```

**Result shape**:

```json
{
  "schema_version": 1,
  "operation_id": "01J2X3...",
  "query": "SessionManager",
  "hits": [
    {
      "path": "src/session.rs",
      "symbol": "SessionManager::start",
      "kind": "method",
      "signature": "pub fn start(&mut self, user: UserId) -> Result<Session>",
      "start_line": 42,
      "end_line": 88,
      "score": 12.4,
      "token_estimate": 310
    }
  ],
  "hits_total": 7,
  "tokens_used": 640,
  "token_provider": "openai_o200k",
  "truncated": false
}
```

**Errors**: `not_indexed`, `invalid_arguments` (empty query). Zero hits is a success with `hits: []`, plus a one-line suggestion to broaden the query or reindex if `stale`.

**Tetris compatibility (D7)**: replaces `tetris_search`. Argument names `query`, `limit`, `max_tokens` match Tetris semantics; results are token-bounded the same way.

### 1.5 compresso_read

**Description (final copy)**: Reads a file (or line range) from an approved repository under a hard token budget, truncating with explicit elision markers when the budget is exceeded. With `gate: true` it returns only a worth-reading verdict and token cost estimate — no content — so you can decide whether a large file is worth the budget before spending it. Read-only; paths must be inside an approved repository root.

**inputSchema**:

```json
{
  "type": "object",
  "properties": {
    "root": { "type": "string", "description": "Absolute repository root. Optional." },
    "path": { "type": "string", "minLength": 1, "description": "Repo-relative file path." },
    "start_line": { "type": "integer", "minimum": 1, "description": "1-based inclusive. Optional." },
    "end_line": { "type": "integer", "minimum": 1, "description": "1-based inclusive. Optional; requires start_line." },
    "max_tokens": { "type": "integer", "minimum": 100, "maximum": 100000, "default": 2000 },
    "gate": { "type": "boolean", "default": false, "description": "If true, return verdict + cost estimate only, no content." }
  },
  "required": ["path"]
}
```

**Result shape (normal read)**:

```json
{
  "schema_version": 1,
  "operation_id": "01J2X3...",
  "path": "src/session.rs",
  "content": "pub struct SessionManager { ... }\n...",
  "range": { "start_line": 1, "end_line": 220 },
  "file_tokens_total": 5100,
  "tokens_used": 1998,
  "token_provider": "openai_o200k",
  "truncated": true,
  "elision": "[lines 221-540 elided: 3102 tokens over budget]"
}
```

**Result shape (gate mode)**:

```json
{
  "schema_version": 1,
  "operation_id": "01J2X3...",
  "path": "src/session.rs",
  "gate": {
    "worth_reading": true,
    "reason": "Defines 14 symbols including 3 with high in-degree (SessionManager). Matches recent search terms.",
    "file_tokens_total": 5100,
    "recommended_budget": 2200,
    "cheaper_alternative": "compresso_outline then compresso_unfold SessionManager::start"
  },
  "tokens_used": 55,
  "token_provider": "openai_o200k"
}
```

**Errors**: `not_indexed`, `outside_root` (canonicalized path escapes approved root), `not_found` (file missing on disk), `invalid_arguments` (end_line < start_line, end_line without start_line). Binary files return `invalid_arguments` with a note; never raw bytes.

**Tetris compatibility (D7)**: replaces `tetris_read` (token-budgeted file read; budget argument is named `max_tokens` exactly as in Tetris) and replaces `tetris_context_gate` via `gate: true` (verdict + cost estimate without content).

### 1.6 compresso_outline

**Description (final copy)**: Returns the symbol skeleton of one file: every indexed symbol with kind, signature, line range, and per-symbol token estimate, but no bodies. Combine with compresso_unfold to fetch only the bodies you need — typically ~70% cheaper than a full file read for navigation. Read-only; requires an indexed repository.

**inputSchema**:

```json
{
  "type": "object",
  "properties": {
    "root": { "type": "string", "description": "Absolute repository root. Optional." },
    "path": { "type": "string", "minLength": 1, "description": "Repo-relative file path." },
    "max_tokens": { "type": "integer", "minimum": 100, "maximum": 100000, "default": 1500 }
  },
  "required": ["path"]
}
```

**Result shape**:

```json
{
  "schema_version": 1,
  "operation_id": "01J2X3...",
  "path": "src/session.rs",
  "language": "rust",
  "symbols": [
    { "qualified_name": "SessionManager", "kind": "struct", "signature": "pub struct SessionManager", "start_line": 10, "end_line": 18, "token_estimate": 60 },
    { "qualified_name": "SessionManager::start", "kind": "method", "signature": "pub fn start(&mut self, user: UserId) -> Result<Session>", "start_line": 42, "end_line": 88, "token_estimate": 310 }
  ],
  "stale": false,
  "tokens_used": 420,
  "token_provider": "openai_o200k",
  "truncated": false
}
```

**Errors**: `not_indexed`, `outside_root`, `not_found` (file not in index — message distinguishes "not on disk" from "on disk but unparsed language; use compresso_read"). `stale: true` when the on-disk blake3 hash no longer matches the index; outline is still served with a reindex hint.

**Tetris compatibility (D7)**: replaces `tetris_outline`; same skeleton-first workflow and argument semantics.

### 1.7 compresso_unfold

**Description (final copy)**: Returns the exact source body of one named symbol, located via the index's stored line range and read from disk with a content-hash check. This is the precision companion to compresso_outline: fetch only the symbols you need instead of whole files. Read-only; requires an indexed repository.

**inputSchema**:

```json
{
  "type": "object",
  "properties": {
    "root": { "type": "string", "description": "Absolute repository root. Optional." },
    "path": { "type": "string", "minLength": 1, "description": "Repo-relative file path." },
    "symbol": { "type": "string", "minLength": 1, "description": "Qualified symbol name as shown by compresso_outline or compresso_search, e.g. 'SessionManager::start'." },
    "max_tokens": { "type": "integer", "minimum": 100, "maximum": 100000, "default": 2000 }
  },
  "required": ["path", "symbol"]
}
```

**Result shape**:

```json
{
  "schema_version": 1,
  "operation_id": "01J2X3...",
  "path": "src/session.rs",
  "symbol": "SessionManager::start",
  "kind": "method",
  "start_line": 42,
  "end_line": 88,
  "content": "pub fn start(&mut self, user: UserId) -> Result<Session> {\n    ...\n}",
  "stale": false,
  "tokens_used": 310,
  "token_provider": "openai_o200k",
  "truncated": false
}
```

**Errors**: `not_indexed`, `outside_root`, `not_found` (symbol unknown — message lists up to 5 closest qualified-name matches). Staleness is NOT an error (fail-open, matching 03-interfaces §4): on a blake3 hash mismatch the tool re-locates the symbol in the current on-disk content by name; if found, it serves the current body with `stale: true` and a reindex hint; only if the symbol cannot be re-located in the changed file does it return `not_found` with a `compresso index` instruction.

**Tetris compatibility (D7)**: replaces `tetris_unfold`; identical outline→unfold contract.

### 1.8 compresso_context

**Description (final copy)**: Builds a minimal context pack for a stated task within a token budget: a focused repository-map excerpt, outlines of the most relevant files, and the highest-value symbol bodies, selected by index search and edge ranking. Use it to start a task with one call instead of several exploratory reads. Read-only; requires an indexed repository.

**inputSchema**:

```json
{
  "type": "object",
  "properties": {
    "root": { "type": "string", "description": "Absolute repository root. Optional." },
    "task": { "type": "string", "minLength": 3, "description": "Natural-language task, e.g. 'fix session timeout'." },
    "max_tokens": { "type": "integer", "minimum": 100, "maximum": 100000, "default": 4000 }
  },
  "required": ["task"]
}
```

Default of 4000 mirrors `compression.target_tokens` (build-doc §17); an explicit `max_tokens` always wins.

**Result shape**:

```json
{
  "schema_version": 1,
  "operation_id": "01J2X3...",
  "task": "fix session timeout",
  "plan": {
    "map_excerpt": "src/\n  session.rs — SessionManager...",
    "outlines": [ { "path": "src/session.rs", "symbols": [ "...as in compresso_outline..." ] } ],
    "bodies": [ { "path": "src/session.rs", "symbol": "SessionManager::start", "content": "..." } ],
    "omitted": [ { "path": "src/auth/login.ts", "reason": "over budget", "token_estimate": 900 } ]
  },
  "tokens_used": 3910,
  "token_provider": "openai_o200k",
  "truncated": true
}
```

**Errors**: `not_indexed`, `invalid_arguments`. If search yields nothing relevant, success with an empty `bodies` list and a `map_excerpt` only, plus a note — never a guess.

**Tetris compatibility**: counterpart of `tetris_minimal_context`; not name-mapped in D7 but keeps the same budgeted-pack semantics.

### 1.9 compresso_impact

**Description (final copy)**: Reports what depends on a file: the reverse-edge closure (imports, references, tests) to depth 2, so you can see the blast radius of a change before making it. Returns direct dependents, transitive dependents, and test files that exercise the target. Read-only; requires an indexed repository.

**inputSchema**:

```json
{
  "type": "object",
  "properties": {
    "root": { "type": "string", "description": "Absolute repository root. Optional." },
    "path": { "type": "string", "minLength": 1, "description": "Repo-relative file path to analyze." },
    "max_tokens": { "type": "integer", "minimum": 100, "maximum": 100000, "default": 1500 }
  },
  "required": ["path"]
}
```

**Result shape**:

```json
{
  "schema_version": 1,
  "operation_id": "01J2X3...",
  "path": "src/session.rs",
  "direct_dependents": [ { "path": "src/server.rs", "kind": "imports" } ],
  "transitive_dependents": [ { "path": "src/main.rs", "via": "src/server.rs", "depth": 2 } ],
  "tests": [ { "path": "tests/session_test.rs" } ],
  "dependents_total": 9,
  "stale": false,
  "tokens_used": 380,
  "token_provider": "openai_o200k",
  "truncated": false
}
```

**Errors**: `not_indexed`, `outside_root`, `not_found`. A file with no dependents is a success with empty arrays.

**Tetris compatibility**: counterpart of Tetris impact checking (`tetris_session_insights` impact surface); no direct name mapping in D7.

### 1.10 compresso_compress

**Description (final copy)**: Compresses arbitrary text (logs, transcripts, search dumps, tool output) using Compresso's safe, deterministic strategy pipeline, and returns the compressed string with before/after token counts per provider. Safe mode is deterministic and rule-based (dedup, log collapsing, structured compaction, budget packing): removals are whole units, every removal is enumerated in the operation trace, protected content is never altered, and any failure returns the original text unchanged. It is not lossless; request passthrough mode for byte-exact needs. Use before passing long text to a sub-agent or follow-up prompt. Does not require an indexed repository.

**inputSchema**:

```json
{
  "type": "object",
  "properties": {
    "text": { "type": "string", "minLength": 1, "description": "Raw text to compress." },
    "mode": { "enum": ["safe", "passthrough"], "default": "safe", "description": "Slice supports safe and passthrough (D10)." },
    "max_tokens": { "type": "integer", "minimum": 100, "maximum": 100000, "description": "Optional target budget; when set, pack_budget trims lowest-priority content to fit." },
    "max_latency_ms": { "type": "integer", "minimum": 1, "maximum": 10000, "default": 200 }
  },
  "required": ["text"]
}
```

**Result shape**:

```json
{
  "schema_version": 1,
  "operation_id": "01J2X3...",
  "compressed": "...compressed text...",
  "before_tokens": { "openai_o200k": 8140, "anthropic_calibrated": 8790 },
  "after_tokens": { "openai_o200k": 4630, "anthropic_calibrated": 5010 },
  "net_tokens_saved": { "openai_o200k": 3510, "anthropic_calibrated": 3780 },
  "strategies_applied": ["dedup_exact", "collapse_logs", "compact_json"],
  "fallback_used": false,
  "fallback_reason": null,
  "duration_ms": 41
}
```

Counts are reported for both `openai_o200k` (exact) and `anthropic_calibrated` (o200k × calibration factor, D14) so clients can attribute savings to their own provider.

**Errors**: `invalid_arguments` (empty text) only. Compression failures, safety-check failures, and latency overruns are not errors: the engine fails open (D38) and returns the original text with `fallback_used: true` and a reason. Input larger than the engine's body limit (8 MiB) returns the original text with `fallback_used: true`, `fallback_reason: "input_too_large"`.

**Tetris compatibility (D7)**: replaces `tetris_compress_text`. Accepts raw text, returns compressed string plus before/after token counts per provider — the same contract, with real-tokenizer counts.

### 1.11 compresso_stats

**Description (final copy)**: Returns Compresso's local savings summary: operations recorded, observed vs applied tokens, net tokens saved, and modeled cost saved, using the fixed savings vocabulary. All figures come from the local SQLite store; nothing is transmitted anywhere. Optional filters narrow by time window or session.

**inputSchema**:

```json
{
  "type": "object",
  "properties": {
    "since": { "type": "string", "description": "ISO 8601 timestamp or relative duration ('24h', '7d', '30d'). Default: all retained history (90 days)." },
    "session_id": { "type": "string", "description": "Restrict to one session (ulid)." }
  },
  "required": []
}
```

**Result shape** (savings vocabulary from 02-schemas §3):

```json
{
  "schema_version": 1,
  "operation_id": "01J2X3...",
  "window": { "since": "2026-07-14T00:00:00Z", "until": "2026-07-21T09:00:00Z" },
  "operations": 312,
  "observed_tokens": 1240000,
  "applied_tokens": 861000,
  "removed_tokens": 379000,
  "compression_overhead": 12400,
  "net_tokens_saved": 366600,
  "modeled_cost_saved": { "amount_usd": 4.12, "label": "modeled" },
  "fallback_rate": 0.02,
  "by_tool": [ { "tool": "compresso_read", "operations": 120, "net_tokens_saved": 141000 } ]
}
```

**Errors**: `invalid_arguments` (unparseable `since`). An empty store is a success with zeros. `modeled_cost_saved` is always labelled `modeled`; `provider_verified_cost_saved` is post-slice and never fabricated (D6: local-only, no telemetry).

**Tetris compatibility**: counterpart of `tetris stats` / `tetris_session_insights`; no argument mapping required by D7.

### 1.12 compresso_explain

**Description (final copy)**: Returns the full compression trace for one past operation: mode, strategies attempted and applied, per-strategy token deltas, safety-check outcomes, fallback status, and a human-readable explanation. Traces contain no raw content — only identifiers, kinds, counts, and explanations. Use it to understand exactly what Compresso did to a given result.

**inputSchema**:

```json
{
  "type": "object",
  "properties": {
    "operation_id": { "type": "string", "minLength": 1, "description": "ulid returned by any Compresso tool result." }
  },
  "required": ["operation_id"]
}
```

**Result shape** (compression-trace v1, 02-schemas §2):

```json
{
  "schema_version": 1,
  "operation_id": "01J2X3...",
  "tool": "compresso_compress",
  "trace": {
    "schema_version": 1,
    "operation_id": "01J2X3...",
    "mode": "safe",
    "before_tokens": 8140,
    "after_tokens": 4630,
    "token_provider": "openai_o200k",
    "fallback_used": false,
    "fallback_reason": null,
    "strategy_results": [
      { "strategy_id": "collapse_logs", "applied": true, "before_tokens": 8140, "after_tokens": 5920, "duration_ms": 11.2, "confidence": 0.98, "notes": ["collapsed 214 repeated log lines"], "skipped_reason": null }
    ],
    "safety_checks": [ { "name": "protected_unchanged", "passed": true, "detail": "" } ],
    "explanation": "Collapsed repeated log lines and minified embedded JSON; all safety checks passed.",
    "created_at": 1784600000
  }
}
```

**Errors**: `not_found` (unknown or pruned operation_id — message notes the 90-day retention), `invalid_arguments`. This tool records an operation row like every other, with `observed_tokens = applied_tokens` (no compression applied).

**Tetris compatibility**: no Tetris equivalent; new surface.

## 2. Client integrations

Three adapters ship in the slice: Claude Code, Codex, Cursor (D4). **Claude Desktop is explicitly excluded (D5)**: no detection, no bundle staging, no config writes, no adapter stub. Each adapter implements the `Integration` trait (03-interfaces §6) and follows the invariants in owner hard constraint 5: preview + timestamped backup + key-level receipt + reversible uninstall.

### 2.1 Shared mechanics (all adapters)

- **Server entry Compresso writes**: `command` is the absolute, resolved path to the installed binary (default `~/.local/bin/compresso` expanded at install time, D23); `args` is exactly `["mcp", "serve"]`. No env vars are written; configuration comes from `~/.config/compresso/config.toml`.
- **Backup**: before any write, the current config file is copied byte-for-byte to `<config>.compresso-backup-<ISO timestamp>`, where the timestamp is UTC with colons replaced by hyphens for filesystem safety: `date -u +%Y-%m-%dT%H-%M-%SZ`. Example: `~/.claude.json.compresso-backup-2026-07-21T09-15-42Z`. Backups are never overwritten or pruned automatically. If the config file does not exist, no backup is created and the receipt records `backup_path` as the sentinel string `"(none: file created)"`.
- **Plan preview**: `plan_install` produces a unified diff (`diff -u` format) of the exact bytes to be written, headed by the config path, plus a one-line summary of the owned keys and the backup path that will be used. `compresso integrate <client>` prints the diff and requires interactive confirmation (or `--yes`); the local API two-step (`plan_hash` then approve, 03-interfaces §7) hashes this exact diff.
- **Write discipline**: parse existing config with a format-preserving parser (`toml_edit` for TOML; JSON written with 2-space indentation preserving key order of untouched keys). Modify only the owned keys. Write atomically: temp file in the same directory, fsync, rename.
- **Uninstall semantics**: remove only the Compresso-owned key recorded in the receipt. If the user has edited values inside the owned key (e.g. changed `args`), still remove the whole key but print exactly what was removed (the serialized subtree) so the user can restore their edit manually. Never touch sibling keys, comments outside the owned key (TOML), or unrelated formatting. Uninstall takes a fresh backup with the same convention before writing, then sets `uninstalled_at` on the receipt. Restoring the pre-install backup wholesale is never done automatically (it would revert unrelated user edits made since install).
- **Receipt**: one row in `integration_receipts` (02-schemas §4) per successful install:

| Receipt field | Value |
|---|---|
| `client` | `claude-code` \| `codex` \| `cursor` |
| `config_path` | absolute path of the file written |
| `keys_owned` | JSON array of exact key paths written (see per-client below) |
| `backup_path` | backup file created before the write (or `"(none: file created)"`) |
| `installed_at` | unix seconds |
| `compresso_version` | semver of the installing binary |
| `uninstalled_at` | NULL until uninstall |

### 2.2 Claude Code

- **Detection**: `claude` binary resolvable on `PATH` (equivalent of `command -v claude`); version check `claude --version` (must parse a semver; any parseable version is accepted in the slice, with the version recorded in the detection report). Config: user scope, `~/.claude.json`, key `mcpServers`. Project scope (`Scope::Project`) writes `<project>/.mcp.json` with the same `mcpServers` shape; user scope is the default.
- **Snippet Compresso writes** (merged into existing JSON; sibling keys untouched):

```json
{
  "mcpServers": {
    "compresso": {
      "type": "stdio",
      "command": "/Users/dev/.local/bin/compresso",
      "args": ["mcp", "serve"]
    }
  }
}
```

- **Owned keys**: exactly `mcpServers.compresso` (the whole object). `keys_owned = ["mcpServers.compresso"]`. Compresso creates `mcpServers` if absent but does not own it — uninstall removes `mcpServers.compresso` and leaves an empty `mcpServers` object in place if no siblings remain.
- **Backup**: `~/.claude.json.compresso-backup-<ISO timestamp>`.
- **Plan preview**: unified diff of `~/.claude.json`, e.g.:

```diff
--- ~/.claude.json (current)
+++ ~/.claude.json (after compresso integrate claude-code)
@@
   "mcpServers": {
+    "compresso": {
+      "type": "stdio",
+      "command": "/Users/dev/.local/bin/compresso",
+      "args": ["mcp", "serve"]
+    },
     "other-server": { ... }
```

- **Verification**: (1) `~/.claude.json` re-parses as JSON and the `compresso` entry's `command` resolves to an existing executable file; (2) `claude mcp list` output contains `compresso`; (3) optional live probe: spawn `compresso mcp serve`, perform MCP `initialize`, confirm the ten tools are listed, terminate. Verification failures roll nothing back automatically; they are reported with the backup path so the user can decide.
- **Uninstall**: remove `mcpServers.compresso` only, per §2.1. Note: direct config writes are chosen over `claude mcp add` so backup/diff/receipt guarantees hold uniformly across clients; `claude mcp list` remains the verification oracle.

### 2.3 Codex

- **Detection**: `codex` binary resolvable on `PATH`; version check `codex --version`. Config: `~/.codex/config.toml`, table `[mcp_servers.compresso]`. (Codex Desktop reads the same file, so one write covers CLI and Desktop, D4.)
- **Snippet Compresso writes** (via `toml_edit`, preserving all other tables and comments):

```toml
[mcp_servers.compresso]
command = "/Users/dev/.local/bin/compresso"
args = ["mcp", "serve"]
```

- **Owned keys**: exactly the `mcp_servers.compresso` table. `keys_owned = ["mcp_servers.compresso"]`. The parent `[mcp_servers]` namespace is not owned.
- **Backup**: `~/.codex/config.toml.compresso-backup-<ISO timestamp>`.
- **Plan preview**: unified diff of `~/.codex/config.toml` showing the added table (and nothing else changing).
- **Verification**: (1) `~/.codex/config.toml` re-parses as valid TOML and `mcp_servers.compresso.command` resolves to an existing executable; (2) `codex mcp list` shows `compresso` where the installed Codex version supports the `mcp` subcommand — otherwise the TOML re-parse plus a live `initialize` probe of `compresso mcp serve` stands in; (3) detection report records which verification path ran.
- **Uninstall**: delete the `[mcp_servers.compresso]` table only, printing its serialized contents if they differ from what the receipt says Compresso wrote. Comments and sibling tables (including other `[mcp_servers.*]` entries) are preserved byte-for-byte by `toml_edit`.

### 2.4 Cursor

- **Detection**: `cursor` command resolvable on `PATH`, or the application present at `/Applications/Cursor.app` (macOS; platform-native equivalents elsewhere); version check `cursor --version` (falls back to reading the app bundle version when only the app is present). Config: `~/.cursor/mcp.json`, key `mcpServers`. Project scope writes `<project>/.cursor/mcp.json` with the same shape.
- **Snippet Compresso writes**:

```json
{
  "mcpServers": {
    "compresso": {
      "command": "/Users/dev/.local/bin/compresso",
      "args": ["mcp", "serve"]
    }
  }
}
```

- **Owned keys**: exactly `mcpServers.compresso`. `keys_owned = ["mcpServers.compresso"]`. If `~/.cursor/mcp.json` does not exist, Compresso creates it containing only the object above (receipt records the created-file sentinel for `backup_path`).
- **Backup**: `~/.cursor/mcp.json.compresso-backup-<ISO timestamp>`.
- **Plan preview**: unified diff of `~/.cursor/mcp.json` (or a full-file "new file" diff when creating it).
- **Verification**: (1) `~/.cursor/mcp.json` re-parses as JSON and `command` resolves to an existing executable; (2) live `initialize` probe of `compresso mcp serve` confirming the tool list; (3) Cursor exposes no CLI to enumerate MCP servers, so the verification report instructs the user to confirm `compresso` appears under Cursor Settings → MCP after a restart. Steps 1–2 are automatic; step 3 is a printed instruction, not a gate.
- **Uninstall**: remove `mcpServers.compresso` only, per §2.1. If Compresso created the file and `compresso` is the sole remaining key, the file is left as `{ "mcpServers": {} }` rather than deleted (Cursor tolerates the empty object; deleting a file the user may have since edited is out of bounds).

### 2.5 Explicit non-goals

- **Claude Desktop**: no adapter, no detection, no writes to `~/Library/Application Support/Claude/claude_desktop_config.json` under any code path (D5). The CLI responds to `compresso integrate claude-desktop` with a refusal citing this decision.
- No hooks, wrappers, or PATH shims are installed for any client in the slice; the integration surface is exactly one MCP server entry per client.
- No automatic migration of Tetris client configs. Tetris entries are never read, modified, or removed by Compresso adapters (D2/D3); users remove Tetris separately.
