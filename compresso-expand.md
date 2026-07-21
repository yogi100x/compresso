# Compresso Model and Client Expansion Architecture

Status: Implementation-ready architecture

Purpose: Extend Compresso across Claude Code, Codex, Kimi, GLM, MiniMax, DeepSeek, and future model providers without coupling Atlas, memory, compression, permissions, or tool execution to any vendor.

## 1. Executive decision

Compresso will use two independent extension surfaces:

1. Client adapters integrate Compresso into hosts such as Claude Code, Codex, Kimi Code, IDEs, and terminal agents.
2. Provider adapters connect the Compresso agent runtime to model APIs such as Anthropic, OpenAI, Kimi, GLM, MiniMax, and DeepSeek.

The Compresso core will communicate through canonical request, event, tool, error, usage, and capability contracts. It will never branch directly on a provider name.

The key implementation rule is:

> A compatible provider should normally be added with a model profile using an existing protocol driver. Provider-specific native code is reserved for behavior that cannot be represented safely by the existing drivers and profile schema.

This produces the following dependency direction:

```text
Claude Code | Codex | Kimi Code | IDEs | Native CLI
                         |
                   Client adapters
                         |
              Compresso Agent Runtime
       Atlas | memory | compression | policy | tools
                         |
               Canonical Model Protocol
                         |
             Protocol drivers and profiles
                         |
 Anthropic | OpenAI | Kimi | GLM | MiniMax | DeepSeek
```

## 2. Goals

- Support multiple model providers without duplicating the agent loop.
- Make Claude Code and Codex first-class clients of the same Compresso runtime.
- Allow a new compatible provider to be added primarily through signed configuration.
- Preserve provider-specific state without exposing it to unrelated core systems.
- Normalize streaming, tool calls, errors, token usage, and finish reasons.
- Route tasks using verified capabilities, policy, reliability, cost, and latency.
- Keep source code and API keys local by default through direct BYOK connections.
- Enforce the same tool permissions regardless of which model is selected.
- Test every advertised capability through a provider conformance suite.
- Permit future local models and gateways without redesigning the core.

## 3. Non-goals

- Pretending that OpenAI-compatible APIs have identical semantics.
- Allowing model output to invoke arbitrary tools without Compresso authorization.
- Sending repository contents through the Compresso cloud by default.
- Building provider-specific branches into Atlas, memory, compression, or permissions.
- Depending on provider model names remaining stable.
- Persisting private reasoning content in normal logs or project memory.
- Treating MCP support and model-provider support as the same integration problem.

## 4. Architectural layers

### 4.1 Client adapter layer

Client adapters connect Compresso to the application in which the developer works.

Responsibilities:

- Install and remove Compresso integration safely.
- Register the Compresso MCP server where MCP is supported.
- Register hooks, commands, skills, or instructions where appropriate.
- Detect client capabilities and version compatibility.
- Translate host lifecycle events into Compresso lifecycle events.
- Display or relay permission prompts.
- Keep host configuration changes reversible and auditable.

Initial client adapters:

| Adapter | Integration method | Primary role |
|---|---|---|
| Claude Code | MCP, hooks, commands, project instructions | Claude-native developer workflow |
| Codex | MCP, skills, project instructions, CLI integration | OpenAI Codex developer workflow |
| Native Compresso CLI | Direct runtime API | Provider-neutral reference client |
| Generic MCP client | MCP server registration | Broad compatibility fallback |

Client adapters do not choose HTTP request formats for model providers. They request work from the agent runtime using Compresso contracts.

### 4.2 Agent runtime layer

The agent runtime is the provider-independent execution engine.

Responsibilities:

- Accept a user task and project context.
- Query Atlas and assemble relevant context.
- Apply context compression and token budgeting.
- Select an eligible model through the router.
- Execute the model and tool loop.
- Validate all model-generated tool arguments.
- Enforce permissions before tool execution.
- Record normalized usage and operational telemetry.
- Return canonical results to the client adapter.

The runtime operates on capabilities and canonical events. It must not contain logic such as:

```rust
if provider_name == "deepseek" {
    // Provider-specific behavior is forbidden here.
}
```

### 4.3 Protocol driver layer

A protocol driver implements a reusable wire format.

Initial drivers:

| Driver | Purpose |
|---|---|
| `OpenAiChatDriver` | OpenAI-compatible Chat Completions requests and streams |
| `AnthropicMessagesDriver` | Anthropic-compatible Messages requests and streams |
| `LocalOpenAiDriver` | OpenAI-compatible local gateways with explicit local-network policy |

A driver handles:

- HTTP request construction.
- Authentication header placement.
- Request serialization.
- Server-sent event or response stream parsing.
- Tool-call extraction.
- Tool-result serialization.
- Token usage extraction.
- Finish-reason normalization.
- Generic HTTP and protocol error mapping.
- Cancellation and request deadlines.

A driver does not assume all models using the protocol have the same capabilities.

### 4.4 Provider profile layer

A provider profile is declarative configuration layered over a protocol driver.

It describes:

- Provider identifier and display name.
- Driver selection.
- Approved base URLs and regional endpoints.
- Authentication scheme and key reference type.
- Model identifiers and aliases.
- Context and output limits.
- Modalities.
- Tool-call behavior.
- Supported JSON Schema subset.
- Reasoning behavior.
- Retry and rate-limit hints.
- Usage and pricing metadata.
- Privacy and residency metadata.
- Known compatibility flags.

Profiles are signed, versioned, and updateable independently from the Compresso binary.

### 4.5 Native provider adapter layer

A native adapter is provider-specific executable code. It is required only when an existing protocol driver plus profile cannot correctly represent the provider.

Valid reasons for native code:

- Proprietary request or response envelope.
- Nonstandard request signing.
- Proprietary streaming protocol.
- Tool calls with incompatible lifecycle semantics.
- Stateful server-managed conversations.
- Mandatory upload or asset preparation workflow.
- Provider-specific cache negotiation.
- Non-configurable reasoning-state behavior.
- WebSocket or bidirectional realtime protocol.
- Error recovery that requires provider-specific state transitions.

Native adapters still translate to and from the canonical Compresso protocol. Atlas, permissions, tools, and the agent loop remain unchanged.

## 5. Model profile versus native adapter

### 5.1 Profile-only integration

Use a profile when the provider can be expressed through an existing driver.

```text
Canonical request
    -> existing protocol driver
    -> provider endpoint
    -> existing stream parser
    -> canonical events
```

Typical work:

- Register base URLs.
- Register model IDs and aliases.
- Declare capabilities and limits.
- Declare tool-schema restrictions.
- Declare reasoning and compatibility flags.
- Add pricing and privacy metadata.
- Run conformance tests.

### 5.2 Native-adapter integration

Use native code when configuration cannot express required behavior.

```text
Canonical request
    -> native provider adapter
    -> proprietary provider API
    -> custom response parser
    -> canonical events
```

Typical work:

- Translate canonical requests.
- Implement authentication or signing.
- Parse proprietary streaming events.
- Translate tool calls and results.
- Preserve opaque provider state.
- Normalize errors and usage.
- Run the same conformance suite.

### 5.3 Decision test

Before creating a native adapter, answer these questions:

| Question | If yes |
|---|---|
| Can an existing driver serialize the request correctly? | Prefer a profile |
| Can profile flags express the provider's capability restrictions? | Prefer a profile |
| Can generic opaque state preserve required round-trip data? | Prefer a profile |
| Does the provider require a different stream parser? | Consider a native adapter |
| Does the provider require custom authentication or signing? | Consider a native adapter |
| Does correct recovery require a provider-specific state machine? | Use a native adapter |

Native code requires an ADR explaining why a profile is insufficient.

## 6. Canonical model protocol

The canonical protocol is an internal Compresso API. It is not a copy of any provider API.

### 6.1 Request

```rust
pub struct ModelRequest {
    pub request_id: RequestId,
    pub model: ModelRef,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolDefinition>,
    pub response_format: Option<ResponseFormat>,
    pub reasoning: ReasoningPolicy,
    pub sampling: SamplingOptions,
    pub budget: GenerationBudget,
    pub metadata: RequestMetadata,
    pub provider_state: Option<ProviderState>,
}

pub struct GenerationBudget {
    pub max_input_tokens: Option<u64>,
    pub max_output_tokens: Option<u64>,
    pub deadline_ms: Option<u64>,
    pub maximum_cost_micros: Option<u64>,
}

pub enum ReasoningPolicy {
    Disabled,
    Automatic,
    Enabled { effort: ReasoningEffort },
}
```

### 6.2 Messages

```rust
pub struct Message {
    pub role: MessageRole,
    pub parts: Vec<MessagePart>,
}

pub enum MessageRole {
    System,
    Developer,
    User,
    Assistant,
    Tool,
}

pub enum MessagePart {
    Text(String),
    Image(ImageReference),
    File(FileReference),
    ToolCall(ToolCall),
    ToolResult(ToolResult),
    ProviderState(ProviderState),
}
```

Provider drivers must define deterministic behavior when a remote API does not support a canonical role. They may safely merge compatible instructions, reject the request, or select another model. They must not silently discard instructions.

### 6.3 Streaming events

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
```

Tool arguments may arrive as incomplete JSON fragments. Drivers collect and bound fragments, while the tool executor validates the completed object before execution.

### 6.4 Finish reasons

```rust
pub enum FinishReason {
    Completed,
    ToolCall,
    OutputLimit,
    ContentFiltered,
    Cancelled,
    Timeout,
    Failed,
    Unknown(String),
}
```

Unknown provider values must be retained for diagnostics while mapping to safe runtime behavior.

### 6.5 Usage

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

pub enum UsageSource {
    ProviderReported,
    CompressoEstimated,
    Reconciled,
}
```

Provider-reported usage is authoritative for billing. Compresso estimates are used for preflight budgeting and clearly labeled as estimates.

## 7. Provider adapter interface

The Rust interface should be object-safe and stream-oriented.

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

Protocol drivers can implement this interface directly. Native adapters can wrap a driver or provide a complete implementation.

## 8. Capability registry

Capabilities are assigned per model profile, not globally per provider.

```rust
pub struct ModelCapabilities {
    pub input_modalities: CapabilitySet<InputModality>,
    pub output_modalities: CapabilitySet<OutputModality>,
    pub context_window_tokens: u64,
    pub maximum_output_tokens: u64,
    pub streaming: bool,
    pub tool_calls: bool,
    pub parallel_tool_calls: bool,
    pub tool_choice: CapabilitySet<ToolChoiceMode>,
    pub structured_output: StructuredOutputCapability,
    pub reasoning: ReasoningCapability,
    pub prompt_caching: PromptCacheCapability,
    pub provider_state_replay: bool,
    pub usage_reporting: UsageReportingCapability,
}
```

Each capability has an evidence state:

| State | Meaning |
|---|---|
| `declared` | Present in official provider documentation but not tested locally |
| `verified` | Passed the Compresso conformance suite |
| `degraded` | Supported with a documented limitation |
| `unsupported` | Confirmed unavailable |
| `unknown` | No safe claim can be made |

Routing uses only `verified` capabilities in production by default. Users can opt into `declared` or experimental capabilities.

## 9. Profile format

Illustrative profile:

```toml
schema_version = 1
profile_version = "2026-07-21"

[provider]
id = "example-provider"
display_name = "Example Provider"
driver = "openai-chat"
approved_hosts = ["api.example.com"]
default_base_url = "https://api.example.com/v1"
auth = "bearer"
key_environment_variable = "EXAMPLE_API_KEY"

[privacy]
direct_byok_supported = true
cloud_proxy_required = false
regions = ["global"]
data_policy_url = "https://example.com/privacy"

[[models]]
id = "example-coding"
remote_model = "provider-model-id"
aliases = ["coding-fast"]
context_window_tokens = 200000
maximum_output_tokens = 32000

[models.capabilities]
streaming = "declared"
tool_calls = "declared"
parallel_tool_calls = "unknown"
structured_output = "declared"
reasoning = "declared"
provider_state_replay = false

[models.tool_schema]
tool_choice = ["auto"]
additional_properties = "supported"
unsupported_keywords = ["unevaluatedProperties"]

[models.compatibility]
legacy_function_call = false
requires_assistant_reasoning_replay = false
```

Profiles contain no secrets. User configuration references keys stored in the operating-system credential store.

## 10. Initial provider strategy

### 10.1 Kimi

Initial approach: `OpenAiChatDriver` plus Kimi profiles.

Profile considerations:

- OpenAI-compatible endpoint configuration.
- Modern `tool_calls` support.
- Legacy `function_call` disabled.
- Per-model reasoning, multimodal, and context capabilities.
- Official provider tools kept separate from Compresso-owned tools.
- Regional endpoint and privacy metadata represented explicitly.

Native adapter trigger:

- A future Kimi feature requires proprietary agent state, streaming events, authentication, or official-tool orchestration that cannot be represented in the shared driver.

Official references:

- https://platform.kimi.ai/docs/api/overview
- https://platform.kimi.ai/docs/guide/use-kimi-api-to-complete-tool-calls

### 10.2 GLM

Initial approach: `OpenAiChatDriver` plus GLM profiles.

Profile considerations:

- OpenAI SDK-compatible endpoint.
- Function calling and tool-result round trips.
- `tool_choice` capability recorded per model; current documented restrictions must not be generalized.
- Thinking, context caching, structured output, multimodal, and output limits recorded per model.
- Coding-plan endpoints represented as separate endpoint profiles if their behavior or terms differ.

Native adapter trigger:

- A GLM endpoint requires a different protocol, JWT lifecycle, realtime WebSocket state machine, or provider-specific MCP orchestration.

Official references:

- https://docs.bigmodel.cn/cn/guide/develop/openai/introduction
- https://docs.bigmodel.cn/cn/guide/capabilities/function-calling

### 10.3 MiniMax

Initial approach: create separate OpenAI-compatible and Anthropic-compatible profiles.

Profile considerations:

- Test both protocols rather than assuming equivalent behavior.
- Select the protocol independently for each client and model.
- Record tool calling, reasoning, streaming, and usage differences discovered by conformance tests.
- Prefer the protocol with the most reliable complete tool loop for Compresso-native execution.

Native adapter trigger:

- A MiniMax-native feature is valuable and unavailable through both compatibility protocols.

Official reference:

- https://platform.minimax.io/docs/token-plan/other-tools

### 10.4 DeepSeek

Initial approach: test both `OpenAiChatDriver` and `AnthropicMessagesDriver`, then select a verified default profile.

Profile considerations:

- Thinking-mode state may need to be replayed on subsequent tool turns.
- Provider reasoning state must be preserved without entering project memory.
- Compatibility mode can affect whether reasoning and tool loops work correctly.
- Strict function schemas may support only a subset of JSON Schema.
- Model aliases and deprecation dates belong in the updateable registry.

Native adapter trigger:

- Generic opaque-state replay and compatibility flags cannot implement DeepSeek's current tool and reasoning lifecycle reliably.

Official references:

- https://api-docs.deepseek.com/quick_start/pricing-details-usd/
- https://api-docs.deepseek.com/guides/thinking_mode
- https://api-docs.deepseek.com/guides/tool_calls/

## 11. Tool schema compiler

Compresso owns canonical tool definitions. Each provider receives a compatible projection.

```rust
pub trait ToolSchemaCompiler: Send + Sync {
    fn compile(
        &self,
        tools: &[ToolDefinition],
        capability: &ToolSchemaCapability,
    ) -> Result<Vec<CompiledTool>, ToolSchemaError>;
}
```

The compiler handles:

- Tool-name character and length limits.
- Description length limits.
- Unsupported JSON Schema keywords.
- Required and optional property behavior.
- Enum, union, tuple, numeric, and string restrictions.
- `additionalProperties` behavior.
- Strict-mode requirements.
- Single versus parallel tool-call support.
- Provider-specific tool-result envelopes.

Compilation is deterministic and cacheable by profile version plus tool-set hash.

The compiler must not weaken runtime validation. Compresso validates every completed argument object against the canonical schema before executing a tool.

## 12. Provider state and reasoning

Provider-specific round-trip state is represented as an opaque, bounded value:

```rust
pub struct ProviderState {
    pub provider: ProviderId,
    pub profile_version: String,
    pub turn_id: String,
    pub opaque: serde_json::Value,
    pub retention: ProviderStateRetention,
}

pub enum ProviderStateRetention {
    CurrentRequest,
    CurrentSession,
    EncryptedResumeState,
}
```

Rules:

- Provider state is accessible only to the selected adapter and session coordinator.
- Reasoning content is not written to Atlas, project memory, ordinary logs, or analytics.
- Persisted resume state is encrypted and opt-in.
- State size is bounded to prevent memory and storage abuse.
- Switching providers invalidates incompatible opaque state.
- Diagnostics expose metadata and hashes, not private reasoning content.

## 13. Agent loop

The provider-neutral loop is:

```text
1. Receive task from client adapter.
2. Resolve project, user, and organization policy.
3. Query Atlas and retrieve relevant project context.
4. Build canonical messages and tool definitions.
5. Determine required model capabilities.
6. Select an eligible model profile.
7. Execute the provider adapter and consume canonical events.
8. Assemble and validate tool calls.
9. Ask for approval when policy requires it.
10. Execute permitted tools in the Compresso tool runtime.
11. Add canonical tool results and preserved provider state.
12. Continue until completion, limit, cancellation, or failure.
13. Return canonical output and usage to the client adapter.
```

The loop owns maximum-turn, maximum-cost, maximum-time, and repeated-tool-call protections.

## 14. MCP relationship

MCP integration and model-provider integration are related but distinct.

```text
Model-enabled host client
        -> host MCP client
        -> Compresso MCP server
        -> Atlas, memory, compression, and tools
```

When Claude Code, Codex, or another host already supports both MCP and a chosen model, Compresso may require only the client adapter. The host owns the model loop.

When Compresso owns model selection, fallback, and execution, the provider adapter layer is required.

Compresso should support both operating modes:

| Mode | Model loop owner | Use case |
|---|---|---|
| MCP server mode | Host client | Fast integration with Claude Code, Codex, and MCP clients |
| Compresso agent mode | Compresso runtime | Cross-provider routing, consistent policy, cost control, and native workflows |

Provider documentation describing MCP support does not remove the need for Compresso permissions. The model's requested tool call is still untrusted input.

## 15. Routing architecture

### 15.1 Eligibility

A model is eligible only if it satisfies:

- Organization provider allowlist.
- Project data-residency policy.
- Direct versus proxied execution policy.
- Required modalities.
- Required verified tool capabilities.
- Context and output budget.
- Maximum task cost.
- Model health and circuit-breaker status.

### 15.2 Scoring

Eligible models are scored using measured data:

```text
score = quality_weight * evaluation_score
      + reliability_weight * recent_success_rate
      + cache_weight * cache_affinity
      - cost_weight * estimated_cost
      - latency_weight * expected_latency
      - risk_weight * policy_risk
```

Provider marketing benchmarks do not directly set routing scores. Compresso evaluations and production health signals do.

### 15.3 Routing policy example

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

### 15.4 Fallback rules

- Fallback chains are explicit and visible to the user or administrator.
- A request does not cross privacy, region, or billing boundaries silently.
- Only idempotent transport failures are automatically retried.
- Tool execution is never repeated unless its idempotency policy permits it.
- Completed tool results are retained when safely moving to another provider.
- Provider-specific reasoning state is not sent to another provider.
- A fallback receives a normalized conversation rebuilt from canonical messages.
- The UI reports which model completed the task.

## 16. Error model and resilience

Canonical errors:

```rust
pub enum ModelErrorKind {
    Authentication,
    Authorization,
    InvalidRequest,
    UnsupportedCapability,
    ContextOverflow,
    RateLimited,
    ProviderUnavailable,
    Timeout,
    Cancelled,
    ContentFiltered,
    ProtocolViolation,
    ToolSchemaRejected,
    BillingLimit,
    Unknown,
}
```

Each error includes:

- Safe user-facing message.
- Internal diagnostic code.
- Provider request ID when available.
- Retryability classification.
- Retry-after duration when supplied.
- Whether fallback is policy-safe.
- Redacted provider details for diagnostics.

Resilience controls:

- Request deadlines.
- Connect and idle-stream timeouts.
- Exponential backoff with jitter.
- Provider and model circuit breakers.
- Per-provider concurrency limits.
- Per-user and per-organization budgets.
- Cancellation propagation.
- Bounded response and tool-argument buffers.
- Retry protection around non-idempotent tools.

## 17. Security architecture

### 17.1 API keys

- Store secrets in macOS Keychain, Windows Credential Manager, or Linux Secret Service.
- Use environment variables only as an explicit automation option.
- Never store provider keys in project files or model profiles.
- Never include keys in logs, crash reports, analytics, support bundles, or command history.
- Reference secrets using opaque identifiers such as `keychain:compresso/deepseek`.
- Support independent keys by user, organization, provider, and environment.

### 17.2 Network security

- Allow only HTTPS provider endpoints by default.
- Validate the resolved host against the signed profile allowlist.
- Block profile-driven redirects to unapproved hosts.
- Apply DNS rebinding and local-network protections to custom endpoints.
- Require explicit approval for local gateways and private IP addresses.
- Use bounded timeouts and response sizes.
- Support enterprise outbound proxies without exposing keys to Compresso Cloud.

### 17.3 Tool security

- Treat every model response as untrusted input.
- Validate tool names and arguments against canonical schemas.
- Resolve tool permission from user, organization, project, and session policy.
- Require approval for destructive or high-impact operations.
- Run tools with least privilege and workspace boundaries.
- Prevent provider output from changing its own permission level.
- Record auditable tool decisions without storing hidden reasoning.
- Apply the same security policy to every provider.

### 17.4 Data privacy

- Use direct BYOK connections by default.
- Keep source code local unless the selected provider request requires sending it.
- Show provider, endpoint region, and data-policy information during setup.
- Require explicit opt-in before routing prompts through Compresso Cloud.
- Redact secrets before model requests using deterministic scanners and policy rules.
- Keep telemetry disabled by default for prompt and source content.
- Never collect reasoning content in product analytics.

### 17.5 Supply-chain security

- Sign model profile registries.
- Verify profile signatures before activation.
- Pin schema versions and reject unknown security-sensitive fields.
- Do not download arbitrary native provider code at runtime.
- Ship native adapters through the signed Compresso release process.
- Produce an SBOM and provenance attestation for release artifacts.

## 18. IP protection boundaries

Provider portability must not force all Compresso intellectual property into distributed clients.

Local binary responsibilities:

- Provider protocol drivers.
- Key handling.
- Local Atlas queries.
- Tool execution and permission enforcement.
- Context assembly needed for privacy-preserving direct requests.

Server-side optional responsibilities:

- Signed model registry distribution.
- Proprietary routing weights and evaluation aggregates.
- Organization policy management.
- Billing and entitlement enforcement.
- Fleet health and provider reliability aggregates.

Do not depend on obfuscation as the primary protection. Use service boundaries, signed artifacts, licensing, access control, and minimal distribution of proprietary routing and evaluation logic.

## 19. Configuration experience

Provider onboarding flow:

```text
1. Select provider.
2. Select approved region or endpoint.
3. Enter API key into secure credential storage.
4. Discover or select model.
5. Load signed capability profile.
6. Run a safe connection and conformance smoke test.
7. Show verified capability badges and limitations.
8. Assign the model to routing policies.
```

The UI and CLI should expose:

- Provider and model display name.
- Exact remote model identifier.
- Protocol driver in use.
- Verified capabilities.
- Context and output limits.
- Estimated and reconciled cost.
- Region and privacy information.
- Health status.
- Fallback chain.
- Profile version and last verification date.

## 20. Model discovery

Model discovery can use a provider model-list endpoint when available, but discovery does not establish capabilities.

Rules:

- Treat discovered IDs as untrusted remote data.
- Match IDs against signed profiles when possible.
- Mark unmatched models as experimental.
- Require conservative limits for unknown models.
- Do not infer tool calling from model names.
- Do not infer context windows from provider families.
- Run smoke tests before enabling an experimental model.

## 21. Conformance suite

Every provider, driver, and model profile must be tested against controlled fixtures.

### 21.1 Required cases

1. Basic non-streaming text response.
2. Basic streaming text response.
3. UTF-8 characters split across transport chunks.
4. Single tool call.
5. Sequential tool calls.
6. Parallel tool calls where declared.
7. Tool arguments split across stream events.
8. Invalid JSON tool arguments.
9. Unknown tool request.
10. Tool failure returned to the model.
11. Tool result containing Unicode and large payloads.
12. Structured output where declared.
13. Unsupported JSON Schema keyword handling.
14. Reasoning plus tool calls where declared.
15. Provider-state replay across tool turns.
16. Context-window boundary behavior.
17. Maximum-output boundary behavior.
18. Request cancellation.
19. Connect timeout.
20. Mid-stream timeout.
21. Rate-limit response and retry-after parsing.
22. Authentication failure redaction.
23. Provider 5xx recovery.
24. Usage and cost accounting.
25. Fallback conversation reconstruction.
26. Prompt injection attempting unauthorized tool use.
27. Secret redaction before outbound requests.
28. Circuit-breaker activation and recovery.

### 21.2 Fixture policy

- Use synthetic repositories and deterministic Compresso tools in CI.
- Never use production API keys in pull-request workflows.
- Run provider network tests in protected scheduled jobs.
- Store response metadata and normalized events, not sensitive prompts or reasoning.
- Account for provider nondeterminism with structural assertions.
- Quarantine unstable profiles rather than weakening global assertions.

### 21.3 Capability promotion

```text
unknown -> declared -> verified
                    -> degraded
                    -> unsupported
```

Promotion to `verified` requires passing the relevant conformance cases on the current profile and model version.

## 22. Evaluation and routing data

Conformance proves compatibility; evaluations measure usefulness.

Initial evaluation categories:

- Repository retrieval use.
- Multi-file code editing.
- Debugging and error localization.
- Code review accuracy.
- Tool selection.
- Tool-argument correctness.
- Long-horizon task completion.
- Context compression resilience.
- Instruction hierarchy adherence.
- Prompt-injection resistance.
- Latency to first token.
- Total task latency.
- Cost per successful task.

Store evaluation results by exact provider, model ID, profile version, driver version, and Compresso runtime version.

## 23. Observability

Operational telemetry must be content-free by default.

Allowed default fields:

- Random request ID.
- Provider and model profile IDs.
- Driver and profile versions.
- Latency buckets.
- Token counts.
- Estimated cost.
- Normalized error category.
- Tool names and success status when organization policy permits.
- Circuit-breaker state.

Prohibited default fields:

- Prompt text.
- Source code.
- Tool arguments or results.
- API keys.
- Reasoning content.
- File contents.
- Raw provider response bodies.

Debug capture is local, time-limited, visibly enabled, aggressively redacted, and never uploaded automatically.

## 24. Suggested repository structure

```text
crates/
  compresso-model-core/
    src/request.rs
    src/message.rs
    src/event.rs
    src/error.rs
    src/usage.rs
    src/capability.rs
  compresso-provider-api/
    src/adapter.rs
    src/profile.rs
    src/registry.rs
  compresso-driver-openai/
    src/request.rs
    src/stream.rs
    src/tools.rs
    src/errors.rs
  compresso-driver-anthropic/
    src/request.rs
    src/stream.rs
    src/tools.rs
    src/errors.rs
  compresso-provider-native/
    src/lib.rs
  compresso-tool-schema/
    src/compiler.rs
    src/validator.rs
  compresso-router/
    src/eligibility.rs
    src/scoring.rs
    src/fallback.rs
    src/health.rs
  compresso-agent-runtime/
    src/session.rs
    src/agent_loop.rs
    src/budget.rs
    src/permissions.rs
  compresso-client-claude/
  compresso-client-codex/
  compresso-mcp-server/
profiles/
  providers/
  models/
fixtures/
  providers/
  tools/
  code/
```

The native-provider crate remains small. Growth there is a signal to improve generic protocol and profile abstractions.

## 25. Implementation phases

### Phase 1: Canonical contracts

Deliverables:

- Request, message, event, error, usage, and capability types.
- Provider adapter trait.
- Bounded provider-state representation.
- Snapshot tests for canonical serialization where persistence is required.

Exit criteria:

- The agent runtime can operate against an in-memory fake provider.
- The fake provider can stream text and perform a complete tool loop.
- No provider names appear in the runtime crate.

### Phase 2: Protocol drivers

Deliverables:

- OpenAI-compatible Chat Completions driver.
- Anthropic-compatible Messages driver.
- Streaming parsers with bounded buffers.
- Error and usage normalization.
- Cancellation and deadline propagation.

Exit criteria:

- Both drivers pass transport-level fixtures.
- Tool calls normalize to identical canonical events.
- Authentication details are redacted from all errors.

### Phase 3: Profile system

Deliverables:

- Versioned profile schema.
- Signed registry loader.
- Host allowlist enforcement.
- Capability evidence states.
- Model discovery and experimental-model handling.

Exit criteria:

- A compatible test provider can be added without rebuilding Compresso.
- Modified or unsigned production profiles are rejected.
- Profiles cannot inject arbitrary headers containing secrets.

### Phase 4: Initial providers

Deliverables:

- Kimi profiles.
- GLM profiles.
- MiniMax OpenAI and Anthropic profiles.
- DeepSeek OpenAI and Anthropic profiles.
- Provider-specific conformance fixtures.

Exit criteria:

- Each enabled profile passes basic streaming and tool-loop tests.
- Declared limitations are visible in configuration and UI.
- The runtime contains no provider-name conditionals.

### Phase 5: Tool schema portability

Deliverables:

- Canonical tool schema.
- Capability-aware schema compiler.
- Runtime argument validator.
- Parallel tool-call policy.
- Tool-result normalization.

Exit criteria:

- The same Atlas tool definition works across every verified provider.
- Unsupported schema features fail before network execution or are transformed without weakening validation.
- Invalid model arguments never reach a tool implementation.

### Phase 6: Router and resilience

Deliverables:

- Eligibility filtering.
- Policy-aware scoring.
- Explicit fallback chains.
- Circuit breakers.
- Retry classifier.
- Cost, time, and turn budgets.

Exit criteria:

- Routing decisions are explainable from recorded non-sensitive metadata.
- Fallback never crosses configured security boundaries.
- Non-idempotent tools are not repeated automatically.

### Phase 7: Client parity

Deliverables:

- Claude Code adapter.
- Codex adapter.
- Native Compresso CLI.
- Generic MCP setup flow.
- Reversible installer and uninstaller behavior.

Exit criteria:

- Claude Code and Codex access the same Compresso tools and project data.
- Client-specific setup does not alter provider-runtime semantics.
- Removing one client adapter does not remove Compresso data or other integrations.

### Phase 8: Production operations

Deliverables:

- Scheduled provider conformance jobs.
- Evaluation pipeline.
- Signed profile release pipeline.
- Health dashboards and alerts.
- Organization policy controls.
- Usage and cost reconciliation.

Exit criteria:

- A provider regression can automatically degrade or disable an affected profile.
- Registry rollback is supported.
- Operational telemetry remains content-free by default.

## 26. Definition of done for a new provider

A provider integration is complete only when:

- An approved protocol driver or justified native adapter exists.
- The profile schema validates.
- Approved hosts and authentication behavior are defined.
- Secrets are stored outside project configuration.
- Tool schema restrictions are represented.
- Reasoning and provider-state behavior are represented.
- Required conformance cases pass.
- Capabilities carry evidence states.
- Privacy and region metadata are visible.
- Usage and errors normalize correctly.
- Cancellation and rate limiting behave correctly.
- Routing policy can include or exclude the model.
- Fallback behavior is tested.
- Documentation explains limitations.
- No provider-specific condition was added to Atlas, memory, compression, permissions, or the generic agent loop.

## 27. Architecture guardrails

The following changes require architecture review:

- Adding a provider-name conditional to a core runtime crate.
- Creating a native adapter without a profile-insufficiency ADR.
- Persisting reasoning content.
- Adding an endpoint not covered by signed host policy.
- Sending prompts through Compresso Cloud by default.
- Marking an untested capability as verified.
- Retrying a non-idempotent tool automatically.
- Allowing provider profiles to define executable code.
- Allowing a model to bypass Compresso tool permissions.

## 28. Final product behavior

From the user's perspective:

```text
compresso provider add kimi
compresso provider add glm
compresso provider add minimax
compresso provider add deepseek

compresso model list
compresso model test <model-alias>
compresso route set code-edit --primary <model-alias>
compresso route set code-edit --fallback <model-alias>
```

The same Atlas index, memory, compression engine, tools, permissions, and audit model work regardless of provider. Claude Code and Codex can either consume these capabilities through MCP or delegate complete agent execution to Compresso. Provider differences remain isolated in profiles, protocol drivers, and the smallest possible set of native adapters.
