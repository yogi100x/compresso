# Compresso WOZ-Inspired Add-on Plan

## 1. Purpose

This document specifies how Compresso can implement the useful product capabilities observed in WOZCODE without copying proprietary source code, prompts, branding, or obfuscated implementation details.

The work must use a clean-room approach:

- Treat public documentation, public manifests, and runtime protocol schemas as behavioral requirements only.
- Design original interfaces and implementations inside Compresso.
- Do not copy WOZCODE source, bundled prompts, marketing language, or internal identifiers.
- Do not depend on WOZCODE at runtime.
- Validate Compresso against independently created fixtures and product requirements.

This add-on extends the main production plan in `compresso-build.md`.

## 2. Evidence and confidence levels

The feature inventory below comes from:

- The `@wozcode/install` npm package metadata and launcher.
- The public `WithWoz/wozcode-plugin` repository.
- Claude plugin manifests and hook definitions.
- Public agent and skill definitions.
- An isolated MCP initialization and `tools/list` handshake with telemetry disabled.
- Static inspection of package structure and bundled dependencies.

Confidence labels:

- **Confirmed:** directly exposed in manifests, public skill contracts, or MCP schemas.
- **Strong inference:** implementation is indicated by bundled components and public behavior.
- **Unknown:** obfuscation or missing source prevents reliable implementation-level confirmation.

## 3. High-level WOZCODE product architecture

WOZCODE is not only a search plugin. It behaves as a replacement coding-agent layer:

```text
Claude Code / Codex
        |
        | default agent selection
        v
WOZ coding agent
        |
        +-- Search MCP tool
        +-- Edit MCP tool
        +-- SQL MCP tool
        +-- Recall MCP tool
        +-- controlled Bash capability
        |
        +-- Explore subagent
        +-- Review personas
        +-- Knowledge base
        +-- Savings and benchmark system
        +-- Authentication and entitlements
        +-- Session hooks and telemetry
```

The current product replaces several native Claude tools for its main agent. Compresso should provide equivalent capabilities without forcing replacement or disabling native tools by default.

## 4. Confirmed feature inventory

### 4.1 Installer and marketplace integration

The npm installer:

1. Requires Node.js 22.5 or newer.
2. Verifies that the Claude CLI is on `PATH`.
3. Adds the public WOZCODE marketplace repository.
4. Installs the WOZCODE Claude plugin.
5. Prints restart instructions.

The installer does not pin the second-stage plugin to a specific immutable commit. Compresso must improve this with signed and version-pinned installation.

### 4.2 Main and fallback agents

WOZCODE exposes three agent profiles:

#### Main coding agent

- Uses the current primary model.
- Uses WOZ search, edit, and SQL tools.
- Disables native Read, Edit, Write, Grep, Glob, and NotebookEdit.

#### Free-plan fallback agent

- Uses native Claude tools.
- Disables WOZ Search, Edit, and SQL after a plan cap is exhausted.

#### Explore agent

- Uses a cheaper model.
- Is read-only.
- Handles symbol lookup, definitions, references, callers, and architecture exploration.
- Is expected to complete bounded exploration in a small number of tool calls.

Compresso should implement agent profiles as optional policy presets, not hard-coded replacements.

### 4.3 Combined Search tool

The Search tool combines:

- File discovery.
- Glob matching.
- Regular-expression content search.
- Full or partial file reading.
- Line-range reads.
- Image return for visual analysis.
- PDF text extraction.
- File-type filtering.
- Result limits.
- Per-file line limits.
- Long-line truncation.
- Case-insensitive search.
- Multiline regular expressions.
- Modified-since caching.
- TypeScript/JavaScript structural summaries.
- TypeScript/JavaScript importer discovery.

Its central optimization is combining discovery and reading in one tool call.

Confirmed input concepts:

```text
content pattern
file paths or glob patterns
output mode
lines before and after matches
case sensitivity
file type
file limit
lines per file
modified-since timestamp
maximum line length
multiline matching
structural summary mode
importer listing
```

Output modes include:

```text
file paths with content
file paths only
file paths with match counts
```

### 4.4 Batch Edit tool

The Edit tool supports:

- Multiple edits across multiple files in one call.
- Exact and fuzzy search/replace.
- File creation.
- Whole-file overwrite.
- Replace-all.
- Line-range-scoped matching.
- Sequential edits where later edits see earlier results.
- Jupyter notebook cell editing.
- Notebook cell insertion, deletion, movement, and type changes.
- Post-edit syntax validation.

WOZCODE encourages batching to reduce model round trips.

Compresso should implement the batching benefit but use safer defaults:

- Exact matching by default.
- Fuzzy matching only when explicitly enabled.
- Refuse ambiguous matches.
- Produce a diff before committing writes.
- Keep an atomic rollback record.
- Restrict all relative and absolute paths to approved roots.

### 4.5 SQL tool

The SQL tool supports:

- PostgreSQL.
- MySQL and MariaDB.
- SQLite.
- Schema discovery.
- Table inspection.
- Function inspection.
- Enum and type inspection.
- Relationship discovery.
- Schema search.
- SQL linting.
- Live query execution.
- Multiple statements.
- Batches of named queries.
- Automatic row limits.
- Configurable statement timeouts.
- CSV, columnar JSON, and table output.
- Cell and line-width limits.

Confirmed actions include:

```text
tables
table
functions
function
enums
types
relationships
search
lint
query
connect
```

Compresso should separate read-only inspection from destructive execution. The default connection role should be read-only, and write-capable statements should require explicit approval.

### 4.6 Session Recall

Recall semantically searches past coding sessions for:

- Commands.
- Solutions.
- Explanations.
- Decisions.
- Prior context.

The public schema exposes a natural-language query and result limit. The package describes the implementation as compressed semantic search and ships a background recall index worker.

Compresso should keep this local by default and index only approved session sources.

### 4.7 Repository exploration subagent

The Explore agent:

- Uses a cheaper model tier.
- Has Search, SQL, and limited Bash access.
- Cannot edit.
- Returns dense path-and-line findings.
- Distinguishes definitions, references, and callers.
- Uses parallel searches for independent questions.
- Discourages broad file reads before locating relevant entry points.

Compresso can reproduce this as a routing policy and output contract rather than a branded agent.

### 4.8 Syntax validation

Confirmed validation surfaces include:

- Tree-sitter validation.
- TypeScript validation.
- JSON validation.
- YAML validation.
- HTML validation.

The plugin bundles parsers for:

- C.
- C++.
- Go.
- Java.
- Python.
- Rust.
- Markdown.

TypeScript and JavaScript behavior appears to use a separate native query parser and TypeScript-aware implementation.

### 4.9 Deep review system

The deep reviewer uses multiple narrow review personas and a final cross-cutting pass.

Confirmed review lenses:

- Cross-file consistency.
- Duplication and DRY violations.
- Existing codebase and schema reuse.
- Type safety.
- SDK and library type reuse.
- Correctness and edge cases.
- Comments and documentation hygiene.
- Cross-cutting synthesis.

Supported modes include:

- Read-only report.
- Save report.
- Apply high-confidence findings.
- Interactive finding selection.
- Persona subset selection.
- Model override.
- Alternate repository path.

The reviewer examines committed, staged, working-tree, and untracked changes against a base branch.

### 4.10 Live review

The package includes a PostToolUse reviewer hook for edit operations.

Confirmed controls include:

- Live reviewer enablement.
- Reviewer model selection.
- Deep review every N edits.
- Configurable review interval.
- Dedicated deep-review model.

Compresso should make live review opt-in because it adds latency, cost, and a second automated actor after every edit.

### 4.11 Knowledge base

The knowledge system has three scopes:

```text
company
repository
personal
```

Confirmed operations:

- Status.
- Query.
- Add note.
- Remove note.
- Suppress a result.
- Undo suppression.
- Boost a result.
- Undo boost.
- Ingest a file.
- Refresh an index.
- Inspect personalization operations.

Additional capabilities:

- Repository architecture manifests.
- Cross-repository planning briefs.
- Optional code and pull-request grounding.
- Pull-request history ingestion.
- Distillation of human review comments.
- Reviewer backtesting.
- Reviewer persona tuning.
- Organization-wide tuning.
- A/B comparison and automatic rollback.

### 4.12 Reviewer backtesting

The backtesting system:

- Samples historical merged pull requests.
- Reviews a clean version without exposing human comments to the reviewer.
- Compares generated findings against human review feedback and merged changes.
- Measures recall and precision.
- Persists reports and per-round artifacts.
- Supports model and persona comparisons.
- Can tune reviewer hints and re-measure lift.

It attempts to isolate test clones by removing remotes, rewriting push URLs, stripping GitHub credentials, and sandboxing user-home access.

This evaluation capability is strategically important for Compresso because it measures quality instead of only token reduction.

### 4.13 Savings reports

The savings system reports:

- Calls or round trips saved.
- Time saved.
- Tokens saved.
- Cost saved.
- Session totals.
- Lifetime totals.
- Per-project breakdowns.
- Workflow and task-type breakdowns.
- Trends.

It can scan local Claude session history for deeper analysis.

Compresso must keep observed, applied, modeled, and provider-verified savings separate.

### 4.14 Controlled benchmark system

The benchmark system compares WOZCODE and vanilla Claude behavior on the same repository and prompts.

Confirmed outputs include:

- Cost comparison.
- Token comparison.
- Turn count.
- Elapsed time.
- Detailed JSON report.

Compresso should add correctness and test outcomes so cost reduction is never presented without quality context.

### 4.15 Authentication and entitlements

Confirmed authentication methods:

- Browser login.
- Token login.
- API-key login.
- Environment-based API key for CI/headless use.
- Automatic token refresh.
- Logout and credential clearing.
- Account and subscription status.

Confirmed entitlement concepts:

- Free-plan cap.
- Paid access.
- Organization membership.
- Knowledge-base access.
- Reviewer access.

The package includes OIDC/SSO-related chunks and Supabase dependencies. The exact credential storage implementation is not reliably auditable from the obfuscated bundle.

### 4.16 Settings and desktop controls

Confirmed settings include:

- Commit and pull-request attribution.
- Status line.
- Session and lifetime savings display.
- Status tips and share hints.
- Custom spinner language.
- Eager or deferred MCP tool loading.
- Recall enablement.
- Syntax validation.
- Live reviewer controls.
- Master plugin enablement.
- macOS menu-bar application enablement.

The distribution also contains desktop, terminal UI, pseudo-terminal, and co-drive components.

### 4.17 Hooks

Confirmed hook events:

- SessionStart.
- UserPromptSubmit.
- PreToolUse.
- PostToolUse.
- SubagentStop.
- Stop.
- StopFailure.
- PreCompact.
- PostCompact.

The hooks support:

- Session setup.
- Agent and entitlement selection.
- Reviewer triggering.
- Savings tracking.
- Recall indexing.
- Telemetry.
- Update checks.
- Status-line behavior.

Compresso should use the fewest hooks required and make each hook independently observable and disableable.

### 4.18 Telemetry and feedback

The plugin enables PostHog telemetry by default and runs telemetry hooks throughout the session lifecycle.

Published behavior says telemetry includes operational metrics such as token use and tool invocation counts, not source code or prompt content. Bug reports can attach:

- Session identifier.
- Anonymous telemetry identifier.
- Operating system.
- Architecture.
- Node runtime version.
- Logged-in account email.

Compresso telemetry must be opt-in and must expose the exact payload before upload.

### 4.19 Other bundled capabilities

The distribution indicates additional features:

- Browser automation through Playwright.
- Image support.
- PDF extraction.
- PostgreSQL query parsing through a native module.
- Terminal and pseudo-terminal control.
- Co-drive controller and IPC server.
- Architecture-document workers.
- Baseline scan workers.
- Knowledge refresh workers.
- Recall index workers.
- Background router daemon.
- macOS menu-bar or desktop application.
- Codex-specific packaging and hooks.

These are confirmed as shipped components, but some detailed behavior remains unknown because the main bundles are obfuscated.

## 5. Clean-room Compresso architecture

Map the observed capabilities into original Compresso modules:

```text
compresso-search
  discovery, regex search, bounded reads, summaries, importers

compresso-edit
  batch changes, preview, validation, atomic commit, rollback

compresso-sql
  schema graph, linting, read-only queries, approved mutations

compresso-recall
  local session ingestion, embeddings, semantic retrieval

compresso-review
  diff collection, personas, synthesis, findings, patch approval

compresso-knowledge
  personal, repository, and organization knowledge layers

compresso-eval
  benchmarks, historical review backtests, quality gates

compresso-integrations
  Claude, Codex, Cursor, hooks, MCP registration

compresso-control-plane
  accounts, organizations, entitlements, device auth, billing
```

## 6. Compresso Search specification

### 6.1 Goals

- Replace multi-step discovery/search/read sequences with one bounded operation.
- Preserve precise path and line references.
- Return enough context to act without flooding the model.
- Avoid re-sending unchanged content.
- Support text, code, images, notebooks, and PDFs through typed outputs.

### 6.2 Proposed request

```rust
struct SearchRequest {
    roots: Vec<PathBuf>,
    paths: Vec<String>,
    pattern: Option<String>,
    file_types: Vec<String>,
    output: SearchOutputMode,
    context_before: usize,
    context_after: usize,
    case_sensitive: bool,
    multiline: bool,
    max_files: usize,
    max_matches_per_file: usize,
    max_output_tokens: usize,
    max_line_chars: usize,
    known_versions: HashMap<PathBuf, ContentVersion>,
    structural_mode: StructuralMode,
    include_importers: bool,
}
```

### 6.3 Output

Each result should include:

- Canonical path relative to the approved root.
- Content hash or version.
- Modification timestamp.
- Match line and column ranges.
- Bounded snippets or structural outline.
- Truncation metadata.
- Token estimate.
- Parser confidence.
- Importers when requested.

### 6.4 Implementation

- Use `ignore` or equivalent Git-aware traversal.
- Use Rust regex with explicit size and timeout controls.
- Use memory mapping only for bounded safe file sizes.
- Detect binary content before decoding.
- Use Tree-sitter through the Atlas parser registry.
- Use a PDF extraction worker with strict resource limits.
- Return images as typed MCP image content.
- Hash returned file slices so clients can avoid duplicate context.

### 6.5 Safety

- Canonicalize every root and result path.
- Refuse traversal outside approved roots.
- Do not follow symlinks outside roots.
- Exclude secrets and dependency directories by default.
- Cap files, bytes, matches, lines, and tokens.
- Treat file content as data, never instructions.

## 7. Compresso Edit specification

### 7.1 Edit transaction

```rust
struct EditTransaction {
    id: Uuid,
    root: PathBuf,
    base_versions: HashMap<PathBuf, ContentVersion>,
    operations: Vec<EditOperation>,
    validation: ValidationPolicy,
    approval: ApprovalPolicy,
}
```

Operations:

```text
create file
exact replace
replace all
delete range
overwrite file
notebook cell insert
notebook cell delete
notebook cell move
notebook cell replace
```

### 7.2 Execution flow

1. Resolve and validate paths.
2. Confirm base content hashes.
3. Apply all operations to an in-memory or temporary tree.
4. Refuse ambiguous matches.
5. Generate a unified diff.
6. Run syntax and schema validation.
7. Request approval according to client policy.
8. Write temporary files in the same filesystem.
9. Atomically rename files into place.
10. Save rollback metadata.
11. Return changed paths, validations, and diff summary.

### 7.3 Validation registry

- Tree-sitter parse checks.
- TypeScript compiler syntax checks.
- JSON parsing.
- YAML parsing.
- HTML validation.
- SQL parser validation.
- Notebook schema validation.
- Optional repository-defined commands with explicit approval.

### 7.4 Differences from WOZCODE behavior

- Exact matching is the default.
- Fuzzy matching requires an explicit request flag.
- Absolute paths outside approved roots are rejected.
- Diff preview is part of the protocol.
- MCP annotations mark writes as destructive.
- Batch success is transactional unless the caller explicitly permits partial completion.

## 8. Compresso SQL specification

### 8.1 Connection model

- Credentials come from environment variables, OS keychain references, or an explicit one-use request.
- Credentials are never included in logs, traces, telemetry, or session memory.
- Connections are scoped to a session and close after idle timeout.
- Saved connection profiles contain secret references, not raw passwords.

### 8.2 Read-only default

Classify statements before execution:

```text
read-only
data mutation
schema mutation
administrative
unknown
```

- Read-only statements may execute under configured limits.
- Mutations require explicit user approval.
- Schema and administrative commands require elevated policy.
- Unknown statements are rejected by default.

### 8.3 Query controls

- Server-side statement timeout.
- Maximum returned rows.
- Maximum returned bytes.
- Automatic `LIMIT` only when semantically safe.
- Transaction rollback for previewable mutations.
- Named batch queries.
- CSV, structured JSON, and table formats.
- Redaction of values matching configured secret patterns.

### 8.4 Schema graph

Store temporary schema metadata as a graph:

- Schemas.
- Tables and views.
- Columns and types.
- Primary and foreign keys.
- Indexes.
- Functions.
- Enums and custom types.
- Relationship edges.

This enables task-oriented schema retrieval without repeatedly querying system catalogs.

## 9. Compresso Recall specification

### 9.1 Sources

- Compresso session traces.
- Approved Claude history.
- Approved Codex history.
- User-imported notes.
- Explicitly saved decisions.

### 9.2 Pipeline

1. Discover approved session files.
2. Parse sessions into turns, tool events, decisions, commands, and outcomes.
3. Redact secrets.
4. Chunk semantically.
5. Generate local embeddings or lexical vectors.
6. Store encrypted local index entries.
7. Rank by semantic relevance, recency, project, and outcome confidence.
8. Return bounded excerpts with provenance.

### 9.3 Privacy

- Disabled until explicitly enabled.
- Local-only by default.
- Per-source and per-project controls.
- Retention limits.
- Delete and reindex commands.
- No session content in remote telemetry.
- Optional encrypted synchronization designed separately.

## 10. Compresso Review specification

### 10.1 Review inputs

- Base branch.
- Committed diff.
- Staged diff.
- Working-tree diff.
- Untracked files.
- Repository architecture context.
- Relevant local knowledge.
- User-specified review lenses.

### 10.2 Review personas

Implement original Compresso personas:

- Behavioral correctness.
- Boundary and failure handling.
- Data and type integrity.
- Security and privacy.
- Existing abstraction reuse.
- Duplication and maintainability.
- Tests and observability.
- Documentation and migration safety.

### 10.3 Pipeline

```text
collect change set
  -> classify files and risk
  -> retrieve bounded context per persona
  -> run narrow reviewers in parallel
  -> normalize and deduplicate findings
  -> run cross-cutting synthesis
  -> validate file and line references
  -> rank by severity and confidence
  -> present or apply with approval
```

### 10.4 Finding schema

```rust
struct ReviewFinding {
    id: Uuid,
    persona: String,
    severity: Severity,
    confidence: f32,
    title: String,
    explanation: String,
    path: PathBuf,
    line: Option<u32>,
    evidence: Vec<EvidenceRef>,
    suggested_change: Option<Patch>,
    validation: Vec<ValidationResult>,
}
```

### 10.5 Application modes

- Report only by default.
- Save report.
- Interactive selection.
- Apply selected findings.
- Apply high-confidence findings only after explicit approval.

## 11. Compresso Knowledge specification

### 11.1 Layers

```text
personal
repository
organization
```

### 11.2 Knowledge item

```rust
struct KnowledgeItem {
    id: Uuid,
    scope: KnowledgeScope,
    kind: KnowledgeKind,
    content: String,
    source: Provenance,
    confidence: f32,
    created_at: DateTime<Utc>,
    expires_at: Option<DateTime<Utc>>,
    repository: Option<RepositoryId>,
    tags: Vec<String>,
}
```

### 11.3 User operations

- Add a note.
- Edit or delete a note.
- Suppress an item.
- Boost an item.
- Undo every operation.
- Inspect operation history.
- Import a file.
- Refresh derived knowledge.

### 11.4 Provenance

Every item must show:

- Who or what created it.
- Source repository or document.
- Source revision.
- Whether it is user-authored or inferred.
- Confidence.
- Last validation time.
- Whether a user boosted or suppressed it.

## 12. Compresso evaluation and tuning

### 12.1 Historical review backtest

For each selected pull request:

1. Create an isolated worktree or clone.
2. Remove write-capable remotes.
3. Remove Git hosting credentials.
4. Provide the pre-change code and proposed diff to reviewers.
5. Hide human review comments and final corrections.
6. Run Compresso reviewers.
7. Compare findings with human comments and merged fixes.
8. Calculate precision, recall, severity-weighted recall, and cost.

### 12.2 Compression benchmark

Compare:

```text
assistant baseline
Compresso safe mode
Compresso balanced mode
Compresso retrieval-first mode
```

Measure:

- Task success.
- Tests passed.
- Correctness of edits.
- Token use.
- Cost.
- Turns.
- Latency.
- Context precision.
- Context recall.
- User intervention.

### 12.3 Tuning safety

- Tuning changes are versioned.
- Every new policy receives an A/B backtest.
- Regression thresholds block rollout.
- Automatic rollback is supported.
- Training repositories and evaluation repositories remain separate.

## 13. Integration behavior

Compresso should not set itself as the default coding agent during installation.

Provide explicit modes:

```text
assist
  Compresso tools are available alongside native tools.

prefer
  Agent instructions prefer Compresso for large or broad operations.

enforce
  Organization policy may restrict selected native operations.
```

`assist` is the default. `enforce` requires explicit administrator and user visibility.

## 14. Product improvements over the observed design

### 14.1 Trust

- Signed, immutable releases.
- No mutable second-stage installer.
- Published SBOM and provenance.
- Auditable source for security-sensitive local components.

### 14.2 Privacy

- Telemetry off by default.
- Inspectable payloads.
- Local recall disabled until enabled.
- No source or prompt content uploaded.
- No mandatory telemetry tied to a free plan.

### 14.3 Tool safety

- Read-only MCP tools first.
- Exact edits by default.
- Approved workspace boundaries.
- SQL read-only by default.
- Diff previews and rollback.
- Accurate MCP destructive annotations.

### 14.4 User control

- Do not disable native tools by default.
- One global bypass switch.
- Per-feature controls.
- Visible hook health and latency.
- Reversible configuration changes.

### 14.5 Measurement integrity

- Task quality shown next to savings.
- Applied savings separated from opportunity estimates.
- Provider-verified usage separated from modeled pricing.
- Benchmarks include failures and regressions.

## 15. Delivery sequence

### Workstream A: Search and structural retrieval

1. Implement approved-root traversal.
2. Implement combined glob and regex search.
3. Implement bounded reads and line ranges.
4. Add output modes and token caps.
5. Add content hashes and unchanged markers.
6. Add Atlas structural summaries.
7. Add importer relationships.
8. Add PDF and image adapters.

Exit criteria:

- One Search call can replace discovery plus read for common tasks.
- Every output is bounded and references exact paths and lines.
- No path escapes approved roots.

### Workstream B: Safe batch editing

1. Define edit transaction schemas.
2. Implement exact multi-file replacements.
3. Implement create, overwrite, delete, and replace-all.
4. Add diff preview.
5. Add syntax validation registry.
6. Add atomic commit and rollback.
7. Add notebook operations.
8. Add optional fuzzy matching.

Exit criteria:

- Multi-file edits are atomic.
- Ambiguous matches fail.
- Every committed change has a rollback record.

### Workstream C: SQL intelligence

1. Implement connection profiles and secret references.
2. Implement PostgreSQL schema discovery.
3. Add SQLite and MySQL adapters.
4. Build schema relationship graph.
5. Add SQL parser and classifier.
6. Add bounded read-only query execution.
7. Add named batches and output formats.
8. Add approved mutation mode.

Exit criteria:

- Default operation cannot mutate a database.
- Query output is bounded by rows, bytes, and time.
- Credentials never enter logs or traces.

### Workstream D: Recall and knowledge

1. Define approved session sources.
2. Implement local transcript parsing.
3. Add secret redaction.
4. Build lexical search.
5. Add local semantic indexing.
6. Add provenance and bounded results.
7. Add personal and repository knowledge.
8. Add organization knowledge synchronization.

Exit criteria:

- Recall is fully local by default.
- Every result includes provenance.
- Users can delete and rebuild all indexed data.

### Workstream E: Review and evaluation

1. Implement change-set collection.
2. Implement risk and file classification.
3. Implement parallel review personas.
4. Implement synthesis and deduplication.
5. Add report, interactive, and apply modes.
6. Build historical pull-request backtesting.
7. Add quality and cost metrics.
8. Add versioned tuning and rollback.

Exit criteria:

- Findings use verified file and line references.
- Review quality is measured against held-out historical changes.
- Applying findings always requires explicit approval.

## 16. Suggested Compresso MCP tools

Avoid one overly broad server contract. Use explicit tools:

```text
compresso_search
compresso_read
compresso_outline
compresso_unfold
compresso_importers
compresso_edit_plan
compresso_edit_apply
compresso_edit_rollback
compresso_sql_schema
compresso_sql_query
compresso_sql_mutate
compresso_recall
compresso_remember
compresso_review
compresso_review_apply
compresso_stats
compresso_explain
```

Separating plan/apply and query/mutate makes permissions understandable to clients and users.

## 17. Implementation priorities

Priority 0:

- Combined Search.
- Bounded Read.
- Structural Outline and Unfold.
- Path security.
- Token-aware output limits.

Priority 1:

- Transactional batch Edit.
- Syntax validation.
- SQL schema discovery and read-only queries.
- Local Recall.

Priority 2:

- Multi-persona review.
- Historical review backtesting.
- Knowledge layers.
- Savings and benchmark dashboard.

Priority 3:

- Controlled mutation SQL.
- Notebook editing.
- Browser/PDF workflows.
- Terminal co-drive.
- Organization-wide reviewer tuning.

## 18. Remaining unknowns

The following details could not be confirmed safely from the public obfuscated distribution:

- Exact ranking algorithm used by Search.
- Exact fuzzy-matching algorithm used by Edit.
- Exact syntax-validation ordering and recovery behavior.
- Recall vector format and compression implementation.
- Credential encryption and storage details.
- Complete event-level telemetry payloads.
- Reviewer model prompts and merge algorithm.
- Knowledge-base storage and remote retrieval implementation.
- Router daemon policy and model-selection algorithm.
- Co-drive terminal control boundaries.

Compresso does not need these internals. It should implement the behaviors from first principles and validate them against Compresso's own safety and quality requirements.

## 19. Definition of done

The WOZ-inspired Compresso add-on is production-ready when:

- Combined Search replaces common discover-search-read chains with bounded output.
- Search supports text, source files, images, and PDFs safely.
- Batch edits are transactional, previewable, validated, and reversible.
- SQL inspection works across PostgreSQL, MySQL, and SQLite.
- Database mutations require explicit approval.
- Recall is local, optional, inspectable, and deletable.
- Review findings are measurable against historical pull requests.
- Knowledge items have provenance and reversible personalization.
- Native coding-assistant tools remain available by default.
- Telemetry is opt-in.
- All integrations are signed, versioned, and reversible.
- Quality metrics are reported alongside token and cost savings.

## 20. Claude Code and Codex parity

All WOZ-inspired Compresso features must be implemented for both Claude Code and Codex. The product core must not depend on either client's private configuration format or lifecycle hooks.

### 20.1 Shared feature surface

Both clients receive the same core capabilities:

```text
Search and bounded Read
Atlas Outline and Unfold
Importer and impact analysis
Transactional batch Edit
SQL schema and query tools
Local Recall and Knowledge
Multi-persona Review
Savings and quality reports
Benchmarks and explanations
Account and entitlement status
```

The tools use one shared, versioned MCP schema. Tool names, request validation, safety behavior, output limits, operation traces, and permission classifications remain consistent across clients.

### 20.2 Adapter architecture

```text
Claude Code                         Codex
    |                                 |
compresso-adapter-claude        compresso-adapter-codex
    |                                 |
    +------------ MCP/IPC ------------+
                    |
             compresso-agent
                    |
       Search/Edit/SQL/Recall/Review
```

Create separate integration modules:

```text
compresso-integration-claude
compresso-integration-codex
```

Each adapter owns only:

- Client detection and compatibility checks.
- MCP registration and removal.
- Client-specific configuration planning.
- Hook or notification translation.
- Approval and permission translation.
- Client-specific instructions or skills.
- Health checks and restart guidance.

The adapters must not contain search, editing, compression, ranking, SQL, recall, or review logic.

### 20.3 Claude Code adapter

The Claude adapter may use:

- Claude plugin manifests when appropriate.
- MCP server registration.
- Skills or commands.
- Supported lifecycle hooks.
- Agent profiles as explicit user-selected modes.
- Status-line integration.

Installation must not automatically replace the user's active agent or disable native Claude tools.

### 20.4 Codex adapter

The Codex adapter may use:

- MCP registration in Codex configuration.
- Codex skills or instruction packages.
- Supported notification or hook mechanisms.
- CLI and desktop restart guidance.
- Compresso session identifiers passed through environment or MCP metadata.

No core feature may require a Claude-only lifecycle event. When Codex lacks an equivalent hook, Compresso must derive the event from MCP activity, local operation records, explicit CLI commands, or supported client notifications.

### 20.5 Capability negotiation

At startup, each adapter reports capabilities:

```rust
struct ClientCapabilities {
    mcp_stdio: bool,
    mcp_http: bool,
    lifecycle_hooks: HashSet<HookKind>,
    approval_requests: bool,
    image_content: bool,
    resource_links: bool,
    skills: bool,
    status_ui: bool,
}
```

Compresso selects an integration path from reported capabilities instead of assuming client behavior.

### 20.6 Parity testing

Every shared tool receives the same contract suite against both adapters:

- Tool registration and schema equality.
- Approved-root path enforcement.
- Output token limits.
- Edit preview, approval, commit, and rollback.
- SQL read-only policy.
- Recall privacy controls.
- Review finding references.
- Offline behavior.
- Disable and uninstall restoration.

A feature is not complete until it works through both Claude Code and Codex or is explicitly documented as a client-specific presentation feature.
