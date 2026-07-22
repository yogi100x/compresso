# 05 — Ticket Backlog

Tickets are dependency-ordered within and across epics. Every ticket ends with a conventional commit. TDD is expected on every ticket: write the failing test first, implement, make it pass, commit. The global constraints in `00-overview.md` (fail-open, protected-node invariants, no telemetry, no Claude Desktop, `--dry-run`/`--json` support, schema versioning at boundaries, latency gates) apply to **every** ticket below and are not repeated in each acceptance list. Interface names are contractual and come from `03-interfaces.md`; changing one requires updating that document first.

Checkbox syntax `[ ]` / `[x]` tracks completion. Owner codes are defined in `00-overview.md` §Staffing.

---

## E1 — Foundation

### [x] E1-01: Monorepo workspace scaffold
Owner: I1 · Estimate: 2d · Depends: —
Files: Cargo.toml, rust-toolchain.toml, package.json, pnpm-workspace.yaml, turbo.json, biome.json, rust/*/Cargo.toml, rust/*/src/lib.rs
Description: Create the workspace exactly per `01-stack-and-workspace.md` §3: Cargo workspace (resolver 3, members `rust/*`, edition 2024, toolchain 1.96 pinned in rust-toolchain.toml) with all ten crates (`compresso-core`, `compresso-atlas`, `compresso-store`, `compresso-mcp`, `compresso-integrations`, `compresso-cli`, `compresso-eval`, plus stub crates `compresso-agent`, `compresso-proxy`, `compresso-updater`), pnpm root workspace with turbo and biome configs, and the directory skeleton (`schemas/`, `fixtures/`, `docs/`, `installers/`, `infra/`, `packages/`, `apps/`). Workspace dependencies declared in `[workspace.dependencies]` per §1 with exact version floors.
Acceptance criteria:
- `cargo build --workspace` and `cargo test --workspace` succeed on a clean checkout with no network access beyond crates.io.
- All ten crates exist; stub crates compile with an empty lib and a doc comment naming their epic (E8/E11/E12).
- Crate dependency rule from `01-stack` §3 holds: `compresso-core` has zero internal dependencies; `compresso-store` does not depend on `compresso-atlas`.
- `pnpm install` succeeds; `biome check .` passes on committed TS/JSON files.
- No crate in core/atlas/mcp/store paths declares a network-I/O dependency (reqwest, hyper, ureq).
Test cases:
- test_workspace_builds: CI step `cargo build --workspace` exits 0.
- test_no_internal_deps_in_core: parse `rust/compresso-core/Cargo.toml`, assert no `compresso-*` entries under `[dependencies]`.
- test_store_never_imports_atlas: parse `rust/compresso-store/Cargo.toml`, assert `compresso-atlas` absent.

### [x] E1-02: CI workflow
Owner: I1 · Estimate: 2d · Depends: E1-01
Files: .github/workflows/ci.yml, .github/workflows/release.yml (stub)
Description: Author `ci.yml` per `01-stack-and-workspace.md` §4: rust job matrix (`macos-14`, `macos-15-intel`, `ubuntu-24.04`, `windows-2022`) running `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `cargo build --release -p compresso-cli`; `Swatinem/rust-cache@v2`; release-binary artifact upload on main. The `eval` job is added in E7-04; the `ts` job is gated off until Phase 6. `release.yml` is a named no-op stub until Phase 8 (E12).
Acceptance criteria:
- CI triggers on push to main and on PRs; all four matrix legs green on the scaffold commit.
- Clippy runs with `-D warnings` and formatting is enforced via `cargo fmt --check`.
- Release binary for each platform uploaded as an artifact on main pushes.
Test cases:
- test_ci_yaml_valid: `actionlint .github/workflows/ci.yml` exits 0 (run locally and as a CI step).
- test_matrix_covers_d33: assert ci.yml matrix contains macos-14, macos-15-intel, ubuntu-24.04, windows-2022 (yq assertion in a repo lint script).

### [x] E1-03: Supply-chain and capability-boundary checks
Owner: I1 · Estimate: 1d · Depends: E1-02
Files: deny.toml, scripts/check-crate-capabilities.ts, .github/workflows/ci.yml
Description: Enforce 01-stack network/process classes per owning crate.
Acceptance criteria:
- Unknown classes fail; network/process dependencies pass only in sanctioned crates; audit/deny/gitleaks gate.
Test cases:
- reqwest core fail/proxy pass; terminal direct spawn fail; secret canary.

### [x] E1-04: Commit versioned JSON Schemas
Owner: R1 · Estimate: 1d · Depends: E1-01
Files: schemas/context-graph/v1.json, schemas/compression-trace/v1.json, rust/compresso-core/tests/schema_meta.rs
Description: Commit the two authoritative slice schemas verbatim from `02-schemas.md` §1 and §2 under `schemas/`, and add a meta-validation test that compiles each with the `jsonschema` crate so a malformed edit to a schema file fails CI. `schemas/local-api/` and `schemas/cloud-api/` get versioned placeholder directories with a README naming their epic (E8/E10).
Acceptance criteria:
- `schemas/context-graph/v1.json` and `schemas/compression-trace/v1.json` byte-match the JSON in `02-schemas.md` (modulo whitespace) and declare `$id` under `https://compresso.dev/schemas/`.
- Both schemas compile as JSON Schema draft 2020-12 via the `jsonschema` crate in a test.
- Sample valid and invalid documents validate/fail as expected.
Test cases:
- test_context_graph_schema_compiles: load v1.json, compile with jsonschema, no error.
- test_minimal_graph_validates: `{"schema_version":1,"nodes":[],"edges":[]}` validates against context-graph v1.
- test_bad_kind_rejected: node with `"kind":"banana"` fails validation with a pointer to `/nodes/0/kind`.
- test_trace_requires_fallback_used: trace document missing `fallback_used` fails against the compression-trace v1 root schema (which `$ref`s `$defs/trace`).

### [x] E1-05: Fixtures skeleton and conventions doc
Owner: PQ · Estimate: 1.5d · Depends: E1-01
Files: fixtures/{code,debugging,logs,conversations,provider-payloads,adversarial}/README.md, fixtures/logs/repeated-npm-install/, fixtures/code/mini-monorepo/, docs/plan/06-fixtures.md (skeleton referencing final format), docs/architecture/conventions.md
Description: Create the six-category fixture directory tree from `06-fixtures.md` §3 (code, debugging, logs, conversations, provider-payloads, adversarial) with at least one seed fixture per category (a 500-line repeated log, a small Rust crate, a failing-test debugging log, a tool-call conversation JSON, an Anthropic count-calibration payload stub, a prompt-injection adversarial repo), each carrying a `fixture.toml` in the authoritative `06-fixtures.md` §2 format: exactly one input form (`graph` | `[[input.files]]`/`[[input.nodes]]` | `repo`), an `[expectations]` table, optional `[[recall_probes]]`, and the top-level `eligible` flag. Write `conventions.md` covering error conventions (thiserror per crate, anyhow in binaries only per D20), commit format, module layout, and test naming (`test_snake_case`).
Acceptance criteria:
- All six fixture categories exist with at least one committed fixture and a `fixture.toml` declaring an input form, an `[expectations]` table, and the top-level `eligible` flag per `06-fixtures.md` §2.
- `fixtures/logs/repeated-npm-install/input.txt` contains exactly 500 log lines identical after timestamp normalization (used verbatim by E2-05 and E7).
- conventions.md documents error, naming, commit, and schema-versioning conventions and is linked from the repo README.
Test cases:
- test_fixture_tomls_parse: walk fixtures/, parse every fixture.toml with the `toml` crate, assert `name`, `category`, `description`, `eligible`, `mode`, `[budget]`, one input form, and `[expectations]` present.
- test_repeated_log_fixture_shape: read repeated-npm-install/input.txt, assert 500 lines and that stripping the leading ISO-8601 timestamp yields a single distinct line.

### [x] E1-06: Slice release tarball CI job (D42)
Owner: I1 · Estimate: 1.5d · Depends: E1-02
Files: .github/workflows/ci.yml (package job), scripts/package-tarball.sh
Description: Add a `package` CI job on all four D33 matrix legs producing a per-platform tarball `compresso-<version>-<target>.tar.gz` (targets: aarch64-apple-darwin, x86_64-apple-darwin, x86_64-unknown-linux-gnu, x86_64-pc-windows-msvc) containing exactly the release `compresso` binary (`compresso.exe` on Windows), plus a single aggregated `SHA256SUMS` file in `shasum -a 256 -c`-compatible format covering every tarball. Artifacts uploaded on main pushes; consumed by `installers/install-local.sh` (E4-05). No signing — D42 keeps the slice installer checksum-only; signing/notarization stay E12.
Acceptance criteria:
- A main push produces four tarball artifacts plus one `SHA256SUMS`; `shasum -a 256 -c SHA256SUMS` passes against the downloaded artifacts.
- Each tarball contains exactly one entry (the binary); no fixtures, docs, debug symbols, or source maps (tar listing asserted in the packaging script's self-test).
- `scripts/package-tarball.sh` is deterministic for a given binary (stable tar metadata: sorted entries, fixed mtime, numeric owners).
Test cases:
- test_package_script_single_entry: run package-tarball.sh against a stub binary → `tar -tzf` lists exactly `compresso`.
- test_sha256sums_verifies: generated SHA256SUMS verifies with `shasum -a 256 -c`; flipping one byte in a tarball makes verification fail.
- test_package_deterministic: two runs on the same stub binary produce byte-identical tarballs.

---

## E2 — Core engine

### [x] E2-01: ContextGraph types, validation, serde round-trip
Owner: R1 · Estimate: 2d · Depends: E1-01, E1-04
Files: rust/compresso-core/src/graph.rs, tests in same file + rust/compresso-core/tests/graph_schema_roundtrip.rs
Description: Implement `ContextGraph`, `ContextNode`, `NodeKind`, `Source`, `ContextEdge`, `EdgeKind`, `GraphError` exactly per `03-interfaces.md` §2.1, with serde renames matching `schemas/context-graph/v1.json` (snake_case kinds, tagged `source`). Implement `ContextGraph::validate()` enforcing the code-level invariants from `02-schemas.md` §1 (unique ids, edge endpoints exist, `protected: true` required for `SystemInstruction`/`UserInstruction`/`AssistantReasoning`/`ToolDefinition`/`ToolRequest`; arbitrary metadata can never downgrade protection), plus `force_ingress_protection(&mut self) -> Vec<String>` implementing the D48 ingress rule as extended by D53: on any externally supplied graph (MCP, proxy, fixtures loaded as untrusted input) `protected` is forced to `true` for those five kinds and a `protection_forced:<node_id>` note is returned for inclusion in the trace — unprotecting is possible only from trusted local surfaces (CLI `--allow-unprotect`, local config), never from an inbound payload. Also `total_tokens()` and `from_text(content, kind, counter)`.
Acceptance criteria:
- A graph containing one node of every `NodeKind` and one edge of every `EdgeKind` serializes to JSON that validates against `schemas/context-graph/v1.json` and deserializes back to an equal value.
- `validate()` returns typed `GraphError` variants for duplicate ids, dangling edges, and protection violations; never panics on adversarial input.
- `token_provider` defaults to `openai_o200k` on deserialization when absent.
- `force_ingress_protection` flips `protected` to true for the five instruction/reasoning/tool kinds, returns one `protection_forced:<id>` note per flipped node, is a no-op (empty notes) on already-compliant graphs, and cannot be suppressed by any metadata key (D48, D53).
Test cases:
- test_roundtrip_all_kinds_validates_against_schema: build the every-kind graph, serialize, validate with jsonschema against v1.json, deserialize, assert equality.
- test_validate_rejects_duplicate_ids: two nodes with id "n1" → `Err(GraphError::DuplicateId("n1"))`.
- test_validate_rejects_dangling_edge: edge from "n1" to "missing" → `Err(GraphError::UnknownEdgeEndpoint { .. })`.
- test_system_instruction_must_be_protected: node kind `SystemInstruction` with `protected: false` and no trusted-local unprotect → protection-invariant error.
- test_ingress_forces_protection: external graph with node `sys-1` (`system_instruction`, `protected: false`) → `force_ingress_protection` sets `protected: true`, returns `["protection_forced:sys-1"]`, and `validate()` then passes; adding metadata `"allow_unprotect": true` to the node does not suppress the forcing (D48).
- test_assistant_reasoning_must_be_protected: `assistant_reasoning` with `protected: false` fails validation; ingress forces it protected and records the note (D53).
- test_from_text_counts_tokens: `ContextGraph::from_text("fn main() {}", NodeKind::SourceFile, counter)` yields one node with `token_count == counter.count("fn main() {}")` and `total_tokens()` equal to it.

### [x] E2-02: TokenCounter providers
Owner: R1 · Estimate: 1.5d · Depends: E1-01, E1-05
Files: rust/compresso-core/src/tokens.rs, tests in same file
Description: Implement `TokenProvider`, the `TokenCounter` trait, and `counter_for(provider) -> &'static dyn TokenCounter` per `03-interfaces.md` §2.2: tiktoken-rs `o200k_base` and `cl100k_base` counters, `AnthropicCalibrated` (o200k count × `CALIBRATION`; until E2-02a commits measured counts into `fixtures/provider-payloads/anthropic-counts/anthropic-counts.json`, `CALIBRATION` is the documented interim default `1.15` per ADR-0017 and `03-interfaces.md` §2.2), and a `Heuristic` fallback (bytes/4) for use when tokenizer init fails. Singletons are eagerly initialized during process startup once.
Acceptance criteria:
- `counter_for(TokenProvider::OpenAiO200k).count(text)` matches tiktoken-rs `o200k_base` exactly; same for cl100k.
- `AnthropicCalibrated` returns `ceil(o200k_count × CALIBRATION)` and its `provider()` tag is `AnthropicCalibrated`; CALIBRATION is a named const with a doc comment citing the fixture file and ADR-0017 — interim value `1.15` while the fixture's `anthropic_tokens`/`calibration_factor` are null, replaced by the measured median once E2-02a lands.
- `counter_for` returns the same static reference on repeated calls (no re-init); counting a representative 1 MiB multiline string completes in < 200 ms after prewarming in release mode, aligned with the canonical warm product gate owned by E2-15.
Test cases:
- test_o200k_hello_world: `count("hello world") == 2` for OpenAiO200k.
- test_empty_string_zero: all four providers return 0 for "".
- test_calibrated_is_scaled_o200k: for the text of `fixtures/provider-payloads/anthropic-counts/anthropic-counts.json` samples, calibrated count equals `ceil(o200k × CALIBRATION)`.
- test_calibration_accuracy_gated_on_fixture: while the fixture's `anthropic_tokens` values are null, the ±10% accuracy assertion is skipped with a `calibration pending (E2-02a)` warning; once they are non-null, calibrated counts must be within 10% of the recorded Anthropic count for every sample.
- test_counter_for_singleton: `std::ptr::eq(counter_for(OpenAiO200k), counter_for(OpenAiO200k))` is true.
- test_provider_tag_roundtrip: each counter's `provider()` serializes to the schema enum string (`"openai_o200k"` etc.).
- test_warm_o200k_one_mib_under_two_hundred_ms_in_release: after prewarming, count one representative 1 MiB multiline input in release mode and assert elapsed time is < 200 ms; E2-15 owns the canonical warm P95 product gate.

### [ ] E2-02a: Anthropic calibration measurement
Owner: PQ+R1 · Estimate: 1d · Depends: E2-02, E1-05
Files: fixtures/provider-payloads/anthropic-counts/anthropic-counts.json, rust/compresso-eval/src/calibrate.rs, docs/adr/0017-token-counting-o200k-anthropic-calibration.md
Description: Implement `compresso eval calibrate-anthropic --model <model>` running the documented procedure from `06-fixtures.md` §7.6: for each sample, call Anthropic `POST /v1/messages/count_tokens` with the sample text as a single user message, subtract the once-measured empty-message envelope overhead, fill `anthropic_tokens`/`anthropic_model`/`counted_at`, compute `ratio_i = anthropic_tokens_i / o200k_tokens_i`, and set `calibration_factor` to the median ratio rounded to three decimals. Run offline by a maintainer with an API key — never in CI. Commit the measured counts, update the `CALIBRATION` const from the fixture (provenance comment naming fixture, model, date), and thereby activate E2-02's ±10% accuracy assertion. Counts produced with the factor stay tagged `anthropic_calibrated` and never feed `provider_observed_cost` (02-schemas §3).
Acceptance criteria:
- Committed `anthropic-counts.json` has non-null `anthropic_tokens` for all ten samples plus `anthropic_model`, `counted_at`, and a three-decimal `calibration_factor` equal to the median ratio.
- `CALIBRATION` equals the fixture's `calibration_factor` (test reads the fixture and compares), replacing the 1.15 interim; ADR-0017 updated with model and date.
- E2-02's `test_calibration_accuracy_gated_on_fixture` now executes its ±10% assertion (no longer skipped) and passes.
Test cases:
- test_calibration_factor_is_median_ratio: recompute the median of `anthropic_tokens_i / o200k_tokens_i` from the committed fixture → equals `calibration_factor` to three decimals.
- test_calibration_const_matches_fixture: `CALIBRATION == fixture.calibration_factor`.
- test_calibrate_requires_api_key: `compresso eval calibrate-anthropic` without `ANTHROPIC_API_KEY` exits 1 with `error.code == "missing_api_key"` and writes nothing.
- test_calibrate_never_in_ci: repo lint asserts no CI workflow invokes `calibrate-anthropic`.

### [x] E2-03: CompressionStrategy trait and StrategyRegistry
Owner: R1 · Estimate: 1d · Depends: E2-01, E2-02
Files: rust/compresso-core/src/strategy.rs, tests in same file
Description: Implement `Estimate`, `StrategyOutput`, `CompressionStrategy` (with `id`, `supports`, `estimate`, `execute`), `StrategyError`, and `StrategyRegistry` per `03-interfaces.md` §2.3. `StrategyRegistry::safe_default(Mode::Safe)` returns the ordered list `[dedup_exact, collapse_logs, compact_json, pack_budget]`; `Mode::Passthrough` returns an empty registry. Add `StrategyRegistry::permitted(&self, lossiness: Lossiness) -> Vec<&dyn CompressionStrategy>` enforcing the D39 lossiness taxonomy (03-interfaces §2.3 table): `Lossiness::None` → `dedup_exact` only; `Structural` (safe-mode default) → all four; `Summarizing` reserved for post-slice aggressive mode. Ships with a `NoopStrategy` test double so the registry is testable before E2-04..07 land.
Acceptance criteria:
- `safe_default(Mode::Safe).strategies()` yields exactly four entries whose `id()`s are, in order: `"dedup_exact"`, `"collapse_logs"`, `"compact_json"`, `"pack_budget"` (registered as stubs until their tickets land, each stub returning `supports() == false`).
- `safe_default(Mode::Passthrough).strategies()` is empty.
- Trait objects are `Send + Sync` (compile-time assertion).
Test cases:
- test_safe_registry_order: collect `id()`s from `safe_default(Mode::Safe)`, assert `["dedup_exact","collapse_logs","compact_json","pack_budget"]`.
- test_passthrough_registry_empty: `safe_default(Mode::Passthrough).strategies().len() == 0`.
- test_strategy_object_safety: `fn assert_send_sync<T: Send + Sync>() {}` instantiated with `Box<dyn CompressionStrategy>`.
- test_lossiness_none_permits_dedup_only: `permitted(Lossiness::None)` ids == `["dedup_exact"]`; `permitted(Lossiness::Structural)` ids == all four in registry order (D39).

### [x] E2-04: Exact-duplicate removal strategy
Owner: R1 · Estimate: 1d · Depends: E2-03
Files: rust/compresso-core/src/strategies/dedup_exact.rs, tests in same file
Description: Implement `dedup_exact` per `03-interfaces.md` §2.3. Each replacement emits `RemovalRecord::DuplicateReference` with node/survivor locator, original/replacement hashes, and exact before/after counts; notes are explanatory only (D51).
Acceptance criteria:
- First occurrence of duplicated content is byte-unchanged; replacements reference the kept node's id; edges untouched.
- Protected nodes are never candidates, even when byte-identical to unprotected ones.
- Output graph passes `validate()`; `token_count` on stub nodes is recomputed with the graph's counter.
Test cases:
- test_dedup_three_identical_tool_responses: three unprotected `ToolResponse` nodes with content `"SELECT * FROM users LIMIT 50;\n...200 identical rows..."`; after execute, node 1 byte-equal, nodes 2–3 contain `"[duplicate of node"` and node 1's id, `total_tokens()` strictly decreases; notes contain `removed node:` entries for nodes 2–3 naming node 1 as survivor (D39).
- test_protected_duplicate_untouched: a protected `SystemInstruction` and an unprotected `ToolResponse` with identical content → both byte-unchanged, `supports()` false when they are the only pair.
- test_no_duplicates_not_supported: graph of three distinct contents → `supports() == false`; `execute` (if forced) returns the input unchanged with an explanatory note.
- test_estimate_matches_execute: `estimate().tokens_saved` equals `before_tokens - after_tokens` measured after execute on the three-duplicate graph, within the stub-token overhead.

### [x] E2-05: Log-collapse strategy
Owner: R1 · Estimate: 1.5d · Depends: E2-03, E1-05
Files: rust/compresso-core/src/strategies/collapse_logs.rs, tests in same file
Description: Implement `collapse_logs` per `03-interfaces.md` §2.3. Each non-overlapping collapsed span emits `RemovalRecord::CollapsedLines` with locator, hashes, exact counts, and removed-line count (D51).
Acceptance criteria:
- Runs shorter than 3 lines are never collapsed; collapsed output keeps the first raw line verbatim plus the `[×N repeats]` suffix.
- Normalization treats ISO-8601 timestamps, `0x`-prefixed and bare hex ≥8 chars, and RFC-4122 UUIDs as equal; everything else must match exactly.
- Protected nodes and `SourceFile`/`CodeSymbol` nodes are untouched.
Test cases:
- test_collapse_500_timestamped_lines: load `fixtures/logs/repeated-npm-install/input.txt` (500 identical lines with varying timestamps) into one `ToolResponse` node; output content has ≤5 lines and contains `"[×500"`; notes contain a `collapsed` entry accounting for the removed lines.
- test_two_repeats_untouched: node with 2 identical lines → byte-unchanged, strategy note records the skip.
- test_uuid_and_hex_normalization: 10 lines `"request 3f2a9c1d4b5e6f70 done (id=550e8400-e29b-41d4-a716-446655440000)"` with differing hex/uuid values → collapsed to 1 line containing `"[×10"`.
- test_protected_log_untouched: same 500-line content in a node with `protected: true` → byte-identical output.
- test_interleaved_runs: 5×"A", 1×"B", 4×"A" → output `A [×5]`, `B`, `A [×4]` (three lines; runs never merge across interruptions).

### [x] E2-06: JSON-compaction strategy
Owner: R1 · Estimate: 1.5d · Depends: E2-03
Files: rust/compresso-core/src/strategies/compact_json.rs, tests in same file
Description: Implement `compact_json` per `03-interfaces.md` §2.3. Each non-overlapping elision emits `RemovalRecord::JsonElision` with JSON Pointer, hashes, exact counts, and removed-unit count (D51).
Acceptance criteria:
- Output content of processed nodes always re-parses as valid JSON; keys are never dropped.
- String values > 500 chars truncated with `…[+N chars]`; arrays > 50 elements keep first 25 and last 5 with an elision sentinel counting the omitted items.
- Invalid JSON, non-`ToolResponse` kinds, and protected nodes pass through byte-identical.
Test cases:
- test_minify_whitespace: node content `"{\n  \"a\": 1,\n  \"b\": [1, 2, 3]\n}"` → `{"a":1,"b":[1,2,3]}`.
- test_truncate_2000_element_array: array of 2000 integers → output parses as JSON, has ≤31 array elements, one of which is a string containing `"[+1970 items]"`.
- test_long_string_elided: 10 000-char string value → output value length ≤ 520 and ends with `"…[+9500 chars]"` (exact remainder count asserted).
- test_invalid_json_untouched: content `"{not json"` → byte-identical, `skipped_reason` note recorded.
- test_structure_preserved: nested object with 4 levels → output has identical key set at every level (recursive key comparison).
- test_truncations_enumerated_in_notes: every elision marker in the output has a matching `truncated <pointer>` note; a node with no elisions contributes no `truncated` notes.

### [x] E2-07: Budget-packing strategy
Owner: R1 · Estimate: 1.5d · Depends: E2-03
Files: rust/compresso-core/src/strategies/pack_budget.rs, tests in same file
Description: Implement `pack_budget` per `03-interfaces.md` §2.3. Every dropped node emits `RemovalRecord::DroppedNode` with original hash/count and zero replacement count; edge pruning cannot create overlapping records (D51).
Acceptance criteria:
- Output `total_tokens() ≤ budget.max_tokens` whenever the protected set alone fits; otherwise the strategy stops at protected-only and records that the budget is unsatisfiable in notes (engine surfaces this; no protected node is ever dropped).
- Drop order is deterministic: ties on priority/token broken by node id lexicographic order.
- No node content is ever modified — nodes are only removed.
Test cases:
- test_drops_lowest_value_first: nodes A(priority 0.9, 100 tok), B(0.2, 100 tok), C(0.5, 100 tok), budget 250 → B dropped, A and C kept, note lists `"removed node:B"`.
- test_protected_never_dropped: protected node of 1000 tokens with budget 100 → protected node kept, all unprotected dropped, note records unsatisfiable budget, no error.
- test_code_never_truncated: `SourceFile` node of 400 tokens with budget 300 and no other nodes → node dropped whole (content never sliced) or budget unsatisfied note; assert no output node has content that is a strict prefix of the input content.
- test_edges_pruned: edge A→B where B is dropped → edge absent from output and `output.validate()` passes.
- test_deterministic_tiebreak: two identical-score nodes "n_a" and "n_b" → "n_a" dropped first on two consecutive runs (assert equality of both outputs).

### [x] E2-08: Safety module with eleven mandatory checks
Owner: R1 · Estimate: 2d · Depends: E2-01, E2-02
Files: rust/compresso-core/src/safety.rs
Description: Implement the eleven checks in 03 §2.5 in order.
Acceptance criteria:
- All eleven always run; provider atoms, non-empty output, dedup refs, token recounts, and removals gate output.
Test cases:
- one failure per check; all-pass; every failure restores bytes.

### [x] E2-09: Engine::compress pipeline, trace generation, fallback
Owner: R1 · Estimate: 2d · Depends: E2-04, E2-05, E2-06, E2-07, E2-08, E1-04
Files: rust/compresso-core/src/engine.rs, rust/compresso-core/src/trace.rs, tests in both + rust/compresso-core/tests/trace_schema.rs
Description: Implement `Mode`, `CompressionBudget`, `Lossiness`, `CompressionResult`, `Engine::new(mode, counter)`, and `Engine::compress(input, budget) -> CompressionResult` per `03-interfaces.md` §2.4, plus `Trace` serialization matching `schemas/compression-trace/v1.json`. Pipeline per build-doc §10.3: validate → mark protected → run registry strategies in order within the remaining latency budget → `safety::run_all` on the cumulative result → on any strategy error, failed check, or panic (caught via `catch_unwind`), restore the original graph and set `fallback_used`/`fallback_reason`. `compress` **never returns Err**. Traces carry ULID `operation_id`, per-strategy `StrategyResult`s, all safety checks, and a human-readable explanation with no raw content.
Acceptance criteria:
- `compress` signature returns `CompressionResult` (not `Result`); every failure path yields `output == input`, `fallback_used == true`, non-null `fallback_reason`.
- `Mode::Passthrough` returns the input unchanged with `strategy_results` empty and `mode: "passthrough"` in the trace.
- Serialized trace validates against the `schemas/compression-trace/v1.json` **root** schema (which `$ref`s the trace definition); `operation_id` is a valid ULID; trace JSON contains no node content substring.
- Identical input + budget produce identical output graphs across runs (determinism; trace ids differ).
Test cases:
- test_never_err_on_panicking_strategy: engine with an injected strategy whose `execute` panics → returns normally, `fallback_used == true`, `fallback_reason` contains the strategy id, output byte-equal to input.
- test_failed_safety_restores_original: injected strategy that mutates a protected node → `protected_unchanged` fails, output equals input, `fallback_used == true`.
- test_removals_enumerated_failure_falls_back: injected strategy silently drops an unprotected node without emitting a note → `removals_enumerated` fails → output byte-equal to input, `fallback_used == true`, `fallback_reason` names the check.
- test_passthrough_identity: `Engine::new(Mode::Passthrough, counter).compress(g, &budget)` → `after_tokens == before_tokens`, zero strategy_results.
- test_trace_validates_against_schema: compress the E1-05 log fixture graph, serialize `result.trace`, validate against compression-trace v1 → ok.
- test_trace_leaks_no_content: assert the trace JSON string does not contain any 20-char substring of any input node's content.
- test_safe_mode_on_log_fixture: 500-line log fixture in Safe mode → `after_tokens < before_tokens × 0.5`, `fallback_used == false`, `strategy_results` includes `collapse_logs` with `applied == true`.
- test_deterministic_output: two compress calls on the same input → outputs serialize to identical JSON (excluding operation_id/created_at).

### [x] E2-10: Property tests for engine invariants
Owner: R1 · Estimate: 1d · Depends: E2-09
Files: rust/compresso-core/tests/proptest_engine.rs
Description: proptest generators for arbitrary `ContextGraph`s (random kinds, protection flags, contents including unicode, empty strings, lone surrogate-free control chars, JSON and non-JSON) and budgets; assert the two core invariants across thousands of cases: compress never mutates protected nodes, and in Safe mode output tokens ≤ input tokens.
Acceptance criteria:
- Generator produces graphs that always pass `validate()` (protection invariants respected by construction) with 0–50 nodes and 0–100 edges.
- Property `protected_nodes_unchanged`: for every generated graph/budget, every protected node's content in the output is byte-equal to the input.
- Property `safe_mode_never_inflates`: `result.after_tokens <= result.before_tokens` for `Mode::Safe`.
- 1000+ cases per property in CI within the test-time budget; failures shrink to a minimal counterexample.
Test cases:
- prop_compress_never_mutates_protected: proptest over arbitrary graphs × budgets(max_tokens 1..100_000) → all protected contents byte-equal after compress.
- prop_safe_output_never_exceeds_input_tokens: proptest over arbitrary graphs → `after_tokens <= before_tokens`.
- prop_output_always_validates: `result.output.validate().is_ok()` for every case.
- prop_fallback_implies_identity: whenever `fallback_used`, output serializes byte-equal to input.

### [x] E2-11: Criterion latency benchmarks with P95 gate
Owner: R1 · Estimate: 1.5d · Depends: E2-09, E1-05
Files: rust/compresso-core/benches/compress_latency.rs, docs/plan/gates.toml (latency section stub consumed fully in E7-03)
Description: Criterion benchmarks running `Engine::compress` in Safe mode over the committed fixtures (500-line log, 2000-element JSON tool response, 40-node mixed conversation graph, 200 KB source-file graph), reporting P50/P95. Adds a plain `#[test]` (release-profile CI leg) asserting P95 < 200 ms per D37 so regressions fail CI even where criterion output isn't parsed.
Acceptance criteria:
- `cargo bench -p compresso-core` produces named benchmarks `compress_log_500`, `compress_json_2000`, `compress_conversation_40`, `compress_source_200kb`.
- P95 across 100 iterations of each fixture < 200 ms on CI hardware (macos-14 and ubuntu-24.04 legs).
- Benchmark inputs are the committed fixtures, not synthetic in-bench strings, so eval and bench measure the same corpus.
Test cases:
- test_p95_under_200ms_all_fixtures: run each fixture 100×, sort durations, assert `durations[94] < Duration::from_millis(200)` (release mode, `#[cfg_attr(debug_assertions, ignore)]`).
- test_passthrough_under_5ms: Passthrough mode on the 200 KB fixture P95 < 5 ms (guards against accidental work in the bypass path).

---

## E3 — Atlas

### [x] E3-01: Store::open and migrations
Owner: R2 · Estimate: 1.5d · Depends: E1-01
Files: rust/compresso-store/src/lib.rs, rust/compresso-store/migrations/0001_init.sql, tests in rust/compresso-store/tests/migrations.rs
Description: Implement `Store::open(path)` and `Store::open_default()` per `03-interfaces.md` §3: open rusqlite (bundled) with WAL and foreign keys on, run pending migrations transactionally from embedded SQL files, record them in `schema_migrations`. Commit `0001_init.sql` byte-for-byte per `02-schemas.md` §4 (all twelve tables, FTS5 `symbols_fts` with triggers, indexes). `open_default` resolves the platform data dir per `01-stack` §5 via `dirs`. Startup prunes `operations` older than 90 days (configurable). Implement the D41 concurrency contract (03-interfaces §3): `Store` wraps a single `Arc<Mutex<rusqlite::Connection>>`, is `Send + Sync` and cheaply cloneable, exposes only blocking methods, and sets `busy_timeout = 5s` at open (guarding against a second process, e.g. the CLI while the MCP server is running); async callers wrap every call in `tokio::task::spawn_blocking`. Escape hatch if profiling ever shows lock contention: a dedicated writer thread behind the same API.
Acceptance criteria:
- `Store::open` on a fresh temp path creates the DB, applies migration 1, and inserts `(1, applied_at)` into `schema_migrations`; reopening applies nothing and changes nothing.
- `PRAGMA journal_mode` returns `wal`; `PRAGMA foreign_keys` returns 1 on every opened connection.
- A migration that fails mid-way (injected bad SQL in a test-only migration) leaves the DB at the prior version with no partial objects.
- `Store` is `Send + Sync + Clone` (compile-time assertion); `PRAGMA busy_timeout` returns 5000 on every opened connection; two threads writing concurrently through clones of one `Store` both succeed, serialized by the mutex.
Test cases:
- test_open_fresh_creates_all_tables: open temp DB, query `sqlite_master`, assert all table names from 02-schemas §4 present including `symbols_fts`.
- test_open_idempotent: open twice; `schema_migrations` has exactly one row with version 1.
- test_failed_migration_rolls_back: register test migration 2 with invalid SQL → open returns `StoreError`, `schema_migrations` max version still 1, no `test_table` exists.
- test_wal_and_fk_enabled: pragmas assert as above.
- test_retention_prunes_old_operations: insert operation with `created_at` 91 days ago and one 89 days ago, reopen → only the recent row remains.
- test_concurrent_writes_serialized: two threads each record 50 operations through clones of the same `Store` → both complete without `SQLITE_BUSY`, final `operations` count == 100.
- test_busy_timeout_5s: `PRAGMA busy_timeout` == 5000 after open.
- test_store_send_sync_clone: `fn assert_bounds<T: Send + Sync + Clone>() {}` instantiated with `Store`.

### [x] E3-02: Repository, file, and symbol DAOs
Owner: R2 · Estimate: 1.5d · Depends: E3-01
Files: rust/compresso-store/src/repos.rs, rust/compresso-store/src/files.rs, rust/compresso-store/src/symbols.rs, tests in same files
Description: Implement the repository/file/symbol surface of `Store` per `03-interfaces.md` §3: `approve_repo`, `repo_by_root`, `upsert_file`, `file_hashes`, `replace_symbols` (symbols + edges in a single transaction), `delete_files`, `search_symbols` (literal FTS5 trigram queries, bm25-ranked with deterministic ties), `symbols_in_file`, and D59-compliant `symbols_by_identity`, with `RepoRecord`/`FileRecord`/`SymbolRecord`/`EdgeRecord`/`SymbolHit` types. `SymbolSelector` is owned by `compresso-store` so Store never depends on Atlas; lookup returns all deterministic candidates and never an arbitrary first match. Only this crate contains SQL.
Acceptance criteria:
- `approve_repo` canonicalizes the root and is idempotent (same `RepoId` on repeat); `repo_by_root` finds it by canonical path.
- `upsert_file` twice for the same `(repo, path)` yields one row with updated hash/size; `delete_files` cascades symbol and edge rows (FK verified).
- `replace_symbols` is atomic: an injected constraint violation mid-batch leaves the file's previous symbols intact.
- `search_symbols` matches trigram substrings and orders by bm25.
Test cases:
- test_approve_repo_idempotent: approve `/tmp/x/../x` and `/tmp/x` → same RepoId, one `repositories` row.
- test_upsert_file_updates_in_place: insert path `src/lib.rs` hash "aa", upsert hash "bb" → one row, hash "bb", `file_hashes` returns `{"src/lib.rs": "bb"}`.
- test_replace_symbols_transactional: seed 3 symbols; call `replace_symbols` with a batch containing an invalid row (end_line < start_line triggers CHECK/validation) → Err, `symbols_in_file` still returns the 3 originals.
- test_delete_files_cascades: file with 2 symbols and 1 edge; `delete_files` → `symbols` and `edges` rows for it gone, `symbols_fts` no longer matches.
- test_search_symbols_trigram: symbol `SessionManager::start`; `search_symbols(repo, "essionMan", 10)` returns it ranked first.
- test_symbols_by_identity_qualified_and_ambiguous: a selector with path + qualified name and optional kind/signature hash/start-line hint returns all matching `(FileRecord, SymbolRecord)` candidates in deterministic order; optional fields are honored and no arbitrary first match is selected.

### [x] E3-03: Session, operation, receipt, and stats DAOs
Owner: R2 · Estimate: 1.5d · Depends: E3-01, E1-04
Files: rust/compresso-store/src/ops.rs, rust/compresso-store/src/receipts.rs, tests in same files
Description: Implement `start_session`, `record_operation` implementing the D47 split (02-schemas §4): compression operations (`compresso_compress`, engine runs) must carry `trace_json` validated against the compression-trace v1 **root** schema before insert; retrieval operations (map, search, read, outline, unfold, context, impact, stats, explain) must store `trace_json = NULL` and set `tokens_served`. Implement `stats(since)` returning `StatsSummary` in the savings vocabulary of `02-schemas.md` §3 (`observed_tokens`, `applied_tokens`, `removed_tokens`, `compression_overhead`, `net_tokens_saved`, labelled `modeled_cost_saved`), plus `save_receipt`/`receipts` for `integration_receipts`.
Acceptance criteria:
- `record_operation` rejects (typed error, no row written) any `trace_json` failing root-schema validation; accepted compression rows also populate `strategy_results` child rows.
- Retrieval operations are accepted with `trace_json = NULL` and `tokens_served` set; a compression operation with NULL trace, or a retrieval operation carrying a trace, is rejected with a typed error (D47).
- `stats(None)` aggregates across all operations; `stats(Some(ts))` filters by `created_at >= ts`; `net_tokens_saved == removed_tokens - compression_overhead` in the returned summary.
- Receipts round-trip all fields including `keys_owned` JSON array; `receipts(Some("codex"))` filters by client.
Test cases:
- test_record_operation_rejects_bad_trace: trace_json `{"schema_version":1}` (missing required fields) → `Err(StoreError::InvalidTrace)`, `operations` count 0.
- test_retrieval_op_null_trace_tokens_served: record `tool = "compresso_read"`, `trace_json = None`, `tokens_served = Some(1874)` → accepted; row has NULL `trace_json` and `tokens_served == 1874`.
- test_compress_op_requires_trace: `tool = "compresso_compress"` with `trace_json = None` → `Err(StoreError::MissingTrace)`, no row written.
- test_record_operation_persists_strategy_results: valid trace with 2 strategy_results → 1 `operations` row + 2 `strategy_results` rows keyed by operation_id.
- test_stats_vocabulary_math: two ops (observed 1000/applied 700/overhead 50, observed 500/applied 500/overhead 0, second with fallback_used=1) → `observed_tokens == 1500`, `applied_tokens == 1200`, `removed_tokens == 300`, `net_tokens_saved == 250`.
- test_stats_since_filters: op at t=100 and t=200; `stats(Some(150))` counts only the second.
- test_receipt_roundtrip_and_filter: save claude-code and codex receipts → `receipts(Some("codex"))` returns exactly the codex one with `keys_owned == ["mcp_servers.compresso"]`.

### [x] E3-04: Repository scanner
Owner: R2 · Estimate: 2d · Depends: E3-02
Files: rust/compresso-atlas/src/scanner.rs, tests in same file + fixtures under rust/compresso-atlas/tests/fixtures/scan-repo/
Description: Implement `ScanResult` and `scan(store, repo, root)` per `03-interfaces.md` §4: walk with the `ignore` crate (gitignore-aware), apply built-in exclusions (binary sniff, files > `max_file_bytes`, `node_modules/`, `target/`, `dist/`, `.git/`, secret patterns `.env*`, `*.pem`, `id_rsa*`), hash candidates with blake3, and diff against `Store::file_hashes` to produce `added`/`changed`/`removed`/`skipped` with `SkipReason`.
Acceptance criteria:
- `.gitignore` entries are honored (nested gitignores included); excluded dirs are never descended into (verified via walk instrumentation, not just filtered output).
- Secret-pattern files appear in `skipped` with `SkipReason::SecretPattern` and are never hashed or opened beyond the initial stat.
- `removed` lists store paths absent on disk; hashes are blake3 hex matching `b3sum` output.
Test cases:
- test_gitignore_respected: fixture repo with `ignored.log` listed in .gitignore → not in added, present in skipped with `SkipReason::Ignored`.
- test_secret_patterns_skipped: fixture containing `.env.local`, `server.pem`, `id_rsa` → all three in skipped with `SkipReason::SecretPattern`.
- test_binary_and_oversize_skipped: a 3 MB file with max_file_bytes=1 MB and a PNG → skipped with `SkipReason::TooLarge` / `SkipReason::Binary`.
- test_changed_detected_by_hash: index fixture, modify one byte of `src/main.rs`, rescan → exactly `["src/main.rs"]` in changed, nothing in added.
- test_removed_detected: delete `src/util.rs` after indexing, rescan → `removed == ["src/util.rs"]`.
- test_blake3_matches_reference: hash of committed file equals precomputed blake3 hex constant in the test.

### [x] E3-05: ParserRegistry scaffold + Rust extraction
Owner: R2 · Estimate: 1.5d · Depends: E3-02
Files: rust/compresso-atlas/src/parsers.rs, rust/compresso-atlas/queries/rust.scm, tests + fixture rust/compresso-atlas/tests/fixtures/lang/sample.rs
Description: Implement `ParserRegistry::for_path` (extension map: rs, ts, tsx, js, jsx, mjs, cjs, py per `03-interfaces.md` §4) and `ParserRegistry::extract(lang, source) -> Extraction` for Rust via tree-sitter-rust and `queries/rust.scm`: functions, methods (with `Type::method` qualified names), structs, enums, traits, consts, type aliases, modules; imports from `use` declarations as written. Symbol kinds normalize to the store vocabulary from 02-schemas §4.
Acceptance criteria:
- `for_path("foo.rs") == Some(Language::Rust)`; unknown extensions return None.
- Extraction on the committed fixture yields exact names, kinds, 1-based inclusive line ranges, and signatures; malformed source returns partial symbols, never panics.
- Methods inside `impl SessionManager` get `qualified_name` `SessionManager::start`.
Test cases:
- test_rust_extracts_fn_struct_impl: fixture `sample.rs` containing `pub struct SessionManager` (lines 3–7), `impl SessionManager { pub fn start(&self) -> Result<(), Error> { .. } }` (fn at lines 10–14), and `pub fn helper()` (line 17) → symbols `[SessionManager (struct, 3–7), SessionManager::start (method, 10–14), helper (function, 17–19)]` with signature `pub fn start(&self) -> Result<(), Error>`.
- test_rust_imports_as_written: `use crate::graph::ContextGraph;` and `use serde::{Serialize, Deserialize};` → imports `["crate::graph::ContextGraph", "serde::Serialize", "serde::Deserialize"]`.
- test_rust_malformed_no_panic: source `"fn broken( {"` → `Ok(Extraction)` or typed `ParseError`, never a panic (wrapped in catch_unwind assertion).
- test_for_path_extension_map: table-driven over all eight extensions plus `foo.go → None`.

### [x] E3-06: TypeScript + TSX extraction
Owner: R2 · Estimate: 1.5d · Depends: E3-05
Files: rust/compresso-atlas/queries/typescript.scm, rust/compresso-atlas/queries/tsx.scm, tests + fixtures tests/fixtures/lang/sample.ts, sample.tsx
Description: tree-sitter-typescript (both grammars) queries extracting functions, arrow-function consts, classes, methods, interfaces, enums, type aliases, and exported consts; imports from `import`/`export from` statements. TSX additionally covers function components.
Acceptance criteria:
- `.ts` and `.tsx` route to distinct grammars; both fixtures extract without error.
- Class methods yield `ClassName::method` qualified names; interface members are not emitted as separate symbols (interface itself is one symbol, kind `interface`).
- Default and named imports captured as written module specifiers.
Test cases:
- test_ts_extracts_class_and_interface: `sample.ts` with `export interface RetryPolicy` (lines 1–4), `export class HttpClient` with method `async get(url: string): Promise<Response>` and `export const parseUrl = (s: string) => new URL(s)` → symbols `[RetryPolicy (interface), HttpClient (class), HttpClient::get (method), parseUrl (function)]` with correct line ranges.
- test_ts_imports: `import { z } from "zod"; import fs from "node:fs";` → imports `["zod", "node:fs"]`.
- test_tsx_component_extracted: `sample.tsx` with `export function UserCard(props: { name: string }): JSX.Element` → symbol `UserCard`, kind `function`, signature containing `props: { name: string }`.
- test_type_alias_kind: `export type UserId = string;` → symbol `UserId`, kind `type`.

### [x] E3-07: JavaScript extraction
Owner: R2 · Estimate: 1d · Depends: E3-05
Files: rust/compresso-atlas/queries/javascript.scm, tests + fixture tests/fixtures/lang/sample.js
Description: tree-sitter-javascript queries for function declarations, arrow/function-expression consts, classes and methods, and module-level `module.exports` assignments; imports from ESM `import` and CJS `require()` calls with string-literal arguments.
Acceptance criteria:
- `.js`, `.jsx`, `.mjs`, `.cjs` all route to the JavaScript grammar.
- Both ESM and CJS import forms are captured; dynamic `require(variable)` is ignored (not an error).
- Class methods produce `ClassName::method` qualified names.
Test cases:
- test_js_functions_and_class: `sample.js` with `function connect(url) {}`, `const retry = async (fn) => {}` and `class Pool { acquire() {} }` → symbols `[connect (function), retry (function), Pool (class), Pool::acquire (method)]`.
- test_js_cjs_and_esm_imports: `const path = require("path");` plus `import express from "express";` → imports `["path", "express"]`.
- test_js_dynamic_require_ignored: `require(pluginName)` → not present in imports, extraction succeeds.

### [x] E3-08: Python extraction
Owner: R2 · Estimate: 1.5d · Depends: E3-05
Files: rust/compresso-atlas/queries/python.scm, tests + fixture tests/fixtures/lang/sample.py
Description: tree-sitter-python queries for module functions, classes, methods (including decorated and async defs), and module-level constants (UPPER_CASE assignments); imports from `import x`, `import x as y`, and `from x import y` forms as written.
Acceptance criteria:
- Methods carry `ClassName::method` qualified names; decorators do not shift the recorded `start_line` (line range covers `def` through body end; decorator line recorded in signature when present).
- `async def` extracted with kind `function`/`method` and `async` preserved in the signature.
- All three import forms captured as written source module strings.
Test cases:
- test_py_class_and_methods: `sample.py` with `class JobQueue:` containing `def push(self, job):` and `async def drain(self):`, plus module-level `def make_queue():` and `MAX_RETRIES = 3` → symbols `[JobQueue (class), JobQueue::push (method), JobQueue::drain (method), make_queue (function), MAX_RETRIES (const)]`.
- test_py_imports_three_forms: `import os`, `import numpy as np`, `from typing import Optional` → imports `["os", "numpy", "typing.Optional"]`.
- test_py_decorated_line_range: `@retry` on the line above `def push` → symbol start_line is the `def` line and signature notes the decorator.

### [x] E3-09: Incremental Indexer
Owner: R2 · Estimate: 2.5d · Depends: E3-04, E3-05, E3-06, E3-07, E3-08, E2-02
Files: rust/compresso-atlas/src/indexer.rs, tests in rust/compresso-atlas/tests/indexer_incremental.rs
Description: Implement `Indexer::index(root, progress)` and `IndexReport` per `03-interfaces.md` §4: canonicalize + `approve_repo` → `scan` → parse only `added`/`changed` via `ParserRegistry` → `replace_symbols` per file in its own transaction → `delete_files` for removed → stamp `index_versions` with `parser_version` and `schema_version`. `token_estimate` computed with `core::tokens` counter. A parser_version mismatch against `index_versions` marks the report `stale` and triggers full reparse.
Acceptance criteria:
- Second run on an unchanged tree parses zero files (`progress` callback observes zero parse events; `files_indexed == 0`).
- Touching one file reparses exactly that file; deleting a file removes its rows via cascade and lists it in the report.
- A crash injected between two files (test hook) leaves every fully-processed file consistent and the interrupted file at its previous state (per-file transactions).
- `IndexReport.duration_ms` populated; `stale == true` when stored parser_version differs.
Test cases:
- test_full_then_noop_index: index the multi-language fixture repo (rs+ts+js+py, 8 files) → `files_indexed == 8`; rerun → `files_indexed == 0`, symbol count unchanged.
- test_touch_one_file_reparses_only_it: modify `src/app.ts`, rerun with a progress recorder → parse events `== ["src/app.ts"]`.
- test_delete_cascades: remove `src/util.py`, rerun → its symbols/edges gone from store, `report` reflects one removed file.
- test_parser_version_bump_flags_stale: stamp index_versions with "0.0.1", run indexer built as "0.0.2" → `stale == true` and all files reparsed.
- test_per_file_transaction_isolation: inject failure while indexing the 3rd of 5 changed files → first two updated, last three untouched, no partial symbol sets.

### [x] E3-10: Queries — repo_map and outline
Owner: R2 · Estimate: 2d · Depends: E3-09
Files: rust/compresso-atlas/src/queries.rs, tests in rust/compresso-atlas/tests/queries_map_outline.rs
Description: Implement `repo_map(store, repo, budget_tokens)` (directory tree + top symbols per file, ranked PageRank-ish by edge in-degree, truncated to the token budget using the core counter) and `outline(store, repo, path)` (full symbol skeleton with line ranges and token estimates) per `03-interfaces.md` §4.
Acceptance criteria:
- `repo_map` output token count (counted with the same counter used to build it) never exceeds `budget_tokens`; higher in-degree files survive truncation before leaf files.
- `outline` returns every stored symbol for the path in line order with `start_line`/`end_line`/`token_estimate`; unknown path returns a typed not-found error.
- Both are read-only: no writes occur (asserted via SQLite `total_changes`).
Test cases:
- test_repo_map_respects_budget: fixture repo, `repo_map(store, repo, 300)` → `counter.count(rendered) <= 300`.
- test_repo_map_ranks_by_indegree: fixture where `core.rs` is imported by 3 files and `leaf.rs` by none, budget forcing truncation → map contains `core.rs`, omits `leaf.rs`.
- test_outline_line_order_and_ranges: `outline(.., "src/lib.rs")` on the E3-05 fixture → symbols sorted by start_line, ranges matching the extraction test constants.
- test_outline_missing_path: `outline(.., "src/nope.rs")` → `Err(AtlasError::FileNotIndexed)`.

### [x] E3-11: Queries - selector-based unfold, search, and bounded read
Owner: R2 · Estimate: 2.5d · Depends: E3-09
Files: rust/compresso-atlas/src/queries.rs, tests in rust/compresso-atlas/tests/queries_unfold_search_read.rs
Description: Implement `unfold(store, repo, selector: &SymbolSelector)`, ranked search, and `read_bounded`. Unfold uses path plus qualified name and optional kind/signature hash/start-line hint. Zero matches returns NotFound; multiple matches returns typed Ambiguous candidates; it never selects an arbitrary body. On stale files it re-parses and matches the complete selector against current content.
Acceptance criteria:
- Unique selectors return exact current source; ambiguous selectors return at most 20 deterministic candidate identities.
- Staleness is a flag, not an error; deleted symbols return NotFound with reindex hint.
- Every path is canonicalized after symlink resolution and bounded reads honor inclusive ranges/token limits.
Test cases:
- `test_unfold_unique_selector_exact_body`, `test_unfold_overload_returns_candidates`, `test_unfold_trait_impl_disambiguated_by_signature`, `test_unfold_stale_relocates_full_selector`, `test_unfold_deleted_not_found`.
- Search ranking, dotdot/absolute/symlink escapes, internal symlink, range, and token truncation.

### [x] E3-12: Queries — impact and plan_context
Owner: R2 · Estimate: 1.5d · Depends: E3-10, E3-11
Files: rust/compresso-atlas/src/queries.rs, tests in rust/compresso-atlas/tests/queries_impact_plan.rs
Description: Implement `impact(store, repo, path)` (reverse-edge closure over `edges`, depth 2, reporting dependent files and the linking edge kinds) and `plan_context(store, repo, task, budget_tokens)` (search-driven pack: repo map slice + outlines + top matching symbol bodies assembled within the token budget) per `03-interfaces.md` §4.
Acceptance criteria:
- `impact` includes direct importers (depth 1) and their importers (depth 2), never depth 3; each entry names the edge kind; self is excluded.
- `plan_context` output token count ≤ `budget_tokens`; sections ordered map → outlines → symbol bodies; every included symbol matches the task query via FTS.
- Both functions read-only and deterministic for a fixed store state.
Test cases:
- test_impact_depth_two: fixture edges `a.rs → b.rs → c.rs → d.rs` (imports); `impact(.., "c.rs")` → contains b.rs (depth 1) and a.rs (depth 2); wait — reverse edges: importers of c.rs are b.rs, importers of b.rs are a.rs; d.rs (which c imports) absent.
- test_impact_edge_kinds_reported: tests edge `tests/a_test.rs → a.rs (tests)` → impact of a.rs lists a_test.rs with kind `tests`.
- test_plan_context_within_budget: `plan_context(.., "session retry logic", 800)` on fixture → `counter.count(rendered) <= 800` and includes the `SessionManager` outline.
- test_plan_context_deterministic: two calls with identical inputs → identical rendered output.

---

## E4 — CLI

### [x] E4-01: CLI application and canonical command tree
Owner: D1 · Estimate: 2d · Depends: E2-09, E3-12
Files: rust/compresso-cli/src/{main,commands}.rs, rust/compresso-eval/src/cli.rs
Description: Expose account login|status|logout and eval run|gates|diff-gates|workflow|calibrate-anthropic with all existing commands.
Acceptance criteria:
- Shared eval parser/output/exit codes; help, JSON, dry-run; account uses AccountClient.
Test cases:
- command snapshot; both eval entry points; cancelled login writes nothing.

### [x] E4-02: CLI retrieval, compression, and diagnostics
Owner: D1 · Estimate: 2d · Depends: E4-01
Files: rust/compresso-cli/src/commands/
Description: Wire canonical interfaces without duplicated logic.
Acceptance criteria:
- Unfold selector/ambiguity and impact completeness are surfaced; bounds/root checks match MCP; no empty compression.
Test cases:
- JSON snapshots; root, ambiguity, unresolved, stale, malformed, fallback.

### [x] E4-03: Wire engine commands (compress, explain, stats)
Owner: D1 · Estimate: 1d · Depends: E4-01, E2-09, E3-03
Files: rust/compresso-cli/src/commands/engine.rs, tests in rust/compresso-cli/tests/cli_engine.rs
Description: `compress` reads stdin (or `--file`), builds a graph via `ContextGraph::from_text` (kind from `--kind`, default `tool_response`), runs `Engine::compress` with mode/budget flags (`--mode safe|passthrough`, `--target-tokens`), prints compressed content, records the operation via `Store::record_operation`; `explain` prints the trace for an operation id (or `--last`) from the store — ids, counts, checks, never raw content; `stats` renders `Store::stats` in the §3 savings vocabulary with `modeled_cost_saved` explicitly labelled `modeled`.
Acceptance criteria:
- `echo <500-line log> | compresso compress --json` reports `before_tokens`, `after_tokens`, `fallback_used` and stores one operation row; passthrough mode round-trips stdin byte-exact.
- `compresso explain --last --json` returns the stored trace, validating against compression-trace v1.
- `compresso stats --json` field names match 02-schemas §3 exactly; human output labels modeled savings as "modeled".
Test cases:
- test_compress_log_fixture_via_stdin: pipe fixtures/logs/repeated-npm-install/input.txt → JSON `after_tokens < before_tokens`, stdout content contains `"[×500"`.
- test_passthrough_byte_exact: `--mode passthrough` with arbitrary UTF-8 input → stdout equals stdin.
- test_explain_last_matches_schema: after a compress, `explain --last --json` → `data.trace` validates against schemas/compression-trace/v1.json and contains no input substring.
- test_stats_labels_modeled: `stats --json` → keys include `net_tokens_saved` and `modeled_cost_saved`; human mode output contains the word "modeled".

### [x] E4-04: doctor and config explain
Owner: D1 · Estimate: 1d · Depends: E4-01, E2-09
Files: rust/compresso-cli/src/commands/doctor.rs, rust/compresso-core/src/config.rs, tests in both
Description: Implement `Config::load()` (default < file < env < flag per `03-interfaces.md` §2.6, file at `~/.config/compresso/config.toml`, env overrides `COMPRESSO_MODE`, `COMPRESSO_TARGET_TOKENS`, `COMPRESSO_BYPASS`, `COMPRESSO_LOG`, `COMPRESSO_TELEMETRY` accepted but forced false) and `Config::explain()` backing `compresso config explain` (key, effective value, `ConfigSource`). `compresso doctor` checks: binary resolvable, data dir writable, DB opens and migrations current, config parses, tokenizers initialize, tree-sitter grammars load, and prints pass/fail per check with remediation hints.
Acceptance criteria:
- `config explain` lists every config key with source `default|file|env|flag`; setting `COMPRESSO_MODE=passthrough` flips the `compression.mode` row to source `env`.
- `COMPRESSO_TELEMETRY=true` is accepted, the effective value remains `false`, and explain annotates it "forced off (D6)".
- `doctor --json` returns a check array; any failure sets exit 1; a corrupted config file yields a failing `config_parses` check, not a crash.
Test cases:
- test_config_precedence_env_over_file: file sets mode=safe, env COMPRESSO_MODE=passthrough → effective passthrough with source Env.
- test_telemetry_forced_false: COMPRESSO_TELEMETRY=true → `Config::load().privacy.telemetry == false`.
- test_doctor_all_green_on_healthy_tmp: doctor with temp HOME and fresh db → exit 0, all checks `passed == true`.
- test_doctor_bad_config_fails_gracefully: config.toml containing `mode = [` → exit 1, JSON check `config_parses.passed == false`, hint names the file path.

### [ ] E4-05: install-local.sh slice installer (D42)
Owner: I1 · Estimate: 1.5d · Depends: E1-06, E4-04
Files: installers/install-local.sh, installers/tests/install_local_test.sh, docs/runbooks/install-local.md
Description: POSIX-sh installer that (1) detects platform/arch, (2) locates the matching `compresso-<version>-<target>.tar.gz` and `SHA256SUMS` from a local path or URL argument, (3) verifies the SHA256 checksum before unpacking, (4) installs atomically to `~/.local/bin/compresso` (temp file + rename, mode 0755, per D23), (5) is idempotent — a re-run replaces the binary and keeps the prior one as `compresso.previous`, and (6) prints PATH guidance when `~/.local/bin` is not on PATH. Unsigned per D42 (signing/notarization/package managers stay E12); conforms to the 08 §3.4 bootstrap constraints: small enough to read in one sitting, no telemetry, no assistant-config mutation, no mutable second-stage code.
Acceptance criteria:
- Clean-machine script run installs a working binary: on a fresh temp `$HOME`, `~/.local/bin/compresso --version` prints the packaged version and `compresso doctor` executes its checks.
- A checksum mismatch aborts before any write under `~/.local/bin`; the error names the expected and actual digests.
- Re-run is idempotent; the previous binary is preserved as `compresso.previous`; the installer never touches `~/.config/compresso` or any assistant config.
Test cases:
- test_install_clean_home: temp HOME + E1-06 fixture tarball → binary at `~/.local/bin/compresso`, executable, `--version` matches the tarball version.
- test_checksum_mismatch_aborts: flip one byte in the tarball → non-zero exit, stderr contains `checksum`, `~/.local/bin` unchanged.
- test_install_idempotent: run twice → working binary plus `compresso.previous` present.
- test_no_config_writes: directory hash of `~/.config` before/after → identical.

---

## E5 — MCP server

### [ ] E5-01: rmcp server bootstrap and initialize handshake
Owner: R2 · Estimate: 1.5d · Depends: E2-09, E3-03, E4-01
Files: rust/compresso-mcp/src/lib.rs, rust/compresso-mcp/src/server.rs, rust/compresso-cli/src/commands/mcp.rs, tests in rust/compresso-mcp/tests/handshake.rs
Description: Implement `serve_stdio(deps: McpDeps)` per `03-interfaces.md` §5 using rmcp's ServerHandler over stdio, wire `compresso mcp serve` to it, and register all ten tool declarations from D7 / `04-mcp-and-integrations.md` §1 (`compresso_repo_map`, `compresso_search`, `compresso_read`, `compresso_outline`, `compresso_unfold`, `compresso_context`, `compresso_impact`, `compresso_compress`, `compresso_stats`, `compresso_explain`) with versioned JSON input schemas; handlers stubbed until E5-02..04. Server instructions string ≤200 words, factual, lists tools, never instructs the client to prefer Compresso over its own policy (build-doc §13.4).
Acceptance criteria:
- Spawning `compresso mcp serve` and sending a JSON-RPC `initialize` returns serverInfo `name == "compresso"` with the crate version, and `tools/list` returns exactly the ten D7 tool names, each with a schema declaring required arguments.
- Instructions string is ≤200 words (unit-tested word count) and contains no imperative directed at the client's tool-choice policy (denylist assertion: no "always use", "prefer compresso", "instead of your").
- Server exits cleanly on stdin EOF; a malformed JSON-RPC line yields a protocol error response, not a crash.
Test cases:
- test_initialize_handshake: spawn binary, send initialize + initialized, read response → `serverInfo.name == "compresso"`, protocol version echoed.
- test_tools_list_exact_ten: `tools/list` result names sorted == the ten D7 names sorted.
- test_instructions_word_budget: instructions split_whitespace count ≤ 200; denylist phrases absent.
- test_malformed_frame_survives: send `{"jsonrpc":"2.0",` then a valid request → valid request still answered.

### [ ] E5-02: Navigation handlers - repo map, outline, selector-based unfold
Owner: R2 · Estimate: 1.5d · Depends: E5-01, E3-10, E3-11
Files: rust/compresso-mcp/src/tools/atlas_nav.rs, tests in rust/compresso-mcp/tests/tools_nav.rs
Description: Implement the three navigation handlers against the exact JSON schemas in 04 §1. Root naming, defaults, bounds, SymbolSelector, ambiguity, and errors must match those schemas.
Acceptance criteria:
- Repo map and outline preserve their bounded contracts.
- Unfold passes the complete selector to E3-11 and returns found/ambiguous/not_found as structured success; ambiguous candidates are capped at 20 and deterministic.
- Invalid arguments and root violations return stable MCP errors; stale unique matches return current content with stale=true.
Test cases:
- `test_repo_map_budget`, `test_outline_matches_atlas`, `test_unfold_unique`, `test_unfold_ambiguous_candidates`, `test_unfold_signature_disambiguates`, `test_unfold_stale_current`, `test_unfold_deleted_not_found`, `test_unfold_schema_bounds`.

### [ ] E5-03: Tool handlers — compresso_search, compresso_read (gate mode), compresso_context
Owner: R2 · Estimate: 1.5d · Depends: E5-01, E3-11, E3-12
Files: rust/compresso-mcp/src/tools/atlas_read.rs, tests in rust/compresso-mcp/tests/tools_read.rs
Description: Implement `compresso_search` (→ `search`), `compresso_read` (→ `read_bounded`, plus a `gate: true` argument implementing the Tetris `tetris_context_gate` semantics per D7: return size/token estimate and a worth-reading verdict without file content), and `compresso_context` (→ `plan_context`). All path arguments flow through the E3-11 canonicalization boundary; `OutsideRoot` maps to a stable MCP error code and never echoes the resolved path outside the root.
Acceptance criteria:
- `compresso_read` with `gate: true` returns the 04 §1.5 gate object (`worth_reading`, `reason`, `file_tokens_total`, `recommended_budget`, `cheaper_alternative`) only — response contains zero file-content bytes.
- Traversal/symlink-escape inputs return error code `outside_root`; repository content in responses is data-only (no tool-side execution of anything found in files).
- `compresso_context` respects `max_tokens` and returns the plan sections in map→outline→symbol order.
Test cases:
- test_read_tool_bounded: read `src/sample.rs` with `max_tokens: 100` → ≤100 tokens, `truncated: true` in structured result.
- test_read_gate_mode_no_content: `{"path":"src/sample.rs","gate":true}` → response includes `gate.worth_reading`, `gate.file_tokens_total`, and `gate.recommended_budget` and omits every line of the file (assert a known source line absent).
- test_read_dotdot_via_mcp: `{"path":"../../etc/passwd"}` → MCP error `outside_root`.
- test_search_tool_hits: `{"query":"SessionManager"}` → first hit qualified_name `SessionManager` with path and line.
- test_context_tool_budget: `{"task":"session retry logic","max_tokens":800}` → ≤800 tokens.

### [ ] E5-04: Tool handlers — compresso_compress, compresso_impact, compresso_stats, compresso_explain
Owner: R2 · Estimate: 1.5d · Depends: E5-01, E2-09, E3-03, E3-12
Files: rust/compresso-mcp/src/tools/engine_tools.rs, tests in rust/compresso-mcp/tests/tools_engine.rs
Description: `compresso_compress` builds a graph from the `text` argument (D7: compatible with `tetris_compress_text`), runs `Engine::compress` in the configured mode, returns compressed text plus real-tokenizer before/after counts; `compresso_impact` → `impact`; `compresso_stats` → `Store::stats` in the §3 vocabulary; `compresso_explain` returns the stored trace for an `operation_id`. Operation recording follows D47 (02-schemas §4, 04 §1.1): `compresso_compress` records a compression operation whose `trace_json` is the compression-trace v1 document validated against the **root** schema; the retrieval handlers (`compresso_impact`, `compresso_stats`, `compresso_explain`, plus the E5-02/E5-03 navigation and read tools) record `trace_json = NULL` with `tokens_served` set to the tokens returned to the client (E5-05 supplies the session id plumbing).
Acceptance criteria:
- `compresso_compress` on the 500-line log fixture text returns content containing `[×500` with `after_tokens < before_tokens`; on unparseable/edge input it returns the original text with `fallback_used: true` — never an MCP error for content reasons.
- `compresso_explain` output validates against compression-trace v1 and contains no raw content.
- `compresso_stats` returns `observed_tokens`/`applied_tokens`/`net_tokens_saved` consistent with the operations written during the test session.
Test cases:
- test_compress_tool_log_fixture: text = fixture → structured `{before_tokens, after_tokens, fallback_used:false}`, text contains `"[×500"`.
- test_compress_tool_fail_open: engine forced into a failing strategy (test seam via env `COMPRESSO_TEST_FAIL_STRATEGY`) → returned text equals input, `fallback_used:true`.
- test_impact_tool_depth_two: fixture edges as E3-12 → same b.rs/a.rs result via MCP.
- test_explain_roundtrip: compress then explain with the returned operation_id → trace validates against schema.
- test_stats_reflects_session_ops: two compress calls → `observed_tokens` equals the sum of both inputs' counts.
- test_compress_records_root_valid_trace: after a compress call, its `operations` row's `trace_json` validates against the compression-trace v1 root schema.
- test_retrieval_records_tokens_served: after `compresso_impact`, its `operations` row has `trace_json` NULL and `tokens_served` equal to the returned payload's token count (D47).

### [ ] E5-05: Sessions and operation ledger
Owner: R2 · Estimate: 1.5d · Depends: E5-01, E3-03
Files: rust/compresso-mcp/src/session.rs, tests in rust/compresso-mcp/tests/session_ledger.rs
Description: On MCP connect, call `Store::start_session(client, "stdio")` (client name from initialize clientInfo, `"unknown"` fallback); thread the `SessionId` through `McpDeps` so every tool handler records an `OperationRecord` with the tool name, mode, observed/applied token counts, and — per D47 — `trace_json` for `compresso_compress` only, while retrieval tools record `tokens_served` with `trace_json = NULL`; close the session (set `ended_at`) on disconnect. Bounded concurrency: tool calls execute serially per session (rmcp default) and the ledger write failure never fails the tool response (log + continue, fail-open).
Acceptance criteria:
- One `sessions` row per MCP connection with `client` from clientInfo and `ended_at` set after EOF.
- Every tool invocation produces exactly one `operations` row with `tool` equal to the D7 tool name and `session_id` linking to the session.
- A read-only DB (chmod 444 in test) does not break tool responses; the failure is logged to stderr.
Test cases:
- test_session_row_lifecycle: connect, call one tool, disconnect → session row with client `"test-harness"`, non-null ended_at.
- test_operation_per_tool_call: 3 tool calls (search, read, compress) → 3 operations rows sharing the session_id; the search and read rows have `trace_json` NULL with `tokens_served` set, the compress row carries a root-schema-valid trace (D47).
- test_ledger_failure_fail_open: make DB read-only, call `compresso_search` → valid MCP result returned, stderr contains a ledger warning.

### [ ] E5-06: MCP startup and eager tokenizer lifecycle
Owner: R2 · Estimate: 1.5d · Depends: E5-01, E2-02
Files: rust/compresso-mcp/src/server.rs, rust/compresso-core/src/tokens.rs, benches/startup.rs
Description: Initialize configured tokenizers before accepting tools; separate cold startup from warm calls.
Acceptance criteria:
- Initialize once under 500ms; warm P95 under 200ms; failure keeps passthrough MCP alive.
Test cases:
- pre-initialize construction; no handler construction; cold/warm; failure; EOF.

### [ ] E5-07: Manual verification in Claude Code
Owner: R2 · Estimate: 1.5d · Depends: E5-02, E5-03, E5-04, E5-05, E5-06
Files: docs/runbooks/mcp-manual-verification.md
Description: Register the built server in Claude Code (manual `claude mcp add compresso -- ~/.local/bin/compresso mcp serve` — the E6 adapter is not a dependency), exercise every one of the ten tools against the compresso repo itself (dogfood per D9), and record the results in a runbook checklist: arguments used, response summary, latency observation, and any schema mismatch. File and fix (or ticket) every deviation before closing.
Acceptance criteria:
- All ten tools invoked from a live Claude Code session with results recorded; zero handshake or schema errors in the client log.
- `compresso_read` gate mode and an `outside_root` rejection are both demonstrated from the client side.
- Runbook committed with the exact registration command, verification date, client version, and a checklist that is fully checked.
Test cases:
- manual_checklist_all_tools_pass: runbook table has a checked row per tool with observed latency.
- manual_outside_root_demonstrated: transcript excerpt in the runbook shows `outside_root` returned for a traversal path.

---

## E6 — Integrations

### [ ] E6-01: Integration trait, base-bound plans, and receipts
Owner: D1 · Estimate: 2d · Depends: E5-01
Files: rust/compresso-integrations/src/{lib,plan,receipt}.rs, schemas/integrations/v1.json
Description: Define detect and plan/apply install/uninstall with full-file base hash, plan, diff, expiry, session, backup.
Acceptance criteria:
- Rehash before writes; drift/forge/expiry/session reject; atomic receipts; owned-key uninstall; idempotent.
Test cases:
- outside-diff rewrite; forged/expired; crash; unknown keys.

### [ ] E6-02: Claude Code adapter
Owner: D1 · Estimate: 2d · Depends: E6-01, E5-01
Files: rust/compresso-integrations/src/claude_code.rs, tests in same file
Description: Adapter with `client_id() == "claude-code"` implementing detection (CLI on PATH, config file per `04-mcp-and-integrations.md` §2 — user scope `~/.claude.json`, project scope `.mcp.json`), plan/install writing only the Compresso-owned key `mcpServers.compresso` = stdio server entry `{command: <compresso_bin>, args: ["mcp","serve"]}`, verify (config parses and the command path resolves to an existing executable), and uninstall (remove only the owned key; restore untouched siblings). JSON edits preserve unrelated keys and their order-insensitive content exactly. No Claude Desktop paths anywhere (D5).
Acceptance criteria:
- Install into a config that already has other `mcpServers` entries leaves those entries and all top-level keys byte-preserved after JSON normalization; only `mcpServers.compresso` is added.
- Uninstall with the receipt removes exactly that key; a user-modified `mcpServers.compresso` (differs from receipt) is left in place with a warning in `RestoreReport` (never clobber non-owned changes).
- `detect` distinguishes Installed{version, config_path} / NotFound; adapter contains no reference to Claude Desktop config locations.
Test cases:
- test_install_preserves_existing_servers: temp HOME with `~/.claude.json` containing `mcpServers.other` → after install both `other` and `compresso` present, `other` deep-equal to before.
- test_verify_fails_on_missing_binary: entry pointing at a nonexistent path → `VerificationReport` failing check `binary_resolves`.
- test_uninstall_removes_only_owned_key: install then uninstall → config deep-equal to the original file.
- test_uninstall_skips_user_modified_entry: mutate the compresso entry after install, uninstall → entry retained, RestoreReport warns `key_modified`.
- test_project_scope_writes_mcp_json: `Scope::Project(dir)` → `.mcp.json` created in dir with the compresso entry, user config untouched.

### [ ] E6-03: Codex integration adapter
Owner: D1 · Estimate: 2d · Depends: E6-01
Files: rust/compresso-integrations/src/codex.rs
Description: Implement Codex CLI/Desktop without Claude assumptions.
Acceptance criteria:
- Base-bound owned-key lifecycle and typed unsupported no-op.
Test cases:
- fresh/existing/malformed; drift; disable/uninstall/repeat; platforms.

### [ ] E6-04: Cursor adapter
Owner: D1 · Estimate: 1.5d · Depends: E6-01, E5-01
Files: rust/compresso-integrations/src/cursor.rs, tests in same file
Description: Adapter with `client_id() == "cursor"` targeting `~/.cursor/mcp.json` (user scope) and `<project>/.cursor/mcp.json` (project scope) per `04-mcp-and-integrations.md` §2, owning `mcpServers.compresso` with the stdio entry. Same preservation, verify, and uninstall semantics as E6-02; shares the JSON editing helper from E6-01.
Acceptance criteria:
- Install/uninstall round-trip preserves unrelated `mcpServers` entries and unknown top-level keys.
- `verify` checks JSON validity and binary resolution; `detect` reports Installed with the config path that exists (user or project).
- Project scope never touches the user file and vice versa.
Test cases:
- test_cursor_install_adds_entry: temp `~/.cursor/mcp.json` with one existing server → both present after install.
- test_cursor_uninstall_restores: uninstall → deep-equal to original JSON.
- test_cursor_project_scope_isolated: `Scope::Project` install → project file has entry, user file byte-unchanged.

### [ ] E6-05: Integration parity and restoration suite
Owner: D1+PQ · Estimate: 2d · Depends: E6-02, E6-03, E6-04
Files: rust/compresso-integrations/tests/parity.rs, fixtures/integrations/
Description: Run one contract over Claude Code, Codex, Cursor.
Acceptance criteria:
- Detection, base plan, drift, atomic apply, receipts, disable, uninstall match.
Test cases:
- rewrite; malformed; crash; unknown keys; repeats; privacy.


---

## E7 — Eval & fixtures

### [x] E7-01: Fixture loader
Owner: PQ · Estimate: 2d · Depends: E1-05, E2-01
Files: rust/compresso-eval/src/fixtures.rs, tests in same file
Description: Implement `FixtureSet` loading the authoritative corpus format from `docs/plan/06-fixtures.md` §2: each fixture directory contains `fixture.toml` declaring exactly one input form (`graph` | `[[input.files]]`/`[[input.nodes]]` | `repo`), an `[expectations]` table (`min_reduction_pct`, `must_preserve`, `must_not_contain`, `must_not_contain_in_trace`, `protected_node_ids`, `expect_fallback`, optional `[expectations.atlas]`), optional `[[recall_probes]]` entries (D40: `question`, `answer_substring`, `protected`), and the top-level `eligible` flag governed by the §4 eligibility rules (adversarial, provider-payloads, and atlas fixtures are ineligible). Loader validates every fixture at load time (unknown TOML keys, name/dir and category/dir mismatch, multiple input forms, eligibility-rule violations are errors), recomputes token counts, and materializes a `ContextGraph` per fixture.
Acceptance criteria:
- `FixtureSet::load("fixtures/")` loads every committed fixture across all six categories; any malformed `fixture.toml` fails loading with the offending path in the error.
- Eligibility flags parsed and exposed; adversarial fixtures load with `eligible == false`; §4 rule violations (`eligible = true` with `mode != "safe"`, `expect_fallback = true`, or `min_reduction_pct == 0`) are load errors.
- `[[recall_probes]]` parse with `answer_substring` required and `protected` defaulting to false.
- Graphs built from fixtures pass `validate()` including declared protection flags.
Test cases:
- test_load_all_committed_fixtures: load fixtures/ → count ≥ 5, one per category minimum, zero errors.
- test_malformed_fixture_rejected: temp fixture with the `[expectations]` table missing → `Err` naming the fixture path and the missing table.
- test_eligibility_parsed: `fixtures/adversarial/prompt-injection-repo/fixture.toml` → `eligible == false`; `fixtures/logs/repeated-npm-install` → `eligible == true`.
- test_recall_probes_parsed: fixture with two `[[recall_probes]]` (one `protected = true`) → probes exposed with correct `answer_substring` and protected flags.
- test_eligibility_rule_violation_rejected: temp fixture with `eligible = true` and `mode = "passthrough"` → load error citing the §4 eligibility rules.
- test_fixture_graph_validates: every loaded fixture's graph `validate().is_ok()`.

### [x] E7-02: Eval runner CLI
Owner: R1 · Estimate: 2d · Depends: E7-01, E2-09
Files: rust/compresso-eval/src/runner.rs, rust/compresso-eval/src/main.rs, tests in rust/compresso-eval/tests/runner.rs
Description: Implement `EvalRunner`: for each fixture, run `Engine::compress` in the fixture's declared mode, verify `expected` assertions (required/forbidden substrings, reduction bounds, protected-node byte-equality, output JSON validity where input was JSON), collect per-fixture and aggregate metrics (median reduction on eligible fixtures, P95 latency, protected-mutation count, malformed-JSON count, fallback rate per build-doc §25.3), and emit a JSON report. Binary interface: `cargo run -p compresso-eval -- run fixtures/ --gates docs/plan/gates.toml` per `01-stack` §4.
Acceptance criteria:
- Report JSON contains per-fixture rows (id, mode, before/after tokens, latency ms, passed) and aggregates (`median_reduction_eligible`, `p95_latency_ms`, `protected_mutations`, `malformed_json`, `fallback_rate`).
- A fixture whose expected substring is missing marks that fixture failed without aborting the run.
- Runner is deterministic across runs on the committed corpus (identical report modulo timings).
Test cases:
- test_runner_full_corpus_report_shape: run on fixtures/ → JSON parses, has `fixtures[]` and `aggregate` keys, all committed fixtures present.
- test_log_fixture_expectations: repeated-npm-install row → `after_tokens < before_tokens`, output contains `"[×500"` per its fixture.toml expected block.
- test_failed_expectation_isolated: inject a temp fixture expecting substring "UNOBTAINIUM" → that row `passed == false`, aggregate still produced, exit code reflects failure only via gates (E7-03).
- test_protected_mutation_counter_zero: aggregate `protected_mutations == 0` on the committed corpus.

### [ ] E7-03: Release gates file and gate checks
Owner: PQ · Estimate: 2d · Depends: E7-02, E2-11
Files: docs/plan/gates.toml, rust/compresso-eval/src/gates.rs, tests in same file
Description: Implement `Gates` loading `docs/plan/gates.toml` and evaluating the aggregate report against the D37 / build-doc §25.6 slice gates: `median_reduction_eligible > 0.25`, `protected_mutations == 0`, `malformed_json == 0`, `p95_latency_ms < 200`. `compresso-eval -- run ... --gates` exits non-zero with a named failing gate when any gate regresses; gates.toml is the single committed source of thresholds (consumed by CI, referenced by E2-11).
Acceptance criteria:
- gates.toml committed with the four D37 slice gates and their thresholds, each with a comment citing D37/§25.6 (E7-06 extends the same file with the D40 recall thresholds).
- Gate evaluation prints one line per gate: name, threshold, observed, pass/fail; JSON mode includes the same structure.
- Committed corpus passes all four gates in CI.
Test cases:
- test_gates_pass_on_committed_corpus: run + gates on fixtures/ → exit 0, all four gates pass.
- test_reduction_gate_fails_below_threshold: synthetic report with median_reduction 0.10 → exit non-zero, output names `median_reduction_eligible`.
- test_protected_gate_zero_tolerance: report with `protected_mutations == 1` → gate fails regardless of other metrics.
- test_gates_toml_parses_and_matches_d37: parse gates.toml → thresholds `{0.25, 0, 0, 200}` exactly.

### [ ] E7-04: Eval CI integration
Owner: I1 · Estimate: 1.5d · Depends: E7-03, E1-02
Files: .github/workflows/ci.yml (eval job)
Description: Add the `eval` CI job per `01-stack` §4 on ubuntu-24.04 and macos-14: build release, run `cargo run -p compresso-eval --release -- run fixtures/ --gates docs/plan/gates.toml`, upload the JSON report as an artifact, and fail the workflow on any gate regression. Latency-sensitive gates run only on the macos-14 leg (dedicated hardware class) with the ubuntu leg enforcing the non-latency gates, to keep the P95 gate meaningful.
Acceptance criteria:
- Eval job green on main with the committed corpus; report artifact downloadable per run.
- A deliberate gate regression on a scratch branch (threshold temporarily raised to 0.99) fails the job with the gate name in the log, verified once and documented in the PR.
- Job runs after the rust build job and reuses its cache (no duplicate cold compile).
Test cases:
- test_ci_eval_wiring: actionlint passes; ci.yml contains the eval invocation string with `--gates docs/plan/gates.toml`.
- test_gate_regression_fails_ci: scratch-branch verification recorded in the PR description with the failing run link.

### [x] E7-05: Recall-probe evaluation and passthrough baseline
Owner: PQ · Estimate: 2d · Depends: E7-01, E7-02
Files: rust/compresso-eval/src/recall.rs, rust/compresso-eval/src/runner.rs (probe wiring), tests in rust/compresso-eval/tests/recall.rs
Description: Implement the D40 task-success proxy per `06-fixtures.md` §4.9. After each compression fixture run, search every `[[recall_probes]]` entry's `answer_substring` byte-exact over the concatenated output node contents; aggregate corpus-wide into `protected_probe_recall` (probes with `protected = true`) and `overall_probe_recall` (all probes). Additionally run every compression fixture that carries at least one probe in `passthrough` mode as a baseline: a probe failing on passthrough output means the probe itself is broken, and the run errors (broken-probe detection, distinct exit from a gate failure) instead of counting the probe. The eval report gains a `recall` block with per-fixture probe outcomes and the two aggregates.
Acceptance criteria:
- Probe evaluation is byte-exact substring search over compressed output node contents; per-probe pass/fail is recorded with fixture name and probe index in the report.
- The passthrough baseline runs automatically for every compression fixture carrying probes; a probe absent from passthrough output aborts the run with error code `broken_probe` naming the fixture and probe.
- `protected_probe_recall` and `overall_probe_recall` are computed over the whole corpus, emitted in the JSON report, and deterministic across runs.
Test cases:
- test_probe_passes_on_preserved_answer: fixture whose compression preserves `Cannot find module 'left-pad'` → probe passes; both aggregates equal 1.0 on that corpus.
- test_probe_failure_counted_not_fatal: temp fixture whose probe answer is dropped by pack_budget → probe fails, `overall_probe_recall < 1.0`, run completes normally.
- test_broken_probe_errors_on_passthrough: temp fixture whose probe answer never existed in the input → run errors with `broken_probe`, no recall aggregates emitted.
- test_protected_probes_bucketed_separately: mixed protected/unprotected probes land in the correct aggregates.

### [ ] E7-06: Recall gates in gates.toml
Owner: PQ · Estimate: 1d · Depends: E7-03, E7-05
Files: docs/plan/gates.toml, rust/compresso-eval/src/gates.rs, tests in same file
Description: Extend `Gates` and `docs/plan/gates.toml` with the D40 recall thresholds from `06-fixtures.md` §4.9: `protected_probe_recall == 1.0` (100% of protected probes) and `overall_probe_recall >= 0.99`. Gate evaluation consumes the E7-05 recall block; a probes-exist sanity check (mirroring the five-fixture-minimum rule of §4.5) fails the gate when the corpus contains zero recall probes, so deleting probes cannot game the gate. Runs through the existing E7-04 eval CI job unchanged.
Acceptance criteria:
- gates.toml gains a `[recall]` section with `protected_min = 1.0` and `overall_min = 0.99`, each commented citing D40 / 06-fixtures §4.9.
- A single failed protected probe fails the gate regardless of overall recall; the overall boundary is exact (0.989 fails, 0.990 passes).
- The committed corpus passes both recall gates in CI.
Test cases:
- test_recall_gates_pass_on_committed_corpus: run + gates on fixtures/ → both recall gates pass.
- test_protected_recall_zero_tolerance: synthetic report with `protected_probe_recall = 0.99` → gate fails naming `protected_probe_recall`.
- test_overall_recall_boundary: synthetic reports at 0.989 and 0.990 → fail then pass.
- test_probeless_corpus_fails_gate: synthetic report with zero probes → recall gate fails with reason `no_probes`.

---

## E8 — Supervisor & local API

Per `03-interfaces.md` §7 and build-doc Phase 4. All endpoints are authenticated and localhost-only; every payload carries `schema_version` and is validated against `schemas/local-api/v1.json`; unknown versions fail open, never crash. Epic estimate: ~4 wk.

### [ ] E8-01: Supervisor lifecycle and single-instance lock
Owner: R2 · Estimate: 3d · Depends: E5-07, E4-04
Files: rust/compresso-agent/src/lib.rs, rust/compresso-agent/src/supervisor.rs, rust/compresso-cli/src/commands/agent.rs, tests in rust/compresso-agent/tests/lifecycle.rs
Description: `compresso-agent` becomes the local supervisor: `compresso agent start|stop|status` (all with `--json`) daemonize, terminate, and query it. The supervisor owns the `Store` handle (one process, one connection per D41), supervises its worker tasks, and restarts a crashed component with exponential backoff (cap 60 s). A single-instance lock at `~/.local/state/compresso/agent.lock` (advisory lock + PID) prevents double starts; a stale lock (dead PID) is reclaimed with a log line.
Acceptance criteria:
- `agent start` twice: the second invocation exits 1 with `error.code == "already_running"` and the first instance is unaffected; kill-and-restart reclaims the stale lock.
- A crashed worker (test seam panics a component) is restarted with backoff; restarts are visible in `agent status --json` as a per-component restart count.
- `agent stop` terminates cleanly: lock removed, open sessions closed (`ended_at` set), exit 0.
Test cases:
- test_agent_start_stop_status_roundtrip: start under a temp state dir, status shows `running: true` with PID, stop → `running: false`, lock file gone.
- test_second_start_rejected: start twice → second exits 1 with `already_running`.
- test_stale_lock_reclaimed: write a lock naming a dead PID → start succeeds, log notes reclamation.
- test_component_restart_backoff: seam-crash a worker 3× → restart timestamps show increasing intervals, `restart_count == 3` in status.

### [ ] E8-02: API token generation and rotation
Owner: R2 · Estimate: 1.5d · Depends: E8-01
Files: rust/compresso-agent/src/token.rs, rust/compresso-cli/src/commands/agent.rs (rotate-token), tests in rust/compresso-agent/tests/token.rs
Description: Bearer token per 03-interfaces §7: 32 random bytes hex-encoded, generated on first start into `~/.local/state/compresso/api-token` (file 0600, parent directory 0700), reused across restarts. `compresso agent rotate-token` regenerates it atomically (tmp + rename); the running supervisor reloads without restart and rejects the old token immediately. The token value is never logged (redaction verified).
Acceptance criteria:
- Fresh start creates the token file 0600 inside a 0700 directory; restart reuses the same token.
- After rotate-token: new value accepted, old value rejected with 401, no listener downtime (in-flight requests complete).
- Log scan finds zero occurrences of any token value.
Test cases:
- test_token_created_0600_dir_0700: permissions asserted on first start.
- test_token_reused_across_restart: stop + start → same token bytes.
- test_rotate_invalidates_old: request with the pre-rotation token → 401; with the new token → 200.
- test_token_not_in_logs: canary token generated, agent exercised → canary absent from all log files.

### [ ] E8-03: Transport bootstrap — UDS/named pipe + authenticated localhost HTTP
Owner: R2 · Estimate: 3d · Depends: E8-02
Files: rust/compresso-agent/src/api/mod.rs, rust/compresso-agent/src/api/auth.rs, schemas/local-api/v1.json, tests in rust/compresso-agent/tests/transport.rs
Description: Serve the 03-interfaces §7 local API over (a) a Unix domain socket at `~/.local/state/compresso/api.sock` (0600, dir 0700; Windows: named pipe `\\.\pipe\compresso-<user-sid>`) and (b) localhost HTTP bound to `127.0.0.1`/`::1` only on a configured private port — remote interfaces never bound. Every HTTP request requires `Authorization: Bearer <api-token>`; failures return 401 with no detail body. Browser `Origin` headers and unexpected `Host` values are rejected; no CORS headers are ever emitted. Commit `schemas/local-api/v1.json` and validate every request/response at the boundary (unknown `schema_version` → versioned error response, never a crash). Request-size, concurrency, and per-method timeout limits per 08 §7 (the full E14-04 suite extends these tests).
Acceptance criteria:
- Listener address asserts loopback-only binding; the UDS path lives under the 0700 state dir with 0600 socket permissions.
- Missing/incorrect bearer → 401 with empty body; UDS requests are authenticated by peer credentials (SO_PEERCRED/getpeereid) where the platform provides them, bearer otherwise.
- `Origin: https://evil.example` or a mismatched `Host` → 403; no `Access-Control-Allow-*` header appears in any response.
Test cases:
- test_http_401_without_token / test_http_200_with_token_on_health.
- test_origin_header_rejected: browser-style Origin → 403.
- test_host_confusion_rejected: `Host: evil.example` → 403.
- test_uds_health_roundtrip: UDS request → health payload.
- test_unknown_schema_version_fails_open: request with `schema_version: 99` → typed error response, server still serving.
- test_body_size_limit_413: oversized body rejected.

### [ ] E8-04: Health, session, operation, and stats endpoints
Owner: R2 · Estimate: 2.5d · Depends: E8-08
Files: rust/compresso-agent/src/api/{health,sessions,operations,stats}.rs
Description: Expose authenticated bounded views backed by HealthService.
Acceptance criteria:
- Deterministic pagination; no content; observed actuals separate modeled savings.
Test cases:
- health states; pagination; auth/origin; redaction.

### [ ] E8-05: Repo management endpoints
Owner: R2 · Estimate: 2d · Depends: E8-03
Files: rust/compresso-agent/src/api/repos.rs, tests in rust/compresso-agent/tests/api_repos.rs
Description: `GET /v1/repos` (approved repositories + index status from `index_versions`), `POST /v1/repos` `{root}` → canonicalize, `Store::approve_repo`, then an async `Indexer::index` whose progress is readable via the `/v1/health` `indexer` component; `DELETE /v1/repos/:id` removes the repository with cascade. Every path flows through the E3-11 canonicalization boundary; a non-directory or unresolvable root returns `invalid_arguments`; no repository is ever indexed via the API without first appearing in `repositories`.
Acceptance criteria:
- POST canonicalizes symlinked roots to one repository identity (no duplicate rows for `/tmp/x` vs `/tmp/link-to-x`).
- Indexing runs asynchronously; GET /v1/repos reflects `last_indexed_at` and staleness; DELETE cascades files/symbols/edges rows.
- Errors use the standard local-API error shape with stable codes.
Test cases:
- test_post_repo_approves_and_indexes: POST fixture repo → row created, index completes, GET shows symbol counts via status.
- test_symlinked_root_canonicalized: POST the symlink → same repo id as the real path.
- test_delete_repo_cascades: DELETE → files/symbols gone from the store.
- test_nondirectory_root_rejected: POST a file path → `invalid_arguments`, no row.
- test_index_progress_via_health: during a large index, health `indexer` component reports progress.

### [ ] E8-06: Configuration plan/apply endpoints
Owner: R2+D1 · Estimate: 2d · Depends: E8-03
Files: rust/compresso-agent/src/api/config.rs, schemas/local-api/config-v1.json
Description: Implement GET plus POST plan/apply; no direct PUT.
Acceptance criteria:
- Plan includes base/hash/expiry; apply checks approval/session/expiry/full bytes then atomic validates/writes.
Test cases:
- drift anywhere; forge/expire/session; invalid; crash; dry-run.

### [ ] E8-07: Integration install/uninstall plan/apply endpoints
Owner: R2+D1 · Estimate: 3d · Depends: E8-03, E6-05
Files: rust/compresso-agent/src/api/integrations.rs, schemas/local-api/integrations-v1.json
Description: Expose separate base-bound install and uninstall flows.
Acceptance criteria:
- Drift/session/expiry/approval fail before disk; owned-key restore; idempotent.
Test cases:
- all clients; rewrite; forge; malformed; crash; repeats.

### [ ] E8-08: Health subsystem and read-only fallback
Owner: R2 · Estimate: 3d · Depends: E8-01, E8-03
Files: rust/compresso-agent/src/{health,fallback}.rs
Description: Implement probes/fallback before endpoints.
Acceptance criteria:
- Content-free component health; supervisor failure permits read-only fallback only; clean recovery.
Test cases:
- each unhealthy; crash/restart; fallback read; mutation denial.


---

## E9 — Desktop (Tauri)

Per amended ADR-0010: the desktop app is a **local-API client only** — it never embeds the runtime crates and never touches the SQLite store directly; all data flows over the authenticated E8 local API. Presentation logic only (08 §6); ranking, entitlement evaluation, and strategy logic never appear in webview-reachable state. Epic estimate: ~5 wk.

### [ ] E9-01: Tauri shell, API client, and desktop test harness
Owner: D1 · Estimate: 4d · Depends: E8-04
Files: apps/desktop/, packages/api-client/, apps/desktop/{vitest,playwright}.config.*
Dependency rationale: E9-01 owns security-by-construction defaults for the initial shell: the webview receives no bearer token and has no direct filesystem, shell, or arbitrary-network capability. It intentionally does not depend on E14-05, because E14-05 requires a concrete shell and dist build to harden and verify.
Description: Build shell plus Vitest, RTL, Playwright, mocked contracts, packaged smoke.
Acceptance criteria:
- No webview token/fs/shell/http; component/a11y/contract/package CI exists before pages.
Test cases:
- token/CSP; schema drift; keyboard; offline; package smoke.

### [ ] E9-02: Onboarding flow
Owner: D1 · Estimate: 4d · Depends: E9-01, E8-05, E8-07
Files: apps/desktop/src/onboarding/, e2e in apps/desktop/tests/onboarding.spec.ts
Description: First-run wizard: (1) detect assistants via `GET /v1/integrations`; (2) approve the first repository (`POST /v1/repos`) with index progress from `/v1/health`; (3) install the chosen integration via the two-step plan/approve flow with the unified diff shown before approval; (4) confirmation. Completable without terminal knowledge (Phase 5 exit criterion); every step is skippable and resumable; closing mid-onboarding leaves no partial config writes (every write is one atomic API operation).
Acceptance criteria:
- End-to-end onboarding on a machine with a fake `~/.claude.json` produces an indexed repo and an install receipt without any terminal use.
- Skip and resume both work; wizard state survives app restart.
- Killing the app between plan and approve leaves the assistant config byte-untouched.
Test cases:
- test_onboarding_completes_e2e: temp HOME → wizard through all steps → receipt exists, repo indexed.
- test_skip_and_resume: skip integration step, resume later → completes.
- test_close_midway_no_partial_writes: exit after plan → config hash unchanged.
- test_diff_shown_before_install: approve button disabled until the diff view rendered.

### [ ] E9-03: Integrations UI with diff preview
Owner: D1 · Estimate: 3d · Depends: E9-01, E8-07
Files: apps/desktop/src/integrations/, tests in apps/desktop/tests/integrations.spec.ts
Description: Per-client cards for claude-code, codex, cursor (no Claude Desktop card anywhere, D5) showing detection, receipt status, and verification results. Install/uninstall drive the E8-07 two-step endpoints: the `unified_diff` is rendered verbatim in a monospace view together with the backup path, and the approve action submits the exact `plan_hash`; the `RestoreReport` is shown after uninstall.
Acceptance criteria:
- Approve is disabled until the diff has been displayed; the submitted hash equals the plan's hash.
- A `plan_stale` rejection re-requests a plan and re-renders the new diff (never silently retries install).
- No UI element, string, or route references Claude Desktop.
Test cases:
- test_diff_rendered_before_approve_enabled.
- test_install_submits_matching_plan_hash.
- test_stale_plan_triggers_replan: config mutated mid-flow → new diff shown.
- test_no_claude_desktop_card: rendered client list == three adapters.
- test_restore_report_shown_after_uninstall.

### [ ] E9-04: Repositories UI
Owner: D1 · Estimate: 3d · Depends: E9-01, E8-05
Files: apps/desktop/src/repos/, tests in apps/desktop/tests/repos.spec.ts
Description: Add/remove repositories via `POST/DELETE /v1/repos`; index status with staleness and parser-version mismatch surfaced as a badge with one-click reindex; per-repo skipped-file counts (secret patterns, oversize, binary) from the index report surface.
Acceptance criteria:
- Adding a repo shows live progress and ends in an indexed state; removing prompts for confirmation and deletes index data.
- Stale/parser-mismatch badge triggers reindex with one click.
- Skipped counts render per skip reason.
Test cases:
- test_add_repo_shows_progress_and_completes.
- test_remove_repo_confirmation_and_cascade.
- test_stale_badge_one_click_reindex.
- test_skipped_counts_by_reason.

### [ ] E9-05: Compression Explorer
Owner: D1 · Estimate: 4d · Depends: E9-01, E8-04
Files: apps/desktop/src/explorer/, tests in apps/desktop/tests/explorer.spec.ts
Description: Browse sessions and operations from `/v1/sessions` and `/v1/operations`. Per-operation view renders the trace v1 document for compression rows — mode, per-strategy `strategy_results` deltas, safety-check outcomes, fallback status, explanation — and `tokens_served` for retrieval rows (D47). Zero raw content is displayed anywhere: traces contain none by construction and the UI adds none.
Acceptance criteria:
- Compression rows render all strategy results and the eleven safety checks; retrieval rows render tokens_served and tool name.
- Fallback operations carry a visible badge with `fallback_reason`.
- A canary-content compression (from a test session) never surfaces its content anywhere in the Explorer DOM.
Test cases:
- test_trace_rendered_full_detail.
- test_retrieval_rows_show_tokens_served.
- test_fallback_badge_with_reason.
- test_no_raw_content_in_dom: canary absent from rendered output.
- test_session_filtering.

### [ ] E9-06: Savings dashboard (metrics vocabulary)
Owner: D1 · Estimate: 2.5d · Depends: E9-05
Files: apps/desktop/src/dashboard/, tests in apps/desktop/tests/dashboard.spec.ts
Description: Dashboard over `GET /v1/stats` using the 02-schemas §3 vocabulary verbatim: `observed_tokens`, `applied_tokens`, `removed_tokens`, `compression_overhead`, `net_tokens_saved`, and `modeled_cost_saved` visibly labelled "modeled". `provider_observed_cost` appears only when present (post-slice proxy data, E11-06) and is never fabricated or merged with modeled figures. Time-window and per-tool breakdowns.
Acceptance criteria:
- Field labels map 1:1 to the §3 vocabulary; modeled figures always carry the "modeled" label in the DOM.
- With no provider-verified data, that panel is absent (not zero-filled).
- Window and per-tool filters match `/v1/stats` aggregates.
Test cases:
- test_vocabulary_labels_exact.
- test_modeled_label_always_visible.
- test_provider_observed_hidden_when_absent.
- test_window_and_tool_filters.

### [ ] E9-07: Diagnostics view
Owner: D1 · Estimate: 2d · Depends: E9-01
Files: apps/desktop/src/diagnostics/, tests in apps/desktop/tests/diagnostics.spec.ts
Description: Doctor checks rendered with remediation hints; redacted-log tail viewer (reads redacted logs only); "export diagnostics bundle" writes a local file at the user's explicit request only — the user inspects and transmits it themselves (08 §15; no upload path exists, D6). Bundles contain redacted logs and traces only: no source, prompts, repo paths beyond the config's own, symbols, patches, or tool arguments. Update panel shows current/previous version from `update_state` (the updater itself is E12).
Acceptance criteria:
- Bundle export performs zero network I/O and writes only to the user-chosen local path.
- Bundle content scan finds no source-code or prompt canaries seeded during the test session.
- Doctor failures render remediation text from the CLI doctor's hints.
Test cases:
- test_doctor_checks_rendered_with_hints.
- test_log_viewer_redacted_only.
- test_bundle_export_local_no_network.
- test_bundle_contains_no_canary_content.

### [ ] E9-08: Desktop packaging
Owner: D1 · Estimate: 2.5d · Depends: E9-02, E9-03, E9-04, E9-05, E9-06, E9-07, E14-05
Files: apps/desktop/src-tauri/tauri.conf.json (bundle targets), .github/workflows/ci.yml (desktop job)
Description: Tauri bundle targets — macOS .app/.dmg, Windows .msi, Linux AppImage/.deb — built by CI as dev-signed/unsigned artifacts for testing; production signing, notarization, and updater wiring land in E12-03/E12-04/E12-06 (the desktop shares the updater trust logic). Bundles must pass the E14-05 hardening checks: devtools disabled in dist, restrictive CSP, minimal capability allowlist, no source maps.
Acceptance criteria:
- CI produces installable bundles on all three platforms; the app launches and reaches the local API on each.
- Dist bundles have devtools disabled and contain no source maps (artifact scan).
- Bundle size and contents reviewed against the artifact allowlist (precursor to E14-10).
Test cases:
- test_bundle_builds_per_platform.
- test_dist_devtools_disabled.
- test_csp_no_remote_origins.
- test_no_source_maps_in_bundle.

---

## E10 — Cloud control plane & local entitlements

Cloud per `03-interfaces.md` §8 and `02-schemas.md` §5; local entitlement side per `03-interfaces.md` §9, `02-schemas.md` §4 migration 0002, D43, ADR-0015. Owner hard constraints: the control-plane database never stores source code, prompts, file paths, repo names, symbols, patches, or raw tool args (D30); every cloud/licensing failure degrades to `local_only()` and never blocks the assistant. No telemetry: `event_batches` exists in schema but no slice or post-slice client calls `POST /v1/events` (D6). Epic estimate: ~8 wk.

### [ ] E10-01: Fastify scaffold, migrations, and cloud test harness
Owner: F1 · Estimate: 3d · Depends: E1-01
Files: apps/control-plane/, drizzle/, apps/control-plane/vitest.config.ts
Description: Build modules, migration runner, schema registry, ephemeral-Postgres tests.
Acceptance criteria:
- Health, request ids, redaction, migration lock, transaction helpers, JSONB guard.
Test cases:
- migrate/lock/rollback; unknown JSONB; redaction.

### [ ] E10-02: Better Auth setup
Owner: F1 · Estimate: 3d · Depends: E10-01
Files: apps/control-plane/src/auth.ts, apps/control-plane/drizzle/0002_better_auth.sql, tests in apps/control-plane/test/auth.test.ts
Description: Better Auth (D29) self-hosted with the organization plugin; session cookies for web surfaces; generated auth tables coexisting with the 02-schemas §5 `users` table; `GET /v1/me` returns user + orgs + plan. SSO plugin wiring is scaffolded for enterprise (config only; no identity data ever includes local source metadata, 08 §10).
Acceptance criteria:
- Signup/login issues a session cookie; `GET /v1/me` returns the user with org memberships and effective plan.
- Unauthenticated `/v1/me` → 401 in the standard error shape.
- Auth tables and `users` stay consistent (one row per identity; email unique).
Test cases:
- test_signup_login_session_cookie.
- test_me_returns_orgs_and_plan.
- test_unauthenticated_me_401.
- test_duplicate_email_rejected.

### [ ] E10-03: Device-code flow
Owner: F1 · Estimate: 3d · Depends: E10-02
Files: apps/control-plane/src/routes/device.ts, tests in apps/control-plane/test/device_flow.test.ts
Description: `POST /v1/device/authorize` → `{device_code, user_code, verify_url}`; `POST /v1/device/token` polls per 03-interfaces §8 with `authorization_pending`/`slow_down`/`expired_token` semantics; on browser approval (account-app page, E10-13) issues an RS256 device JWT (15-minute access) plus a rotating refresh token bound to the device keypair — refresh requests are signed with the device private key and verified against `devices.public_key`; the refresh token hash is stored in `device_sessions.refresh_token_hash` and rotated on every use.
Acceptance criteria:
- Full flow: authorize → user approves with `user_code` → token poll returns access + refresh; access token expires in 15 minutes.
- Refresh rotation: each refresh invalidates the previous token (replay of the old one → 401 and session revocation).
- A refresh request signed with the wrong device key → 401.
Test cases:
- test_device_flow_happy_path.
- test_poll_pending_then_slow_down_then_issued.
- test_refresh_rotation_invalidates_old_token.
- test_refresh_bound_to_device_keypair.
- test_access_token_expires_15min.

### [ ] E10-04: Devices CRUD and revocation
Owner: F1 · Estimate: 2d · Depends: E10-03
Files: apps/control-plane/src/routes/devices.ts, tests in apps/control-plane/test/devices.test.ts
Description: `POST /v1/devices` registers `{name, platform, public_key}` — name is user-chosen and never defaults to the hostname (02-schemas §5); `DELETE /v1/devices/:id` sets `revoked_at` and invalidates all of the device's `device_sessions`; a revoked device's refresh attempts → 401 and it cannot re-authorize without re-registration. Rate limits by account and device (08 §9); idempotency keys accepted on device operations.
Acceptance criteria:
- Registration stores only name/platform/public_key + timestamps; device list is scoped to the authenticated user.
- Revocation kills refresh immediately; access tokens die at their 15-minute expiry.
- Registration payload containing a hostname-like default name is accepted only if user-supplied (client contract documented; server never generates names).
Test cases:
- test_register_and_list_devices_scoped.
- test_revoke_invalidates_sessions.
- test_revoked_device_refresh_401.
- test_idempotent_device_registration.

### [ ] E10-05: Device-bound machine-lease issuance and three-generation rotation
Owner: F1+I1 · Estimate: 4.5d · Depends: E10-02, E10-04
Files: apps/control-plane/src/modules/entitlements/, apps/control-plane/src/plugins/paid_authorization.ts, schemas/cloud-api/entitlement-v1.json, docs/runbooks/entitlement-key-rotation.md
Description: Issue Ed25519 previous/current/next signed machine leases bound to account, device ID, and device public-key hash. A standard lease carries `lease_class = standard`, `issued_at`, `refresh_after = issued_at + 14d`, `expires_at = issued_at + 21d`, `lease_id`, plan/features, and schema version. An explicitly entitled enterprise organization may issue `enterprise_offline` leases for exactly 30, 60, or 90 days, bound to one registered org/device, with renewal seven days before expiry and a protocol maximum of 90 days. Renewal requires proof of possession of the registered device private key. A shared paid-authorization middleware independently derives every cloud capability from current server subscription, organization membership, seat, device, and revocation state; it never trusts client lease claims, integrity claims, UI state, or requested feature names.
Acceptance criteria:
- Strict allowlisted claims; stale supported clients survive two rotations; five-minute iat/nbf skew; refresh/expiry cannot be extended by wall-clock rollback.
- A challenge nonce is single-use, short-lived, audience-bound, and verified against the registered device public key before a lease is issued.
- The issuance endpoint accepts no prompts, source, repository metadata, paths, operation history, or usage statistics.
- Only an enterprise entitlement and authorized org administrator can select 30/60/90-day offline duration; standard accounts cannot request or tamper into that class, and revocation exposure is explicitly shown before issuance.
- Every route declared as a paid cloud capability is registered with `paid_authorization`; authorization is recomputed from server tables on every request before domain execution.
Test cases:
- test_machine_lease_claims_and_14_21_day_windows.
- test_enterprise_offline_30_60_90_day_allowlist_and_seven_day_renewal_window.
- test_standard_account_cannot_request_enterprise_or_custom_duration.
- test_device_challenge_proof_success_replay_expiry_and_wrong_key.
- test_previous_current_next_rotation_and_retired_kid.
- test_revoked_subscription_device_or_seat_refuses_renewal.
- test_clock_skew_and_malformed_or_extra_claim_rejection.
- test_renewal_request_privacy_allowlist_rejects_content_fields.
- test_every_paid_cloud_route_uses_server_authorization_middleware.
- test_forged_local_pro_lease_integrity_claim_or_feature_name_never_authorizes_cloud.
- test_subscription_seat_org_or_device_change_takes_effect_on_next_cloud_request.

### [ ] E10-06: Local — migration 0002 entitlement_cache
Owner: R2 · Estimate: 1d · Depends: E3-01
Files: rust/compresso-store/migrations/0002_entitlements.sql, rust/compresso-store/src/entitlements.rs, tests in rust/compresso-store/tests/migrations.rs
Description: Commit `0002_entitlements.sql` byte-for-byte per 02-schemas §4: `entitlement_cache` singleton table (`id CHECK (id = 1)`, `bundle_jws`, `key_id`, `lease_class`, `fetched_at`, `refresh_after`, `expires_at`, with lease-class and ordered-timestamp checks). Add `Store::entitlement_cache_get() -> Result<Option<EntitlementCacheRow>>` and `Store::entitlement_cache_put(&EntitlementCacheRow)` (singleton upsert). Applies transactionally on top of 0001.
Acceptance criteria:
- Fresh open applies migrations 1 and 2; `schema_migrations` has both rows; reopen applies nothing.
- Two puts leave exactly one row (latest wins); get round-trips every field.
- A second row insert attempt violates the `id = 1` check.
Test cases:
- test_migration_0002_applies_on_fresh_and_existing_db.
- test_cache_singleton_upsert_latest_wins.
- test_cache_roundtrip_fields.
- test_second_row_rejected_by_check.
- test_refresh_after_must_be_between_fetch_and_expiry.
- test_lease_class_rejects_unknown_or_locally_invented_value.

### [ ] E10-07: Local EntitlementVerifier and keychain credentials
Owner: R1+R2 · Estimate: 3d · Depends: E10-05, E10-06
Files: rust/compresso-agent/src/{entitlements,credentials}.rs, docs/adr/0015-cached-entitlements-offline-grace.md
Description: Verify previous/current/next signing keys and all device-bound machine-lease claims; generate and sign server challenges with the device private key held in the OS keychain; enforce bounded issuance skew and monotonic 14-day refresh/21-day expiry windows.
Acceptance criteria:
- Unknown kid may trigger one due/explicit refresh then local-only; device/public-key mismatch is rejected; clock rollback cannot extend refresh or expiry; local tools never block.
- Plan, features, lease class, refresh, and expiry are read only from the verified JWS; same-named config, environment, CLI, or denormalized-cache values are ignored and cannot extend access.
Test cases:
- test_previous_current_next_keys_and_two_rotations.
- test_device_id_and_public_key_hash_binding.
- test_challenge_signature_uses_keychain_key_without_export.
- test_day_14_renewal_due_and_day_21_hard_expiry.
- test_enterprise_signed_30_60_90_day_lease_and_90_day_maximum.
- test_local_config_env_cli_and_cache_cannot_override_signed_claims.
- test_clock_rollback_and_forward_jump_do_not_extend_lease.
- test_unknown_kid_invalid_signature_and_keychain_failure_local_only.
- test_secret_canaries_absent_from_files_logs_and_errors.

### [ ] E10-08: Local — EntitlementCache, background refresh, degradation matrix
Owner: R1 · Estimate: 2.5d · Depends: E10-07
Files: rust/compresso-agent/src/entitlements/cache.rs, tests in rust/compresso-agent/tests/entitlements_degradation.rs
Description: `EntitlementCache::current(now)` per 03-interfaces §9 **never errors**: missing row, invalid JWS, unknown kid, expired lease, or store failure returns `Entitlements::local_only()`. The scheduler consumes only verified signed `refresh_after`/`expires_at` claims: standard leases attempt renewal at day 14 and expire at day 21; enterprise offline leases renew seven days before their signed 30/60/90-day expiry. No automatic request occurs before the signed refresh time. After failure, retry is at most once per 24 hours with bounded jitter until signed expiry. A valid cached lease remains usable during that renewal window. At hard expiry paid local features downgrade after the active operation while compression, MCP, CLI, indexing, and free features continue. Explicit login/status/refresh may contact the server sooner.
Acceptance criteria:
- `current()` is total: the full failure matrix returns `local_only()`, never `Err`, never a panic, never a block.
- No automatic network call occurs before day 14; failed renewal retries are capped at one per 24 hours with bounded jitter; successful renewal atomically installs a new 21-day lease without restart.
- Renewal sends only the D65 privacy allowlist and device proof; it is licensing traffic, never telemetry.
- An end-to-end test runs MCP tool calls during a simulated cloud outage → all succeed.
Test cases:
- test_current_never_errors_failure_matrix: table-driven over the six failure modes → `local_only()`.
- test_no_automatic_remote_check_before_day_14.
- test_day_14_attempt_then_daily_retry_cap_until_day_21.
- test_enterprise_scheduler_uses_signed_expiry_minus_seven_days_without_standard_day_14_check.
- test_valid_cached_paid_features_continue_during_renewal_window.
- test_day_21_expiry_downgrades_after_active_operation.
- test_renewal_payload_contains_only_allowlisted_fields_and_proof.
- test_valid_refresh_upgrades_entitlements_live.
- test_mcp_serves_during_cloud_outage.
- test_revoked_device_degrades_silently_no_retry_storm.

### [ ] E10-09: Privacy — allowlist schemas and write-path validation middleware
Owner: F1 · Estimate: 3d · Depends: E10-01
Files: schemas/cloud-api/plans-features/v1.json, schemas/cloud-api/entitlements-value/v1.json, schemas/cloud-api/feature-flags/v1.json, schemas/cloud-api/policy-set/v1.json, schemas/cloud-api/event-batches-aggregates/v1.json, schemas/cloud-api/audit-detail/v1.json, apps/control-plane/src/plugins/jsonb_allowlist.ts, tests in apps/control-plane/test/jsonb_allowlist.test.ts
Description: Implement the 02-schemas §5 content-level privacy invariant: a versioned allowlist JSON Schema with `additionalProperties: false` for **every** JSONB column (`plans.features`, `entitlements.value`, `feature_flags.default_value`/`rules`, `policy_sets.policy`, `event_batches.aggregates`, `audit_logs.detail`, subscriptions metadata). A Fastify middleware validates every write path against the registered schema before any DB write; failing requests are rejected (400) and audited. `audit_logs.detail` is restricted to an enumerated key set — ids, enum values, counts; no free-form strings from clients. A column-name check alone is explicitly insufficient.
Acceptance criteria:
- Every route that writes a JSONB column passes through the middleware (route-registration assertion, not convention).
- Any unknown property in any JSONB payload → 400 with a pointer to the offending key; the rejection writes an `audit_logs` row.
- `audit_logs.detail` schema enumerates its keys; a free-form string key is rejected.
Test cases:
- test_every_jsonb_write_route_registered_with_middleware.
- test_additional_properties_rejected_with_pointer.
- test_audit_detail_enumerated_keys_only.
- test_rejection_is_audited.
- test_valid_payloads_pass_all_columns.

### [ ] E10-10: Privacy — adversarial property tests and CI schema-registry check
Owner: F1+PQ · Estimate: 2d · Depends: E10-09
Files: apps/control-plane/test/privacy_adversarial.test.ts, apps/control-plane/test/jsonb_registry.test.ts
Description: fast-check property tests generate adversarial payloads embedding file paths (`/Users/x/src/main.rs`, `C:\repo\secret.ts`), prompt fragments, symbol names (`SessionManager::start`), SQL statements, and patch hunks into every writable JSONB field, asserting rejection by the E10-09 middleware. A CI test introspects the drizzle schema, enumerates every JSONB column, and fails if any column lacks a registered allowlist schema — so adding a JSONB column without a schema breaks the build (02-schemas §5 enforcement stack items 2 and 3).
Acceptance criteria:
- Property tests run ≥1000 cases per field class in CI within budget; all adversarial payloads rejected.
- The registry test fails when a scratch JSONB column is added without a schema (verified once on a scratch branch, documented in the PR).
- Both tests are wired into the ts CI job as required checks.
Test cases:
- prop_file_paths_rejected_all_jsonb_fields.
- prop_prompt_fragments_rejected.
- prop_symbols_sql_patches_rejected.
- test_every_jsonb_column_has_registered_schema.
- test_scratch_column_breaks_registry_check (scratch-branch verification).

### [ ] E10-11: Organizations, policies, and feature flags
Owner: F1 · Estimate: 3d · Depends: E10-02, E10-09
Files: apps/control-plane/src/routes/orgs.ts, apps/control-plane/src/routes/flags.ts, tests in apps/control-plane/test/orgs_policies.test.ts
Description: Organizations and members with owner/admin/member roles (Better Auth organization plugin mapped to `organization_members`); `GET /v1/orgs/:id/policies` returns the policy_set JSON **signed** (same ed25519 machinery as entitlements) and `PUT /v1/orgs/:id/policies` is admin-only, validating the payload against the policy-set allowlist schema (mode floors, strategy allowlist, privacy floors per 02-schemas §5); `GET /v1/flags` returns feature flags for the authenticated device from `feature_flags`.
Acceptance criteria:
- PUT by a member → 403; by an admin → stored, audited, and served signed on the next GET.
- Policy payloads violating the allowlist schema → 400 (middleware path from E10-09).
- Flags evaluation applies `rules` deterministically per device.
Test cases:
- test_policy_put_admin_only.
- test_policy_validated_against_allowlist.
- test_policy_response_signature_verifies.
- test_flags_rules_deterministic_per_device.

### [ ] E10-12: Stripe subscriptions and webhook
Owner: F1 · Estimate: 3d · Depends: E10-11
Files: apps/control-plane/src/routes/stripe_webhook.ts, apps/control-plane/src/billing/, tests in apps/control-plane/test/billing.test.ts
Description: Stripe (D28) checkout for pro/team plans with seats; `POST /v1/webhooks/stripe` verifies the Stripe signature before any processing and is idempotent under event replay (idempotency keys on billing operations, 08 §9); subscription lifecycle transitions (trialing → active → past_due → canceled) update `subscriptions.status` and are reflected in `/v1/me` and in freshly issued entitlement bundles (E10-05).
Acceptance criteria:
- Webhook without a valid signature → 400, nothing processed; replayed event ids are no-ops.
- Each lifecycle transition updates status and seat counts correctly; downgrade to canceled yields degraded entitlement bundles, never local breakage.
- Billing writes are audited.
Test cases:
- test_webhook_signature_required.
- test_webhook_idempotent_replay.
- test_lifecycle_transitions_update_status_and_me.
- test_seats_sync_from_subscription.
- test_cancel_degrades_entitlements_not_errors.

### [ ] E10-13: Next.js public site and account app
Owner: F1 · Estimate: 4d · Depends: E10-03, E10-12
Files: apps/web/, tests in apps/web/tests/account.spec.ts
Description: `apps/web`: public marketing site plus the authenticated account app — signup, plan purchase (Stripe checkout), device list with revoke, entitlement status, and the device-code `verify_url` page where the user enters `user_code` to approve a CLI/desktop login (E10-03). Completes the Phase 6 exit criterion (full account and purchase flow). The site states plainly that local features work without an account, forever (ADR-0011).
Acceptance criteria:
- E2E: signup → purchase (test-mode Stripe) → device-code approval → device visible → revoke works.
- The verify page rejects expired/unknown user codes with clear messaging.
- Local-independence copy present and asserted (ADR-0011).
Test cases:
- test_signup_purchase_e2e.
- test_device_code_verify_page_approves_login.
- test_expired_user_code_rejected.
- test_device_revoke_from_account.
- test_local_independence_copy_present.

### [ ] E10-14: Admin console and immutable audit log
Owner: F1 · Estimate: 3d · Depends: E10-12
Files: apps/admin/, tests in apps/admin/tests/admin.spec.ts
Description: `apps/admin` Next.js console for users, orgs, subscriptions, releases (including yank via `releases.yanked_at`), and support cases. Every administrative action writes an `audit_logs` row; append-only is enforced at the DB role level (E10-01). Admin authentication requires the stronger factor (hardware-backed MFA per 08 §10); support permissions are separated from release administration (a support role cannot publish or yank releases).
Acceptance criteria:
- Every admin mutation produces exactly one audit row with actor, action, subject, and enumerated-key detail.
- Attempted UPDATE/DELETE on audit rows fails even through the admin app's DB role.
- Role separation enforced: support role sees cases but gets 403 on release routes.
Test cases:
- test_admin_actions_write_audit_rows.
- test_audit_rows_immutable_from_admin_role.
- test_release_yank_sets_yanked_at_and_audits.
- test_support_role_cannot_touch_releases.

---

## E11 — Provider proxy

Per build-doc §14 and ADR-0014 (passthrough guarantees). The proxy passively intermediates a host client's own model traffic; wire-format knowledge is shared with the E13 drivers (07 §1.1) but roles are opposite. Provider API keys are forwarded, never persisted, never logged (07/08 hard rule). Epic estimate: ~3 wk.

### [ ] E11-01: Proxy skeleton — localhost bind, limits, no key persistence
Owner: R1 · Estimate: 3d · Depends: E2-09, E3-03
Files: rust/compresso-proxy/src/lib.rs, rust/compresso-proxy/src/server.rs, tests in rust/compresso-proxy/tests/skeleton.rs
Description: `compresso-proxy` binds `127.0.0.1`/`::1` only on a configured port; enforces body-size, concurrency, and per-request timeout limits; forwards `Authorization`/`x-api-key` headers verbatim to the upstream provider over rustls TLS and NEVER persists or logs them — the redaction layer strips key material from logs, error values, and operation records before those values are constructed, not after.
Acceptance criteria:
- Listener verifiably loopback-only; upstream connections HTTPS only.
- A canary API key sent through the proxy appears in no log line, no error message, no store row, and no file under the data/state dirs.
- Limit violations return clean gateway-style errors without dropping other in-flight requests.
Test cases:
- test_bind_localhost_only.
- test_api_key_never_logged_or_stored: canary scan over logs + SQLite + state files.
- test_body_size_limit_rejected.
- test_upstream_timeout_maps_to_gateway_error.
- test_concurrency_limit_enforced.

### [ ] E11-02: Anthropic Messages adapter
Owner: R1 · Estimate: 3d · Depends: E11-01
Files: rust/compresso-proxy/src/adapters/anthropic.rs, tests in rust/compresso-proxy/tests/anthropic.rs, fixtures/provider-payloads/ additions
Description: Anthropic Messages requests are mapped into a `ContextGraph` — the top-level `system` field becomes a `system_instruction` node and tool definitions become `tool_definition` nodes, with `force_ingress_protection` applied per D48 (external payload: protection forced, `protection_forced` trace note) — compressed with `Engine::compress` in the configured mode, then re-serialized preserving unknown fields and headers byte-for-byte. Unsupported payload shapes pass through unchanged (D38); the engine's fail-open behavior applies end to end.
Acceptance criteria:
- System prompts and tool definitions are never altered (protected + safety-checked); user/tool content compresses per mode.
- Unknown fields (e.g. vendor extensions) round-trip byte-identically; malformed bodies pass through unchanged.
- Engine failure → original request forwarded, `fallback_used` recorded.
Test cases:
- test_system_field_protected_and_forced_d48.
- test_unknown_fields_byte_roundtrip.
- test_tool_results_compressed_in_safe_mode.
- test_malformed_body_passthrough_unchanged.
- test_engine_failure_forwards_original.

### [ ] E11-03: OpenAI Chat Completions and Responses adapters
Owner: R1 · Estimate: 3d · Depends: E11-01
Files: rust/compresso-proxy/src/adapters/openai_chat.rs, rust/compresso-proxy/src/adapters/openai_responses.rs, tests in rust/compresso-proxy/tests/openai.rs
Description: Both OpenAI APIs adapted: Chat Completions `messages`/`tools` arrays and Responses API `input` items map to graph nodes with `tool_definition`/`tool_request` kinds protected per D48; per-provider and per-model policy hooks select mode; multimodal parts (images, files) pass through without inspection; unknown models or shapes pass through unchanged.
Acceptance criteria:
- Tool schemas and system/developer messages byte-preserved; assistant/tool content compressed per mode.
- Responses API item ordering and ids preserved exactly.
- `fixtures/provider-payloads/openai-unknown-fields` passes through byte-identical (06-fixtures #15).
Test cases:
- test_chat_completions_tools_protected.
- test_responses_items_order_and_ids_preserved.
- test_multimodal_parts_uninspected_passthrough.
- test_unknown_fields_fixture_byte_identical.
- test_unknown_model_passthrough.

### [ ] E11-04: Streaming passthrough without buffering
Owner: R1 · Estimate: 3d · Depends: E11-02, E11-03
Files: rust/compresso-proxy/src/stream.rs, tests in rust/compresso-proxy/tests/streaming.rs
Description: SSE responses are proxied chunk-by-chunk with bounded per-chunk buffers and no whole-response buffering; stream behavior matches direct provider behavior in recorded-fixture tests (Phase 7 exit criterion); mid-stream provider errors and `[DONE]` sentinels propagate unchanged; client cancellation closes the upstream connection promptly.
Acceptance criteria:
- First chunk reaches the client before the upstream response completes (latency assertion).
- Memory stays bounded on a 50 MB synthetic stream (RSS delta cap asserted).
- Recorded-fixture comparison: byte-identical event sequences versus direct provider capture.
Test cases:
- test_first_chunk_before_upstream_complete.
- test_memory_bounded_large_stream.
- test_midstream_error_propagates_unchanged.
- test_client_cancel_closes_upstream.
- test_recorded_fixture_stream_parity.

### [ ] E11-05: Bypass modes and unknown-payload passthrough
Owner: R1 · Estimate: 2d · Depends: E11-02, E11-03
Files: rust/compresso-proxy/src/bypass.rs, tests in rust/compresso-proxy/tests/bypass.rs
Description: `COMPRESSO_BYPASS=1` (process-level) and the request-level header `x-compresso-bypass: 1` pass traffic through byte-identical with zero graph construction and near-zero overhead; unknown payload shapes — unrecognized content types, unparseable JSON, unknown API routes — always pass through byte-identical (D38). Bypassed requests are visible in stats as passthrough operations.
Acceptance criteria:
- Env and header bypass both produce byte-identical request/response round-trips (headers included, apart from hop-by-hop).
- Bypass adds <1 ms P95 overhead on the benchmark payload.
- Unknown routes are proxied verbatim, never rejected.
Test cases:
- test_env_bypass_byte_identical.
- test_header_bypass_per_request_only.
- test_bypass_overhead_under_1ms_p95.
- test_unknown_route_passthrough.
- test_unparseable_json_passthrough.

### [ ] E11-06: Proxy records, observed usage, and modeled savings
Owner: R1+R2 · Estimate: 3d · Depends: E11-02, E11-03, E3-03
Files: rust/compresso-proxy/src/operations.rs, rust/compresso-store/migrations/0003_model_runtime.sql
Description: Record observed input/cache/output and actual cost; counterfactual stays modeled.
Acceptance criteria:
- Versioned metadata, no content, no verified counterfactual, absent usage null.
Test cases:
- cache/no-cache; missing usage; prices; modeled; redaction.


---

## E12 — Release engineering & signing

Requirement source: 08 §13–§14; gates G1–G15 (08 §22) block stable publication. This epic folds the E14-tagged pipeline seeds that belong here: E14-06 (secret management) into E12-01, E14-07 (SBOM/provenance) into E12-02, and E14-13 (incident runbooks) into E12-09; those seed numbers are reserved and not reused. Epic estimate: ~5 wk.

### [ ] E12-01: Signed release manifest format and release pipeline
Owner: I1 · Estimate: 4d · Depends: E7-04, E14-02
Files: .github/workflows/release.yml, schemas/release-manifest/v1.json, scripts/release/sign_manifest.rs, docs/adr/0013 (update)
Description: Define the signed release manifest at `schemas/release-manifest/v1.json`: version, channel, per-platform artifact entries (url, sha256, blake3, size), minimum-compatible version, publish timestamp, signing `kid`; served via `GET /v1/releases/manifest?channel=&platform=` (03-interfaces §8) and from CDN. `release.yml` builds `dist`-profile binaries (E14-02) on ephemeral runners, signs the manifest with a KMS-held non-exportable key (E14-06: GitHub OIDC to cloud KMS, no long-lived CI cloud keys; signing permission separated from deployment permission; two-person approval via GitHub environment protection for the stable channel), uploads split debug symbols to private storage, and refuses to publish unless the E7 gates pass on the dist build (G1) — `compresso-eval` runs against the dist binaries inside the workflow.
Acceptance criteria:
- Manifest schema compiles and every published manifest validates and verifies against the release public key.
- Publishing is blocked when any gate fails on the dist build (verified once with a deliberately failing threshold on a scratch branch).
- Workflow lint: no static cloud credentials in secrets used by release jobs; stable channel requires two approvals; runners are ephemeral.
Test cases:
- test_manifest_schema_and_signature_verify.
- test_gate_failure_blocks_publish (scratch-branch verification).
- test_no_static_cloud_keys_in_release_workflow.
- test_stable_requires_two_person_approval (environment config assertion).
- test_dist_profile_used_for_release_builds.

### [ ] E12-02: SBOM, provenance, and artifact secret scanning
Owner: I1 · Estimate: 2.5d · Depends: E12-01
Files: .github/workflows/release.yml (sbom + scan steps), scripts/release/sbom.sh
Description: (Absorbs E14-07.) CycloneDX SBOM via `cargo cyclonedx` plus a pnpm SBOM for every artifact; SLSA-style provenance attestation generated and verifiable per artifact; secret scanning (gitleaks) extended beyond the repo to release artifacts, container layers, and CI logs (08 §12); malware/vulnerability scan step on artifacts. Feeds gates G4 and G5.
Acceptance criteria:
- Every published artifact has an SBOM and a provenance attestation that verifies against the workflow identity.
- A canary secret embedded in a scratch artifact is caught by the artifact scan and fails the pipeline.
- CI-log scanning runs on release workflows.
Test cases:
- test_sbom_generated_per_artifact.
- test_provenance_attestation_verifies.
- test_artifact_secret_scan_catches_canary (scratch-branch verification).
- test_ci_log_scan_step_present.

### [ ] E12-03: macOS Developer ID signing and notarization
Owner: I1 · Estimate: 3d · Depends: E12-01
Files: .github/workflows/release.yml (macos sign/notarize steps), scripts/release/notarize.sh
Description: Developer ID signing, notarization, and stapling for the CLI binary and the Tauri .app/.dmg (E9-08 bundles); verification on a clean macOS runner via `spctl --assess` and `stapler validate`; the pipeline refuses to promote unsigned or un-notarized macOS artifacts to the stable channel.
Acceptance criteria:
- `spctl --assess --type execute` accepts the signed binary on a clean runner; `stapler validate` passes for the dmg.
- Signing uses the KMS/HSM-held identity from E12-01; no signing material on runners after the job.
- Unsigned artifact promotion to stable is impossible (pipeline enforcement test).
Test cases:
- test_spctl_accepts_signed_cli.
- test_stapler_validate_dmg.
- test_unsigned_blocked_from_stable.
- test_no_signing_material_persisted_on_runner.

### [ ] E12-04: Windows Authenticode signing
Owner: I1 · Estimate: 2d · Depends: E12-01
Files: .github/workflows/release.yml (windows sign step), scripts/release/sign_windows.ps1
Description: Authenticode signing (KMS-backed certificate) for `compresso.exe` and the desktop `.msi`; `signtool verify /pa` on a clean windows-2022 runner; unsigned Windows artifacts cannot reach stable; SmartScreen reputation considerations documented in the release runbook.
Acceptance criteria:
- `signtool verify /pa` passes for both artifacts on a clean runner.
- Timestamping applied so signatures outlive certificate expiry.
- Stable-channel enforcement mirrors E12-03.
Test cases:
- test_signtool_verify_exe_and_msi.
- test_signature_timestamped.
- test_unsigned_windows_blocked_from_stable.

### [ ] E12-05: Linux checksums and signatures
Owner: I1 · Estimate: 1.5d · Depends: E12-01
Files: .github/workflows/release.yml (linux sign step), docs/security/release-keys.md
Description: Signed `SHA256SUMS` for Linux tarballs/AppImage/.deb: a detached ed25519 (minisign-compatible) signature published alongside every release, with the public-key fingerprint documented at a stable location; `install.sh` (E12-07) verifies the signature when the key is present and the checksum always.
Acceptance criteria:
- `SHA256SUMS.minisig` verifies against the documented public key for every release.
- A tampered tarball fails checksum verification; a tampered SHA256SUMS fails signature verification.
- Key fingerprint documented and stable across releases (rotation follows the E12-01 key procedures).
Test cases:
- test_sha256sums_signature_verifies.
- test_tampered_tarball_fails_checksum.
- test_tampered_sums_fails_signature.
- test_key_fingerprint_documented.

### [ ] E12-06: Updater with staging, atomic switch, and health-check rollback
Owner: I1 · Estimate: 4d · Depends: E12-01
Files: rust/compresso-updater/src/lib.rs, rust/compresso-updater/src/verify.rs, rust/compresso-updater/src/apply.rs, tests in rust/compresso-updater/tests/update_flow.rs
Description: `compresso-updater` fetches the signed manifest, verifies signature, digest, platform/arch match, compatibility constraints, and version monotonicity (rollback protection), downloads into a private staging directory, re-verifies the staged artifact, stops affected components safely, installs via atomic switch (rename/dir swap), runs store migrations, health-checks (`compresso doctor` + an MCP `initialize` probe), and rolls back automatically to the retained previous version on any failure, recording `update_state` (02-schemas §4). At least one known-good previous version is always retained. A tampered manifest being refused is a release-gate test (G3). E14-08 later upgrades the metadata to a TUF-style hierarchy; the verification seams here must accommodate it.
Acceptance criteria:
- Happy path: staged, verified twice, atomically switched, health-checked, `update_state` updated with previous version retained.
- Tampered manifest, tampered artifact, wrong platform, and downgrade attempts are all refused with distinct reasons.
- A failing post-update health check triggers automatic rollback to the previous binary, verified on macOS, Windows, Linux CI legs.
Test cases:
- test_update_happy_path_atomic_switch.
- test_tampered_manifest_refused.
- test_tampered_artifact_refused_after_staging.
- test_downgrade_refused_version_monotonicity.
- test_failed_health_check_rolls_back.
- test_previous_version_retained.

### [ ] E12-07: install.sh, install.ps1, and Homebrew
Owner: I1 · Estimate: 3d · Depends: E12-03, E12-04, E12-05
Files: installers/install.sh, installers/install.ps1, installers/homebrew/compresso.rb, tests in installers/tests/
Description: Production installers superseding the E4-05 slice installer: `install.sh` (macOS/Linux; signature verification per E12-05 plus checksum), `install.ps1` (Authenticode-verified artifact), and a Homebrew formula — all installing to `~/.local/bin/compresso` per D23, idempotently. All three conform to the 08 §3.4 bootstrap trust chain: small and auditable, fetch signed metadata, download immutable versioned artifacts, verify before install, install atomically, print next steps; no telemetry, no auto sign-in, no assistant-config mutation, no mutable second-stage code.
Acceptance criteria:
- Clean-machine install on each platform ends with `compresso doctor` green.
- Signature/checksum verification failures abort before any write; re-runs are idempotent.
- None of the installers touch assistant configs or `~/.config/compresso`.
Test cases:
- test_installsh_clean_machine_doctor_green.
- test_ps1_verifies_authenticode.
- test_brew_install_and_uninstall_roundtrip.
- test_verification_failure_aborts_prewrite.
- test_installers_idempotent_and_config_untouched.

### [ ] E12-08: Security testing matrix and external review
Owner: I1 · Estimate: 3d · Depends: E12-06, E12-07
Files: docs/security/testing-matrix.md, rust/*/tests/security_matrix additions
Description: Execute the build-doc §25.4 security testing matrix against dist builds: path traversal, symlink escape, malicious MCP arguments, config injection, and a release-compromise simulation (forged manifest/artifact rejected end-to-end); track findings to closure. Commission an external or independent review; no unresolved critical/high findings (gate G14; Phase 8 exit criterion).
Acceptance criteria:
- Every matrix case has an automated test or a documented manual execution with results.
- Release-compromise simulation demonstrates refusal at every verification layer (manifest, artifact, updater).
- External review report on file with all critical/high findings closed or formally accepted by the owner.
Test cases:
- test_security_matrix_all_cases_executed (checklist artifact in docs/security/).
- test_release_compromise_simulation_refused.
- external_review_no_unresolved_critical_high (release-checklist item).

### [ ] E12-09: Closed beta, runbooks, and launch decision
Owner: PQ+I1+PO · Estimate: 5d · Depends: E12-06, E12-08, E7-06
Files: rust/compresso-eval/src/beta.rs, schemas/beta-export/v1.json, docs/runbooks/
Description: Execute D62 paired local beta and launch drills.
Acceptance criteria:
- 200 pairs/20 people; lower 95% bound >-1pp; probes 100%; previewed allowlist export only.
Test cases:
- schema attack; refusal/incomplete; randomization; statistics; drills.


---

## E13 — Model & client expansion

Per `07-model-expansion.md` (adopted by D45; source `compresso-expand.md`). Prime directive: the profile-first rule — a compatible provider is added as a model profile over an existing protocol driver; native code requires an ADR (ADR-0020). Hard rules throughout: `compresso-mcp` keeps zero E13 dependencies; compression and indexing work with zero providers configured; a fresh install performs zero provider network I/O; provider keys live only in the OS keychain and are never persisted to disk; no provider names in `compresso-agent`/`compresso-router`/`compresso-core`/`compresso-atlas`/`compresso-store` (ADR-0019, CI-enforced). Epic estimate: ~12 wk.

### [ ] E13-01: compresso-model-protocol crate — canonical request and message types
Owner: R1 · Estimate: 2.5d · Depends: E1-01
Files: rust/compresso-model-protocol/Cargo.toml, rust/compresso-model-protocol/src/lib.rs, rust/compresso-model-protocol/src/request.rs, rust/compresso-model-protocol/src/message.rs, tests in same files
Description: New crate per 07 §1.1/§2: `ModelRequest` (`request_id: RequestId` generated locally, `model: ModelRef` — profile id + model id, never a bare provider string —, `messages`, `tools`, `response_format`, `reasoning: ReasoningPolicy` (Disabled/Automatic/Enabled{effort}), `sampling`, `budget: GenerationBudget` (max_input_tokens, max_output_tokens, deadline_ms, maximum_cost_micros), content-free `RequestMetadata`, optional `provider_state`); `Message { role, parts }` with `MessageRole` (System, Developer, User, Assistant, Tool) and `MessagePart` (Text, Image, File, ToolCall, ToolResult, ProviderState). All serde round-trippable; anything crossing a process boundary carries `schema_version`. Crate depends on `compresso-core` tokens only, no network crates (07 dependency rules).
Acceptance criteria:
- All types round-trip via serde; `ModelRef` is unconstructable from a bare provider string (type-enforced constructor requiring profile id + model id).
- `RequestMetadata` fields are enumerated (session id, task kind, policy tags) — no free-form content field exists.
- Cargo.toml assertion: only `compresso-core` internal dep; no reqwest/hyper/ureq.
Test cases:
- test_model_request_serde_roundtrip.
- test_model_ref_requires_profile_and_model_id.
- test_message_parts_all_variants_roundtrip.
- test_metadata_is_content_free_enumerated.
- test_no_network_or_extra_internal_deps.

### [ ] E13-02: ModelEvent stream, FinishReason, TokenUsage, ModelError taxonomy
Owner: R1 · Estimate: 2.5d · Depends: E13-01
Files: rust/compresso-model-protocol/src/event.rs, rust/compresso-model-protocol/src/usage.rs, rust/compresso-model-protocol/src/error.rs, tests in same files
Description: `ModelEvent` per 07 §2.3 (ResponseStarted, TextDelta, ReasoningDelta, ToolCallStarted, ToolArgumentsDelta, ToolCallCompleted, ProviderStateUpdated, UsageUpdated, ResponseCompleted{reason}, ResponseFailed) and `ModelEventStream`; `FinishReason` with `Unknown(String)` retaining the raw provider value while mapping to safe behavior; `TokenUsage` with `UsageSource` (ProviderReported authoritative for cost, CompressoEstimated for preflight only, Reconciled) preserving the observed/applied/modeled separation; `ModelError { kind, user_message, diagnostic_code, remote_request_id, retryable, redacted_detail }` with `ModelErrorKind` (Authentication..Unknown per 07 §6) and `Retryability` (No | SameProvider{after} | FallbackSafe). Redaction is structural: `redacted_detail` is built from an allowlist of fields (status code, error type, request id) — never from raw response bodies; authentication material is stripped before any error value is constructed.
Acceptance criteria:
- All event variants round-trip; `Unknown` finish reasons carry the raw string for diagnostics and classify as non-retryable-safe by default.
- `TokenUsage` sources are mutually exclusive and serialized explicitly.
- Constructing a `ModelError` from a response containing a canary key yields no field containing the canary.
Test cases:
- test_event_variants_roundtrip.
- test_unknown_finish_reason_preserved_and_safe.
- test_usage_source_separation_explicit.
- test_error_redaction_structural_canary_absent.
- test_retryability_classification_table.

### [ ] E13-03: ProviderAdapter trait, bounded ProviderState, FakeProvider
Owner: R1 · Estimate: 3d · Depends: E13-02
Files: rust/compresso-model-protocol/src/adapter.rs, rust/compresso-model-protocol/src/provider_state.rs, rust/compresso-model-protocol/src/fake.rs, tests in same files
Description: Object-safe `ProviderAdapter` per 07 §2.6 (`provider_id`, `capabilities(model)`, `validate_request`, `execute -> ModelEventStream`, `estimate_tokens`, `classify_error`); `ProviderState { provider, profile_version, turn_id, opaque, retention }` bounded (default max 256 KiB, profile-tunable downward) with `ProviderStateRetention` (CurrentRequest, CurrentSession, EncryptedResumeState — opt-in, keychain-encrypted). Type visibility restricts provider state to the selected adapter and the session coordinator; it is not a `ContextGraph` node kind and cannot enter the compression engine, Atlas, or store operation records. In-memory `FakeProvider` streams a complete tool loop (text deltas, tool call with fragmented arguments, usage, completion) with zero network — the Phase-1 exit vehicle for E13-21.
Acceptance criteria:
- `dyn ProviderAdapter` compiles (object safety) and the agent/router surface accepts only the trait object.
- Oversized opaque state (> bound) is rejected at construction; `ReasoningDelta` content and opaque state never appear in Debug/log output (size + blake3 hash only).
- `FakeProvider` drives a full streaming tool loop deterministically.
Test cases:
- test_adapter_object_safety.
- test_provider_state_size_bound_enforced.
- test_provider_state_debug_shows_hash_not_content.
- test_provider_state_not_a_graph_node_kind.
- test_fake_provider_complete_streaming_tool_loop.

### [ ] E13-04: Capability registry and CI provider-name gate
Owner: R1 · Estimate: 2d · Depends: E13-01
Files: rust/compresso-model-protocol/src/capability.rs, scripts/ci/no_provider_names.sh, .github/workflows/ci.yml (gate step), tests in capability.rs
Description: `ModelCapabilities` per 07 §2.7 (modalities, context/output limits, streaming, tool_calls, parallel_tool_calls, tool_choice set, structured_output, reasoning, prompt_caching, provider_state_replay, usage_reporting) with `Capability { evidence, disabled_by_policy }` and `CapabilityEvidence` (Unknown/Declared/Verified/Degraded/Unsupported). Capabilities attach to a model within a profile, never globally to a provider. `disabled_by_policy` is an orthogonal override preserving evidence; routing uses `verified` by default and `declared` only behind the explicit `allow_declared_capabilities` route setting. CI grep gate (ADR-0019): the build fails if any provider identifier (anthropic, openai, kimi, glm, minimax, deepseek) appears in `compresso-agent`, `compresso-router`, `compresso-core`, `compresso-atlas`, or `compresso-store` source.
Acceptance criteria:
- Evidence states round-trip; policy-disable flips availability without touching evidence.
- The grep gate passes on the current tree and fails when a provider name is seeded into compresso-router (scratch-branch verification, documented).
- Capability lookups are per (profile, model), never per provider.
Test cases:
- test_evidence_states_roundtrip.
- test_policy_disable_preserves_evidence.
- test_capability_keyed_by_profile_and_model.
- test_grep_gate_green_on_tree.
- test_grep_gate_catches_seeded_name (scratch-branch verification).

### [ ] E13-05: compresso-providers and OpenAiChatDriver
Owner: R2 · Estimate: 3d · Depends: E13-03
Files: rust/compresso-providers/src/, fixtures/providers/openai/
Description: Implement protocol behind sanctioned provider network boundary.
Acceptance criteria:
- Explicit config/keychain; host/TLS/redirect/private-IP/limits; validated turns; allowlisted errors.
Test cases:
- tool loops; SSE; cancel; retry; network attacks; unknown; secrets.

### [ ] E13-06: AnthropicMessagesDriver
Owner: R2 · Estimate: 3d · Depends: E13-03
Files: rust/compresso-providers/src/drivers/anthropic_messages.rs, tests + fixtures under fixtures/providers/anthropic/
Description: Driver encoding the Anthropic Messages quirks from 07 §7: system prompt as the top-level `system` field (not a message role), tool results as user-content blocks, prompt caching via cache-control breakpoint passthrough, event-stream mapping (content block deltas, tool_use blocks) into canonical `ModelEvent`s with usage normalization.
Acceptance criteria:
- Canonical System/Developer messages map deterministically per the profile's declared behavior (merge or reject — never silent discard; conformance-tested).
- Tool results serialize as user-content blocks and round-trip through the event mapping.
- cache-control markers pass through unaltered when the profile declares prompt_caching.
Test cases:
- test_system_top_level_field_mapping.
- test_tool_results_as_user_content_blocks.
- test_cache_control_passthrough_when_declared.
- test_stream_events_normalized_from_fixture.
- test_role_without_mapping_rejected_not_dropped.

### [ ] E13-07: Driver resilience layer
Owner: R2 · Estimate: 3d · Depends: E13-05, E13-06
Files: rust/compresso-providers/src/resilience.rs, tests in rust/compresso-providers/tests/resilience.rs
Description: Cross-driver resilience per 07 §6: request deadlines, connect and idle-stream timeouts, cancellation propagation through the event stream, bounded response buffers, exponential backoff with jitter honoring `retry-after`, per-provider concurrency limits, and structural secret redaction in every error path — authentication material stripped before any error value is constructed (conformance case 22).
Acceptance criteria:
- Timeouts classify into the correct `ModelErrorKind` with correct `Retryability`; retry-after values are honored, capped, and jittered.
- Cancellation mid-stream closes the connection and emits `ResponseCompleted { reason: Cancelled }`.
- A canary API key never appears in any error, log, or diagnostic emitted by any failure path (table-driven over failure modes).
Test cases:
- test_connect_timeout_classification.
- test_idle_stream_timeout_bounded.
- test_cancellation_propagates_and_closes.
- test_retry_after_honored_with_jitter_cap.
- test_concurrency_limit_per_provider.
- test_canary_key_absent_all_error_paths.

### [ ] E13-08: Profile schema and TOML loader
Owner: R1 · Estimate: 2.5d · Depends: E13-04
Files: schemas/model-profile/v1.json, rust/compresso-providers/src/profiles/loader.rs, fixtures/profiles/reference.toml, tests in loader.rs
Description: Commit `schemas/model-profile/v1.json` verbatim from 07 §2.8 (`additionalProperties: false` everywhere; `default_base_url` HTTPS-only; driver enum `openai-chat|anthropic-messages|local-openai`; per-model capabilities with evidence values, tool_schema block, compatibility flags, retry, pricing). TOML profiles are parsed to JSON and boundary-validated with the `jsonschema` crate (same pattern as E1-04); the reference instance from compresso-expand §9 is committed as a fixture. Profiles contain no secrets: user config references keys by opaque keychain identifier; `key_environment_variable` is the only key-related field and is pattern-constrained.
Acceptance criteria:
- Schema compiles as draft 2020-12; the reference profile validates; unknown fields anywhere are rejected (not ignored).
- `default_base_url` with `http://` fails validation; `local-openai` local endpoints are not expressible in a distributed profile (loader enforces the per-endpoint local-approval path).
- A profile containing a literal key-like string fails a lint check.
Test cases:
- test_schema_compiles_draft_2020_12.
- test_reference_profile_validates.
- test_unknown_field_rejected_additional_properties.
- test_http_base_url_rejected.
- test_local_endpoint_requires_local_approval_path.
- test_literal_secret_lint_fails.

### [ ] E13-09: Signed profile registry
Owner: I1 · Estimate: 3d · Depends: E13-08
Files: rust/compresso-providers/src/profiles/registry.rs, rust/compresso-providers/src/profiles/verify.rs, tests in rust/compresso-providers/tests/registry.rs
Description: Registry profiles are ed25519-signed and verified before activation (embedded current+next public keys with `kid` rotation, mirroring the 03-interfaces §9 entitlement pattern); modified or unsigned registry profiles are rejected. The signed profile's `approved_hosts` allowlist is enforced at connect time: redirects to unapproved hosts are blocked; DNS-rebinding and private-IP protections apply to custom endpoints; local gateways (`local-openai`) require explicit per-endpoint user approval in local config, never via a distributed profile. Locally authored dev profiles load only with `--unsigned-profile` and are labeled experimental on every surface (07 §2.8, §9). Profiles cannot define executable code or inject arbitrary secret-bearing headers.
Acceptance criteria:
- Signed profiles verify with either live kid; any byte modification fails verification and the profile is rejected.
- Connection attempts to a host outside `approved_hosts` (including via 3xx redirect) are blocked pre-connect.
- `--unsigned-profile` profiles carry an experimental label in CLI/API/UI surfaces.
Test cases:
- test_signed_profile_verifies_both_kids.
- test_modified_profile_rejected.
- test_unsigned_profile_rejected_without_flag.
- test_unsigned_flag_labels_experimental_everywhere.
- test_redirect_to_unapproved_host_blocked.
- test_private_ip_endpoint_requires_explicit_approval.

### [ ] E13-10: Provider API keys in the OS keychain
Owner: I1 · Estimate: 2d · Depends: E13-08
Files: rust/compresso-providers/src/keys.rs, tests in rust/compresso-providers/tests/keys.rs
Description: Provider keys are stored via the `keyring` crate (macOS Keychain, Windows Credential Manager, Linux Secret Service — the same crate as D43 device credentials) under opaque identifiers like `keychain:compresso/deepseek`; profiles and config reference keys only by identifier. Environment-variable auth is an explicit automation opt-in. Keys are never persisted to disk — not in profiles, project files, config.toml, logs, local stats, error values, support bundles, or shell history (07 §9). A dedicated regression test writes a canary key, exercises the full provider path (request, streaming, error, cancellation, diagnostics export), and scans every file the provider path can write.
Acceptance criteria:
- Key set/get/delete round-trips through the OS keychain; the resolved key exists only in memory during a request.
- Env-var auth requires the explicit opt-in flag in local config; without it, env keys are ignored.
- The canary-scan regression test passes across all provider code paths.
Test cases:
- test_key_roundtrip_via_keyring.
- test_opaque_identifier_resolution.
- test_env_var_auth_requires_optin.
- test_no_key_on_disk_canary_scan.
- test_key_absent_from_diagnostics_bundle.

### [ ] E13-11: Model discovery with experimental quarantine
Owner: D1 · Estimate: 2d · Depends: E13-08, E13-09
Files: rust/compresso-providers/src/discovery.rs, tests in rust/compresso-providers/tests/discovery.rs
Description: Provider model-listing endpoints are untrusted input: returned model IDs are matched against the signed profile's `models` and `aliases`; matched models adopt their profile capabilities; unmatched models load only as quarantined experimental entries with conservative limits (smallest declared context/output bounds, zero verified capabilities, experimental label). `deprecated_after` dates from the profile are surfaced as warnings.
Acceptance criteria:
- Known IDs and aliases resolve to profile models; capability evidence comes from the profile, never from the discovery response.
- Unknown IDs are quarantined with conservative limits and never routable by default.
- Deprecated models warn at selection time.
Test cases:
- test_known_ids_matched_to_profile.
- test_alias_resolution.
- test_unknown_model_quarantined_conservative.
- test_quarantined_not_routable_by_default.
- test_deprecated_model_warns.

### [ ] E13-12: Anthropic and OpenAI first-party profiles
Owner: R2 · Estimate: 2.5d · Depends: E13-07, E13-08, E13-16
Files: profiles/providers/anthropic.toml, profiles/providers/openai.toml, fixtures/providers/{anthropic,openai}/, tests in rust/compresso-providers/tests/first_party_profiles.rs
Description: First-party profiles over the two native drivers (07 §7 delivery order) wired through the recorded-fixture conformance suite: capabilities marked `verified` only for the cases the suite passed on the current profile and model versions; pricing blocks populated for preflight `estimated_cost_micros`; privacy blocks (`direct_byok_supported`, data policy URLs) filled.
Acceptance criteria:
- Both profiles validate against the schema and pass their conformance case sets; evidence values in the TOML match the recorded conformance results exactly (a test cross-checks).
- Pricing feeds `TokenUsage.estimated_cost_micros` in preflight estimates.
- No capability is marked `verified` without a passing recorded case (07 §8.3 rule).
Test cases:
- test_profiles_validate_and_load.
- test_evidence_matches_conformance_results.
- test_pricing_feeds_preflight_estimate.
- test_unverified_capability_not_marked_verified.

### [ ] E13-13: ToolSchemaCompiler
Owner: R1 · Estimate: 3.5d · Depends: E13-04, E13-05
Files: rust/compresso-model-protocol/src/tool_schema.rs, tests in rust/compresso-model-protocol/tests/tool_schema.rs
Description: `ToolSchemaCompiler::compile(tools, capability) -> Result<Vec<CompiledTool>, ToolSchemaError>` per 07 §3, applying the ordered deterministic downgrade rules: (1) stable collision-checked name mangling and word-boundary description truncation with elision markers — a collision is `ToolSchemaError`, never a silent rename; (2) `unsupported_keywords` removed only when removal widens what the provider accepts; (3) structural rewrites (union/tuple/bounds) to the nearest accepting superset with the constraint restated in the description; (4) `additionalProperties` policy per profile (`supported`|`must_be_false`|`stripped`); (5) strict-mode compilation; (6) single-call compilation when `parallel_tool_calls` is not verified. Rejection: `ToolSchemaError::Unrepresentable { tool, keyword, model }` **before any network request** when a required constraint can be neither expressed nor enforced at runtime — the router treats this as ineligibility. Compilation is pure and cacheable, keyed by `(profile_version, blake3(tool_set))`.
Acceptance criteria:
- Rules apply in the documented order and deterministically; identical inputs produce identical compiled output (cache-key test).
- Keyword removal never narrows acceptance; canonical runtime validation is unaffected by any downgrade.
- Unrepresentable tools reject at compile time with the exact error triple; nothing is best-effort transmitted.
Test cases:
- test_name_mangling_stable_collision_checked.
- test_description_truncation_word_boundary.
- test_keyword_removal_only_widens.
- test_union_rewrite_restates_constraint_in_description.
- test_additional_properties_three_policies.
- test_parallel_unverified_compiles_single_call.
- test_unrepresentable_rejects_pre_network.
- test_compile_pure_and_cache_keyed.

### [ ] E13-14: Runtime canonical argument validator
Owner: R1 · Estimate: 2d · Depends: E13-13
Files: rust/compresso-agent/src/tools/validator.rs, tests in rust/compresso-agent/tests/tool_validation.rs
Description: The tool executor validates every **completed** tool-argument object against the canonical schema before execution, regardless of what wire schema was sent (07 §3): invalid model arguments never reach a tool implementation; streamed fragments are accumulated (bounded, E13-05) and never validated individually — only assembled objects.
Acceptance criteria:
- Arguments valid against a widened wire schema but invalid against the canonical schema are blocked with a typed error returned to the model.
- Fragment accumulation cannot bypass validation (no partial execution path exists).
- Valid arguments pass with zero mutation.
Test cases:
- test_invalid_args_blocked_before_tool_execution.
- test_widened_wire_schema_still_enforced_canonically.
- test_fragments_never_validated_individually.
- test_valid_args_untouched.

### [ ] E13-15: Kimi and GLM profiles
Owner: R2+PQ · Estimate: 2.5d · Depends: E13-12, E13-13
Files: profiles/providers/kimi.toml, profiles/providers/glm.toml, fixtures/providers/{kimi,glm}/, tests in rust/compresso-providers/tests/expansion_profiles.rs
Description: Profiles over `OpenAiChatDriver` encoding the 07 §7 quirks: Kimi — modern `tool_calls` only (`legacy_function_call = false`), per-model reasoning/multimodal/context capabilities, official provider tools kept separate from Compresso-owned tools, regional endpoints and privacy metadata explicit; GLM — per-model `tool_choice` restrictions never generalized across the family, thinking/context-caching/structured-output/output limits per model, coding-plan endpoints as separate endpoint profiles when behavior or terms differ. Recorded conformance fixtures per provider; zero native code (ADR-0020 — the trigger conditions are recorded in profile review notes).
Acceptance criteria:
- Both profiles validate, load, and pass their conformance case sets over recorded fixtures.
- GLM `tool_choice` restrictions verified per model id, not family-wide; Kimi legacy function-call path is rejected by the driver under this profile.
- `git diff` shows no additions under a `native/` module for this ticket.
Test cases:
- test_kimi_profile_conformance_passes.
- test_kimi_legacy_function_call_rejected.
- test_glm_tool_choice_per_model_enforced.
- test_glm_endpoint_profiles_separate.
- test_no_native_code_added.

### [ ] E13-16: Conformance harness — 28 required cases
Owner: PQ · Estimate: 4d · Depends: E13-05, E13-06
Files: rust/compresso-providers/tests/conformance/mod.rs, rust/compresso-providers/tests/conformance/cases.rs, fixtures/providers/<provider>/README.md (sanitization policy)
Description: Harness running every driver+profile+model combination against the 28 required cases of 07 §8.1 (basic/streaming text, UTF-8 split across chunks, single/sequential/parallel tool calls, fragmented and invalid tool arguments, unknown tool, tool failure, unicode/large payloads, structured output, unsupported keyword handling, reasoning + tools, provider-state replay, context/output boundaries, cancellation, connect and mid-stream timeouts, rate-limit + retry-after, auth-failure redaction, 5xx recovery, usage accounting, fallback reconstruction, injection attempting unauthorized tool use, secret redaction outbound, circuit breaker). Fixture policy per 07 §8.2: recorded and sanitized (response metadata and normalized events only — never prompts, reasoning content, or credentials), committed under `fixtures/providers/<provider>/`; CI runs entirely against recorded fixtures with synthetic repositories; production keys never appear in PR workflows; structural assertions (event ordering, schema validity, invariants), not byte-exact matching; unstable profiles are quarantined rather than weakening global assertions.
Acceptance criteria:
- All 28 cases execute per target; a per-target matrix report is emitted and consumed by E13-18 promotion.
- Fixture sanitizer provably strips credentials and prompt/reasoning content (canary test on the recording pipeline).
- Quarantining a profile removes it from the global matrix without weakening any assertion.
Test cases:
- test_all_28_cases_execute_per_target.
- test_fixture_sanitizer_strips_canaries.
- test_structural_assertions_not_byte_exact.
- test_quarantine_isolates_profile.
- test_ci_uses_recorded_fixtures_only_no_keys.

### [ ] E13-17: MiniMax and DeepSeek dual-protocol profiles
Owner: R2+PQ · Estimate: 3d · Depends: E13-15, E13-16
Files: profiles/minimax-openai.toml, profiles/minimax-anthropic.toml, profiles/deepseek-openai.toml, profiles/deepseek-anthropic.toml, fixtures/providers/{minimax,deepseek}/, tests in rust/compresso-providers/tests/dual_protocol.rs
Description: Two separate conformance-tested profiles per provider (openai-chat AND anthropic-messages) — the compatibility surfaces are never assumed identical (07 §7); the verified default protocol per client/model is the one passing the most reliable complete tool loop in conformance. DeepSeek: thinking-mode state replayed on subsequent tool turns via `provider_state_replay` + `requires_assistant_reasoning_replay` profile flags (the E13-03 ProviderState mechanism — no native adapter); reasoning state never enters project memory, Atlas, or the store; strict function schemas accepting only a JSON Schema subset expressed via `tool_schema.unsupported_keywords`; aliases and deprecation dates in the updateable registry.
Acceptance criteria:
- All four profiles pass conformance independently; default-protocol selection is recorded with the conformance evidence that justified it.
- DeepSeek reasoning replay works across a multi-turn tool loop via ProviderState; a scan proves reasoning content never persisted to Atlas/store/logs.
- DeepSeek schema-subset restrictions drive the E13-13 compiler (unsupported keywords removed or tool rejected pre-network).
Test cases:
- test_minimax_both_protocols_pass_independently.
- test_verified_default_protocol_selected_by_evidence.
- test_deepseek_reasoning_replay_multi_turn.
- test_reasoning_never_persisted_anywhere.
- test_deepseek_schema_subset_via_compiler.

### [ ] E13-18: Capability promotion pipeline
Owner: PQ · Estimate: 2d · Depends: E13-16
Files: rust/compresso-providers/src/profiles/promotion.rs, tests in rust/compresso-providers/tests/promotion.rs
Description: Evidence transitions per 07 §8.3: unknown → declared → verified/degraded/unsupported. Promotion to `verified` requires passing the relevant conformance cases on the **current** profile version and **current** remote model version; any profile or remote-model version change demotes affected capabilities back to `declared` until re-verified. Promotion runs record locally (local store) which cases passed, when, and against which versions — the data behind verified-capability badges and last-verification dates in CLI/UI. Marking an untested capability `verified` is rejected mechanically. No remote reporting exists (D6).
Acceptance criteria:
- Promotion requires a matching (profile_version, model_version) conformance record; stale records do not promote.
- Version bump demotes to `declared` automatically.
- Promotion records are queryable locally and never transmitted.
Test cases:
- test_promotion_requires_current_versions.
- test_version_change_demotes_to_declared.
- test_untested_verified_rejected_mechanically.
- test_promotion_records_local_only.

### [ ] E13-19: Router eligibility
Owner: R1 · Estimate: 2.5d · Depends: E13-04
Files: rust/compresso-router/Cargo.toml, rust/compresso-router/src/eligibility.rs, tests in same file
Description: `compresso-router` crate — a pure function of its inputs (policy, profiles, capability registry, health snapshot, and budget are passed in; the router performs no I/O and depends only on compresso-model-protocol). Implements the nine ordered conjunctive eligibility filters of 07 §5.1: (1) org/user provider allowlist, (2) project data-residency policy vs `privacy.regions`, (3) direct-BYOK vs proxied execution policy, (4) required modalities, (5) required capabilities `verified` (or `declared` when the route explicitly allows), (6) context/output budget fit, (7) maximum task cost preflight, (8) tool set compilable (E13-13 rejection = ineligible), (9) circuit breaker closed or half-open. The result records which filter excluded each rejected model.
Acceptance criteria:
- Filters evaluate in the documented order; the first failing filter is recorded per rejected model.
- `declared` capabilities pass filter 5 only with `allow_declared_capabilities = true` on the route.
- Determinism: identical inputs produce identical eligibility results; Cargo.toml has no I/O deps.
Test cases:
- test_filters_ordered_and_conjunctive.
- test_exclusion_reason_recorded_per_model.
- test_declared_requires_route_opt_in.
- test_unrepresentable_toolset_ineligible.
- test_pure_deterministic_no_io_deps.

### [ ] E13-20: Router scoring, fallback chains, circuit breakers
Owner: R1 · Estimate: 3d · Depends: E13-19, E13-07
Files: rust/compresso-router/src/scoring.rs, rust/compresso-router/src/fallback.rs, rust/compresso-router/src/health.rs, tests in rust/compresso-router/tests/routing.rs
Description: Scoring per 07 §5.2 over measured data only: `quality_weight × evaluation_score + reliability_weight × recent_success_rate + cache_weight × cache_affinity − cost_weight × estimated_cost − latency_weight × expected_latency − risk_weight × policy_risk`; weights from route policy with documented defaults; ties break deterministically by profile id; provider marketing benchmarks never enter the function. Fallback per 07 §5.4: chains are explicit configuration from the routing policy TOML (07 §5.3 `[routing.<task>]` with `primary`/`fallback`/`maximum_cost_usd`/`allow_declared_capabilities`) — no implicit try-everything; a fallback that would cross privacy, region, or billing boundaries is skipped with a recorded reason; only idempotent transport failures retry automatically; completed tool results are retained; provider-specific opaque state is never sent to a different provider — the fallback receives a normalized conversation rebuilt from canonical messages; the operation record always reports which model actually completed the task. Per-provider and per-model circuit breakers (closed → open on failure-rate threshold → half-open probe) with in-memory state and optional local-store snapshot — never reported remotely (D6).
Acceptance criteria:
- Scoring is deterministic with the documented tiebreak; changing a policy weight changes the decision reproducibly.
- Boundary-crossing fallbacks are skipped with reasons in the decision record; opaque state never crosses providers (type-level + test).
- Breaker transitions follow closed/open/half-open semantics and gate eligibility (filter 9).
Test cases:
- test_score_deterministic_with_profile_id_tiebreak.
- test_weights_from_route_policy_defaults_documented.
- test_fallback_chain_explicit_only.
- test_privacy_boundary_fallback_skipped_with_reason.
- test_fallback_conversation_rebuilt_canonical_no_opaque_state.
- test_circuit_breaker_transitions_and_eligibility.
- test_completing_model_reported.

### [ ] E13-21: Agent loop and migration 0003
Owner: R1+D1 · Estimate: 4d · Depends: E13-03, E13-14, E13-20
Files: rust/compresso-agent/src/agent_loop.rs, rust/compresso-store/migrations/0003_model_runtime.sql
Description: Integrate provider/tool loop and always apply reserved 0003.
Acceptance criteria:
- No content persisted; turns validate; no cross-provider opaque state; permissions/budgets/cancel.
Test cases:
- drivers; migration; invalid args; fallback; cancel; DB canaries.

### [ ] E13-22: Provider CLI surface
Owner: D1 · Estimate: 2.5d · Depends: E13-09, E13-10, E13-11, E13-20
Files: rust/compresso-cli/src/commands/provider.rs, rust/compresso-cli/src/commands/model.rs, rust/compresso-cli/src/commands/route.rs, tests in rust/compresso-cli/tests/cli_provider.rs
Description: `compresso provider add <profile-id>` (key prompted and stored in the keychain, host allowlist validated, optional live verification probe), `compresso model list` (models with capability evidence badges and last-verification dates from E13-18 records), `compresso model test <model>` (runs the conformance smoke subset against the live endpoint at the user's request), `compresso route set <task> ...` (edits the routing policy TOML) — all supporting `--json` and, for modifying commands, `--dry-run` per the global constraints.
Acceptance criteria:
- `provider add` writes the key to the OS keychain only (canary scan) and refuses hosts outside the signed allowlist.
- `model list --json` includes evidence and last-verified timestamps; quarantined models are labeled experimental.
- `route set --dry-run` prints the TOML diff and writes nothing.
Test cases:
- test_provider_add_key_in_keyring_only.
- test_provider_add_rejects_unapproved_host.
- test_model_list_shows_evidence_and_dates.
- test_model_test_runs_smoke_cases.
- test_route_set_dry_run_no_write.
- test_json_envelopes_all_commands.

---

## E13 future phases — protocol and ecosystem expansion

Tickets below implement D52 and `07-model-expansion.md` §11. Each uses the E13 conformance harness and must not add provider branches to the runtime.

### [ ] E13-23: OpenAI Responses driver
Owner: R2 · Estimate: 3d · Depends: E13-07, E13-16 · Files: rust/compresso-providers/src/openai_responses.rs, fixtures/providers/openai-responses/
Acceptance/tests: canonical text/tool/state/usage loop; fragmented streams, unknown items, cancellation, and redaction canary.

### [ ] E13-24: Local inference gateways
Owner: R2 · Estimate: 3d · Depends: E13-09, E13-16 · Files: rust/compresso-providers/src/local.rs, profiles/providers/local-openai.toml
Acceptance/tests: explicit loopback/private-network approval, no auto-discovery, unknown-model quarantine, offline tool loop.

### [ ] E13-25: Enterprise and multi-provider gateways
Owner: R2+I1 · Estimate: 2.5d · Depends: E13-09, E13-20 · Files: rust/compresso-router/src/gateway.rs, profiles/providers/gateway.schema.toml
Acceptance/tests: direct/gateway identities remain distinct; billing, residency, auth, and retention boundary fallback tests.

### [ ] E13-26: Canonical multimodal content
Owner: R1+R2 · Estimate: 4d · Depends: E13-02, E13-16 · Files: rust/compresso-model-protocol/src/media.rs, fixtures/providers/multimodal/
Acceptance/tests: bounded digest-addressed image/file/audio/video parts; MIME spoof, oversize, unsupported-model, and no-log tests.

### [ ] E13-27: Realtime protocol
Owner: R2 · Estimate: 4d · Depends: E13-02, E13-07 · Files: rust/compresso-model-protocol/src/realtime.rs, rust/compresso-providers/src/realtime.rs
Acceptance/tests: deterministic interruption/reconnect/tool state; out-of-order, replay, buffer, timeout, and approval tests.

### [ ] E13-28: Embedding and reranking protocol
Owner: R1+R2 · Estimate: 3d · Depends: E13-01, E13-09 · Files: rust/compresso-model-protocol/src/retrieval.rs, rust/compresso-providers/src/embeddings.rs
Acceptance/tests: dimensions, batching, privacy policy, local/direct routing, usage, and redaction validated.

### [ ] E13-29: Asynchronous batch execution
Owner: R2 · Estimate: 2.5d · Depends: E13-07, E13-18 · Files: rust/compresso-agent/src/batch.rs, rust/compresso-providers/src/batch.rs
Acceptance/tests: idempotent submit/poll/cancel/resume; encrypted local inputs, partial-failure reconciliation, cost cap.

### [ ] E13-30: Evaluation-driven route calibration
Owner: PQ+R1 · Estimate: 3.5d · Depends: E13-18, E13-20, E7-06 · Files: rust/compresso-eval/src/model_eval.rs, rust/compresso-router/src/calibration.rs
Acceptance/tests: held-out quality gate, set separation, policy invariants, deterministic calibration, rollback artifact.

### [ ] E13-31: Provider profile authoring SDK
Owner: R1+PQ · Estimate: 2.5d · Depends: E13-09, E13-16 · Files: packages/provider-sdk/, schemas/provider-conformance/v1.json
Acceptance/tests: sample provider added without core change; malformed profile and dev/production signature separation tested.

### [ ] E13-32: Self-hosted registry operations
Owner: I1+F1 · Estimate: 3d · Depends: E13-09, E10-05, E12-01 · Files: apps/control-plane/src/modules/model-registry/, docs/runbooks/profile-rollback.md
Acceptance/tests: delegated threshold signing, revoke, staged rollout, rollback, stale metadata, and air-gap import.

---

## E14 — IP protection & secure deployment

Per `08-ip-and-secure-deployment.md` (adopted by D46; source `ip.md`). Seeds E14-06 and E14-07 land inside E12-01/E12-02 and E14-13 inside E12-09; those numbers are reserved and not reused here. Honesty constraint from 08: local binaries can always be disassembled — strong controls live in release authenticity, cloud-side authorization, trade-secret handling, and architectural separation, never client-side obscurity. Every anti-tamper control is fail-open (ADR-0008): no protection mechanism may ever break the user's coding assistant. Epic estimate: ~5.5 wk.

### [ ] E14-01: IP inventory and classification document
Owner: I1+PQ · Estimate: 1.5d · Depends: E1-01
Files: docs/security/ip-inventory.md
Description: Maintain the asset inventory per 08 §1: one row per asset with owner, classification (`crown-jewel` | `shipped-proprietary` | `auditable-public` | `brand`), storage location, protection method, and disclosure status. First pass seeded from 08 §1.1–§1.4: crown jewels (strategy registry ordering and estimate weights, `pack_budget` scoring, `queries.rs` ranking heuristics, model-routing policies, eval tuning internals, strategy performance corpora, org knowledge ranking, entitlement issuance policy); auditable-public boundary that is never obfuscated (`schemas/`, MCP contracts, `installers/`, `compresso-integrations`, updater verification logic, `EntitlementVerifier` contract, privacy docs). Includes the 08 §3.3 review rule: any PR moving algorithmic logic into `compresso-integrations`, `installers/`, or desktop webview code is rejected.
Acceptance criteria:
- Every asset named in 08 §1.1–§1.4 appears with all five columns filled; classifications match the 08 tables.
- The document names a review cadence and the owning role per classification.
- The 08 §3.3 layering rule is stated as a PR-review checklist item and linked from conventions.md.
Test cases:
- inventory_covers_all_08_assets (review checklist against 08 §1 tables).
- classification_columns_complete.
- layering_rule_linked_from_conventions.

### [ ] E14-02: dist Cargo profile, path remapping, private symbol archive
Owner: I1 · Estimate: 2d · Depends: E1-02
Files: Cargo.toml ([profile.dist]), .github/workflows/release.yml (symbol-archive step), tests in scripts/ci/check_dist_profile.sh
Description: Add `[profile.dist]` verbatim per 08 §4: inherits release, opt-level 3, lto fat, codegen-units 1, `debug = 2` with `split-debuginfo = "packed"` and `strip = "symbols"`, incremental false, and `panic = "unwind"` — a deliberate deviation from ip.md §7: fail-open (ADR-0008, D38) requires that a panicking strategy degrades to passthrough inside `Engine::compress` via `catch_unwind` and that the MCP server survives a bad request; `abort` would kill the server mid-session. Release builds set `RUSTFLAGS="--remap-path-prefix=$CARGO_WORKSPACE_DIR=/compresso --remap-path-prefix=$HOME=/~"`. Split debug artifacts (dSYM/.dwp/.pdb) upload to private symbol storage and are never attached to public releases. CI and dev builds keep the default `release` profile so gate benchmarks stay comparable.
Acceptance criteria:
- Cargo.toml dist profile matches 08 §4 exactly; only release.yml builds with `--profile dist`.
- A strings scan of a dist binary finds no `$HOME`-prefixed developer path, username, or repository URL.
- Shipped binaries are symbol-stripped; the matching symbol archive exists in private storage per release.
Test cases:
- test_dist_profile_matches_08_spec.
- test_no_home_paths_or_usernames_in_binary.
- test_symbols_stripped_and_archived_privately.
- test_panic_unwind_not_abort.
- test_ci_builds_keep_release_profile.

### [ ] E14-03: Panic hook and release assertion policy
Owner: R1 · Estimate: 1.5d · Depends: E14-02
Files: rust/compresso-cli/src/panic_hook.rs (shared via a small crate-internal module per binary), clippy.toml / lint config, docs/architecture/conventions.md (assertion policy)
Description: Production panic hook in each binary logging exactly `internal error <version> <build-id> at <remapped location>` to the redacted local log — never node content, paths under user repositories, or SQL. Descriptive internal assertions demoted to `debug_assert!`; `assert!` messages in dist builds must not describe algorithm internals or include user data (clippy lint plus review-checklist entry). Build identifiers derive from version + content hash — no repository URLs, commit author names, or CI credentials embedded (08 §4). There is no remote crash reporting (D6): symbolication happens locally against the private archive.
Acceptance criteria:
- Forced panic in a dist build produces the redacted one-line log entry and a clean process exit path (MCP server survives via catch_unwind where applicable).
- A canary in node content or a user path never appears in panic output (test seeds both and panics).
- Lint fails on an `assert!` whose message contains a banned descriptive pattern.
Test cases:
- test_panic_hook_one_line_redacted.
- test_panic_output_no_user_content_or_paths.
- test_lint_catches_descriptive_assert_messages.
- test_build_id_content_derived_no_repo_urls.

### [ ] E14-04: Local IPC security test suite
Owner: I1+R2 · Estimate: 2.5d · Depends: E8-03, E8-07
Files: rust/compresso-agent/tests/ipc_security.rs
Description: The full 08 §7 control set as tested requirements on top of the E8-03 baseline (gate G7): socket and token files owned by the current user with user-only permissions (0600 file / 0700 dir); loopback-only binding; 401 without detail on bad auth; browser-Origin and Host-confusion rejection with zero CORS allowances; `schema_version` negotiation failing open; request-size, concurrency, and per-method timeout limits with explicit cancellation semantics; method-level authorization (config writes and integration installs only via the E8-07 two-step plan/approve flow); approved-root path authorization reusing the E3-11 canonicalization boundary for every path arriving via API or MCP; peer credential checks (SO_PEERCRED / getpeereid / named-pipe SIDs) where the platform provides them; read endpoints never returning raw content — structured, redacted records only.
Acceptance criteria:
- Every 08 §7 bullet has at least one named test; the suite runs on macOS, Linux, and Windows CI legs (platform-conditional where necessary).
- A world-readable token file or socket fails startup with a remediation message.
- The suite is wired into `gates` as G7 (E14-16).
Test cases:
- test_token_socket_permissions_enforced.
- test_world_readable_token_refused_at_startup.
- test_peer_credentials_checked_where_available.
- test_no_cors_headers_on_any_response.
- test_writes_require_two_step_plan_approve.
- test_api_paths_canonicalized_against_approved_roots.
- test_reads_never_return_raw_content.

### [ ] E14-05: Tauri hardening
Owner: D1 · Estimate: 2d · Depends: E9-01
Files: apps/desktop/src-tauri/tauri.conf.json, apps/desktop/src-tauri/capabilities/, tests in apps/desktop/src-tauri/tests/hardening.rs
Dependency rationale: E14-05 is post-shell dist hardening and verification, not the source of baseline shell safety. It validates devtools-off production behavior, restrictive CSP/capability configuration, webview isolation, and artifact properties after E9-01 exists; release enforcement consumes these results later.
Description: Per 08 §6: devtools disabled in dist builds; restrictive CSP with no remote origins (the app talks only to the local API); Tauri capability allowlist exposing only the explicit commands the UI needs — no generic fs/shell/http primitives reachable from the webview; every frontend→Rust command validates its input; privileged behavior stays in Rust commands with capability checks; the local-API bearer token is read by the Rust side and attached in Rust — never handed to the webview, never in localStorage; no private source maps in public artifacts; desktop updates are signed and verified before install (shared updater trust logic, E12-06).
Acceptance criteria:
- Dist bundle refuses devtools; CSP blocks a test fetch to a remote origin from the webview.
- The capability manifest enumerates every command; an unlisted command invocation fails.
- Token-isolation and source-map checks from E9-01/E9-08 are promoted into this suite as release requirements.
Test cases:
- test_devtools_disabled_in_dist.
- test_csp_blocks_remote_fetch.
- test_capability_allowlist_exhaustive.
- test_unlisted_command_rejected.
- test_token_unreachable_from_webview.
- test_no_source_maps_in_public_bundle.

### [ ] E14-08: TUF-style update metadata
Owner: I1 · Estimate: 3d · Depends: E12-06
Files: rust/compresso-updater/src/tuf.rs, schemas/release-manifest/v2.json (metadata hierarchy), tests in rust/compresso-updater/tests/tuf.rs
Description: Upgrade the E12-06 updater metadata to a TUF-style hierarchy (root → timestamp → snapshot → targets) per 08 §14 so a single leaked CDN or metadata key cannot forge accepted updates. The updater verifies trusted root keys, signature thresholds, metadata expiration, version monotonicity, artifact digest, platform/arch match, and compatibility constraints. Trust-root rotation is exercised as part of the signing-compromise runbook drill (E12-09).
Acceptance criteria:
- An update signed by only one key of a two-of-three threshold is refused; expired timestamp/snapshot metadata is refused.
- Root rotation via a properly threshold-signed new root is accepted; an attacker-signed root is refused.
- All E12-06 protections (rollback, digest, platform) remain intact under the new hierarchy.
Test cases:
- test_threshold_single_key_insufficient.
- test_expired_metadata_refused.
- test_root_rotation_threshold_signed_accepted.
- test_forged_root_refused.
- test_rollback_protection_retained.

### [ ] E14-09: Signed strategy-pack format and fail-open verifier
Owner: R1+I1 · Estimate: 2.5d · Depends: E12-01, E2-09
Files: schemas/strategy-pack-manifest/v1.json, rust/compresso-core/src/strategy/pack_loader.rs, tests in rust/compresso-core/tests/pack_loader.rs
Description: Post-slice strategy-pack distribution per 08 §8: `strategy-pack-v<N>.bin` + `.manifest` + `.sig`, the manifest carrying pack version, min/max compatible client versions, release channel, issued/expires timestamps, blake3 artifact digest, signing key id, and rollback policy. The native client verifies the ed25519 signature (embedded public keys, current+next `kid` rotation — the ADR-0015 pattern) before loading; an invalid, expired, or version-incompatible pack is ignored and the compiled-in defaults are used — fail open, never fail closed. Stated limitation honored in design review: packs are a tamper-resistance and operational-update mechanism, not a secrecy mechanism — anything that must stay secret stays server-side (08 §3.1); this bounds what E13 model profiles may contain when distributed as packs.
Acceptance criteria:
- A valid signed pack loads and replaces tuned orderings/weights; every invalid case (bad signature, expired, wrong version range, corrupt digest) silently falls back to compiled defaults with a logged reason.
- Pack loading failure can never block or delay `Engine::compress` (load happens off the compression path).
- Manifest schema committed and validated at load.
Test cases:
- test_valid_pack_loads_and_applies.
- test_bad_signature_ignored_defaults_used.
- test_expired_pack_ignored.
- test_version_incompatible_ignored.
- test_corrupt_digest_ignored.
- test_pack_failure_never_blocks_compression.

### [ ] E14-10: Artifact allowlist and signed runtime-integrity manifest
Owner: I1+R1 · Estimate: 3d · Depends: E14-02, E12-01, E12-06
Files: scripts/release/artifact_allowlist.sh, schemas/runtime-integrity-manifest/v1.json, rust/compresso-agent/src/integrity.rs, .github/workflows/release.yml (gate step)
Description: Release gate G13 asserts every published artifact contains only allowlisted contents: symbols stripped, no fixtures, prompts/design documents, source maps, debug info, `.git` metadata, or developer paths; private symbols remain separate. The release pipeline also emits a signed manifest covering executable and paid resource hashes, version, platform/arch, signing identity, and build ID. At startup and before loading paid resources, the native agent verifies the OS code signature where supported plus the manifest signature/hash set. Failure is visible tamper evidence: finish the active operation, disable paid local features, and continue free local operation. Verification code, manifests, signatures, and integrity status are explicitly patchable on a user-controlled machine and therefore never authorize cloud access; D66 and server middleware remain the security boundary.
Acceptance criteria:
- The gate passes on a clean release build for all platforms.
- Seeding a fixture file, a source map, or an unstripped binary into an artifact fails the gate (scratch-branch verification, documented).
- The allowlist itself is committed and reviewed — additions require a PR.
- Official artifacts carry a release-signed integrity manifest whose complete file set and hashes verify before paid local resources load; unknown extra executable/resource files fail verification.
- Signature/hash failure never crashes, deletes data, blocks MCP/free features, phones home, or claims authoritative revocation; it records a content-free local status and downgrades paid local features after the active operation.
Test cases:
- test_allowlist_passes_clean_release.
- test_seeded_fixture_fails_gate.
- test_source_map_detected_and_failed.
- test_unstripped_symbols_detected.
- test_signed_integrity_manifest_covers_executable_and_paid_resources.
- test_modified_deleted_or_extra_paid_resource_detected.
- test_wrong_signing_identity_manifest_signature_or_build_id_detected.
- test_integrity_failure_finishes_active_operation_then_local_only.
- test_integrity_failure_never_blocks_free_mcp_or_emits_network_request.
- test_forged_client_integrity_ok_claim_never_authorizes_cloud.

### [ ] E14-11: License-compliance program
Owner: I1 · Estimate: 2d · Depends: E1-03
Files: deny.toml (extended), scripts/ci/pnpm_license_check.mjs, scripts/release/third_party_notices.sh, .github/workflows/ci.yml
Description: Extend the E1-03 gate into the full 08 §18 program: machine-generated dependency inventory (Rust + TS) reviewed each release; MIT/BSD/Apache-2.0 allowed with notice compliance and Apache-2.0 patent terms preserved; LGPL requires linking/modification review; GPL requires distribution-model review; AGPL requires legal review before any use — CI fails when an unapproved license enters a release dependency graph via either `cargo deny` or the pnpm license check. A generated `THIRD-PARTY-NOTICES` file ships in every artifact (gate G12). Clean-room rule (D2) restated as a review requirement: no code, prompts, or assets copied from Tetris or WOZCODE.
Acceptance criteria:
- pnpm and cargo license gates both green on the tree; an injected AGPL dev-dependency in a scratch branch fails CI.
- THIRD-PARTY-NOTICES generated per artifact and verified present by the E14-10 allowlist.
- The clean-room rule appears in conventions.md and the PR template.
Test cases:
- test_pnpm_license_gate_fails_on_agpl (scratch-branch verification).
- test_cargo_deny_licenses_green.
- test_third_party_notices_complete_per_artifact.
- test_notices_present_in_artifact_allowlist.

### [ ] E14-12: Repository and contributor controls
Owner: I1+owner · Estimate: 1.5d · Depends: E14-01
Files: docs/security/trade-secret-handling.md, .github/CODEOWNERS (prepared), docs/security/access-review.md
Description: Controls that activate when a remote repository and contributors exist (D32), per 08 §17: crown-jewel crates/paths (from the E14-01 inventory) restricted via CODEOWNERS + branch protections (or a split private engine repo if needed) to engineers who need them; hardware-backed MFA required org-wide; owners/admins reviewed quarterly; dormant accounts disabled promptly; access to sensitive repos and release systems logged; signed IP assignment/confidentiality before first commit; secure offboarding with credential rotation; production datasets and strategy performance corpora never on developer laptops by default. Delivers the written trade-secret handling policy — restricted access, classification labels, NDAs, and consistent handling are the operational evidence trade-secret status requires.
Acceptance criteria:
- The prepared CODEOWNERS maps exactly to the E14-01 crown-jewel paths.
- The handling policy covers classification labels, access, NDAs, offboarding, and dataset residency.
- An activation checklist exists for the moment a remote is created.
Test cases:
- codeowners_matches_ip_inventory_paths.
- handling_policy_sections_complete.
- activation_checklist_reviewed_by_owner.

### [ ] E14-14: Legal program checklist
Owner: PO+counsel · Estimate: 2d · Depends: E14-01
Files: docs/legal/program-checklist.md, docs/legal/eula-draft.md
Description: With qualified counsel per 08 §19 (this plan is not legal advice): confirm ownership of all source and design assets; employee/contractor IP assignments with prior-invention disclosure; trade-secret inventory + handling policy cross-referenced (E14-01/E14-12); copyright registration where useful; trademark search and filing for Compresso and primary marks — resolving D36, which is launch-blocking; patentability review of genuinely novel compression/routing/evaluation methods before public disclosure; proprietary EULA and enterprise terms prohibiting unauthorized redistribution and circumvention where lawful; an infringement reporting and response process.
Acceptance criteria:
- D36 resolved: trademark search completed and filing initiated before public launch; result recorded in decisions.md.
- EULA and enterprise terms drafted, reviewed by counsel, and published before the stable channel opens.
- Checklist signed by owner and counsel with dates.
Test cases:
- d36_resolution_recorded_in_decisions.
- eula_published_before_stable (release-checklist item).
- checklist_signed_owner_and_counsel.

### [ ] E14-15: Obfuscation evaluation spike (go/no-go)
Owner: I1+R1 · Estimate: 3d · Depends: E12-08, E7-04
Files: docs/adr/0022-selective-obfuscation-policy.md (update), scripts/release/obfuscate/ (spike artifacts)
Description: Last in the epic per ip.md §28.12 and ADR-0022 — permitted only after performance baselines and support processes exist. Evaluate candidate techniques in escalating order (symbol renaming beyond stripping, string-literal protection for tuning constants, constant splitting, selective control-flow transformation, commercial LLVM-based transformation) applied ONLY to the §1.1 crown-jewel modules (strategy registry ordering/estimate weights, `pack_budget` scoring, `queries.rs` ranking, proxy model-selection rules). Never obfuscated: everything in 08 §1.3 plus signature verification, cryptographic code, installer/updater trust logic, privacy controls and redaction, secret detection, uninstaller/restore, crash recovery, data deletion. Each adopted technique requires the full gate set to pass on the obfuscated dist build (G15: all G1–G15 re-run). Deliverable is a go/no-go decision recorded as an ADR-0022 update with the measured gate deltas.
Acceptance criteria:
- Gate suite (median reduction, P95 latency, recall, MCP startup) re-run on each candidate obfuscated build; results tabulated.
- Symbol/section diff proves only crown-jewel modules were transformed; the auditable boundary is byte-comparable.
- ADR-0022 updated with the decision and evidence; if no-go, the eligible-technique list is annotated with the blocking measurement.
Test cases:
- test_obfuscated_build_passes_all_gates (per adopted technique).
- test_only_crown_jewel_modules_transformed.
- test_auditable_boundary_unchanged.
- adr_0022_updated_with_go_no_go.

### [ ] E14-16: Merge the G1–G15 gate table into gates.toml and release enforcement
Owner: I1 · Estimate: 1.5d · Depends: E7-03, E7-06, E12-01
Files: docs/plan/gates.toml, .github/workflows/release.yml, docs/runbooks/release-checklist.md
Description: Implement the 08 §22 merged gate table: measurable engine gates (G1 — median reduction, P95 latency, protected mutations, malformed JSON, recall 100%/≥99%, all evaluated on the dist build; G2 — MCP startup < 500 ms) live in `gates.toml` with a `build_profile = "dist"` annotation; pipeline gates (G3 signed metadata/rollback, G4 secret scanning, G5 SBOM/provenance, G6 dependency findings, G10 privacy schema tests, G13 artifact allowlist) are enforced as `release.yml` required steps; the remaining gates (G7 IPC tests, G8 uninstall/restore, G9 no-telemetry code path, G11 cloud-outage independence, G12 notices, G14 security review, G15 obfuscated re-run) live in `docs/runbooks/release-checklist.md` with a named owner per gate. Stable publication is blocked unless all fifteen pass.
Acceptance criteria:
- gates.toml carries G1/G2 with dist annotation; release.yml fails on any pipeline-gate step failure.
- The release checklist lists G7–G9, G11–G12, G14–G15 with named owners and evidence links.
- A dry-run release with one gate deliberately failing is blocked end-to-end (scratch verification).
Test cases:
- test_gates_toml_g1_g2_dist_annotated.
- test_release_yml_enforces_pipeline_gates.
- test_checklist_owner_per_gate.
- test_failing_gate_blocks_publish (scratch-branch verification).

### [ ] E14-17: High-impact E15 threat model
Owner: I1+R1+R2 · Estimate: 2.5d · Depends: E15-01 · Files: docs/security/threat-model-e15.md, docs/security/data-flow-e15.md
Acceptance/tests: edit/SQL/recall/review/browser/terminal assets, boundaries, abuse cases, controls, owners, and release-blocking high findings traced to tickets.

### [ ] E14-18: Shared sandbox policy and OS primitives
Owner: I1+R2 · Estimate: 2.5d · Depends: E14-17
Files: rust/compresso-sandbox/src/, docs/security/sandbox-boundary.md
Description: Implement deny-default process/network/fs/resource isolation.
Acceptance criteria:
- No direct bypass; unsupported disables; typed roots/argv/env/destination/resources/cancel.
Test cases:
- escape; interpolation; exfiltration; fork/output; cancel; unsupported.


---

## E15 — Clean-room WOZ-inspired capabilities

Per `09-woz-inspired-capabilities.md`, D49-D50, and ADR-0023/0024. All fixtures and implementation artifacts are original. Epic estimate: 116.5 engineer-days.

### [ ] E15-01: Schemas, migration 0004, and permission types
Owner: R1+R2 · Estimate: 2.5d · Depends: E3-01, E8-02 · Files: schemas/{search,edit,sql,recall,knowledge,review}/v1.json, rust/compresso-store/migrations/0004_woz_capabilities.sql
Acceptance/tests: strict schemas, transactional migration, unforgeable scoped grants, stale/expired plans, and ciphertext-only DB scan.

### [ ] E15-02: Combined bounded search core
Owner: R2 · Estimate: 3d · Depends: E3-12, E15-01 · Files: rust/compresso-search/src/{search,regex}.rs, fixtures/search/
Acceptance/tests: glob/regex/path/count/snippet modes, deterministic bounds, traversal/symlink/secret rejection, regex-bomb timeout.

### [ ] E15-03: Structural search, importers, and versions
Owner: R2 · Estimate: 3d · Depends: E15-02 · Files: rust/compresso-search/src/{structure,importers}.rs
Acceptance/tests: exact ranges/confidence, Atlas importer graph, known-version no-content marker, stale/hash and token-cap tests.

### [ ] E15-04: Image, PDF, and notebook search adapters
Owner: R2+I1 · Estimate: 4d · Depends: E15-02, E14-18 · Files: rust/compresso-search/src/assets/, fixtures/search/assets/
Acceptance/tests: isolated bounded workers; MIME spoof, encrypted/bomb PDF, oversized image, malformed notebook.

### [ ] E15-05: Edit planner and exact operations
Owner: R1 · Estimate: 3d · Depends: E15-01, E14-17 · Files: rust/compresso-edit/src/{plan,operations}.rs
Acceptance/tests: no side effects, canonical plan hash, base hashes, bounded diff, ambiguity/path/hash mutation failures.

### [ ] E15-06: Atomic edit apply and rollback
Owner: R1 · Estimate: 4d · Depends: E15-05 · Files: rust/compresso-edit/src/{apply,rollback}.rs
Acceptance/tests: grant/base recheck, all-or-nothing staging, encrypted rollback, crash phases, concurrent edit, rollback divergence.

### [ ] E15-07: Validation registry
Owner: R1+R2 · Estimate: 3d · Depends: E15-05 · Files: rust/compresso-edit/src/validation/, fixtures/edit/validation/
Acceptance/tests: bounded tree-sitter/TS/JSON/YAML/HTML/SQL/notebook validation; failure, timeout, and approved-command isolation.

### [ ] E15-08: Notebook edit and explicit fuzzy matching
Owner: R1 · Estimate: 4d · Depends: E15-06, E15-07 · Files: rust/compresso-edit/src/{notebook,fuzzy}.rs
Acceptance/tests: cell/schema/metadata preservation, deterministic thresholds, ambiguity refusal, rollback.

### [ ] E15-09: SQL profiles and keychain credentials
Owner: R2+I1 · Estimate: 3d · Depends: E15-01, E13-10, E14-17 · Files: rust/compresso-sql/src/{profile,connect}.rs
Acceptance/tests: secret references only, read-only default, TLS/host policy, idle close, logout clear, disk/log canary.

### [ ] E15-10: PostgreSQL/MySQL/SQLite schema graph
Owner: R2 · Estimate: 5d · Depends: E15-09 · Files: rust/compresso-sql/src/{postgres,mysql,sqlite,schema}.rs, fixtures/sql/
Acceptance/tests: normalized objects/relationships across engines, restricted catalogs, cycles, caching, and output bounds.

### [ ] E15-11: SQL parser, linter, and classifier
Owner: R1 · Estimate: 3d · Depends: E15-01 · Files: rust/compresso-sql/src/{parse,classify,lint}.rs
Acceptance/tests: read/data/schema/admin/unknown classes; writable CTE, stacked statement, comment-smuggling, and dialect corpus.

### [ ] E15-12: Bounded read-only SQL execution
Owner: R2 · Estimate: 3d · Depends: E15-09, E15-11 · Files: rust/compresso-sql/src/query.rs
Acceptance/tests: read-only role/transaction, timeout/cancel, row/byte/cell/line bounds, formats, attempted-write and redaction tests.

### [ ] E15-13: SQL mutation plan/apply
Owner: R1+R2 · Estimate: 4d · Depends: E15-11, E15-12, E14-17 · Files: rust/compresso-sql/src/mutate.rs
Acceptance/tests: rollback preview, elevated grant, stale schema/plan, partial failure, audit; admin denied by default.

### [ ] E15-14: Recall ingestion and secret redaction
Owner: R2+I1 · Estimate: 4d · Depends: E15-01, E14-17 · Files: rust/compresso-recall/src/{sources,parse,redact}.rs, fixtures/recall/
Acceptance/tests: disabled default, per-source consent, typed parsing, malformed/duplicate handling, secret corpus, no plaintext storage.

### [ ] E15-15: Lexical and semantic recall index
Owner: R1+R2 · Estimate: 4d · Depends: E15-14, E13-28 · Files: rust/compresso-recall/src/{chunk,index,rank}.rs
Acceptance/tests: offline lexical mode, optional policy-approved embeddings, relevance/recency/provenance ranking, deterministic rebuild.

### [ ] E15-16: Recall query, export, delete, and rebuild
Owner: R2 · Estimate: 3d · Depends: E15-15 · Files: rust/compresso-recall/src/api.rs, rust/compresso-cli/src/commands/recall.rs
Acceptance/tests: bounded citations, retention, export, full row/vector/blob/cache deletion, rebuild parity, no telemetry.

### [ ] E15-17: Knowledge scopes and provenance ledger
Owner: R2 · Estimate: 4d · Depends: E15-16 · Files: rust/compresso-knowledge/src/{item,store,personalization}.rs
Acceptance/tests: personal/repo/org isolation, source revision/confidence/expiry, reversible add/edit/suppress/boost/undo/delete.

### [ ] E15-18: Organization knowledge envelope cryptography
Owner: R2+I1 · Estimate: 2.5d · Depends: E15-17, E10-05
Files: rust/compresso-knowledge/src/{envelope,keys}.rs
Description: Implement client encryption/key lifecycle only.
Acceptance criteria:
- Ciphertext metadata only; revoke rotates; no plaintext/keys persist.
Test cases:
- crypto/tamper; membership; rotation; stale; keychain; canaries.

### [ ] E15-19: Review change-set collector
Owner: R1 · Estimate: 3d · Depends: E3-12, E15-01 · Files: rust/compresso-review/src/changes.rs, fixtures/review/repos/
Acceptance/tests: committed/staged/worktree/approved-untracked manifest, rename/binary/secret handling, malicious git config, no hooks/remotes.

### [ ] E15-20: Review persona orchestration
Owner: R1+PQ · Estimate: 4d · Depends: E15-19, E13-21 · Files: rust/compresso-review/src/{persona,orchestrate}.rs, profiles/review/
Acceptance/tests: original versioned prompts, least context, cost/model policy, parallel cancellation, fallback, failure isolation.

### [ ] E15-21: Finding validation, deduplication, synthesis
Owner: R1+PQ · Estimate: 3d · Depends: E15-20 · Files: rust/compresso-review/src/{finding,validate,synthesize}.rs
Acceptance/tests: verified current path/line/evidence, stale rejection, duplicate/conflict handling, separate severity/confidence.

### [ ] E15-22: Review report and apply modes
Owner: R1+D1 · Estimate: 4d · Depends: E15-21, E15-06 · Files: rust/compresso-review/src/apply.rs, rust/compresso-cli/src/commands/review.rs
Acceptance/tests: report default, save/select/high-confidence plan, no autonomous write, stale/grant denial, edit rollback.

### [ ] E15-23: Historical review backtesting
Owner: PQ+I1 · Estimate: 5d · Depends: E15-21, E14-18 · Files: rust/compresso-eval/src/review_backtest.rs
Acceptance/tests: isolated clone, no remotes/credentials, hidden human outcomes, precision/recall/severity recall/false-positive cost, reproducibility.

### [ ] E15-24: Controlled workflow benchmark
Owner: PQ · Estimate: 4d · Depends: E15-23, E7-06 · Files: rust/compresso-eval/src/workflow_bench.rs, fixtures/workflows/
Acceptance/tests: baseline/safe/retrieval/E15 success, tests, edit correctness, tokens, cost, turns, latency, context metrics, intervention; quality gates savings.

### [ ] E15-25: E15 MCP tools and permission annotations
Owner: R2 · Estimate: 3d · Depends: E15-02, E15-06, E15-12, E15-16, E15-22 · Files: rust/compresso-mcp/src/tools/e15/
Acceptance/tests: separate plan/apply tools, strict schemas, read/write/destructive annotations, forged/stale plan rejection, bounded output.

### [ ] E15-26: Claude Code and Codex parity harness
Owner: D1+PQ · Estimate: 3d · Depends: E15-25, E6-05 · Files: rust/compresso-integrations/tests/e15_parity.rs
Acceptance/tests: identical schemas, paths, permissions, offline behavior, disable, and uninstall; presentation-only differences documented.

### [ ] E15-27: Capability-negotiated hooks and live review
Owner: D1 · Estimate: 4d · Depends: E15-22, E15-26 · Files: rust/compresso-integrations/src/capabilities.rs, rust/compresso-agent/src/events.rs
Acceptance/tests: no Claude-only core dependency, Codex derived events, opt-in intervals/model/cost, timeout fail-open, independent disable.

### [ ] E15-28: Desktop controls and quality/savings UI
Owner: D1 · Estimate: 3d · Depends: E15-16, E15-22, E9-08 · Files: apps/desktop/src/features/e15/
Acceptance/tests: consent/source/permission/review controls, visible destructive previews, quality beside savings, telemetry off, token isolation.

### [ ] E15-29: Assist/prefer/enforce presets
Owner: R1+D1 · Estimate: 2d · Depends: E15-25, E13-20 · Files: profiles/agents/, rust/compresso-agent/src/presets.rs
Acceptance/tests: assist default, native tools retained, enforce visible/acknowledged, bypass and uninstall restoration.

### [ ] E15-30: E15 production and clean-room gates
Owner: PQ+I1 · Estimate: 4d · Depends: E15-24, E15-26, E14-17 · Files: docs/plan/gates.toml, docs/security/clean-room-e15.md, .github/workflows/ci.yml
Acceptance/tests: G16-G17, search/edit/SQL/recall/review/parity/provenance/license gates; deliberate failure and no-WOZ-dependency scans block publish.

### [ ] E15-31: Sandboxed browser workflows
Owner: R2+I1 · Estimate: 5d · Depends: E14-19, E15-30
Files: rust/compresso-browser/, fixtures/browser/security/
Description: Implement disabled browser only over E14-19.
Acceptance criteria:
- Fresh profile, destinations/private-IP/redirect, quarantine, no ambient creds, visible bounded cancel.
Test cases:
- redirect/DNS/localhost; cookies; download; timeout/crash/escape.

### [ ] E15-32: Sandboxed terminal co-drive
Owner: R2+I1 · Estimate: 5d · Depends: E14-20, E15-30
Files: rust/compresso-terminal/, fixtures/terminal/security/
Description: Implement disabled terminal only over E14-20 approved plans.
Acceptance criteria:
- Exact cwd/exe/argv/env/resources; no interpolation; grants; bounded output; kill descendants.
Test cases:
- injection; symlink; exfiltration; fork/output; escape; cancel.


---

## Cross-epic correctness tickets

### [x] E2-12: Provider conversation graph and validator
Owner: R1 · Estimate: 2d · Depends: E2-01, E2-08
Files: rust/compresso-core/src/{graph,provider_conversation}.rs, schemas/context-graph/v1.json, fixtures/conversations/provider-tool-loop/
Description: Implement protected reasoning nodes, typed provider turn/tool identifiers, atomic assistant/tool groups, and profile-extensible pre/post-compression validation from D53.
Acceptance/tests: Anthropic/OpenAI single and parallel tool loops remain serializable; orphan request/result, removed owner turn, changed reasoning, unknown opaque state, and cross-provider state replay all force byte-identical fallback.

### [x] E2-13: Generic-text segmentation and minimum-output invariant
Owner: R1 · Estimate: 2d · Depends: E2-08
Files: rust/compresso-core/src/segment.rs, rust/compresso-mcp/src/tools/compress.rs, fixtures/conversations/single-oversize-text/
Description: Segment plain text into deterministic structural units before packing and enforce non-empty output for non-empty input.
Acceptance/tests: paragraphs/fences/JSON/log groups retain boundaries; final anchor survives; impossible budgets return original bytes with fallback; no successful result contains zero nodes or empty text.

### [x] E2-14: Dedup referential integrity and atomic packing
Owner: R1 · Estimate: 1.5d · Depends: E2-04, E2-07, E2-08
Files: rust/compresso-core/src/strategies/{dedup_exact,pack_budget}.rs, rust/compresso-core/src/safety.rs
Description: Add `duplicate_of` edges and treat referent/stubs as one retention group.
Acceptance/tests: a retained stub always resolves to one byte-preserved referent; pack cannot orphan, cycle, or multiply-reference a stub; malformed external references force fallback.

### [ ] E2-15: Differential token accounting and large-payload latency
Owner: R1+PQ · Estimate: 2.5d · Depends: E2-11, E5-06
Files: rust/compresso-core/src/{graph,engine,tokens}.rs, rust/compresso-eval/benches/, fixtures/logs/mib-rebuild-stream/
Description: Recount changed nodes only, enforce count equality, initialize tokenizers at startup, and benchmark 1 MiB/4 MiB/configured-max payloads.
Acceptance/tests: no strategy invokes full-graph counting; changed and aggregate counts match independent recounts; cold startup stays below 500 ms or fails its distinct gate; warm P95 satisfies the 200 ms product gate.

### [x] E7-07: Canonical quality and benchmark assets
Owner: PQ · Estimate: 3d · Depends: E1-05
Files: fixtures/, docs/plan/06-fixtures.md
Description: Commit fifteen quality fixtures plus two benchmark assets; canonical seeds are repeated-npm-install and mini-monorepo.
Acceptance criteria:
- All paths/hash/size pinned; no silent skip; benchmarks excluded from quality median.
Test cases:
- delete/rename fails lint; gate population; benchmark hash.

### [ ] E10-15: Local account client and device authentication CLI
Owner: R2+D1 · Estimate: 3d · Depends: E10-03, E10-04, E10-07
Files: rust/compresso-agent/src/account.rs, rust/compresso-cli/src/commands/account.rs, rust/compresso-store/migrations/0002_entitlements.sql
Description: Implement `CloudClient`, device-code polling, rotating refresh, keychain persistence, challenge-based device proof, 14-day scheduled entitlement renewal, and login/status/logout/manual-refresh surfaces.
Acceptance/tests: slow_down/expiry/denial/cancel/offline/revocation are bounded; challenge replay/wrong-key proofs fail; no scheduled lease request occurs before day 14; retries are at most daily until day 21; secrets and non-allowlisted local content never reach requests/files/logs; logout clears locally while offline; missing cloud degrades to local-only without blocking MCP.

### [ ] E11-07: Prompt-cache-stable compression epochs
Owner: R1 · Estimate: 2d · Depends: E11-02, E11-03
Files: rust/compresso-proxy/src/cache_epoch.rs, fixtures/provider-payloads/cache/
Description: Freeze serialized prefixes already assigned a provider cache epoch and compress only the uncached suffix. This E11-local contract does not depend on E13 profiles; E13-08 later represents the same capability in model profiles.
Acceptance criteria:
- Second-turn cached prefix and key remain byte-identical; repack/fallback creates an explicit new epoch.
- Provider-observed cached/uncached usage is recorded; unknown cache semantics disable mutation.
Test cases:
- `test_second_turn_prefix_byte_stable`, `test_suffix_only_compression`, `test_repack_starts_new_epoch`, `test_unknown_cache_passthrough`, `test_cached_usage_recorded`.

### [ ] E14-19: Browser sandbox implementation and escape suite
Owner: I1+R2 · Estimate: 3d · Depends: E14-18
Files: rust/compresso-browser/src/sandbox.rs, fixtures/browser/security/
Description: Implement OS-specific browser isolation separately from shared policy.
Acceptance/tests: fresh profiles, destination allowlists, download quarantine, no credential reuse, redirect/private-IP/localhost/resource/escape tests on supported OSes.

### [ ] E14-20: Terminal sandbox implementation and escape suite
Owner: I1+R2 · Estimate: 3d · Depends: E14-18
Files: rust/compresso-terminal/src/sandbox.rs, fixtures/terminal/security/
Description: Implement OS-specific terminal process isolation separately from command policy.
Acceptance/tests: approved cwd/executable/argv, restricted environment, resource/process/output limits, cancellation, and exfiltration/injection/fork/escape tests.

### [ ] E15-18b: Encrypted organization knowledge sync
Owner: R2+I1 · Estimate: 3.5d · Depends: E15-18
Files: rust/compresso-knowledge/src/{sync,queue,merge}.rs
Description: Sync ciphertext with offline queue, merge, replay and key migration.
Acceptance criteria:
- Idempotent convergence/conflict; scope validation; logout clears queue.
Test cases:
- offline/order/duplicate/conflict/revoke/rotate/malformed/cancel.

### [ ] E15-18c: Organization knowledge cloud service
Owner: F1+I1 · Estimate: 4d · Depends: E15-18b, E10-11
Files: apps/control-plane/src/modules/knowledge/, schemas/cloud-api/knowledge-v1.json
Description: Relay ciphertext and membership/key metadata only.
Acceptance criteria:
- Reject plaintext; scoped auth; transactional revoke; id/count audit; explicit limits.
Test cases:
- cross-org; revoke race; replay; oversize; plaintext; DB scan.


## E16 — Full-product scope completion

### [ ] E16-01: Compression plan router
Owner: R1 · Estimate: 3d · Depends: E2-15
Files: rust/compresso-core/src/routing/, fixtures/routing/
Description: Deterministic classification, candidate construction, utility/risk ranking, explanation.
Acceptance criteria:
- Identical replay; declared estimates/risk; policy before rank; no network/model.
Test cases:
- `test_classification_matrix_selects_expected_candidates`: table-drive every supported context kind, mode, and budget class; assert the eligible strategy set and rejection reasons.
- `test_policy_filters_before_utility_ranking`: give a forbidden strategy the highest utility and assert policy removes it before ranking and records the policy reason.
- `test_equal_utility_tie_break_is_stable`: permute equal-scoring candidates and assert the same strategy order and explanation bytes.
- `test_latency_budget_excludes_over_budget_candidate`: assert estimated latency above the remaining budget cannot be selected even when token savings are highest.
- `test_no_eligible_plan_returns_byte_identical_passthrough`: assert original bytes, token count, and protected content are unchanged and the trace names `no_eligible_plan`.
- `test_router_replay_is_byte_identical`: replay the same graph, policy, profile, and budget 100 times and assert identical plan, plan hash, and explanation.
- `test_router_has_no_network_or_model_capability`: compile/test the router with only local traits and assert no provider, socket, or process capability is reachable.

### [ ] E16-02: Advanced strategy pack A
Owner: R1 · Estimate: 4d · Depends: E16-01
Files: rust/compresso-core/src/strategies/, fixtures/strategies/pack-a/
Description: Implement compiler_noise, test_noise, superseded_decisions, source_outline.
Acceptance criteria:
- Each has supports/estimate/execute, lossiness, invariants, flag, fixture floor.
Test cases:
- `test_compiler_noise_removes_repeated_diagnostics_only`: preserve the first diagnostic and distinct locations while enumerating every removed duplicate.
- `test_test_noise_compacts_repeated_pass_output_but_preserves_failures`: remove repetitive passing lines while retaining failures, stack traces, and protected output byte-for-byte.
- `test_superseded_decisions_keeps_latest_complete_chain`: remove only whole superseded decisions and preserve dependency-linked or protected decisions.
- `test_source_outline_emits_signatures_without_bodies`: produce a deterministic symbol skeleton while retaining protected source nodes unchanged.
- `test_pack_a_unsupported_inputs_are_noops`: feed malformed, unknown, and unsupported node kinds to every Pack A strategy and assert unchanged output.
- `test_pack_a_removal_records_reconcile_exactly`: for every strategy, assert summed typed removals equal the before/after token and unit deltas.
- `test_pack_a_feature_flags_disable_independently`: disable each strategy in turn and assert it is neither estimated nor executed while peers remain available.
- `test_pack_a_adversarial_markers_cannot_spoof_structure`: place compiler/test/decision markers inside quoted or source content and assert they are not treated as removable structure.
- `test_pack_a_replay_is_deterministic`: repeat each positive fixture with shuffled non-semantic map ordering and assert byte-identical output and trace.

### [ ] E16-03: Strategy pack B and canonical fourteen
Owner: R1+PQ · Estimate: 4d · Depends: E16-02
Files: rust/compresso-core/src/strategies/, docs/architecture/strategy-catalog.md
Description: Complete the canonical fourteen: dedup_exact, collapse_logs, compact_json, pack_budget, compiler_noise, test_noise, superseded_decisions, source_outline, table_compact, search_cluster, repo_map_prune, history_prune, import_context_prune, and repeated_tool_result.
Acceptance criteria:
- Every registry item maps lossiness/kinds/protection/trace/fixture/modes; no unnamed entry.
Test cases:
- `test_registry_has_exactly_fourteen_unique_ids`: assert the registry contains the canonical fourteen IDs once each in deterministic order.
- `test_every_registry_entry_has_catalog_and_fixture`: assert each ID maps to a catalog row, positive fixture, adversarial fixture, and protected-content fixture.
- `test_catalog_contract_matches_implementation`: compare supported kinds, lossiness, modes, feature flag, and trace removal kinds against each strategy implementation.
- `test_table_compact_preserves_headers_and_ragged_rows`: compact eligible tables while retaining headers, protected cells, and malformed rows without mutation.
- `test_search_cluster_preserves_distinct_high_rank_hits`: deduplicate equivalent hits but retain distinct paths, scores, and protected snippets.
- `test_repo_map_and_history_pruning_preserve_dependencies`: prune low-value units only when no retained node or protected turn depends on them.
- `test_import_context_and_repeated_tool_result_are_atomic`: remove only complete import-context units and complete duplicate tool-result groups with valid referents.
- `test_unknown_or_unnamed_strategy_is_rejected`: fail profile/catalog validation before engine execution and fall back to compiled defaults.
- `test_fourteen_strategy_holdout_floors`: run each strategy against its holdout set and assert declared reduction, recall, safety, and latency floors.

### [ ] E16-04: Balanced and aggressive modes
Owner: R1+PQ · Estimate: 4d · Depends: E16-01, E13-21
Files: rust/compresso-core/src/modes/, profiles/compression/, fixtures/modes/
Description: Implement balanced and opt-in summarizing aggressive modes.
Acceptance criteria:
- Signed profiles; consent/policy/cost; provider/cache preservation; labeled summaries; no remote default.
Test cases:
- `test_mode_precedence_resolves_cli_project_team_defaults`: table-drive all precedence combinations and assert the selected mode plus explanation source.
- `test_aggressive_requires_explicit_current_scope_consent`: reject absent, expired, or differently scoped consent and never persist implicit consent.
- `test_team_policy_can_deny_or_cap_modes`: assert a denied aggressive request resolves to the policy-approved mode without bypass through CLI flags.
- `test_balanced_mode_operates_fully_offline`: deny all network capabilities and assert deterministic compression and trace generation still succeed.
- `test_provider_cache_prefix_is_byte_preserved`: assert signed reasoning, opaque state, and provider cache-prefix bytes and ordering never change in either mode.
- `test_aggressive_summary_is_labeled_and_traceable`: assert each generated summary is marked summarizing lossiness and records source node hashes without raw source in the trace.
- `test_quality_gate_failure_falls_back_to_safer_mode`: force recall or safety failure and assert balanced/aggressive output is discarded for safe or passthrough output.
- `test_tampered_expired_or_incompatible_profile_uses_defaults`: reject invalid signatures, expiry, version bounds, and unknown fields without blocking compression.

### [ ] E16-05: Remaining Atlas language packs
Owner: R2 · Estimate: 4d · Depends: E3-12
Files: rust/compresso-atlas/src/queries/{go,java,c_cpp,c_sharp,ruby,php}.scm, fixtures/code/languages/
Description: Add Go, Java, C/C++, C#, Ruby, PHP packs.
Acceptance criteria:
- Pinned grammars; normalized extraction; malformed recovery; quality/migration per pack.
Test cases:
- `test_go_pack_extracts_packages_imports_types_methods_and_tests`: compare normalized symbols and edges against the committed Go golden fixture.
- `test_java_pack_extracts_packages_imports_nested_types_and_overloads`: preserve qualified names and distinguish overloaded methods in the Java golden fixture.
- `test_c_and_cpp_dispatch_and_extract_distinct_constructs`: select the correct grammar and extract includes, namespaces, structs/classes, functions, and methods without collisions.
- `test_csharp_pack_extracts_namespaces_usings_generics_and_partial_types`: merge only valid partial declarations and preserve generic signatures.
- `test_ruby_pack_extracts_modules_classes_mixins_and_dynamic_calls`: record unresolved dynamic calls explicitly rather than inventing targets.
- `test_php_pack_extracts_namespaces_uses_traits_and_functions`: distinguish imports from trait use and preserve fully qualified names.
- `test_each_pack_recovers_from_malformed_and_truncated_files`: assert bounded partial extraction, diagnostic status, and no panic for every grammar.
- `test_generated_vendor_and_ignored_paths_are_excluded_for_all_packs`: assert scanner policy is applied before parsing and excluded files create no rows.
- `test_grammar_version_change_triggers_transactional_reindex`: upgrade each pinned grammar fingerprint and assert old symbols are atomically replaced or rolled back.
- `test_mixed_language_repo_dispatch_is_deterministic_and_bounded`: index one repository containing all languages and enforce file-size, parse-time, and node-count limits.

### [ ] E16-06: Import resolver and impact completeness
Owner: R2 · Estimate: 3d · Depends: E16-05
Files: rust/compresso-atlas/src/resolve/, rust/compresso-store/migrations/0005_import_resolution.sql
Description: Persist relative/package/workspace/alias tri-state resolution.
Acceptance criteria:
- D60 reserves 0005; deterministic; completeness surfaced; transactional reindex.
Test cases:
- `test_relative_package_workspace_and_alias_resolution`: table-drive supported import forms and assert the canonical target plus `resolved` status.
- `test_ambiguous_workspace_import_persists_all_candidates`: assert deterministic candidate ordering, `ambiguous` status, and surfaced completeness counts.
- `test_missing_or_external_package_is_explicitly_unresolved`: create no false edge and return an unresolved reason without treating it as an index failure.
- `test_resolution_cycle_is_bounded_and_reported`: assert cyclic aliases terminate within the configured hop limit and remain queryable.
- `test_file_rename_invalidates_and_rebuilds_affected_edges`: rename a target and assert stale edges disappear in the same transaction as replacement edges.
- `test_failed_reindex_rolls_back_symbols_and_resolution_rows`: inject a write failure and assert the previous complete index remains visible.
- `test_0005_upgrade_and_downgrade_fixture`: migrate a populated 0004 database, verify tri-state values/indexes, and reject unsupported schema rollback safely.
- `test_impact_response_counts_resolved_ambiguous_and_unresolved`: assert MCP/API completeness totals reconcile with persisted import rows.

### [ ] E16-07: Authenticated Streamable HTTP MCP
Owner: R2+I1 · Estimate: 3d · Depends: E8-03, E14-04
Files: rust/compresso-mcp/src/http/, schemas/local-api/mcp-v1.json
Description: Implement HTTP over stdio ToolRouter.
Acceptance criteria:
- Loopback/auth/peer/origin/host; versions/sessions/resume/cancel/limits; no open SSE.
Test cases:
- `test_stdio_and_http_tool_contracts_are_schema_identical`: enumerate both routers and assert matching names, schemas, results, errors, and protocol versions.
- `test_http_binds_loopback_only`: reject wildcard/non-loopback bind configuration and prove no external interface listener is created.
- `test_missing_invalid_expired_or_wrong_audience_token_is_rejected`: return the same content-free authorization error before dispatch.
- `test_peer_origin_and_host_policy_rejects_confused_deputy_requests`: cover spoofed forwarding headers, DNS rebinding hostnames, disallowed origins, and non-loopback peers.
- `test_session_resume_requires_matching_identity_and_version`: resume valid sessions while rejecting stolen, expired, or protocol-incompatible session IDs.
- `test_request_cancellation_releases_work_and_capacity`: cancel during indexing/read work and assert task, permit, and temporary state cleanup.
- `test_body_response_concurrency_and_timeout_limits`: exercise each boundary at limit and limit-plus-one with deterministic typed errors.
- `test_version_negotiation_rejects_unknown_major_and_accepts_supported_minor`: assert no tool executes before negotiation succeeds.
- `test_shutdown_closes_listener_sessions_and_inflight_requests`: assert bounded graceful shutdown and forced cancellation after deadline.

### [ ] E16-08: Additional coding-client integrations
Owner: D1 · Estimate: 5d · Depends: E6-05, E16-07
Files: rust/compresso-integrations/src/{gemini,opencode,aider,vscode}.rs, fixtures/integrations/
Description: Implement four adapters on E6 contract.
Acceptance criteria:
- Versioned detection/ownership; base-bound idempotent lifecycle; no core branches.
Test cases:
- `test_gemini_adapter_contract_matrix`: cover detection, fresh install, existing config merge, plan hash, apply, verification, and uninstall on every supported platform path.
- `test_opencode_adapter_contract_matrix`: cover detection, fresh install, existing config merge, plan hash, apply, verification, and uninstall on every supported platform path.
- `test_aider_adapter_contract_matrix`: cover detection, fresh install, existing config merge, plan hash, apply, verification, and uninstall on every supported platform path.
- `test_vscode_adapter_contract_matrix`: cover user/workspace scope, fresh install, existing config merge, plan hash, apply, verification, and uninstall on every supported platform path.
- `test_all_new_adapters_preserve_unowned_keys_and_format`: mutate only declared owned keys and retain unrelated client configuration byte-equivalently where the format permits.
- `test_all_new_adapters_reject_malformed_or_unsupported_versions`: produce a previewable refusal without writing, backing up, or creating a receipt.
- `test_all_new_adapters_detect_base_drift_before_apply`: modify the complete config after planning and assert apply fails without partial writes.
- `test_all_new_adapters_restore_receipts_and_are_idempotent`: run install/verify/uninstall twice and assert exact owned-state restoration with no duplicate entries.
- `test_client_addition_requires_no_core_or_mcp_branch`: enforce dependency/lint checks proving adapters register through the E6 interface only.

### [ ] E16-09: Desktop account/settings/privacy/overview
Owner: D1 · Estimate: 4d · Depends: E9-08, E10-15
Files: apps/desktop/src/features/
Description: Implement missing pages on E9-01 harness.
Acceptance criteria:
- Offline/degraded; config preview; privacy truth; sources; a11y/responsive.
Test cases:
- `test_account_page_device_login_success_cancel_expiry_and_retry`: drive the complete device flow and assert credentials cross only the Rust keychain boundary.
- `test_account_page_offline_and_entitlement_degraded_states`: display cached status, expiry, and local-only behavior without blocking local features.
- `test_settings_preview_apply_and_base_drift`: show the exact diff, require confirmation, reject stale base hashes, and refresh from authoritative config.
- `test_privacy_page_matches_no_telemetry_and_local_data_contract`: assert copy and controls expose no nonexistent upload toggle and list every local data class and deletion action.
- `test_overview_page_uses_local_ledger_without_content_leakage`: reconcile displayed operations/savings with local records while excluding prompts, source, and secrets.
- `test_all_pages_pass_axe_keyboard_focus_and_reduced_motion`: run component accessibility checks at desktop and narrow responsive widths.
- `test_webview_cannot_access_tokens_files_shell_or_arbitrary_http`: invoke each prohibited bridge path and assert it is absent or denied by Rust capability policy.
- `test_packaged_desktop_account_settings_privacy_overview_flow`: execute the four-page happy path and cloud-outage path against the packaged application.

### [ ] E16-10: Desktop update/rollback/disable/uninstall
Owner: D1+I1 · Estimate: 3d · Depends: E12-07, E16-09
Files: apps/desktop/src/features/lifecycle/, apps/desktop/src-tauri/lifecycle.rs
Description: Expose signed lifecycle with Rust privilege.
Acceptance criteria:
- Preview/cancel/recovery/rollback/base-uninstall; assistant survives failures.
Test cases:
- `test_valid_signed_update_previews_then_installs_atomically`: require explicit confirmation and assert version, migration, health check, and retained previous version.
- `test_tampered_expired_or_wrong_platform_metadata_is_refused`: perform no download/install mutation and present a content-free recovery message.
- `test_interrupted_download_or_install_resumes_or_rolls_back_cleanly`: restart at every lifecycle checkpoint and assert one healthy version plus consistent update state.
- `test_failed_post_update_health_check_restores_previous_version`: roll back binaries and migrations and preserve assistant availability.
- `test_disable_and_reenable_are_idempotent_and_non_destructive`: stop Compresso hooks/services without deleting indexes, settings, or unrelated client configuration.
- `test_uninstall_preview_detects_config_drift_and_preserves_unowned_keys`: refuse stale plans, remove only receipt-owned state, and retain user data unless explicitly selected.
- `test_progress_cancellation_obeys_last_safe_checkpoint`: cancel download, staging, migration, and cleanup phases with documented outcomes.
- `test_lifecycle_secrets_never_enter_webview_logs_or_error_payloads`: seed tokens and signing material and assert redaction across UI, IPC, diagnostics, and crash paths.

### [ ] E16-11: Local team-policy enforcement
Owner: R1+R2 · Estimate: 4d · Depends: E10-11, E10-15
Files: rust/compresso-agent/src/policy/, rust/compresso-core/src/policy.rs
Description: Fetch/verify/cache/explain/enforce signed policy.
Acceptance criteria:
- Explicit precedence/constraints; stale/offline/revoke/malformed visible and local fail-open.
Test cases:
- `test_valid_signed_policy_is_fetched_cached_and_enforced`: assert issuer, audience, organization, version, and expiry validation before policy activation.
- `test_policy_key_rotation_accepts_overlap_and_rejects_unknown_kid`: exercise previous/current/next windows and retirement of the old key.
- `test_policy_precedence_matrix_is_deterministic`: table-drive hard safety, organization policy, project config, user config, and command overrides.
- `test_stale_or_offline_policy_degrades_to_documented_local_behavior`: anchor expiry monotonically, expose state, and never disable unrelated local compression.
- `test_revoked_org_or_device_invalidates_cached_policy_and_entitlement`: prevent privileged policy use while preserving local-only operation.
- `test_malformed_unknown_or_rollback_policy_fails_open_locally`: reject schema errors, unknown critical fields, lower versions, and replayed bundles without crashing.
- `test_policy_cannot_unprotect_forced_protected_kinds_or_enable_forbidden_network`: assert immutable safety constraints override every signed policy field.
- `test_policy_explanation_snapshot_names_value_and_source_without_secrets`: verify stable user-facing provenance for each effective setting.

### [ ] E16-12: Cloud infrastructure provisioning
Owner: I1 · Estimate: 4d · Depends: E10-01
Files: infra/terraform/, infra/fly/, docs/runbooks/deploy.md
Description: Provision staging/prod network/compute/DB/TLS/secrets/backups/migrations.
Acceptance criteria:
- Least privilege/isolation/PITR/OIDC/drift/destroy/rollback.
Test cases:
- `test_terraform_validate_and_policy_checks_for_staging_and_prod`: reject public databases, wildcard IAM, plaintext secrets, missing encryption, and unpinned providers.
- `test_clean_account_plan_is_reproducible_and_apply_is_idempotent`: compare two plans and assert the second apply has no unintended changes.
- `test_staging_smoke_covers_dns_tls_api_auth_database_and_jobs`: provision from empty state and verify each dependency through public and private paths as appropriate.
- `test_postgres_pitr_restores_to_declared_rpo`: write a marker, restore around its timestamp, and verify integrity plus measured RPO/RTO evidence.
- `test_failed_forward_migration_blocks_release_and_runs_documented_recovery`: inject a migration fault and assert old application/database compatibility remains.
- `test_secret_rotation_preserves_service_and_revokes_previous_secret`: rotate every cloud secret class and verify old credentials fail after overlap.
- `test_staging_and_prod_have_no_network_identity_or_data_path_overlap`: assert separate state, accounts/projects, databases, keys, and allowlists.
- `test_production_destroy_requires_break_glass_approval_and_backup_evidence`: prove ordinary CI credentials cannot destroy protected resources.

### [ ] E16-13: Cloud observability/SLOs/runbooks
Owner: I1+F1 · Estimate: 4d · Depends: E16-12
Files: infra/observability/, docs/runbooks/, docs/slo/
Description: Implement content-free telemetry, SLOs, alerts, dashboards, response.
Acceptance criteria:
- API/auth/billing/update SLOs; no content; owned drills; local independence.
Test cases:
- `test_api_auth_billing_and_update_sli_queries_match_synthetic_events`: assert numerator, denominator, labels, and exclusion rules for every SLI.
- `test_fast_and_slow_burn_alerts_fire_at_declared_thresholds`: replay multi-window failures and assert paging versus ticket routing.
- `test_redaction_canaries_never_reach_logs_metrics_traces_or_alerts`: seed prompt, source, token, email, and path canaries at every cloud boundary.
- `test_alert_routes_to_current_owner_with_runbook_and_context`: validate severity, deduplication, escalation timeout, and content-free diagnostic links.
- `test_dashboards_handle_empty_delayed_and_partial_data_honestly`: never display fabricated zeroes or success during telemetry pipeline failure.
- `test_each_incident_runbook_completes_in_staging_with_evidence`: exercise API outage, auth compromise, billing webhook backlog, failed release, and database recovery.
- `test_cloud_outage_does_not_affect_local_compression_index_or_mcp`: deny DNS/network for the control plane and assert local operations remain within gates.
- `test_observability_pipeline_has_retention_access_and_cost_bounds`: verify deletion, least-privilege queries, cardinality limits, and budget alerts.

### [ ] E16-14: Public site and admin completion
Owner: F1+D1 · Estimate: 4d · Depends: E10-13, E10-14
Files: apps/web/, apps/admin/
Description: Implement downloads/docs/pricing/status/legal and device/flags/releases admin.
Acceptance criteria:
- Authz; signed downloads; a11y/SEO/states; audited changes; independent status.
Test cases:
- `test_public_download_resolves_platform_channel_and_signed_artifact`: verify checksum/signature metadata, expiry, unsupported-platform behavior, and no unsigned fallback.
- `test_pricing_docs_status_and_legal_pages_render_without_auth`: assert canonical metadata, structured data, stable links, and independent status hosting.
- `test_admin_role_matrix_denies_every_cross_role_action_server_side`: table-drive support, release, billing, security, and read-only roles rather than relying on hidden UI.
- `test_admin_device_revoke_invalidates_target_only_and_is_audited`: require confirmation, preserve other devices, and record actor/reason/time without secrets.
- `test_feature_flag_and_release_changes_are_base_bound_and_audited`: reject stale plans and record before/after values, approvals, rollout, and rollback.
- `test_site_and_admin_pass_axe_keyboard_focus_and_responsive_checks`: cover primary routes, dialogs, tables, errors, and narrow viewports.
- `test_seo_metadata_and_robots_rules_match_environment`: index public production pages while excluding admin, account, previews, and non-production hosts.
- `test_loading_empty_error_partial_and_outage_states_are_truthful`: inject API/auth/billing/status failures and assert recoverable actions without leaking internals.

### [ ] E16-15: Parser fuzzing and platform gates
Owner: PQ+I1 · Estimate: 3d · Depends: E16-05
Files: rust/compresso-atlas/fuzz/, .github/workflows/ci.yml
Description: Add parser fuzz corpus and Linux ARM64 test/package.
Acceptance criteria:
- Every parser bounded; crashes regress; ARM artifact parity; no panic/root escape.
Test cases:
- `test_each_parser_fuzz_target_accepts_seed_corpus_without_crash`: run all language targets against golden, malformed, binary-like, deeply nested, and Unicode seeds.
- `test_parser_crash_corpus_replays_in_normal_ci`: promote every discovered crash/minimized input to a deterministic regression corpus.
- `test_parser_resource_caps_stop_depth_size_node_and_time_bombs`: assert typed bounded failure with no panic, runaway allocation, or partial committed index.
- `test_parser_paths_cannot_escape_repository_root`: fuzz path, symlink, include/import, and encoding inputs and assert canonical root enforcement.
- `test_linux_arm64_build_and_unit_integration_suites_match_x64`: run the supported test matrix and compare feature/config parity.
- `test_linux_arm64_release_archive_has_checksum_signature_sbom_and_provenance`: verify every artifact before publication.
- `test_linux_arm64_installer_verifies_then_runs_smoke_commands`: install into an isolated home and execute version, doctor, index, and MCP initialize.
- `test_linux_arm64_artifact_allowlist_rejects_source_fixtures_maps_and_symbols`: seed each forbidden class and assert the release gate fails.

### [ ] E16-16: Full-product contract closure
Owner: PQ · Estimate: 3d · Depends: E16-01..E16-15
Files: docs/plan/, docs/adr/, docs/architecture/, scripts/plan-lint.ts
Description: Machine-check deliverables/dependencies/references/owners/estimates/conventions.
Acceptance criteria:
- No overlays; full mapping; acyclic order; derived rollups; existing refs; full templates.
Test cases:
- `test_plan_lint_rejects_dangling_declared_file_path`: distinguish future ticket outputs from references required to exist at plan time.
- `test_plan_lint_rejects_unknown_stale_or_duplicate_ticket_id`: report every source location and the nearest canonical ID without rewriting files.
- `test_plan_lint_rejects_dependency_cycle_and_wave_inversion`: emit the complete cycle or violating edge and fail nonzero.
- `test_plan_lint_recomputes_epic_slice_post_and_grand_rollups`: detect count/day drift from ticket bodies, including duplicate correction rows.
- `test_plan_lint_requires_owner_estimate_depends_files_description_acceptance_and_tests`: fail each missing or malformed ticket field independently.
- `test_plan_lint_validates_owner_alias_adr_profile_and_command_references`: reject undefined owners, missing ADR files, root-level profiles, and noncanonical CLI/MCP names.
- `test_plan_lint_rejects_contract_drift_across_schema_interface_tool_and_ticket`: fixture mismatched field names, versions, errors, and enum values across canonical documents.
- `test_plan_lint_rejects_amendment_overlays_and_mutated_source_documents`: require in-place canonical edits and explicit plan deltas for immutable source inputs.
- `test_plan_lint_clean_corpus_matches_repository_plan`: run against the complete Compresso plan and emit stable machine-readable counts, days, and dependency order.


## Execution order (flat, dependency-ordered)

Slice: E1-01, E1-02, E1-03, E1-04, E1-05, E1-06, E2-01, E2-02, E2-02a, E3-01, E2-03, E3-02, E3-03, E2-04, E2-05, E2-06, E2-07, E2-08, E2-12, E2-13, E2-14, E3-04, E3-05, E3-06, E3-07, E3-08, E2-09, E7-01, E7-07, E2-10, E2-11, E3-09, E7-02, E3-10, E3-11, E3-12, E4-01, E4-02, E4-03, E4-04, E4-05, E5-01, E5-02, E5-03, E5-04, E5-05, E5-06, E2-15, E7-03, E7-04, E7-05, E7-06, E5-07, E6-01, E6-02, E6-03, E6-04, E6-05.

Post-slice, wave 1 — local platform: E8-01, E8-02, E8-03, E8-08, E8-04, E8-05, E8-06, E8-07, E14-01, E14-02, E14-03, E14-04.
Wave 2 — desktop: E9-01, E14-05, E9-02, E9-03, E9-04, E9-05, E9-06, E9-07, E9-08.
Wave 3 — cloud + local entitlements: E10-01, E10-02, E10-09, E10-03, E10-04, E10-05, E10-06, E10-07, E10-15, E10-08, E10-10, E10-11, E10-12, E10-13, E10-14.
Wave 4 — provider proxy: E11-01, E11-02, E11-03, E11-04, E11-05, E11-06, E11-07.
Wave 5 — model expansion: E13-01..E13-11, E13-13, E13-14, E13-16, E13-12, E13-15, E13-17..E13-22; then E13-23..E13-31 subject to dependencies.
Wave 6 — release engineering and hardening close-out: E12-01, E13-32, E12-02, E12-03, E12-04, E12-05, E12-06, E14-08, E14-09, E14-10, E14-11, E12-07, E12-08, E14-15, E14-16, E14-12, E14-14, E12-09.
Wave 7 — E15 A-F plus threat-model and sandbox foundations: E15-01, E14-17, E14-18, E15-02, E15-03, E15-05, E15-09, E15-11, E15-14, E15-19, E15-06..E15-17, E15-18, E15-18b, E15-18c, E15-20..E15-30 subject to dependencies.
Wave 8 — full-product completion: E16-01..E16-16 subject to listed dependencies.
Wave 9 — deferred sandbox actions: E14-19, E14-20, E15-04, E15-31, E15-32.

Parallelism notes:
- Slice: E2 (R1) and E3 (R2) run concurrently after E1; E7-01/E7-02 (PQ/R1) overlap E2's tail; E4 (D1) starts once E2-09 and E3-12 land; E5 (R2) follows E3; E6 (D1) follows E5-01 and overlaps E5-02..07; E1-06 and E4-05 (I1) slot in any time after E1-02/E4-04. E2-02a is a maintainer task (needs an Anthropic API key) and can land any time after E2-02 without blocking anything — until it lands, the interim 1.15 factor applies.
- Post-slice: wave 1 (R2) and wave 3 (F1 owns cloud; R1/R2 own the E10-06..08 local entitlement tickets) run concurrently; wave 2 (D1) follows E8-04. Wave 5 (R1/R2/PQ) is independent of E8–E12 through its first three phases and should start alongside wave 1 to smooth R1/R2 utilization. E11 (R1) needs only E2/E3 and can overlap wave 3. E14-01..04 land early in wave 1 so release hardening inputs (dist profile, IPC suite) are ready when E12-01 starts; E14-05 rides with E9-01.
- Hard cross-epic edges: existing edges remain; E15 high-impact work needs E14-17; browser/terminal need E14-18 and E15-30.
- Solo mode: all roles collapse to the owner; the flat lists above remain the execution order.

## Estimate rollup

| Epic | Tickets | Days | Weeks | Notes |
|------|---------|------|-------|-------|
| E1 Foundation | 6 | 9 | ~2 wk | +E1-06 tarball job (D42) |
| E2 Core engine | 16 | 25.5 | ~5 wk | +E2-02a calibration; eleven safety checks |
| E3 Atlas | 12 | 20.5 | ~4 wk | D41 concurrency + D44 staleness folded in |
| E4 CLI | 5 | 7.5 | ~1.5 wk | +E4-05 installer (D42) |
| E5 MCP server | 7 | 10.5 | ~2 wk | D47 recording, D44 unfold |
| E6 Integrations | 5 | 9.5 | ~2 wk | installed-binary path resolution (D42) |
| E7 Eval & fixtures | 7 | 13.5 | ~2.5 wk (overlaps E2/E3) | +recall gate tickets (D40) |
| **Slice total** | **58** | **96 gross** | **~19.2 solo engineer-wk; 2 engineers ≈ 9–10 calendar wk** | Derived directly from ticket bodies |
| E8 Supervisor & local API | 8 | 20 | ~4 wk | |
| E9 Desktop (Tauri) | 8 | 25 | ~5 wk | |
| E10 Cloud + local entitlements | 15 | 43 | ~8.5 wk (F1, with R1/R2 local tickets in parallel) | |
| E11 Provider proxy | 7 | 19 | ~4 wk | |
| E12 Release engineering | 9 | 28 | ~5.5 wk | absorbs E14-06/07/13 |
| E13 Model & client expansion | 32 | 91 | ~18 engineer-wk, phased | +D52 future phases |
| E14 IP & secure deployment | 17 | 39 | ~8 wk | +E15 threat/sandbox gates |
| E15 WOZ-inspired capabilities | 34 | 121.5 | ~24.5 engineer-wk | A-F incremental; G deferred |
| E16 Full-product completion | 16 | 59 | ~12 engineer-wk, dependency-phased | |
| **Post-slice total** | **146** | **445.5 gross** | heavily parallel across roles | Derived directly from ticket bodies |
| **Grand total** | **204** | **541.5 gross** | **~108.3 engineer-wk** | |
