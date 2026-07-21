# 05 — Ticket Backlog

Tickets are dependency-ordered within and across epics. Every ticket ends with a conventional commit. TDD is expected on every ticket: write the failing test first, implement, make it pass, commit. The global constraints in `00-overview.md` (fail-open, protected-node invariants, no telemetry, no Claude Desktop, `--dry-run`/`--json` support, schema versioning at boundaries, latency gates) apply to **every** ticket below and are not repeated in each acceptance list. Interface names are contractual and come from `03-interfaces.md`; changing one requires updating that document first.

Checkbox syntax `[ ]` / `[x]` tracks completion. Owner codes are defined in `00-overview.md` §Staffing.

---

## E1 — Foundation

### [ ] E1-01: Monorepo workspace scaffold
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

### [ ] E1-02: CI workflow
Owner: I1 · Estimate: 2d · Depends: E1-01
Files: .github/workflows/ci.yml, .github/workflows/release.yml (stub)
Description: Author `ci.yml` per `01-stack-and-workspace.md` §4: rust job matrix (`macos-14`, `macos-13`, `ubuntu-24.04`, `windows-2022`) running `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `cargo build --release -p compresso-cli`; `Swatinem/rust-cache@v2`; release-binary artifact upload on main. The `eval` job is added in E7-04; the `ts` job is gated off until Phase 6. `release.yml` is a named no-op stub until Phase 8 (E12).
Acceptance criteria:
- CI triggers on push to main and on PRs; all four matrix legs green on the scaffold commit.
- Clippy runs with `-D warnings` and formatting is enforced via `cargo fmt --check`.
- Release binary for each platform uploaded as an artifact on main pushes.
Test cases:
- test_ci_yaml_valid: `actionlint .github/workflows/ci.yml` exits 0 (run locally and as a CI step).
- test_matrix_covers_d33: assert ci.yml matrix contains macos-14, macos-13, ubuntu-24.04, windows-2022 (yq assertion in a repo lint script).

### [ ] E1-03: Supply-chain checks (cargo-deny, cargo-audit, gitleaks)
Owner: I1 · Estimate: 1d · Depends: E1-02
Files: deny.toml, .github/workflows/ci.yml (security job), .gitleaks.toml
Description: Add the `security` CI job from `01-stack-and-workspace.md` §4: `cargo audit` for advisories, `cargo deny check licenses bans` with a deny.toml that encodes the license allowlist and bans network-I/O crates from core/atlas/mcp/store dependency trees, and gitleaks secret scanning.
Acceptance criteria:
- `cargo deny check licenses bans` passes locally and in CI; deny.toml bans `reqwest`, `hyper-tls`, `native-tls`, `ureq` except via an explicit allow for `compresso-updater` (post-slice).
- `cargo audit` runs in CI and fails the job on unaddressed advisories.
- gitleaks action runs on PRs; a fixture secret in a test branch is caught (verified once manually, documented in the PR).
Test cases:
- test_deny_bans_network_crates: `cargo deny check bans` exits 0 on the current tree; adding `reqwest` to compresso-core in a scratch branch makes it exit non-zero.
- test_audit_runs: CI security job log contains `cargo audit` summary line.

### [ ] E1-04: Commit versioned JSON Schemas
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
- test_trace_requires_fallback_used: trace document missing `fallback_used` fails against compression-trace v1 `$defs/trace`.

### [ ] E1-05: Fixtures skeleton and conventions doc
Owner: PQ · Estimate: 1.5d · Depends: E1-01
Files: fixtures/{code,logs,conversations,provider-payloads,adversarial}/README.md, fixtures/logs/repeated-timestamps/, fixtures/code/rust-small/, docs/plan/06-fixtures.md (skeleton referencing final format), docs/architecture/conventions.md
Description: Create the fixture directory tree from `01-stack-and-workspace.md` §3 with at least one seed fixture per category (a 500-line repeated log, a small Rust crate, a tool-call conversation JSON, an Anthropic count-calibration payload stub, a prompt-injection adversarial repo), each carrying a `fixture.toml` in the format defined by `docs/plan/06-fixtures.md` (input/expected/eligibility keys). Write `conventions.md` covering error conventions (thiserror per crate, anyhow in binaries only per D20), commit format, module layout, and test naming (`test_snake_case`).
Acceptance criteria:
- All five fixture categories exist with at least one committed fixture and a `fixture.toml` declaring `input`, `expected`, and `eligibility` per `06-fixtures.md`.
- `fixtures/logs/repeated-timestamps/input.txt` contains exactly 500 log lines identical after timestamp normalization (used verbatim by E2-05 and E7).
- conventions.md documents error, naming, commit, and schema-versioning conventions and is linked from the repo README.
Test cases:
- test_fixture_tomls_parse: walk fixtures/, parse every fixture.toml with the `toml` crate, assert required keys present.
- test_repeated_log_fixture_shape: read repeated-timestamps/input.txt, assert 500 lines and that stripping the leading ISO-8601 timestamp yields a single distinct line.

---

## E2 — Core engine

### [ ] E2-01: ContextGraph types, validation, serde round-trip
Owner: R1 · Estimate: 2d · Depends: E1-01, E1-04
Files: rust/compresso-core/src/graph.rs, tests in same file + rust/compresso-core/tests/graph_schema_roundtrip.rs
Description: Implement `ContextGraph`, `ContextNode`, `NodeKind`, `Source`, `ContextEdge`, `EdgeKind`, `GraphError` exactly per `03-interfaces.md` §2.1, with serde renames matching `schemas/context-graph/v1.json` (snake_case kinds, tagged `source`). Implement `ContextGraph::validate()` enforcing the code-level invariants from `02-schemas.md` §1 (unique ids, edge endpoints exist, protection required for `SystemInstruction`/`UserInstruction`/`ToolDefinition`/`ToolRequest` unless explicitly unprotected via metadata flag), `total_tokens()`, and `from_text(content, kind, counter)`.
Acceptance criteria:
- A graph containing one node of every `NodeKind` and one edge of every `EdgeKind` serializes to JSON that validates against `schemas/context-graph/v1.json` and deserializes back to an equal value.
- `validate()` returns typed `GraphError` variants for duplicate ids, dangling edges, and protection violations; never panics on adversarial input.
- `token_provider` defaults to `openai_o200k` on deserialization when absent.
Test cases:
- test_roundtrip_all_kinds_validates_against_schema: build the every-kind graph, serialize, validate with jsonschema against v1.json, deserialize, assert equality.
- test_validate_rejects_duplicate_ids: two nodes with id "n1" → `Err(GraphError::DuplicateId("n1"))`.
- test_validate_rejects_dangling_edge: edge from "n1" to "missing" → `Err(GraphError::UnknownEdgeEndpoint { .. })`.
- test_system_instruction_must_be_protected: node kind `SystemInstruction` with `protected: false` and no unprotect flag → protection-invariant error.
- test_from_text_counts_tokens: `ContextGraph::from_text("fn main() {}", NodeKind::SourceFile, counter)` yields one node with `token_count == counter.count("fn main() {}")` and `total_tokens()` equal to it.

### [ ] E2-02: TokenCounter providers
Owner: R1 · Estimate: 1.5d · Depends: E1-01, E1-05
Files: rust/compresso-core/src/tokens.rs, tests in same file
Description: Implement `TokenProvider`, the `TokenCounter` trait, and `counter_for(provider) -> &'static dyn TokenCounter` per `03-interfaces.md` §2.2: tiktoken-rs `o200k_base` and `cl100k_base` counters, `AnthropicCalibrated` (o200k count × `CALIBRATION` constant derived from `fixtures/provider-payloads/anthropic-counts.json`, documented in ADR-0017), and a `Heuristic` fallback (bytes/4) for use when tokenizer init fails. Singletons are lazily initialized once.
Acceptance criteria:
- `counter_for(TokenProvider::OpenAiO200k).count(text)` matches tiktoken-rs `o200k_base` exactly; same for cl100k.
- `AnthropicCalibrated` returns `ceil(o200k_count × CALIBRATION)` and its `provider()` tag is `AnthropicCalibrated`; CALIBRATION is a named const with a doc comment citing the fixture file.
- `counter_for` returns the same static reference on repeated calls (no re-init); counting a 1 MB string completes in < 50 ms in release mode.
Test cases:
- test_o200k_hello_world: `count("hello world") == 2` for OpenAiO200k.
- test_empty_string_zero: all four providers return 0 for "".
- test_calibrated_is_scaled_o200k: for the text of `fixtures/provider-payloads/anthropic-counts.json` samples, calibrated count equals `ceil(o200k × CALIBRATION)` and is within 10% of the recorded Anthropic count for every sample.
- test_counter_for_singleton: `std::ptr::eq(counter_for(OpenAiO200k), counter_for(OpenAiO200k))` is true.
- test_provider_tag_roundtrip: each counter's `provider()` serializes to the schema enum string (`"openai_o200k"` etc.).

### [ ] E2-03: CompressionStrategy trait and StrategyRegistry
Owner: R1 · Estimate: 1d · Depends: E2-01, E2-02
Files: rust/compresso-core/src/strategy.rs, tests in same file
Description: Implement `Estimate`, `StrategyOutput`, `CompressionStrategy` (with `id`, `supports`, `estimate`, `execute`), `StrategyError`, and `StrategyRegistry` per `03-interfaces.md` §2.3. `StrategyRegistry::safe_default(Mode::Safe)` returns the ordered list `[dedup_exact, collapse_logs, compact_json, pack_budget]`; `Mode::Passthrough` returns an empty registry. Ships with a `NoopStrategy` test double so the registry is testable before E2-04..07 land.
Acceptance criteria:
- `safe_default(Mode::Safe).strategies()` yields exactly four entries whose `id()`s are, in order: `"dedup_exact"`, `"collapse_logs"`, `"compact_json"`, `"pack_budget"` (registered as stubs until their tickets land, each stub returning `supports() == false`).
- `safe_default(Mode::Passthrough).strategies()` is empty.
- Trait objects are `Send + Sync` (compile-time assertion).
Test cases:
- test_safe_registry_order: collect `id()`s from `safe_default(Mode::Safe)`, assert `["dedup_exact","collapse_logs","compact_json","pack_budget"]`.
- test_passthrough_registry_empty: `safe_default(Mode::Passthrough).strategies().len() == 0`.
- test_strategy_object_safety: `fn assert_send_sync<T: Send + Sync>() {}` instantiated with `Box<dyn CompressionStrategy>`.

### [ ] E2-04: Exact-duplicate removal strategy
Owner: R1 · Estimate: 1d · Depends: E2-03
Files: rust/compresso-core/src/strategies/dedup_exact.rs, tests in same file
Description: Implement `dedup_exact` per `03-interfaces.md` §2.3: identical unprotected node contents keep the first occurrence and replace later occurrences with a short reference stub (`[duplicate of node <id>]`), preserving node ids, kinds, and edges. `supports()` is true only when at least two unprotected nodes share byte-identical content; `estimate()` returns saved tokens for all-but-first duplicates.
Acceptance criteria:
- First occurrence of duplicated content is byte-unchanged; replacements reference the kept node's id; edges untouched.
- Protected nodes are never candidates, even when byte-identical to unprotected ones.
- Output graph passes `validate()`; `token_count` on stub nodes is recomputed with the graph's counter.
Test cases:
- test_dedup_three_identical_tool_responses: three unprotected `ToolResponse` nodes with content `"SELECT * FROM users LIMIT 50;\n...200 identical rows..."`; after execute, node 1 byte-equal, nodes 2–3 contain `"[duplicate of node"` and node 1's id, `total_tokens()` strictly decreases.
- test_protected_duplicate_untouched: a protected `SystemInstruction` and an unprotected `ToolResponse` with identical content → both byte-unchanged, `supports()` false when they are the only pair.
- test_no_duplicates_not_supported: graph of three distinct contents → `supports() == false`; `execute` (if forced) returns the input unchanged with an explanatory note.
- test_estimate_matches_execute: `estimate().tokens_saved` equals `before_tokens - after_tokens` measured after execute on the three-duplicate graph, within the stub-token overhead.

### [ ] E2-05: Log-collapse strategy
Owner: R1 · Estimate: 1.5d · Depends: E2-03, E1-05
Files: rust/compresso-core/src/strategies/collapse_logs.rs, tests in same file
Description: Implement `collapse_logs` per `03-interfaces.md` §2.3: within unprotected nodes of kinds `ToolResponse`, `CompilerOutput`, `TestOutput`, runs of ≥3 consecutive lines that are identical after normalizing timestamps, hex strings (≥8 chars), and UUIDs are collapsed to `<first line> [×N repeats]`. Non-matching runs and protected nodes pass through byte-identical.
Acceptance criteria:
- Runs shorter than 3 lines are never collapsed; collapsed output keeps the first raw line verbatim plus the `[×N repeats]` suffix.
- Normalization treats ISO-8601 timestamps, `0x`-prefixed and bare hex ≥8 chars, and RFC-4122 UUIDs as equal; everything else must match exactly.
- Protected nodes and `SourceFile`/`CodeSymbol` nodes are untouched.
Test cases:
- test_collapse_500_timestamped_lines: load `fixtures/logs/repeated-timestamps/input.txt` (500 identical lines with varying timestamps) into one `ToolResponse` node; output content has ≤5 lines and contains `"[×500"`.
- test_two_repeats_untouched: node with 2 identical lines → byte-unchanged, strategy note records the skip.
- test_uuid_and_hex_normalization: 10 lines `"request 3f2a9c1d4b5e6f70 done (id=550e8400-e29b-41d4-a716-446655440000)"` with differing hex/uuid values → collapsed to 1 line containing `"[×10"`.
- test_protected_log_untouched: same 500-line content in a node with `protected: true` → byte-identical output.
- test_interleaved_runs: 5×"A", 1×"B", 4×"A" → output `A [×5]`, `B`, `A [×4]` (three lines; runs never merge across interruptions).

### [ ] E2-06: JSON-compaction strategy
Owner: R1 · Estimate: 1.5d · Depends: E2-03
Files: rust/compresso-core/src/strategies/compact_json.rs, tests in same file
Description: Implement `compact_json` per `03-interfaces.md` §2.3: unprotected `ToolResponse` nodes whose content parses as JSON are minified, long string values are truncated with an elision marker (`"…[+N chars]"`), and long arrays are elided in the middle (`"…[+N items]"` sentinel element), preserving object/array structure and all keys. Content that fails `serde_json` parsing is untouched.
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

### [ ] E2-07: Budget-packing strategy
Owner: R1 · Estimate: 1.5d · Depends: E2-03
Files: rust/compresso-core/src/strategies/pack_budget.rs, tests in same file
Description: Implement `pack_budget` per `03-interfaces.md` §2.3: while `total_tokens() > budget.max_tokens`, drop whole unprotected nodes in ascending `priority / token_count` order. Code nodes (`SourceFile`, `CodeSymbol`) are dropped whole or kept whole — never partially truncated. Dropped node ids are recorded in strategy notes; edges referencing dropped nodes are removed so the output still validates.
Acceptance criteria:
- Output `total_tokens() ≤ budget.max_tokens` whenever the protected set alone fits; otherwise the strategy stops at protected-only and records that the budget is unsatisfiable in notes (engine surfaces this; no protected node is ever dropped).
- Drop order is deterministic: ties on priority/token broken by node id lexicographic order.
- No node content is ever modified — nodes are only removed.
Test cases:
- test_drops_lowest_value_first: nodes A(priority 0.9, 100 tok), B(0.2, 100 tok), C(0.5, 100 tok), budget 250 → B dropped, A and C kept, note lists `"dropped:B"`.
- test_protected_never_dropped: protected node of 1000 tokens with budget 100 → protected node kept, all unprotected dropped, note records unsatisfiable budget, no error.
- test_code_never_truncated: `SourceFile` node of 400 tokens with budget 300 and no other nodes → node dropped whole (content never sliced) or budget unsatisfied note; assert no output node has content that is a strict prefix of the input content.
- test_edges_pruned: edge A→B where B is dropped → edge absent from output and `output.validate()` passes.
- test_deterministic_tiebreak: two identical-score nodes "n_a" and "n_b" → "n_a" dropped first on two consecutive runs (assert equality of both outputs).

### [ ] E2-08: Safety module — run_all with seven checks
Owner: R1 · Estimate: 1.5d · Depends: E2-01, E2-02
Files: rust/compresso-core/src/safety.rs, tests in same file
Description: Implement `SafetyCheck` and `run_all(original, candidate, budget, elapsed_ms) -> Vec<SafetyCheck>` per `03-interfaces.md` §2.5 with exactly the seven named checks from build-doc §10.6: `protected_unchanged`, `no_inflation`, `json_still_valid`, `fences_balanced`, `refs_valid`, `latency_ok`, `schema_valid`. Every check always appears in the output vector with `passed` and a human-readable `detail` containing no raw node content.
Acceptance criteria:
- `run_all` returns exactly seven checks with those names, in that order, on every call.
- `protected_unchanged` compares byte equality of content for every node protected in the original; a missing protected node also fails it.
- `detail` strings reference node ids and counts only — a test asserts no node content substring leaks into any detail.
Test cases:
- test_all_pass_on_identity: `run_all(g, g, budget, 10)` → all seven `passed == true`.
- test_protected_mutation_fails: candidate changes one byte of a protected node → `protected_unchanged.passed == false`, detail names the node id.
- test_inflation_fails_without_note: candidate `total_tokens()` > original → `no_inflation.passed == false`.
- test_broken_json_fails: original node content `{"a":1}` becomes `{"a":` → `json_still_valid` fails; a node that was never valid JSON does not trigger it.
- test_unbalanced_fence_fails: original with two ``` fences, candidate with one → `fences_balanced` fails.
- test_dangling_file_ref_fails: candidate contains `src/ghost.rs:42` absent from the original → `refs_valid` fails; refs present in the original pass.
- test_latency_exceeded_fails: `elapsed_ms = 500` with `budget.max_latency_ms = 200` → `latency_ok` fails.
- test_invalid_candidate_schema_fails: candidate with duplicate node ids → `schema_valid` fails.

### [ ] E2-09: Engine::compress pipeline, trace generation, fallback
Owner: R1 · Estimate: 2d · Depends: E2-04, E2-05, E2-06, E2-07, E2-08, E1-04
Files: rust/compresso-core/src/engine.rs, rust/compresso-core/src/trace.rs, tests in both + rust/compresso-core/tests/trace_schema.rs
Description: Implement `Mode`, `CompressionBudget`, `Lossiness`, `CompressionResult`, `Engine::new(mode, counter)`, and `Engine::compress(input, budget) -> CompressionResult` per `03-interfaces.md` §2.4, plus `Trace` serialization matching `schemas/compression-trace/v1.json`. Pipeline per build-doc §10.3: validate → mark protected → run registry strategies in order within the remaining latency budget → `safety::run_all` on the cumulative result → on any strategy error, failed check, or panic (caught via `catch_unwind`), restore the original graph and set `fallback_used`/`fallback_reason`. `compress` **never returns Err**. Traces carry ULID `operation_id`, per-strategy `StrategyResult`s, all safety checks, and a human-readable explanation with no raw content.
Acceptance criteria:
- `compress` signature returns `CompressionResult` (not `Result`); every failure path yields `output == input`, `fallback_used == true`, non-null `fallback_reason`.
- `Mode::Passthrough` returns the input unchanged with `strategy_results` empty and `mode: "passthrough"` in the trace.
- Serialized trace validates against `schemas/compression-trace/v1.json` `$defs/trace`; `operation_id` is a valid ULID; trace JSON contains no node content substring.
- Identical input + budget produce identical output graphs across runs (determinism; trace ids differ).
Test cases:
- test_never_err_on_panicking_strategy: engine with an injected strategy whose `execute` panics → returns normally, `fallback_used == true`, `fallback_reason` contains the strategy id, output byte-equal to input.
- test_failed_safety_restores_original: injected strategy that mutates a protected node → `protected_unchanged` fails, output equals input, `fallback_used == true`.
- test_passthrough_identity: `Engine::new(Mode::Passthrough, counter).compress(g, &budget)` → `after_tokens == before_tokens`, zero strategy_results.
- test_trace_validates_against_schema: compress the E1-05 log fixture graph, serialize `result.trace`, validate against compression-trace v1 → ok.
- test_trace_leaks_no_content: assert the trace JSON string does not contain any 20-char substring of any input node's content.
- test_safe_mode_on_log_fixture: 500-line log fixture in Safe mode → `after_tokens < before_tokens × 0.5`, `fallback_used == false`, `strategy_results` includes `collapse_logs` with `applied == true`.
- test_deterministic_output: two compress calls on the same input → outputs serialize to identical JSON (excluding operation_id/created_at).

### [ ] E2-10: Property tests for engine invariants
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

### [ ] E2-11: Criterion latency benchmarks with P95 gate
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

### [ ] E3-01: Store::open and migrations
Owner: R2 · Estimate: 1.5d · Depends: E1-01
Files: rust/compresso-store/src/lib.rs, rust/compresso-store/migrations/0001_init.sql, tests in rust/compresso-store/tests/migrations.rs
Description: Implement `Store::open(path)` and `Store::open_default()` per `03-interfaces.md` §3: open rusqlite (bundled) with WAL and foreign keys on, run pending migrations transactionally from embedded SQL files, record them in `schema_migrations`. Commit `0001_init.sql` byte-for-byte per `02-schemas.md` §4 (all twelve tables, FTS5 `symbols_fts` with triggers, indexes). `open_default` resolves the platform data dir per `01-stack` §5 via `dirs`. Startup prunes `operations` older than 90 days (configurable).
Acceptance criteria:
- `Store::open` on a fresh temp path creates the DB, applies migration 1, and inserts `(1, applied_at)` into `schema_migrations`; reopening applies nothing and changes nothing.
- `PRAGMA journal_mode` returns `wal`; `PRAGMA foreign_keys` returns 1 on every opened connection.
- A migration that fails mid-way (injected bad SQL in a test-only migration) leaves the DB at the prior version with no partial objects.
Test cases:
- test_open_fresh_creates_all_tables: open temp DB, query `sqlite_master`, assert all table names from 02-schemas §4 present including `symbols_fts`.
- test_open_idempotent: open twice; `schema_migrations` has exactly one row with version 1.
- test_failed_migration_rolls_back: register test migration 2 with invalid SQL → open returns `StoreError`, `schema_migrations` max version still 1, no `test_table` exists.
- test_wal_and_fk_enabled: pragmas assert as above.
- test_retention_prunes_old_operations: insert operation with `created_at` 91 days ago and one 89 days ago, reopen → only the recent row remains.

### [ ] E3-02: Repository, file, and symbol DAOs
Owner: R2 · Estimate: 1.5d · Depends: E3-01
Files: rust/compresso-store/src/repos.rs, rust/compresso-store/src/files.rs, rust/compresso-store/src/symbols.rs, tests in same files
Description: Implement the repository/file/symbol surface of `Store` per `03-interfaces.md` §3: `approve_repo`, `repo_by_root`, `upsert_file`, `file_hashes`, `replace_symbols` (symbols + edges in a single transaction), `delete_files`, `search_symbols` (FTS5, bm25-ranked), `symbols_in_file`, `symbol_by_name`, with `RepoRecord`/`FileRecord`/`SymbolRecord`/`EdgeRecord`/`SymbolHit` types. Only this crate contains SQL.
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
- test_symbol_by_name_qualified: `symbol_by_name(repo, "SessionManager::start")` returns `(FileRecord, SymbolRecord)` with correct path and line range.

### [ ] E3-03: Session, operation, receipt, and stats DAOs
Owner: R2 · Estimate: 1.5d · Depends: E3-01, E1-04
Files: rust/compresso-store/src/ops.rs, rust/compresso-store/src/receipts.rs, tests in same files
Description: Implement `start_session`, `record_operation` (validating `trace_json` against `schemas/compression-trace/v1.json` before insert per 02-schemas §4 rules), `stats(since)` returning `StatsSummary` in the savings vocabulary of `02-schemas.md` §3 (`observed_tokens`, `applied_tokens`, `removed_tokens`, `compression_overhead`, `net_tokens_saved`, labelled `modeled_cost_saved`), plus `save_receipt`/`receipts` for `integration_receipts`.
Acceptance criteria:
- `record_operation` rejects (typed error, no row written) any `trace_json` failing schema validation; accepted rows also populate `strategy_results` child rows.
- `stats(None)` aggregates across all operations; `stats(Some(ts))` filters by `created_at >= ts`; `net_tokens_saved == removed_tokens - compression_overhead` in the returned summary.
- Receipts round-trip all fields including `keys_owned` JSON array; `receipts(Some("codex"))` filters by client.
Test cases:
- test_record_operation_rejects_bad_trace: trace_json `{"schema_version":1}` (missing required fields) → `Err(StoreError::InvalidTrace)`, `operations` count 0.
- test_record_operation_persists_strategy_results: valid trace with 2 strategy_results → 1 `operations` row + 2 `strategy_results` rows keyed by operation_id.
- test_stats_vocabulary_math: two ops (observed 1000/applied 700/overhead 50, observed 500/applied 500/overhead 0, second with fallback_used=1) → `observed_tokens == 1500`, `applied_tokens == 1200`, `removed_tokens == 300`, `net_tokens_saved == 250`.
- test_stats_since_filters: op at t=100 and t=200; `stats(Some(150))` counts only the second.
- test_receipt_roundtrip_and_filter: save claude-code and codex receipts → `receipts(Some("codex"))` returns exactly the codex one with `keys_owned == ["mcp_servers.compresso"]`.

### [ ] E3-04: Repository scanner
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

### [ ] E3-05: ParserRegistry scaffold + Rust extraction
Owner: R2 · Estimate: 1.5d · Depends: E3-01
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

### [ ] E3-06: TypeScript + TSX extraction
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

### [ ] E3-07: JavaScript extraction
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

### [ ] E3-08: Python extraction
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

### [ ] E3-09: Incremental Indexer
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

### [ ] E3-10: Queries — repo_map and outline
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

### [ ] E3-11: Queries — unfold, search, read_bounded (path security)
Owner: R2 · Estimate: 2.5d · Depends: E3-09
Files: rust/compresso-atlas/src/queries.rs, tests in rust/compresso-atlas/tests/queries_unfold_search_read.rs
Description: Implement `unfold(store, repo, path, symbol)` (exact body via stored line range, read from disk, blake3 hash-checked against the stored file hash — mismatch returns the content flagged stale with a reindex hint), `search(store, repo, query, limit)` over FTS5 (delegating to `Store::search_symbols`), and `read_bounded(root, path, range, budget_tokens, counter)` per `03-interfaces.md` §4. Every externally-supplied path is canonicalized and must satisfy `starts_with(approved_root)` **after** symlink resolution; violations return `AtlasError::OutsideRoot`. Path traversal and symlink-escape tests are REQUIRED for this ticket to close.
Acceptance criteria:
- `unfold` returns the exact source lines `start_line..=end_line`; after an on-disk edit without reindex, result carries `stale: true` and a reindex hint, and still returns current-disk content for the stored range.
- `read_bounded` truncates at `budget_tokens` with an explicit truncation flag and honors 1-based inclusive `range`.
- `../` traversal, absolute paths outside root, and symlinks pointing outside root all return `AtlasError::OutsideRoot`; symlinks resolving inside root are allowed.
Test cases:
- test_unfold_exact_body: unfold `SessionManager::start` on the E3-05 fixture → returned text byte-equal to lines 10–14 of sample.rs.
- test_unfold_stale_after_disk_edit: index, append a comment to the file, unfold → `stale == true`, hint mentions `compresso index`.
- test_search_ranked: seed symbols `parse_config`, `ConfigParser`, `apply_theme`; `search(.., "config", 10)` returns the two config symbols before `apply_theme` is absent/last.
- test_read_bounded_dotdot_rejected: `read_bounded(root, "../../../etc/passwd", None, 1000, counter)` → `Err(AtlasError::OutsideRoot)`.
- test_read_bounded_absolute_outside_rejected: path `/etc/hosts` → `Err(AtlasError::OutsideRoot)`.
- test_read_bounded_symlink_escape_rejected: create `root/link.rs -> /etc/passwd` (unix) → `Err(AtlasError::OutsideRoot)`; on Windows the test is skipped with a named guard.
- test_read_bounded_internal_symlink_ok: `root/alias.rs -> root/src/lib.rs` → content returned.
- test_read_bounded_budget_truncates: 5000-token file with budget 200 → `counter.count(result.content) <= 200`, `truncated == true`.
- test_read_bounded_range: range `(3, 5)` returns exactly lines 3–5 inclusive.

### [ ] E3-12: Queries — impact and plan_context
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

### [ ] E4-01: clap skeleton with all slice commands and --json
Owner: D1 · Estimate: 1.5d · Depends: E1-01, E2-01
Files: rust/compresso-cli/src/main.rs, rust/compresso-cli/src/cli.rs, tests in rust/compresso-cli/tests/cli_skeleton.rs
Description: clap (derive) command tree with every slice command: `init`, `doctor`, `index`, `map`, `search`, `outline`, `unfold`, `context`, `compress`, `explain`, `stats`, `config` (with `explain` subcommand), `mcp serve`, `integrations` (`list|install|uninstall|verify`), `uninstall`. Global `--json` flag emitting a versioned envelope `{"schema_version":1,"ok":bool,"data":...,"error":...}` on every command; modifying commands (`init`, `index`, `integrations install|uninstall`, `uninstall`) accept `--dry-run`. Commands return typed exit codes (0 ok, 1 operational error, 2 usage). Handlers are stubs returning `unimplemented` errors in the JSON envelope until wired in E4-02/03/04; `anyhow` at the binary edge only per D20.
Acceptance criteria:
- `compresso --help` lists all fifteen commands; `compresso <cmd> --help` renders for each without panic.
- Every command run with `--json` emits exactly one line of parseable JSON to stdout matching the envelope; human output goes to stdout without `--json`, diagnostics to stderr.
- Unknown subcommand exits 2; stubbed command exits 1 with `error.code == "unimplemented"` in JSON mode.
Test cases:
- test_help_lists_all_commands: assert_cmd `--help` output contains each of the fifteen command names.
- test_json_envelope_on_stats: `compresso stats --json` → stdout parses as JSON with keys `schema_version`, `ok`, and exit code consistent with `ok`.
- test_dry_run_accepted_on_modifiers: `compresso index --dry-run --json` parses args successfully (no usage error).
- test_unknown_command_exit_2: `compresso frobnicate` → exit code 2.

### [ ] E4-02: Wire Atlas commands (init, index, map, search, outline, unfold, context)
Owner: D1 · Estimate: 1.5d · Depends: E4-01, E3-10, E3-11, E3-12
Files: rust/compresso-cli/src/commands/{init.rs,index.rs,atlas.rs}, tests in rust/compresso-cli/tests/cli_atlas.rs
Description: `init` approves the current (or `--repo`) root via `Store::approve_repo` and runs a first `Indexer::index`; `index` re-runs incrementally with a progress bar (suppressed under `--json`); `map`/`search`/`outline`/`unfold`/`context` call `repo_map`/`search`/`outline`/`unfold`/`plan_context` with `--budget` token flags defaulting per config. `impact` is exposed as `compresso context --impact <path>`? No — `impact` gets its own hidden alias under `search --impact`? Neither: expose `impact` through `compresso map --impact <path>` is wrong too; it is wired as its own flag on `context`. Correction: `impact(path)` is wired as `compresso context --impact <path>`, documented in `--help`.
Acceptance criteria:
- `compresso init` in a fixture repo creates the DB (default path per 01-stack §5, overridable via `--db` for tests), indexes it, and prints an `IndexReport` summary; `--dry-run` prints the scan plan and writes nothing.
- `compresso search <q> --json` returns hits with path, qualified_name, kind, line; `unfold` prints the exact body; all outputs match their atlas-layer test constants.
- Path arguments outside the approved root surface `AtlasError::OutsideRoot` as exit 1 with `error.code == "outside_root"`.
Test cases:
- test_init_then_search_e2e: temp fixture repo; `init --db tmp.db` then `search SessionManager --json` → hit for `SessionManager` with `path == "src/sample.rs"`.
- test_index_incremental_via_cli: run `index` twice; second `--json` report has `files_indexed == 0`.
- test_unfold_outputs_exact_body: `unfold src/sample.rs SessionManager::start` stdout equals lines 10–14 of the fixture.
- test_init_dry_run_writes_nothing: `init --dry-run --db tmp.db` → tmp.db does not exist afterwards.
- test_outside_root_error_code: `compresso unfold ../etc/passwd x --json` → exit 1, `error.code == "outside_root"`.

### [ ] E4-03: Wire engine commands (compress, explain, stats)
Owner: D1 · Estimate: 1d · Depends: E4-01, E2-09, E3-03
Files: rust/compresso-cli/src/commands/engine.rs, tests in rust/compresso-cli/tests/cli_engine.rs
Description: `compress` reads stdin (or `--file`), builds a graph via `ContextGraph::from_text` (kind from `--kind`, default `tool_response`), runs `Engine::compress` with mode/budget flags (`--mode safe|passthrough`, `--target-tokens`), prints compressed content, records the operation via `Store::record_operation`; `explain` prints the trace for an operation id (or `--last`) from the store — ids, counts, checks, never raw content; `stats` renders `Store::stats` in the §3 savings vocabulary with `modeled_cost_saved` explicitly labelled `modeled`.
Acceptance criteria:
- `echo <500-line log> | compresso compress --json` reports `before_tokens`, `after_tokens`, `fallback_used` and stores one operation row; passthrough mode round-trips stdin byte-exact.
- `compresso explain --last --json` returns the stored trace, validating against compression-trace v1.
- `compresso stats --json` field names match 02-schemas §3 exactly; human output labels modeled savings as "modeled".
Test cases:
- test_compress_log_fixture_via_stdin: pipe fixtures/logs/repeated-timestamps/input.txt → JSON `after_tokens < before_tokens`, stdout content contains `"[×500"`.
- test_passthrough_byte_exact: `--mode passthrough` with arbitrary UTF-8 input → stdout equals stdin.
- test_explain_last_matches_schema: after a compress, `explain --last --json` → `data.trace` validates against schemas/compression-trace/v1.json and contains no input substring.
- test_stats_labels_modeled: `stats --json` → keys include `net_tokens_saved` and `modeled_cost_saved`; human mode output contains the word "modeled".

### [ ] E4-04: doctor and config explain
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

### [ ] E5-02: Tool handlers — compresso_repo_map, compresso_outline, compresso_unfold
Owner: R2 · Estimate: 1.5d · Depends: E5-01, E3-10, E3-11
Files: rust/compresso-mcp/src/tools/atlas_nav.rs, tests in rust/compresso-mcp/tests/tools_nav.rs
Description: Implement the three navigation handlers per `04-mcp-and-integrations.md` §1, calling `repo_map`, `outline`, and `unfold` with token budgets from arguments (defaults from `Config`). Argument names/semantics stay compatible with the Tetris equivalents per D7 (`tetris_outline`→`compresso_outline`, `tetris_unfold`→`compresso_unfold`). Handlers are pure request/response, validate arguments against the tool schema, and map `AtlasError` variants to MCP error responses with stable codes.
Acceptance criteria:
- Each tool called against a temp indexed fixture repo returns content matching the atlas-layer constants (same body as E3-10/E3-11 tests) inside MCP content blocks.
- Invalid arguments (missing `path`, negative budget) return schema-validation errors without invoking atlas.
- `compresso_unfold` on a stale file returns the stale flag and reindex hint in structured content.
Test cases:
- test_repo_map_tool_budget: call with `{"budget_tokens": 300}` → returned text ≤ 300 tokens by the o200k counter.
- test_outline_tool_matches_atlas: outline of `src/sample.rs` via MCP equals the E3-10 outline rendering.
- test_unfold_tool_exact_body: unfold `SessionManager::start` → text equals fixture lines 10–14.
- test_unfold_missing_symbol_error: unknown symbol → MCP error with code `symbol_not_found`, no panic.

### [ ] E5-03: Tool handlers — compresso_search, compresso_read (gate mode), compresso_context
Owner: R2 · Estimate: 1.5d · Depends: E5-01, E3-11, E3-12
Files: rust/compresso-mcp/src/tools/atlas_read.rs, tests in rust/compresso-mcp/tests/tools_read.rs
Description: Implement `compresso_search` (→ `search`), `compresso_read` (→ `read_bounded`, plus a `gate: true` argument implementing the Tetris `tetris_context_gate` semantics per D7: return size/token estimate and a worth-reading verdict without file content), and `compresso_context` (→ `plan_context`). All path arguments flow through the E3-11 canonicalization boundary; `OutsideRoot` maps to a stable MCP error code and never echoes the resolved path outside the root.
Acceptance criteria:
- `compresso_read` with `gate: true` returns token estimate and metadata only — response contains zero file-content bytes.
- Traversal/symlink-escape inputs return error code `outside_root`; repository content in responses is data-only (no tool-side execution of anything found in files).
- `compresso_context` respects `budget_tokens` and returns the plan sections in map→outline→symbol order.
Test cases:
- test_read_tool_bounded: read `src/sample.rs` with `budget_tokens: 100` → ≤100 tokens, `truncated: true` in structured result.
- test_read_gate_mode_no_content: `{"path":"src/sample.rs","gate":true}` → response includes `token_estimate` and omits every line of the file (assert a known source line absent).
- test_read_dotdot_via_mcp: `{"path":"../../etc/passwd"}` → MCP error `outside_root`.
- test_search_tool_hits: `{"query":"SessionManager"}` → first hit qualified_name `SessionManager` with path and line.
- test_context_tool_budget: `{"task":"session retry logic","budget_tokens":800}` → ≤800 tokens.

### [ ] E5-04: Tool handlers — compresso_compress, compresso_impact, compresso_stats, compresso_explain
Owner: R2 · Estimate: 1.5d · Depends: E5-01, E2-09, E3-03, E3-12
Files: rust/compresso-mcp/src/tools/engine_tools.rs, tests in rust/compresso-mcp/tests/tools_engine.rs
Description: `compresso_compress` builds a graph from the `text` argument (D7: compatible with `tetris_compress_text`), runs `Engine::compress` in the configured mode, returns compressed text plus real-tokenizer before/after counts; `compresso_impact` → `impact`; `compresso_stats` → `Store::stats` in the §3 vocabulary; `compresso_explain` returns the stored trace for an `operation_id`. Compress and every other handler record an `OperationRecord` (E5-05 supplies the session id plumbing).
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

### [ ] E5-05: Sessions and operation ledger
Owner: R2 · Estimate: 1.5d · Depends: E5-01, E3-03
Files: rust/compresso-mcp/src/session.rs, tests in rust/compresso-mcp/tests/session_ledger.rs
Description: On MCP connect, call `Store::start_session(client, "stdio")` (client name from initialize clientInfo, `"unknown"` fallback); thread the `SessionId` through `McpDeps` so every tool handler records an `OperationRecord` with the tool name, mode, observed/applied token counts, and trace JSON; close the session (set `ended_at`) on disconnect. Bounded concurrency: tool calls execute serially per session (rmcp default) and the ledger write failure never fails the tool response (log + continue, fail-open).
Acceptance criteria:
- One `sessions` row per MCP connection with `client` from clientInfo and `ended_at` set after EOF.
- Every tool invocation produces exactly one `operations` row with `tool` equal to the D7 tool name and `session_id` linking to the session.
- A read-only DB (chmod 444 in test) does not break tool responses; the failure is logged to stderr.
Test cases:
- test_session_row_lifecycle: connect, call one tool, disconnect → session row with client `"test-harness"`, non-null ended_at.
- test_operation_per_tool_call: 3 tool calls → 3 operations rows, tools recorded as called, all sharing the session_id.
- test_ledger_failure_fail_open: make DB read-only, call `compresso_search` → valid MCP result returned, stderr contains a ledger warning.

### [ ] E5-06: MCP startup-time budget test
Owner: R2 · Estimate: 1d · Depends: E5-01
Files: rust/compresso-mcp/tests/startup_budget.rs, .github/workflows/ci.yml (release-mode test step)
Description: Measure spawn-to-initialize-response latency for the release binary: spawn `compresso mcp serve`, send `initialize`, and require a response within the D37 budget of 500 ms, sampled over 10 runs using the P95. Guards against eager tokenizer/grammar/db work moving into the startup path (heavy singletons must stay lazy per E2-02).
Acceptance criteria:
- P95 of 10 cold spawns < 500 ms in release mode on macos-14 and ubuntu-24.04 CI legs.
- Test is `#[ignore]`d in debug and executed via a dedicated `cargo test --release -p compresso-mcp -- --ignored startup` CI step.
- A regression demonstration (eagerly loading both tiktoken encoders at startup in a scratch branch) makes the test fail, documented in the PR.
Test cases:
- test_startup_p95_under_500ms: 10 spawn+initialize cycles, sort, assert `samples[8] < 500ms`.
- test_no_db_migration_before_initialize: spawn with `COMPRESSO_DB` pointing at a nonexistent path → initialize still answers in budget (store opens lazily on first tool call).

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

### [ ] E6-01: Integration trait, backups, and receipt plumbing
Owner: D1 · Estimate: 2d · Depends: E1-01, E3-03
Files: rust/compresso-integrations/src/lib.rs, rust/compresso-integrations/src/backup.rs, tests in same files
Description: Implement the `Integration` trait (`client_id`, `detect`, `plan_install`, `install`, `verify`, `uninstall`), `InstallCtx` with `Scope::User | Scope::Project(PathBuf)`, `Detection`, `InstallPlan` (exact key-level diff, human-readable), `InstallReceipt`, `VerificationReport`, `RestoreReport` per `03-interfaces.md` §6, plus shared helpers: timestamped backup (`<config>.compresso-backup-<unix>`), atomic write (tmp file + rename on the same filesystem), and receipt persistence via `Store::save_receipt` (wired at the CLI layer so this crate keeps zero internal deps per `01-stack` §3 — receipts are returned as values, stored by the caller).
Acceptance criteria:
- `install` always writes a backup before modifying anything and returns a receipt listing the exact keys written, backup path, and compresso version.
- Atomic write proven: killing the process between tmp-write and rename (test seam) leaves the original config byte-intact.
- Crate has no `compresso-*` dependencies (serde types only), matching the dependency rule.
Test cases:
- test_backup_created_before_write: fake adapter installs into a temp config → backup file exists with original bytes and a unix-timestamp suffix.
- test_atomic_write_crash_safe: inject failure after tmp write, before rename → target file unchanged, tmp cleaned or ignorable.
- test_receipt_lists_owned_keys: receipt `keys_owned == ["mcpServers.compresso"]` for the fake adapter, `compresso_version` equals crate version.
- test_no_internal_deps: Cargo.toml assertion mirroring E1-01.

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

### [ ] E6-03: Codex adapter (toml_edit)
Owner: D1 · Estimate: 2d · Depends: E6-01, E5-01
Files: rust/compresso-integrations/src/codex.rs, rust/compresso-integrations/Cargo.toml (add toml_edit), tests in codex.rs
Description: Adapter with `client_id() == "codex"` targeting `~/.codex/config.toml` per `04-mcp-and-integrations.md` §2, owning only the `[mcp_servers.compresso]` table (`command`, `args`). Chosen approach, documented in the module header and ADR-0018: use the `toml_edit` crate for format-preserving edits so user comments, key ordering, and whitespace outside the owned table survive install/uninstall; plain `toml` (de)serialization is used only for read-side detection. Covers Codex CLI and Codex Desktop, which share this config file (D4).
Acceptance criteria:
- Installing into a config containing comments and custom tables preserves every comment and unrelated table byte-for-byte; only the owned table is added/updated.
- Uninstall removes the owned table and any trailing blank line it introduced; result is byte-identical to the pre-install file in the round-trip test.
- Missing `~/.codex/` yields `Detection::NotFound`; malformed TOML yields a plan-time typed error, never a partial write.
Test cases:
- test_codex_comments_preserved: config with `# my proxy settings` comment and `[profile.o3]` table → post-install file contains both verbatim plus `[mcp_servers.compresso]`.
- test_codex_install_uninstall_byte_roundtrip: bytes(before) == bytes(after uninstall) for a commented fixture config.
- test_codex_malformed_toml_no_write: config `"[mcp_servers"` → `plan_install` returns Err, file mtime/bytes unchanged.
- test_codex_updates_stale_entry: existing compresso table pointing at an old binary path → install rewrites just that table, receipt notes `updated`.

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

### [ ] E6-05: Idempotency, restore, and CLI wiring with temp HOME
Owner: D1 · Estimate: 2.5d · Depends: E6-02, E6-03, E6-04, E4-01
Files: rust/compresso-cli/src/commands/integrations.rs, rust/compresso-integrations/tests/idempotency_restore.rs
Description: Wire `compresso integrations list|install|uninstall|verify` (per-client or `--all`) with mandatory dry-run preview (`--dry-run` prints the `InstallPlan` diff; interactive install shows the plan and requires confirmation unless `--yes`), receipt persistence via `Store::save_receipt`, and `compresso uninstall` performing all-client restore then self-cleanup listing. Cross-adapter test suite runs every adapter under a fabricated `$HOME` (tempfile + env override; `USERPROFILE` on Windows) proving idempotency and restoration per build-doc §25.6 gates.
Acceptance criteria:
- Running `integrations install <client>` twice yields identical configs and a single active receipt per client (second run reports `already-installed`, writes nothing, exit 0).
- `integrations uninstall <client>` restores each adapter's config to its pre-install bytes (JSON deep-equal for claude-code/cursor, byte-equal for codex) and stamps `uninstalled_at` on the receipt.
- `--dry-run` never modifies any file (asserted via before/after directory hash) and prints a key-level diff naming exactly the owned keys.
Test cases:
- test_install_twice_idempotent_all_clients: for each of the three adapters under temp HOME: install, snapshot config, install again → snapshots equal, receipts count 1.
- test_uninstall_restores_all_clients: install all three, uninstall all → all three configs equal their originals.
- test_dry_run_no_writes: hash config dir, run `integrations install claude-code --dry-run --json`, rehash → equal; JSON plan lists `mcpServers.compresso`.
- test_receipt_survives_and_stamps: after uninstall, `Store::receipts(Some("codex"))[0].uninstalled_at.is_some()`.
- test_missing_client_graceful: no `~/.codex` dir → `install codex --json` exits 1 with `error.code == "client_not_found"`, other clients unaffected in `--all` mode.

---

## E7 — Eval & fixtures

### [ ] E7-01: Fixture loader
Owner: PQ · Estimate: 2d · Depends: E1-05, E2-01
Files: rust/compresso-eval/src/fixtures.rs, tests in same file
Description: Implement `FixtureSet` loading the corpus format defined in `docs/plan/06-fixtures.md`: each fixture directory contains `fixture.toml` declaring `input` (file + node kind + protection flags), `expected` (bounds on reduction, required/forbidden output substrings), and `eligibility` (whether the fixture counts toward the reduction gate — adversarial and passthrough fixtures are ineligible). Loader validates every fixture at load time and materializes a `ContextGraph` per fixture via `ContextGraph::from_text` and the declared kinds.
Acceptance criteria:
- `FixtureSet::load("fixtures/")` loads every committed fixture across all five categories; any malformed `fixture.toml` fails loading with the offending path in the error.
- Eligibility flags parsed and exposed; adversarial fixtures load with `eligible == false`.
- Graphs built from fixtures pass `validate()` including declared protection flags.
Test cases:
- test_load_all_committed_fixtures: load fixtures/ → count ≥ 5, one per category minimum, zero errors.
- test_malformed_fixture_rejected: temp fixture with `expected` missing → `Err` naming the fixture path and missing key.
- test_eligibility_parsed: `fixtures/adversarial/prompt-injection-repo/fixture.toml` → `eligible == false`; `fixtures/logs/repeated-timestamps` → `eligible == true`.
- test_fixture_graph_validates: every loaded fixture's graph `validate().is_ok()`.

### [ ] E7-02: Eval runner CLI
Owner: R1 · Estimate: 2d · Depends: E7-01, E2-09
Files: rust/compresso-eval/src/runner.rs, rust/compresso-eval/src/main.rs, tests in rust/compresso-eval/tests/runner.rs
Description: Implement `EvalRunner`: for each fixture, run `Engine::compress` in the fixture's declared mode, verify `expected` assertions (required/forbidden substrings, reduction bounds, protected-node byte-equality, output JSON validity where input was JSON), collect per-fixture and aggregate metrics (median reduction on eligible fixtures, P95 latency, protected-mutation count, malformed-JSON count, fallback rate per build-doc §25.3), and emit a JSON report. Binary interface: `cargo run -p compresso-eval -- run fixtures/ --gates docs/plan/gates.toml` per `01-stack` §4.
Acceptance criteria:
- Report JSON contains per-fixture rows (id, mode, before/after tokens, latency ms, passed) and aggregates (`median_reduction_eligible`, `p95_latency_ms`, `protected_mutations`, `malformed_json`, `fallback_rate`).
- A fixture whose expected substring is missing marks that fixture failed without aborting the run.
- Runner is deterministic across runs on the committed corpus (identical report modulo timings).
Test cases:
- test_runner_full_corpus_report_shape: run on fixtures/ → JSON parses, has `fixtures[]` and `aggregate` keys, all committed fixtures present.
- test_log_fixture_expectations: repeated-timestamps row → `after_tokens < before_tokens`, output contains `"[×500"` per its fixture.toml expected block.
- test_failed_expectation_isolated: inject a temp fixture expecting substring "UNOBTAINIUM" → that row `passed == false`, aggregate still produced, exit code reflects failure only via gates (E7-03).
- test_protected_mutation_counter_zero: aggregate `protected_mutations == 0` on the committed corpus.

### [ ] E7-03: Release gates file and gate checks
Owner: PQ · Estimate: 2d · Depends: E7-02, E2-11
Files: docs/plan/gates.toml, rust/compresso-eval/src/gates.rs, tests in same file
Description: Implement `Gates` loading `docs/plan/gates.toml` and evaluating the aggregate report against the D37 / build-doc §25.6 slice gates: `median_reduction_eligible > 0.25`, `protected_mutations == 0`, `malformed_json == 0`, `p95_latency_ms < 200`. `compresso-eval -- run ... --gates` exits non-zero with a named failing gate when any gate regresses; gates.toml is the single committed source of thresholds (consumed by CI, referenced by E2-11).
Acceptance criteria:
- gates.toml committed with exactly the four gates and their thresholds, each with a comment citing D37/§25.6.
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

---

## E8 — Supervisor & local API (post-slice, coarse)

Per `03-interfaces.md` §7 and build-doc Phase 4. Estimate rollup: 3 wk.

### [ ] E8-01: Supervisor lifecycle in compresso-agent
Owner: R2 · Estimate: 3d · Depends: E5-07
- `compresso-agent` daemonizes, supervises the MCP/stdio children, and exposes start/stop/status via `compresso agent` subcommands.
- Crash of a child restarts it with backoff; supervisor failure never blocks direct `compresso mcp serve` (embedded fallback mode per Phase 4 exit criteria).
- Health subsystem reports component status consumed by `/v1/health`.

### [ ] E8-02: API token auth and transport bootstrap
Owner: R2 · Estimate: 2d · Depends: E8-01
- Bearer token generated at first start into `~/.local/state/compresso/api-token` with 0600 permissions; rejected requests return 401 with no body detail.
- Localhost HTTP + UDS listeners per `03-interfaces.md` §7; remote interfaces never bound.
- `schemas/local-api/v1.json` committed and request/response validation enforced at the boundary.

### [ ] E8-03: Read endpoints — health, sessions, operations, stats
Owner: R2 · Estimate: 3d · Depends: E8-02
- `GET /v1/health`, `/v1/sessions` (paginated), `/v1/operations?session=&since=&limit=`, `/v1/stats?since=` implemented against `Store`, traces returned without raw content.
- Responses conform to `schemas/local-api/v1.json`; savings vocabulary matches 02-schemas §3.

### [ ] E8-04: Repo management endpoints
Owner: R2 · Estimate: 2d · Depends: E8-02
- `GET/POST /v1/repos` and `DELETE /v1/repos/:id` wired to `approve_repo`/`Indexer::index`/removal; indexing runs async with progress readable via health.
- Path approval reuses the E3-11 canonicalization boundary; no repo outside an approved root is ever indexed via API.

### [ ] E8-05: Config endpoints
Owner: R2 · Estimate: 2d · Depends: E8-02
- `GET /v1/config` returns effective config with sources (from `Config::explain`); `PUT /v1/config` performs validated partial update with atomic file write and rejects unknown keys.
- Concurrent PUTs serialize; a failed validation leaves the file untouched.

### [ ] E8-06: Integration endpoints (two-step plan/approve)
Owner: D1 · Estimate: 3d · Depends: E8-02, E6-05
- `GET /v1/integrations` returns detection + receipt status; `POST /v1/integrations/:client/install` requires `{approve: true, plan_hash}` matching a previously issued plan (two-step flow), uninstall mirrors it.
- Stale `plan_hash` (config changed since planning) is rejected; all writes reuse E6 adapters unchanged.

---

## E9 — Desktop (Tauri) (post-slice, coarse)

Build-doc Phase 5 deliverables. Estimate rollup: 4 wk.

### [ ] E9-01: Tauri shell and local API client
Owner: D1 · Estimate: 4d · Depends: E8-03
- `apps/desktop` Tauri app boots, authenticates to the local API via the api-token file, and renders health/status.
- No direct DB or filesystem access from the webview — all data flows through the local API.

### [ ] E9-02: Onboarding flow
Owner: D1 · Estimate: 4d · Depends: E9-01
- First-run flow: detect assistants, approve first repository, run first index with progress, and confirm MCP registration — completable without terminal knowledge (Phase 5 exit criterion).
- Every step is skippable and resumable; closing mid-onboarding leaves no partial config writes.

### [ ] E9-03: Integration management UI
Owner: D1 · Estimate: 3d · Depends: E9-01, E8-06
- Per-client cards show detection, receipt status, and health; install/uninstall drive the two-step plan/approve endpoints with the key-level diff rendered before approval.
- Uninstall from the UI restores configs and shows the RestoreReport.

### [ ] E9-04: Repository management UI
Owner: D1 · Estimate: 3d · Depends: E9-01, E8-04
- Add/remove repositories, view index status, trigger reindex; staleness and parser-version mismatches surfaced with one-click reindex.
- Excluded/secret-skipped file counts visible per repo.

### [ ] E9-05: Compression Explorer
Owner: D1 · Estimate: 4d · Depends: E9-01
- Browse sessions and operations; per-operation view renders the trace (strategies, safety checks, before/after counts) with zero raw content displayed.
- Savings dashboard uses the §3 vocabulary and labels modeled figures as modeled.

### [ ] E9-06: Diagnostics and local update UI
Owner: D1 · Estimate: 2d · Depends: E9-01
- Doctor checks rendered with remediation actions; log tail viewer reads redacted logs only.
- Update panel shows current/previous version from `update_state` (updater itself is E12).

---

## E10 — Cloud control plane (post-slice, coarse)

Per `03-interfaces.md` §8 and `02-schemas.md` §5; build-doc Phase 6. Estimate rollup: 3 wk.

### [ ] E10-01: Control-plane bootstrap and Postgres schema
Owner: F1 · Estimate: 3d · Depends: E1-01
- Fastify app in `apps/control-plane` with `/v1` prefix, error shape `{error:{code,message,request_id}}`, drizzle schema mirroring 02-schemas §5, migrations in CI.
- CI privacy test proves no column stores source code, prompts, file paths, repo names, symbols, patches, or raw tool args.

### [ ] E10-02: Auth — Better Auth + device authorization flow
Owner: F1 · Estimate: 4d · Depends: E10-01
- Better Auth session cookies for web; `POST /v1/device/authorize` + `/v1/device/token` device-code flow issuing RS256 device JWTs (15-min access, rotating refresh bound to the device keypair).
- `POST /v1/devices` registration and `DELETE /v1/devices/:id` revocation enforced on refresh.

### [ ] E10-03: Organizations, plans, billing
Owner: F1 · Estimate: 4d · Depends: E10-02
- Organizations/members with owner/admin/member roles; Stripe subscriptions with seats via `POST /v1/webhooks/stripe` (signature-verified), plan rows per 02-schemas §5.
- Subscription lifecycle transitions (trialing/active/past_due/canceled) reflected in `/v1/me`.

### [ ] E10-04: Entitlements with offline grace
Owner: F1 · Estimate: 3d · Depends: E10-03
- `GET /v1/entitlements` returns a signed JWS bundle with 30-day offline validity; degraded-but-valid bundles on failure — never a shape that blocks local operation (build-doc §4).
- Local caching contract documented; expiry/grace behavior integration-tested against a clock-skewed client.

### [ ] E10-05: Team policies and feature flags
Owner: F1 · Estimate: 2d · Depends: E10-03
- `GET/PUT /v1/orgs/:id/policies` (signed policy_set JSON, admin-only PUT) and `GET /v1/flags` per device.
- Policy schema covers mode floors, strategy allowlist, privacy floors per 02-schemas §5.

### [ ] E10-06: Admin console and audit logs
Owner: F1 · Estimate: 3d · Depends: E10-03
- `apps/admin` Next.js console for users/orgs/subscriptions/releases; every administrative action writes an `audit_logs` row (append-only enforced).
- Release yank and support-case views included.

### [ ] E10-07: Public site and account app
Owner: F1 · Estimate: 4d · Depends: E10-02
- `apps/web` public site + authenticated account app: signup, purchase, device list, entitlement status — the complete account and purchase flow (Phase 6 exit criterion).
- Cloud outage banner verified: local product docs state and demonstrate independence.

---

## E11 — Provider proxy (post-slice, coarse)

Build-doc §14, Phase 7. Estimate rollup: 2 wk.

### [ ] E11-01: Proxy skeleton, localhost-only, bypass
Owner: R1 · Estimate: 3d · Depends: E2-09
- `compresso-proxy` binds localhost only; `COMPRESSO_BYPASS=1` and request-level bypass headers pass traffic through untouched.
- Body-size, concurrency, and timeout limits enforced; provider API keys never persisted or logged (redaction tested).

### [ ] E11-02: Anthropic Messages adapter
Owner: R1 · Estimate: 3d · Depends: E11-01
- Messages API requests mapped into ContextGraph, compressed in configured mode, system prompts protected by default.
- Unknown fields and headers preserved byte-for-byte; unsupported payload shapes pass through unchanged.

### [ ] E11-03: OpenAI Chat Completions + Responses adapters
Owner: R1 · Estimate: 3d · Depends: E11-01
- Both OpenAI APIs adapted with tool-call payload support; per-provider and per-model policies applied.
- Multimodal parts passed through without inspection.

### [ ] E11-04: Streaming support
Owner: R1 · Estimate: 3d · Depends: E11-02, E11-03
- SSE streaming proxied without whole-response buffering; stream behavior matches direct provider behavior in recorded-fixture tests (Phase 7 exit criterion).
- Mid-stream provider errors propagate unchanged.

### [ ] E11-05: Proxy audit traces and metrics
Owner: R1 · Estimate: 2d · Depends: E11-02, E11-03
- Request-level local audit traces recorded via the operations ledger without storing raw prompts; observed/applied metrics feed `compresso stats`.
- `provider_verified_cost_saved` populated only from provider-reported usage deltas.

---

## E12 — Release engineering (post-slice, coarse)

Build-doc Phases 8–9. Estimate rollup: 3 wk.

### [ ] E12-01: Signed release pipeline
Owner: I1 · Estimate: 4d · Depends: E7-04
- `release.yml` builds, signs, and publishes per-platform binaries with SBOM and provenance attestation; manifest signed and served per `03-interfaces.md` §8 releases endpoint shape.
- Release gates (E7-03) are a hard precondition of publishing.

### [ ] E12-02: macOS notarization and Windows signing
Owner: I1 · Estimate: 3d · Depends: E12-01
- macOS builds notarized and stapled; Windows builds Authenticode-signed; both verified in CI on clean runners.
- Unsigned artifacts cannot reach the stable channel (pipeline enforcement).

### [ ] E12-03: Installers and package managers
Owner: I1 · Estimate: 3d · Depends: E12-01
- `installers/install.sh`, `install.ps1`, and Homebrew formula install to `~/.local/bin/compresso` per D23, idempotently, with checksum verification.
- Clean-machine install → `compresso doctor` green on all three platforms.

### [ ] E12-04: Updater with rollback
Owner: I1 · Estimate: 4d · Depends: E12-01
- `compresso-updater` verifies manifest signatures, applies updates atomically, records `update_state`, and auto-rolls back on failed post-update health checks.
- Signature verification cannot be bypassed silently (release-gate test: tampered manifest refused).

### [ ] E12-05: Threat-model close-out and external security review
Owner: I1 · Estimate: 3d · Depends: E12-02, E12-04
- Security testing matrix from build-doc §25.4 executed (path traversal, symlink escape, malicious MCP args, config injection, release-compromise simulation); findings tracked to closure.
- External or independent review completed with no unresolved critical/high findings (Phase 8 exit criterion).

### [ ] E12-06: Beta, runbooks, and launch
Owner: PQ · Estimate: 3d · Depends: E12-03, E12-05
- Closed beta executed with real coding sessions; incident, rollback, and support runbooks committed under `docs/runbooks/`.
- Production release gates (build-doc §25.6) pass; stable channel opened.

---

## Execution order (flat, dependency-ordered)

Slice: E1-01, E1-02, E1-03, E1-04, E1-05, E2-01, E2-02, E3-01, E2-03, E3-02, E3-03, E2-04, E2-05, E2-06, E2-07, E2-08, E3-04, E3-05, E3-06, E3-07, E3-08, E2-09, E7-01, E2-10, E2-11, E3-09, E7-02, E3-10, E3-11, E3-12, E4-01, E4-02, E4-03, E4-04, E7-03, E7-04, E5-01, E5-02, E5-03, E5-04, E5-05, E5-06, E5-07, E6-01, E6-02, E6-03, E6-04, E6-05.

Post-slice: E8-01, E8-02, E8-03, E8-04, E8-05, E8-06, E9-01, E9-02, E9-03, E9-04, E9-05, E9-06, E10-01, E10-02, E10-03, E10-04, E10-05, E10-06, E10-07, E11-01, E11-02, E11-03, E11-04, E11-05, E12-01, E12-02, E12-03, E12-04, E12-05, E12-06.

Parallelism notes: E2 (R1) and E3 (R2) run concurrently after E1; E7-01/E7-02 (R1/PQ) overlap E2's tail; E4 (D1) starts once E2-09 and E3-12 land; E5 (R2) follows E3; E6 (D1) follows E5-01 and can overlap E5-02..07.

## Estimate rollup (consistent with 00-overview.md)

| Epic | Tickets | Days | Weeks | 00-overview |
|------|---------|------|-------|-------------|
| E1 Foundation | 5 | 7.5 | 1.5 wk | 1.5 wk |
| E2 Core engine | 11 | 16 | ~3 wk | 3 wk |
| E3 Atlas | 12 | 20.5 | ~4 wk | 4 wk |
| E4 CLI | 4 | 5 | 1 wk | 1 wk |
| E5 MCP server | 7 | 10 | 2 wk | 2 wk |
| E6 Integrations | 5 | 10 | 2 wk | 2 wk |
| E7 Eval & fixtures | 4 | 7.5 | 1.5 wk (overlaps E2/E3) | 1.5 wk |
| **Slice total** | **48** | **76.5 gross / ~66 critical-path** | **~13 engineer-wk** | ~13 wk |
| E8 Supervisor & local API | 6 | 15 | 3 wk | per roadmap |
| E9 Desktop (Tauri) | 6 | 20 | 4 wk | per roadmap |
| E10 Cloud control plane | 7 | 23 | ~3 wk (F1 + overlap) | per roadmap |
| E11 Provider proxy | 5 | 14 | ~2 wk | per roadmap |
| E12 Release engineering | 6 | 20 | ~3 wk (I1 + overlap) | per roadmap |

With two engineers on the slice (R1/R2 parallel, D1/PQ overlapping), the slice lands in ≈6–7 calendar weeks, matching `00-overview.md`.
