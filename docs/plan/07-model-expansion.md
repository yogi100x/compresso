# 07 — Model & Client Expansion (E13)

Implementation spec for the architecture in `compresso-expand.md`, adopted by decision D45. This document translates that source into the plan's crate boundaries, interface style, and ticket vocabulary. Where the source document and this spec differ in a detail, this spec wins for implementation; where this spec is silent, the source document is authoritative. Owner hard constraints apply throughout: no remote telemetry (D6), no Claude Desktop (D5), source and prompt content never leave the device by default, and cloud failure never breaks the local assistant.

## 1. Position in the architecture

Compresso gains two independent extension surfaces. **Client adapters** (Claude Code, Codex, Cursor, native CLI, generic MCP) already exist as E5/E6 work and connect hosts to the runtime. **Provider adapters** connect the runtime to model APIs and are new. Between them sits a canonical model protocol that the core speaks exclusively — the runtime never branches on a provider name.

```text
Claude Code | Codex | Cursor | Native CLI          (E5/E6 — exists)
            |
     compresso-integrations / compresso-mcp        (client adapters)
            |
     compresso-agent                                (agent loop — E8 crate, extended)
     Atlas | memory | compression | policy | tools
            |
     compresso-model-protocol                       (canonical contracts — new)
            |
     compresso-router                               (eligibility, scoring, fallback — new)
            |
     compresso-providers                            (drivers + profiles + native adapters — new)
            |
Anthropic | OpenAI | Kimi | GLM | MiniMax | DeepSeek
```

### 1.1 New crates

Three crates are added to the `rust/` workspace; `compresso-agent` (existing E8 stub) absorbs the agent loop. The source document's suggested `crates/` layout (§24) maps onto our existing `rust/` convention as follows: `compresso-model-core` + `compresso-provider-api` + `compresso-tool-schema` collapse into `compresso-model-protocol`; `compresso-driver-openai` + `compresso-driver-anthropic` + `compresso-provider-native` collapse into `compresso-providers` (drivers as modules, native adapters as a deliberately small submodule); `compresso-router` and `compresso-agent-runtime` map to `compresso-router` and the existing `compresso-agent`.

Extension of the crate-boundary table from `03-interfaces.md` §1:

| Crate | Owner | Public surface | May depend on |
|-------|-------|----------------|----------------|
| compresso-model-protocol | R1 | request, message, event, error, usage, capability, provider_state, adapter trait, tool_schema | core (tokens only) |
| compresso-providers | R2 | drivers (openai_chat, anthropic_messages, local_openai), profiles (loader, registry, verify), native | model-protocol, core (tokens only) |
| compresso-router | R1 | eligibility, scoring, fallback, health | model-protocol |
| compresso-agent (extended) | R1/D1 | agent loop, session coordinator, budgets, provider-state custody | model-protocol, providers, router, core, atlas, store |

Dependency rules, extending `01-stack-and-workspace.md` §3: `model-protocol → core(tokens only)`; `providers → model-protocol`; `router → model-protocol` and nothing else internal (the router is a pure function of its inputs — health and evaluation data are passed in, never fetched); `agent → all of the above plus core/atlas/store`. No new crate may depend on `compresso-mcp`, and `compresso-core`, `compresso-atlas`, `compresso-store`, and `compresso-mcp` may not depend on any E13 crate — compression and indexing must keep working with zero providers configured.

**Network exception.** `01-stack` §1 forbids network-I/O crates outside `compresso-updater`. `compresso-providers` becomes the second sanctioned exception (`reqwest`, rustls), enforced in `deny.toml`: it is the only crate that opens outbound connections to model providers, and it does so only for endpoints the user explicitly configured against a signed host allowlist. There are no default cloud calls; a fresh install performs zero provider network I/O.

**Relationship to compresso-proxy (E11).** The localhost proxy and the drivers share wire-format knowledge (Anthropic Messages, OpenAI Chat Completions) but have opposite roles: the proxy passively intermediates a host client's own model traffic; the drivers actively originate requests when Compresso owns the agent loop. Shared payload types live in `compresso-providers` and the proxy consumes them; the proxy's passthrough guarantees (ADR-0014) are unchanged.

### 1.2 The profile-first rule

Verbatim from `compresso-expand.md` §1, restated as the E13 prime directive:

> **A compatible provider should normally be added with a model profile using an existing protocol driver. Provider-specific native code is reserved for behavior that cannot be represented safely by the existing drivers and profile schema.**

The decision test (§5.3 of the source) is mechanical:

| Question | If yes |
|---|---|
| Can an existing driver serialize the request correctly? | Prefer a profile |
| Can profile flags express the provider's capability restrictions? | Prefer a profile |
| Can generic opaque state preserve required round-trip data? | Prefer a profile |
| Does the provider require a different stream parser? | Consider a native adapter |
| Does the provider require custom authentication or signing? | Consider a native adapter |
| Does correct recovery require a provider-specific state machine? | Use a native adapter |

Native code requires an ADR explaining why a profile is insufficient (see ADR-0020). Growth of the `native` submodule is treated as a design smell: the correct response is usually to generalize the driver or profile schema.

## 2. Canonical model protocol (`compresso-model-protocol`)

The canonical protocol is an internal Compresso API, not a copy of any provider API. All types are `serde` round-trippable; anything persisted or crossing a process boundary carries `schema_version` and validates at the boundary (global constraint).

### 2.1 `request` module

```rust
pub struct ModelRequest {
    pub request_id: RequestId,            // random, generated locally
    pub model: ModelRef,                  // profile id + model id, never a bare provider string
    pub messages: Vec<Message>,
    pub tools: Vec<ToolDefinition>,       // canonical schema; compiled per-provider (§3)
    pub response_format: Option<ResponseFormat>,
    pub reasoning: ReasoningPolicy,
    pub sampling: SamplingOptions,
    pub budget: GenerationBudget,
    pub metadata: RequestMetadata,        // content-free: session id, task kind, policy tags
    pub provider_state: Option<ProviderState>,   // opaque, bounded (§4)
}

pub struct GenerationBudget {
    pub max_input_tokens: Option<u64>,
    pub max_output_tokens: Option<u64>,
    pub deadline_ms: Option<u64>,
    pub maximum_cost_micros: Option<u64>,
}

pub enum ReasoningPolicy { Disabled, Automatic, Enabled { effort: ReasoningEffort } }
```

### 2.2 `message` module

```rust
pub struct Message { pub role: MessageRole, pub parts: Vec<MessagePart> }
pub enum MessageRole { System, Developer, User, Assistant, Tool }
pub enum MessagePart {
    Text(String),
    Image(ImageReference),
    File(FileReference),
    ToolCall(ToolCall),
    ToolResult(ToolResult),
    ProviderState(ProviderState),
}
```

When a remote API lacks a canonical role, the driver's behavior is deterministic and declared by the profile: merge compatible instructions, reject the request with `UnsupportedCapability`, or let the router pick another model. Silently discarding instructions is forbidden and conformance-tested.

### 2.3 `event` module — streaming

```rust
pub enum ModelEvent {
    ResponseStarted { remote_request_id: Option<String> },
    TextDelta(String),
    ReasoningDelta(String),
    ToolCallStarted { id: String, name: String },
    ToolArgumentsDelta { id: String, json_fragment: String },
    ToolCallCompleted { id: String },
    ProviderStateUpdated(ProviderState),
    UsageUpdated(TokenUsage),
    ResponseCompleted { reason: FinishReason },
    ResponseFailed(ModelError),
}

pub type ModelEventStream = Pin<Box<dyn Stream<Item = ModelEvent> + Send>>;
```

Tool arguments may arrive as incomplete JSON fragments. Drivers accumulate fragments in bounded buffers (default 1 MiB per call, profile-tunable downward); the tool executor validates the completed object against the canonical schema before execution — never the fragments.

### 2.4 Finish reasons

```rust
pub enum FinishReason {
    Completed, ToolCall, OutputLimit, ContentFiltered,
    Cancelled, Timeout, Failed,
    Unknown(String),   // raw provider value retained for diagnostics, mapped to safe behavior
}
```

### 2.5 `usage` module

```rust
pub struct TokenUsage {
    pub input_tokens: Option<u64>,
    pub cached_input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub estimated_cost_micros: Option<u64>,
    pub source: UsageSource,
}
pub enum UsageSource { ProviderReported, CompressoEstimated, Reconciled }
```

Provider-reported usage is authoritative for cost; Compresso estimates (via the `TokenCounter` trait from `03-interfaces.md` §2.2, provider-tagged per D14) are used for preflight budgeting only and stay in the `CompressoEstimated` bucket — this is the same observed/applied/modeled separation the savings vocabulary already enforces.

### 2.6 `adapter` module — ProviderAdapter trait

```rust
pub trait ProviderAdapter: Send + Sync {
    fn provider_id(&self) -> &ProviderId;
    fn capabilities(&self, model: &ModelRef) -> Result<ModelCapabilities, ModelError>;
    fn validate_request(&self, request: &ModelRequest) -> Result<(), ModelError>;
    fn execute(&self, request: ModelRequest) -> ModelEventStream;
    fn estimate_tokens(&self, request: &ModelRequest) -> TokenEstimate;
    fn classify_error(&self, error: &ModelError) -> RetryDecision;
}
```

The trait is object-safe. Protocol drivers implement it directly (parameterized by a profile); native adapters either wrap a driver or implement it whole. The agent loop and router see only `dyn ProviderAdapter` — that is the enforcement point for "no provider names in the runtime" (ADR-0019), backed by a CI grep test that fails if any provider identifier appears in `compresso-agent`, `compresso-router`, `compresso-core`, `compresso-atlas`, or `compresso-store` source.

### 2.7 `capability` module — registry

Capabilities attach to a model within a profile, never globally to a provider.

```rust
pub struct ModelCapabilities {
    pub input_modalities: CapabilitySet<InputModality>,
    pub output_modalities: CapabilitySet<OutputModality>,
    pub context_window_tokens: u64,
    pub maximum_output_tokens: u64,
    pub streaming: Capability,
    pub tool_calls: Capability,
    pub parallel_tool_calls: Capability,
    pub tool_choice: CapabilitySet<ToolChoiceMode>,
    pub structured_output: Capability,
    pub reasoning: Capability,
    pub prompt_caching: Capability,
    pub provider_state_replay: bool,
    pub usage_reporting: UsageReportingCapability,
}

pub struct Capability { pub evidence: CapabilityEvidence, pub disabled_by_policy: bool }
pub enum CapabilityEvidence { Unknown, Declared, Verified, Degraded, Unsupported }
```

| Evidence | Meaning |
|---|---|
| `unknown` | No safe claim can be made |
| `declared` | In official provider documentation, not tested locally |
| `verified` | Passed the Compresso conformance suite (§8) |
| `degraded` | Supported with a documented limitation |
| `unsupported` | Confirmed unavailable |

`disabled_by_policy` is an orthogonal user/org override: a `verified` capability the user has switched off behaves as unavailable for routing but keeps its evidence. Routing uses only `verified` capabilities by default; opting into `declared` capabilities is an explicit per-route setting (`allow_declared_capabilities`, §5) and is surfaced in the UI.

### 2.8 Profile schema

Profiles are TOML on disk (human-authored, reviewable), parsed to a JSON value and validated against `schemas/model-profile/v1.json` with the `jsonschema` crate — the same boundary-validation pattern as context-graph and trace schemas (E1-04). Profiles contain no secrets; user configuration references keys by opaque identifier resolved through the OS keychain (§9). The illustrative TOML in `compresso-expand.md` §9 is the reference instance.

`schemas/model-profile/v1.json`:

```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "$id": "https://compresso.dev/schemas/model-profile/v1.json",
  "title": "Compresso model profile v1",
  "type": "object",
  "required": ["schema_version", "profile_version", "provider", "models"],
  "additionalProperties": false,
  "properties": {
    "schema_version": { "const": 1 },
    "profile_version": { "type": "string", "pattern": "^\\d{4}-\\d{2}-\\d{2}(\\.\\d+)?$" },
    "provider": {
      "type": "object",
      "required": ["id", "display_name", "driver", "approved_hosts", "default_base_url", "auth"],
      "additionalProperties": false,
      "properties": {
        "id": { "type": "string", "pattern": "^[a-z0-9][a-z0-9-]{1,63}$" },
        "display_name": { "type": "string", "maxLength": 120 },
        "driver": { "enum": ["openai-chat", "anthropic-messages", "local-openai"] },
        "approved_hosts": {
          "type": "array", "minItems": 1,
          "items": { "type": "string", "format": "hostname" }
        },
        "default_base_url": { "type": "string", "pattern": "^https://" },
        "auth": { "enum": ["bearer", "x-api-key"] },
        "key_environment_variable": { "type": "string", "pattern": "^[A-Z][A-Z0-9_]*$" }
      }
    },
    "privacy": {
      "type": "object",
      "additionalProperties": false,
      "properties": {
        "direct_byok_supported": { "type": "boolean" },
        "cloud_proxy_required": { "type": "boolean" },
        "regions": { "type": "array", "items": { "type": "string" } },
        "data_policy_url": { "type": "string", "pattern": "^https://" }
      }
    },
    "models": { "type": "array", "minItems": 1, "items": { "$ref": "#/$defs/model" } }
  },
  "$defs": {
    "evidence": { "enum": ["unknown", "declared", "verified", "degraded", "unsupported"] },
    "model": {
      "type": "object",
      "required": ["id", "remote_model", "context_window_tokens", "maximum_output_tokens", "capabilities"],
      "additionalProperties": false,
      "properties": {
        "id": { "type": "string", "pattern": "^[a-z0-9][a-z0-9.-]{1,63}$" },
        "remote_model": { "type": "string", "maxLength": 256 },
        "aliases": { "type": "array", "items": { "type": "string" } },
        "deprecated_after": { "type": "string", "format": "date" },
        "context_window_tokens": { "type": "integer", "minimum": 1 },
        "maximum_output_tokens": { "type": "integer", "minimum": 1 },
        "capabilities": {
          "type": "object",
          "additionalProperties": false,
          "properties": {
            "streaming": { "$ref": "#/$defs/evidence" },
            "tool_calls": { "$ref": "#/$defs/evidence" },
            "parallel_tool_calls": { "$ref": "#/$defs/evidence" },
            "structured_output": { "$ref": "#/$defs/evidence" },
            "reasoning": { "$ref": "#/$defs/evidence" },
            "prompt_caching": { "$ref": "#/$defs/evidence" },
            "input_modalities": { "type": "array", "items": { "enum": ["text", "image", "file"] } },
            "provider_state_replay": { "type": "boolean" }
          }
        },
        "tool_schema": {
          "type": "object",
          "additionalProperties": false,
          "properties": {
            "tool_choice": { "type": "array", "items": { "enum": ["auto", "none", "required", "named"] } },
            "additional_properties": { "enum": ["supported", "must_be_false", "stripped"] },
            "unsupported_keywords": { "type": "array", "items": { "type": "string" } },
            "max_tool_name_length": { "type": "integer", "minimum": 1 },
            "max_description_length": { "type": "integer", "minimum": 1 },
            "strict_mode": { "enum": ["required", "optional", "unsupported"] }
          }
        },
        "compatibility": {
          "type": "object",
          "additionalProperties": false,
          "properties": {
            "legacy_function_call": { "type": "boolean" },
            "requires_assistant_reasoning_replay": { "type": "boolean" },
            "system_role_supported": { "type": "boolean" },
            "developer_role_supported": { "type": "boolean" }
          }
        },
        "retry": {
          "type": "object",
          "additionalProperties": false,
          "properties": {
            "max_attempts": { "type": "integer", "minimum": 0, "maximum": 5 },
            "respect_retry_after": { "type": "boolean" }
          }
        },
        "pricing": {
          "type": "object",
          "additionalProperties": false,
          "properties": {
            "input_micros_per_mtok": { "type": "integer", "minimum": 0 },
            "cached_input_micros_per_mtok": { "type": "integer", "minimum": 0 },
            "output_micros_per_mtok": { "type": "integer", "minimum": 0 }
          }
        }
      }
    }
  }
}
```

Schema rules: `additionalProperties: false` everywhere — unknown security-sensitive fields are rejected, not ignored, so an old binary cannot silently mis-enforce a newer profile. `default_base_url` must be HTTPS; the `local-openai` driver additionally accepts user-local endpoints (localhost, private ranges) only through explicit per-endpoint user approval in local config, never through a distributed profile. Profiles distributed through the registry are signed (ed25519, same verification machinery pattern as entitlements, `03-interfaces.md` §9) and verified before activation; a modified or unsigned registry profile is rejected. Locally authored profiles for development load only with an explicit `--unsigned-profile` opt-in flag and are labeled experimental in every surface.

## 3. Tool schema compiler

Compresso owns canonical tool definitions (the MCP tool schemas of `04-mcp-and-integrations.md` §1 plus agent-mode tools). Each provider receives a compiled projection driven entirely by the profile's `tool_schema` capability block — never by provider name.

```rust
pub trait ToolSchemaCompiler: Send + Sync {
    fn compile(
        &self,
        tools: &[ToolDefinition],
        capability: &ToolSchemaCapability,
    ) -> Result<Vec<CompiledTool>, ToolSchemaError>;
}
```

Downgrade rules, applied deterministically and in order:

1. **Names and lengths.** Tool names are transposed to the provider's character set and length limit with a stable, collision-checked mangling; descriptions truncate at word boundaries with an elision marker. A collision is a `ToolSchemaError`, never a silent rename.
2. **Keyword removal.** Keywords listed in `unsupported_keywords` are removed only when removal *widens* what the provider accepts (e.g. dropping `unevaluatedProperties`); the canonical schema still validates arguments at runtime, so widening the wire schema never weakens enforcement.
3. **Structural rewrites.** Unions, tuples, and numeric bounds unsupported by the provider are rewritten to the nearest accepting superset (union → `anyOf` flattening or a discriminated object; tuple → array with description) with the constraint restated in the description so the model still sees it.
4. **`additionalProperties` policy.** Applied per profile: `supported` passes through, `must_be_false` forces the flag, `stripped` removes it.
5. **Strict mode.** When the profile requires strict mode, optional properties are compiled per the provider's strict-mode rules; if a tool cannot be expressed strictly, see rejection below.
6. **Parallelism.** If `parallel_tool_calls` is not verified, the request is compiled single-call and the agent loop serializes.

**Rejection behavior.** If a required constraint cannot be expressed *or* enforced at runtime — that is, the downgrade would let semantically invalid arguments through even after canonical validation (e.g. a provider that cannot round-trip a required discriminator) — compilation fails with `ToolSchemaError::Unrepresentable { tool, keyword, model }` *before any network request*. The router treats this as ineligibility for that model, not as a runtime error. Nothing is ever "best-effort" transmitted.

Compilation is pure and cacheable, keyed by `(profile_version, blake3(tool_set))`. The compiler must not weaken runtime validation: every completed argument object is validated against the **canonical** schema by the tool executor before execution, regardless of what was sent over the wire. Invalid model arguments never reach a tool implementation.

## 4. Provider state, reasoning preservation, agent loop, MCP relationship

### 4.1 Provider state

```rust
pub struct ProviderState {
    pub provider: ProviderId,
    pub profile_version: String,
    pub turn_id: String,
    pub opaque: serde_json::Value,     // bounded: default max 256 KiB, profile-tunable downward
    pub retention: ProviderStateRetention,
}
pub enum ProviderStateRetention { CurrentRequest, CurrentSession, EncryptedResumeState }
```

Rules (enforced by type visibility, not convention):

- Provider state is accessible only to the selected adapter and the session coordinator in `compresso-agent`. It is not a `ContextGraph` node kind and cannot enter the compression engine, Atlas, or the store's operation records.
- Reasoning content (including `ReasoningDelta` text and any reasoning carried in opaque state) is never written to Atlas, project memory, ordinary logs, or local stats. Diagnostics expose sizes and blake3 hashes, never content.
- `EncryptedResumeState` persistence is opt-in, encrypted with a key held in the OS keychain, and bounded.
- Switching providers invalidates opaque state: a fallback provider receives a normalized conversation rebuilt from canonical messages only (§5.4).

This is the mechanism that makes DeepSeek-style thinking-mode replay (§7.4) a profile flag (`requires_assistant_reasoning_replay`) instead of a native adapter.

### 4.2 Agent loop (compresso-agent)

The provider-neutral loop:

```text
1. Receive task from client adapter.
2. Resolve project, user, and organization policy.
3. Query Atlas and retrieve relevant project context.
4. Build canonical messages and tool definitions.
5. Determine required model capabilities.
6. Select an eligible model profile (router, §5).
7. Execute the provider adapter; consume canonical events.
8. Assemble and validate tool calls (canonical schema).
9. Ask for approval when policy requires it.
10. Execute permitted tools in the Compresso tool runtime.
11. Append canonical tool results and preserved provider state.
12. Continue until completion, limit, cancellation, or failure.
13. Return canonical output and usage to the client adapter.
```

The loop owns maximum-turn, maximum-cost, maximum-time, and repeated-tool-call protections. Steps 3–4 run the existing compression engine on the assembled context — compression sits *above* the canonical protocol and is identical for every provider. Tool permissions (step 9–10) are resolved by the same policy machinery regardless of which model requested the call; a model can never change its own permission level. Phase-1 exit criterion: this entire loop runs against an in-memory `FakeProvider` with zero network access, including a complete streaming tool loop.

Per D6, step 13's accounting is local-only: normalized usage and operation metadata go to the local store (`record_operation`, `trace_json = NULL` per D47 semantics extended with a usage column) — no remote telemetry exists.

### 4.3 MCP relationship — providers are not MCP

MCP integration and model-provider integration are distinct problems and stay in distinct crates. The MCP server (`compresso-mcp`) is the **inbound** surface: a host client (Claude Code, Codex) that owns its own model loop calls Compresso tools over MCP. Provider drivers (`compresso-providers`) are the **outbound** surface: when Compresso owns the model loop, it originates provider requests itself. Neither implies the other:

| Mode | Model loop owner | Compresso surface used |
|---|---|---|
| MCP server mode | Host client | `compresso-mcp` only; no provider crate loaded |
| Compresso agent mode | Compresso runtime | `compresso-agent` + router + providers; MCP optional |

A provider advertising "MCP support" changes nothing here: an MCP-capable *model API* is still accessed through a driver, and its requested tool calls are still untrusted input passing through the same validation and permission gates. `compresso-mcp` keeps zero dependencies on E13 crates.

## 5. Router (`compresso-router`)

The router follows the same philosophy as the compression engine: deterministic first, explainable always. Given identical inputs (policy, profiles, capability registry, health snapshot, budget), it returns identical decisions, and every decision is reconstructible from recorded content-free metadata.

### 5.1 Eligibility

A model is eligible only if it passes every filter; filters are conjunctive and ordered for explainability:

1. Organization/user provider allowlist.
2. Project data-residency policy (profile `privacy.regions`).
3. Direct-BYOK versus proxied execution policy.
4. Required modalities present.
5. Required capabilities `verified` (or `declared` when the route explicitly allows it).
6. Context and output budget fit (request estimate vs. profile limits).
7. Maximum task cost (preflight estimate vs. route ceiling).
8. Tool set compilable for the model (§3 rejection = ineligible).
9. Health: circuit breaker closed or half-open.

The eligibility result records which filter excluded each rejected model.

### 5.2 Scoring

Eligible models are scored with measured data only — provider marketing benchmarks never enter the function:

```text
score = quality_weight     * evaluation_score      // compresso-eval results, per exact model+profile version
      + reliability_weight * recent_success_rate   // local operation history
      + cache_weight       * cache_affinity        // prompt-cache continuity with prior turns
      - cost_weight        * estimated_cost
      - latency_weight     * expected_latency
      - risk_weight        * policy_risk
```

Weights come from route policy with documented defaults. Ties break deterministically by profile id. Evaluation scores and health inputs are passed to the router by the agent; the router performs no I/O.

### 5.3 Routing policy

```toml
[routing.code_edit]
required_capabilities = ["streaming", "tool_calls"]
primary = "kimi-coding"
fallback = ["glm-coding", "deepseek-coding"]
maximum_cost_usd = 2.00
allow_declared_capabilities = false

[routing.architecture]
required_capabilities = ["reasoning", "tool_calls"]
primary = "deepseek-reasoning"
fallback = ["minimax-reasoning"]
maximum_cost_usd = 5.00

[routing.private_repository]
execution = "direct-byok-only"
allowed_providers = ["organization-approved"]
cloud_prompt_logging = false
```

### 5.4 Fallback rules

- Fallback chains are explicit configuration, visible to the user; there is no implicit "try everything" mode.
- A request never crosses privacy, region, or billing boundaries silently — a fallback that would is skipped with a recorded reason.
- Only idempotent transport failures are retried automatically; tool execution is never repeated unless the tool's declared idempotency policy permits it.
- Completed tool results are retained across fallback; provider-specific reasoning/opaque state is never sent to a different provider — the fallback receives a normalized conversation rebuilt from canonical messages.
- The UI and operation record always report which model actually completed the task.

## 6. Error model and resilience

```rust
pub enum ModelErrorKind {
    Authentication, Authorization, InvalidRequest, UnsupportedCapability,
    ContextOverflow, RateLimited, ProviderUnavailable, Timeout, Cancelled,
    ContentFiltered, ProtocolViolation, ToolSchemaRejected, BillingLimit, Unknown,
}

pub struct ModelError {
    pub kind: ModelErrorKind,
    pub user_message: String,          // safe, never contains payloads or keys
    pub diagnostic_code: &'static str,
    pub remote_request_id: Option<String>,
    pub retryable: Retryability,       // No | SameProvider { after: Option<Duration> } | FallbackSafe
    pub redacted_detail: Option<String>,
}
```

Redaction is structural, not best-effort: driver error paths build `redacted_detail` from an allowlist of fields (status code, error type, request id), never from raw response bodies, and authentication material is stripped before any error value is constructed (conformance case 22 verifies this).

Resilience controls, all bounded and all local: request deadlines; connect and idle-stream timeouts; exponential backoff with jitter honoring `retry-after`; per-provider and per-model circuit breakers (closed → open on failure-rate threshold → half-open probe); per-provider concurrency limits; per-user cost budgets; cancellation propagation through the event stream; bounded response and tool-argument buffers; retry protection around non-idempotent tools. Circuit-breaker state lives in memory with an optional local-store snapshot — never reported anywhere remote (D6).

## 7. Initial provider strategy

Order of delivery: **Anthropic and OpenAI first**, as first-party profiles over the two native drivers (`AnthropicMessagesDriver`, `OpenAiChatDriver`) — these drivers double as the reference implementations and reuse the wire-format knowledge from the E11 proxy work. Then the four expansion providers, all as profiles per the profile-first rule. A `LocalOpenAiDriver` variant covers OpenAI-compatible local gateways (Ollama-class) with the explicit local-network approval policy from §2.8.

| Provider | Driver | Quirks to encode in the profile |
|---|---|---|
| Anthropic | anthropic-messages | System prompt as top-level field, not a message role; tool results as user-content blocks; prompt caching via cache-control breakpoints |
| OpenAI | openai-chat | Strict-mode structured outputs; `developer` role mapping; parallel tool calls on by default where supported |
| Kimi | openai-chat | Modern `tool_calls` only — `legacy_function_call = false`; per-model reasoning/multimodal/context capabilities; official provider tools kept separate from Compresso-owned tools; regional endpoints and privacy metadata explicit |
| GLM | openai-chat | `tool_choice` restrictions recorded per model, never generalized across the family; thinking, context caching, structured output, and output limits per model; coding-plan endpoints as separate endpoint profiles when behavior or terms differ |
| MiniMax | openai-chat AND anthropic-messages | Two separate profiles, both conformance-tested — never assume the compatibility surfaces behave identically; per client and model, prefer whichever protocol passes the most reliable complete tool loop |
| DeepSeek | openai-chat AND anthropic-messages (verified default chosen by conformance) | Thinking-mode state replayed on subsequent tool turns via `provider_state_replay` + `requires_assistant_reasoning_replay`; reasoning state never enters project memory; strict function schemas accept only a JSON Schema subset (`tool_schema.unsupported_keywords`); aliases and deprecation dates in the updateable registry |

Each provider's native-adapter trigger is recorded in its profile's review notes (proprietary agent state, JWT lifecycles, realtime WebSocket protocols, official-tool orchestration); until such a feature is actually needed, no native code is written (ADR-0020).

## 8. Conformance suite

Conformance proves compatibility; `compresso-eval` evaluations measure usefulness. The suite runs every driver, profile, and model against controlled fixtures.

### 8.1 Required cases

- [ ] 1. Basic non-streaming text response
- [ ] 2. Basic streaming text response
- [ ] 3. UTF-8 characters split across transport chunks
- [ ] 4. Single tool call
- [ ] 5. Sequential tool calls
- [ ] 6. Parallel tool calls where declared
- [ ] 7. Tool arguments split across stream events
- [ ] 8. Invalid JSON tool arguments
- [ ] 9. Unknown tool request
- [ ] 10. Tool failure returned to the model
- [ ] 11. Tool result containing Unicode and large payloads
- [ ] 12. Structured output where declared
- [ ] 13. Unsupported JSON Schema keyword handling
- [ ] 14. Reasoning plus tool calls where declared
- [ ] 15. Provider-state replay across tool turns
- [ ] 16. Context-window boundary behavior
- [ ] 17. Maximum-output boundary behavior
- [ ] 18. Request cancellation
- [ ] 19. Connect timeout
- [ ] 20. Mid-stream timeout
- [ ] 21. Rate-limit response and retry-after parsing
- [ ] 22. Authentication failure redaction
- [ ] 23. Provider 5xx recovery
- [ ] 24. Usage and cost accounting
- [ ] 25. Fallback conversation reconstruction
- [ ] 26. Prompt injection attempting unauthorized tool use
- [ ] 27. Secret redaction before outbound requests
- [ ] 28. Circuit-breaker activation and recovery

### 8.2 Fixture policy

Fixtures are **recorded and sanitized**: real provider responses are captured in protected scheduled jobs, then stripped to response metadata and normalized events — never sensitive prompts, reasoning content, or credentials — and committed under `fixtures/providers/<provider>/`. CI runs entirely against these recorded fixtures with synthetic repositories and deterministic Compresso tools; production API keys never appear in pull-request workflows. Provider nondeterminism is handled with structural assertions (event ordering, schema validity, invariants) rather than byte-exact matching. An unstable profile is quarantined rather than weakening global assertions.

### 8.3 Capability promotion

```text
unknown -> declared -> verified
                    -> degraded
                    -> unsupported
```

Promotion to `verified` requires passing the relevant conformance cases on the current profile version *and* current model version; a profile or remote-model version change demotes affected capabilities back to `declared` until re-verified. Marking an untested capability `verified` is an architecture-review violation (source §27). Promotion runs record locally which cases passed, when, and against which versions — the data behind the UI's "verified capability badges and last verification date".

## 9. Security and IP boundaries

- **API keys live in the OS keychain** (macOS Keychain, Windows Credential Manager, Linux Secret Service) via the `keyring` crate already adopted for device credentials (D43). Keys are referenced by opaque identifiers such as `keychain:compresso/deepseek` and are **never persisted to disk** — not in profiles, project files, config.toml, logs, local stats, error values, support bundles, or shell history. Environment-variable auth is an explicit automation opt-in only. A dedicated test asserts no key material survives in any file the provider path can write.
- **All provider network access is explicit user configuration.** No default cloud calls exist: a provider connects only after the user runs `compresso provider add`, supplies a key, and the resolved host validates against the signed profile allowlist. HTTPS only; profile-driven redirects to unapproved hosts are blocked; DNS-rebinding and private-IP protections apply to custom endpoints; local gateways require explicit per-endpoint approval.
- **No telemetry (D6).** The source document's observability section is implemented local-only: content-free operation metadata (request id, profile/driver versions, latency buckets, token counts, normalized error category, circuit-breaker state) goes to the local store and `compresso stats`. Prohibited even locally in ordinary records: prompt text, source code, tool arguments/results, API keys, reasoning content, raw provider bodies. Debug capture is local, time-limited, visibly enabled, aggressively redacted, and never uploaded.
- **Crown-jewel boundary (E14 / ip.md, D46).** Proprietary routing weights, evaluation aggregates, and policy logic are the highest-value IP: the routing *engine* (deterministic eligibility + scoring code) ships in the local binary, but tuned weights and evaluation aggregates distribute only as signed data through the optional registry, and organization policy management stays server-side. Local responsibilities: drivers, key handling, Atlas queries, tool execution, permission enforcement, context assembly. Protection relies on service boundaries, signed artifacts, licensing, and minimal distribution — never on obfuscation alone.
- **Supply chain.** Profile registries are signed and verified before activation (ed25519, current+next key rotation, mirroring the entitlement pattern of `03-interfaces.md` §9). Profiles cannot define executable code or inject arbitrary secret-bearing headers; schema versions are pinned and unknown security-sensitive fields rejected. Native adapters ship only through the signed Compresso release process — never downloaded at runtime.
- **Every model response is untrusted input**, identical to the repository-content rule: validated against canonical schemas, gated by the same permission policy for every provider, and unable to alter its own permission level (D48 applies to provider-originated payloads: `protected` is forced on instruction/tool kinds).

## 10. E13 ticket seeds

One-line seeds, phased per `compresso-expand.md` §25; a later pass expands these into full tickets in `05-tickets.md`. All depend transitively on E1; provider network tickets additionally require the E13-10 key handling and E13-09 signing tickets so no code path ever ships that could leak a key.

**Phase 1 — canonical contracts**

- [ ] E13-01: `compresso-model-protocol` crate — canonical request/message/content types with serde round-trip and schema_version. Owner: R1 · Depends: E1-01
- [ ] E13-02: ModelEvent stream, FinishReason, TokenUsage, ModelError taxonomy with retryability classification. Owner: R1 · Depends: E13-01
- [ ] E13-03: ProviderAdapter trait, bounded ProviderState, in-memory FakeProvider streaming a complete tool loop. Owner: R1 · Depends: E13-02
- [ ] E13-04: Capability registry — evidence states, policy-disable overrides, CI grep gate banning provider names in core crates. Owner: R1 · Depends: E13-01

**Phase 2 — protocol drivers**

- [ ] E13-05: `compresso-providers` crate + OpenAiChatDriver (request build, SSE parse, tool-call extraction, usage/finish normalization). Owner: R2 · Depends: E13-03
- [ ] E13-06: AnthropicMessagesDriver (system-field mapping, content-block tool results, cache-control passthrough). Owner: R2 · Depends: E13-03
- [ ] E13-07: Driver resilience — deadlines, cancellation, bounded buffers, backoff with retry-after, structural secret redaction in all error paths. Owner: R2 · Depends: E13-05, E13-06

**Phase 3 — profile system**

- [ ] E13-08: `schemas/model-profile/v1.json` + TOML profile loader with boundary validation and reference-instance fixtures. Owner: R1 · Depends: E13-04
- [ ] E13-09: Signed profile registry — ed25519 verify (current+next keys), host allowlist enforcement, unsigned/modified-profile rejection, `--unsigned-profile` dev escape hatch. Owner: I1 · Depends: E13-08
- [ ] E13-10: Provider API keys in OS keychain via `keyring` — opaque key references, env-var opt-in path, no-key-on-disk regression test. Owner: I1 · Depends: E13-08
- [ ] E13-11: Model discovery endpoint support — untrusted-ID matching against signed profiles, experimental-model quarantine with conservative limits. Owner: D1 · Depends: E13-08, E13-09

**Phase 4/5 — providers and tool schema**

- [ ] E13-12: Anthropic + OpenAI first-party profiles over native drivers, wired through recorded-fixture conformance. Owner: R2 · Depends: E13-07, E13-08, E13-16
- [ ] E13-13: ToolSchemaCompiler — capability-driven downgrade rules, deterministic cacheable output, pre-network rejection of unrepresentable tools. Owner: R1 · Depends: E13-04, E13-05
- [ ] E13-14: Runtime canonical argument validator — invalid model arguments never reach a tool implementation. Owner: R1 · Depends: E13-13
- [ ] E13-15: Kimi + GLM profiles over OpenAiChatDriver with per-model quirk flags and recorded conformance fixtures. Owner: R2/PQ · Depends: E13-12, E13-13
- [ ] E13-16: Conformance harness — 28 required cases against fixture transport, structural assertions, quarantine mechanism. Owner: PQ · Depends: E13-05, E13-06
- [ ] E13-17: MiniMax + DeepSeek dual-protocol profiles — reasoning-state replay via ProviderState, verified-default protocol selection. Owner: R2/PQ · Depends: E13-15, E13-16
- [ ] E13-18: Capability promotion pipeline — unknown→declared→verified/degraded transitions recorded in local store, demotion on version change. Owner: PQ · Depends: E13-16

**Phase 6 — router and loop**

- [ ] E13-19: `compresso-router` eligibility filtering with per-model exclusion reasons. Owner: R1 · Depends: E13-04
- [ ] E13-20: Router scoring, explicit fallback chains, circuit breakers, canonical conversation reconstruction on fallback. Owner: R1 · Depends: E13-19, E13-07
- [ ] E13-21: Agent-loop integration in `compresso-agent` — full canonical loop with permissions, budgets, and provider-state custody over FakeProvider then real drivers. Owner: R1/D1 · Depends: E13-03, E13-14, E13-20
- [ ] E13-22: CLI surface — `compresso provider add`, `model list`, `model test`, `route set` with `--json`/`--dry-run`. Owner: D1 · Depends: E13-09, E13-10, E13-11, E13-20
