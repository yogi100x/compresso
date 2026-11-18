# 02 — Versioned Schemas

All schemas versioned by integer `schema_version`. Breaking change → new version file under `schemas/<name>/v<N>.json`; readers accept declared versions only, unknown version → fail open (passthrough) never crash. JSON Schemas below are authoritative; Rust types in `03-interfaces.md` mirror them and round-trip via serde (tested).

## 1. ContextGraph v1 (`schemas/context-graph/v1.json`)

```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "$id": "https://compresso.dev/schemas/context-graph/v1.json",
  "type": "object",
  "required": ["schema_version", "nodes", "edges"],
  "properties": {
    "schema_version": { "const": 1 },
    "nodes": { "type": "array", "items": { "$ref": "#/$defs/node" } },
    "edges": { "type": "array", "items": { "$ref": "#/$defs/edge" } }
  },
  "$defs": {
    "node": {
      "type": "object",
      "required": ["id", "kind", "content", "source", "token_count", "priority", "protected"],
      "properties": {
        "id": { "type": "string", "minLength": 1 },
        "kind": { "enum": [
          "system_instruction", "user_instruction", "assistant_response", "assistant_reasoning",
          "tool_definition", "tool_request", "tool_response",
          "source_file", "code_symbol", "search_result",
          "compiler_output", "test_output", "repo_map",
          "summary", "decision", "memory_fact" ] },
        "content": { "type": "string" },
        "source": { "$ref": "#/$defs/source" },
        "token_count": { "type": "integer", "minimum": 0 },
        "token_provider": { "enum": ["openai_o200k", "openai_cl100k", "anthropic_calibrated", "heuristic"], "default": "openai_o200k" },
        "priority": { "type": "number", "minimum": 0, "maximum": 1 },
        "protected": { "type": "boolean" },
        "recency": { "type": ["integer", "null"], "minimum": 0, "description": "unix seconds" },
        "metadata": { "type": "object" },
        "provider_turn": { "oneOf": [{ "$ref": "#/$defs/provider_turn" }, { "type": "null" }] }
      }
    },
    "provider_turn": {
      "type": "object",
      "additionalProperties": false,
      "required": ["provider", "turn_id", "tool_call_id"],
      "properties": {
        "provider": { "type": "string", "minLength": 1, "description": "typed provider profile identifier" },
        "turn_id": { "type": "string", "minLength": 1, "description": "typed provider turn identifier" },
        "tool_call_id": { "type": "string", "minLength": 1, "description": "typed provider tool-call identifier" },
        "opaque_state_hash": { "type": ["string", "null"] }
      }
    },
    "source": {
      "oneOf": [
        { "type": "object", "required": ["type"], "properties": { "type": { "const": "client" } } },
        { "type": "object", "required": ["type", "path"], "properties": { "type": { "const": "repository" }, "path": { "type": "string" } } },
        { "type": "object", "required": ["type", "name"], "properties": { "type": { "const": "tool" }, "name": { "type": "string" } } },
        { "type": "object", "required": ["type"], "properties": { "type": { "const": "synthetic" } } }
      ]
    },
    "edge": {
      "type": "object",
      "required": ["from", "to", "kind", "weight"],
      "properties": {
        "from": { "type": "string" },
        "to": { "type": "string" },
        "kind": { "enum": ["imports", "calls", "defines", "references", "tests", "supersedes", "produced_by_tool", "responds_to", "depends_on", "same_error_group", "duplicate_of"] },
        "weight": { "type": "number", "minimum": 0, "maximum": 1 }
      }
    }
  }
}
```

Invariants (validated in code, not expressible in JSON Schema): node ids unique; edge endpoints exist; every node's `token_count` equals a recount by its declared provider; and `protected: true` is forced at ingress for `kind ∈ {system_instruction, user_instruction, assistant_reasoning, tool_definition, tool_request}`. Provider reasoning and opaque continuation state are never compressed, persisted as memory, or moved between providers. Reasoning and in-flight tool requests cannot be unprotected.

Provider conversations are ordered atomic turns. Every `tool_request` has `provider`, `turn_id`, and `tool_call_id` metadata and exactly one owning assistant turn. Every `tool_response` carries the same identifiers and a `responds_to` edge. A candidate may retain a complete pair or remove a complete pair only when no later provider turn depends on it; orphan responses, pending requests, and results without their owning assistant turn are invalid. Unknown provider state forces passthrough.

Deduplication references use a `duplicate_of` edge from stub to byte-preserved referent. A stub without exactly one surviving referent is invalid, and budget packing treats a referent plus all stubs as one atomic group.

D53 extends the build source's original safety list: unknown provider/client conversation format is a provider-conversation validation failure and therefore byte-identical passthrough. The source document remains unchanged; this paragraph is the authoritative plan delta.

## 2. CompressionBudget / Result / Trace v1 (`schemas/compression-trace/v1.json`)

```json
{
  "$id": "https://compresso.dev/schemas/compression-trace/v1.json",
  "type": "object",
  "$ref": "#/$defs/trace",
  "$defs": {
    "budget": {
      "type": "object",
      "required": ["max_tokens", "max_latency_ms", "allowed_lossiness"],
      "properties": {
        "max_tokens": { "type": "integer", "minimum": 1 },
        "max_latency_ms": { "type": "integer", "minimum": 1, "default": 200 },
        "allowed_lossiness": { "enum": ["none", "structural", "summarizing"] },
        "protected_kinds": { "type": "array", "items": { "type": "string" } }
      }
    },
    "removal_record": {
      "type": "object",
      "additionalProperties": false,
      "required": ["strategy_id", "node_id", "kind", "original_hash", "before_tokens", "after_tokens"],
      "properties": {
        "strategy_id": { "type": "string" },
        "node_id": { "type": "string" },
        "kind": { "enum": ["duplicate_reference", "collapsed_lines", "json_elision", "dropped_node"] },
        "locator": { "type": ["string", "null"], "description": "line span or JSON pointer; never raw content" },
        "original_hash": { "type": "string" },
        "replacement_hash": { "type": ["string", "null"] },
        "before_tokens": { "type": "integer", "minimum": 0 },
        "after_tokens": { "type": "integer", "minimum": 0 },
        "units_removed": { "type": ["integer", "null"], "minimum": 0 }
      }
    },
    "strategy_result": {
      "type": "object",
      "required": ["strategy_id", "applied", "before_tokens", "after_tokens", "duration_ms", "removals"],
      "properties": {
        "strategy_id": { "type": "string" },
        "applied": { "type": "boolean" },
        "before_tokens": { "type": "integer" },
        "after_tokens": { "type": "integer" },
        "duration_ms": { "type": "number" },
        "confidence": { "type": "number", "minimum": 0, "maximum": 1 },
        "notes": { "type": "array", "items": { "type": "string" } },
        "removals": { "type": "array", "items": { "$ref": "#/$defs/removal_record" } },
        "skipped_reason": { "type": ["string", "null"] }
      }
    },
    "safety_check": {
      "type": "object",
      "required": ["name", "passed"],
      "properties": { "name": { "type": "string" }, "passed": { "type": "boolean" }, "detail": { "type": "string" } }
    },
    "trace": {
      "type": "object",
      "required": ["schema_version", "operation_id", "mode", "before_tokens", "after_tokens", "fallback_used", "strategy_results", "safety_checks", "explanation"],
      "properties": {
        "schema_version": { "const": 1 },
        "operation_id": { "type": "string", "description": "ulid" },
        "session_id": { "type": ["string", "null"] },
        "mode": { "enum": ["safe", "balanced", "aggressive", "passthrough"] },
        "before_tokens": { "type": "integer" },
        "after_tokens": { "type": "integer" },
        "token_provider": { "type": "string" },
        "fallback_used": { "type": "boolean" },
        "fallback_reason": { "type": ["string", "null"] },
        "strategy_results": { "type": "array", "items": { "$ref": "#/$defs/strategy_result" } },
        "safety_checks": { "type": "array", "items": { "$ref": "#/$defs/safety_check" } },
        "explanation": { "type": "string", "description": "human-readable, no raw content" },
        "created_at": { "type": "integer" }
      }
    }
  }
}
```

Traces never contain raw node content — only ids, kinds, counts, and human-readable explanations (privacy: build-doc §16/§21).

## 3. Savings metrics vocabulary (used in `operations`, `compresso stats`, dashboards)

```text
observed_tokens            tokens seen entering Compresso
applied_tokens             tokens actually delivered after applied compression
removed_tokens             observed - applied, only when compression was applied
compression_overhead       tokens added by Compresso itself (maps, explanations)
net_tokens_saved           removed_tokens - compression_overhead
provider_observed_input_tokens  actual input/cache fields reported for the request sent
provider_observed_cost          actual cost from observed usage and a versioned price table
modeled_cost_saved              counterfactual only; never provider-verified
retrieval_pack_tokens           exact tokens returned by retrieval tools
estimated_raw_source_tokens     modeled raw-source baseline; never provider-observed
retrieval_tokens_avoided        saturating modeled baseline - exact pack tokens
follow_up_calls_avoided         modeled separate reads replaced by a ready pack
total_effective_savings         applied net compression + modeled retrieval avoidance
cost_basis                      model, priced flag, rate, price table version and effective date behind modeled_cost_saved
transcript_savings              the same vocabulary derived from a client transcript; CLI only, never added to ledger totals
```

Cost and transcript rules (E17-03, ADR-0029): `modeled_cost_saved` prices `total_effective_savings` (floored at zero) once at the base input rate of a caller-named model from the checked-in price table `rust/compresso-core/data/price-table-v1.json` (schema `schemas/price-table/v1.json`, carrying `schema_version`, `table_version`, and `effective_date`). Prices are never fetched and never guessed: a model absent from the table is reported with tokens only. `transcript_savings` counts each distinct `operation_id` once, prefers the ledger row when one exists, takes exact before/after counts for ledgerless compression (zero on fallback), and claims zero retrieval avoidance without a recorded baseline.

## 4. SQLite DDL v1 (`rust/compresso-store/migrations/0001_init.sql`)

```sql
PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;

CREATE TABLE schema_migrations (
  version INTEGER PRIMARY KEY,
  applied_at INTEGER NOT NULL
);

CREATE TABLE repositories (
  id INTEGER PRIMARY KEY,
  root TEXT NOT NULL UNIQUE,            -- canonicalized absolute path
  approved_at INTEGER NOT NULL,
  parser_version TEXT NOT NULL,
  schema_version INTEGER NOT NULL DEFAULT 1,
  last_indexed_at INTEGER
);

CREATE TABLE files (
  id INTEGER PRIMARY KEY,
  repo_id INTEGER NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
  path TEXT NOT NULL,                   -- repo-relative, forward slashes
  hash TEXT NOT NULL,                   -- blake3 hex
  language TEXT,                        -- rust|typescript|tsx|javascript|python|NULL
  size_bytes INTEGER NOT NULL,
  token_estimate INTEGER NOT NULL,
  indexed_at INTEGER NOT NULL,
  UNIQUE (repo_id, path)
);
CREATE INDEX idx_files_repo_lang ON files(repo_id, language);

CREATE TABLE symbols (
  id INTEGER PRIMARY KEY,
  file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
  name TEXT NOT NULL,
  qualified_name TEXT NOT NULL,         -- e.g. SessionManager::start
  kind TEXT NOT NULL,                   -- function|method|struct|class|enum|trait|interface|const|type|module
  signature TEXT,
  start_line INTEGER NOT NULL,          -- 1-based inclusive
  end_line INTEGER NOT NULL,
  token_estimate INTEGER NOT NULL
);
CREATE INDEX idx_symbols_file ON symbols(file_id);
CREATE INDEX idx_symbols_name ON symbols(name);

CREATE TABLE edges (
  id INTEGER PRIMARY KEY,
  repo_id INTEGER NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
  from_file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
  to_path TEXT NOT NULL,                -- import target as written, resolved when possible
  to_file_id INTEGER REFERENCES files(id) ON DELETE SET NULL,
  resolution_status TEXT NOT NULL DEFAULT 'unresolved' CHECK (resolution_status IN ('resolved','ambiguous','unresolved')),
  resolution_detail TEXT,
  kind TEXT NOT NULL                    -- imports|references|tests
);
CREATE INDEX idx_edges_from ON edges(from_file_id);
CREATE INDEX idx_edges_to ON edges(to_file_id);

CREATE VIRTUAL TABLE symbols_fts USING fts5(
  name, qualified_name, signature,
  content='symbols', content_rowid='id', tokenize='trigram'
);
CREATE TRIGGER symbols_ai AFTER INSERT ON symbols BEGIN
  INSERT INTO symbols_fts(rowid, name, qualified_name, signature)
  VALUES (new.id, new.name, new.qualified_name, new.signature);
END;
CREATE TRIGGER symbols_ad AFTER DELETE ON symbols BEGIN
  INSERT INTO symbols_fts(symbols_fts, rowid, name, qualified_name, signature)
  VALUES ('delete', old.id, old.name, old.qualified_name, old.signature);
END;

CREATE TABLE sessions (
  id TEXT PRIMARY KEY,                  -- ulid
  client TEXT NOT NULL,                 -- claude-code|codex|cursor|cli|unknown
  transport TEXT NOT NULL,              -- stdio|http
  started_at INTEGER NOT NULL,
  ended_at INTEGER
);

CREATE TABLE operations (
  id TEXT PRIMARY KEY,                  -- ulid = trace.operation_id
  session_id TEXT REFERENCES sessions(id),
  tool TEXT NOT NULL,                   -- compresso_read|compresso_compress|...
  mode TEXT NOT NULL,
  observed_tokens INTEGER NOT NULL,
  applied_tokens INTEGER NOT NULL,
  compression_overhead INTEGER NOT NULL DEFAULT 0,
  fallback_used INTEGER NOT NULL DEFAULT 0,
  tokens_served INTEGER,                -- retrieval tools: tokens returned to the client
  trace_json TEXT,                      -- compression-trace v1, no raw content; NULL for retrieval-only operations
  created_at INTEGER NOT NULL
);
CREATE INDEX idx_operations_session ON operations(session_id, created_at);

CREATE TABLE strategy_results (
  id INTEGER PRIMARY KEY,
  operation_id TEXT NOT NULL REFERENCES operations(id) ON DELETE CASCADE,
  strategy_id TEXT NOT NULL,
  applied INTEGER NOT NULL,
  before_tokens INTEGER NOT NULL,
  after_tokens INTEGER NOT NULL,
  duration_ms REAL NOT NULL
);

CREATE TABLE memory_facts (
  id TEXT PRIMARY KEY,
  scope TEXT NOT NULL,                  -- global|repo:<root-hash>
  key TEXT NOT NULL,
  content TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  UNIQUE (scope, key)
);

CREATE TABLE integration_receipts (
  id INTEGER PRIMARY KEY,
  client TEXT NOT NULL,                 -- non-authoritative metadata projection; claude-code|codex|cursor
  config_path TEXT NOT NULL,
  keys_owned TEXT NOT NULL,             -- JSON array of exact keys written
  backup_path TEXT NOT NULL,
  installed_at INTEGER NOT NULL,
  compresso_version TEXT NOT NULL,
  uninstalled_at INTEGER
);

CREATE TABLE index_versions (
  repo_id INTEGER NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
  parser_version TEXT NOT NULL,
  schema_version INTEGER NOT NULL,
  created_at INTEGER NOT NULL
);

CREATE TABLE update_state (               -- used by updater, Phase 8
  id INTEGER PRIMARY KEY CHECK (id = 1),
  current_version TEXT NOT NULL,
  previous_version TEXT,
  channel TEXT NOT NULL DEFAULT 'stable',
  last_check_at INTEGER
);
```

The v1 `files.language` column is intentionally unconstrained `TEXT`. D68 adds
the canonical values `shell` and `text` without rewriting the already-applied
`0001` migration or requiring a schema migration.

Migration `0006_retrieval_metrics.sql` adds nullable `repository_id`,
`estimated_raw_source_tokens`, and `modeled_follow_up_calls_avoided` columns to
`operations`. `tokens_served` remains the exact retrieval-pack count. Null
historical baselines produce zero modeled retrieval avoidance (D69).
Migration `0007_reset_unqualified_retrieval_baselines.sql` conservatively
resets baselines written by the initial whole-file estimator; corrected
matched-evidence estimates are recorded only after that migration.
Migration `0008_call_sites.sql` (D89, ADR-0033) adds the call graph:

```sql
CREATE TABLE call_sites (
  id INTEGER PRIMARY KEY,
  repo_id INTEGER NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
  from_file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
  caller_symbol_id INTEGER NOT NULL REFERENCES symbols(id) ON DELETE CASCADE,
  callee_name TEXT NOT NULL,            -- as written, identifier characters, <= 256 bytes
  callee_qualifier TEXT,                -- Store in Store::open(), os.path in os.path.join(), self/Self/this/cls
  call_form TEXT NOT NULL CHECK (call_form IN ('plain','path','self','receiver','reference')), -- reference: a bare name passed as a call argument
  line INTEGER NOT NULL CHECK (line >= 1)  -- 1-based line of the callee name
);
CREATE INDEX idx_call_sites_caller ON call_sites(caller_symbol_id);
CREATE INDEX idx_call_sites_callee ON call_sites(repo_id, callee_name);
CREATE INDEX idx_call_sites_file ON call_sites(from_file_id);
```

Rows are written in the caller file's `replace_indexed_file` transaction and
never name a callee symbol id; the callee is resolved at query time
(`compresso_trace`, 04 §`compresso_trace`). Indexes stamped before the
`+calls-v1` parser-version suffix hold no rows and are reparsed once.

`integration_receipts` is a query/reporting projection, not the E6 safety
receipt. Migration identifiers `0002`–`0005` are reserved by D60, and this
`0001` table has no lossless fields for `plan_hash` or per-key
`owned_value_hashes`. The keyed-MAC `FileReceiptStore` per-plan envelope and
its independently authenticated active-head pointer are canonical for integration restore,
modified-key comparison, ordering, and idempotence. SQLite MUST NOT authorize
uninstall or select an active receipt. The CLI writes/repairs this projection
only after the canonical file receipt is durable; it may reconcile missing or
stale rows from those records. Config replacement, canonical receipt
persistence, and projection are not claimed to be one atomic transaction; the
authenticated integration journal owns crash recovery (04-mcp-and-integrations
§2.1).

Rules: raw prompts and full source content never stored. Operations come in two kinds: **compression operations** (`compresso_compress`, engine runs) must carry `trace_json`, validated against the trace v1 **root** schema before insert; **retrieval operations** (map, search, read, outline, unfold, context, impact, stats, explain) store `trace_json = NULL` and set `tokens_served`. Retention: operations older than 90 days pruned on startup (configurable).

```sql
-- 0002_entitlements.sql (local side of ADR-0015; implemented in E10-local tickets)
CREATE TABLE entitlement_cache (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  bundle_jws TEXT NOT NULL,             -- signed, device-bound machine lease; verified before every paid use
  key_id TEXT NOT NULL,                 -- signing key id (kid) used, for rotation
  lease_class TEXT NOT NULL CHECK (lease_class IN ('standard', 'enterprise_offline')),
  fetched_at INTEGER NOT NULL,          -- local monotonic anchor paired with signed server issued_at
  refresh_after INTEGER NOT NULL,       -- scheduled remote renewal start (normally issued_at + 14d)
  expires_at INTEGER NOT NULL,          -- hard lease end (normally issued_at + 21d)
  CHECK (fetched_at < refresh_after AND refresh_after < expires_at)
);
```

## 5. PostgreSQL DDL v1 (Phase 6; `infra/` + drizzle schema mirrors this)

```sql
CREATE TABLE users (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  email TEXT NOT NULL UNIQUE,
  name TEXT,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now()
  -- credentials/sessions managed by Better Auth tables (generated)
);

CREATE TABLE organizations (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  name TEXT NOT NULL,
  slug TEXT NOT NULL UNIQUE,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE organization_members (
  org_id UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
  user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  role TEXT NOT NULL CHECK (role IN ('owner','admin','member')),
  PRIMARY KEY (org_id, user_id)
);

CREATE TABLE plans (
  id TEXT PRIMARY KEY,                  -- local|pro|team|enterprise
  stripe_price_id TEXT,
  features JSONB NOT NULL
);

CREATE TABLE subscriptions (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  owner_user_id UUID REFERENCES users(id),
  owner_org_id UUID REFERENCES organizations(id),
  plan_id TEXT NOT NULL REFERENCES plans(id),
  stripe_subscription_id TEXT UNIQUE,
  status TEXT NOT NULL,                 -- active|past_due|canceled|trialing
  seats INTEGER NOT NULL DEFAULT 1,
  current_period_end TIMESTAMPTZ,
  CHECK (num_nonnulls(owner_user_id, owner_org_id) = 1)
);

CREATE TABLE entitlements (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  subscription_id UUID NOT NULL REFERENCES subscriptions(id) ON DELETE CASCADE,
  feature TEXT NOT NULL,
  value JSONB NOT NULL,
  UNIQUE (subscription_id, feature)
);

CREATE TABLE devices (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  name TEXT NOT NULL,                   -- user-chosen, never hostname by default
  platform TEXT NOT NULL,
  public_key TEXT NOT NULL,             -- device-held keypair, private key in OS keychain
  registered_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  revoked_at TIMESTAMPTZ
);

CREATE TABLE device_sessions (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  device_id UUID NOT NULL REFERENCES devices(id) ON DELETE CASCADE,
  refresh_token_hash TEXT NOT NULL,
  expires_at TIMESTAMPTZ NOT NULL,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE release_channels (
  id TEXT PRIMARY KEY                   -- stable|beta|nightly|enterprise
);

CREATE TABLE releases (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  version TEXT NOT NULL UNIQUE,
  channel TEXT NOT NULL REFERENCES release_channels(id),
  manifest_url TEXT NOT NULL,
  manifest_signature TEXT NOT NULL,
  published_at TIMESTAMPTZ,
  yanked_at TIMESTAMPTZ
);

CREATE TABLE feature_flags (
  key TEXT PRIMARY KEY,
  default_value JSONB NOT NULL,
  rules JSONB NOT NULL DEFAULT '[]'
);

CREATE TABLE policy_sets (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  org_id UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
  name TEXT NOT NULL,
  policy JSONB NOT NULL,                -- mode floors, strategy allowlist, privacy floors
  updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE event_batches (              -- opt-in aggregate only; NOT built in slice (D6)
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  device_id UUID NOT NULL REFERENCES devices(id),
  period_start TIMESTAMPTZ NOT NULL,
  period_end TIMESTAMPTZ NOT NULL,
  aggregates JSONB NOT NULL,            -- counts only; schema-allowlisted
  received_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE audit_logs (
  id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  actor_user_id UUID,
  actor_type TEXT NOT NULL,             -- user|admin|system
  action TEXT NOT NULL,
  subject TEXT NOT NULL,
  detail JSONB NOT NULL DEFAULT '{}',
  created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
-- append-only: REVOKE UPDATE, DELETE ON audit_logs FROM app_role;

CREATE TABLE support_cases (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  user_id UUID REFERENCES users(id),
  status TEXT NOT NULL DEFAULT 'open',
  subject TEXT NOT NULL,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
```

Privacy invariant — enforced at **content level**, not column names: every JSONB field (`plans.features`, `entitlements.value`, `feature_flags.default_value`/`rules`, `policy_sets.policy`, `event_batches.aggregates`, `audit_logs.detail`, `subscriptions` metadata) has a versioned allowlist JSON Schema under `schemas/cloud-api/` with `additionalProperties: false`, validated in Fastify middleware on every write; requests failing validation are rejected and audited. `audit_logs.detail` is restricted to an enumerated key set (ids, enum values, counts — no free-form strings from clients). Enforcement stack: (1) write-path schema validation, (2) property tests generating adversarial payloads containing paths/prompts/symbols and asserting rejection, (3) CI test asserting every JSONB column has a registered allowlist schema. A column-name check alone is explicitly insufficient.

## 6. Migration registry and E15 local schemas

Migration numbers are reserved centrally and never introduced conditionally inside a ticket:

| Number | Owner | Purpose |
|---|---|---|
| `0001` | E3 | Core local store |
| `0002` | E10 | Entitlements and device account metadata |
| `0003` | E13 | Provider state, capability evidence, and observed usage |
| `0004` | E15 | Advanced local capabilities |
| `0005` | E16-06 | Import-resolution status/detail migration |
| `0006` | E7/D69 | Retrieval savings metrics |
| `0007` | D69 | Reset unqualified retrieval baselines |
| `0008` | E17-24 | Call sites (D89) |

E15 adds strict boundary schemas under `schemas/search/v1.json`, `schemas/edit/v1.json`, `schemas/sql/v1.json`, `schemas/recall/v1.json`, `schemas/knowledge/v1.json`, and `schemas/review/v1.json`. Apply requests contain plan hashes and approvals, never credentials. Migration `0004_advanced_capabilities.sql` adds `edit_transactions`, `sql_connection_profiles`, `recall_sources`, `recall_chunks`, `knowledge_items`, `review_runs`, and `review_findings`. Recall, knowledge, rollback, and review content is encrypted with keychain-held keys; rows store ciphertext, hashes, provenance, states, and retention metadata. SQL profiles store only opaque keychain references. Migration `0003_model_runtime.sql` adds normalized E13 provider state and usage without prompts or reasoning. Exact DDL and invariants are owned by E15-01 and validated by E15-30 before release.
