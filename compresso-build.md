# Compresso Production Build Plan

## 1. Document purpose

This document defines the plan for building Compresso as a complete, production-ready product. It is not an MVP specification. The target includes the local context engine, desktop application, CLI, coding-assistant integrations, cloud control plane, accounts, billing, updates, security, observability, support operations, and cross-platform distribution.

Compresso is a local-first context intelligence layer for AI coding tools. It reduces unnecessary model context while preserving user intent, system instructions, tool contracts, code correctness, and operational reliability.

The core promise is:

> Compresso gives coding agents the smallest useful context, explains every reduction, and safely passes through the original input whenever uncertainty is too high.

## 2. Assumptions

- Initial team: 5-7 people, with some roles combined during early development.
- Initial production timeline: 24-30 weeks.
- A solo implementation would likely require 12-18 months.
- Local runtime and compression engine: Rust.
- Desktop application: Tauri using the Rust runtime.
- Cloud control plane and web application: TypeScript.
- Primary local storage: SQLite.
- Primary cloud storage: PostgreSQL.
- macOS, Windows, and Linux are required production targets.
- Claude, Codex, and Cursor are the first supported coding assistants.
- The compression path must continue working during cloud outages.
- Source code and prompt content must not leave the device by default.

The Compresso name, package names, domains, and trademarks must be checked before public launch.

## 3. Product goals

Compresso must provide:

1. Repository understanding through files, symbols, imports, references, tests, and change relationships.
2. Progressive context retrieval through repository maps, ranked search, outlines, symbol expansion, and bounded reads.
3. Deterministic compression for logs, repeated tool output, structured content, conversation history, and source context.
4. MCP integration for compatible coding assistants.
5. Optional native hooks for clients that expose suitable integration points.
6. An optional provider proxy for Anthropic- and OpenAI-compatible APIs.
7. Transparent explanations of every compression or routing decision.
8. Accurate separation of observed, applied, modeled, and provider-verified savings.
9. A desktop application and local dashboard.
10. Accounts, subscriptions, organizations, devices, entitlements, and team policies.
11. Secure installation, updates, rollback, migration, and uninstallation.
12. Privacy controls, diagnostic tooling, operational monitoring, and support workflows.

## 4. Product principles

- Local by default.
- Offline-capable core.
- Read-only integrations before write-capable integrations.
- Explicit consent before modifying assistant configuration.
- Fail open to the original content.
- Do not modify system prompts or tool schemas by default.
- Do not force assistants to use Compresso tools.
- Never report savings that were not actually applied.
- Every compression operation produces an audit trace.
- Installation and uninstallation must be reversible.
- Task success takes priority over token savings.
- Repository content is untrusted data, not executable instruction.
- Cloud licensing failures must never break the underlying coding assistant.
- Unknown request formats must pass through unchanged.

## 5. Complete user journey

The production journey must support:

1. User visits the Compresso website.
2. User downloads a signed installer or uses a package manager.
3. Installer verifies the signed release and installs only the application.
4. User launches Compresso and chooses local-only or account-backed operation.
5. Compresso detects supported coding assistants.
6. User previews the exact configuration changes for each assistant.
7. Compresso creates backups and installs approved integrations.
8. User restarts the selected assistants.
9. Compresso indexes approved repositories in the background.
10. The assistant accesses bounded context through MCP, hooks, or the optional proxy.
11. The user inspects context decisions, performance, and savings locally.
12. Compresso updates through a signed, rollback-capable release channel.
13. The user can remove one integration or completely uninstall Compresso without losing unrelated configuration.

## 6. System architecture

```text
                         COMPRESSO CLOUD
 +-------------------------------------------------------------+
 | Public site | Web app | Team console | Admin console         |
 +-------------------------------+-----------------------------+
                                 |
 +-------------------------------v-----------------------------+
 | Control-plane API                                           |
 | Auth | Devices | Entitlements | Billing | Releases | Policy |
 +----------+--------------------+--------------------+---------+
            |                    |                    |
        PostgreSQL          Queue / Redis        Object storage
                                                   and CDN
                                 |
                       Signed HTTPS communication
                                 |
                         LOCAL COMPRESSO
 +-------------------------------------------------------------+
 | Desktop application | CLI | Local dashboard                 |
 +-------------------------------+-----------------------------+
                                 |
 +-------------------------------v-----------------------------+
 | Local supervisor                                            |
 | Health | Sessions | Updates | Config | Integration control  |
 +----------+--------------------+--------------------+---------+
            |                    |                    |
      Context engine        Atlas index       Protocol adapters
            |                    |                    |
      Compression          Repo intelligence   MCP/hooks/proxy
            |                    |                    |
            +--------------------+--------------------+
                                 |
                         SQLite and local cache
```

The cloud begins as a modular monolith. Service extraction is allowed only when scaling, security isolation, or team ownership provides a concrete reason.

## 7. Repository structure

```text
compresso/
  apps/
    desktop/
    web/
    control-plane/
    admin/
  rust/
    compresso-cli/
    compresso-agent/
    compresso-core/
    compresso-atlas/
    compresso-mcp/
    compresso-proxy/
    compresso-integrations/
    compresso-store/
    compresso-updater/
    compresso-eval/
  packages/
    api-client/
    shared-types/
    ui/
    config/
  schemas/
    context-graph/
    compression-trace/
    local-api/
    cloud-api/
    telemetry/
  fixtures/
    code/
    logs/
    conversations/
    provider-payloads/
    adversarial/
  installers/
    install.sh
    install.ps1
    homebrew/
  infra/
    terraform/
    docker/
    monitoring/
  docs/
    architecture/
    security/
    privacy/
    integrations/
    runbooks/
    adr/
```

## 8. Local product architecture

### 8.1 Local supervisor

The local supervisor coordinates the installed product.

Responsibilities:

- Start and stop local components.
- Manage MCP sessions.
- Run the optional provider proxy.
- Coordinate repository indexing.
- Coordinate SQLite access and migrations.
- Apply configuration and policy.
- Track local operations and savings.
- Monitor integration health.
- Handle updates and rollback.
- Expose an authenticated local API to the desktop application.
- Enforce workspace and repository boundaries.

Interfaces:

- Unix domain socket on macOS and Linux.
- Named pipe on Windows.
- Authenticated localhost HTTP for the desktop application.
- Stdio bridge for MCP clients.

If the supervisor is unavailable, read-only MCP should fall back to an embedded mode where practical. Failure must not prevent the coding assistant from starting.

### 8.2 CLI

Proposed command surface:

```text
compresso init
compresso doctor
compresso index
compresso map
compresso search
compresso outline
compresso unfold
compresso context
compresso compress
compresso explain
compresso mcp serve
compresso proxy
compresso integrations
compresso stats
compresso config
compresso account
compresso update
compresso uninstall
```

All modifying commands must support a dry-run or preview where appropriate. All automation-oriented commands must support structured JSON output.

### 8.3 Desktop application

The Tauri desktop application must provide:

- First-run onboarding.
- Local-only and account-backed setup.
- Assistant detection and integration management.
- Repository approval and indexing controls.
- Live health status.
- Session and savings dashboard.
- Compression Explorer with before/after views.
- Operation audit trail.
- Strategy and privacy configuration.
- Account, team, device, and plan management.
- Update, rollback, diagnostics, and uninstallation.

Primary pages:

```text
Overview
Sessions
Repositories
Integrations
Compression Explorer
Savings
Privacy
Account
Diagnostics
Settings
```

## 9. ContextGraph domain model

All compression operates on a typed ContextGraph.

### 9.1 Context node

```rust
struct ContextNode {
    id: String,
    kind: NodeKind,
    content: String,
    source: Source,
    token_count: usize,
    priority: f32,
    protected: bool,
    recency: Option<u64>,
    metadata: Map<String, Value>,
}
```

Node kinds include:

- System instruction.
- User instruction.
- Assistant response.
- Tool definition.
- Tool request.
- Tool response.
- Source file.
- Code symbol.
- Search result.
- Compiler output.
- Test output.
- Repository map.
- Summary.
- Decision.
- Memory fact.

### 9.2 Context edge

```rust
struct ContextEdge {
    from: String,
    to: String,
    kind: EdgeKind,
    weight: f32,
}
```

Relationships include imports, calls, defines, references, tests, supersedes, produced-by-tool, responds-to, depends-on, and same-error-group.

### 9.3 Compression budget

```rust
struct CompressionBudget {
    max_tokens: usize,
    max_latency_ms: u64,
    allowed_lossiness: Lossiness,
    protected_kinds: Vec<NodeKind>,
}
```

### 9.4 Compression result

```rust
struct CompressionResult {
    output: ContextGraph,
    before_tokens: usize,
    after_tokens: usize,
    strategy_results: Vec<StrategyResult>,
    safety_checks: Vec<SafetyCheck>,
    fallback_used: bool,
}
```

Schemas must be versioned and validated at every process or network boundary.

## 10. Compression engine

### 10.1 Strategy interface

```rust
trait CompressionStrategy {
    fn id(&self) -> StrategyId;
    fn supports(&self, input: &ContextGraph) -> bool;
    fn estimate(&self, input: &ContextGraph) -> Estimate;
    fn execute(
        &self,
        input: &ContextGraph,
        budget: &CompressionBudget,
    ) -> Result<StrategyOutput>;
}
```

### 10.2 Production strategies

- Exact duplicate removal.
- Near-duplicate detection.
- Repetitive log collapsing.
- Stack-trace normalization.
- JSON and structured payload compaction.
- Source-file outlining.
- Symbol extraction.
- Search-result ranking.
- Conversation-history pruning.
- Superseded-decision removal.
- Tool-output bounding.
- Task-relevance packing.
- Optional local-model summarization.
- Optional bring-your-own-model summarization.

### 10.3 Processing stages

1. Detect content and protocol type.
2. Normalize without changing meaning.
3. Calculate provider-appropriate token estimates.
4. Mark protected nodes.
5. Generate candidate strategy plans.
6. Rank plans by expected utility and risk.
7. Execute the selected plan within latency limits.
8. Validate structural and semantic invariants.
9. Fall back to original content on failure or uncertainty.
10. Produce an explanation and local operation record.

### 10.4 Protected content

Compresso must protect:

- Current user request.
- System and developer instructions.
- Active policies.
- Tool schemas.
- Tool arguments.
- Current code patches.
- Current errors.
- Explicit decisions.
- Security warnings.
- Content marked as immutable by the client.

### 10.5 Modes

- `safe`: deterministic deduplication, log collapsing, outlining, and budget packing.
- `balanced`: safe mode plus deterministic prose ranking and low-information removal.
- `aggressive`: stronger optional summarization, explicitly enabled.
- `passthrough`: no mutation, with local measurement only.

Safe mode is the default.

### 10.6 Safety checks

Reject a compressed result if:

- Protected nodes changed.
- Required JSON fields disappeared.
- A tool schema or tool call no longer validates.
- Code fences or structured content became malformed.
- File or symbol references became invalid.
- Output is larger than input without an explicit reason.
- The latency budget was exceeded.
- Strategy confidence is below policy.
- Provider or client format is unknown.

## 11. Routing engine

The router selects passthrough, compression, retrieval, outlining, or bounded expansion.

Inputs:

- Assistant and integration.
- Provider and model.
- Task classification.
- Context size and content types.
- Repository and Git state.
- User and team policy.
- Latency budget.
- Previous local strategy outcomes.

Outputs:

```text
passthrough
deduplicate
collapse_logs
outline_file
retrieve_symbols
pack_context
compress_prose
reject_operation
```

The first production router is deterministic. Learned routing may be added only after opt-in outcome data demonstrates that it improves task success.

## 12. Atlas repository intelligence

Atlas provides repository maps, search, structural outlines, symbol expansion, minimal context planning, and impact analysis.

### 12.1 Indexed data

- Repository identity and approved root.
- File path and content hash.
- Language, size, and token estimate.
- Imports and exports.
- Symbols, signatures, and line boundaries.
- Symbol references and calls.
- Test relationships.
- Git branch and status.
- Last indexed timestamp and parser version.

### 12.2 Indexing behavior

1. Resolve and approve the repository root.
2. Apply Git ignore rules and Compresso exclusions.
3. Exclude binaries, generated content, dependencies, and known secrets.
4. Hash files.
5. Parse changed files only.
6. Replace affected symbols and edges transactionally.
7. Recalculate impacted graph regions.
8. Record parser and schema versions.
9. Report staleness clearly.

### 12.3 Retrieval levels

```text
Level 1: repository map
Level 2: ranked search and file outline
Level 3: symbol body or bounded line range
```

### 12.4 Initial production languages

- TypeScript and JavaScript.
- Python.
- Rust.
- Go.
- Java.
- C and C++.
- C#.
- Ruby.
- PHP.

### 12.5 Atlas commands

```text
compresso index
compresso index --status
compresso map
compresso search "SessionManager"
compresso outline src/session.rs
compresso unfold src/session.rs SessionManager::start
compresso impact src/session.rs
compresso context "fix session timeout" --budget 4000
```

## 13. MCP server

### 13.1 Read-only tools

- `compresso_repo_map`
- `compresso_search`
- `compresso_read`
- `compresso_outline`
- `compresso_unfold`
- `compresso_context`
- `compresso_impact`
- `compresso_compress`
- `compresso_stats`
- `compresso_explain`

### 13.2 Local memory tools

- `compresso_remember`
- `compresso_recall`
- `compresso_forget`

Memory remains local unless the user explicitly enables encrypted synchronization.

### 13.3 Write-capable tools

- `compresso_edit`
- `compresso_batch_edit`

Write-capable tools require:

- Approved repository boundaries.
- Exact matching by default.
- Diff preview.
- Explicit client approval.
- Atomic file writes.
- Rollback records.
- Post-write validation.
- Refusal when matching is ambiguous.
- No shell execution.

### 13.4 MCP transports

- Stdio for assistant-launched sessions.
- Authenticated streamable HTTP on localhost.
- Session isolation and bounded concurrency.

MCP instructions must be brief and factual. Compresso may recommend bounded tools but must not attempt to override higher-level client policy.

## 14. Provider proxy

The production proxy supports:

- Anthropic Messages API.
- OpenAI Chat Completions API.
- OpenAI Responses API.
- Explicitly configured compatible gateways.
- Streaming requests and responses.
- Tool calls.
- Multimodal passthrough.
- Request-level bypass.
- Per-provider and per-model policies.

Requirements:

- Bind to localhost only.
- Never persist provider API keys.
- Preserve unknown headers and fields.
- Pass unsupported payloads through unchanged.
- Protect system prompts by default.
- Avoid buffering streaming responses.
- Redact secrets from diagnostic logs.
- Enforce body-size, concurrency, and timeout limits.
- Provide a process-wide `COMPRESSO_BYPASS=1` switch.
- Produce request-level local audit traces without storing raw prompts.

## 15. Assistant integrations

Each integration implements:

```rust
trait Integration {
    fn detect(&self) -> Detection;
    fn plan_install(&self) -> InstallPlan;
    fn install(&self, approval: Approval) -> Result<InstallReceipt>;
    fn verify(&self) -> VerificationReport;
    fn migrate(&self) -> Result<MigrationReport>;
    fn uninstall(&self) -> Result<RestoreReport>;
}
```

Initial clients:

- Claude Code.
- Claude Desktop.
- Codex CLI and Desktop.
- Cursor.
- VS Code-compatible MCP clients.
- Gemini CLI.
- OpenCode.
- Aider.

Every integration must:

- Detect existing configuration.
- Check compatible client versions.
- Generate a user-visible plan and diff.
- Create a timestamped backup.
- Modify only Compresso-owned keys.
- Save a key-level installation receipt.
- Verify that MCP or hooks load successfully.
- Migrate old Compresso configuration safely.
- Restore only Compresso-owned changes during uninstall.

## 16. Local storage

Recommended locations:

```text
~/.config/compresso/config.toml
~/.local/share/compresso/compresso.db
~/.cache/compresso/indexes/
~/.local/state/compresso/logs/
```

Use native equivalents on each platform.

SQLite tables:

```text
repositories
files
symbols
edges
sessions
operations
strategy_results
memory_facts
integration_receipts
index_versions
update_state
schema_migrations
```

Raw prompts and full source content are not stored by default. Local storage uses transactions, WAL mode, bounded retention, and versioned migrations.

## 17. Configuration

Example:

```toml
[compression]
mode = "safe"
target_tokens = 4000
max_latency_ms = 200

[index]
enabled = true
respect_gitignore = true
max_file_bytes = 1000000

[privacy]
store_raw_content = false
telemetry = false

[mcp]
transport = "stdio"

[proxy]
enabled = false
bind = "127.0.0.1:7410"
compress_system = false
```

Environment overrides:

```text
COMPRESSO_MODE
COMPRESSO_TARGET_TOKENS
COMPRESSO_TELEMETRY
COMPRESSO_BYPASS
COMPRESSO_LOG
```

Configuration precedence and the source of every effective value must be inspectable through `compresso config explain`.

## 18. Cloud control plane

Cloud responsibilities:

- Authentication.
- Organizations and memberships.
- Device registration and revocation.
- Subscription and billing state.
- Entitlements.
- License issuance and offline grace.
- Feature flags.
- Release manifests and update channels.
- Optional aggregate telemetry.
- Team policy management.
- Audit logs.
- Support administration.

Core compression must not depend on a live cloud request.

### 18.1 Offline behavior

- Permanent access to local/free features.
- Cached paid entitlements.
- At least 30 days of offline grace.
- Background refresh with exponential backoff.
- No interruption of active coding sessions.
- Clear, non-blocking entitlement status.

### 18.2 Cloud data model

```text
users
organizations
organization_members
subscriptions
plans
entitlements
devices
device_sessions
release_channels
releases
feature_flags
policy_sets
event_batches
audit_logs
support_cases
```

Cloud tables must not store source code, prompts, file paths, repository names, symbols, patches, or raw tool arguments.

## 19. Web products

### 19.1 Public website

- Product overview.
- Downloads.
- Documentation.
- Pricing.
- Security and privacy.
- Changelog.
- System status.

### 19.2 Account application

- Profile and authentication.
- Devices.
- Subscription.
- Organizations and members.
- Team policies.
- Data and privacy controls.
- Release channel selection.

### 19.3 Admin application

- Account lookup.
- Device revocation.
- Entitlement inspection.
- Release management.
- Feature flags.
- Event pipeline health.
- Support actions.
- Immutable admin audit log.

## 20. Plans and entitlements

### 20.1 Local

- Deterministic compression.
- Repository maps.
- Outlines and symbol retrieval.
- MCP integration.
- Local statistics.
- No account required.

### 20.2 Pro

- Advanced routing.
- All assistant integrations.
- Provider proxy.
- Desktop analytics.
- Local memory.
- Multiple devices.
- Priority updates.

### 20.3 Team

- Seat management.
- Shared policies.
- Organization controls.
- Aggregated privacy-safe metrics.
- SSO.
- Audit logs.
- Managed deployment.

### 20.4 Enterprise

- Offline licenses.
- Private update channel.
- MDM deployment.
- Custom retention.
- Support SLA.
- Security documentation.
- Optional self-hosted control plane.

Entitlements enable Compresso features. They must never disable or interfere with the underlying coding assistant.

## 21. Security and privacy

### 21.1 Threats

- Compromised release infrastructure.
- Malicious repository content.
- Prompt injection through source files.
- Path traversal and symlink escapes.
- Archive extraction attacks.
- Proxy credential leakage.
- Unauthorized local MCP clients.
- Configuration corruption.
- Sensitive telemetry.
- Malicious update or rollback behavior.
- Compromised cloud administrator accounts.

### 21.2 Required controls

- Signed release manifests and artifacts.
- Native platform code signing.
- Published SBOM and build provenance.
- Canonical workspace path validation.
- Symlink and traversal checks before writes.
- Localhost-only services.
- Authentication for HTTP MCP and local APIs.
- No shell execution through MCP inputs.
- Strict size, time, and concurrency limits.
- Secret detection and redaction.
- Atomic configuration writes.
- Key-level integration receipts and backups.
- Encryption in transit and at rest for cloud data.
- Short-lived cloud access tokens.
- OS keychain storage for device credentials.
- Role-based cloud authorization.
- Immutable administrative audit logs.
- Dependency, container, and infrastructure scanning in CI.

### 21.3 Telemetry policy

Remote telemetry is opt-in.

It must never include:

- Prompts.
- Source code.
- File paths.
- Repository names.
- Symbols.
- Patches.
- Tool arguments.
- Provider API keys.

Users must be able to inspect a sample payload, export local data, disable upload, and delete cloud-associated telemetry.

Savings fields remain separate:

```text
observed_tokens
applied_tokens
removed_tokens
compression_overhead
net_tokens_saved
modeled_cost_saved
provider_verified_cost_saved
```

## 22. Installer and distribution

The convenience installer may be:

```sh
curl -fsSL https://get.compresso.dev | sh
```

The installer only installs Compresso. It does not sign users in, collect telemetry, modify assistant configuration, or start onboarding automatically.

Installer process:

1. Detect operating system and architecture.
2. Resolve a version through a signed manifest.
3. Download an immutable artifact.
4. Verify manifest and artifact signatures.
5. Extract into a private temporary directory.
6. Validate archive paths.
7. Verify the embedded binary version.
8. Atomically install the new binary.
9. Preserve the previous version for rollback.
10. Print explicit next steps.

Distribution targets:

- Signed direct downloads.
- Homebrew.
- PowerShell installer.
- Windows package manager where practical.
- Cargo installation for developers.
- Managed enterprise packages.

## 23. Updates and releases

Release channels:

```text
stable
beta
nightly
enterprise
```

Update process:

1. Fetch signed release manifest.
2. Verify the manifest signature.
3. Select the correct platform artifact.
4. Download to a staging directory.
5. Verify artifact signature and digest.
6. Run compatibility and disk-space checks.
7. Stop local components safely.
8. Atomically switch binaries.
9. Run schema and configuration migrations.
10. Perform a local health check.
11. Roll back automatically on failure.

Production releases require:

- macOS Developer ID signing and notarization.
- Windows code signing.
- Linux signatures and checksums.
- SBOM.
- Build provenance.
- Dependency vulnerability scanning.
- Canary rollout.
- Automatic rollback.
- Published changelog and migration notes.

## 24. Observability and operations

### 24.1 Local observability

- Structured logs.
- Automatic redaction.
- Bounded rotating files.
- Health and diagnostics report.
- User-controlled diagnostic export.
- No raw prompt or source logging by default.

### 24.2 Cloud observability

- API latency and error rate.
- Database and connection pool health.
- Queue depth and processing lag.
- Authentication failure rate.
- Entitlement refresh health.
- Release download and signature failure rate.
- Billing webhook health.
- Email delivery failures.
- Feature flag and policy propagation health.

### 24.3 Service objectives

- Control-plane availability: 99.9%.
- Release download availability: 99.99%.
- P95 cloud API latency below 300 ms.
- P95 local compression latency below 200 ms.
- Local MCP startup below 500 ms.
- Zero cloud dependency in the active compression path.

### 24.4 Runbooks

Create runbooks for:

- Control-plane outage.
- Database degradation.
- Entitlement service failure.
- Billing webhook backlog.
- Release signing compromise.
- Bad client release rollback.
- Telemetry pipeline incident.
- Data deletion request.
- Device credential compromise.

## 25. Testing and evaluation

### 25.1 Core testing

- Unit tests.
- Property-based tests.
- Snapshot tests.
- Malformed-input tests.
- Token-budget invariants.
- Protected-content invariants.
- Compression inflation tests.
- Determinism tests.
- Parser fuzzing.

### 25.2 Integration testing

- Every supported assistant.
- Supported MCP protocol versions.
- Provider request formats.
- Streaming proxy behavior.
- Configuration migration.
- Install and uninstall restoration.
- Update and rollback.
- Local/cloud outage behavior.

### 25.3 Evaluation corpus

Include:

- Repository exploration tasks.
- Debugging tasks.
- Code reviews.
- Multi-file refactors.
- Test failures.
- Compiler output.
- Large repetitive logs.
- Monorepos.
- Multilingual repositories.
- Tool-call conversations.
- Prompt-injection repositories.
- Malformed provider payloads.

Measure:

- Token reduction.
- Task success.
- Information recall.
- Incorrect conclusions.
- Incorrect edits.
- Tool-call count.
- End-to-end latency.
- Passthrough rate.
- Strategy failure rate.
- User rollback rate.

### 25.4 Security testing

- Path traversal.
- Symlink escape.
- Archive traversal.
- Malicious MCP arguments.
- Proxy credential leakage.
- Local API authentication.
- Configuration injection.
- Malicious repository instructions.
- Release-signing compromise simulation.

### 25.5 Platform matrix

- macOS ARM64 and x86_64.
- Windows x86_64 and ARM64 where practical.
- Linux x86_64 and ARM64.
- Common shells.
- Clean machines.
- Existing complex assistant configurations.

### 25.6 Production release gates

- Zero protected-node mutations in the evaluation suite.
- Zero malformed tool or provider payloads.
- Zero writes outside approved roots.
- Safe mode median token reduction above 25% on eligible fixtures.
- Safe mode task-success degradation below 1%.
- P95 local compression below 200 ms for normal payloads.
- Incremental indexing processes only changed files.
- Integration installation is idempotent.
- Uninstall restores all Compresso-owned changes.
- Signed update verification cannot be bypassed silently.
- Cloud outage does not interrupt local compression.

## 26. Delivery roadmap

### Phase 1: Product and platform foundation, weeks 1-4

Deliverables:

- Final product requirements.
- Brand, domain, and package validation.
- Monorepo and build system.
- Local and cloud schema foundations.
- CI/CD and development environments.
- Authentication foundation.
- Signed development builds.
- Threat model.
- Architecture decision records.
- Test and fixture conventions.

Exit criteria:

- Reproducible local builds.
- macOS, Windows, and Linux CI builds.
- Development cloud environment deployed.
- Schema round-trip tests.
- Initial security review complete.

### Phase 2: Compression core, weeks 5-8

Deliverables:

- ContextGraph.
- Token counter abstraction.
- Protected-node policy.
- Strategy interface.
- Exact deduplication.
- Log collapsing.
- Structured payload compaction.
- Budget packing.
- Safe and passthrough modes.
- Compression traces and explanations.
- Evaluation harness.

Exit criteria:

- Deterministic output.
- All safety invariants enforced.
- Unsafe operations fall back cleanly.
- Baseline evaluation metrics recorded.

### Phase 3: Atlas, weeks 9-13

Deliverables:

- Repository approval and scanning.
- Git ignore support.
- Tree-sitter integration.
- SQLite index schema.
- Incremental indexing.
- Repository maps.
- Ranked search.
- File outlines.
- Symbol unfolding.
- Impact analysis.
- Minimal context planning.

Exit criteria:

- Changed-file-only refresh works.
- Supported languages produce stable symbol maps.
- Repository boundaries are enforced.
- Index versioning and recovery work.

### Phase 4: Local runtime and MCP, weeks 14-16

Deliverables:

- Local supervisor.
- IPC.
- Stdio MCP server.
- Authenticated local HTTP MCP.
- Read-only tool catalog.
- Sessions and operation ledger.
- Health subsystem.
- Embedded fallback mode.

Exit criteria:

- MCP initializes in target clients.
- Tool schemas are versioned and validated.
- MCP failure does not prevent assistant startup.
- Read-only tools cannot write files.

### Phase 5: Desktop and integrations, weeks 17-20

Deliverables:

- Tauri desktop application.
- Onboarding.
- Assistant detection.
- Dry-run integration plans.
- Backups and key-level receipts.
- Claude, Codex, and Cursor integrations.
- Repository management UI.
- Compression Explorer.
- Diagnostics and local update UI.

Exit criteria:

- Installation is idempotent.
- Uninstall preserves unrelated configuration.
- Fresh users complete onboarding without terminal knowledge.
- Integration health is visible and actionable.

### Phase 6: Cloud and commercial product, weeks 21-23

Deliverables:

- Public website.
- Account web application.
- Control-plane API.
- Organizations and devices.
- Billing and subscriptions.
- Entitlements and offline grace.
- Team policies.
- Admin console.
- Transactional email.

Exit criteria:

- Complete account and purchase flow.
- Entitlements cache locally.
- Cloud outage does not block local operation.
- Administrative actions are audited.

### Phase 7: Proxy and advanced capabilities, weeks 24-25

Deliverables:

- Anthropic and OpenAI proxy adapters.
- Streaming support.
- Request-level bypass.
- Local memory.
- Advanced routing.
- Controlled edit tools.
- Team policy enforcement.

Exit criteria:

- Unknown payloads pass through unchanged.
- Provider keys are never persisted.
- Write tools require approval and remain workspace-bounded.
- Streaming behavior matches direct provider behavior.

### Phase 8: Security, reliability, and release engineering, weeks 26-28

Deliverables:

- Signed release pipeline.
- macOS notarization.
- Windows signing.
- Installers and package manager distributions.
- Update rollback.
- Migration testing.
- Load and fault tests.
- Backup and recovery procedures.
- Privacy review.
- Operational runbooks.

Exit criteria:

- External or independent security review completed.
- Critical findings resolved.
- Rollback tested on all platforms.
- Release compromise and outage drills completed.

### Phase 9: Beta and production launch, weeks 29-30

Deliverables:

- Closed beta.
- Compatibility and quality fixes.
- Documentation.
- Support workflows.
- Status page.
- Stable release channel.
- Public launch.

Exit criteria:

- Production gates pass.
- Beta users complete real coding sessions successfully.
- No unresolved critical or high-severity security issues.
- Support, incident, and rollback ownership is assigned.

## 27. Team plan

Recommended team:

- Two Rust engineers for compression, Atlas, MCP, and proxy.
- One desktop and integration engineer.
- One full-stack engineer for web and control plane.
- One infrastructure and security engineer.
- One product designer.
- One product and QA lead.

Roles may be combined early, but compression evaluation, integration compatibility, security, and cross-platform release engineering require explicit ownership.

## 28. Architecture decision records

Create ADRs for:

- Rust modular local runtime.
- TypeScript modular cloud monolith.
- Local-first operation.
- SQLite local storage.
- PostgreSQL cloud storage.
- Deterministic compression before model-based summarization.
- MCP as the primary integration protocol.
- Fail-open passthrough.
- Tree-sitter symbol extraction.
- Tauri desktop application.
- No account requirement for local features.
- Opt-in telemetry.
- Signed releases and update rollback.
- Localhost-only proxy.
- Cached entitlements and offline grace.

## 29. Major risks and mitigations

| Risk | Mitigation |
|---|---|
| Compression removes critical context | Protected nodes, deterministic safe mode, passthrough |
| Token savings reduce task success | Task-based evaluation and release gates |
| Index becomes stale | Content hashes, parser versions, staleness reporting |
| MCP instructions distort agent behavior | Brief factual instructions and optional use |
| Integrations damage user configuration | Dry runs, atomic writes, key-level receipts |
| Proxy leaks credentials | Local-only operation, no key persistence, redaction |
| Licensing outage breaks coding | Cached entitlement and permanent local features |
| Installer is compromised | Signatures, provenance, native platform signing |
| Product scope becomes unmanageable | Independent workstreams and explicit release gates |
| Cross-platform behavior diverges | Mandatory platform CI and clean-machine tests |
| Savings claims become misleading | Separate observed, applied, modeled, and verified metrics |
| Cloud receives sensitive code metadata | Strict schemas, allowlists, privacy tests |

## 30. Implementation backlog

### Foundation

- Initialize Rust and TypeScript workspaces.
- Configure cross-platform CI.
- Define coding, schema, and error conventions.
- Create architecture and threat-model documents.
- Provision development cloud infrastructure.
- Implement signed development build pipeline.

### Core engine

- Implement ContextGraph types and validation.
- Implement token counting abstraction.
- Implement protected-node invariants.
- Implement strategy registry.
- Implement exact deduplication.
- Implement log collapse.
- Implement structured payload compaction.
- Implement budget packing.
- Implement routing policy.
- Implement trace generation and explanation.
- Implement evaluation runner.

### Atlas

- Implement repository root approval.
- Implement ignore rules and secret exclusions.
- Implement file hashing.
- Implement Tree-sitter parser registry.
- Implement symbol and edge extraction.
- Implement SQLite migrations.
- Implement incremental indexing.
- Implement map, search, outline, unfold, and impact.
- Implement context planning.

### Local platform

- Implement supervisor lifecycle.
- Implement IPC and local authentication.
- Implement MCP transports.
- Implement read-only MCP tools.
- Implement sessions and local metrics.
- Implement desktop shell.
- Implement onboarding and diagnostics.
- Implement integration framework.
- Implement Claude, Codex, and Cursor adapters.

### Cloud platform

- Implement authentication and device registration.
- Implement organizations and memberships.
- Implement plans and entitlements.
- Implement billing integration.
- Implement release metadata.
- Implement feature flags and policies.
- Implement admin audit logs.
- Implement public and authenticated web applications.

### Advanced local features

- Implement provider proxy.
- Implement streaming adapters.
- Implement local memory.
- Implement advanced routing.
- Implement controlled edit tools.
- Implement team policy synchronization.

### Production operations

- Implement installer scripts.
- Implement platform code signing.
- Implement updater and rollback.
- Publish SBOM and provenance.
- Add monitoring and alerts.
- Write incident runbooks.
- Complete privacy and security reviews.
- Execute closed beta and launch process.

## 31. Production definition of done

Compresso is a complete running product when:

- A new user can discover, install, configure, use, update, and uninstall it.
- Releases and updates are cryptographically signed.
- Claude, Codex, and Cursor integrations work reliably.
- Repository indexing is incremental, bounded, and recoverable.
- Context reduction is measurable and explainable.
- Safe mode does not materially reduce task success.
- The local product continues working during cloud outages.
- Account, subscription, entitlement, organization, and device flows work.
- Team policies and administrative audit logs work.
- Configuration changes are reversible.
- Updates automatically roll back after failed health checks.
- Privacy controls are clear and enforceable.
- Monitoring, incident response, and customer support workflows exist.
- macOS, Windows, and Linux distribution paths are operational.
- Production release gates pass with no unresolved critical or high-severity security issues.

## 32. Immediate next actions

1. Confirm the product assumptions, team size, and expected launch window.
2. Validate the Compresso name, domains, and package identifiers.
3. Create the monorepo structure.
4. Write the first architecture decision records.
5. Define and version the ContextGraph schema.
6. Create the evaluation fixture format before implementing compression.
7. Implement the Rust core with protected-node invariants.
8. Establish signed CI builds from the first internal release.
9. Build Atlas and read-only MCP as the first complete vertical product slice.
10. Add desktop, cloud, proxy, commercial, and operational workstreams according to the roadmap above.
