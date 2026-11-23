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
- `cargo test --workspace --locked --no-fail-fast` step of the `rust` job in `.github/workflows/ci.yml` (a CI step, not a named test): CI step `cargo build --workspace` exits 0.
- test_no_internal_deps_in_core: parse `rust/compresso-core/Cargo.toml`, assert no `compresso-*` entries under `[dependencies]`.
- test_store_never_imports_atlas: parse `rust/compresso-store/Cargo.toml`, assert `compresso-atlas` absent.

### [x] E1-02: CI workflow
Owner: I1 · Estimate: 2d · Depends: E1-01
Files: .github/workflows/ci.yml, .github/workflows/release.yml (stub)
Description: Author `ci.yml` per `01-stack-and-workspace.md` §4: rust job matrix (`macos-15`, `macos-15-intel`, `ubuntu-24.04`, `windows-2022`) running `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `cargo build --release -p compresso-cli`; `Swatinem/rust-cache@v2`; release-binary artifact upload on main. The `eval` job is added in E7-04; the `ts` job is gated off until Phase 6. `release.yml` is a named no-op stub until Phase 8 (E12).
Acceptance criteria:
- CI triggers on push to main and on PRs; all four matrix legs green on the scaffold commit.
- Clippy runs with `-D warnings` and formatting is enforced via `cargo fmt --check`.
- Release binary for each platform uploaded as an artifact on main pushes.
Test cases:
- `Lint CI workflow` actionlint step of the `Workflow lint` job in `.github/workflows/ci.yml` (a CI step, not a named test): `actionlint .github/workflows/ci.yml` exits 0 (run locally and as a CI step).
- `scripts/ci/check_ci_matrix.rb` (CI step `Assert required runner matrix`, not a named test): assert ci.yml matrix contains macos-15, macos-15-intel, ubuntu-24.04, windows-2022 (yq assertion in a repo lint script).

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
Description: Add a `package` CI job on all four D33 matrix legs producing a per-platform tarball `compresso-<version>-<target>.tar.gz` (targets: aarch64-apple-darwin, x86_64-apple-darwin, x86_64-unknown-linux-gnu, x86_64-pc-windows-msvc) containing exactly the release `compresso` binary (`compresso.exe` on Windows), plus a single aggregated `SHA256SUMS` file in `shasum -a 256 -c`-compatible format covering every tarball. Artifacts uploaded on main pushes; consumed by `installers/install-local.sh` (E4-05). No signing — D42 keeps the slice installer checksum-only; signing/notarization stay E12. (Since ADR-0030 the `checksums` job also keyless-signs `SHA256SUMS`.)
Acceptance criteria:
- A main push produces four tarball artifacts plus one `SHA256SUMS`; `shasum -a 256 -c SHA256SUMS` passes against the downloaded artifacts.
- Each tarball contains exactly one entry (the binary); no fixtures, docs, debug symbols, or source maps (tar listing asserted in the packaging script's self-test).
- `scripts/package-tarball.sh` is deterministic for a given binary (stable tar metadata: sorted entries, fixed mtime, numeric owners).
Test cases:
- `scripts/package-tarball.sh --self-test` (CI step `Test deterministic release packaging`, not a named test): run package-tarball.sh against a stub binary → `tar -tzf` lists exactly `compresso`.
- `scripts/package-tarball.sh --self-test` (same self-test): generated SHA256SUMS verifies with `shasum -a 256 -c`; flipping one byte in a tarball makes verification fail.
- `scripts/package-tarball.sh --self-test` (same self-test): two runs on the same stub binary produce byte-identical tarballs.

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
- test_assistant_reasoning_cannot_be_unprotected, test_force_ingress_protection_for_all_protected_kinds: `assistant_reasoning` with `protected: false` fails validation; ingress forces it protected and records the note (D53).
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
- test_equal_ratio_tiebreaks_by_id: two identical-score nodes "n_a" and "n_b" → "n_a" dropped first on two consecutive runs (assert equality of both outputs).

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
- P95 across 100 iterations of each fixture < 200 ms on CI hardware (macos-15 and ubuntu-24.04 legs).
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
- test_wal_and_fk_enabled: `PRAGMA busy_timeout` == 5000 after open.
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
- test_d47_tool_classification_and_shape: `tool = "compresso_compress"` with `trace_json = None` → `Err(StoreError::MissingTrace)`, no row written.
- test_record_operation_persists_strategy_results: valid trace with 2 strategy_results → 1 `operations` row + 2 `strategy_results` rows keyed by operation_id.
- test_stats_vocabulary_math_and_since_filter: two ops (observed 1000/applied 700/overhead 50, observed 500/applied 500/overhead 0, second with fallback_used=1) → `observed_tokens == 1500`, `applied_tokens == 1200`, `removed_tokens == 300`, `net_tokens_saved == 250`.
- test_stats_vocabulary_math_and_since_filter: op at t=100 and t=200; `stats(Some(150))` counts only the second.
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
- test_secret_patterns_skipped_without_opening: fixture containing `.env.local`, `server.pem`, `id_rsa` → all three in skipped with `SkipReason::SecretPattern`.
- test_binary_and_oversize_skipped_before_oversize_open: a 3 MB file with max_file_bytes=1 MB and a PNG → skipped with `SkipReason::TooLarge` / `SkipReason::Binary`.
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
- test_tsx_component_extracted_with_distinct_dialect: `sample.tsx` with `export function UserCard(props: { name: string }): JSX.Element` → symbol `UserCard`, kind `function`, signature containing `props: { name: string }`.
- test_type_alias_enum_and_exported_const_kinds: `export type UserId = string;` → symbol `UserId`, kind `type`.

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
- test_js_cjs_and_esm_imports_in_source_order: `const path = require("path");` plus `import express from "express";` → imports `["path", "express"]`.
- test_js_dynamic_and_noncanonical_require_ignored: `require(pluginName)` → not present in imports, extraction succeeds.

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
- test_py_imports_three_forms_and_multiple_names: `import os`, `import numpy as np`, `from typing import Optional` → imports `["os", "numpy", "typing.Optional"]`.
- test_py_decorated_line_range_and_signature: `@retry` on the line above `def push` → symbol start_line is the `def` line and signature notes the decorator.

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
- test_parser_version_bump_flags_stale_and_reparses_all: stamp index_versions with "0.0.1", run indexer built as "0.0.2" → `stale == true` and all files reparsed.
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
Description: Implement `impact(store, repo, path)` (reverse-edge closure over `edges`, depth 2, reporting dependent files and the linking edge kinds) and `plan_context(store, repo, task, budget_tokens)` (search-driven pack: focused map plus compact, file-diverse matching evidence excerpts assembled within the token budget) per `03-interfaces.md` §4.
Acceptance criteria:
- `impact` includes direct importers (depth 1) and their importers (depth 2), never depth 3; each entry names the edge kind; self is excluded.
- `plan_context` output token count ≤ `budget_tokens`; sections are ordered focused map → evidence excerpts, broad packs prioritize distinct files before a second symbol from one file, and every excerpt's symbol matches at least one deterministic FTS query derived from the natural-language task.
- Both functions read-only and deterministic for a fixed store state.
Test cases:
- test_impact_depth_two: fixture edges `a.rs → b.rs → c.rs → d.rs` (imports); `impact(.., "c.rs")` → contains b.rs (depth 1) and a.rs (depth 2); wait — reverse edges: importers of c.rs are b.rs, importers of b.rs are a.rs; d.rs (which c imports) absent.
- test_impact_edge_kinds_reported: tests edge `tests/a_test.rs → a.rs (tests)` → impact of a.rs lists a_test.rs with kind `tests`.
- test_plan_context_within_budget_and_section_order: `plan_context(.., "session retry logic", 800)` on fixture → `counter.count(rendered) <= 800` and includes the `SessionManager` outline.
- test_plan_context_expands_natural_language_task_and_prioritizes_relevant_symbols: one 800-token authentication task recalls OAuth, credentials, login/session/password, and browser-cookie symbols without generic token-processing false positives.
- test_plan_context_budget_tight_values_and_deterministic: two calls with identical inputs → identical rendered output.

### [x] E3-13: Safe shell and generic-text indexing
Owner: R2 · Estimate: 1d · Depends: E3-04, E3-09, E3-12
Files: rust/compresso-store/src/{files,symbols}.rs, rust/compresso-atlas/src/parsers/{shell,text}.rs, rust/compresso-atlas/src/{parsers,indexer,queries}.rs, tests in rust/compresso-atlas/tests/{parsers_shell_text,indexer_shell_text}.rs, rust/compresso-mcp/tests/tools_read.rs, docs/adr/0026-safe-shell-and-generic-text-indexing.md
Description: Extend the parser registry with conservative shell (`.sh`, `.bash`, `.zsh`) and inert documentation/plain-text (`.md`, `.markdown`, `.txt`, `.rst`, `.adoc`) packs. Shell extraction is lexical only: it emits one searchable script summary plus bounded function definitions and never invokes a shell, expands a variable, follows a reference, or reads another file. Generic text emits bounded heading sections or paragraphs and never interprets markup directives, links, includes, or code blocks. Existing approval, canonical-root, ignore, secret-path, binary, UTF-8, and one-million-byte scanner boundaries apply before persistence. Config/data formats remain unsupported. A parser fingerprint change forces one deterministic re-index of existing repositories.
Acceptance criteria:
- Shell scripts are searchable by descriptive comments, function names, and assignment identifiers without indexing assignment values into the script-summary signature; function bodies remain available only through the existing bounded read/unfold path.
- Generic text produces at most 256 deterministic records per file, each covering at most 40 lines with a UTF-8-safe signature of at most 1,024 bytes.
- Context planning prefers implementation records when a concept matches both implementation and generic prose; text joins a mixed evidence pack only when the task explicitly asks for documentation/design/specification evidence, and remains the fallback when no implementation record matches.
- Extracting hostile or malformed shell source performs no process or filesystem action, never panics, and returns at most one script record plus 256 function records.
- `.env*` and every existing secret-path pattern remain excluded before content inspection; unsupported config formats produce no FTS records.
- Re-indexing a repository created with the former package-only parser version marks it stale once, adds the new records, then returns to incremental no-op behavior.
Test cases:
- test_shell_extracts_script_summary_and_functions_without_execution.
- test_shell_malformed_is_partial_deterministic_and_bounded.
- test_generic_text_extracts_searchable_bounded_sections.
- test_generic_text_plain_paragraphs_and_resource_caps.
- test_shell_and_generic_text_index_incrementally_with_secret_paths_excluded.
- test_context_prefers_shell_evidence_over_duplicate_documentation_mentions.
- test_context_tool_prefers_shell_implementation_and_keeps_text_opt_in.
- test_shell_text_parser_fingerprint_forces_one_time_reindex.

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
- test_compress_log_fixture_via_stdin_records_valid_trace: pipe fixtures/logs/repeated-npm-install/input.txt → JSON `after_tokens < before_tokens`, stdout content contains `"[×500"`.
- test_passthrough_stdin_is_byte_exact: `--mode passthrough` with arbitrary UTF-8 input → stdout equals stdin.
- test_explain_last_and_id_return_typed_trace_without_raw_content, test_operation_lookup_revalidates_persisted_trace: after a compress, `explain --last --json` → `data.trace` validates against schemas/compression-trace/v1.json and contains no input substring.
- test_stats_json_uses_exact_savings_vocabulary_and_human_labels_modeled: `stats --json` → keys include `net_tokens_saved` and `modeled_cost_saved`; human mode output contains the word "modeled".

### [x] E4-04: doctor and config explain
Owner: D1 · Estimate: 1d · Depends: E4-01, E2-09
Files: rust/compresso-cli/src/commands/doctor.rs, rust/compresso-core/src/config.rs, tests in both
Description: Implement `Config::load()` (default < file < env < flag per `03-interfaces.md` §2.6, file at `~/.config/compresso/config.toml`, env overrides `COMPRESSO_MODE`, `COMPRESSO_TARGET_TOKENS`, `COMPRESSO_BYPASS`, `COMPRESSO_LOG`, `COMPRESSO_TELEMETRY` accepted but forced false) and `Config::explain()` backing `compresso config explain` (key, effective value, `ConfigSource`). `compresso doctor` checks: binary resolvable, data dir writable, DB opens and migrations current, config parses, tokenizers initialize, tree-sitter grammars load, and prints pass/fail per check with remediation hints.
Acceptance criteria:
- `config explain` lists every config key with source `default|file|env|flag`; setting `COMPRESSO_MODE=passthrough` flips the `compression.mode` row to source `env`.
- `COMPRESSO_TELEMETRY=true` is accepted, the effective value remains `false`, and explain annotates it "forced off (D6)".
- `doctor --json` returns a check array; any failure sets exit 1; a corrupted config file yields a failing `config_parses` check, not a crash.
Test cases:
- test_config_precedence_env_over_file_and_flag_over_env: file sets mode=safe, env COMPRESSO_MODE=passthrough → effective passthrough with source Env.
- test_telemetry_forced_false_and_annotated: COMPRESSO_TELEMETRY=true → `Config::load().privacy.telemetry == false`.
- test_doctor_reports_all_checks_green_for_healthy_runtime: doctor with temp HOME and fresh db → exit 0, all checks `passed == true`.
- test_doctor_bad_config_fails_gracefully_with_path_hint: config.toml containing `mode = [` → exit 1, JSON check `config_parses.passed == false`, hint names the file path.

### [x] E4-05: install-local.sh slice installer (D42)
Owner: I1 · Estimate: 1.5d · Depends: E1-06, E4-04
Files: installers/install-local.sh, installers/tests/install_local_test.sh, docs/runbooks/install-local.md
Description: POSIX-sh installer that (1) detects platform/arch, (2) locates the matching `compresso-<version>-<target>.tar.gz` and `SHA256SUMS` from a local path or URL argument, (3) verifies the SHA256 checksum before unpacking, (4) installs atomically to `~/.local/bin/compresso` (temp file + rename, mode 0755, per D23), (5) is idempotent — a re-run replaces the binary and keeps the prior one as `compresso.previous`, and (6) prints PATH guidance when `~/.local/bin` is not on PATH. Unsigned per D42 (signing/notarization/package managers stay E12; since ADR-0030 the installer also verifies the keyless signature on `SHA256SUMS`); conforms to the 08 §3.4 bootstrap constraints: small enough to read in one sitting, no telemetry, no assistant-config mutation, no mutable second-stage code.
Acceptance criteria:
- Clean-machine script run installs a working binary: on a fresh temp `$HOME`, `~/.local/bin/compresso --version` prints the packaged version and `compresso doctor` executes its checks.
- A checksum mismatch aborts before any write under `~/.local/bin`; the error names the expected and actual digests.
- Re-run is idempotent; the previous binary is preserved as `compresso.previous`; the installer never touches `~/.config/compresso` or any assistant config.
Test cases:
- test_install_clean_home: temp HOME + E1-06 fixture tarball → binary at `~/.local/bin/compresso`, executable, `--version` matches the tarball version.
- test_checksum_mismatch_aborts_before_install_write: flip one byte in the tarball → non-zero exit, stderr contains `checksum`, `~/.local/bin` unchanged.
- test_install_idempotent_with_previous_backup: run twice → working binary plus `compresso.previous` present.
- test_no_config_or_assistant_writes: directory hash of `~/.config` before/after → identical.

---

## E5 — MCP server

### [x] E5-01: rmcp server bootstrap and initialize handshake
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

### [x] E5-02: Navigation handlers - repo map, outline, selector-based unfold
Owner: R2 · Estimate: 1.5d · Depends: E5-01, E3-10, E3-11
Files: rust/compresso-mcp/src/tools/atlas_nav.rs, tests in rust/compresso-mcp/tests/tools_nav.rs
Description: Implement the three navigation handlers against the exact JSON schemas in 04 §1. Root naming, defaults, bounds, SymbolSelector, ambiguity, and errors must match those schemas.
Acceptance criteria:
- Repo map and outline preserve their bounded contracts.
- Unfold passes the complete selector to E3-11 and returns found/ambiguous/not_found as structured success; ambiguous candidates are capped at 20 and deterministic.
- Invalid arguments and root violations return stable MCP errors; stale unique matches return current content with stale=true.
Test cases:
- `test_repo_map_budget`, `test_outline_matches_atlas`, `test_unfold_unique`, `test_unfold_ambiguous_candidates`, `test_unfold_signature_disambiguates`, `test_unfold_stale_current`, `test_unfold_deleted_not_found`, `test_unfold_schema_bounds`.

### [x] E5-03: Tool handlers — compresso_search, compresso_read (gate mode), compresso_context
Owner: R2 · Estimate: 1.5d · Depends: E5-01, E3-11, E3-12
Files: rust/compresso-mcp/src/tools/atlas_read.rs, tests in rust/compresso-mcp/tests/tools_read.rs
Description: Implement `compresso_search` (→ `search`), `compresso_read` (→ `read_bounded`, plus a `gate: true` argument implementing the predecessor `context_gate` tool's semantics per D7: return size/token estimate and a worth-reading verdict without file content), and `compresso_context` (→ `plan_context`). All path arguments flow through the E3-11 canonicalization boundary; `OutsideRoot` maps to a stable MCP error code and never echoes the resolved path outside the root.
Acceptance criteria:
- `compresso_read` with `gate: true` returns the 04 §1.5 gate object (`worth_reading`, `reason`, `file_tokens_total`, `recommended_budget`, `cheaper_alternative`) only — response contains zero file-content bytes.
- Traversal/symlink-escape inputs return error code `outside_root`; repository content in responses is data-only (no tool-side execution of anything found in files).
- `compresso_read` clamps an `end_line` beyond EOF, reports returned and resolved ranges plus total file lines, and keeps starts beyond EOF invalid.
- `compresso_context` treats `max_tokens` as a caller ceiling up to 4000, starts at 800 serialized-response tokens, expands through 1600 and 3200 only while ranked evidence coverage is insufficient, clamps oversized stale-client requests to 4000, distinguishes task-pack-ready, targeted-follow-up, and no-match/stale broad-follow-up results, and requires evidence from every distinct candidate file in the bounded ranked result before declaring the pack ready.
Test cases:
- test_read_tool_bounded: read `src/sample.rs` with `max_tokens: 100` → ≤100 tokens, `truncated: true` in structured result.
- test_read_end_line_beyond_eof_clamps_and_reports_resolved_range: read a 73-line file with `start_line:1,end_line:240` → success, `resolved_range.end_line == 73`, `file_lines_total == 73`, `range_clamped:true`.
- test_read_gate_mode_no_content: `{"path":"src/sample.rs","gate":true}` → response includes `gate.worth_reading`, `gate.file_tokens_total`, and `gate.recommended_budget` and omits every line of the file (assert a known source line absent).
- test_read_dotdot_via_mcp: `{"path":"../../etc/passwd"}` → MCP error `outside_root`.
- test_search_tool_hits: `{"query":"SessionManager"}` → first hit qualified_name `SessionManager` with path and line.
- test_context_tool_budget_and_sections: `{"task":"session retry logic","max_tokens":800}` → ≤800 tokens.
- test_context_tool_adaptively_expands_to_smallest_sufficient_budget: a twenty-file authentication corpus with `max_tokens:4000` → expands through declared stages, stops at the first stage with complete ranked-file evidence coverage, and returns a complete serialized payload within `budget_applied`.
- test_context_tool_respects_explicit_800_token_ceiling: the same corpus with `max_tokens:800` → no expansion, `targeted_follow_up_needed`, and complete serialized payload ≤800 tokens.
- test_context_tool_clamps_oversized_ceiling_but_stops_at_smallest_ready_pack: `{"task":"session retry logic","max_tokens":10000}` → successful response with `budget_ceiling:4000`, `budget_clamped:true`, while `budget_applied` remains the smallest sufficient stage and the complete serialized payload stays within it.

### [x] E5-04: Tool handlers — compresso_compress, compresso_impact, compresso_stats, compresso_explain
Owner: R2 · Estimate: 1.5d · Depends: E5-01, E2-09, E3-03, E3-12
Files: rust/compresso-mcp/src/tools/engine_tools.rs, tests in rust/compresso-mcp/tests/tools_engine.rs
Description: `compresso_compress` builds a graph from the `text` argument (D7: compatible with the predecessor `compress_text` tool), runs `Engine::compress` in the configured mode, returns compressed text plus real-tokenizer before/after counts; `compresso_impact` → `impact`; `compresso_stats` → `Store::stats` in the §3 vocabulary; `compresso_explain` returns the stored trace for an `operation_id`. Operation recording follows D47 (02-schemas §4, 04 §1.1): `compresso_compress` records a compression operation whose `trace_json` is the compression-trace v1 document validated against the **root** schema; the retrieval handlers (`compresso_impact`, `compresso_stats`, `compresso_explain`, plus the E5-02/E5-03 navigation and read tools) record `trace_json = NULL` with `tokens_served` set to the tokens returned to the client (E5-05 supplies the session id plumbing).
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

### [x] E5-05: Sessions and operation ledger
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

### [x] E5-06: MCP startup and eager tokenizer lifecycle
Owner: R2 · Estimate: 1.5d · Depends: E5-01, E2-02
Files: rust/compresso-mcp/src/server.rs, rust/compresso-core/src/tokens.rs, benches/startup.rs
Description: Initialize configured tokenizers before accepting tools; separate cold startup from warm calls.
Acceptance criteria:
- Initialize once under 500ms; warm P95 under 200ms; failure keeps passthrough MCP alive.
Test cases:
- pre-initialize construction; no handler construction; cold/warm; failure; EOF.

### [x] E5-07: Manual verification in Claude Code
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

### [x] E6-01: Integration trait, base-bound plans, and receipts
Owner: D1 · Estimate: 2d · Depends: E5-01
Files: rust/compresso-integrations/src/{lib,plan,receipt}.rs, schemas/integrations/v1.json
Description: Define detect and plan/apply install/uninstall with full-file base/result hashes, a secret-safe owned-key preview, private exact target, expiry, session, and backup.
Acceptance criteria:
- Rehash before writes; drift/forge/expiry/session reject; atomic receipts; owned-key uninstall; idempotent. Human/JSON previews never serialize exact target bytes or unrelated configuration and structurally redact unknown fields inside an owned value.
Test cases:
- outside-diff rewrite; forged/expired; crash; unknown keys; secret canary absent from preview and Debug output.

### [x] E6-02: Claude Code adapter
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
- test_project_scope_pins_and_verifies_exact_canonical_root, test_project_scope_is_explicit_and_does_not_touch_user_config: `Scope::Project(dir)` → `.mcp.json` created in dir with the compresso entry, user config untouched.

### [x] E6-03: Codex integration adapter
Owner: D1 · Estimate: 2d · Depends: E6-01
Files: rust/compresso-integrations/src/codex.rs
Description: Implement Codex CLI/Desktop without Claude assumptions.
Acceptance criteria:
- Base-bound owned-key lifecycle and typed unsupported no-op.
Test cases:
- fresh/existing/malformed; drift; disable/uninstall/repeat; platforms; unrelated authorization header absent from minimal preview.

### [x] E6-04: Cursor adapter
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
- test_cursor_project_scope_pins_and_verifies_exact_canonical_root: `Scope::Project` install → project file has entry, user file byte-unchanged.

### [x] E6-05: Integration parity and restoration suite
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

### [x] E7-03: Release gates file and gate checks
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

### [x] E7-04: Eval CI integration
Owner: I1 · Estimate: 1.5d · Depends: E7-03, E1-02
Files: .github/workflows/ci.yml (eval job)
Description: Add the `eval` CI job per `01-stack` §4 on ubuntu-24.04 and macos-15: build release, run `cargo run -p compresso-eval --release -- run fixtures/ --gates docs/plan/gates.toml`, upload the JSON report as an artifact, and fail the workflow on any gate regression. Latency-sensitive gates run only on the macos-15 leg (dedicated hardware class) with the ubuntu leg enforcing the non-latency gates, to keep the P95 gate meaningful.
Acceptance criteria:
- Eval job green on main with the committed corpus; report artifact downloadable per run.
- A deliberate gate regression on a scratch branch (threshold temporarily raised to 0.99) fails the job with the gate name in the log, verified once and documented in the PR.
- Job runs after the rust build job and reuses its cache (no duplicate cold compile).
Test cases:
- `Lint CI workflow` actionlint step in `.github/workflows/ci.yml` and `scripts/ci/check_ci_matrix.rb` (CI checks, not named tests): actionlint passes; ci.yml contains the eval invocation string with `--gates docs/plan/gates.toml`.
- test_gate_regression_fails_ci (no covering test found — see review): scratch-branch verification recorded in the PR description with the failing run link.

Evidence:

- Green `main` run: https://github.com/yogi100x/compresso/actions/runs/30101524537
- Deliberate `0.99` scratch regression: PR https://github.com/yogi100x/compresso/pull/8 was closed unmerged after both Eval legs failed with `gate_regression` naming `median_reduction_eligible` (Ubuntu job https://github.com/yogi100x/compresso/actions/runs/30103553223/job/89517646125; macOS job https://github.com/yogi100x/compresso/actions/runs/30103553223/job/89517646193).

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

### [x] E7-06: Recall gates in gates.toml
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

## PB — Public package-manager beta

Authorized by D67 and ADR-0025 as a bounded lane after the E1-E7 slice. PB
pulls only the named beta release-hardening/signing subsets from E12/E14; it
does not authorize E8-E16, licensing, cloud, desktop, updater, Homebrew, or
stable publication. The private engine repository remains private. The public
distribution repository contains auditable packaging code and verified signed
native artifacts only. Epic estimate: 16.5d.

### [x] PB-01: Public beta distribution decision and contracts
Owner: PQ+I1 · Estimate: 0.5d · Depends: none
Files: AGENTS.md, decisions.md, docs/adr/0021-crown-jewel-local-boundary.md, docs/adr/0025-public-package-manager-beta.md, docs/plan/04-mcp-and-integrations.md, docs/plan/05-tickets.md, docs/plan/08-ip-and-secure-deployment.md
Description: Lock the public package-manager beta boundary and its real release blockers. A session root is the immutable canonical approved repository bound when an MCP server starts, never launcher scratch space. The launcher creates no mutable or temporary state. Define honest channel trust, binary recoverability, the exact E12/E14 subsets pulled forward, and explicit exclusions. D36 remains open and the stable npm tag remains blocked.
Acceptance criteria:
- ADR-0025, D67, AGENTS.md, and the MCP contract agree that a session root comes only from explicit `--root` or approved cwd, has no sole-approved-repository fallback, and cannot be changed by a tool call.
- The decision names repository-bound MCP behavior, truthful provider disclosure, secret exclusions, repository management, `compresso setup CLIENT ROOT`, and the stable `COMPRESSO_LAUNCHER_PATH` upgrade contract as beta blockers owned by PB-02/PB-03.
- Trust text states that signatures authenticate private-to-public import and enable independent verification, launcher SHA-256 is tamper/corruption detection rather than an unpatchable trust anchor, and full npm/package compromise can replace launcher, binary, and checks.
- D46, ADR-0021, and 08 §2 retain the same honest recoverability language; D36 remains OPEN.
- PB-02..PB-08 are dependency ordered, total 16.5d, and every beta acceptance gate has an owner.
Test cases:
- `test_pb01_cross_references_resolve` (no covering test found — see review).
- `test_pb01_session_root_and_launcher_state_contracts_do_not_conflate` (no covering test found — see review).
- `test_pb01_trust_claims_include_full_npm_compromise` (no covering test found — see review).
- `test_pb01_scope_scan_and_d36_remains_open` (no covering test found — see review).

### [x] PB-02: Repository-bound MCP and secret-safe disclosure
Owner: R2+PQ · Estimate: 3d · Depends: PB-01, E6-05
Files: rust/compresso-cli/src/commands/mcp.rs, rust/compresso-mcp/src/server.rs, rust/compresso-atlas/src/scanner.rs, rust/compresso-store/src/, docs/plan/03-interfaces.md, docs/plan/04-mcp-and-integrations.md
Description: Make one canonical approved repository the immutable boundary for each MCP process. Bind at startup from explicit `mcp serve --root ROOT` or the canonical cwd's longest approved ancestor; never fall back merely because one repository is approved. Tool roots may only confirm the bound root. Keep provider disclosure truthful and exclude documented secret classes before read, hash, tokenization, parsing, persistence, traces, search, or MCP output.
Acceptance criteria:
- Explicit root must be an exact approved canonical repository; cwd binding uses the longest approved ancestor. With neither, repository-independent compression/stats/explain remain available while repository tools fail with an actionable content-free error; a tool call cannot bind the session.
- A tool cannot select a different repository, including another approved repository, and symlink/path aliases cannot escape the bound root.
- Server instructions state that Compresso makes no independent network request while the coding client may send returned MCP content to its configured provider.
- At minimum `.env*`, `*.pem`, and `id_rsa*` are excluded before file contents are opened or indexed and cannot appear in files, symbols, FTS, traces, or MCP results.
Test cases:
- `test_explicit_root_rejects_unapproved_and_invalid_directories`, `test_cwd_binding_selects_longest_approved_ancestor`, `test_omitted_root_does_not_select_sole_approved_repository`.
- `test_session_bound_to_repo_a_cannot_access_approved_repo_b`, `test_bound_root_retarget_cannot_leak_another_approved_repository`.
- `test_instructions_word_budget`.
- `test_secret_patterns_skipped_without_opening`, `test_secret_files_never_reach_store_or_fts`, `test_secret_pattern_content_never_reaches_mcp`.

### [x] PB-03: Repository lifecycle and one-command client setup
Owner: D1+R2 · Estimate: 2.5d · Depends: PB-02, E6-05
Files: rust/compresso-cli/src/commands/, rust/compresso-store/src/, rust/compresso-integrations/src/, docs/plan/03-interfaces.md, docs/plan/04-mcp-and-integrations.md
Description: Add deterministic `compresso repos list`, safe `compresso repos remove ROOT`, and `compresso setup CLIENT ROOT`. Setup canonicalizes and indexes the repository, then installs client-appropriate MCP configuration. A global Codex entry omits `--root` so each Codex session binds from its cwd's longest approved ancestor; project-scoped Claude Code/Cursor entries may pass `mcp serve --root CANONICAL_ROOT`. Public launchers export a validated stable `COMPRESSO_LAUNCHER_PATH`; integrations record that normalized absolute lexical package-manager shim, not a resolved/versioned engine path, so upgrades require no client rewrite.
Acceptance criteria:
- Repositories list deterministically in text and JSON; remove previews or explicitly confirms removal of Compresso approval/index metadata and never deletes repository files.
- Setup dry-run mutates nothing and shows index/approval plus exact Compresso-owned client-config changes through the secret-safe public preview; it never returns private target bytes or unrelated configuration. Real setup preserves backup, receipt, idempotence, and reversible uninstall behavior.
- Global Codex configuration contains no fixed `--root` and is tested from two approved cwd sessions; each binds its own longest approved ancestor. Project-scoped Claude Code/Cursor configuration may include the exact canonical root, and no tool can switch the resulting session.
- `COMPRESSO_LAUNCHER_PATH` is normalized to an absolute lexical path without resolving away the final shim symlink; its resolved target must be an existing executable regular file. Relative, missing, non-regular, or non-executable targets fail closed, while expected npm/pnpm symlinks remain valid. Direct engine use retains a documented safe fallback.
- Upgrading package contents beneath the same launcher leaves installed client configuration byte-identical and still invokes the new engine.
Test cases:
- `test_repos_list_is_deterministic_in_human_and_json_modes`, `test_remove_repo_dry_run_is_write_free_and_never_prefix_matches`, `test_remove_repo_requires_confirmation_and_cascades_when_root_is_missing`, `test_remove_repo_human_confirmation_can_decline_or_approve`.
- `test_setup_dry_run_renders_both_plans_without_any_writes`, `test_setup_real_install_is_idempotent_and_uninstall_keeps_repository_indexed`.
- `test_global_codex_setup_binds_each_launched_session_from_its_cwd`.
- `test_setup_project_clients_pin_exact_canonical_root`.
- `test_stable_launcher_path_is_not_canonicalized`, `test_stable_launcher_normalizes_dot_and_duplicate_separators_without_resolving_shim`, `test_stable_launcher_rejects_relative_missing_and_directory_paths`, `test_setup_passes_normalized_stable_launcher_path_to_client_config`.
- `test_package_upgrade_preserves_client_config_and_uses_new_engine`.
- `test_setup_codex_dry_run_redacts_unrelated_authorization_and_target_document`.

### [ ] PB-04: Private signed beta artifact pipeline
Owner: I1+PQ · Estimate: 3d · Depends: PB-03, E7-04
Files: Cargo.toml, .github/workflows/package-beta.yml, scripts/release/beta/, schemas/release-manifest/beta-v1.json
Description: Build the final E1-E7 plus PB-02/PB-03 engine for `aarch64-apple-darwin`, `x86_64-apple-darwin`, and `x86_64-unknown-linux-gnu`. Pull forward only the required E12/E14 subsets: hardened dist build with private split symbols, exact artifact allowlist and secret scan, immutable artifacts, signed versioned manifest, SBOM, and private-workflow provenance. Private symbols, source, signing material, prompts, fixtures, and corpora never enter the public payload.
Acceptance criteria:
- Exactly three immutable artifacts are emitted; the signed manifest binds version, target, SHA-256, size, immutable identity, and provenance identity.
- Public verification material accepts authentic artifacts and rejects mutation, wrong target/version/provenance, extra files, and an untrusted signer.
- Release blocks on E1-E7/PB tests, E7 gates, payload allowlist, secret scan, signing, manifest validation, SBOM, or provenance failure.
- Native macOS artifacts receive required OS signing; Linux retains independently verifiable signatures without claiming automatic OS enforcement.
Test cases:
- `test_beta_matrix_exact_three_targets`.
- `test_beta_manifest_signature_digest_and_provenance_reject_tamper`.
- `test_beta_artifact_allowlist_rejects_each_private_class`.
- `test_beta_symbols_private_and_native_signing_policy`.

### [ ] PB-05: Single package assembly and stateless launcher
Owner: D1+I1 · Estimate: 2d · Depends: PB-03, PB-04
Files: separate public distribution repository bin/compresso, package.json, package payload allowlist, launcher and npm pack tests
Description: Assemble one `@compresso/cli` beta package with a readable POSIX launcher and the three verified native engines. The launcher self-locates through npm/pnpm layouts, selects one exact target, checks bundled SHA-256 for corruption/tampering, exports its normalized absolute lexical shim as `COMPRESSO_LAUNCHER_PATH` without resolving away the final package-manager symlink, and directly execs the engine. It creates no files, temporary directories, cleanup traps, or session state. The package has no lifecycle script, runtime dependency, or runtime Node.js requirement.
Acceptance criteria:
- Launcher behavior is identical from arbitrary cwd, project install, npm global symlink, and pnpm symlink layouts; unsupported OS/architecture executes nothing and reports exact supported targets.
- A filesystem trace proves the launcher performs no write/mkdir/delete operation and direct exec preserves cwd, argv, streams, signals, and exit status so cwd-derived repository binding remains intact.
- `npm pack --dry-run` contains exactly the launcher, three artifacts, signed manifest/evidence, public trust material, SBOM/provenance references, notices, and docs; private or unlisted material is absent.
- Package metadata has no install/preinstall/postinstall/prepare equivalent or runtime dependency; installed execution works without Node/npm/pnpm on PATH.
Test cases:
- `test_launcher_self_location_exact_target_and_unsupported_refusal`.
- `test_launcher_is_stateless_and_exec_transparent`.
- `test_launcher_sha256_detects_corruption_without_claiming_signature_enforcement`.
- `test_npm_pack_allowlist_no_lifecycle_and_node_free_runtime`.

### [ ] PB-06: Public import and independent npm publication trust
Owner: I1+PQ · Estimate: 2d · Depends: PB-04, PB-05
Files: separate public distribution repository import and publish workflows, verification scripts, trust tests, docs/runbooks/public-beta-import.md
Description: Import the immutable private release into a separate packaging-only public repository only after independent signature, digest, platform/version, and provenance verification. Prepare a protected publication pipeline with staged approval, native OS signing evidence, npm trusted publishing through OIDC, and npm provenance; use no long-lived npm token. Preserve signatures for external/manual verification while stating clearly that full npm/package compromise can replace launcher, engine, and bundled checks, especially on Linux. PB-08 alone authorizes the registry write.
Acceptance criteria:
- Mutable, unsigned, wrong-signer, digest/platform/version/provenance-mismatched, or extra-file imports cannot reach assembly or publication.
- Protected review is recorded for import, launcher, trust-root, and workflow changes; trust-root changes cannot hide inside artifact import.
- Public checkout records immutable source reference, manifest/artifact digests, signer identity, provenance, reviewer, SBOM, and exact package composition without private material.
- The publication workflow uses OIDC/provenance and staged approval with no long-lived token; native OS signing is verified where available, and pipeline validation performs no registry mutation.
- Verification docs distinguish independent import/manual/OS signatures from launcher SHA-256 and acknowledge that neither prevents a fully compromised package channel from replacing its own checks.
Test cases:
- `test_public_import_accepts_only_pinned_signed_release`.
- `test_public_repo_private_material_scan_and_protected_review`.
- `test_publish_uses_oidc_provenance_staged_approval_and_native_signing`.
- `test_trust_claims_cover_full_registry_package_compromise`.

### [ ] PB-07: Truthful beta, provider, platform, and IP disclosure
Owner: PQ+D1 · Estimate: 1d · Depends: PB-02, PB-03, PB-05, PB-06
Files: separate public distribution repository README.md, package metadata, docs/beta/, CLI/help snapshot fixtures
Description: Align package, README, CLI/help, doctor, setup, verification, and uninstall disclosures. Name the proprietary recoverable engine, exact targets, immutable repository session-root behavior, stable launcher upgrade path, persistent data, secret exclusions, provider data path, channel trust and limitations, and beta exclusions. Do not turn accounting labels into live-provider claims.
Acceptance criteria:
- All surfaces state that Compresso makes no independent provider request but MCP results may be sent by the configured coding client to its provider under client policy.
- Docs explain explicit-root/approved-cwd binding, no sole-repository fallback or tool root switching, stateless launcher behavior, and repository list/remove/setup recovery paths.
- Trust disclosure states native OS signing, npm OIDC/provenance, staged approval, independent verification, launcher SHA-256 limitations, and full package-channel compromise risk.
- No surface claims stable, Windows, source-open engine, secrecy, unpatchability, losslessness, cloud, licensing, desktop, telemetry, or resolved trademark clearance.
Test cases:
- `test_beta_disclosure_snapshots_agree_across_surfaces`.
- `test_provider_and_session_root_disclosure_is_truthful`.
- `test_disclosure_names_proprietary_recoverable_engine_and_channel_limits`.
- `test_forbidden_beta_claim_scan_is_empty`.

### [ ] PB-08: Public beta publication and clean-host acceptance
Owner: I1+PQ+D1 · Estimate: 2.5d · Depends: PB-06, PB-07
Files: separate public distribution repository publish workflow, docs/runbooks/public-beta-release.md, beta evidence records
Description: Publish only `@compresso/cli@beta` after every PB gate passes. Test the packed and published bytes on clean macOS arm64, macOS x86_64, and Linux x86_64 hosts. Exercise repository-bound setup, upgrade stability, Node-free/offline runtime, stateless launcher behavior, secret exclusion, independent verification, recovery, and uninstall. Record immutable evidence and yank/revocation procedure; never change latest/stable or publish Windows.
Acceptance criteria:
- Clean hosts run `compresso doctor`, `compresso setup codex ROOT --dry-run`, and `compresso setup codex ROOT`; Codex launched from ROOT completes MCP initialize/tool-list and repository tools bound to ROOT, while another approved root is rejected.
- Package upgrade preserves client configuration while invoking the upgraded engine; repo list/remove and reversible integration uninstall behave as documented.
- Network-denied runtime works without Node/npm; install executes no lifecycle/download; launcher creates no mutable state; secret fixtures never reach store or MCP.
- Release evidence links import review, signatures, OS signing, SBOM, private and npm provenance, clean-host results, package digest, and yank/revocation drill.
- Only the npm beta tag changes; D36 remains OPEN and Windows, stable, licensing, cloud, desktop, updater, Homebrew, and general E8-E16 stay excluded.
Test cases:
- `test_published_beta_clean_host_setup_mcp_and_root_isolation`.
- `test_published_beta_upgrade_preserves_config_and_node_free_runtime`.
- `test_published_beta_stateless_launcher_secret_exclusion_and_recovery`.
- `test_beta_tag_only_d36_open_and_no_windows_artifact`.

### PB beta acceptance gates

| Gate | Requirement | Owner |
|---|---|---|
| PB-G1 | MCP binds one immutable canonical approved repository from explicit root or approved cwd only; tools cannot switch roots; provider disclosure and secret exclusions are truthful and tested | PB-02, PB-08 |
| PB-G2 | Repository list/remove, one-command setup, dry-run/backup/receipt/uninstall, client-appropriate cwd/explicit root binding, and lexical shim-path upgrades work without deleting repository files | PB-03, PB-08 |
| PB-G3 | Every private artifact verifies against immutable manifest, signature, digest, target, version, and provenance; public payloads exclude private material | PB-04, PB-06 |
| PB-G4 | The single allowlisted package has no lifecycle/runtime dependency; its launcher self-locates, selects exactly, creates no mutable state, preserves cwd for immutable repository-session binding, directly execs, and detects corruption | PB-05, PB-08 |
| PB-G5 | Public import, native OS signing, npm OIDC/provenance, and staged approval are auditable; disclosure acknowledges full package-channel compromise and the limits of launcher SHA-256 | PB-06, PB-07, PB-08 |
| PB-G6 | Package, README, CLI/help, setup, and verification surfaces agree on proprietary recoverability, platforms, provider path, session-root rules, trust limits, and exclusions | PB-07 |
| PB-G7 | Clean supported hosts complete doctor, setup dry-run/install, repository-bound offline MCP, upgrade, recovery, and uninstall with Node absent at runtime | PB-08 |
| PB-G8 | Only the npm `beta` tag is changed; D36 remains OPEN; stable/latest, Windows, licensing, cloud, desktop, updater, Homebrew, and general E8-E16 remain out of scope | PB-01, PB-08 |

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
Description: Extend the E1-03 gate into the full 08 §18 program: machine-generated dependency inventory (Rust + TS) reviewed each release; MIT/BSD/Apache-2.0 allowed with notice compliance and Apache-2.0 patent terms preserved; LGPL requires linking/modification review; GPL requires distribution-model review; AGPL requires legal review before any use — CI fails when an unapproved license enters a release dependency graph via either `cargo deny` or the pnpm license check. A generated `THIRD-PARTY-NOTICES` file ships in every artifact (gate G12). Clean-room rule (D2) restated as a review requirement: no code, prompts, or assets copied from the predecessor CLI or the reference product.
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

## E15 — Clean-room advanced capabilities

Per `09-advanced-capabilities.md`, D49-D50, and ADR-0023/0024. All fixtures and implementation artifacts are original. Epic estimate: 116.5 engineer-days.

### [ ] E15-01: Schemas, migration 0004, and permission types
Owner: R1+R2 · Estimate: 2.5d · Depends: E3-01, E8-02 · Files: schemas/{search,edit,sql,recall,knowledge,review}/v1.json, rust/compresso-store/migrations/0004_advanced_capabilities.sql
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
Note (2026-11-22): E17-50 (D115, ADR-0036) lands part of this ticket early by owner request: one environment-variable URL, the TLS/host policy, and bounded read-only execution for the `compresso_sql` tool; the rest stays here.

### [ ] E15-10: PostgreSQL/MySQL/SQLite schema graph
Owner: R2 · Estimate: 5d · Depends: E15-09 · Files: rust/compresso-sql/src/{postgres,mysql,sqlite,schema}.rs, fixtures/sql/
Acceptance/tests: normalized objects/relationships across engines, restricted catalogs, cycles, caching, and output bounds.

### [ ] E15-11: SQL parser, linter, and classifier
Owner: R1 · Estimate: 3d · Depends: E15-01 · Files: rust/compresso-sql/src/{parse,classify,lint}.rs
Acceptance/tests: read/data/schema/admin/unknown classes; writable CTE, stacked statement, comment-smuggling, and dialect corpus.

### [ ] E15-12: Bounded read-only SQL execution
Owner: R2 · Estimate: 3d · Depends: E15-09, E15-11 · Files: rust/compresso-sql/src/query.rs
Acceptance/tests: read-only role/transaction, timeout/cancel, row/byte/cell/line bounds, formats, attempted-write and redaction tests.
Note (2026-11-22): E17-50 (D115, ADR-0036) lands part of this ticket early by owner request: one environment-variable URL, the TLS/host policy, and bounded read-only execution for the `compresso_sql` tool; the rest stays here.

### [ ] E15-13: SQL mutation plan/apply
Owner: R1+R2 · Estimate: 4d · Depends: E15-11, E15-12, E14-17 · Files: rust/compresso-sql/src/mutate.rs
Acceptance/tests: rollback preview, elevated grant, stale schema/plan, partial failure, audit; admin denied by default.

### [ ] E15-14: Recall ingestion and secret redaction
Owner: R2+I1 · Estimate: 4d · Depends: E15-01, E14-17 · Files: rust/compresso-recall/src/{sources,parse,redact}.rs, fixtures/recall/
Acceptance/tests: disabled default, per-source consent, typed parsing, malformed/duplicate handling, secret corpus, no plaintext storage.
Note (D74): extends the `compresso-recall` crate delivered by E17-02 (`sources`, `parse`, `redact` already exist index-free); add ingestion and the index alongside, do not fork the crate.

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
Acceptance/tests: G16-G17, search/edit/SQL/recall/review/parity/provenance/license gates; deliberate failure and no-reference-product-dependency scans block publish.

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

### [x] E2-15: Differential token accounting and large-payload latency
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
Partly delivered 2026-11-15 without the 0005 migration (`rust/compresso-atlas/src/rust_imports.rs`): Rust `use` paths resolve across crates of one Cargo workspace and to module files, external and same-file imports no longer make `impact` incomplete, and `node:` built-ins are external. Tests: `test_rust_imports_resolve_to_the_file_that_defines_the_path`, `test_same_named_crate_in_another_workspace_is_not_the_import_target`, `test_external_and_same_file_imports_do_not_count_against_completeness`. Still open: persisted resolution table and migration, alias cycles, Python and TypeScript package resolution, rename invalidation, and the remaining test cases below.
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


## E17 — Session continuity, savings fidelity, and platform hardening

Added 2026-11-14 (D70). Clean-room follow-ups to the local slice; E17-01..10 landed together through `integration/e17`, E17-11..14 are open follow-ups; E17-15 is open and E17-16 landed 2026-11-15 (D77). E17-17..21 landed 2026-11-16 (D83–D86); E17-22..29 landed 2026-11-18 (D87–D94). D83–D94 were confirmed by the owner on 2026-11-19. E17-30..32 landed 2026-11-19 (D95–D97); D97 records the owner's decision to build the opt-in edit tool; D95, D96, and the design choices listed in D97 were confirmed by the owner on 2026-11-19. E17-33 landed 2026-11-19 (D98, confirmed by the owner the same day). E17-34 was implemented 2026-11-19 (D99, confirmed by the owner the same day). E17-35 was implemented 2026-11-19 (D100, confirmed by the owner on 2026-11-20). E17-36 was implemented 2026-11-20 (D101, confirmed by the owner on 2026-11-20). E17-37 was implemented 2026-11-20 (D102, confirmed by the owner on 2026-11-20). E17-38 was implemented 2026-11-20 (D103, proposed by the implementing session, confirmed by the owner on 2026-11-20). E17-39 was implemented 2026-11-20 (D104, proposed by the implementing session, confirmed by the owner on 2026-11-20). E17-41 was implemented 2026-11-21 (D106, proposed by the implementing session, confirmed by the owner on 2026-11-21). E17-42 was implemented 2026-11-21 (D107, proposed by the implementing session, confirmed by the owner on 2026-11-21; it records the owner's two earlier decisions of 2026-11-21 as given). E17-43, E17-44, and E17-45 were implemented 2026-11-21 at the owner's request of the same day for performance work that changes no result (D108, D109, D110, proposed by the implementing session, confirmed by the owner on 2026-11-21). E17-46 was implemented 2026-11-21 at the owner's request of that day (D111, proposed by the implementing session, confirmed by the owner on 2026-11-21; it amends D102's confirmed `impact` outputs). E17-47 was implemented 2026-11-22 at the owner's request (D112, proposed by the implementing session, awaiting owner confirmation; it amends ADR-0035 decisions 2, 5, and 8). E17-48 was implemented 2026-11-22 at the owner's request (D113, proposed by the implementing session, awaiting owner confirmation). E17-49 was implemented 2026-11-22 at the owner's request of that day (D114: the owner approved both parts; the bounds, refusals, and note format the session chose await owner confirmation; it amends D112). E17-50 was implemented 2026-11-22 at the owner's request (D115: the owner's decisions recorded as given; the design awaiting owner confirmation); it partly lands E15-09 and E15-12.

### [x] E17-01: Re-assert tool routing after client compaction
Owner: D1 · Estimate: 2d · Depends: E5-01, E6-01, E6-02, E6-03
Files: rust/compresso-mcp/src/reminder.rs, rust/compresso-integrations/src/compaction_hook.rs, rust/compresso-cli/src/commands/{mcp,integrations}.rs, rust/compresso-cli/tests/cli_compaction_hook.rs, docs/integrations/compaction-reminder.md
Description: Constant bounded routing reminder (`compresso mcp routing-reminder`) plus an opt-in `SessionStart`/`compact` hook for Claude Code and Codex, managed by `integrations … --compaction-hook` through plans and receipts. Cursor is unsupported.
Acceptance criteria:
- Reminder ≤640 bytes, byte-identical per call, names only served tools; no store, network, or telemetry.
- Adapter owns only `hooks.SessionStart`; idempotent; refuses user-defined hooks; uninstall retains modified keys; symlinked config rejected.
- Hook and MCP entry have independent receipts.
- Follow-up (D76): `compresso setup` installs the hook by default for Claude Code and Codex (`--no-compaction-hook` opts out); a refusal because the user defines `SessionStart` hooks is a skipped step with warning and fix text and exit 0; Cursor has no step; `integrations install` still requires `--compaction-hook`. Covered by `rust/compresso-cli/tests/cli_setup_compaction_hook.rs`.
Test cases:
- `test_routing_reminder_is_bounded_single_line_ascii`: size, ASCII, single line.
- `test_routing_reminder_hook_envelope_is_session_start_additional_context`: exact JSON envelope.
- `test_install_refuses_to_rewrite_user_defined_session_start_hooks`: typed refusal, bytes unchanged.
- `test_repeated_install_is_idempotent_and_follows_a_moved_launcher`: one group, updated path.
- `test_uninstall_retains_hooks_the_user_added_after_install`: `key_modified` warning.
- `test_hook_and_mcp_entry_have_independent_receipts`: uninstalling the hook keeps the MCP entry.
- `test_hook_dry_run_and_unsupported_clients_write_nothing`: Cursor refused, no writes.
- `test_installed_hook_command_emits_the_reminder_when_run_by_a_shell`: real binary through `/bin/sh -c`.

### [x] E17-02: Index-free local session recall across clients
Owner: R2 · Estimate: 3d · Depends: E1-03, E4-01, E5-01
Files: rust/compresso-recall/src/{lib,sources,parse,redact,search}.rs, rust/compresso-recall/tests/recall_search.rs, rust/compresso-cli/src/commands/{recall,mcp}.rs, rust/compresso-cli/tests/cli_recall.rs, rust/compresso-mcp/src/tools/recall.rs, rust/compresso-mcp/src/server.rs, rust/compresso-mcp/tests/tools_recall.rs, rust/compresso-store/src/ops.rs, docs/plan/04-mcp-and-integrations.md
Description: Add the pure-local `compresso-recall` crate, `compresso recall QUERY --source SOURCE`, and the opt-in `compresso_recall` MCP tool. Recall searches Claude Code (`~/.claude/projects/<encoded-cwd>/<session-id>.jsonl`) and Codex (`~/.codex/sessions/**/rollout-*.jsonl`) transcripts directly from disk with no persistent index, cache, write, or upload. It is the index-free precursor to E15-14–E15-16 and keeps their private-read, per-source opt-in rule. Every line is shape-probed; unrecognized files are counted, not guessed. Cursor is reported `unsupported` and never read.
Acceptance criteria:
- Recall reads nothing until a source is named: the CLI requires `--source`; the MCP tool is neither listed nor callable unless the server starts with `--recall-source`, and the default tool list stays at ten.
- Follow-up (D75): `integrations install` and `setup` write `--recall-source <own client>` into Claude Code and Codex entries by default (`--no-recall` opts out; Cursor never gets a source). Covered by `rust/compresso-integrations/tests/session_recall.rs` (per-client source, opt-out, foreign-source rejection, upgrade from a pre-D75 entry, idempotent repeat, byte-exact uninstall) and the recall cases in `rust/compresso-cli/tests/cli_mcp_health.rs` (receipted recall entry is probed; an unreceipted, foreign, repeated, or Cursor recall argument is never executed).
- MCP recall is always scoped to the bound repository; the CLI defaults to the repository enclosing the cwd and widens only with `--all-repositories`. A tool call cannot add a source, widen scope, or raise a bound.
- Text is secret-redacted before matching, so a query for a redacted value returns no hit.
- Results, files, bytes per file, line size, and wall-clock time are bounded; `truncated` is reported when a bound stops the search.
- Transcript directories are byte-for-byte unchanged after recall; symbolic links are never followed.
- Each MCP call records a `compresso_recall` ledger operation.
Test cases:
- test_recall_finds_decisions_across_claude_code_and_codex
- test_recall_is_repository_scoped_by_default_and_widens_only_explicitly
- test_recall_requires_an_explicitly_enabled_source
- test_recall_redacts_secrets_and_never_matches_a_redacted_value
- test_recall_reports_unverified_formats_instead_of_guessing
- test_recall_is_read_only_and_keeps_no_index
- test_recall_bounds_results_snippets_files_and_time
- test_redact_removes_every_secret_in_the_corpus
- test_discover_never_follows_symlinks
- test_recall_tool_is_absent_and_refused_until_an_operator_enables_a_source
- test_recall_tool_returns_redacted_repository_scoped_hits_and_records_the_operation
- test_recall_tool_cannot_widen_sources_scope_or_bounds
- test_recall_json_searches_enabled_sources_in_the_enclosing_repository
- test_recall_sources_are_disabled_by_default_and_enabled_only_explicitly

### [x] E17-03: Transcript-derived savings and cost reporting
Owner: R1+PQ · Estimate: 2d · Depends: E2-15, E3-03, E4-03, E5-04
Files: rust/compresso-core/src/{pricing,transcript}.rs, rust/compresso-core/data/price-table-v1.json, schemas/price-table/v1.json, rust/compresso-cli/src/commands/{engine,transcript_stats}.rs, rust/compresso-mcp/src/{server.rs,tools/engine_tools.rs}, docs/adr/0029-transcript-savings-and-local-price-table.md, docs/plan/{02-schemas,04-mcp-and-integrations}.md
Description: Derive savings from a client JSON Lines transcript of Compresso tool calls (`compresso stats --transcript`) so saved headless runs report savings without a ledger, and price modeled savings per model from a checked-in versioned price table. Ledger rows stay authoritative; CLI and MCP stats remain schema_version 1 with additive `cost_basis` and `transcript_savings`.
Acceptance criteria:
- Price table is local, schema-validated, carries table_version and effective_date, and is never fetched; unknown models report tokens only.
- Savings are priced once at the base input rate and labelled modeled; negative totals cost nothing.
- Each operation_id counts once; ledger row wins; ledgerless compression uses exact counts (zero on fallback); ledgerless retrieval claims zero avoidance.
- Errors, unparsed, duplicate, and missing results add nothing; no transcript text is retained; scanning writes no ledger rows; MCP accepts no file path.
- Model ids (transcript, `--model`, MCP `model`) must be 1–128 bytes of printable ASCII with no spaces; others bucket to unknown, are never priced, are counted in `model_ids_rejected`, and are never echoed; human output escapes non-printable characters.
Test cases:
- `test_headless_transcript_without_ledger_reports_exact_compression_savings`, `test_fallback_and_baseline_free_retrieval_claim_zero_savings`, `test_ledger_row_is_authoritative_over_transcript_numbers`, `test_unknown_and_synthetic_models_report_tokens_without_price`, `test_duplicates_errors_foreign_tools_and_garbage_never_add_savings`, `test_scan_retains_no_transcript_text`, `test_unknown_model_is_unpriced_and_never_guessed`, `test_checked_in_price_table_validates_against_v1_schema`, `test_stats_transcript_reports_savings_and_cost_for_headless_run_without_ledger`, `test_stats_transcript_prefers_ledger_row_over_transcript_numbers`, `test_stats_prices_modeled_savings_only_for_models_in_price_table`, `test_every_control_byte_in_model_id_is_rejected_and_unpriced`, `test_escape_sequences_oversized_and_invisible_model_ids_are_rejected`, `test_stats_transcript_hostile_model_ids_never_reach_terminal_or_pricing`, `test_stats_rejects_unsafe_model_argument_without_echoing_it`.

### [x] E17-04: Parallel scanning and reads in Atlas
Owner: R2 · Estimate: 2d · Depends: E3-04, E3-09
Files: rust/compresso-atlas/src/concurrency.rs, rust/compresso-atlas/src/scanner.rs, rust/compresso-atlas/src/indexer.rs, rust/compresso-atlas/src/lib.rs, rust/compresso-atlas/benches/parallel_index.rs, rust/compresso-atlas/Cargo.toml, docs/plan/03-interfaces.md
Description: Parallelize the repository scanner and incremental indexer without changing their results. Add `Concurrency` (`default()`, `serial()`, `bounded(jobs, memory_budget_bytes)`): jobs are clamped to `1..=available CPUs` and a byte budget (default 64 MiB) caps buffered in-flight source bytes. `scan_with_concurrency` walks with the `ignore` parallel walker and fans file inspection (canonicalize, binary sniff, BLAKE3) out to scoped worker threads; secret-pattern, symlink, file-type, and size exclusions are decided on the calling thread before any worker can open a file. `Indexer::with_concurrency` reads, hashes, parses, resolves imports, and estimates tokens on workers, while every `Store` write stays on the calling thread in candidate order, one transaction per file (single writer, WAL unchanged). Budget bytes are charged at dispatch and released at commit. Small inputs (under 16 files to inspect, under 8 index candidates) stay serial. No new dependency: `std::thread::scope`, `std::sync::mpsc`, and the already-locked `criterion` as a dev-dependency.
Acceptance criteria:
- `ScanResult` is identical for serial and 2/4/8 workers, including ordering of `added`, `changed`, `removed`, and `skipped`.
- Files, symbols, edges, `IndexReport` counts, and the `IndexProgress` sequence are identical for serial and parallel indexing.
- Secret-pattern and oversize entries are never opened at any concurrency.
- When several files fail, the lowest-ordered failure is reported; an indexing failure leaves every earlier candidate committed and nothing later.
- Worker count never exceeds available CPUs or `memory_budget_bytes / per-worker bytes`; a one-byte budget still completes in order.
- A criterion benchmark compares serial and parallel scan and first index; existing latency gates stay green.
Test cases:
- test_parallel_scan_matches_serial_scan: wide tree with added/changed/removed/binary/secret entries; jobs 2, 4, 8 repeated three times equal the serial result.
- test_parallel_scan_never_opens_excluded_entries: recording inspector never sees `.env.production` or a 2 MB file with 4 workers.
- test_parallel_scan_reports_lowest_ordered_read_error: two injected read denials with 8 workers always report the lower path.
- test_memory_budget_limits_inspection_workers: tight budgets reduce 8 requested workers to 2 and 1.
- test_parallel_index_matches_serial_index: 64 generated Rust/TypeScript/Python/Markdown files; snapshot of report, progress, files, symbols, and edges equals serial for jobs 2, 4, 8.
- test_parallel_index_stops_at_lowest_ordered_failure: injected failure at candidate 5 with 8 workers leaves exactly the first five files committed and five progress events.
- test_parallel_preparation_commits_in_order_under_a_starved_memory_budget: 4 workers with a one-byte budget commit all 48 candidates in order.
- test_bounded_concurrency_never_exceeds_available_cpus, test_workers_are_limited_by_memory_budget_and_item_count, test_read_ahead_holds_budget_until_ordered_release, test_read_ahead_admits_one_over_budget_item_alone, test_read_ahead_window_bounds_zero_cost_items: bounds and admission-control units.
- bench parallel_index: `cargo bench -p compresso-atlas --bench parallel_index -- --noplot` reports `atlas_scan_1200_files/{serial,parallel}` and `atlas_first_index_1200_files/{serial,parallel}`.

### [x] E17-05: Windows hardening — line endings, console windows, broken pipes
Owner: D1 · Estimate: 2d · Depends: E6-05, E5-01, E4-02
Files: rust/compresso-integrations/src/{text_fidelity,plan,lib,claude_code,codex,cursor}.rs, rust/compresso-integrations/tests/text_fidelity.rs, rust/compresso-core/src/{stdio,background_spawn}.rs, rust/compresso-mcp/src/{server,session}.rs, rust/compresso-mcp/tests/closed_pipe.rs, rust/compresso-cli/src/commands.rs, rust/compresso-cli/tests/cli_closed_pipe.rs, rust/compresso-eval/src/cli.rs
Description: Preserve BOM and line endings in every client-config mutation, provide the CREATE_NO_WINDOW policy for background launches, and classify closed stdio pipes as a typed error instead of a panic. By owner decision the `compresso` CLI then exits quietly like a standard Unix filter, while the MCP server and `compresso-eval` report the typed error.
Acceptance criteria:
- BOM kept, never added; untouched lines keep exact terminators; written lines use the dominant ending (tie → LF); created files are LF without BOM.
- Uninstall of a config untouched since install is byte-identical; later user edits are never reverted.
- Launch sites route through `suppress_console_window`; no production spawn exists.
- EPIPE / ERROR_BROKEN_PIPE / ERROR_NO_DATA never panic; the MCP server and `compresso-eval` report `output_closed`.
- The `compresso` CLI is silent when its stdout reader closes early (`compresso ... | head -c1`): no stderr output and exit 0. An error reported before the reader left keeps its message and exit code, and a closed stderr never panics.
Test cases:
- test_install_then_uninstall_restores_byte_identical_original_for_every_encoding
- test_install_preserves_bom_untouched_terminators_and_inserts_dominant_ending
- test_uninstall_restores_non_canonical_json_formatting_byte_for_byte
- test_uninstall_never_reverts_a_whitespace_only_user_edit_to_the_backup
- test_windows_builder_receives_create_no_window
- test_cli_exits_silently_when_stdout_reader_closed_early
- test_cli_piped_into_head_exits_zero_without_stderr_output
- test_cli_reports_real_error_even_when_stdout_reader_is_closed
- test_cli_failure_with_closed_stderr_exits_without_panicking
- test_mcp_server_exits_with_typed_error_when_stdout_reader_closed_early
- test_writing_to_child_that_exited_early_is_typed_closed_error
- test_output_closed_during_session_is_typed_error

### [x] E17-06: MCP server health and unresponsive detection
Owner: D1 · Estimate: 3d · Depends: E4-04, E5-01, E6-01, E6-05
Files: rust/compresso-cli/src/commands/{mcp_probe,doctor,integrations}.rs, rust/compresso-integrations/src/server_entry.rs, rust/compresso-cli/tests/cli_mcp_health.rs, scripts/check-crate-capabilities.ts, docs/adr/0028-bounded-mcp-health-probe.md
Description: `compresso doctor` and `integrations status` report a configured server that is stale, not executable, crashes, hangs, or answers wrongly, each with fix text, using one bounded stdio handshake against trusted launchers only.
Acceptance criteria:
- Only the running executable or a receipt-authenticated entry is started; all else is `not_probed`/`config_modified`.
- `not_probed` and `config_modified` are warnings (owner decision): the fix text is shown, the additive `severity` field is `warning`, and `doctor` exits 0 when nothing else fails. A missing or non-executable binary, a crash, a handshake timeout, a malformed response, and an unreadable config are errors and fail `doctor`. `integrations status` reports the same `severity`.
- One deadline, 1 MiB output cap, scrubbed environment, cwd `/`, process-group kill.
- Injected environments start no process; the capability allowance is one file in one crate.
Test cases:
- `test_doctor_reports_handshake_timeout_for_unresponsive_server`: bounded timeout verdict.
- `test_doctor_reports_crash_with_exit_status`: `exited_early`, status 3.
- `test_doctor_reports_stale_and_non_executable_launcher_paths`: `binary_missing`, `binary_not_executable`.
- `test_doctor_reports_malformed_output_and_foreign_tool_list`: `malformed_response`.
- `test_project_config_pointing_at_canary_is_never_executed`: canary file absent.
- `test_symlink_to_a_non_compresso_binary_is_rejected`: `not_probed`, a warning, never executed.
- `test_doctor_human_output_shows_warning_and_fix_text_for_hand_edited_entry`: `[WARN]` with the fix text.
- `test_doctor_fails_on_real_failure_and_keeps_warning_fix_text`: one warning plus one crash exits non-zero with both fixes.
- `test_integration_status_shows_fix_text_for_unverifiable_entry`: `warning:` line, exit 0.
- `test_timeout_kills_the_server_process_tree`: descendant is dead.
- `test_doctor_reports_healthy_for_real_compresso_server`: real binary handshake.
- `test_mcp_probe_allowance_does_not_cover_other_cli_modules`: checker still denies siblings.

### [x] E17-07: Clearer search diagnostics
Owner: R2 · Estimate: 1.5d · Depends: E3-02, E3-11, E4-02, E5-03
Files: rust/compresso-atlas/src/search.rs, rust/compresso-atlas/src/queries.rs, rust/compresso-atlas/src/lib.rs, rust/compresso-store/src/symbols.rs, rust/compresso-mcp/src/tools/atlas_read.rs, rust/compresso-mcp/src/tools/common.rs, rust/compresso-mcp/src/server.rs, rust/compresso-cli/src/commands/retrieval.rs, docs/plan/03-interfaces.md, docs/plan/04-mcp-and-integrations.md, tests in rust/compresso-atlas/tests/search_diagnostics.rs, rust/compresso-mcp/tests/tools_read.rs, rust/compresso-cli/tests/cli_retrieval.rs
Description: Make symbol search explain empty and partial results. Add `search_with_diagnostics(store, repo, query, SearchOptions { limit, path })` returning `SearchReport { hits, diagnostics }`. An optional repo-relative `path` scope (file or directory, applied before the hit limit) that names no indexed file yields a successful empty result with a `path_not_found` diagnostic; a query under three characters yields `query_too_short`; a scope applied to a capped ranked window yields `path_scope_window_capped`. Malformed queries fail with `QueryError::InvalidSearchQuery { position, reason }` (zero-based byte offset); an escaping scope fails with `QueryError::InvalidSearchPath`. `compresso_search` gains `path`, `diagnostics`, `truncated_hits` (`path`, `start_line`, `end_line` of hits dropped by `max_tokens`, counted inside the budget), and `truncated_hits_omitted`; `compresso search` gains `--path` and `diagnostics`. All result fields are additive, so `schema_version` stays 1. Multi-word queries match symbols containing every whitespace-separated term (`Store::search_symbols_all_terms`): any order and letter case, surrounding punctuation ignored, terms under three characters dropped, exact-phrase matches first. Search has no regex mode, so regex validation is out of scope here and belongs to the E15 combined-search work.
Acceptance criteria:
- A missing path scope is a success with `hits: []` and one `path_not_found` info diagnostic in Atlas, MCP, and CLI; a secret-pattern scope reports the same without revealing existence.
- A path scope restricts hits to that file or directory and is applied before `limit`.
- Empty, whitespace-only, and NUL-containing queries are `invalid_arguments` naming the byte offset and reason.
- When `max_tokens` drops hits, `truncated_hits` lists their path and line range, `tokens_used` stays within `max_tokens`, and kept + listed + omitted equals `hits_total`.
- `search "safety gate"` matches `safety_gate` and `SafetyGates`; case, extra spaces, and edge punctuation do not change the result; a term absent from a symbol excludes it; one-word queries are unchanged.
- Query text is always quoted and never parsed as FTS5 syntax.
Test cases:
- test_search_missing_path_scope_is_informational_not_an_error: Atlas report has no hits and a `path_not_found` diagnostic naming the scope.
- test_search_path_scope_filters_to_file_or_directory: directory and file scopes; `src/auth/` does not match `src/auth_extra.rs`.
- test_search_path_scope_is_applied_before_the_hit_limit: 40 higher-ranked out-of-scope symbols do not crowd out the scoped hit at limit 2.
- test_search_rejects_path_scope_that_escapes_the_repository: `../outside`, `/etc`, and backslash paths are `InvalidSearchPath`.
- test_search_secret_path_scope_reports_not_found_without_hits: `.env` scope is `path_not_found`.
- test_search_query_below_trigram_length_is_diagnosed: `io` returns `query_too_short`.
- test_invalid_query_reports_byte_position_and_reason: positions 0, 0, 5, and 3 (multi-byte prefix) with matching reasons.
- test_search_with_diagnostics_rejects_invalid_query_before_searching: invalid query wins over a missing scope.
- test_multi_word_search_requires_all_terms_and_ranks_exact_phrase_first: phrase heading first, then `safety_gate` and `gate_for_safety`; case, punctuation, and term order variants.
- test_search_symbols_all_terms_matches_any_order_with_phrase_first, test_multi_word_query_quotes_every_term: Store-level semantics, limits, and FTS quoting.
- test_search_truncation_reports_line_numbers_for_dropped_hits, test_search_untruncated_result_has_no_truncated_hits, test_search_missing_path_scope_is_informational_success, test_search_invalid_query_reports_position_and_reason, test_search_multi_word_query_matches_symbols_containing_all_terms: MCP `compresso_search` contract.
- test_search_missing_path_scope_is_informational_success, test_search_invalid_query_reports_position_and_reason, test_search_multi_word_query_matches_symbols_containing_all_terms: CLI `compresso search --json` contract.

### [x] E17-08: Doc–code drift linter
Owner: PQ · Estimate: 3d · Depends: E1-02, E4-01, E5-01, E17-10
Files: scripts/doc-drift-lint.ts, scripts/doc-drift-lint.test.ts, scripts/doc-drift-lint.config.json, rust/compresso-cli/src/surface.rs, rust/compresso-cli/examples/surface_inventory.rs, rust/compresso-mcp/src/server.rs, docs/architecture/doc-drift-lint.md, .github/workflows/ci.yml
Description: Lint current-state docs against a surface inventory generated from the live clap trees, MCP tool registry, and config key table. Plan docs are out of scope except `Files:` of completed tickets; plan-internal consistency stays with E16-16.
Acceptance criteria:
- Unknown commands, flags, MCP tools, config keys, and repository paths fail; undocumented visible commands and tools fail.
- JSON report; exit 0/1/2; reasoned allowlist whose unused entries fail; enforced in CI.
Test cases:
- test_surface_inventory_reflects_live_clap_trees, test_surface_inventory_lists_registered_mcp_tools_and_config_keys
- test_drift_lint_reports_unknown_cli_command_and_flag, test_drift_lint_ignores_flags_of_other_programs
- test_drift_lint_reports_unknown_mcp_tool_but_not_crate_identifiers, test_drift_lint_reports_unknown_config_key_only_in_known_sections
- test_drift_lint_reports_missing_paths_and_links, test_drift_lint_reports_undocumented_commands_and_tools
- test_drift_lint_checks_files_of_completed_tickets_only, test_drift_lint_allowlist_suppresses_matches_and_flags_stale_entries
- test_drift_lint_cli_emits_json_and_exit_codes

### [x] E17-09: Session decisions to pull request draft
Owner: R2 · Estimate: 2d · Depends: E1-03, E4-02, E17-02
Files: rust/compresso-cli/src/commands/decisions.rs, rust/compresso-cli/src/commands.rs, rust/compresso-cli/src/lib.rs, rust/compresso-cli/tests/cli_decisions.rs, docs/plan/04-mcp-and-integrations.md
Description: Add `compresso decisions [--session ULID] [--limit N] [--pr NUMBER] [--post --yes] [--dry-run] [--json]`. Summarizes one client session's operation-ledger decisions (compression mode, strategies applied or skipped, fallbacks, per-tool retrieval counts, savings totals) as a Markdown pull request comment draft built only from the content-free ledger; every ledger string is secret-redacted and stripped of markup, mentions, and issue references. Posting goes through the `PullRequestPoster` boundary; because `compresso-cli` is `bounded-local` and may not start child processes, a confirmed post fails closed and prints the manual `gh pr comment` pipeline. (Changed 2026-11-15 by D81/ADR-0031: the production poster now runs `gh pr comment` from `commands/pr_post.rs`; tests `test_post_runs_gh_with_fixed_arguments_and_the_draft_on_stdin`, `test_post_reports_a_refusal_from_gh_as_failed`, `test_post_stops_a_gh_that_never_finishes`, `test_post_reports_a_gh_that_cannot_be_started`, `test_system_poster_only_uses_gh_from_an_absolute_search_path_entry` start only a stand-in program.)
Acceptance criteria:
- Preview is the default; `--post` without `--yes` reports `confirmation_required`; `--dry-run` always previews; the poster is never called in any of these cases.
- A post is attempted only for `--pr NUMBER --post --yes`, with a body identical to the preview draft.
- The draft and all output contain no secret and no mention or markup taken from ledger strings.
- Poster failures exit non-zero with `gh_not_found`, `post_unavailable`, or `post_failed` and state that nothing was posted.
- The default session is the one owning the newest session-bound operation; `--session` selects another; unknown, idle, and missing sessions return typed errors.
- No test reaches the network. Since ADR-0031 the `pr_post` tests start a process, and only a stand-in script; no test starts the real `gh`.
Test cases:
- test_decisions_preview_is_the_default_and_never_calls_the_poster
- test_decisions_post_only_with_pr_and_explicit_confirmation
- test_decisions_redact_secrets_and_neutralize_untrusted_ledger_text
- test_decisions_report_poster_failures_and_dry_run_never_reaches_the_system_poster
- test_decisions_select_the_newest_session_or_an_explicit_one
- test_system_poster_only_uses_gh_from_an_absolute_search_path_entry
- test_inline_neutralizes_markup_mentions_and_secrets
- test_format_utc_matches_known_instants

### [x] E17-10: Resumable large-scale benchmark campaigns
Owner: PQ · Estimate: 3d · Depends: E7-02, E1-03
Files: rust/compresso-eval/src/campaign/{mod,state,adapter,checkpoint,runner,report,fixture_adapter,cli}.rs, rust/compresso-eval/src/bin/campaign.rs, rust/compresso-eval/tests/campaign.rs, rust/compresso-eval/tests/data/campaign-smoke/suite.json, docs/architecture/benchmark-campaigns.md
Description: Run external task suites as checkpointed campaigns through a `SuiteAdapter` trait, with a local scripted fixture adapter. Network/container adapters are an opt-in extension point and are not built into this bounded-local crate.
Acceptance criteria:
- Per-task state machine; atomic versioned checkpoint bound to suite and task set; resume never reruns terminal tasks.
- Bounded retries (interrupted attempts counted), per-task timeout with cooperative cancellation, concurrency limit.
- Report in evaluation-report shape plus `campaign` section, ordered by task identifier.
Test cases:
- test_task_state_machine_permits_only_documented_transitions
- test_campaign_drives_every_task_to_a_terminal_state_with_bounded_retries
- test_campaign_resume_does_not_rerun_completed_tasks, test_campaign_resume_counts_attempt_interrupted_by_crash
- test_campaign_resume_applies_attempt_budget_to_pending_retries, test_campaign_resume_rejects_corrupt_or_mismatched_checkpoint
- test_campaign_task_timeout_cancels_attempt_and_exhausts_retries, test_campaign_never_exceeds_concurrency_limit
- test_campaign_persists_running_state_before_each_attempt
- test_campaign_report_uses_eval_report_shape_and_ignores_completion_order
- test_external_adapters_require_opt_in_and_are_not_built_in, test_campaign_cli_runs_resumes_and_reports_status_as_json

### [ ] E17-11: Codex and Cursor transcripts for savings reporting
Owner: R1+PQ · Estimate: 2d · Depends: E17-03, E17-02
Files: rust/compresso-core/src/transcript.rs, rust/compresso-cli/src/commands/transcript_stats.rs, rust/compresso-core/tests/fixtures/transcripts/
Description: Extend `compresso stats --transcript` beyond the Claude Code JSON Lines shape to Codex rollout transcripts, and to Cursor only if its local format can be verified from documentation or format probing. Reuse the E17-02 shape probes instead of a second parser.
Acceptance criteria:
- Format is auto-detected per line; an unverified format is reported `unsupported` and never guessed.
- The same savings rules apply: the ledger row wins, each operation counts once, no transcript text is retained, and model ids are sanitized.
Test cases:
- `test_codex_rollout_transcript_reports_savings_like_claude_code`
- `test_unverified_transcript_format_is_reported_unsupported`
- `test_mixed_format_file_is_rejected_without_partial_savings`

### [ ] E17-12: Retrieval savings without an operation ledger
Owner: R2 · Estimate: 2d · Depends: E17-03, E5-03, D69
Files: rust/compresso-mcp/src/tools/atlas_read.rs, rust/compresso-core/src/transcript.rs, schemas/, docs/plan/04-mcp-and-integrations.md
Description: Retrieval results carry a content-free raw-source baseline (estimated raw tokens plus a basis label), so transcript-derived savings can credit retrieval calls when no ledger row exists. The baseline is additive in the tool schema; savings stay explicitly modelled (D69).
Acceptance criteria:
- Additive schema field with a version note; old transcripts without the field still claim zero retrieval savings.
- The baseline never includes source content or paths beyond what the result already shows.
Test cases:
- `test_retrieval_result_carries_modelled_raw_baseline`
- `test_ledgerless_retrieval_credits_modelled_savings_from_baseline`
- `test_transcript_without_baseline_still_claims_zero_retrieval_savings`

### [ ] E17-13: Document every public CLI command
Owner: D1 · Estimate: 2d · Depends: E17-08
Files: docs/cli/, README.md, scripts/doc-drift-lint.config.json
Description: Write user documentation for the 14 commands the drift linter allowlists as undocumented (`search`, `map`, `outline`, `read`, `unfold`, `context`, `impact`, `compress`, `explain`, `init`, and `account` with its subcommands), then delete those allowlist entries.
Acceptance criteria:
- Every visible CLI command and MCP tool appears in a current-state doc; the drift lint passes with no `undocumented_cli_command` allowlist entries.
Test cases:
- `doc-drift-lint` in CI with the allowlist entries removed

### [ ] E17-14: Free-text session decisions in the operation ledger
Owner: R2 · Estimate: 2d · Depends: E17-09, E3-03
Files: rust/compresso-store/migrations/, rust/compresso-store/src/ops.rs, rust/compresso-cli/src/commands/decisions.rs
Description: Add a ledger table for short free-text design decisions recorded during a session, so `compresso decisions` drafts can include real choices as well as compression statistics. Recording is explicit and local; text is secret-redacted and markup-neutralized on write and on render.
Acceptance criteria:
- Forward-only migration; older binaries refuse the newer store with an actionable message, not `internal`.
- Size and count bounds per session; no automatic capture from transcripts.
Test cases:
- `test_decision_text_is_redacted_and_neutralized_on_write_and_render`
- `test_decisions_draft_includes_recorded_free_text_choices`
- `test_older_binary_reports_newer_store_with_upgrade_hint`

### [ ] E17-15: Compaction reminder hook on Windows
Owner: D1 · Estimate: 1.5d · Depends: E17-01, E17-05
Files: rust/compresso-integrations/src/compaction_hook.rs, rust/compresso-cli/src/commands/setup.rs, rust/compresso-cli/tests/cli_setup.rs, docs/integrations/compaction-reminder.md
Description: The hook command is a POSIX shell line, so `compresso setup` skips the compaction hook on Windows today. Emit a Windows-native hook command (direct executable invocation with Windows quoting, no shell) for Claude Code and Codex, keep the same receipts and ownership rules, and remove the Windows skip.
Acceptance criteria:
- On Windows, setup installs a hook whose command runs `compresso mcp routing-reminder` without a POSIX shell; paths with spaces and non-ASCII characters are quoted correctly.
- Install, re-install, and uninstall are idempotent and receipt-driven, as on Unix; user-defined `SessionStart` hooks are still refused.
- The Windows CI job runs the hook command and checks its output is the reminder text.
Test cases:
- `test_windows_hook_command_invokes_binary_without_shell`
- `test_windows_hook_command_quotes_paths_with_spaces`
- `test_setup_installs_compaction_hook_on_windows`

### [x] E17-16: Bounded reads for compression input and repository files
Owner: R2 · Estimate: 1d · Depends: E2-09, E3-11, E4-02, E5-03
Files: rust/compresso-cli/src/commands/engine.rs, rust/compresso-atlas/src/queries.rs, rust/compresso-cli/src/commands/retrieval.rs, rust/compresso-mcp/src/tools/common.rs, rust/compresso-cli/tests/cli_engine.rs, rust/compresso-atlas/tests/queries_unfold_search_read.rs
Description: Cap two of the SEC-005 read paths (D77). `compresso compress` reads stdin and `--file` through an 8 MiB ceiling (the engine input limit) and rejects larger input with `input_too_large` before buffering or tokenizing it, never echoing it back; before this change the CLI had no limit. Repository reads used by `read`, `unfold`, `context`, and the read gate stop at 16 MiB (`MAX_READ_FILE_BYTES`) and fail with `file_too_large`, including line-range reads of larger files; `compresso_context` skips the body excerpt of such a file instead of failing. Not covered here: MCP `compresso_compress` (graph built and tokenized before the 8 MiB check, oversized input echoed back per its documented contract) and the whole-file reads used for staleness hashing.
Acceptance criteria:
- The capped paths never buffer more than their ceiling plus one byte; the ceiling is enforced on the open handle, not only on metadata.
- Exactly-at-limit input is accepted; one byte over is rejected; a multibyte character split at the limit is reported as oversized, not as invalid UTF-8.
- Rejections write no ledger row.
Test cases:
- `test_compress_stdin_over_read_limit_is_rejected_without_ledger_row`
- `test_compress_file_over_read_limit_is_rejected`
- `test_compress_input_at_engine_limit_is_accepted`
- `test_read_capped_accepts_input_at_limit`
- `test_read_capped_rejects_one_byte_over_limit`
- `test_read_capped_stops_reading_an_endless_source`
- `test_read_capped_reports_split_multibyte_text_as_oversized`
- `test_read_utf8_capped_accepts_file_at_limit`
- `test_read_utf8_capped_rejects_one_byte_over_limit`
- `test_read_bounded_rejects_file_over_byte_limit_before_loading`
- `test_plan_context_skips_body_of_file_grown_past_read_limit`

Follow-up (D79), files rust/compresso-mcp/src/tools/engine_tools.rs, rust/compresso-atlas/src/queries.rs: MCP `compresso_compress` refuses text over 8 MiB with `input_too_large` before building the graph, without echoing it; staleness checks hash through a fixed buffer and never load a file whole. Test cases:
- `test_compress_tool_refuses_oversized_input_without_echoing_it`
- `test_staleness_hash_matches_unchanged_content_in_bounded_chunks`
- `test_staleness_hash_detects_same_size_content_change`
- `test_staleness_hash_stops_reading_a_file_that_grew_past_its_indexed_size`

Follow-up (D82), files rust/compresso-atlas/src/queries.rs, rust/compresso-mcp/src/tools/atlas_read.rs, rust/compresso-cli/src/commands/retrieval.rs: a read does work in proportion to what it returns; totals above 1,000,000 bytes are flagged estimates. Test cases:
- `test_read_of_a_large_file_counts_about_what_it_returns_not_the_whole_file`
- `test_bounded_read_returns_the_same_prefix_as_counting_the_whole_selection`
- `test_growing_window_stays_within_budget_and_close_to_counting_everything`
- `test_file_line_count_matches_a_full_read_without_counting_tokens`

### [x] E17-17: Context pack ranks by task concepts and reports honest readiness
Owner: R2 · Estimate: 2d · Depends: E3-12, E5-03
Files: rust/compresso-atlas/src/queries.rs, rust/compresso-atlas/tests/queries_impact_plan.rs, rust/compresso-mcp/src/tools/atlas_read.rs, rust/compresso-mcp/tests/tools_read.rs, rust/compresso-cli/src/commands/retrieval.rs, docs/plan/04-mcp-and-integrations.md, decisions.md
Description: D83. `plan_context` scores symbols across every task query, adds compound-identifier queries and body-content scoring, lowers test code, drops single-common-word noise, and centres long excerpts on the matching lines. `ContextPlan` reports task-concept coverage; `compresso_context` and `compresso context --json` expose it as `task_coverage`, and a pack whose evidence misses half of the task's salient words is `targeted_follow_up_needed` without budget expansion.
Acceptance criteria:
- A symbol naming or containing a compound of adjacent task words leads the pack ahead of symbols matching one common word.
- A body longer than its excerpt allowance is excerpted from the matching lines, and the evidence header cites the excerpt's own range.
- Test code is lowered unless the task mentions tests.
- Evidence matching only common words never yields `task_pack_ready`; uncovered words are listed; no budget expansion follows.
- No store migration; reads only; existing context tests unchanged.
Test cases:
- `test_plan_context_ranks_compound_identifier_and_body_matches_above_generic_word_hits`: ranking, centred excerpt, header range.
- `test_plan_context_reports_uncovered_concepts_when_only_generic_words_match`: coverage below half, uncovered words listed.
- `test_plan_context_test_code_ranks_normally_when_the_task_asks_for_tests`: penalty lifted.
- `test_context_tool_generic_word_matches_do_not_claim_a_ready_pack`: MCP status, `task_coverage`, no expansion.

### [x] E17-18: Retrieval eval gates context-pack quality
Owner: PQ+R2 · Estimate: 2d · Depends: E17-17, E7-03
Files: fixtures/retrieval/tasks.toml, fixtures/retrieval/repo/, fixtures/corpus.toml, rust/compresso-eval/src/retrieval.rs, rust/compresso-eval/src/gates.rs, rust/compresso-eval/src/runner.rs, rust/compresso-eval/src/fixtures.rs, rust/compresso-eval/tests/retrieval.rs, rust/compresso-eval/tests/corpus_integrity.rs, docs/plan/gates.toml, docs/plan/06-fixtures.md, decisions.md
Description: D84. `compresso-eval` indexes the committed source corpus under `fixtures/retrieval/repo/` into a temporary store with the production indexer and runs `plan_context` at a fixed 1600-token budget for every task in `fixtures/retrieval/tasks.toml` (06 §12). Each task records gold-file recall, evidence precision, tokens, and readiness; the report gains an additive top-level `retrieval` section, and `gates.toml` gains a `[retrieval]` table that the existing `run --gates` CI step enforces. Tasks marked `known_gap = "body_only"` are reported but kept out of the gated aggregate.
Acceptance criteria:
- `compresso-eval run fixtures/ --gates docs/plan/gates.toml --json` includes `retrieval` and exits non-zero when a `[retrieval]` threshold is missed; a fixture root without `retrieval/` has no section and no retrieval rows.
- The corpus holds decoys sharing single common words with targets, test files, compound-identifier targets, body-only targets, and absent-concept tasks; `fixtures/corpus.toml` pins its tree hash.
- A corpus that cannot be indexed or planned, or a gold file with no indexed symbols, fails the gates instead of dropping the section.
- A ready pack that lacks a gold file, or a ready pack for an absent concept, counts as `false_ready` with zero tolerance.
- The report is deterministic; `schema_version` stays 1 because keys are only added; no network or process capability is added.
Test cases:
- `test_committed_retrieval_corpus_passes_committed_gates`: committed corpus against committed thresholds; absent concepts never ready.
- `test_retrieval_ranking_regression_fails_recall_and_precision_gates`: a synthetic below-threshold report fails by name.
- `test_retrieval_false_ready_fails_gate`, `test_ready_on_absent_concept_is_false_ready`: false-ready counting and gate.
- `test_known_gap_tasks_are_reported_outside_the_gated_aggregate`: exclusion from gated aggregates.
- `test_retrieval_corpus_that_was_not_evaluated_fails_closed`, `test_gold_file_without_indexed_symbols_fails_closed`: fail closed.
- `test_retrieval_report_is_deterministic`, `test_retrieval_corpus_path_hash_and_size`: determinism and corpus lock.

### [x] E17-19: Bounded content scan feeds the context planner
Owner: R2+PQ · Estimate: 2d · Depends: E17-17, E17-18
Files: rust/compresso-atlas/src/queries.rs, rust/compresso-atlas/tests/queries_impact_plan.rs, fixtures/retrieval/tasks.toml, fixtures/retrieval/repo/, fixtures/corpus.toml, rust/compresso-eval/src/gates.rs, rust/compresso-eval/tests/retrieval.rs, docs/plan/gates.toml, docs/plan/04-mcp-and-integrations.md, docs/plan/06-fixtures.md, decisions.md
Description: D85. `plan_context` searches the bound repository's indexed files for task concepts no symbol is named after — at most eight per task: compounds, then salient words whose name query returned nothing — and adds the smallest indexed symbol containing each matching line to the candidate pool, where the D83 ranking, penalty, floor, and readiness rules apply unchanged. The scan is literal, read-only, and bounded by counts and bytes (512 KiB per file, 2,000 files, 32 MiB), never by time. The two `known_gap` retrieval tasks become gated and three body-only tasks, one with a common-word decoy, join the corpus.
Acceptance criteria:
- A symbol whose body alone spells a task compound reaches the pack and leads it; one whose body alone contains a salient task word reaches the pack; in both the excerpt shows the matching line.
- Secret paths, files above the per-file cap, text files for a task that does not ask for documentation, files changed since indexing, and unreadable or non-UTF-8 files are never scanned and never fail the plan; root resolution rejects paths outside the repository as for every other read.
- Reaching the file or byte cap sets `content_scan_truncated` and `truncated`; there is no wall-clock limit and two runs return equal plans.
- A single word found in more than eight symbols adds no evidence; an absent-concept task stays not ready.
- No store write, schema migration, or response field; D83's readiness rule is unchanged.
- `[retrieval]` thresholds are raised to the new measured values minus a margin and no threshold is lowered.
Test cases:
- `test_plan_context_body_only_compound_reaches_the_pack_and_leads_it`: compound in a body, smallest enclosing symbol, centred excerpt, determinism, no store writes.
- `test_plan_context_body_only_single_word_reaches_the_pack_with_the_matching_line`: single word, centred excerpt, coverage.
- `test_plan_context_content_scan_never_reads_secret_or_oversize_files`: secret path, file above the cap, missing file.
- `test_plan_context_content_scan_reads_text_files_only_when_the_task_asks_for_docs`: text files.
- `test_plan_context_content_scan_file_cap_sets_truncated`: file cap and both flags.
- `test_plan_context_content_scan_ignores_a_word_common_to_many_bodies`: common-word rule.
- `test_line_spells_word_matches_whole_identifier_words_only`: whole-word matching.
- `test_committed_retrieval_corpus_passes_committed_gates`, `test_gates_toml_retrieval_thresholds_match_d85` (renamed `..._match_d86` by E17-20): 20 gated gold tasks against the raised thresholds.

### [x] E17-20: Context pack carries use sites and referenced constants, not a broad-word tail
Owner: R2+PQ · Estimate: 2d · Depends: E17-17, E17-18, E17-19
Files: rust/compresso-atlas/src/queries.rs, rust/compresso-atlas/tests/queries_impact_plan.rs, rust/compresso-cli/src/commands/retrieval.rs, rust/compresso-cli/tests/cli_retrieval.rs, rust/compresso-eval/src/retrieval.rs, rust/compresso-eval/src/gates.rs, rust/compresso-eval/tests/retrieval.rs, fixtures/retrieval/tasks.toml, fixtures/retrieval/repo/, fixtures/corpus.toml, docs/plan/gates.toml, docs/plan/04-mcp-and-integrations.md, docs/plan/06-fixtures.md, decisions.md
Description: D86. Four changes to how `plan_context` fills its budget, none adding a scan pass or a dependency. (A) Up to three specific named compounds are use-site concepts: a symbol that defines or uses one may take a third evidence slot in its file, and a file missing such an excerpt does not count as included. (B) Constants that included excerpts reference by name are added after the referencing excerpt: at most four per pack, 40 tokens each, 16 index lookups. (C) Once the evidence that is above the score floor, uses a use-site concept, or matched a specific task word covers the task, candidates that matched only broad words are dropped. (D) The three best strong excerpts get one twelfth of the pack budget, between 96 and 224 tokens, and an excerpt that reaches the end of its body grows upward. The retrieval corpus gains `gold_symbols` and two tasks.
Acceptance criteria:
- A raise site that shares a file with the definition and another user of a named concept is in the pack; a file never holds more than three excerpts, and more than two only for use-site evidence.
- Use-site evidence comes from the one bounded content scan: a use site in a file above the per-file cap or past the file cap is not read, and the cap still sets `content_scan_truncated`.
- A pack that left a use-site excerpt out reports fewer included files than candidate files, so it is not `task_pack_ready` and `compresso_context` expands the budget.
- A constant an included excerpt references is rendered after it as `(Const, referenced above)`; it is listed in `ContextPlan.referenced_constants` and `plan.referenced_constants` of `compresso context --json`, and never counts toward evidence files, coverage, or `truncated`.
- Broad-word tail candidates are dropped only when the remaining evidence covers at least half of the task's salient words; a symbol named after a specific task word is kept.
- Every budget is hard: `tokens_used` never exceeds the requested budget, and two runs return equal plans.
- `[retrieval]` precision threshold is raised to the new measured value minus a margin; no threshold is lowered.
Test cases:
- `test_plan_context_use_site_of_a_named_concept_joins_its_definition_in_one_file`: definition leads, third slot for the raise site, two-per-file share for other symbols.
- `test_plan_context_is_not_file_complete_while_a_use_site_excerpt_is_left_out`: file completeness withheld at tight budgets.
- `test_plan_context_use_site_expansion_stays_within_the_content_scan_caps`: oversize file and file cap.
- `test_plan_context_includes_the_definition_of_a_constant_an_excerpt_references`: definition line, placement, accounting.
- `test_plan_context_referenced_constants_are_capped_in_count_and_tokens`: four constants, 40 tokens, tight budgets.
- `test_plan_context_drops_broad_word_tail_only_when_specific_evidence_covers_the_task`: tail dropped when covered, kept when not, specific-word symbol kept.
- `test_plan_context_top_evidence_gets_a_larger_excerpt_that_scales_with_the_budget`: 224-token ceiling, one twelfth of the budget, upward growth.
- `test_plan_context_expansion_never_exceeds_any_budget_and_is_deterministic`: eighteen budgets from 0 to 4,000, determinism, no store writes.
- `test_score_evidence_requires_each_gold_symbol_not_only_its_file`: gold-symbol scoring.
- `test_committed_retrieval_corpus_passes_committed_gates`, `test_gates_toml_retrieval_thresholds_match_d86`: 22 gated gold tasks against the raised threshold.


### [x] E17-21: One malformed import cannot abort a repository index
Owner: R2 · Estimate: 0.5d · Depends: E3-04
Files: rust/compresso-atlas/src/parsers.rs, rust/compresso-atlas/tests/parsers_python.rs, rust/compresso-atlas/tests/indexer_incremental.rs
Description: Parser error recovery can yield an import whose module name is a zero-width placeholder (a Python file containing `import ,`). The empty edge target was rejected by the store (`edge to_path must not be empty`) and `compresso index` failed for the whole repository with `index_failed`; found indexing a public 7,788-file repository whose parser test corpus contains such a file. Imports that name nothing are dropped at extraction, for every language. No parser version bump: a repository holding such a file could not be indexed before, so no stored index changes meaning.
Acceptance criteria:
- A repository containing a source file whose import statement has no module name indexes successfully; the other imports in that file are still recorded.
- No extracted import has an empty or whitespace-only `raw` value.
Test cases:
- `test_python_import_with_no_module_name_is_not_extracted`: `import ,\nimport x, y,\n` extracts exactly `x` and `y`.
- `test_index_survives_python_import_statement_with_no_module_name`: the fixture repository plus that file indexes nine files and records the file's named imports as unresolved edges.

### [x] E17-22: Bounded content search replaces shell grep for the bound repository
Owner: R2 · Estimate: 2d · Depends: E17-19, E17-07
Files: rust/compresso-atlas/src/grep.rs, rust/compresso-atlas/src/lib.rs, rust/compresso-atlas/src/queries.rs, rust/compresso-atlas/src/scanner.rs, rust/compresso-atlas/Cargo.toml, rust/compresso-atlas/tests/grep_content.rs, rust/compresso-mcp/src/tools/atlas_grep.rs, rust/compresso-mcp/src/tools/mod.rs, rust/compresso-mcp/src/server.rs, rust/compresso-mcp/Cargo.toml, rust/compresso-mcp/tests/tools_grep.rs, rust/compresso-mcp/tests/handshake.rs, rust/compresso-mcp/tests/tools_recall.rs, rust/compresso-cli/src/commands.rs, rust/compresso-cli/src/commands/retrieval.rs, rust/compresso-cli/tests/cli_retrieval.rs, rust/compresso-cli/tests/cli_app.rs, rust/compresso-core/src/transcript.rs, rust/compresso-store/src/ops.rs, rust/compresso-integrations/src/cursor.rs, rust/compresso-integrations/tests/launcher_upgrade.rs, Cargo.toml, Cargo.lock, docs/plan/03-interfaces.md, docs/plan/04-mcp-and-integrations.md, docs/runbooks/mcp-manual-verification.md
Description: D87. Benchmarks showed models calling Compresso and then running shell grep, because `compresso_search` is an exact symbol lookup and cannot return use sites; with shell search removed, 28 of 64 calls were `compresso_search` fanned out one identifier at a time and repeated with larger limits. A new read-only `compresso_grep` MCP tool and `compresso grep` CLI command search the bound repository's indexed files for one to eight literal or regular-expression patterns in one pass. Results are grouped per pattern, then per file, ranked (definition on a listed line first, non-test before test, more matches, path), each line as `LINE [SYMBOL] SNIPPET` with the enclosing indexed symbol. Exact `matches_total`/`files_total` are reported however far the listing is capped, with `complete`, `truncated`, and a note that says not to repeat with larger limits. Reads go through the existing gates; matching uses the linear-time `regex` crate (new workspace dependency, already in the lock graph). No existing tool is renamed or removed.
Acceptance criteria:
- Only indexed files are searched, in index path order, through canonical-root resolution; secret paths are never read even when an index lists them; files over 16 MiB and missing or non-UTF-8 files are skipped and counted.
- A file changed since indexing is searched as it is now, marked stale, and given no symbol names; the payload carries `stale` and `reindex_hint`.
- At most 8 patterns of 512 bytes; snippets at most 160 bytes; `max_files` (1-100, default 20), `max_matches_per_file` (1-50, default 5), and `max_tokens` (300-10,000, default 1,500) are hard caps; the scan stops at 20,000 files or 256 MiB, never at a deadline.
- Totals count every in-scope match; any cap sets `truncated` and clears `complete`.
- A long line, a pathological pattern, or a regular expression that could cross line breaks is searched in linear time (regular expressions are matched within each line); an invalid pattern or path filter is a typed `GrepError` (`invalid_arguments`) before any file is read.
- `tokens_used` never exceeds `max_tokens`; a budget too small for every pattern's totals is rejected.
- Path filter: a repository-relative prefix or a glob (`*`, `**`, `?`; no `/` means file name); case-insensitive flag.
- The tool description says it replaces shell text search for this repository; the default server instructions list it policy-neutrally as "text search of indexed files with use sites" (instructions that direct the client away from shell search exist only on opt-in surfaces, the lean profile and the exploration agent, under the instruction-wording rule as amended in D92).
Test cases:
- `test_grep_returns_use_sites_with_enclosing_symbols_grouped_and_ranked`, `test_grep_never_reads_secret_paths`, `test_grep_skips_files_above_the_read_ceiling`, `test_grep_reports_caps_and_totals_when_output_is_truncated`, `test_grep_stays_linear_on_one_very_long_line`, `test_grep_regex_never_runs_past_a_line_end`, `test_grep_rejects_invalid_patterns_with_a_typed_error`, `test_grep_case_flag`, `test_grep_path_filter_prefix_and_glob`, `test_grep_searches_changed_files_as_they_are_now_without_symbols`, `test_grep_skips_missing_and_non_utf8_files` (Atlas).
- `test_grep_tool_returns_use_sites_for_several_patterns_in_one_call`, `test_grep_tool_reports_totals_and_truncation_under_caps_and_budget`, `test_grep_tool_never_returns_secret_or_oversize_file_content`, `test_grep_tool_rejects_invalid_patterns_and_arguments`, `test_grep_tool_case_flag_and_path_filter`, `test_grep_tool_stays_linear_on_a_long_line_and_a_pathological_pattern`, `test_grep_tool_marks_changed_files_and_the_stale_index` (MCP).
- `test_grep_lists_use_sites_per_pattern_and_rejects_invalid_patterns` (CLI); tool-list, runbook, transcript, store-classification, and client-integration tool-set tests updated for the added tool (eleven tools on this branch; twelve, thirteen with recall, once merged with E17-25).

### [x] E17-23: Verbatim identifiers lead the context pack and numeric literals are searched
Owner: R2+PQ · Estimate: 1.5d · Depends: E17-20, E17-22
Files: rust/compresso-atlas/src/task_identifiers.rs, rust/compresso-atlas/src/queries.rs, rust/compresso-atlas/src/lib.rs, rust/compresso-atlas/tests/queries_task_named.rs, rust/compresso-store/src/symbols.rs, rust/compresso-eval/tests/retrieval.rs, fixtures/retrieval/tasks.toml, fixtures/corpus.toml, docs/plan/03-interfaces.md, docs/plan/04-mcp-and-integrations.md, docs/plan/06-fixtures.md
Description: D88. The planner lowercased the task before reading it, so a type named verbatim scored like any prose word (it ranked about thirteenth on a large repository), and numbers of two digits were never searched. `plan_context` now reads identifiers from the raw task (`CamelCase`, inner `_`, `::`-qualified, paths), resolves them through a new exact-name store lookup (`Store::symbols_named(repo, name, limit)`; not the call-graph lookup `Store::indexed_symbols_named` added by E17-24), and orders named symbols first in the pack; the first two also get use-site content-scan slots. Integer literals of two or more digits get two reserved content-scan slots with digit-boundary matching. The retrieval corpus gains three tasks.
Acceptance criteria:
- A type, function, or constant the task names verbatim leads the pack in an index where ninety files match the task's prose; a path the task names leads with that file's symbols.
- A name defined more than three times, or a bare file name more than two files end with, does not lead.
- A body holding a task literal (`320`, `45`) is evidence; `1320`, `3200`, `450` are not matches.
- The 22 earlier retrieval tasks are unchanged; every gate passes and none is lowered.
Test cases:
- `test_plan_context_named_type_leads_the_pack_in_a_large_index`, `test_plan_context_snake_screaming_and_path_identifiers_lead_prose_matches`, `test_plan_context_common_name_defined_many_times_does_not_lead`, `test_plan_context_bare_file_name_many_files_share_names_nothing`, `test_plan_context_numeric_literal_sites_become_evidence` (all fail without the change).
- `test_task_identifiers_keep_code_shaped_words_only`, `test_task_numbers_take_standalone_integers_of_two_or_more_digits`, `test_line_has_number_respects_digit_boundaries`, `test_symbols_named_matches_the_exact_name_only`.
- `test_committed_retrieval_corpus_passes_committed_gates`: 28 tasks, 25 gold; `two-digit-literal` (recall 0 before) and `named-error-raise` (false-ready before) pass.

### [x] E17-24: Record call sites in the index
Owner: R2 · Estimate: 2d · Depends: E3-04, E17-21
Files: rust/compresso-store/migrations/0008_call_sites.sql, rust/compresso-store/src/calls.rs, rust/compresso-store/src/lib.rs, rust/compresso-store/src/symbols.rs, rust/compresso-store/src/files.rs, rust/compresso-store/tests/migrations.rs, rust/compresso-atlas/src/parsers.rs, rust/compresso-atlas/src/parsers/calls.rs, rust/compresso-atlas/src/parsers/shell.rs, rust/compresso-atlas/src/parsers/text.rs, rust/compresso-atlas/src/indexer.rs, rust/compresso-atlas/tests/parsers_calls.rs, rust/compresso-atlas/tests/indexer_incremental.rs, rust/compresso-cli/src/commands/index.rs, rust/compresso-eval/src/runner.rs, docs/adr/0033-call-sites-resolved-at-query-time.md, docs/adr/0026-safe-shell-and-generic-text-indexing.md, docs/plan/02-schemas.md, docs/plan/03-interfaces.md
Description: D89. Multi-file trace tasks cost about twenty model turns because the index had symbols and file imports but no calls. The parsers walk the syntax tree they already built and record each call inside an indexed Rust, Python, JavaScript, TypeScript, or TSX function or method: callee name as written, optional qualifier, call form (`plain`, `path`, `self`, `receiver`, or `reference` for a bare name passed as a call argument, a function used as a value), and the line of the callee name; Rust calls inside macro arguments are read from the token tree. Migration `0008_call_sites.sql` adds `call_sites`, written in the caller file's transaction and cascading with its file and caller symbol. Rows hold no callee symbol id; the callee is resolved at query time (ADR-0033) through `Store::indexed_symbols_named(repo, name)` (every exact-name definition with its path; distinct from E17-23's bounded `Store::symbols_named(repo, name, limit)`). The parser-version suffix gains `+calls-v1`, so existing indexes reparse once.
Acceptance criteria:
- Every call inside an indexed function or method is recorded once per caller, callee name, qualifier, and form, with the first call site; calls at module level or in class bodies, macro invocations, and calls whose callee is not a name are not.
- A malformed file never yields an empty, garbled, or over-long callee name or qualifier and never aborts an index.
- Per-file bounds: 4,096 kept records and 65,536 examined call expressions; reaching either is reported (`Extraction.calls_truncated`, `IndexReport.call_site_files_truncated`). Tree and member-chain walks are iterative; the enclosing caller is found in O(1) per call.
- Re-indexing or deleting a file replaces or removes its call sites; no row ever names a callee symbol id.
- A store at migration 7 with indexed rows opens, migrates to 8, and keeps its rows; an index stamped by the previous parser version reparses every file.
- Full index time on this repository stays within 25% of the previous build (measured: about 15%).
Test cases:
- `test_rust_calls_record_form_qualifier_caller_and_line`, `test_python_calls_record_self_dotted_and_receiver_forms`, `test_script_calls_record_this_member_new_and_skip_require`, `test_rust_calls_inside_macro_arguments_are_recorded`: forms, qualifiers, callers, lines per language.
- `test_names_passed_as_call_arguments_are_recorded_as_references`: Rust and Python, including keyword arguments.
- `test_repeated_calls_keep_the_first_site_per_caller_and_form`, `test_calls_per_file_are_capped`, `test_shell_and_text_record_no_calls`.
- `test_malformed_calls_never_yield_empty_or_garbled_names`: Rust, Python, JavaScript, TypeScript, TSX.
- `test_very_deep_member_chain_does_not_overflow_and_is_a_receiver_call`: 40,000-deep chain on a 256 KiB stack.
- `test_huge_number_of_calls_and_functions_is_bounded_and_reports_truncation`, `test_a_file_with_a_huge_number_of_calls_never_aborts_the_index`.
- `test_index_records_call_sites_and_survives_malformed_calls`, `test_default_parser_version_marks_call_site_extraction`.
- `test_call_sites_are_replaced_with_their_file_and_cascade_on_delete`, `test_invalid_call_sites_are_rejected_before_any_write`, `test_indexed_symbols_named_and_by_ids_stay_inside_the_repository`.
- `test_migration_8_opens_a_store_indexed_before_call_sites`, `test_open_fresh_creates_all_tables` (14 tables), migration-ledger tests updated for version 8.

### [x] E17-25: One call answers who calls X, what X calls, and how A reaches B
Owner: R2 · Estimate: 2d · Depends: E17-24, E5-02
Files: rust/compresso-atlas/src/trace.rs, rust/compresso-atlas/src/lib.rs, rust/compresso-atlas/src/queries.rs, rust/compresso-atlas/tests/queries_trace.rs, rust/compresso-mcp/src/tools/atlas_trace.rs, rust/compresso-mcp/src/tools/mod.rs, rust/compresso-mcp/src/server.rs, rust/compresso-mcp/tests/tools_trace.rs, rust/compresso-mcp/tests/handshake.rs, rust/compresso-cli/src/commands.rs, rust/compresso-cli/src/commands/retrieval.rs, rust/compresso-cli/src/commands/retrieval/trace.rs, rust/compresso-cli/src/lib.rs, rust/compresso-cli/tests/cli_trace.rs, rust/compresso-cli/tests/cli_app.rs, rust/compresso-core/src/transcript.rs, rust/compresso-store/src/ops.rs, rust/compresso-integrations/src/cursor.rs, rust/compresso-integrations/tests/launcher_upgrade.rs, docs/plan/03-interfaces.md, docs/plan/04-mcp-and-integrations.md, docs/runbooks/mcp-manual-verification.md
Description: D90. New Atlas query `trace`, MCP tool `compresso_trace`, and CLI `compresso trace`. `to` alone lists callers, `from` alone lists callees (breadth first, default depth 1), both give the shortest call paths (default depth 6). Each hop carries symbol, kind, definition `path:line`, call-site `path:line`, a one-line signature, and the resolution rule. Call sites are resolved at query time: candidates by name narrowed by call form, then the only candidate in the caller's file, in a file it imports, or in the repository (`name_only` for a method call on an expression of unknown type). Only resolved calls are followed; ambiguous calls are listed, library calls ignored. No existing tool is renamed or removed.
Acceptance criteria:
- Bounds: `max_depth` 1–8, `max_paths` 1–10, `max_results` 1–500, 2,000 visited symbols, 10,000 path-enumeration steps, `max_tokens` (default 4,000) over the returned list; `total`, `shown`, `truncated`, `depth_limited`, and `complete` let one call stand as the answer.
- An ambiguous name returns candidates and traces nothing; `path:name` and `Type::name` select one.
- Cycles and recursion terminate and are marked `cycle`; a symbol is never expanded twice.
- A function passed by name as a call argument is listed as a caller with `reference: true`; a call never resolves across languages; `depth_limited` is reported for path searches and explicit depths only; `unfollowed` shares the token budget.
- An index stamped before call sites returns `reindex_required`; a stale index is answered with `stale: true` and the reindex hint.
- Same repository binding, approval, and indexed-repository gates as the other repository tools; secret paths are never returned; nothing is executed or written to the store; the MCP call records one retrieval operation.
Test cases:
- Atlas: `test_trace_finds_the_shortest_call_path_across_files`, `test_trace_reports_no_path_when_none_exists`, `test_trace_returns_candidates_for_an_ambiguous_name_and_labels_ambiguous_calls`, `test_trace_depth_cap_stops_the_search_and_says_so`, `test_trace_is_cycle_safe_and_marks_recursion`, `test_trace_callers_report_call_sites_totals_and_truncation`, `test_trace_resolves_self_type_and_python_calls`, `test_trace_labels_a_receiver_call_resolved_only_by_a_unique_name`, `test_trace_never_resolves_a_call_across_languages`, `test_trace_lists_a_function_passed_as_a_value_as_a_reference_caller`, `test_trace_reports_unknown_names_and_invalid_options`, `test_trace_requires_a_reindex_when_the_index_predates_call_sites`, `test_trace_writes_nothing_to_the_store`.
- MCP: `test_trace_tool_returns_a_call_path_with_sites_and_signatures`, `test_trace_tool_reports_no_path`, `test_trace_tool_returns_candidates_for_an_ambiguous_name`, `test_trace_tool_depth_cap_is_reported`, `test_trace_tool_is_cycle_safe_and_marks_recursion`, `test_trace_tool_callers_report_total_and_truncation_in_one_call`, `test_trace_tool_reports_a_stale_index`, `test_trace_tool_requires_reindex_when_the_index_predates_call_sites`, `test_trace_tool_rejects_invalid_arguments_and_unbound_sessions`, `test_trace_tool_is_listed_and_records_a_retrieval_operation`; handshake tool list and runbook row.
- CLI: `test_cli_trace_prints_a_call_path_as_json_and_text`, `test_cli_trace_requires_an_end_and_valid_bounds`, `test_canonical_command_snapshot`.

### [x] E17-26: Module cards give cheap, deterministic orientation
Owner: R2 · Estimate: 1.5d · Depends: E3-12, E5-02
Files: rust/compresso-atlas/src/cards.rs, rust/compresso-atlas/src/lib.rs, rust/compresso-atlas/src/queries.rs, rust/compresso-atlas/tests/module_cards.rs, rust/compresso-mcp/src/tools/atlas_nav.rs, rust/compresso-mcp/src/server.rs, rust/compresso-mcp/tests/tools_nav.rs, rust/compresso-cli/src/commands/retrieval.rs, rust/compresso-cli/tests/cli_retrieval.rs, scripts/doc-drift-lint.config.json
Description: On unfamiliar repositories, conceptual questions ("how does recovery work") need an orientation layer cheaper than reading files. `compresso_repo_map` (and `compresso map`) gains opt-in per-directory module cards built only from the index (D91): a purpose line from the module or file doc comment, top public symbols with kinds, entry points, internal dependencies and dependents from resolved import edges, and file and symbol counts. No model call; bounded by card count and token budget; `path` zooms into a subtree. The existing result is unchanged unless cards are requested.
Acceptance criteria:
- Without `cards`/`path` the MCP and CLI map results are byte-for-byte the previous contract; card fields are additive and opt-in.
- Cards are deterministic for one index, write nothing to SQLite, and never include secret paths or edges that touch them.
- Cards plus map stay within `max_tokens`; cards take at most three quarters of it; at most 12 cards (CLI `--max-cards` 1-50); `cards_truncated` reports omission.
- Purpose text comes only from in-root, non-secret files that still match their index record, reading at most 4 KiB of at most three files per card.
- Approval, session-root, `not_indexed`, and `stale` behavior are unchanged; an unsafe `path` is `invalid_arguments`.
Test cases:
- `test_cards_summarize_each_directory_deterministically`, `test_card_dependencies_come_from_import_edges`, `test_cards_exclude_secret_paths_and_their_edges`, `test_cards_respect_card_cap_and_token_budget`, `test_card_path_zooms_into_a_subtree_and_rejects_unsafe_prefixes`, `test_stale_files_never_contribute_a_purpose_line`, `test_cards_leave_the_existing_repo_map_unchanged` (Atlas); four `cards::tests` unit tests for doc extraction and visibility.
- `test_repo_map_omits_card_fields_unless_requested`, `test_repo_map_cards_are_bounded_and_zoomable`, `test_repo_map_cards_require_an_indexed_bound_repository` (MCP); `test_map_cards_are_opt_in_bounded_and_zoomable` (CLI).

### [x] E17-27: Opt-in lean MCP tool profile
Owner: R2 · Estimate: 1d · Depends: E5-01, E17-02
Files: rust/compresso-mcp/src/profile.rs, rust/compresso-mcp/src/server.rs, rust/compresso-mcp/src/lib.rs, rust/compresso-mcp/tests/tool_profile.rs, rust/compresso-cli/src/commands/mcp.rs
Description: Preloading every Compresso tool schema adds fixed cost to each session. `compresso mcp serve --tools lean` (D92) advertises only a data-driven exploration allowlist (`LEAN_TOOLS` in `profile.rs`: context, repo_map, search, grep, trace, outline, unfold, read, and recall only when `--recall-source` enables it) with one-sentence descriptions, no compatibility-only `root` argument, and shorter instructions generated from the served rows. `compresso_grep` and `compresso_trace` joined the allowlist as one `LeanTool` row each when E17-22 and E17-25 merged. The default profile, tool list, descriptions, and instructions are unchanged.
Acceptance criteria:
- `mcp serve` without `--tools`, and every installed client entry, serves exactly the default tools (twelve after the E17-22/E17-25 merge, thirteen with recall) and the default instructions.
- `--tools lean` lists exactly the allowlist, in order; every allowlisted name is a registered tool.
- Calling a registered tool that lean does not serve returns `isError` text starting `[tool_unavailable]`, naming the tool and the restart remedy, and dispatches nothing; unknown names keep the protocol error.
- Lean instructions keep the no-network, model-provider, and untrusted-content disclosures and name only served tools.
- Lean fixed cost (tools/list payload plus instructions) is under half of full for both counters. Measured after the merge (eight lean tools, twelve full): o200k 4,033 → 2,011 tokens; Anthropic-calibrated 4,639 → 2,314 (about 49.9% of full; at the E17-27 commit, before grep and trace: 2,801 → 1,224 and 3,222 → 1,408).
Test cases:
- `test_default_profile_tool_list_and_instructions_are_unchanged`, `test_lean_profile_advertises_exactly_the_allowlist`, `test_lean_initialize_sends_the_lean_instructions`, `test_lean_profile_rejects_a_hidden_tool_with_a_clean_error`, `test_lean_profile_with_operator_enabled_recall_serves_recall_last`, `test_lean_allowlist_names_only_registered_tools`, `test_lean_instructions_keep_the_mandatory_disclosures_and_name_only_served_tools`, `test_lean_schema_and_instructions_cost_less_than_half_of_default`, `test_profile_names_round_trip`; CLI `test_tool_profile_is_full_by_default_and_lean_only_explicitly`.

### [x] E17-28: Opt-in Claude Code exploration agent definition
Owner: D1 · Estimate: 1.5d · Depends: E6-02, E17-27
Files: rust/compresso-integrations/src/explore_agent.rs, rust/compresso-integrations/src/plan.rs, rust/compresso-integrations/src/server_entry.rs, rust/compresso-integrations/src/lib.rs, schemas/integrations/v1.json, rust/compresso-cli/src/commands.rs, rust/compresso-cli/src/commands/integrations.rs, rust/compresso-cli/tests/cli_explore_agent.rs, docs/adr/0034-whole-file-agent-definition-ownership.md
Description: Models use Compresso in addition to shell search, not instead of it. `compresso integrations install claude-code --explore-agent` (D93, ADR-0034) writes `<project>/.claude/agents/compresso-explorer.md`: a Claude Code subagent whose `tools` are the lean exploration allowlist (minus recall) as `mcp__compresso__*` plus `Read, Edit, Write`, and whose instructions route exploration through Compresso. The integration framework gains a whole-file `ConfigFormat::Markdown` whose uninstall deletes the file, with the same preview, receipt, journal, and recovery guarantees. Never installed by default.
Acceptance criteria:
- `setup` and plain `integrations install` never create the file; `--dry-run` writes nothing.
- Install creates the file with a receipt; re-install is a no-op; a same-named file Compresso did not write is refused with `user_file_present` and unchanged; symlinked `.claude` or `.claude/agents` is refused.
- Uninstall deletes an unchanged file with no backup left behind and retains an edited one with `key_modified`; crash recovery after deletion, before or after receipt persistence, completes without recreating the file.
- `status`/`verify --explore-agent` report presence, marker, and currency; other clients are `unsupported`; the flag conflicts with `--compaction-hook` and `--no-recall`.
- Tests use only temporary home and project directories.
Test cases:
- `explore_agent::tests` (7): render shape, install/uninstall tree restore, preview writes nothing, user file refused, edited file retained, idempotent reinstall, symlink refused.
- `plan::tests::test_whole_file_uninstall_recovers_after_deletion_before_or_after_receipt_save`, `test_whole_file_format_owns_exactly_one_path`.
- `cli_explore_agent` (5): `test_setup_and_default_installs_never_create_the_agent`, `test_agent_dry_run_previews_and_writes_nothing`, `test_agent_install_status_verify_and_uninstall_round_trip` (tools tied to the lean allowlist), `test_agent_is_claude_code_only_and_exclusive_with_other_surfaces`, `test_agent_install_refuses_a_user_agent_of_the_same_name`.

### [x] E17-29: Review-round hardening, grep context lines, and multi-range read
Owner: R2 · Estimate: 2.5d · Depends: E17-22, E17-24, E17-25, E17-27, E17-28
Files: rust/compresso-atlas/src/parsers.rs, rust/compresso-atlas/src/parsers/calls.rs, rust/compresso-atlas/src/indexer.rs, rust/compresso-atlas/src/trace.rs, rust/compresso-atlas/src/queries.rs, rust/compresso-atlas/src/grep.rs, rust/compresso-atlas/src/cards.rs, rust/compresso-atlas/src/lib.rs, rust/compresso-atlas/tests/{parsers_calls,indexer_incremental,queries_trace,symlink_swap,grep_content,queries_unfold_search_read}.rs, rust/compresso-store/src/{calls,symbols,lib}.rs, rust/compresso-mcp/src/{server,profile}.rs, rust/compresso-mcp/src/tools/{atlas_read,atlas_grep,common}.rs, rust/compresso-mcp/tests/{tools_read,tools_grep,tool_profile}.rs, rust/compresso-integrations/src/explore_agent.rs, rust/compresso-cli/src/commands/{index,retrieval}.rs, rust/compresso-cli/tests/{cli_index,cli_retrieval}.rs, rust/compresso-eval/src/runner.rs, docs/plan/03-interfaces.md, docs/plan/04-mcp-and-integrations.md
Description: A review of the E17 call-graph, grep, lean-profile, and explorer-agent work found that untrusted repository content could abort indexing, make a trace quadratic, or reach a secret file through a symbolic link, plus several smaller defects. All are fixed. Benchmark traces also showed about 2 `compresso_grep` calls followed by about 3.7 single-range `compresso_read` calls per task; `compresso_grep` gains `context_lines` and `compresso_read` gains `ranges` so one call returns what took several turns.
Acceptance criteria:
- A call qualifier written in macro arguments (`assert!(<300 A's>::make())`) is dropped by the one record path that every call form passes (valid names joined by `::` or `.`, at most 256 bytes). If the store would still reject a file's call sites, that file is indexed without call sites and counted in `IndexReport.call_site_files_rejected`. `index_failed` names its cause chain on one line of at most 512 bytes.
- Callers of one of many same-named definitions resolve in about one lookup per call site: candidates are held by id, filtered once per call shape and grouped by path, and resolutions are cached per calling file. A trace reads at most 100,000 call-site rows (`TRACE_MAX_CALL_SITE_ROWS`) in a fixed SQL order and sets `truncated` when it reaches the cap. On the generated 1,000- and 2,000-file repositories, `trace --to pkg/m0.py:handle` drops from 6.0 s and 23.4 s to 0.19 s and 0.36 s (release build).
- Repository files are served only through `resolve_under_root`. It refuses a secret canonical target and any symbolic-link component below the root, and reports both as `not_found`. On Unix the reader opens only the inode the gate approved, so a file or parent swapped for a link after the check is never read. A link that escapes the root is still `outside_root`. Hard links are served (04 §1.5a). Read, grep, unfold, outline staleness, the context-pack content scan, and module-card purpose lines all pass this gate.
- Grep reports `complete: false` when in-scope files were skipped, and the MCP `note` names the skipped count. Snippets and a symbol lookup are kept only for files that can still rank within `max_files`, and the result equals sort-then-truncate. The compiled regex limit is 256 KiB, and each regex pattern searches at most 64 MiB, after which `scan_truncated` is set.
- A call is attributed to its enclosing function by byte range, so functions that share a line each own their calls. Lines remain the fallback. There is no parser version bump.
- `trace` checks for the `+calls-v1` marker, which `with_parser_version` appends, instead of the default version string.
- A lean server asked for `compresso_recall` without `--recall-source` says to start the server with `--recall-source`.
- The explorer agent's instructions name every tool in the 8-tool lean list, and its test checks that list.
- The store's two lookups by symbol name are documented as distinct: search versus call resolution.
- `compresso_grep` and `compresso grep -C` accept `context_lines` from 0 to 10. Context lines are merged, cut like snippets, and counted with their match inside `max_tokens`. With the default 0 the output is unchanged.
- `compresso_read` accepts `ranges`, up to 8 `{path, start_line, end_line}` (CLI: `--range PATH[:START[-END]]`). Each range passes every read gate on its own and fails on its own with the single-read error code. The budget is shared in request order as equal shares of what is left. `gate` and the single-path arguments are refused alongside `ranges`, and the single form is unchanged.
- The full and lean descriptions mention both features. The lean surface stays under half of the full one.
Test cases:
- `test_an_oversized_or_garbled_qualifier_drops_only_that_call`, `test_an_oversized_macro_call_qualifier_never_aborts_the_index`, `test_call_sites_the_store_rejects_are_dropped_without_failing_the_file`, and `test_index_failed_names_the_cause_chain_on_one_bounded_line`.
- `test_trace_callers_scale_with_many_same_named_definitions` (1,500 files under an 8 s bound in debug; 19.8 s before the fix), `test_call_site_row_budget_truncates_a_trace_deterministically`, `test_trace_works_on_an_index_built_with_a_custom_parser_version`, and `test_every_stamped_parser_version_carries_the_call_graph_marker`.
- `symlink_swap.rs` (four tests covering read, grep, unfold, outline, context, cards, staleness, internal links, directory links, and escapes), `test_a_file_swapped_for_a_symlink_after_resolution_is_never_opened`, `test_a_parent_directory_swapped_for_a_symlink_after_resolution_is_never_opened`, `test_hard_linked_ordinary_files_are_served`, and `test_indexed_file_swapped_for_a_symlink_to_a_secret_is_never_served` (MCP).
- `test_grep_keeps_only_the_files_that_rank_and_matches_sort_then_truncate`, `test_grep_context_lines_surround_each_match_and_merge_when_overlapping`, `test_regex_byte_budget_stops_a_pattern_and_marks_the_scan_truncated`, `test_grep_tool_context_lines_surround_matches_within_the_token_cap` (MCP), and the skipped-file assertions in the existing oversize and unreadable tests.
- `test_functions_sharing_a_line_each_own_their_calls`.
- `test_read_bounded_ranges_share_one_budget_and_gate_each_range`, `test_read_ranges_reads_several_ranges_under_one_budget_and_gates_each` (MCP), and `test_grep_context_lines_and_multi_range_read` (CLI).
- `test_lean_profile_rejects_a_hidden_tool_with_a_clean_error` (recall message) and the explorer render test (8 tools, every granted tool mentioned).

### [x] E17-30: Several-part tasks are ready only when every part is covered
Owner: R2 · Estimate: 1.5d · Depends: E17-29
Files: rust/compresso-atlas/src/task_facets.rs, rust/compresso-atlas/src/queries.rs, rust/compresso-atlas/src/lib.rs, rust/compresso-atlas/tests/queries_task_facets.rs, rust/compresso-mcp/src/tools/atlas_read.rs, rust/compresso-cli/src/commands/retrieval.rs, rust/compresso-eval/src/retrieval.rs, rust/compresso-eval/tests/retrieval.rs, rust/compresso-atlas/tests/queries_impact_plan.rs, fixtures/retrieval/repo/engine/src/{report_cache,lib}.rs, fixtures/retrieval/tasks.toml, fixtures/corpus.toml, docs/plan/04-mcp-and-integrations.md, docs/plan/06-fixtures.md
Description: In session benchmarks one two-part question (how a query decides that what it stored about a file or the whole repository is out of date, and how that check stays cheap for large files) got `task_pack_ready` from `compresso_context` with none of `repository_stale`, `outline_current`, or the size/mtime fast path in the pack, and the answer missed a fact in every round. Three causes: readiness counted salient words over the whole pack, so one-word matches in unrelated excerpts covered "half the task"; the two-per-file share went to two constants named after the concept, ahead of the functions that use them; and the prose idiom "out of date" and a trailing "Cite source files… Do not modify any files" added words no code spells. The planner now splits the task into parts (D95), drops later instructions to the answerer, reads three staleness idioms as `stale`, chooses each part's best evidence plus same-file symbols related to it by a call, and reports a part no excerpt covers.
Acceptance criteria:
- A task is split into clauses at `.`, `;`, `:`, `?`, `!` followed by a space or the end, at parentheses and brackets, and where `and` or `or` opens a new question (a comma alone does not split); `file.rs` and `a::b` stay whole. A later clause starting with `cite`, `do not`, or `don't` is dropped; the first clause is always kept. `out of date`, `up to date`, and `outdated` (whole words, spaces or hyphens) read as `stale`; `out_of_date` is unchanged. Identifiers and numbers are still read from the raw task (D88).
- Search queries take each clause's salient words in turn, then the question scaffolding, so the 20-query limit cannot starve the last clause.
- Each clause naming a salient word or a 2+-digit number is a facet. A facet is covered when the included non-test excerpts (tests count when the task asks about tests) together mention at least half of its words and one excerpt mentions 1 word of a 1–2-word facet, else max(2, ⌈n/3⌉). `concept_coverage_sufficient` requires every facet; `ContextPlan::evidence_ready` is the one readiness rule used by MCP, the eval runner, and the planner tests.
- For two or more facets, each facet chooses the candidate mentioning most of its words from the pool plus every leaf symbol of the three leading non-test files (content-scan file rules), and up to two same-file symbols that call it, that it calls, or that call what it calls join when they also reach the per-excerpt bar. Chosen excerpts lead their file's items, stay outside the per-file share and the relative floor, make their file incomplete when they do not fit, get at least 96 tokens, and start just above the line calling another chosen symbol of their file, else the line naming most of the clause.
- `task_coverage` (MCP and CLI JSON) adds `facets_covered`, `facets_total`, and up to four `uncovered_facets` (80 characters); an uncovered facet gives `targeted_follow_up_needed` with a `retrieval_note` naming `task_coverage.uncovered_facets`.
- Retrieval corpus: `engine/src/report_cache.rs` and three tasks (`cache-staleness-prose`, `-terse`, `-paraphrase`) with the helper and both callers as gold symbols. Before: recall 0.5 and ready on each (`false_ready` 3, mean recall 0.9464, two gates failing). After: 31 tasks, recall 1.0, precision 0.3784 (0.34 gate), `false_ready` 0, `ready_mismatches` 1; the 28 earlier tasks' evidence is identical with and without the planner change. No gate changed.
- Scratch clone of the repository at the benchmark commit: the three task strings the model actually sent now end `task_pack_ready` at 3,200 with `repository_stale`, `outline_current`, `file_matches_index`, and `freshness_from_metadata` excerpted at their decision lines, plus the `content_matches_index` call and the 64 KiB buffer constant (the hashing loop itself is not excerpted; before: ready at 1,600 with none); the verbatim prompt ends `targeted_follow_up_needed` naming its first clause (before: ready with none). Per-plan time +~15 ms; MCP median for the model strings 360–400 ms → 645–842 ms because readiness now needs the 3,200 stage.
Test cases:
- `test_plan_context_multi_part_task_without_evidence_for_one_part_is_not_ready`, `test_plan_context_multi_part_task_includes_the_helper_and_both_of_its_callers`, and `test_plan_context_reads_out_of_date_as_stale` (all three fail on db0043b), `test_plan_context_names_the_uncovered_part_of_the_task`, `test_plan_context_one_part_tasks_keep_their_readiness`.
- `test_plan_context_drops_broad_word_tail_only_when_specific_evidence_covers_the_task` now expects one `src/mcp/` item: the task's `MCP server:` part keeps its best evidence; the rest of the broad tail is still dropped.
- `task_facets` unit tests: clause splitting, instruction dropping (first clause kept, code nouns kept), idiom bridging (hyphens, case, identifiers untouched).

### [x] E17-31: Compact text tool results for the lean profile
Owner: R2 · Estimate: 2d · Depends: E17-27, E17-29
Files: rust/compresso-mcp/src/{profile,server,lib}.rs, rust/compresso-mcp/src/tools/{mod,text,common,atlas_nav,atlas_grep,atlas_read,atlas_trace}.rs, rust/compresso-mcp/tests/{result_format,tool_profile}.rs, rust/compresso-mcp/tests/golden/{full_profile_json,lean_profile_text}.txt, rust/compresso-cli/src/commands/mcp.rs, docs/plan/03-interfaces.md, docs/plan/04-mcp-and-integrations.md
Description: In a 39-turn benchmark session on the lean profile, Compresso's tool results totalled 177k characters against 148k for a plain shell session, because every result was one JSON document: keys repeated per item, operation ids, token counters, nulls, and JSON escaping of every newline, quote, and backslash in source text. A lean server now sends each successful result as compact plain text built from the same internal payload (D96). The full profile's JSON results are unchanged byte for byte.
Acceptance criteria:
- `McpResultFormat::{Json, Text}`; `McpToolProfile::default_results()` is `Json` for `full` and `Text` for `lean`; `McpServeOptions.results` and `compresso mcp serve --results json|text` override it (at most once; any other value is `invalid_arguments`). No installer or default path writes `--results`.
- Context, repo_map (with cards), search, grep, trace, outline, unfold, read (single, gate, `ranges`), and recall have a text form: raw repository text with no JSON escaping, one header per file, range, or pattern (`@@ ` prefix for ranges), no repeated keys, no operation id, schema version, token counter, provider, or echoed argument, and each non-default state (truncation with how to continue, totals beyond what is shown, stale index, per-range errors, `next_action` and follow-up limit, coverage and uncovered concepts, ambiguity candidates, trace resolution and `ref` labels, unfollowed calls, depth limit, elision, diagnostics, recall notice and redactions) as one trailing `[...]` line.
- Compress, stats, explain, and impact send JSON in both forms; errors keep their `[code] message` text in both forms.
- The text never exceeds `max_tokens`: list tools fit rows as text (`cap_rows`), grep and context measure rendered text, and the dispatcher re-fits with a growing reserve (up to four attempts) before cutting at a line boundary with `[output cut to max_tokens]`. `tokens_served`, `observed_tokens`, and `applied_tokens` count the text sent.
- Operations are recorded, and stats and explain work, exactly as before; tools still build the JSON payload internally.
- The lean and full tool schemas and instructions are unchanged (no per-call `format` or `line_numbers` argument: the lean surface has about 5 tokens of margin under the half-of-full test).
- On the 34-call replay of the benchmark session (`--tools lean`, `--results json` vs. text, o200k ledger tokens): 169,946 -> 129,730 characters (-23.7%) and 44,911 -> 32,110 tokens (-28.5%), with the same grep rows, read lines, and map listed; about 86% of the text form is repository content, so the 30% character target is not reached without dropping requested source.
Test cases:
- `test_full_profile_results_are_byte_for_byte_unchanged` (golden written from the code before this ticket; masks only the operation id, the temporary root, and grep/context `tokens_used`, which counts the random id).
- `test_lean_profile_text_results_match_their_goldens` (every lean tool in success, truncation, ambiguity, empty, stale-hint, and per-range error states) and `test_each_profile_serves_the_other_form_when_the_operator_asks`.
- `test_errors_and_read_gates_are_identical_in_both_forms` (secret path, symlink, 16 MiB ceiling, argument error; multi-range read with three refused ranges).
- `test_text_form_carries_source_verbatim_without_json_escaping`, `test_text_form_never_exceeds_max_tokens` (eight tools at five budgets), `test_text_form_states_truncation_and_continuation`, `test_text_form_reports_a_stale_index`, and `test_ledger_records_the_tokens_of_the_text_sent`.
- Unit tests: `test_symbol_label_keeps_owner_and_drops_redundant_kind`, `test_read_continuation_follows_a_whole_line_and_repeats_a_cut_line`, `test_default_state_adds_no_trailing_lines`, `test_cut_text_keeps_whole_lines_and_fits_the_budget`, and `test_result_form_follows_the_profile_unless_chosen_explicitly` (CLI).

### [x] E17-32: Opt-in verified edit tool (`compresso_edit`, `compresso edit`)
Owner: R1 · Estimate: 3d · Depends: E17-27, E17-28, E17-29, E17-31
Files: rust/compresso-atlas/src/edit.rs, rust/compresso-atlas/src/{lib,grep,queries}.rs, rust/compresso-atlas/tests/edit.rs, rust/compresso-store/src/{lib,ops}.rs, rust/compresso-mcp/src/tools/{edit,mod,text}.rs, rust/compresso-mcp/src/{server,profile,lib}.rs, rust/compresso-mcp/tests/tools_edit.rs, rust/compresso-mcp/tests/tool_profile.rs, rust/compresso-mcp/tests/golden/lean_edit_text.txt, rust/compresso-cli/src/commands/{edit,mcp,integrations,retrieval}.rs, rust/compresso-cli/src/{commands,surface}.rs, rust/compresso-cli/tests/{cli_edit,cli_explore_agent,cli_app}.rs, rust/compresso-integrations/src/{claude_code,explore_agent,server_entry,lib}.rs, docs/adr/0035-opt-in-verified-edit-tool.md, docs/adr/0007-mcp-primary-integration-rmcp.md, docs/plan/{00-overview,03-interfaces,04-mcp-and-integrations,08-ip-and-secure-deployment}.md, docs/runbooks/mcp-manual-verification.md, README.md, compresso-threat-model.md
Description: A benchmark model renamed an identifier with BSD `sed` and `\b`, which changed nothing, and reported success; a competing setup with a verified edit tool made all 8 edits. D97 (ADR-0035) adds one opt-in write tool; the owner decided to build it, and confirmed the design choices listed in D97 on 2026-11-19. `compresso mcp serve --edits` (either profile) serves `compresso_edit`: `replace` (exact literal, `expected_count`), `rename` (whole identifier, explicit Unicode boundary rule; matches next to an escape, literal prefix, printf conversion, or Rust lifetime are reported as adjacent and kept; `$` handled by file language (separate identifier in JS/TS/Java, adjacent in Scala/Dart, the variable in shell/PHP/Perl/Kotlin); the index is refreshed first so new files are in scope; `allow_existing` for collisions), and `insert` (unique anchor), up to 16 per call, all or nothing, no regex, no file creation or deletion. Targets pass the shared read gate plus edit gates (indexed, not secret, not `.git`, not a known control file (any component, any case), not scanner-excluded, owned by this user with the owner write bit set and writable per the OS query `faccessat(AT_EACCESS)` on the file and its directory for files that will change (dry and real runs alike; a rename sweep skips and lists such a file; nothing opened for writing), not the store, UTF-8, at most 1 MB before and after). Writes stage edited bytes and original copies, re-verify device, inode, and hash before each rename, keep permissions, line endings, and BOM, and roll back by renaming originals back, keeping the original copy of any file that cannot be restored. The report lists site lines, a rename's remaining whole, substring, and adjacent matches and `complete`, call-level safety totals and `index_refreshed` that no truncation drops, and a dry-run diff; the index is refreshed after a write. `compresso edit` is the operator-run form. `integrations install claude-code --edits` writes `--edits` into the server entry; `--explore-agent --edits` grants the agent the tool. Defaults are unchanged.
Acceptance criteria:
- A server without `--edits` lists the same tools, instructions, and results as before (golden files unchanged) and answers `compresso_edit` with `[tool_unavailable]` naming `--edits` in both profiles; nothing is written.
- With `--edits`, the tool is the only one annotated not read-only; the full instructions replace only the read-only claim (policy neutral); the lean instructions and the agent add the multi-site edit and rename rule.
- Every gate refuses before writing: outside root, `..`, symbolic-link component, symlink swapped after validation, secret path (reported `not_found`), gitignored, unindexed, control file (any depth, any case), read-only, oversize before or after, non-UTF-8, `.git`, store files or store directory inside the repository.
- `expected_count` mismatch, missing anchor, no match, name collision, hash mismatch, and every bound write nothing; a failure while staging changes no file; a failed rename restores already renamed files unless they changed again.
- Renaming `format_utc` leaves `format_utc_now`, `reformat_utc`, `x.format_utc2` untouched; escapes, literal prefixes, `%d`, and `'a` are kept and reported as adjacent while `\App\User`, `\\n`, and `foo"` are renamed; `$name` follows the file language; files created since the last index are renamed; renames definitions, calls, and doc comments, and reports leftovers as substring matches; CRLF, BOM, and permissions are preserved; the dry-run diff is exact; the index is fresh after a write; the edit is recorded under its own ledger kind and stats and explain still work.
- The report never exceeds `max_tokens` (minimum 300), including 16 operations, and the edit is applied once; `incomplete_operations`, remaining totals, `files_skipped_total`, and `unverified_files` survive every cut, and the text header says INCOMPLETE.
- Replay: in a copy of the project at an older commit, `compresso edit --rename format_utc format_rfc3339_utc --path rust/compresso-cli/src/commands/decisions.rs --expected-count 8` changes 8 sites, keeps `test_format_utc_matches_known_instants`, and reports 0 remaining whole matches.
Test cases:
- `compresso-atlas` `edit::tests` (11) and `tests/edit.rs` (34): second-review cases (backslash boundary, literal prefixes, printf and lifetimes, `$` by language, files created since the index, more control files at any component, temporary files never indexed, non-text files, writability probe), plus boundaries, escapes and prefixes, control paths, shared scan budget, rollback restored and unrestored, staging failure, file changed or symlink swapped after validation, diff; each gate, bounds (sites, files, write bytes, operations, text size), CRLF/BOM, permissions, read-only, dry run, index freshness, stale files, hash precondition, name collision, insert, composition and site lines.
- `compresso-store` `ops::tests::test_edit_operations_record_and_count_no_savings`, `test_store_reports_its_database_path`.
- `compresso-mcp` `tests/tools_edit.rs` (9): safety totals survive any cut, store protected under a non-canonical root, default server refuses, full profile serves, records, re-indexes, explain and stats; error codes; tiny budget applies once; 16-operation cap; lean golden; lean instructions; `server::tests` annotations and instructions; `tool_profile` edit-tool cost.
- `compresso-cli` `cli_edit` (5), `cli_explore_agent` (+4: opt-in round trip, install-only and Claude Code only, on/off transitions, setup keeps an `--edits` entry), `mcp::tests` flag parsing, `surface::tests` serve flags; `compresso-integrations` adapter-argument shapes and agent rendering.

### [x] E17-33: Bounded result size for the compact text form
Owner: R2 · Estimate: 1.5d · Depends: E17-27, E17-28, E17-31
Files: rust/compresso-mcp/src/{profile,server}.rs, rust/compresso-mcp/src/tools/{mod,atlas_read,atlas_grep,text}.rs, rust/compresso-mcp/tests/{result_format,tool_profile}.rs, rust/compresso-integrations/src/explore_agent.rs, rust/compresso-cli/tests/replay_results.rs, docs/plan/{00-overview,03-interfaces,04-mcp-and-integrations,05-tickets}.md, decisions.md
Description: On a long benchmark task (12-part audit plus 3 edits, two runs), the exploration agent's Compresso results put more tokens into context than a run that used only a plain shell session: reads were 107k-109k characters per run (15-17 calls, 26-29 ranges, average span 86-108 lines, up to 560 lines in one range, model-chosen `max_tokens` up to 9,000) and grep 43k-57k (23-26 calls), while the shell run read about 30-line windows. Re-reads overlapped by 0-2 lines and the model already batched patterns and ranges, so the excess was lines served per fact, not repetition. D98 bounds what one text-form result serves: reads serve a line window and name what they leave out, and grep rows stop repeating the enclosing symbol. The JSON form, the full profile's default results, and every default surface are unchanged.
Acceptance criteria:
- Text form only (`--tools lean`, or `--results text`): a `compresso_read` range serves at most 80 lines (a range at most 90 lines long is served whole while the call has room), and one call at most 200 lines across its `ranges`; when the ranges would exceed 200, the call's lines are shared out evenly and a range shorter than its share keeps all of its lines. This applies whatever `max_tokens` the call passes.
- A windowed range keeps its exact header `@@ path:START-END of TOTAL` for the lines served and ends with `[lines A-B not shown: past the N-line read window; starts there: name LINE, ..., +K more; continue with start_line A]`, where `A-B` runs to the requested end clamped to the file. Up to 8 symbols are named, top-level items before members and nested items, in line order; the rest are counted. Symbols are named only while the file matches its index record. Nothing is cut silently.
- When the token budget cuts a read inside its window, the note is the read's ordinary elision, covering the whole requested range with that range's exact overage (`[lines A-B elided: N tokens over budget; continue with start_line A]`).
- Text-form grep rows: a file block's first match row is always tagged (`[-]` for no symbol); a later row is untagged exactly when it shares the symbol of the match row above, is not a definition, and its snippet does not start with `[`. Requested context lines are served in full. Totals are unchanged and exact. Snippets were already cut at 160 bytes with a visible `…`.
- Lean profile (either result form): grep defaults to 3 matching lines per file (was 5) and 1,000 tokens (was 1,500) when the call does not set them, and the lean grep schema states those defaults. The full profile keeps 5 and 1,500 in either form, and its schema bytes are unchanged.
- Window-note symbol names have control characters and `[`, `]`, `;` replaced and are cut to 80 characters; the note's unserved range comes from the read that produced the content.
- The lean `compresso_grep` description says a hit with a few context lines usually replaces a read; the lean `compresso_read` description gives the 80/200-line window and that it names the symbols past it; the exploration agent adds one rule: fewest lines per fact, grep hits before reads, `compresso_unfold` for exactly one symbol, about 30-line windows, never a whole file, and how to read an untagged grep row and `[-]`. The lean surface stays under half of the full one (2,015 of 4,033 o200k tokens; 2,318 of 4,639 Anthropic-calibrated; 1 token of margin under each counter). The default instructions and the full tool list and schemas are byte-identical.
- `rust/compresso-mcp/tests/golden/full_profile_json*.txt` and `lean_profile_text.txt` are unchanged.
- **Target not met.** The ticket's target was at least 30% fewer read and grep result characters on the replay; measured -29.6%, and -22.6% including the estimated follow-up reads. The owner accepted the miss on 2026-11-19 (D98).
- Replay (the ignored test `replay_recorded_calls_from_the_environment` in `rust/compresso-cli/tests/replay_results.rs`, the 81 recorded read and grep calls of both runs, against the base and new release binaries on a fresh index of the benchmark commit): read 215,239 -> 128,435 characters (-40.3%), grep 99,799 -> 93,384 (-6.4%), together 315,038 -> 221,819 (-29.6%; the 30% target is missed by 0.4 points). The base binary reproduces the recorded grep results exactly and the read results to within 0.4%. Of the answer key's file:line facts the old results showed (61 and 63), 8 and 12 now fall past a window; 11 of the 20 lines are named in a window note and the rest lie inside named or partly shown functions. Fetching all of them with one 30-line read per cluster adds 15 calls and 21,872 characters, for -22.6% including those follow-ups. Whether a session now puts fewer tokens into context than the shell-only run (about 100k result characters) is not shown: after the change the agent runs' read and grep results alone are 104k and 117k characters, so only a live run can show whether the new guidance also narrows what the model asks for.
Test cases:
- `test_text_read_shows_a_window_and_names_the_symbols_past_it` (range, whole file, in-window range, 90-line slack, 91 lines windowed, token cut inside the window keeps the elision, JSON form not windowed), `test_text_read_ranges_share_one_call_window`, `test_text_read_window_names_no_symbols_for_a_changed_file`, `test_text_grep_does_not_repeat_the_enclosing_symbol` (JSON rows unchanged), `test_text_grep_lists_fewer_lines_by_default_and_keeps_requested_context` (lean 3 rows, full profile with text results 5), `test_text_grep_rows_are_unambiguous_at_a_block_start_and_before_a_bracket`, `test_lean_grep_schema_states_the_defaults_that_apply`.
- Unit tests: `atlas_read::tests::test_window_symbols_name_top_level_items_before_members`, `test_window_symbols_cannot_forge_a_note_or_a_header`, `test_unserved_range_comes_from_the_read_itself`; `test_lean_descriptions_steer_toward_grep_hits_unfold_and_narrow_reads`; `explore_agent::tests::test_instructions_prefer_grep_hits_and_unfold_over_wide_reads`; `replay_results` tests `test_calls_parse_from_an_array_or_a_stream_transcript`, `test_summary_counts_characters_per_tool`, and `test_replay_measures_both_forms_against_the_built_binary` (a synthetic 300-line file indexed and served by the built binary into a temporary home; the text form windows the read and costs under half the JSON form's characters).
- Existing: `test_full_profile_results_are_byte_for_byte_unchanged`, `test_lean_profile_text_results_match_their_goldens`, `test_text_form_never_exceeds_max_tokens`, `test_text_form_states_truncation_and_continuation`, and `test_lean_schema_and_instructions_cost_less_than_half_of_default` pass unchanged.

### [x] E17-34: Fewer grep rows and round trips per fact in the lean profile
Owner: R2 · Estimate: 1d · Depends: E17-33
Files: rust/compresso-mcp/src/{profile.rs,tools/atlas_grep.rs,tools/text.rs}, rust/compresso-mcp/tests/{result_format,tool_profile}.rs, rust/compresso-mcp/tests/golden/lean_profile_text.txt, rust/compresso-integrations/src/explore_agent.rs, rust/compresso-cli/tests/replay_results.rs, docs/plan/{00-overview,03-interfaces,04-mcp-and-integrations,05-tickets}.md, decisions.md
Description: After E17-33, three exploration-agent runs of the long benchmark task (12-part audit plus 3 edits) cost $0.52-$0.61 against $0.51 for a shell-only run, with 25-33 messages, grep 25-31 calls and 47k-66k characters per run, and reads 16-22 calls. Re-reading context on every message cost about as much as new tokens, so the number of messages matters as much as result size. Analysis of the three transcripts: 20% of grep characters were matches in test code (whose answer facts also appeared in code rows), 16% context rows, 20% headers and notes, 4% lines a second pattern of the same call repeated; reads that followed a grep hit spanned 58 lines on average, of which 27% lay inside the hit's enclosing function, so serving the function body with the hit would have added 8k-56k characters per run to replace 1-3 reads; per-part `compresso_context` packs held 16 of the 47 answer facts in 117k characters, and one pack for the whole task held 1 (the agent was offered the tool and, given exact identifiers, never called it); 4-10 tool messages per run used nothing from the results just before them, and the call dependency graph is 8-16 messages deep against 24-32 messages sent. D99 trims text-form grep rows and tells the agent to batch independent lookups.
Acceptance criteria:
- Text form only (`--tools lean`, or `--results text`): while a pattern lists a new row outside test code and does not itself contain `test` (any case), its matches in test code (a file under a `test`, `tests`, `__tests__`, or `testdata` directory, or a line inside an indexed module named `tests`; file and function names alone never count, and an out-of-line `mod tests;` file is listed) are not listed; the file header names their lines (`path (n; tests: 7, 9)`) and one note says how to see them. Otherwise test rows are listed. Which rows count as new depends on what the call's limits and budget kept, so a truncated result can name test rows that a wider one would also name, or list them when its only code rows were cut.
- A line an earlier pattern of the same call listed with the same snippet is named (`; above: 12`), not listed again, with one note; a long line cut around a different match is listed again.
- Every listed row keeps the requested context lines its block has not already printed: context is re-merged over the listed rows, so a row named instead of listed does not take its neighbours' context with it (a named line inside that span is shown as a context row).
- Named lines are numbers written by the server; file content cannot forge a header suffix or a note, and a header path has control characters, `;`, and a `(` after a space replaced by `?` (ordinary paths are unchanged). Every match the JSON form lists is listed or named; totals and the `counted but not listed` count are unchanged. The grep header reads `# PATTERN — N lines in M files`.
- The exploration agent adds one rule: send lookups that do not depend on each other's results together in one message, and search for several parts of a multi-part task at once. No `compresso_context` guidance is added.
- The lean grep description says test hits are named by line and the lean `context_lines` description says "each listed match"; to pay for that, the lean `compresso_repo_map`, `compresso_search`, `compresso_trace`, and `compresso_read` descriptions are shorter (lean surface 2,010 of 4,033 o200k tokens; 2,313 of 4,639 Anthropic-calibrated; the half-of-full test is unchanged). The full profile's schemas and descriptions are unchanged, so with `--tools full --results text` its grep wording does not mention named test rows (known gap). The JSON form, `rust/compresso-mcp/tests/golden/full_profile_json*.txt`, the tool lists, and the instructions are unchanged; `lean_profile_text.txt` changes only in its four grep headers.
- **Target not met.** Replay (`replay_recorded_calls_from_the_environment`, the read and grep calls of the three runs, base and new release binaries, each on a copy of the same pinned index of the benchmark commit): grep 167,310 -> 132,012 characters (-21.1%; per run -17.6%, -23.2%, -22.2%) against a target of -25%; reads unchanged (204,505). Of 621 answer-key lines the base results showed, 522 are still listed, 95 are named by line, and 4 context lines that belonged only to a named test row are gone; all 47 answer facts the base results showed are still shown in each run, but in 5 calls a fact the call showed before now sits only on a named test line.
- Messages (an estimate, not a measurement): the grep changes save no message by themselves (no follow-up read was replaceable). If the agent follows the batching rule one step (a message that uses nothing from the results just before it joins the previous one), runs would send 10, 4, and 4 fewer messages; the dependency-graph depth bounds the saving at 24, 14, and 9. Only a live run can show what the model does.
Test cases:
- `test_text_grep_names_test_matches_by_line_while_code_matches` (reading the named lines returns them; a test-only pattern lists its rows; JSON lists all), `test_text_grep_names_rows_an_earlier_pattern_listed`, `test_text_grep_serves_requested_context_for_every_listed_row`, `test_text_grep_keeps_context_beside_rows_named_by_line` (test row and repeat between listed rows), `test_text_grep_lists_production_code_with_test_like_names`, `test_text_grep_lists_test_rows_when_the_pattern_targets_tests_or_lists_nothing_new`, `test_text_grep_file_content_cannot_forge_named_lines`, `test_text_grep_header_path_cannot_forge_counts_or_named_lines`, `test_text_grep_lists_a_long_line_again_when_another_pattern_shows_other_text`.
- Unit tests: `test_lean_grep_wording_says_test_hits_are_named_by_line`; `explore_agent::tests::test_instructions_batch_independent_lookups_into_one_message`; `replay_results` test `test_needles_are_kept_named_or_lost_against_a_baseline`.
- Existing: `test_full_profile_results_are_byte_for_byte_unchanged`, `test_lean_profile_text_results_match_their_goldens` (grep headers updated), `test_text_form_never_exceeds_max_tokens`, `test_text_grep_lists_fewer_lines_by_default_and_keeps_requested_context`, and `test_lean_schema_and_instructions_cost_less_than_half_of_default` pass.

### [x] E17-35: Honour the repository's `info/exclude` and skip nested linked worktrees
Owner: R2 · Estimate: 1d · Depends: E17-32
Files: rust/compresso-atlas/src/{scanner,edit,indexer,rust_imports,lib}.rs, rust/compresso-atlas/tests/{git_exclude,edit}.rs, rust/compresso-mcp/src/tools/atlas_read.rs, rust/compresso-cli/src/commands/index.rs, rust/compresso-cli/tests/cli_index.rs, rust/compresso-eval/src/runner.rs, docs/plan/{00-overview,03-interfaces,04-mcp-and-integrations,05-tickets,08-ip-and-secure-deployment}.md, compresso-threat-model.md, decisions.md
Description: `compresso index` on a repository indexed full copies of it under `.claude/worktrees/<name>/` (linked git worktrees) that git itself ignores through `.git/info/exclude` (`**/.claude/worktrees/`): about 2,100 files instead of about 500, and every search and grep hit repeated per copy. Diagnosis: the scanner honoured only `.gitignore` files inside the root (every level, deepest rule wins) plus its built-in exclusions (`node_modules`, `target`, `dist`, `.git` directories, secret-file patterns); it read no `.ignore`, no `info/exclude`, no `core.excludesFile`, and no global excludes file, and it treated a directory whose `.git` is a file (a linked worktree or a submodule) as ordinary content, indexing the `.git` pointer file itself. The opt-in edit tool shared thAcceptance criteria:
- The scan and the shared exclusion policy (`ScannerExclusions`, used by `compresso_read`, `compresso_edit`, and the Rust import resolver; `is_scanner_excluded` for one path) apply the indexed repository's own `info/exclude` with `.gitignore` semantics, rooted at the indexing root, below every `.gitignore` decision (a `.gitignore` rule, including a `!` re-include, outranks it, as in git). CRLF line endings and a leading UTF-8 BOM are accepted.
- The exclude file is found as git finds it: `<root>/.git/info/exclude` for a `.git` directory; for a `.git` file, its `gitdir:` pointer and, when that directory has one, its `commondir` pointer. Pointers are parsed lexically (only trailing CR/LF stripped, as git does) and a UNC, verbatim, device, or drive-relative target or a name with `:` is refused before any file-system call; the resolved directory must be `<A>/.git/...` with `A` the root or one of its ancestors and is opened only as `A` joined with those names. No component from `A` to the file may be a symbolic link; the file is opened without following links or blocking and must be a regular file within its bound on the open handle (pointers 4 KiB, exclude 1 MiB). Any failure means no exclude rules. No global or user git configuration or excludes file is read.
- `ScanResult` and `IndexReport` carry `git_exclude_unusable` (a flag only) when an exclude file exists or is pointed at but was not applied in full (refused pointer, outside the root's ancestors, link, FIFO, oversized, unparsable pattern); `compresso index --json` reports it and the human output adds a note.
- A directory below the root whose `.git` is a regular file of at most 4 KiB with a `gitdir:` pointer naming `<...>/.git/worktrees/<name>` (either separator, absolute or relative) is a nested linked worktree and is skipped like an ignored directory. The decision is lexical: the named directory is never opened. Submodules (a pointer under `.git/modules/`), nested repositories with a `.git` directory, and missing, oversized, or malformed pointers are indexed. The indexing root itself is never skipped.
- An entry named `.git` in any letter case, of any type, is never indexed (git never tracks such a path).
- The edit gate loads the exclusion policy once per call and refuses a newly excluded file even while a stale index row still names it (`excluded`); re-indexing removes rows for newly excluded files. The import resolver and multi-range reads load it once.
Test cases:
- `git_exclude` tests: `test_info_exclude_is_honoured_by_scan_and_single_path_check` (CRLF, BOM, `.gitignore` re-include outranks it), `test_reindex_removes_rows_for_newly_excluded_files`, `test_linked_worktree_root_reads_exclude_from_the_common_dir` (worktrees inside the main checkout, absolute and relative pointers), `test_a_common_dir_outside_the_roots_ancestors_is_never_read`, `test_network_and_device_pointers_fail_open_and_are_flagged`, `test_pointer_whitespace_is_part_of_the_path_as_in_git`, `test_oversized_and_unparsable_excludes_are_flagged`, `test_git_entries_are_never_indexed_in_any_case`, `test_symlinked_or_special_exclude_paths_are_not_followed` (Unix: linked `info`, linked `exclude`, FIFO), `test_submodule_root_reads_exclude_from_its_own_gitdir`, `test_nested_linked_worktree_is_skipped_by_its_pointer_alone` (nonexistent Unix and Windows targets), `test_submodules_nested_repositories_and_malformed_pointers_are_indexed`; each checks that the scan and the single-path check agree on every file.
- `edit` tests: `test_unscoped_rename_never_reaches_a_worktree_copy_excluded_by_info_exclude`, `test_unscoped_rename_never_reaches_a_nested_linked_worktree`, `test_a_stale_row_for_a_newly_excluded_file_is_refused`.
- Unit tests: `scanner::tests::test_worktree_pointer_classification_is_lexical`, `test_pointer_classification_refuses_network_and_device_paths` (Windows and POSIX strings on every platform), `test_git_dirs_are_anchored_below_an_ancestor_of_the_root_only`, `test_oversized_or_unreadable_exclude_sources_fail_open`; `edit::tests::test_a_rename_loads_the_exclude_rules_independently_of_its_file_count`.
- Existing scanner, read-gate, and edit tests pass unchanged; `cli_index` lists the new report key.

### [x] E17-36: Several independent lookups in one call
Owner: R2 · Estimate: 1d · Depends: E17-34
Files: rust/compresso-atlas/src/{grep,edit}.rs, rust/compresso-atlas/tests/{grep_content,symlink_swap,edit}.rs, rust/compresso-cli/src/commands/retrieval.rs, rust/compresso-mcp/src/{profile.rs,tools/atlas_grep.rs,tools/atlas_read.rs,tools/edit.rs,tools/text.rs}, rust/compresso-mcp/tests/{result_format,tool_profile,tools_edit}.rs, rust/compresso-integrations/src/explore_agent.rs, docs/plan/{03-interfaces,04-mcp-and-integrations,05-tickets}.md, docs/adr/0035-opt-in-verified-edit-tool.md, decisions.md
Description: Session benchmarks (five runs per arm) showed that cost follows round trips, because every message re-reads the context, and not result size: result characters were about equal across arms (~110k), but the exploration agent made 64 calls per run on a Go task (43 grep, 16 read) against 25-31 for arms that chain 3-4 independent lookups into one call, 43 against 25 on a TypeScript task, and 49 against 31 on a Rust task. `compresso_grep` already took several patterns, and the E17-34 rule to send independent lookups together did not reduce the number of messages: every recorded message carried exactly one tool call. So packing has to be possible inside one call, and the descriptions and the agent have to steer to it. D101 lets one `compresso_grep` call carry several patterns, each with an optional own scope, plus read ranges of any files.
Acceptance criteria:
- A `compresso_grep` `patterns` entry may be a string or `{pattern, path}`; a scoped pattern searches only the files admitted by both the call's `path` and its own scope, in the same single scan, so round-robin fitting, the `above` repeats, and test-code naming still work across patterns. A file that no pattern may search is neither read nor counted in scope. Works on any indexed file, with or without a grammar.
- `ranges` (up to 8 `{path, start_line, end_line}`, the `compresso_read` argument) reads lines of any files in the same call, through the same read gates, per-range errors, and text-form windows (80 lines per range, 200 per call).
- Text result: one `# PATTERN [in SCOPE] — N lines in M files` section per pattern, then the grep notes, then one `@@ path:START-END of TOTAL` section per range. Every grep section comes before any raw file content; a grep file header path starting with `# ` or `@@ ` has its first character replaced by `?` (other paths, such as `@scope/a.rs`, print unchanged).
- Bounds: with ranges and no `max_tokens`, the default is the grep default plus the read default (2,000). `max_tokens` is split between patterns and reads in the proportion of those defaults (grep share at least 300); patterns share theirs round-robin, ranges evenly, and reads also get what the patterns left. Every cut has a note. The text form and the JSON form are each held to `max_tokens` as sent.
- An invalid scope fails only its own pattern's section (`# PATTERN in SCOPE — [invalid_arguments] message`); the call fails when every pattern does.
- The full profile accepts scoped patterns and `ranges` without advertising them: its schemas, instructions, and `full_profile_json*.txt` goldens are byte-identical, so an installed (full-profile) server answers the agent's packed example. `lean_profile_text.txt` is unchanged.
- Lean schema: the grep `patterns` items state both shapes, `ranges` is added, and the single `pattern` is no longer advertised; the read schema advertises only `ranges` and `max_tokens` (`path`, `start_line`, `end_line`, `gate` are no longer advertised). All stay accepted. Lean surface 1,992 of 4,033 o200k tokens (2,292 of 4,639 Anthropic-calibrated), down from 2,010 (2,313).
- Lean descriptions: grep says one call carries every lookup you have, patterns and ranges; read says to send the ranges in `compresso_grep` when also searching. The exploration agent packs each call with every lookup it can already name (usually one grep call with the next searches in `patterns` and the follow-up reads in `ranges`), shows one call with three lookups, and uses `compresso_read` only when nothing is left to search, with every range in one call.
- `compresso_edit` already takes operations on several files in one call, all or nothing with every gate per file (ADR-0035); a test pins it and no edit code changes.
- Round trips found in live runs of the first cut: (a) a rename scoped by `path` reported 0 remaining while a whole match of the old name sat outside its scope, and the agent stopped; a scoped rename now also counts and lists (up to 20) whole matches in indexed files outside the scope, after every operation of the call, through the read gate only, within the call's rename read budget, nothing written, and is not `complete` while any remain (text: `remaining in scope: ...; N whole outside scope, not renamed`); files outside it could not search are listed as not searched and do not make it incomplete, and the agent's rule says to grep there once if it matters, then stop. (b) The grep regex compiled-size bound rose from 256 KiB to 2 MiB per compiled program (lazy-DFA caches stay 2 MiB): about 8 MiB per pattern and 64 MiB for 8 at most; measured, 8 patterns at the limit over 2.4 MB of text added 22.7 MB peak resident memory. It accepts ordinary counted repetitions of Unicode classes such as `\D{0,30}` (about 1 MiB); a larger pattern's error says to shorten counted repetitions or use ASCII classes. Path globs keep 256 KiB. (c) In the lean profile, a grep limit above its advertised maximum (`max_files`, `max_matches_per_file`, `context_lines`, `max_tokens`) is clamped with a `[limits clamped: ...]` note instead of failing; below-minimum values, and the full profile, still fail (the full rejection is pinned by `test_grep_tool_context_lines_surround_matches_within_the_token_cap`). Known gap: a lean `max_tokens` below 300 still fails.
- Review fixes: a packed call at the smallest `max_tokens` (300) stays within it in both forms or fails with `invalid_arguments` (with ranges the grep share is at most half of a small budget); a failed scope prints at the caller's position and later pattern errors keep the caller's index; patterns and scopes echoed into headers have control characters replaced; the agent's example uses language-neutral names. The gate's `cheaper_alternative` text ("a narrower start_line and end_line range") is kept: lean `ranges` entries still take `start_line` and `end_line`, and changing it would change the full golden.
- Known gaps (measured): `ranges` inside `compresso_grep` was used 0 times in about 180 grep calls over 6 live lean runs (3 on the first cut, 3 on the final wording); reads went to `compresso_read` with 1-6 ranges per call instead, and patterns per grep call rose to 2.7-3.6. With a full-profile server (what every installer writes, D92) the agent never sent a scoped pattern or `ranges` (0 of 32 grep calls, 1 run): cost $0.672, 49 calls, 154k result characters, 47 of 48 facts, against a $0.443 mean, 39 calls, and ~95k characters for the same binary and agent file on a lean server (3 runs), so the agent's benchmarked cost applies only to a lean-profile server. Raw range content can imitate a grep section; only placement (every grep section before the first `@@` header) tells them apart.
- No shell, wrapper, hook, or process capability is added (D76; the server still executes no shell command).
- Live check of the first cut (orchestrator, held-out Go task, 3 runs, lean server with the agent file): 42, 48, and 52 calls per run against a 63.8 mean before, cost $0.462, $0.445, and $0.506 against $0.547, 47-48 of 48 facts, 16 of 16 edits. 60 of 98 grep calls carried several patterns (2.0 on average), but no call used `ranges` and reads stayed at 11-12 separate calls per run, so the read wording above was strengthened afterwards; the final binary has not been measured live.
Test cases:
- Atlas: `test_grep_pattern_scope_narrows_only_its_own_pattern`, `test_grep_accepts_ordinary_counted_repetitions_and_explains_the_size_bound`, `test_a_scoped_rename_counts_whole_matches_outside_its_scope`, `test_scoped_renames_count_outside_matches_after_every_operation`; unit `test_the_outside_scope_count_shares_the_call_budget_and_says_what_it_skipped`.
- MCP: `test_text_grep_packs_scoped_patterns_and_ranges_into_one_call`, `test_text_packed_call_serves_every_lookup_within_its_budget`, `test_text_packed_call_content_cannot_forge_section_headers`, `test_full_profile_grep_accepts_packed_lookups_without_advertising`, `test_text_grep_invalid_scope_fails_only_its_own_section`, `test_json_packed_call_stays_within_max_tokens`, `test_lean_json_packed_call_adds_scope_and_ranges`, `test_lean_grep_schema_advertises_packed_lookups`, `test_grep_clamps_limits_above_their_maximum_with_a_note`, `test_one_call_edits_several_files_all_or_nothing` (pins pre-existing behaviour; passed on first run), `test_a_scoped_rename_reports_whole_matches_outside_its_scope`, `test_packed_call_at_the_smallest_budget_never_exceeds_it`, `test_packed_call_keeps_the_callers_order_and_indexes`; unit `test_rename_rows_name_the_real_reason_and_an_unfinished_outside_count`.
- Unit: `explore_agent::tests::test_instructions_show_one_packed_call_with_three_lookups`, `test_instructions_send_follow_up_reads_with_the_next_searches`, `test_edits_grant_the_edit_tool_and_its_rules_only_when_asked` (scoped-rename rule added).
- Existing: `test_full_profile_results_are_byte_for_byte_unchanged`, `test_lean_profile_text_results_match_their_goldens`, `test_default_profile_tool_list_and_instructions_are_unchanged`, `test_lean_schema_and_instructions_cost_less_than_half_of_default`, `test_lean_descriptions_steer_toward_grep_hits_unfold_and_narrow_reads` (read wording added), `test_text_read_ranges_share_one_call_window` pass.

### [x] E17-37: Go symbols and a table-driven language registry
Owner: R2 · Estimate: 2d · Depends: E17-34
Files: Cargo.toml, Cargo.lock, rust/compresso-store/src/symbols.rs, rust/compresso-atlas/Cargo.toml, rust/compresso-atlas/queries/go.scm, rust/compresso-atlas/src/{lib.rs,parsers.rs,parsers/languages.rs,parsers/go.rs,parsers/calls.rs,go_imports.rs,indexer.rs,trace.rs,cards.rs,queries.rs,task_identifiers.rs}, rust/compresso-atlas/tests/{parsers_go,indexer_go,parsers_rust,parsers_calls}.rs, rust/compresso-atlas/tests/fixtures/lang/sample.go, rust/compresso-store/src/files.rs, rust/compresso-mcp/src/tools/{common,atlas_grep}.rs, rust/compresso-mcp/tests/result_format.rs, rust/compresso-cli/src/commands/{doctor,retrieval}.rs, docs/plan/{01-stack-and-workspace,02-schemas,03-interfaces,04-mcp-and-integrations,05-tickets}.md, docs/adr/{0009-tree-sitter-symbol-extraction,0033-call-sites-resolved-at-query-time}.md, decisions.md
Description: D8 deferred Go, Java, C/C++, C#, Ruby, and PHP to "later behind the same parser registry", but adding a language touched match arms in the parser, indexer, trace, cards, queries, store, CLI, MCP, and doctor. D102 moves the per-language facts into one table and adds Go. Symbol tools are a small share of agent calls (grep and read dominate even on TypeScript), so the expected benefit for Go repositories is mainly enclosing-symbol tags on grep rows and symbol names in read-window notes; no Go benchmark was run.
Acceptance criteria:
- One table (`compresso_atlas::parsers::LANGUAGES`) lists each language's extensions, extractor (grammar, query, processor, call syntax, or a hand-written extractor), import style, call family, module-doc and visibility rules, entry files, test-file suffix, package-is-directory flag, and a smoke sample; parser selection, extraction, import dispatch, trace families, module cards, context language order, and `compresso doctor` read it. Existing languages are byte-identical: every existing test and golden passes unchanged (verified on the refactor alone before Go was added).
- `.go` files are indexed as language `go` with the `tree-sitter-go` 0.25.0 grammar (MIT; `cargo deny check licenses bans sources` passes): functions, `Receiver::Name` methods, struct/interface/type declarations, package-level `const` and `var` names (stored as `const`), imports, and call sites; local declarations, interface method elements, and struct fields are not symbols.
- Go calls: plain, receiver-name self calls, package/variable path calls (import aliases rewritten to the package's default name), receiver calls; builtins and predeclared-type conversions dropped; `handlers[i]()` is not a call. Go resolution: plain, reference, and self calls stop after `same_file` and `same_package`; a qualifier naming an external import is a library call; a qualifier naming a package directory reaches its exported names outside the caller's directory (root-level files name no package); any other qualifier falls back to methods, always `name_only`; `_test.go` candidates are reached only from test files.
- Go imports under a `go.mod` module path resolve to every non-test `.go` file of the package directory (one edge each); others are external. `impact` on a package file lists its importers, including after an incremental index adds a file to the package or a `go.mod` changes. Repeated imports count once; at most 4,096 resolved Go import edges per file, with an unresolved `edge_limit` edge per cut import; `go.mod` under `testdata`, `vendor`, `_*`, `.*` is ignored and a duplicate module path resolves through the nearest declaration. Signatures are capped at 1,024 bytes and multi-name `const`/`var` specs give per-name signatures.
- Module cards read the comment above `package` (not build constraints or license headers) and treat capitalized top-level names as public; `main.go` and `doc.go` are entry files.
- Text-form grep names `*_test.go` rows by line as test code (D99 amended narrowly); other file names never count.
- The parser version gains `+go-v1`, so existing stores re-parse every file on the next `compresso index`.
- `rust/compresso-mcp/tests/golden/*` are unchanged; tool descriptions and schemas are unchanged.
Test cases:
- `parsers_go`: `test_go_files_select_the_go_parser`, `test_go_functions_methods_types_and_package_values`, `test_go_imports_are_unquoted_package_paths`, `test_go_calls_record_receiver_package_and_plain_forms`, `test_go_builtins_and_conversions_are_not_calls`, `test_go_local_declarations_and_func_literals_are_not_symbols`, `test_go_malformed_source_still_extracts_complete_items`, `test_every_table_language_has_unique_extensions_and_parses_its_sample`, `test_go_multi_name_value_signatures_are_per_name_and_bounded`, `test_go_huge_const_block_extracts_in_bounded_time`, `test_go_blank_function_and_type_names_are_not_symbols`, `test_go_indexed_function_values_are_not_calls_to_the_slice`.
- `indexer_go`: `test_go_files_are_indexed_with_symbols_and_language`, `test_go_package_imports_are_edges_to_each_package_file`, `test_go_calls_resolve_through_imports_packages_and_receivers`, `test_go_module_card_reads_the_package_doc_and_exported_names`, `test_a_store_indexed_before_go_support_reindexes_go_files`, `test_a_file_added_to_an_imported_package_gains_its_importers`, `test_a_changed_go_mod_re_resolves_go_imports`, `test_go_library_calls_stay_external_and_variable_calls_are_name_only`, `test_go_package_qualifier_never_resolves_into_its_own_directory`, `test_go_aliased_import_calls_resolve_to_the_package`, `test_go_plain_calls_resolve_only_within_their_package`, `test_go_same_package_calls_skip_test_files_for_non_test_callers`, `test_an_excluded_go_mod_declares_no_module` (E17-35 exclusions apply to `go.mod`).
- `parsers_calls`: `test_deeply_nested_go_receiver_calls_extract_quickly`.
- `result_format`: `test_text_grep_names_go_test_file_rows_by_line`, `test_text_results_name_go_symbols_in_grep_rows_and_read_windows`.
- Unit tests: `cards::tests::test_go_exported_names_are_capitalized_top_level_symbols`, `test_go_package_doc_is_the_comment_above_the_package_clause`; `go_imports::tests::*` (module directive, package files, external and unindexed imports, longest module path, nearest duplicate module, ignored module directories); `indexer::tests::test_go_imports_are_deduplicated_and_capped_per_file`; `parsers::go::tests::test_package_local_name_skips_versions_and_reads_dashes_as_underscores`; `trace::tests::test_owner_base_and_module_name` (Go package directory); `task_identifiers::tests::test_go_file_names_are_paths`; `parsers::languages::tests::test_every_file_language_has_exactly_one_table_entry`, `test_only_go_reserves_a_test_file_name`.
- Existing: every parser, indexer, trace, card, grep, and golden test passes unchanged, except `parsers_rust::test_for_path_extension_map`, which pinned `.go` as unparsed and now expects Go (`.java` stays unparsed).

### [x] E17-38: The exploration agent runs its own lean server; fewer lost round trips
Owner: R2 · Estimate: 2d · Depends: E17-28, E17-36
Files: rust/compresso-integrations/src/{explore_agent.rs,claude_code.rs}, rust/compresso-cli/src/commands/{integrations.rs,pr_post.rs}, rust/compresso-cli/tests/{cli_explore_agent,cli_explore_agent_server}.rs, rust/compresso-atlas/src/{grep.rs,lib.rs}, rust/compresso-mcp/src/{profile.rs,tools/atlas_grep.rs,tools/atlas_read.rs,tools/text.rs}, rust/compresso-mcp/tests/{result_format,tool_profile}.rs, docs/plan/{00-overview,03-interfaces,04-mcp-and-integrations,05-tickets}.md, decisions.md
Description: On a lean-profile server the exploration agent cost $0.461 (TypeScript) and $0.489 (Go) per run, but `integrations install claude-code --explore-agent` paired it with the shared server entry, which runs the full profile (D92): measured once at $0.672 with JSON results and 154k result characters. The owner asked on 2026-11-20 for the agent to install with the lean profile. D103 (proposed by the implementing session, confirmed by the owner on 2026-11-20) gives the agent its own inline lean server and serves, with a note, lean calls that failed or came back empty in live runs.
Acceptance criteria:
- `--explore-agent` writes an agent file whose frontmatter defines the inline stdio server `compresso-lean` (`command` the shared entry's launcher path; `args` `mcp serve --root <canonical project> --tools lean` [+ `--edits`]; no recall source) and whose Compresso tools are `mcp__compresso-lean__<tool>`; no tool names the shared `compresso` server. The shared entry is byte-identical after the agent install and never carries `lean` (D92).
- The written `command` and `args`, started as a real process, serve exactly the tools the agent is granted (lean, plus `compresso_edit` with `--edits`); paths with spaces, quotes, `#`, or `: ` read back unchanged; control and line-separator characters are refused.
- With no Compresso tool (inline server skipped: untrusted folder, `--strict-mcp-config`, `--bare`, managed policy) the agent checks its tool list first, makes no tool call, and says why; `status --explore-agent` reports `agent_server`, advisory `folder_trust` (read-only), and one actionable line; `install` warns unless trust is recorded; `verify` gains `agent_lean_server`; an agent file from an earlier version is reported with the reinstall command and replaced by a reinstall; `doctor` unchanged.
- Two servers on one store: a lean `--edits` server renaming while a full server searches; no call fails.
- Lean only (full profile still rejects): more than 8 grep patterns search the first 8 with a `clamped` note; grep `max_tokens` below 300 is raised to 300; a `compresso_read` `ranges` `max_tokens` outside 100-100,000 is clamped; all with `[limits clamped: ...]`.
- Lean only (the full profile's behaviour, schemas, and goldens unchanged): a zero-match literal pattern with an unescaped `|` is searched as its literal parts with a note (not a pattern holding `||` or a one-character part); a path that matches no indexed file is named with the nearest indexed path; up to 5 counted-but-unlisted files per pattern are named with counts, dropped first when the result would not fit, then the other notes shrink to one line; a retry whose escaped alternation exceeds the pattern limit, or that fails, keeps the empty result.
- The lean `compresso_edit` description and the agent's edit rules say to write tabs as `\t` and line breaks as `\n` in JSON strings; the agent prefers several short replace operations.
- The flaky `pr_post` test's stand-in `gh` is never opened for writing by the test process (created by `cp`), so a concurrently forked child cannot hold it open and make its exec fail with `ETXTBSY`.
- Read window (D98) evaluated by replay and left unchanged (numbers in D103).
- Lean surface 1,992 of 4,033 o200k tokens (unchanged).
- Considered and deferred: a "line must also match Y" grep filter; listing test lines for edit searches.
Test cases:
- Unit (`explore_agent`): `test_the_agent_starts_its_own_lean_server_for_its_project`, `test_inline_server_paths_survive_yaml_special_characters`, `test_instructions_stop_when_the_agent_server_did_not_start`, `test_an_agent_written_for_the_shared_server_is_reported_and_upgraded`, `test_verify_accepts_another_absolute_launcher_only`, `test_folder_trust_is_read_from_the_client_record`, `test_edit_rules_say_to_escape_tabs_and_prefer_short_literals`; updated `test_render_is_a_claude_code_agent_with_compresso_and_file_tools`, `test_edits_grant_the_edit_tool_and_its_rules_only_when_asked`. Atlas: `grep::tests::test_literal_alternatives_split_on_unescaped_pipes_only`.
- CLI: `cli_explore_agent_server::{test_the_agent_file_starts_a_lean_server_and_the_shared_entry_stays_full, test_lean_agent_server_edits_while_the_full_server_reads_the_same_store}`; `cli_explore_agent::{test_agent_install_warns_about_folder_trust_until_it_is_recorded, test_status_reports_an_agent_file_written_for_the_shared_server}` (tool prefix updated in the existing round-trip tests).
- MCP: `test_lean_grep_serves_the_first_eight_patterns_and_names_the_rest`, `test_lean_grep_raises_max_tokens_below_its_minimum_with_a_note`, `test_lean_grep_combines_the_pattern_and_budget_clamps`, `test_lean_read_clamps_max_tokens_with_a_note`, `test_literal_pattern_with_a_pipe_falls_back_to_its_literal_parts`, `test_grep_names_a_path_that_matches_no_indexed_file`, `test_grep_names_files_counted_but_not_listed`, `test_unlisted_file_names_never_fail_a_small_budget` (seen failing with the shedding disabled); review: `test_full_profile_grep_has_no_round_trip_notes`, `test_a_long_literal_with_a_pipe_never_fails_the_call`, `test_round_trip_notes_never_fail_a_small_budget` (all three seen failing), unit `test_noncharacters_need_escaping`, `test_folder_trust_reads_only_a_bounded_regular_file`, `test_verify_names_the_agent_command_and_flags_another_launcher`, CLI `test_status_names_the_agent_command_and_warns_on_another_launcher` (seen failing), `test_agent_command_with_control_characters_is_escaped_in_output` (seen failing), `test_status_never_blocks_on_a_fifo_client_config`; portable unit `test_server_block_round_trips_any_path_text` (Windows and POSIX path text); `tool_profile::test_lean_edit_description_says_to_escape_tabs_and_newlines`.
- Seen failing before the change: the MCP tests above (except the combined-clamp test), the two tab-rule tests; a mutation (tool names back to `mcp__compresso__`) fails 6 agent unit tests. Written with or after the code and not seen failing: the other agent unit tests, the atlas unit test, both `cli_explore_agent_server` tests, the two `cli_explore_agent` status tests, and the combined-clamp test. The `pr_post` race cannot be reproduced on macOS (no `ETXTBSY` for this case); the fix is reasoned, not observed.
- Existing: every golden (`full_profile_json*.txt`, `lean_profile_text.txt`, `lean_edit_text.txt`), `test_lean_schema_and_instructions_cost_less_than_half_of_default`, and `test_default_profile_tool_list_and_instructions_are_unchanged` pass unchanged.

### [x] E17-39: Capped grep files list their most useful rows
Owner: R2 · Estimate: 1d · Depends: E17-34, E17-37, E17-38
Files: rust/compresso-atlas/src/{grep.rs,lib.rs,parsers/languages.rs}, rust/compresso-atlas/tests/{grep_content,symlink_swap}.rs, rust/compresso-cli/src/commands/retrieval.rs, rust/compresso-mcp/src/{profile.rs,tools/atlas_grep.rs,tools/text.rs}, rust/compresso-mcp/tests/{result_format,tool_profile}.rs, rust/compresso-mcp/tests/golden/lean_profile_text.txt, rust/compresso-integrations/src/explore_agent.rs, docs/plan/{03-interfaces,04-mcp-and-integrations,05-tickets}.md, decisions.md
Description: In 18 live runs on a held-out TypeScript repository, 4 of the exploration agent's 9 extra missed facts came from one cause: a file had more matching lines than `max_matches_per_file`, the first lines in line order were listed, doc-comment rows near the top filled the cap, and the code rows that held the fact were only counted. D104 (proposed by the implementing session, revised after an independent review, confirmed by the owner on 2026-11-20) lists a capped file's most useful rows in the text form and names the rest by line.
Acceptance criteria:
- Verified before the change and recorded in D104: rows under the per-file cap were the first ones in line order; definitions only ranked files.
- Text form only (`--tools lean` or `--results text`): every file's rows are ranked (first 2,000 matching lines): definition (match on the symbol's name), code, signature mention, comment or documentation text, `tests` module (not for a pattern containing `test`), rows an earlier pattern of the call chose; spread across enclosing symbols, then line order; a capped file lists its most useful rows in line order. A file matching only in comments lists them; a file at or under the cap lists every match.
- Comment detection comes from the language table (`LanguageSpec.comments`), reads line starts only, follows a block only when its closer comes within 64 KiB (one forward cursor: linear in the file's size), re-parses nothing, and treats unknown languages, Text, and Python docstrings as code. Enclosing symbols come from one sorted pass (bounded-time test).
- File ranking, the JSON form, schemas, instructions, the full-profile goldens, and the CLI `compresso grep` output are unchanged.
- The file header names up to 8 unlisted lines, the most useful first, printed ascending, with the count of the rest (`Shared.ts (28; unlisted: 425, 427, … +14)`); numbers only, server-written; a budget cut inside a file keeps its most useful rows, names the dropped ones, and keeps their text as context for the listed rows; the unlisted-files line reads `[unlisted files: ...]` with `]` and `,` in paths replaced; the trailing note says to pass unlisted lines as ranges in the next packed call; names are cut to 3 per file, then dropped, before any row is displaced, and no call fails at 300 tokens.
- The lean grep description keeps D101's packing sentence and `exact totals`; its `ranges` description and the agent's grep rule say unlisted lines are read as ranges in the next packed call; lean surface 2,008 of 4,033 o200k tokens; every lean description stays within 200 characters.
- Replay of the 687 recorded grep calls of 26 live runs: numbers (a)-(d) in D104.
Test cases:
- MCP (`result_format`): `test_text_grep_lists_definitions_and_code_before_comments_under_the_cap` (also pins the JSON form's first lines), `test_text_grep_spreads_capped_rows_across_enclosing_symbols`, `test_text_grep_names_a_bounded_set_of_unlisted_lines`, `test_text_grep_unlisted_line_names_cannot_be_forged`, `test_text_grep_ranks_test_module_rows_last_under_the_cap`, `test_unlisted_line_names_never_fail_a_small_budget`, `test_unlisted_line_names_shrink_before_rows_are_displaced`, `test_text_grep_row_selection_keeps_the_file_ranking`; review: `test_text_grep_capped_rows_skip_rows_an_earlier_pattern_listed`, `test_text_grep_test_pattern_keeps_test_module_rows_first`, `test_text_grep_unlisted_file_paths_cannot_split_the_list`; updated `test_text_grep_lists_fewer_lines_by_default_and_keeps_requested_context`, `test_grep_names_files_counted_but_not_listed`, `test_unlisted_file_names_never_fail_a_small_budget`, and the lean golden `lean_profile_text.txt`. `tool_profile::test_lean_grep_wording_says_unlisted_lines_can_be_read_directly`.
- Atlas unit: `grep::tests::{test_comment_rows_read_only_line_starts_and_err_toward_code, test_rank_rows_orders_by_class_then_spreads_across_symbols, test_budget_cut_keeps_the_most_useful_rows_and_names_the_rest, test_budget_cut_keeps_a_dropped_middle_rows_text_for_context}`, `languages::tests::test_each_language_states_its_comment_syntax`. Atlas (`grep_content`): `test_grep_row_selection_uses_the_file_language_for_comments`, `test_grep_row_selection_reads_comments_in_a_changed_file`, `test_grep_row_selection_chooses_from_the_first_candidates_only`; review: `test_grep_row_selection_ranks_a_definition_only_when_its_name_matches`, `test_grep_row_selection_ranks_signature_mentions_after_body_code`, `test_grep_row_selection_ranks_rows_of_a_file_under_the_cap`, `test_grep_row_selection_ignores_a_block_opener_with_no_closer`, `test_grep_row_selection_stays_fast_with_many_symbols_and_candidates`; security review: unit `test_comment_rows_stay_linear_on_unclosed_block_openers` (killed after 60 s before the fix, 156 ms after) and `test_enclosing_symbols_match_the_scan_and_stay_fast_when_overlapping` (3.9 s before, 54 ms after), both seen failing first. Agent unit: `test_instructions_read_unlisted_lines_directly`.
- Seen failing before the change: the first four MCP tests above, the `tool_profile` wording test, and the agent rule test; every review test (atlas, MCP, `tool_profile`, agent; the middle-row context unit test failed to compile before its field existed); `test_unlisted_line_names_shrink_before_rows_are_displaced` against the first cut. Written with or after the code: the first three atlas unit tests (each seen failing under a mutation of the code it covers), the first `tests` module test (seen failing with that class disabled), the language-table test, and the first three `grep_content` tests. Passing before the change (guards): `test_unlisted_line_names_never_fail_a_small_budget` and `test_text_grep_row_selection_keeps_the_file_ranking`.
- Existing: every full-profile golden, `test_lean_schema_and_instructions_cost_less_than_half_of_default`, and `test_default_profile_tool_list_and_instructions_are_unchanged` pass unchanged.

### [x] E17-40: Java and C# symbols through the language table
Owner: R2 · Estimate: 2d · Depends: E17-37, E17-39
Files: Cargo.toml, Cargo.lock, rust/compresso-store/src/{files.rs,symbols.rs,calls.rs,lib.rs}, rust/compresso-atlas/Cargo.toml, rust/compresso-atlas/src/{lib.rs,parsers.rs,parsers/languages.rs,parsers/java.rs,parsers/csharp.rs,parsers/declarations.rs,parsers/calls.rs,parsers/go.rs,declared_imports.rs,indexer.rs,trace.rs,cards.rs,queries.rs,edit.rs,task_identifiers.rs}, rust/compresso-atlas/tests/{parsers_java_csharp,indexer_java_csharp,parsers_rust,parsers_go,queries_impact_plan}.rs, rust/compresso-cli/tests/cli_retrieval.rs, rust/compresso-atlas/tests/fixtures/lang/{sample.java,sample.cs}, rust/compresso-mcp/tests/result_format.rs, rust/compresso-cli/src/commands/{doctor,retrieval}.rs, rust/compresso-mcp/src/tools/engine_tools.rs, docs/plan/{01-stack-and-workspace,02-schemas,03-interfaces,04-mcp-and-integrations,05-tickets}.md, docs/adr/{0009-tree-sitter-symbol-extraction,0033-call-sites-resolved-at-query-time}.md, decisions.md
Description (revised 2026-11-21 after an independent review and the owner's import decision): D8 deferred Go, Java, C/C++, C#, Ruby, and PHP behind the parser registry; Go landed in E17-37 (D102). D105 (proposed by the implementing session, confirmed by the owner on 2026-11-21) adds Java and C# as table entries, so symbols work for them: search, outline, unfold, trace and impact, module cards, enclosing-symbol tags on grep rows, symbol names in read-window notes, and the E17-39 row selection. C/C++, Ruby, and PHP are not part of this ticket. No Java or C# benchmark task exists, so no benefit was measured.
Acceptance criteria:
- `.java` and `.cs` files are indexed as languages `java` and `csharp` with `tree-sitter-java` 0.23.5 (MIT, ABI 14) and `tree-sitter-c-sharp` 0.23.5 (MIT, ABI 15) under tree-sitter 0.25; `cargo deny check licenses bans sources` and `cargo audit` pass; `Cargo.lock` gains only the two packages.
- Both are `LANGUAGES` entries (extensions, a walk extractor, import style, call family 4 and 5, module-doc and visibility rules, entry files and entry method, no test-file suffix, `package_is_directory` false, `call_rules: DeclaredPackages`, declarations reader, `//`/`/* */` comments, doctor sample); the exhaustiveness test lists `Java` and `CSharp`; existing languages are byte-identical (every existing test and golden passes unchanged, except `parsers_rust::test_for_path_extension_map`, which pinned `.java` as unparsed).
- Extraction (D105 (2)): declarations by one iterative walk over the declaration levels, nested types qualified through outer types, type parameters never in names, overloads and partial declarations one symbol each, signatures capped at 1,024 bytes (annotations dropped first so modifiers stay; per-name prefixes cut once), types nested deeper than 32 or with qualified names over 1,024 bytes skipped with their members, error-recovery nodes walked through and garbled names dropped.
- Imports (D105 (4), owner decision 2026-11-21): Java `a.b.*` and C# `using N;` naming a declared package are one stored record each (detail `package`, closure-neutral; no migration), expanded at query time: `impact` lists importers and the package's other files (C#: and nested namespaces) with exact `total`, `shown`, `truncated`; `trace` treats the package as imported. A type import (`a.b.C`, static, nested, C# `using static`) is one edge to the type's file (indexed once per run). A namespace the repository declares only children of is external. At most 4,096 import records per file, one `edge_limit` record marking a cut. The map is built once per run from scanned files only; only importers whose rows a change can alter are re-parsed (not every file with an external import).
- Calls (D105 (3)): conservative resolution by reachability (access modifiers; interface members public; Java `protected` from subclasses), file, package or namespace, imports, the declared supertype chain (8 levels, 64 types), owners by full path; library look-alikes (`Collections.sort`, `System.out.println`, `Console.WriteLine`, `java.util.Objects.equals`) never reach a repository method; member qualifiers (`Items.Add`, `INSTANCE.run`) are values (`name_only`); `super.`/`base.` never reach the caller's own type; test code reached only from test code; never across languages; a call the rules cannot place while the supertype chain leaves the repository is `unplaced` and makes the trace incomplete.
- The repository map lists a Java or C# file's types and members, not its package or namespace symbol.
- Module cards read the Javadoc above `package` (or the first type) and the `///` summary above the first C# type; `public` top-level types are public; `package-info.java`, `Program.cs`, and files with a static `main`/`Main` are entry points.
- The parser version gains `+java-csharp-v1` (before `+go-v1`), so existing stores re-parse every file once on the next `compresso index`.
- `rust/compresso-mcp/tests/golden/*`, tool descriptions, schemas, instructions, and the lean surface (2,008 of 4,033 o200k tokens) are unchanged.
Test cases:
- `parsers_java_csharp`: `test_java_and_csharp_files_select_their_parsers`, `test_java_declarations_are_qualified_through_their_outer_types`, `test_java_imports_keep_static_and_wildcard_names`, `test_java_calls_record_plain_self_path_receiver_and_new_forms`, `test_csharp_declarations_are_qualified_through_their_outer_types`, `test_csharp_usings_name_what_they_import`, `test_csharp_calls_record_forms_and_rewrite_using_aliases`, `test_csharp_file_scoped_and_nested_namespaces_and_partial_types`, `test_java_and_csharp_overloads_are_one_symbol_each_with_their_signatures`, `test_java_and_csharp_local_and_anonymous_declarations_are_not_symbols`, `test_java_and_csharp_malformed_sources_still_extract_complete_items`, `test_java_and_csharp_signatures_are_bounded`, bounded time: `test_java_and_csharp_many_members_in_one_type_extract_in_bounded_time`, `test_java_and_csharp_deep_type_nesting_is_bounded`, `test_csharp_deep_namespace_nesting_is_bounded`, `test_java_and_csharp_huge_chained_calls_extract_in_bounded_time`, `test_java_and_csharp_many_field_names_behind_a_huge_annotation_are_linear`; `parsers_go::test_go_many_value_names_with_a_huge_type_are_linear` (time bounded; the cut itself is pinned by `parsers::tests::test_per_name_signatures_copy_a_bounded_prefix`).
- `indexer_java_csharp`: `test_java_and_csharp_files_are_indexed_with_language_and_symbols`, `test_java_imports_are_package_records_and_type_edges_expanded_by_impact`, `test_csharp_usings_are_namespace_records_and_impact_lists_namespace_mates`, `test_impact_over_a_large_namespace_reports_exact_totals`, `test_java_calls_resolve_by_scope_and_visibility`, `test_csharp_calls_resolve_by_scope_partial_types_and_visibility`, `test_java_library_look_alikes_stay_external_and_variables_are_name_only`, `test_an_external_csharp_namespace_never_shadows_a_repository_type`, `test_the_map_lists_types_not_package_or_namespace_symbols`, `test_interface_members_without_modifiers_are_public`, `test_inherited_and_protected_members_resolve_through_the_supertype_chain`, `test_a_call_that_may_be_inherited_from_a_library_type_is_unplaced`, `test_capitalized_member_qualifiers_are_values_not_library_types`, `test_a_long_annotation_never_hides_the_public_modifier`, `test_java_and_csharp_calls_never_cross_languages`, `test_overloaded_calls_are_listed_as_ambiguous`, `test_java_and_csharp_module_cards_read_docs_public_types_and_entry_points`, `test_a_csharp_main_or_program_file_is_an_entry_point`, `test_a_file_added_to_an_imported_package_or_namespace_gains_its_importers`, `test_a_file_added_to_a_package_reaches_its_wildcard_importers_without_reparsing_them`, `test_a_large_namespace_import_is_one_record_and_still_traced`, `test_a_changed_file_that_moves_package_re_resolves_its_importers`, `test_a_removed_file_leaves_its_importers_edges`, `test_an_excluded_java_file_declares_no_package`, `test_a_store_indexed_before_java_and_csharp_support_reindexes_their_files`.
- `result_format`: `test_text_results_name_java_and_csharp_symbols_and_rank_doc_rows_last`.
- Unit: `declared_imports::tests::{test_java_imports_resolve_to_one_package_record_or_one_type_file, test_csharp_usings_are_one_namespace_record_and_types_reach_their_file, test_java_and_csharp_never_resolve_into_each_other}`, `indexer::tests::test_declared_imports_are_one_record_per_package_and_capped_per_file`, `parsers::tests::test_signature_prefix_cuts_once_just_past_the_bound`, `trace::tests::test_declared_supertypes_read_java_and_csharp_signatures`, `parsers::declarations::tests::test_modifiers_skip_annotations_and_attributes`, `cards::tests::{test_java_and_csharp_public_types_carry_the_public_modifier, test_java_package_doc_is_the_javadoc_above_the_package_or_first_type, test_csharp_type_doc_is_the_summary_above_the_first_type}`, `parsers::languages::tests::test_java_and_csharp_use_declared_package_rules_and_their_own_families` (and the exhaustiveness and comment-syntax tests extended), `task_identifiers::tests::test_java_and_csharp_file_names_are_paths`, `edit::tests::test_csharp_verbatim_identifiers_are_renamed_and_java_dollar_names_are_not`, CLI `doctor::tests::test_grammar_check_loads_and_names_java_and_csharp`.
- Seen failing before the change: 12 of the 15 `indexer_java_csharp` tests first written (all but the cross-language, excluded-file, and language/symbol tests) against the extractor with stub import, trace, and card rules; `test_a_changed_file_that_moves_package_re_resolves_its_importers` and `test_a_file_added_to_a_package_reaches_an_importer_with_no_unresolved_imports` with the re-resolution trigger disabled; `test_java_and_csharp_malformed_sources_still_extract_complete_items` against the first walk (it skipped error nodes); `test_a_namespace_over_the_import_bound_is_marked_and_still_traced` with the capped-import tier disabled; `task_identifiers::tests::test_java_and_csharp_file_names_are_paths`; review: `test_an_external_csharp_namespace_never_shadows_a_repository_type` (a `using` of an external namespace hid a repository type of the same last name) and `test_the_map_lists_types_not_package_or_namespace_symbols` (with the map filter disabled). Written with or after the code and not seen failing: the other parser tests (written with the walker; the namespace-nesting test with its bound), the unit tests above, the `result_format` test, and the doctor test; `parsers_rust::test_for_path_extension_map` was updated without being run against the old code; the edit test passes on the old code too (a guard: C# needed no edit rule).
- Review round (2026-11-21), seen failing before the change: the two new `declared_imports` unit tests; `test_an_external_csharp_namespace_never_shadows_a_repository_type`; the five trace tests for M2, M3, S3, S4 (`test_interface_members_without_modifiers_are_public`, `test_inherited_and_protected_members_resolve_through_the_supertype_chain`, `test_a_call_that_may_be_inherited_from_a_library_type_is_unplaced`, `test_capitalized_member_qualifiers_are_values_not_library_types`, `test_a_long_annotation_never_hides_the_public_modifier`); the rewritten import tests (`test_a_file_added_to_a_package_reaches_its_wildcard_importers_without_reparsing_them` failed with 3 files parsed instead of 1, `test_a_large_namespace_import_is_one_record_and_still_traced`, `test_csharp_calls_resolve_by_scope_partial_types_and_visibility` before trace read package records). Not seen failing: the three M1 bounded-time tests pass on the quadratic code too on this machine (9.6 s, 8.3 s, 8.0 s against a 20 s bound; 3.1 s, 2.1 s, 2.0 s after), so the cut is pinned by the unit test written with it; `test_impact_over_a_large_namespace_reports_exact_totals`, `test_java_imports_are_package_records_and_type_edges_expanded_by_impact`, and `test_csharp_usings_are_namespace_records_and_impact_lists_namespace_mates` were written before the `impact` change but run only after it compiled; the CLI impact fix has no new test.
- Review round 3 (2026-11-21; orchestrator design change for same-package dependents, awaiting owner confirmation): same-package, default-package, and global-namespace files are `impact` dependents only with call-site evidence (a type of the file, or a member name no other file defines), unevaluable files counted in `unevaluated` with `complete` false; a namesake type file re-resolves importers; package-qualified calls reach their type; relative and `global` C# usings; cards and map ranking count whole-package imports; more than 64 packages in a file clears `complete` in `impact` and `trace`; package records count as resolved imports; the signature cut is timed against a short type. Tests: `indexer_java_csharp::{test_a_second_file_of_an_imported_type_matches_a_full_index, test_default_package_and_global_namespace_dependents_need_no_import, test_a_mate_with_cut_call_sites_makes_impact_incomplete, test_namespace_mates_need_a_call_naming_the_type_or_a_member_only_it_defines, test_too_many_namespaces_in_one_file_clear_complete, test_package_qualified_calls_reach_their_type, test_relative_and_global_usings_resolve_like_the_compiler, test_cards_and_map_ranking_count_package_imports, test_the_index_report_counts_package_records_as_resolved}`, `parsers::tests::test_a_cut_ending_in_spaces_keeps_the_whole_texts_signature_and_mark`, `result_format::test_impact_result_names_totals_and_the_linking_package`, `cli_retrieval::test_impact_honours_limits_above_the_default_and_reports_a_cut`. Seen failing first: the namesake, package-qualified, relative/global-using, and cards/map tests; the CLI limit test (against the code before b5a258b: 100 shown instead of 120); the cut by the byte-counter test of round 4 (the round-3 timing tests were replaced). Written before the code but first run after it compiled (they need new report fields): the default-package/global-namespace and cut-call-site tests. Written with or after the code: the shared-name, too-many-namespaces, index-report, signature-mark, and MCP impact tests.
- Review round 4 (2026-11-21; simpler, honestly labelled behaviour): evidence is a call naming one of the file's types only; files in scope without evidence are counted `unproven` (and `unevaluated` when a marker row shows their call sites were cut, no migration), either clearing `complete`; a C# namespace appearing or disappearing re-parses every C# file with an import; a `global using` reaches its own project (nearest `.csproj` directory, else the repository); evidence work bounded (64 files, 64 types each); cards and map count package imports from aggregates; the signature cut is pinned by a byte counter, not timing. Tests: `indexer_java_csharp::{test_a_mate_cut_at_the_examined_call_bound_is_unevaluated, test_package_mates_without_call_evidence_are_unproven, test_a_global_using_reaches_only_its_own_project, test_relative_usings_after_a_new_namespace_match_a_full_index}`, `parsers::tests::test_per_name_signatures_copy_a_bounded_prefix` (replaces the two timing tests); updated for the new labels: `test_csharp_usings_are_namespace_records_and_impact_lists_namespace_mates`, `test_java_imports_are_package_records_and_type_edges_expanded_by_impact`, `test_namespace_mates_are_listed_only_for_a_call_naming_the_type`, `test_default_package_and_global_namespace_dependents_need_no_import`, `result_format::test_impact_result_names_totals_and_the_linking_package`. Seen failing first: the examined-bound test. Written after the code, then seen failing under a mutation of the code each covers: the unproven, global-scope, relative-using, and byte-counter tests.
- Closing pass (2026-11-21, last change round; no new inference): `complete` for Java and C# is stated as covering import-backed dependents and the package scope; a use by package-qualified name without an import stays a documented gap (a listing of qualified calls was added and then removed in the final patch: unbounded reads and wrong packages); evidence counts only plain calls to the type and calls qualified by it; C# namesake files re-resolve importers by file name alone (one set lookup per edge); the 64-file bound applies before any work and bounded files count as `unevaluated`; module-card directory pairs bounded at 100,000 with `truncated`; a file importing its own package does not count for itself; known gaps (type-only uses, fully qualified uses without an import, project files, the global-using fallback, the coarse C# re-parse with its measured cost, the Go same-package gap) listed in D105. Tests: `indexer_java_csharp::{test_a_package_qualified_use_without_an_import_is_not_listed (final patch; replaces the listing test), test_a_method_named_like_a_type_is_not_evidence, test_the_evidence_bounds_count_what_they_skip_as_unevaluated, test_a_relative_using_static_namesake_matches_a_full_index, test_a_global_using_without_a_project_file_reaches_the_whole_repository, test_the_cut_marker_is_invisible_to_trace_and_counts}`. Seen failing first: the method-name, bounds, and namesake tests; the global-using fallback and cut-marker tests passed on the code before the pass (guards).
- After the final patch: the CLI `compresso impact` `complete` agrees with the query and the MCP result (false when `unproven`, `unevaluated`, or `packages_limited` is set; each shown when set). Test `cli_retrieval::test_impact_complete_agrees_with_the_query_on_unproven_files`, seen failing first (CLI `complete: true`).
- Existing: every parser, indexer, trace, card, grep, and golden test passes unchanged except `parsers_rust::test_for_path_extension_map`.

### [x] E17-41: The indexer links imports once per run and commits files in bounded batches
Owner: R2 · Estimate: 1.5d · Depends: E17-29, E17-37
Files: rust/compresso-store/src/{index_write.rs,lib.rs,symbols.rs,calls.rs,files.rs,repos.rs}, rust/compresso-atlas/src/indexer.rs, rust/compresso-atlas/src/indexer/write_batch.rs, rust/compresso-atlas/tests/indexer_write_path.rs, docs/plan/{03-interfaces,05-tickets}.md, decisions.md
Description: A full index of a private TypeScript monorepo of about 5,900 indexed files took 19.7 s, and the single writer thread spent about 54% of that time in one per-file statement that scanned every unlinked import edge of the repository (imports of modules outside the repository stay unlinked forever) and about 19% in per-file commits. D106 (proposed by the implementing session, confirmed by the owner on 2026-11-21; records the owner's decision of 2026-11-21 as given) links pending imports once per index run and commits files in bounded batches, with no store migration.
Acceptance criteria:
- Linking: one pass per run (`Store::link_pending_edges`), after deletions, also on a run with nothing to re-index and on a run that stops early; the same edges as the per-file statement (`to_file_id IS NULL` and `resolved`, or `unresolved` with `target_deleted`), set to `resolved` with no detail; one walk of the unlinked edges in id order through `idx_edges_to`, committed every 32,768 edges; idempotent. `replace_indexed_file`/`_with_calls` keep their one-file link for other callers; every production write (the CLI, the MCP server, the edit tool's refresh) goes through the indexer. No migration.
- Batches: at most 32 files, 8 MiB of source, or 250 ms since the oldest buffered file; one IMMEDIATE transaction that commits after 200 ms even with files left; no per-file savepoint (FTS5 flushes at each one); removed files deleted 64 per transaction. A file that fails to write leaves the files before it committed and itself and every later file unchanged, and stops the run with the store's error; a panic writes and links the files before it, then resumes.
- Readers see each file whole; progress events, their order, and every `IndexReport` count are identical to the per-file write; memory grows by at most 32 prepared files; the buffer's age bound also holds while waiting for the next file. A `pending_target` import is read as resolved once its target file exists (`resolved_edges`), and one whose target is not committed makes MCP and CLI impact and `trace` incomplete. The linkable set is `pending_target` and `target_deleted` exactly.
- Every index write is an IMMEDIATE transaction (also `delete_files`, `stamp_index_version`, and `replace_indexed_file_with_calls`), and so are `record_operation` and `replace_symbols`, the other transactions that read before writing; a `WritePacer` leaves the lock free for 120 ms after each second of back-to-back writes.
- Evidence in D106: before/after on two private repositories and two generated worst cases (three interleaved runs each, fresh scratch stores), store equality by row counts and digests, an incremental scenario against a fresh full index, `kill -9` at seven points then a re-index, scripted second-process runs (ledger writers, readers checking whole files, a lock-wait probe; main fails all of them), a pacer A/B, the instrumented longest lock hold, and a fresh profile with the two worker-side costs explained.
Test cases:
- Atlas (`indexer_write_path`): `test_full_index_with_many_unresolved_external_imports_finishes_in_bounded_time` (20 files holding 100,000 external imports, then 2,500 small files; 30 s bound: 51.8 s on main in a debug build, about 5 s after), `test_a_second_store_reads_and_writes_during_a_full_index_without_lock_errors`, `test_an_index_run_links_imports_an_interrupted_run_left_pending`, `test_incremental_runs_end_identical_to_a_fresh_full_index`, `test_removing_more_files_than_one_delete_chunk_removes_them_all_in_order`.
- Review (`indexer::write_batch::tests`): `test_readers_between_a_batch_commit_and_the_link_pass_see_dependents_or_incomplete`, `test_buffered_files_commit_while_a_later_file_is_slow_to_prepare`; store: `ops::tests::test_a_session_ledger_write_waits_for_another_connections_write_lock`, `index_write::tests::test_only_pending_and_deleted_targets_are_linked_by_path`. All four seen failing first (the last against the old linkable clause).
- Atlas unit (`indexer::tests`): `test_batch_size_and_lock_hold_bound_leave_the_index_unchanged`, `test_a_file_that_fails_to_write_keeps_the_files_before_it_in_its_batch`, `test_a_panic_while_committing_keeps_and_links_the_files_before_it`; `test_call_sites_the_store_rejects_are_dropped_without_failing_the_file` now writes through the batch writer.
- Store unit (`index_write::tests`): `test_batch_write_commits_the_files_before_a_failing_file_and_leaves_it_unchanged`, `test_batch_write_reports_a_file_that_fails_validation_without_writing_it`, `test_batch_write_commits_and_returns_once_the_hold_budget_is_spent`, `test_link_pending_edges_links_what_a_per_file_write_links_and_nothing_else`, `test_link_pending_edges_matches_per_file_linking_across_chunks`, `test_write_pacer_leaves_the_lock_free_after_continuous_writes`.
- Seen failing before the change (run against 1c8471b): the bounded-time test (51.8 s) and the second-store test (`database is locked`, extended code 517). The interrupted-run test needs the new store API and was written with it. Written with or after the code and not seen failing: the incremental and delete-chunk tests (guards that pass on main), the three atlas unit tests, and the store unit tests.
- Existing: `test_per_file_transaction_isolation`, `test_parallel_index_matches_serial_index`, `test_parallel_index_stops_at_lowest_ordered_failure`, and every other workspace test pass unchanged.

### [x] E17-42: SQL definitions and references through a hand-written statement scanner
Owner: R2 · Estimate: 2d · Depends: E17-37, E17-39, E17-40
Files: rust/compresso-store/src/files.rs, rust/compresso-atlas/src/{lib.rs,parsers.rs,parsers/languages.rs,parsers/sql.rs,parsers/sql/{references,tests,audit}.rs,sql_definitions.rs,search.rs,queries.rs,cards.rs,scanner.rs,indexer.rs}, rust/compresso-atlas/tests/{parsers_sql,indexer_sql,parsers_rust,edit}.rs, rust/compresso-atlas/tests/fixtures/lang/sample.sql, rust/compresso-mcp/src/tools/{atlas_nav,text}.rs, rust/compresso-mcp/tests/tools_nav.rs, docs/plan/{00-overview,02-schemas,03-interfaces,04-mcp-and-integrations,05-tickets}.md, decisions.md
Description: The owner's largest repository holds a private Postgres migration set of about 700 files that Compresso indexed as unparsed text. D107 records the owner's decisions of 2026-11-21 (SQL symbols through a hand-written, Postgres-tolerant scanner, no grammar dependency, no Solidity; "yes raise the limit" for SQL files, edit limit unchanged) and the implementing session's proposal for everything else (awaits owner confirmation). No SQL benchmark task exists, so no retrieval benefit was measured.
Acceptance criteria:
- `.sql` files are language `sql` (no migration; the column is free text) through a hand-written table entry: `--` and `/* */` comments, leading-comment module doc, no imports or entry files, call family 6, parser version `+sql-v1`; the exhaustiveness test lists `Sql`; existing languages are byte-identical (only `parsers_rust::test_for_path_extension_map` gains `.sql`).
- The scanner splits statements in one linear pass, tolerant of any dialect, never failing a file: comments (nested blocks), strings (`''`, `E'…'`), quoted identifiers, dollar bodies, parentheses, routine `BEGIN … END`, psql meta-commands, COPY data; unterminated constructs run to the end of the file. Bounds: 4 MiB per SQL file (a configured ceiling caps SQL too), 4,096 symbols, 16 MiB of covered line bytes, 16,384 references, 256 head tokens per statement, 1,024-byte names and signatures; every cut is marked (references as other languages' call cuts, symbols through `symbols_truncated` and the file symbol); pinned by step and byte counters.
- One symbol per definition statement (D107 (2) kinds and names), lines covering the whole statement, plus a `script` symbol per file; definitions inside a top-level `DO` body are symbols with their own lines (D107 (4)).
- References as plain call sites with the kind in the qualifier (D107 (3)); `trace --to <table>` lists what touches a table; grep rows carry their enclosing symbol.
- Search lists a SQL name's definitions in path order, last first, with a `sql_definitions` diagnostic (later drops stated as read, without an identity claim; mixed spellings; directories; the path scope respected); `compresso_unfold` of a SQL definition lists the others in `candidates` within its budget; down-migration sections stay out of groups and drop labels; `impact` of a SQL file is never complete (D107 (2), (5), (6)).
- SQL files up to 4 MiB are indexed, read in windows, grepped, searched, outlined, and unfolded; the edit tool refuses a SQL file above 1 MB as too large; every other language keeps 1,000,000 bytes.
- MCP goldens, schemas, instructions, and the lean surface (2,008 of 4,033 tokens) are unchanged; no new dependency; no store migration.
Test cases:
- Atlas unit (`parsers::sql::tests`), written before the scanner and seen failing against a stubbed scanner (21 of 23): `test_semicolons_in_comments_strings_and_quoted_names_do_not_split`, `test_nested_block_comments_hide_semicolons`, `test_dollar_quoted_bodies_hide_semicolons_quotes_and_other_tags`, `test_positional_parameters_and_dollar_in_names_are_not_quotes`, `test_backslash_escapes_only_in_e_strings`, `test_begin_atomic_bodies_are_one_statement_and_transactions_are_not_blocks`, `test_unquoted_trigger_bodies_close_on_their_own_end`, `test_semicolons_inside_parentheses_do_not_split`, `test_copy_data_is_skipped_to_its_terminator_line`, `test_psql_meta_commands_end_at_their_newline`, `test_unterminated_quotes_comments_and_bodies_run_to_the_end_without_failing`, `test_line_ranges_cover_the_whole_statement_with_its_body`, `test_crlf_and_a_missing_final_semicolon_keep_lines_in_range`, `test_tables_views_and_their_modifiers`, `test_functions_and_procedures_sign_with_arguments_and_returns`, `test_triggers_indexes_policies_and_rules_are_named_by_their_table`, `test_types_domains_schemas_extensions_sequences_and_publications`, `test_statements_are_classified_and_unnamed_definitions_are_reported`, `test_empty_overlong_and_control_character_names_make_no_symbol`, `test_signatures_copy_a_bounded_prefix`, `test_symbols_are_capped_in_number_and_covered_bytes`. Passing against the stub, then seen failing under a mutation of the code each covers: `test_hostile_input_is_scanned_in_linear_steps` (a backtracking dollar opener took 44,950,239,395 steps for 888,890 bytes), `test_signatures_are_bounded_and_marked_when_cut`; `test_a_signature_cut_by_the_head_token_cap_is_marked` (seen failing under a mutation, and again after it was strengthened in Stage 2: a statement longer than the head kept only its first word as signature, found by the real-data self-check).
- Atlas unit, Stage 2, written before the code and first run after it compiled (they need the new API), then each seen failing under a mutation (no `DO` scan, no control-word skip, no token stream, no head references): `test_a_file_symbol_covers_the_whole_file`, `test_definitions_reference_their_table_trigger_function_and_parent`, `test_alter_drop_comment_grant_and_revoke_reference_their_objects`, `test_data_statements_and_top_level_from_and_join_are_best_effort_references`, `test_function_bodies_are_never_read_for_references_or_definitions`, `test_do_block_definitions_are_symbols_with_their_inner_lines`, `test_do_blocks_are_scanned_one_level_deep`, `test_references_are_capped_per_file`, `test_overlong_reference_names_are_skipped`, `test_input_at_the_sql_ceiling_is_bounded`; `parsers::languages::tests::{test_sql_is_a_hand_written_extractor_without_imports_or_entry_files, test_only_sql_raises_the_per_file_ceiling}`; ignored audit `parsers::sql::audit::test_audit_sql_files_from_a_list` (counts only).
- Atlas integration: `parsers_sql::{test_sql_files_select_the_sql_extractor, test_a_postgres_migration_lists_its_definitions_with_whole_statement_lines, test_a_postgres_migration_records_what_each_statement_touches, test_malformed_sql_never_fails_extraction}` (written after the code, not seen failing); `indexer_sql::{test_sql_files_are_indexed_with_language_and_definition_symbols, test_definitions_on_one_long_line_are_counted_in_bounded_bytes (seen failing with the covered-bytes cap removed: 3,769,240,000 bytes counted), test_a_store_indexed_before_sql_support_reindexes_sql_files (seen failing before the version bump), test_trace_to_a_table_lists_the_statements_that_touch_it, test_search_lists_redefinitions_in_path_order_with_drops_and_spellings, test_impact_of_a_sql_file_is_never_complete (each seen failing under a mutation: no call family, no search annotation, no impact rule), test_search_without_sql_hits_is_unchanged, test_unfold_of_a_sql_definition_names_its_other_definitions, test_grep_rows_in_sql_files_name_their_enclosing_symbol (written after the code), test_sql_files_up_to_their_own_ceiling_are_indexed_and_served}`; `edit::test_a_sql_file_above_one_megabyte_is_indexed_but_not_editable` (seen failing with the SQL ceiling at 1,000,000 bytes).
- MCP: `tools_nav::test_unfold_of_a_sql_definition_lists_its_other_definitions_last_first` (written after the code; seen failing with the candidates emptied).
- Closing pass (2026-11-21), each seen failing before its code: `parsers::sql::tests::{test_a_do_body_in_a_down_section_is_down, test_a_quoted_separator_does_not_split_a_meta_command, test_copy_from_a_file_named_stdin_reads_no_data, test_a_long_drop_name_is_stored_cut, test_drops_record_the_written_name_and_table}`, `indexer_sql::test_later_drops_are_stated_as_read` (replaces `test_drops_label_only_what_they_are_shown_to_name`; written against the new API, first run after it compiled).
- Review round 2 (2026-11-21), each seen failing under a mutation of the code it covers (written before the code, first run after it compiled, as most need new API): unit `parsers::sql::tests::{test_a_byte_order_mark_does_not_hide_the_first_definition, test_every_reference_cut_marks_the_file, test_an_unnamed_index_references_its_table, test_a_trigger_column_named_begin_opens_no_block, test_copy_meta_command_reads_data_only_from_stdin, test_a_meta_command_separator_ends_the_meta_command, test_the_references_privilege_is_not_a_reference, test_quoted_name_parts_keep_their_quotes_when_needed, test_a_table_named_script_is_not_the_file_symbol (passed a first mutation; strengthened with a schema of that name, then seen failing), test_down_migration_statements_are_labelled, test_a_cut_symbol_list_is_reported}`, `sql_definitions::tests::test_path_order_compares_digit_runs_as_numbers`; `indexer_sql::{test_sql_definitions_respect_the_search_path_scope, test_sql_definitions_are_in_numeric_path_order_and_name_their_directories, test_down_migrations_stay_out_of_groups_and_drop_labels}`; `tools_nav::test_unfold_names_how_many_other_sql_definitions_it_lists`; `test_sql_files_up_to_their_own_ceiling_are_indexed_and_served` reads through the canonical root (it failed with the default macOS temporary directory). Not seen failing: `test_path_order_compares_digit_runs_as_numbers` (written with the function).
- Review (each seen failing against the code before its fix): `indexer_sql::{test_search_shows_the_last_definitions_in_path_order_when_the_limit_holds_fewer, test_a_later_drop_index_labels_the_index, test_trace_finds_a_schema_qualified_sql_name, test_the_map_lists_sql_definitions_not_the_file_symbol}`, `tools_nav::test_unfold_of_a_large_sql_definition_keeps_its_candidates_within_budget`.
- Existing: every other workspace test and golden passes unchanged.

### [x] E17-43: Each language's extraction query is compiled once per process
Owner: R2 · Estimate: 0.5d · Depends: E17-37, E17-41
Files: rust/compresso-atlas/src/parsers.rs, docs/plan/05-tickets.md, decisions.md
Description: The owner asked on 2026-11-21 for three performance follow-ups that change no result, only how much work is done (E17-43, E17-44, E17-45). D106's follow-up (a): D106's sampled profile of a full index of a private TypeScript monorepo (about 5,900 indexed files at that time; about 6,600 when D108 measured it again) put about a fifth of every worker's time in `ts_query_new`, because Rust, TypeScript, TSX, JavaScript, Python, and Go compiled their tree-sitter extraction query again for every file. D108 (proposed by the implementing session, confirmed by the owner on 2026-11-21) compiles it once per language and shares it.
Acceptance criteria:
- One `OnceLock` per table language holds the compiled `Query` and the kind of each capture; every thread shares it (`Query` is `Send + Sync`); parsers and query cursors stay per call. No new dependency, no parser-version change.
- The query is compiled on the first parse that needs it, after the parser is configured and the source parsed, as before, so a `ParseFailed` stays a `ParseFailed`; a query that does not compile fails every call with the same `ParseError::Query` (doctor's grammar check unchanged) and is not compiled again. Java and C# walk their trees, SQL, shell, and text have no grammar query, and call extraction compiles none, so nothing else changes.
- Evidence in D108: sampled profiles before and after attributed per thread, full-index wall time in interleaved runs on the two private repositories and a generated mixed-language repository, and fresh stores from both builds equal by row counts and order-independent digests of files, symbols, edges, call sites, and full-text rows.
Test cases:
- Atlas unit (`parsers::tests`): `test_each_language_query_is_compiled_once_per_process` (six languages extracted three times on each of four threads; a test-build counter per language must read 1; seen failing first with 12 for Rust), `test_a_query_that_fails_to_compile_fails_every_call_the_same_way` (written with the code: it calls the new cache directly).
- Existing: every other workspace test passes unchanged.

### [x] E17-44: A C# namespace change re-parses only the files whose `using` can bind to it
Owner: R2 · Estimate: 1d · Depends: E17-40, E17-41
Files: rust/compresso-atlas/src/{declared_imports.rs,indexer.rs}, rust/compresso-atlas/tests/indexer_csharp_rebind.rs, docs/plan/05-tickets.md, decisions.md
Description: D105's known gap (e): a run in which any C# namespace appeared or disappeared re-parsed every C# file with an import, because a `using` written relative to an enclosing namespace is stored by the full name it resolved to. D109 (proposed by the implementing session, confirmed by the owner on 2026-11-21) re-parses only the files whose stored import records can resolve differently, using only what the store keeps (no migration).
Acceptance criteria:
- Which files: a C# file, not removed, whose stored records can bind differently. Its declared namespaces and their dotted prefixes bound where its `using` directives sit, so each stored name can have been written as itself or as what follows one of them; each such written name gives the full names it was looked up as. The file is re-parsed when one of them is a namespace that appeared or disappeared, has one as a dotted prefix (unless a namespace kept across the run lies between them), or is a dotted prefix of one (unless it is itself kept); or when a type file joined or left the namespace it falls in (`N.T`, from the file's namespace and name up to its first `.`). A record that resolved to a file keeps only that file, so its name is taken as each `namespace.FileName` the target declared, followed by an unknown rest. Some records always re-parse their file, because their written names cannot be bounded: a cut list, a target whose namespaces are unknown, and a name that may be written relative to one of the importer's namespaces lying below the type it reached.
- Every incremental run leaves the store identical to a fresh full index. The type-file rule also closes a divergence found on main: a type file joining an existing nested namespace did not re-resolve a relative `using static` that had resolved to nothing.
- One importer's check costs the depth of its names, not the size of the change (hash lookups on dotted prefixes). One index run reads the resolved edges and the unresolved import targets at most once each.
- Evidence in D109: re-parsed files and time on a generated 5,000-file C# repository before and after, single-file and 500-file changes, store equality with main, and incremental against fresh full index for every step.
Test cases:
- Atlas unit (`declared_imports::tests`): `test_may_rebind_selects_every_importer_whose_records_change` (60,000 seeded cases over three segments, namespaces up to three deep, plain, static, alias, global, and relative `using` directives, one to three files added, removed, or moved per case, files declaring one or two namespaces: every importer whose resolved records change is selected, the indexed lookup agrees with the scan it replaced, and most unaffected importers are left alone), `test_may_rebind_is_conservative_where_the_written_name_is_unknown`, written with the code; each rule of the filter was removed in turn and the differential failed every time. `test_may_rebind_cost_does_not_grow_with_the_change` (10,000 changed namespaces and 10,000 type files; at most 256 lookup steps for one importer, counted) seen failing first with 200,000 steps against the first, scanning cut. `indexer::tests::test_an_index_run_reads_each_edge_list_at_most_once` (a run that removes a file, grows a Go package, and adds a nested C# namespace reads each list once) seen failing with the cache disabled: 4 reads of the resolved edges and 2 of the unresolved targets.
- Atlas integration (`indexer_csharp_rebind`): `test_a_namespace_change_reparses_only_usings_that_can_bind_to_it` (fourteen scenarios: new nested namespace, sibling with a matching last segment, unrelated namespace with a matching last segment, namespace removed, children-only namespace added and removed, `global using` of a removed namespace, `global using static` whose namespace appears, `using static` whose package prefix grows, new ancestor namespace, relative alias, file-scoped namespace, type file joining an existing nested namespace, file moving namespace; incremental equals a fresh full index and twelve unrelated importers are never re-parsed). Seen failing first on main: the bound (all twelve re-parsed) and, for the type-file scenario, the equality. A rule that matches only the last segment fails the children-only scenario; removing the type-file or children-only clause fails its scenario.
- Existing: `indexer_java_csharp::{test_relative_usings_after_a_new_namespace_match_a_full_index, test_a_relative_using_static_namesake_matches_a_full_index, test_a_file_added_to_an_imported_package_or_namespace_gains_its_importers}` and every other workspace test pass unchanged.

### [x] E17-45: The repository staleness check runs on several threads
Owner: R2 · Estimate: 1d · Depends: E17-29, E17-40
Files: rust/compresso-atlas/src/queries.rs, rust/compresso-atlas/tests/queries_map_outline.rs, docs/plan/05-tickets.md, decisions.md
Description: D105's known gap (m): `repository_stale` resolves, opens, and when its modification time is within two seconds of the index hashes every indexed file. It runs on each CLI `grep` and `trace` and on each MCP `search`, `grep`, `trace`, `impact`, `repo_map`, and `context` call (the long-lived server repeats it per call); on a generated 50,000-file repository those calls took 6 to 13 s, most of it opening files. D110 (proposed by the implementing session, confirmed by the owner on 2026-11-21) keeps the exhaustive check and what `stale` means, and spreads the per-file checks over threads; the narrower options that would change what `stale` means are listed in D110 for the owner.
Acceptance criteria:
- Every indexed file is still checked with the same per-file test (resolution under the root, missing, outside the root, or not a file is stale; size, then modification time, then hash). From 256 indexed files up the checks run on up to one thread per CPU; below 256, on the calling thread. The thread count does not follow the configured indexing job limit (known gap).
- The outcome is the first non-fresh file's in store order, as the one-thread walk returned: a later stale file found first never hides an earlier error, nor a later error an earlier stale file; files after the first decisive one are not needed and are skipped.
- `stale` means the same for every tool; per-file staleness (grep rows, outline, unfold, the context planner's skip of changed files, the edit tool's refresh) is unchanged. No cache, no new dependency.
- Evidence in D110: the call graph of the check, CLI and MCP timings before and after on a generated 50,000-file repository in two modification-time regimes and on the two private repositories, and the outputs of search, grep, trace, impact, outline, unfold, repo_map, and context identical between builds apart from operation ids, also with one file edited after indexing.
Test cases:
- Atlas unit (`queries::tests`): `test_parallel_staleness_walk_returns_the_first_outcome_in_order` (400 seeded outcome lists against the one-thread walk at 2, 3, and 8 workers, and a held first item that is checked after a later decisive one). Written with the code (the walk is new); not seen failing.
- Atlas unit (`queries::tests`): `test_parallel_staleness_on_real_files_reports_the_earlier_file` (320 indexed files, four workers whatever the CPU count; two stale files, then the earlier file replaced by a directory before a stale one, the earlier held until the later is checked: the earlier file in store order decides, as the one-thread walk computes) seen failing, with the ordering unit test, when the reduction picks the later file.
- Atlas integration (`queries_map_outline`): `test_repository_stale_holds_on_the_parallel_path` (600 indexed files; one file edited, deleted, or replaced by a directory first, in the middle, and last in store order is stale; restored, fresh). A guard that passes on main; it takes the parallel path only on a machine with more than one CPU.
- Existing guards of the guarantees, unchanged and passing: `queries_map_outline::{test_repository_stale_trusts_an_untouched_file_and_rechecks_a_touched_one, test_repository_stale_detects_changed_and_deleted_file}`, MCP `tools_grep::test_grep_tool_marks_changed_files_and_the_stale_index`, `tools_trace::test_trace_tool_reports_a_stale_index`, `tools_read::test_search_stale_is_structured_success`, atlas `queries_task_facets::test_plan_context_reads_out_of_date_as_stale`, `edit::test_stale_files_are_edited_against_current_content_and_marked`.

### [x] E17-46: Go `impact` lists files of the same package that call the file
Owner: R2 · Estimate: 1d · Depends: E17-37, E17-40, E17-41
Files: rust/compresso-store/src/calls.rs, rust/compresso-atlas/src/{queries.rs,trace.rs,indexer.rs,indexer/write_batch.rs,parsers.rs,parsers/calls.rs,parsers/languages.rs}, rust/compresso-atlas/tests/{indexer_go_impact,indexer_go,parsers_go}.rs, rust/compresso-cli/src/commands/retrieval.rs, rust/compresso-cli/tests/cli_retrieval.rs, rust/compresso-mcp/src/tools/engine_tools.rs, rust/compresso-mcp/tests/result_format.rs, docs/plan/{03-interfaces,04-mcp-and-integrations,05-tickets}.md, decisions.md
Description: D105 known gap (f): in Go every file of a package (a directory) uses the others' declarations without an import, so `impact(a.go)` missed `b.go` of the same directory calling a function of `a.go` and still said `complete`. E17-40 fixed the same shape for Java and C# and left Go alone because the fix changes outputs the owner confirmed under D102; the owner asked on 2026-11-21 for this gap to be tackled. D111 (proposed by the implementing session, confirmed by the owner on 2026-11-21: the owner asked for this work that day and said "merge when green after review" once it had passed review) applies E17-40's rule to Go; it amends D102's confirmed `impact` outputs. No Go benchmark task exists, so no benefit was measured.
Acceptance criteria:
- `impact` on a Go file lists, besides its import-backed dependents (unchanged), the other Go files of its directory (only `_test.go` files for a test file) with a stored call that resolves by the trace rules to exactly one definition in the directory, in the file: a plain call to a function or struct type it defines, or a call on the enclosing method's receiver to a method it defines; kind `references`, `package` the directory (D111 (1), (2)).
- Receiver calls on variables or expressions, bare function arguments, composite literals, type uses, and constant reads are not evidence; a name defined in two files of the directory is evidence for neither; a call to a name the calling function binds anywhere inside it, or on a rebound receiver name, is recorded under the reserved `<shadowed>` qualifier: no evidence, and `trace` reports it as unplaced and its answer incomplete; names in function types or interface method signatures and plain `=` bind nothing (D111 (2), review round 2 and closing pass).
- Every other file in scope is `unevaluated` (call sites cut, or a check bound reached) or `unproven`; `complete` is false while either is non-zero, in the query, the MCP result, and the CLI alike (one rule, unchanged).
- An external test package is listed through its import as before; a file that declares nothing at package level has no same-package dependents.
- Bounds: 64 files checked per `impact` (for Go charged before the first store read; Java and C# exactly as on main), 400 names per Go file; each Go directory read once per call from the files key (at most 2 × 64 + 1 queries), at most 2,048 files per directory and 250,000 rows per call, files not read counted `unevaluated`; pinned by test-build counters and a query-plan test.
- A Go file whose call extraction is cut holds the cut marker row (as Java and C#); the parser version gains `+go-impact-v1`, so existing stores re-parse once; no store migration.
- A dependent linked at one depth by a file edge and through a package keeps the file edge's kind only, whichever arrives first (every language). Search, outline, unfold, grep, module cards, map ranking, and trace (apart from shadowed calls) are unchanged; Java and C# `impact` tests and goldens are unchanged; MCP schemas, descriptions, instructions, goldens, and the lean surface are unchanged; no new dependency.
Test cases:
- Atlas integration `indexer_go_impact`, written before the code and seen failing against it (10 of 11): `test_a_package_mate_calling_a_function_of_the_file_is_a_references_dependent`, `test_a_package_mate_without_a_call_is_unproven`, `test_an_external_test_package_reaches_the_file_only_by_import`, `test_an_internal_test_file_is_a_dependent_and_test_code_reaches_only_test_files`, `test_a_receiver_method_call_is_not_evidence_and_a_self_call_is`, `test_a_call_to_a_name_two_files_define_is_not_evidence`, `test_a_mate_cut_at_the_examined_call_bound_is_unevaluated`, `test_the_evidence_bounds_count_what_they_skip_as_unevaluated`, `test_incremental_index_of_a_package_matches_a_full_index`, `test_a_store_indexed_before_go_cut_markers_reindexes`; passing before the code (a guard): `test_a_file_that_declares_nothing_has_no_package_dependents`.
- Changed (D102 output amended), seen failing before the code: `indexer_go::test_go_package_imports_are_edges_to_each_package_file`.
- Written after the code, each seen failing under a mutation of the code it covers: `indexer::write_batch::tests::test_a_cut_go_file_gets_its_marker_row_and_a_cut_script_file_none` (marker gated on declared packages only), `queries::tests::test_go_package_evidence_work_is_bounded_by_the_check_bounds` (64-file bound removed: 304 queries instead of 193; the budget charged after the name check, as first written: 104 queries instead of 67 for a package of 65-function files, found by an independent review), `cli_retrieval::test_go_impact_complete_agrees_with_the_query_on_package_files` and `result_format::test_go_impact_result_lists_package_files_and_counts_unproven_ones` (Go dispatch disabled).
- Real data (read-only, counts only, round 2): see D111 **Real data** (generated module of 1,123 Go files and a public repository of 871).
- Review round 2 (2026-11-21), each seen failing before its code: `indexer_go_impact::{test_a_call_to_a_name_bound_in_the_function_is_not_evidence, test_a_rebound_receiver_name_is_not_a_self_call, test_a_file_edge_wins_over_a_package_link_in_either_order}` (the last also renames a directory); Written with the code, not seen failing: `queries::tests::test_go_package_evidence_work_is_bounded_by_the_check_bounds` (rewritten for the per-directory read; the round-1 code made 193 queries where it makes 3), `calls::tests::test_directory_reads_stay_in_the_directory_and_use_the_file_indexes` (store, with `EXPLAIN QUERY PLAN`), `parsers::tests::test_go_binding_scan_is_one_pass_per_function`. Passing before the code (a guard): `indexer_go_impact::test_a_self_call_reaches_only_its_own_types_method`. Changed: `parsers_go::test_go_local_declarations_and_func_literals_are_not_symbols` (`f()` after `f := func() ...` is no longer recorded); `indexer_go_impact::test_the_evidence_bounds_count_what_they_skip_as_unevaluated` (401 names past the new bound).
- Closing pass (2026-11-21): seen failing before the code: `indexer_go_impact::{test_a_shadowed_call_is_unplaced_in_trace, test_names_in_signatures_and_plain_assignments_do_not_shadow}`; written with the code, then seen failing under a mutation (no file cap: 30 listed instead of 9; no row-budget check: 0 unevaluated instead of 30): `queries::tests::test_go_directory_reads_stop_at_their_caps_and_count_the_rest`. Changed again: `parsers_go::test_go_local_declarations_and_func_literals_are_not_symbols` (`f()` is a path call with the `<shadowed>` qualifier); store `calls::tests::test_directory_reads_stay_in_the_directory_and_use_the_file_indexes` (language, end path, and row limit). Reverted: the round-2 change that charged the Java and C# budget before their symbols read, and its test (Java and C# `impact` is as on main; recorded as a follow-up in D111).
- Existing: every other workspace test and golden passes unchanged (Java and C# `impact` tests included).

### [x] E17-47: Larger edit batches, create, and session-created files in `compresso_edit`
Owner: R2 · Estimate: 2d · Depends: E17-32, E17-36, E17-38
Files: rust/compresso-atlas/src/{edit.rs,lib.rs}, rust/compresso-atlas/tests/{edit.rs,edit_batch.rs}, rust/compresso-mcp/src/{server.rs,profile.rs}, rust/compresso-mcp/src/tools/{edit.rs,text.rs,atlas_read.rs,common.rs}, rust/compresso-mcp/tests/{tools_edit.rs,tools_read.rs}, rust/compresso-cli/src/commands.rs, docs/plan/{03-interfaces,04-mcp-and-integrations,05-tickets}.md, docs/adr/0035-opt-in-verified-edit-tool.md, decisions.md
Description: A measured probe ran a 15-task editing suite with `compresso_edit` as the model's main edit path. The tool worked (about 350 changes in about 40 calls per run), but whole calls were rejected for reasons that sent the model back to shell scripts and the client's own tools: more than 16 operations (5 to 6 times per run, up to 22 wanted), a file created during the session ("not in the repository index"), identical `old` and `new` in a replace, no way to create a file, and `compresso_read` called with guessed arguments. D112 (proposed by the implementing session, awaiting owner confirmation) raises the limits, adds `create`, edits session-created files through an index refresh, makes identical replaces a reported no-op, and makes the errors name their cause. Delete and move were not asked for and are not added. The probe was not re-run.
Acceptance criteria:
- A call carries up to 64 operations (`EDIT_MAX_OPERATIONS`; the schema's `maxItems` is the same constant) and at most 4 MiB of path and text (`EDIT_MAX_REQUEST_BYTES`); above either the call is refused `limit_exceeded`, naming the bound, the count or bytes sent, and the limit, and saying to split the call; all or nothing as before; the other call bounds are unchanged.
- A `replace` or `insert` target that exists on disk but is not indexed makes the call refresh the index once first (index only, dry runs too, like a rename); it is edited only if the scanner then indexed it. Gitignored, secret, `.git`, control-file, binary, oversized, and linked targets are refused as for an indexed file; the index gate itself stays.
- `create` refuses any hidden component, `:`, Windows reserved device names, and two creates differing only by case (D112 review (b), (c)); the shared denylist covers the review's control files for edit and create.
- A path through a symbolic link, or a missing path, is refused `not_found` alike whatever the link points to. A refresh for an unindexed target runs at most once per call, never for a target the cheap checks refuse, and not again for a target remembered unchanged (at most 1,024 per process).
- `create` writes a new file into an existing directory, never over anything at the path (dangling link, directory, other-case name), never creates directories, refuses secret, `.git` (any case), control, scanner-excluded (requested and canonical directory spelling), protected, linked-directory, and unwritable-directory targets, in dry runs too; content at most 64 KiB without NUL; all or nothing with the other operations; a rollback removes it only while it holds the created bytes; it is never executable; later operations and renames of the call see it; it is indexed after the write.
- An identical `replace` checks its path gate, hash, and count, changes nothing, and reports `unchanged: true` and `matched`; a different count is still `count_mismatch`.
- Errors one operation causes name it; a count mismatch after an earlier operation edited the file says so; an unknown field names the fields its `op` takes; `outside_root` names the path.
- `compresso_read` errors for an unknown argument, a missing `path`, or a malformed `ranges` add one line naming the read arguments; an absolute path lexically under the root (no resolution, no `..`) or a `./` path is answered with the path to pass, and any other spelling keeps the old answer; the retry succeeds.
- New report keys appear only when true or non-zero; existing goldens are unchanged; the lean half-cost surface test and every profile and installer default are unchanged; no store migration, no new dependency, no process spawning.
Test cases:
- Atlas `edit_batch`, written before the code and seen failing against it (a copy without the new `unchanged` field, which did not compile yet): `test_a_call_carries_sixty_four_operations_and_the_limit_error_names_both_counts`, `test_the_call_text_is_bounded_in_total_bytes`, `test_a_file_created_after_the_last_index_is_edited_and_indexed`, `test_an_unindexed_file_passes_the_same_gates_as_an_indexed_one` (failed at the link case; its expectation was then widened to any refusal, since the scanner never indexes a link), `test_identical_old_and_new_is_a_reported_no_op_that_still_checks_its_count`, `test_request_errors_name_the_operation_that_failed`, `test_a_count_mismatch_after_an_earlier_operation_says_the_text_was_already_edited`.
- Atlas `edit_batch` create tests, written after the code, not seen failing against it: `test_create_makes_a_new_file_reports_it_and_indexes_it`, `test_create_refuses_any_existing_path`, `test_create_keeps_every_path_gate_an_indexed_file_has` (the case-insensitive branch runs where the file system is case-insensitive), `test_create_refuses_an_unwritable_directory_in_dry_runs_too` (Unix), `test_create_is_all_or_nothing_with_the_other_operations`, `test_create_content_is_bounded_text`. Then each run under a mutation of the code it covers: dropping the canonical-directory exclusion check fails `test_create_keeps_every_path_gate_an_indexed_file_has`; ignoring an existing path fails `test_create_refuses_any_existing_path`; skipping the directory write query fails `test_create_refuses_an_unwritable_directory_in_dry_runs_too`. Replacing the no-clobber hard link with a rename fails no test (the re-check before the write already catches the injected file; the link guards only the instant after it).
- Atlas unit tests written with the code, not seen failing against the old code: `edit::tests::test_a_created_file_is_removed_when_a_later_write_fails`, `edit::tests::test_a_create_never_overwrites_a_file_that_appeared_after_validation`.
- MCP `tools_edit::test_large_batches_create_and_unchanged_replaces_over_mcp`, written after the code and then seen failing against the original code (schema has no `create`).
- MCP `tools_read::test_read_argument_errors_name_the_arguments_that_work`, written after the code and then seen failing against the original read code (`unknown argument 'file_path'.` named no argument).
- Changed: atlas `edit::test_secret_gitignored_and_unindexed_paths_are_refused` (the unindexed case is now a binary file the scanner skips), `edit::test_oversize_and_non_utf8_files_are_refused` (17 inserts instead of the operation limit, inside the byte bound), `edit::test_bounds_are_enforced_before_anything_is_written` (over the operation limit is `Bound { Operations }`), the `CountMismatch` pattern in `edit.rs` (new field); MCP `tools_edit::test_the_report_cap_holds_for_a_full_batch_of_operations` (64 operations per batch).
- Independent security review (2026-11-22), D112 (a)-(g); written before the code and seen failing against it (by assertion): `edit_batch::{test_control_files_the_review_found_are_refused_for_edit_and_create, test_create_refuses_hidden_paths_with_a_one_line_reason, test_create_refuses_colons_and_reserved_device_names_everywhere, test_two_creates_differing_only_by_case_are_refused_in_dry_and_real_runs, test_a_link_is_refused_alike_whatever_it_points_to, test_the_scanner_skips_exactly_the_edit_temporary_pattern, test_a_target_the_scanner_would_refuse_never_costs_a_refresh, test_an_unchanged_target_a_refresh_did_not_index_is_remembered}`; tightened and then seen failing: `edit_batch::test_an_unindexed_file_passes_the_same_gates_as_an_indexed_one` (one answer for a link, was `outside_root`); changed: `edit::test_symlink_components_and_links_out_of_the_root_are_refused` (an indexed file swapped for a link out of the root is now `not_found`) and `edit_batch::test_create_refuses_an_unwritable_directory_in_dry_runs_too` (asserts `read_only` in both runs; returns early, with a note, only where the process can write a 0o555 directory). Written before the code, failing only to compile (the hook and helpers did not exist), then each seen failing under a mutation: `edit::tests::test_a_file_appearing_after_the_last_recheck_is_never_replaced` (a test-only hook plants a file between the last re-check and the link; fails when the hard link is replaced by a rename) and `edit::tests::test_create_judges_the_canonical_directory_spelling_on_every_file_system` (fails when the canonical-spelling check is dropped; runs on every file system). Bounds: refresh candidates are bounded by the operation count and read at most 8 KiB each; one refresh per call; the per-process memory holds at most 1,024 targets.
- Final review round, D112 (h)-(j); extended or written before the code and seen failing against it (by assertion): `edit_batch::test_control_files_the_review_found_are_refused_for_edit_and_create` (new names), `edit_batch::test_create_refuses_colons_and_reserved_device_names_everywhere` (`CONIN$`, `CONOUT$`, superscript digits), `edit_batch::test_a_non_utf8_text_file_is_skipped_and_the_index_continues` (the fixture's index failed with `ReadSource`), and `edit::tests::test_a_non_utf8_target_and_a_failed_refresh_cost_no_repeated_refresh` (failed first at its non-UTF-8 assertion, one refresh; after that fix, failed at its failed-refresh assertion, three refreshes; passes after (j)). No parser-version change.
- Existing: every other workspace test and golden passes unchanged, `lean_edit_text.txt` included.

### [x] E17-48: In-session `/compresso` commands for Claude Code
Owner: D1 · Estimate: 1.5d · Depends: E17-28, E17-38
Files: rust/compresso-integrations/src/{lib.rs,session_commands.rs}, rust/compresso-cli/src/{commands.rs,commands/integrations.rs}, rust/compresso-cli/tests/cli_session_commands.rs, rust/compresso-store/src/files.rs, rust/compresso-mcp/src/{lib.rs,server.rs,session.rs,tools/mod.rs,tools/report.rs,tools/engine_tools.rs}, rust/compresso-mcp/tests/{tools_report,handshake,tool_profile,tools_recall}.rs, docs/plan/{00-overview,04-mcp-and-integrations,05-tickets,08-ip-and-secure-deployment}.md, docs/runbooks/mcp-manual-verification.md, README.md, decisions.md
Description: The owner decided on 2026-11-22 to add in-session commands a person can type inside the coding client, installed by `compresso integrations install claude-code`: stats, status, recall, update, bug, feedback, share. D113 (proposed by the implementing session, awaiting owner confirmation) writes one project skill, `.claude/skills/compresso/SKILL.md`, typed as `/compresso <command>` because six of the seven bare names are the client's own commands or aliases; the skill calls `compresso_stats`, `compresso_recall`, and a new full-profile tool, `compresso_report` (04 §1.13). `update` prints the running version and the manual steps; `bug`, `feedback`, and `share` send nothing by themselves. Not run in a live client session.
Acceptance criteria:
- A project-scope `integrations install claude-code` writes the marked skill as a second previewed plan with its own receipt (one confirmation; JSON `data.session_commands`); `--no-session-commands` leaves it out; `--session-commands` installs, uninstalls, verifies, and reports only the skill; a plain project uninstall removes it with the entry; a refusal or failure for the skill is a warning, never a failed install.
- Nothing is written under the home directory: user scope writes no skill and warns `session_commands_not_installed`; a project root that is the home directory is refused; `.claude/settings.local.json` is never touched; symbolic links on the path are refused.
- A file of the same name without the Compresso marker is never overwritten; an edited skill survives uninstall (`key_modified`); an unchanged one is deleted.
- The skill grants only `mcp__compresso__compresso_stats`, `mcp__compresso__compresso_recall`, and `mcp__compresso__compresso_report`, has `disable-model-invocation: true`, no shell tool, and no shell injection; the crate capability check passes unchanged.
- `compresso_report` kinds `status`, `update`, `share`, `bug`, `feedback`: no process, no network, never a repository write. `status` counts indexed files with one query and says staleness is not checked. `bug`/`feedback` add version, platform, options, index state without a path, the last ten operation ids, and the failed calls (tool and code) since the server started, then the note as given, fenced and prefixed, after best-effort cleaning (control characters, recall-redactor secrets, known roots whole at a path boundary, every path start with its quoted or space-joined tail, `../`); the header says paths and secrets are removed on a best-effort basis and to read before posting. Reports are saved under `reports/` beside the database (a real directory, 0700, files 0600, temporary file and rename, at most 100 report-named files, then `report_limit_reached`; distinct errors for an unusable or unwritable directory and a failed write); the result names the file `~/...` or in words, never with the home directory or user name; `sent: false`; report calls write no ledger row.
- The lean profile is unchanged (2,008 o200k tokens); full grows from 4,033 to 4,227; the half-cost test passes. No new dependency, no store migration.
Test cases:
- Written with the code, not seen failing before it: integrations `session_commands::tests::{test_session_commands_install_writes_one_marked_project_skill, test_session_commands_grant_no_shell_and_name_only_compresso_tools, test_session_commands_refuse_user_scope_and_home_root, test_session_commands_never_overwrite_a_user_skill_of_the_same_name, test_session_commands_uninstall_removes_exactly_the_installed_file, test_session_commands_uninstall_keeps_an_edited_file, test_session_commands_reinstall_over_a_marked_file_is_allowed}`; MCP `session::tests::test_recent_failures_keep_the_newest_codes_only`; `tools::report::tests::{test_clean_note_removes_absolute_paths_and_secrets_but_keeps_relative_paths, test_bug_report_lists_ids_and_codes_and_holds_no_absolute_path, test_feedback_report_without_note_says_none_given, test_status_text_names_build_options_index_and_failures, test_grouped_numbers, test_update_steps_download_nothing_and_name_the_installer}`.
- Written after the code, passing on first run: MCP `tools_report::{test_bug_report_is_saved_locally_in_full_without_absolute_paths, test_status_update_and_share_write_no_file, test_report_arguments_are_checked}`; added after review with their code: `tools::report::tests::test_clean_note_removes_a_repository_root_that_holds_a_space` (seen failing with the root replacement removed), `tools_report::test_a_full_reports_directory_saves_no_more_reports` (not seen failing); CLI `cli_session_commands::{test_project_install_adds_the_session_command_skill_by_default, test_no_session_commands_leaves_the_skill_out, test_user_scope_install_writes_no_skill_and_says_why, test_a_user_skill_of_the_same_name_is_never_overwritten, test_plain_uninstall_removes_the_entry_and_the_skill, test_session_commands_flag_manages_only_the_skill, test_dry_run_previews_both_plans_and_writes_nothing}`.
- Seen failing under a mutation of the code they cover: path scrubbing disabled (`clean_note` keeps every word): `test_clean_note_removes_absolute_paths_and_secrets_but_keeps_relative_paths`, `test_bug_report_lists_ids_and_codes_and_holds_no_absolute_path`, `test_bug_report_is_saved_locally_in_full_without_absolute_paths`; home-directory refusal removed: `test_session_commands_refuse_user_scope_and_home_root`, `test_user_scope_install_writes_no_skill_and_says_why`.
- A guard only: the ledger assertion in `test_status_update_and_share_write_no_file` passes with or without the change that skips the ledger (before it the store refused the unknown tool name, which a release-build stdio probe showed as a ledger warning).
- Changed goldens, each seen failing after the tool was added: `handshake::{test_tools_list_exact_ten, test_manual_checklist_all_tools_pass}` (thirteen tools; runbook row added from a raw stdio probe, not a client session), `tool_profile::test_default_profile_tool_list_and_instructions_are_unchanged`, `tools_recall::test_recall_tool_is_absent_and_refused_until_an_operator_enables_a_source`.
- Review fixes (2026-11-22), seen failing before their code: `tools::report::tests::{test_clean_note_removes_paths_in_every_reviewed_form, test_clean_note_consumes_path_words_after_a_space, test_known_roots_are_replaced_at_a_path_boundary_only, test_clean_note_strips_control_characters_but_keeps_lines_and_tabs, test_note_is_fenced_so_it_cannot_pass_for_report_lines, test_saved_location_hides_home_and_user_name, test_status_says_staleness_is_not_checked}` and the changed `test_clean_note_removes_a_repository_root_that_holds_a_space` (now `[path] twice`); store `files::tests::test_indexed_file_count_counts_one_repository_without_loading_rows` (did not compile: no such method); MCP `tools_report::{test_the_cap_is_exactly_one_hundred_matching_reports, test_reports_directory_and_files_are_private, test_a_symlinked_reports_directory_is_refused, test_a_reports_path_that_is_a_file_is_refused, test_an_unwritable_reports_directory_is_its_own_error}` and the changed `test_bug_report_is_saved_locally_in_full_without_absolute_paths`; `server::tests::test_default_instructions_are_unchanged_unless_edits_are_served` (new assertions on the `--edits` sentence); integrations `session_commands::tests::test_skill_states_the_note_limit_and_the_full_profile`. Passing on first run, each then seen failing under a mutation of the code it covers: `tools_report::test_saved_location_in_the_result_holds_no_absolute_path` (written after the location code; its unit test above was seen failing), `test_session_commands_grant_no_shell_and_name_only_compresso_tools` (exact list; a fourth tool added), `test_a_symbolic_link_on_the_skill_path_is_refused` (link check disabled, after the assertion was tightened to the link error), `test_a_skill_path_of_the_wrong_type_or_unreadable_content_is_refused` (file-type check, size check, and UTF-8 check each disabled), `test_crlf_and_bom_files_are_refused_and_an_edited_managed_file_is_replaced` (recognition made to ignore CRLF and a byte-order mark), the cap test (cap 99 and 101 each fail), and the mode test (directory 0755). The skill's recognition of CRLF, byte-order-mark, and edited files already matched the agent file's, so that item needed tests only.
- Final review round (2026-11-22), seen failing before their code: `tools::report::tests::{test_clean_note_final_round_forms, test_a_user_directory_without_a_further_separator_is_removed_to_the_line_end}` (top-level directories after a name character, `Users/<name>` at a word start, quoted tails, percent-encoding, user directories ending at the name); `tools_report::test_an_existing_reports_directory_is_made_private`; the save-failure tests rewritten to expect the report text with `saved: false` and `save_error` (`test_the_cap_is_exactly_one_hundred_matching_reports`, `test_a_symlinked_reports_directory_is_refused`, `test_a_reports_path_that_is_a_file_is_refused`, `test_an_unwritable_reports_directory_is_its_own_error`); `server::tests::test_default_instructions_are_unchanged_unless_edits_are_served` (report description wording). Changed: `test_clean_note_consumes_path_words_after_a_space` uses `/srv/...`, because a user directory ending at the name is now removed through the line end.
- Known gaps: secrets with no recognisable form are not redacted (a limit of the recall redactor); a path split across a line break; harmless over-removal such as `run /compresso stats` and URL tails; the model can call `compresso_report` outside the `/compresso` command; a partial file left by a crash is not cleaned up; two servers saving at once can exceed the cap by a few files; `report_write_failed` after a successful create has no test (no portable way to make a write fail).
- Existing: every other workspace test and golden passes unchanged.

### [x] E17-49: `create` makes missing directories; grep names where its pattern also matches outside the path filter
Owner: R2 · Estimate: 1.5d · Depends: E17-47, E17-22, E17-36
Files: rust/compresso-atlas/src/{edit.rs,grep.rs,scanner.rs,lib.rs}, rust/compresso-atlas/tests/{edit_batch.rs,grep_outside_matches.rs}, rust/compresso-mcp/src/server.rs, rust/compresso-mcp/src/tools/{edit.rs,atlas_grep.rs,text.rs}, rust/compresso-mcp/tests/{tools_edit.rs,tools_grep.rs}, docs/plan/{03-interfaces,04-mcp-and-integrations,05-tickets}.md, docs/adr/0035-opt-in-verified-edit-tool.md, decisions.md
Description: A measured probe (15-task edit suite) showed two remaining frictions: `create` refused a path whose parent directory did not exist (10 refusals over 4 runs; the model fell back to shell `mkdir` or the client's write tool), and in every failing run of one task the model grepped with a path filter limited to one folder, edited the handler it found, and never saw the route outside the folder whose validation step was the real fault. The owner approved both changes on 2026-11-22 (D114, amending D112); the bounds, refusals, and note format were chosen by the implementing session and await owner confirmation. After an independent review the grep part names where the pattern also matches outside the filter (a bounded second text scan) instead of stored call sites, which missed the motivating case. The probe was not re-run.
Acceptance criteria:
- `create` makes missing parent directories, outermost first, with create-new calls; something that appears at a directory meanwhile stops the call (`file_changed`); each created directory is confirmed to be a real directory reached without a link; the report lists them in `directories_created` (existing part as spelled on disk; only when not empty), the text header too.
- Every new directory component passes the file's path rules (hidden, `:`, reserved device names, `.git`, control denylist, secret names, scanner exclusions including gitignored and built-in excluded directories) and, new, no trailing `.` or space and no control character (`unportable_name`); the refusal names the directory and the reason; a name equal after lowercasing to an entry beside it (at most 10,000 entries read) or to another create of the call is refused `case_duplicate`, in dry and real runs alike; any new name (directory or file) written in decomposed Unicode form (a combining mark or conjoining jamo) is refused `unportable_name`, so composed and decomposed spellings get the same answer in dry and real runs and distinct names (`한국`/`영어`, `bạn`/`bản`, `мой`/`моё`, `café`/`cafè`) never clash.
- At most 8 new levels per create (`directory_depth`) and 32 new directories per call (`directories`), both `limit_exceeded` naming the operation and the path.
- A symbolic link anywhere on the path is refused `no_directory` with byte-identical answers whatever it points to, an ignore file behind it included; nothing is created through or beside it, inside or outside the repository.
- All or nothing: a failed call removes the directories it made, deepest first, each only while empty and, on Unix, still the directory made (an open handle held during the call keeps its inode from being reused); pre-existing directories are never removed, and a replacement is kept on Unix (on Windows an empty replacement of the same name can be removed); a dry run creates nothing and reports what it would create; directories get the platform default permissions. Caveat (recorded in D114 and ADR-0035): a parent swapped for a link after the last check is followed by `create_dir`; the post-creation check fails the call and the rollback removes the directory made there while it is empty.
- A grep with a path filter (call `path` or pattern scope) whose pattern matched inside it adds one `outside_matches` note naming the files outside the filter where the same patterns match (`[also matches outside this path: PATH (N), ... (+N files)]`; at most 3, most matches first, then path); none without a filter, without a match inside, or without a match outside; search eligibility (never secret, excluded, unindexed, binary, or non-UTF-8 files); stops after 25 matching files, 2,000 files, or 32 MiB, with `at least` when it stopped early; first dropped under `max_tokens`; never fails the call; both profiles. Limitation: the note is call-level, so with several scoped patterns it sums their matches and does not name the pattern. Cost (release, copy of this repository, median of 5 x 10 calls; main / before / after): many outside hits 27 / 436 / 71 ms, few 19 / 105 / 53 ms, no match inside 22 / 22 / 23 ms, no filter 119 / 121 / 120 ms.
- The grep description, goldens, and the lean surface are unchanged; the edit description changes wording at equal token cost (full with edits 5,004, lean with edits 2,573 o200k); no store migration, no new dependency, no process spawning.
- Acceptance experiment (reviewer's repositories, `grep login path=src/controllers/**`, lean text): c1-c8 name `src/routes/auth.ts`; c9 names `src/session/passport.ts (1)` and never its gitignored `gen/out.ts`.
Test cases:
- Atlas `edit_batch`, written before the code and seen failing against it (by assertion; the type surface was added first without behaviour): `test_create_makes_nested_missing_directories_and_reports_them`, `test_create_dry_run_creates_no_directory_and_reports_what_it_would`, `test_two_creates_share_a_new_parent_in_one_call`, `test_a_refused_directory_component_is_named_with_its_reason`, `test_create_bounds_the_directories_it_adds` (later expects `CreateBound` with the operation and path), `test_a_new_directory_differing_only_by_case_is_refused`. Written before the code and passing against it, pinning existing behaviour: `test_a_linked_ancestor_is_refused_alike_and_creates_nothing` (Unix), later extended with secret-leaf paths and an ignore file behind the link and seen failing before that fix.
- Atlas unit tests written before the code and seen failing: `edit::tests::{test_a_rollback_removes_the_directories_the_call_created, test_a_rollback_keeps_a_created_directory_that_is_no_longer_empty, test_a_directory_appearing_before_creation_stops_the_call}`.
- Review round (written after the code; each confirmed to fail under its mutation): `edit::tests::test_a_failing_batch_creates_no_directory` (moved from `edit_batch`; now fails after the directories exist; rollback disabled), `test_a_rollback_keeps_a_directory_that_replaced_the_one_made` (Unix; rollback ignoring identity), `test_a_parent_swapped_for_a_link_stops_the_call_after_creation` (Unix; post-creation link check removed), `test_a_made_directory_swapped_before_the_write_is_not_written_into` (Unix; re-check ignoring a made directory's identity), `test_a_staging_failure_removes_the_directories_the_call_made` (rollback missing after a staging failure); `edit_batch::test_unportable_new_directory_names_are_refused` (check removed); `edit_batch::test_created_directories_are_reported_as_spelled_on_disk` (case-insensitive file systems only; not mutation-checked).
- Grep, written with the replacement code: atlas `grep_outside_matches::{test_a_filtered_search_names_the_files_outside_where_the_pattern_matches (the probe's module-scope route), test_no_note_without_a_filter_or_without_a_match_inside_or_outside, test_a_pattern_scope_is_a_filter_too, test_at_most_three_files_are_named_by_count_then_path, test_an_excluded_or_secret_file_is_never_counted_or_named, test_the_outside_scan_stays_within_its_file_bound}` and `grep::tests::test_the_outside_scan_stops_at_its_file_and_byte_bounds`; MCP `tools_grep::test_grep_tool_names_files_outside_the_path_filter_that_also_match`, `tools_edit::test_lean_grep_text_names_a_file_outside_the_path_filter`, `tools::text::tests::test_grep_names_files_outside_the_path_filter_in_one_line`. Each confirmed to fail under a mutation: exclusion check removed, scan bound removed, filter ignored. The call-site variant and its tests were removed.
- Second review, written before the code and seen failing against it: `edit_batch::test_distinct_non_ascii_names_are_never_a_clash` (the wildcard key refused `ko/한국.md` with `ko/영어.md`), `edit_batch::test_decomposed_new_names_are_refused_alike_in_dry_and_real_runs` (was `case_duplicate`); the second also fails when the decomposed-name check is disabled. Written after the code: `tools_edit::test_lean_grep_text_says_at_least_when_the_outside_scan_stops_early` (fails when the 25-file stop is removed).
- CI round (Linux, Windows): `edit::tests::test_a_rollback_keeps_a_directory_that_replaced_the_one_made` failed on Linux (ext4 reused the freed inode, so the replacement looked like the directory made); fixed by holding a handle to each made directory, not verifiable on the macOS development machine (APFS does not reuse inodes at once). `edit_batch::test_a_refused_directory_component_is_named_with_its_reason` (`a:b` parsed as a Windows drive prefix when naming the directory) and `edit_batch::test_unportable_new_directory_names_are_refused` (a control character failed the walk's file-system call on Windows) failed on Windows; fixed by reading every name rule from the path text before any file-system call.
- MCP `tools_edit::test_create_reports_the_directories_it_makes_over_mcp`, written after the code, not seen failing.
- Changed: `edit_batch::test_create_keeps_every_path_gate_an_indexed_file_has` drops its `missing/new.rs` refusal case (now created).
- Existing: every other workspace test and golden passes unchanged.

### [x] E17-50: Opt-in, read-only live SQL tool (`compresso_sql`)
Owner: D1 · Estimate: 3d · Depends: E17-32, E17-48
Files: rust/compresso-sql/ (new crate: src/{lib,target,gate,cells,redact,sqlite,network}.rs, tests/{sqlite,network}.rs), rust/compresso-mcp/src/{lib.rs,server.rs,tools/mod.rs,tools/sql.rs,tools/report.rs}, rust/compresso-mcp/tests/{tools_sql.rs,golden/sql_surfaces.txt}, rust/compresso-cli/src/{commands.rs,commands/mcp.rs,commands/integrations.rs}, rust/compresso-cli/tests/cli_sql_env.rs, rust/compresso-integrations/src/{lib.rs,claude_code.rs,server_entry.rs}, scripts/check-crate-capabilities{,.test}.ts, scripts/fixtures/crate-capabilities/{allowed-mcp-network-through-sql,denied-mcp-direct-network-beside-sql,denied-store-network-through-sql,denied-mcp-network-through-impostor-sql}/, Cargo.toml, Cargo.lock, AGENTS.md, decisions.md, docs/adr/0036-opt-in-read-only-live-sql.md, docs/plan/{03-interfaces,04-mcp-and-integrations,05-tickets,08-ip-and-secure-deployment}.md, docs/runbooks/install-local.md, README.md
Description: The owner decided on 2026-11-22 to add a live SQL query tool for Postgres, MySQL/MariaDB, and SQLite, with drivers linked into the binary and four gate amendments (D115). `compresso mcp serve --sql-env NAME` reads one `COMPRESSO_SQL_*` variable at startup and serves `compresso_sql` (`query`, `tables`, `describe`) in the full profile only; every statement passes a statement gate and runs in the database's own read-only session (ADR-0036). It partly lands E15-09 and E15-12 by owner request. Postgres and MySQL/MariaDB are exercised only by the CI live job (Postgres 16.4, MariaDB 11.4.3, MySQL 8.4).
Acceptance criteria:
- Without `--sql-env` the full and lean surfaces are byte-identical to before (existing goldens unchanged); with it the full profile lists `compresso_sql` (definition at most 350 o200k tokens; 301 measured) and states its network use for the configured engine; the lean profile never serves it (`--tools lean --sql-env` refused); an unadvertised call returns `tool_unavailable`.
- SQLite: the file is opened read-only and never changes (including a WAL database with a live writer); writes, ATTACH, pragma writes, stacked statements, and a crafted view calling `load_extension` are refused; a runaway recursive query stops at its deadline; rows, cells, and bytes are bounded; Compresso's store is refused directly, through a symbolic link, and through a hard link.
- Postgres and MySQL/MariaDB: one connection per call, privileged roles refused, read-only transaction with server deadlines, verified TLS for any non-loopback host, prepared to refuse several statements; no part of the URL appears in any error, including a server error that names the user.
- `integrations install claude-code --sql-env NAME` writes only the name, verifies, survives `setup`, and uninstalls; invalid names, other clients, uninstall, and the exploration agent are refused.
- The capability check judges each crate's own resolved tree, lets `compresso-mcp` and `compresso-cli` reach network packages only through `compresso-sql`, and its negative controls fail. `cargo deny check licenses bans sources` passes with no `deny.toml` change.
- No ledger row for SQL calls; `compresso_report` shows `sql <engine>` only.
Test cases:
- Seen failing before their code: capability `test_mcp_and_cli_network_only_through_sql_passes`, `test_mcp_direct_network_beside_sql_still_fails`, `test_network_through_non_member_sql_crate_fails_closed` (against the previous script; the real workspace also failed it with `compresso-mcp -> tokio -> socket2`); `gate::tests::{test_postgres_unicode_escaped_names_and_strings_are_refused, test_postgres_functions_that_run_query_text_are_refused}` (security review); `network::tests::{test_sql_mode_drops_the_modes_the_gate_does_not_lex, test_granted_roles_are_found_in_grant_lines}` (did not compile: no such functions); `tests/sqlite.rs` `test_sqlite_stacked_statements_are_refused` (`SELECT 1;/* x */;` was accepted); MCP `tools_sql::test_unconfigured_and_network_servers_state_their_network_use_and_leak_nothing` (the server panicked dropping the SQL runtime on an async task).
- Seen failing under a mutation: `sqlite::tests::test_database_level_controls_hold_without_the_gate` with the authorizer and the attach limit removed (`ATTACH ':memory:'` then succeeded; SQLite reports ATTACH as read-only).
- Written with the code, passing on first run: the rest of `compresso-sql` (`gate`, `target`, `redact`, `cells`, `network` unit tests; `tests/sqlite.rs`; `tests/network.rs` with a fake Postgres server echoing the user name and refused connections); MCP `tools_sql` (four servers, the `sql_surfaces.txt` golden over every profile, `--edits`, and SQL surface, the 350-token budget); CLI `mcp::tests::test_sql_tool_is_off_by_default_and_needs_a_valid_variable_name`, `cli_sql_env`; integrations `server_entry::tests::test_a_sql_env_entry_is_exact_only_in_canonical_order_with_a_valid_name`; capability `test_network_through_sql_fails_for_every_other_crate` (a negative control, passing before and after).
- Live: the first version of these tests was not run (no server on the build machine); `tests/live.rs` replaced them and passes in CI (see below).
- Review fix round (2026-11-22), each seen failing before its fix unless noted: `tests/gate_review.rs` (the reviewer's probes: Unicode whitespace after MySQL `--`, non-ASCII dollar-quote tags, optimizer hints, prefixed and escaped strings, non-ASCII and control characters, words glued to numbers); `target::tests` duplicate keys, parameter allow-list, explicit user/host/database, PG* variables, policy read from the driver options (written with the new parser; the review's probe log shows the old behaviour); `network::tests` grant classification including roles, PROCESS, CONNECTION_ADMIN, PROXY, and MariaDB syntax (new functions); `tests/sqlite.rs` huge values within the deadline, WAL read-only directory code, column names and counts; `sqlite::tests::test_the_connection_is_opened_read_only` (seen failing under the read-write-open mutation); `tools_sql` column bounds under `max_tokens`; `tests/network.rs` stalled peer bounded; capability `test_network_through_a_pure_local_crate_enabled_by_sql_fails` (the reviewer's fixture). `tests/live.rs` replaces the earlier live tests and runs only in the `live-sql` CI job; it passed on 0245a34 against Postgres 16.4, MariaDB 11.4.3, and MySQL 8.4.
- Confirmation review round (2026-11-23), each seen failing first unless noted: `sqlite_memory` fan-out bounded in memory and time (1.44 s at a 1 s deadline before); `gate_review` quoted EXPLAIN options, INTO, SHARE, MySQL SHOW ... WHERE, refusal wording, SQLite backslashes; `network::tests` quoted role names (classified Ordinary before), Postgres role decision and server-side filter (new functions); `environment` (from_env PG* detection, ambient PGOPTIONS, SSL_CERT_FILE/DIR; PGOPTIONS failed before); `sqlite` C1/bidi escaping, hard link into the data directory, fail-closed walk; `cells` escaping by class (written with the code); `target` IPv6 literal; live tests for each cut rule alone, the inverted role rules, and `COMPRESSO_SQL_TEST_REQUIRE_LIVE`.
- Known gaps: ADR-0036 "What a model can still do" (functions with external effects behind a wrapper or `SECURITY DEFINER`, `dblink`/FDW, own-session termination, MySQL UDFs and nested roles, deadlines only on `SLEEP`-style waits); no bounded `EXPLAIN` on timeout; no cancel request.

### [x] E17-52: Opt-in, read-only MongoDB tool (`compresso_mongo`)
Owner: D1 · Estimate: 3d · Depends: E17-50
Files: clippy.toml, rust/compresso-agent/clippy.toml, rust/compresso-updater/clippy.toml, rust/compresso-sql/{Cargo.toml,src/lib.rs,src/target.rs,src/mongo/{mod,gate,account,target,client,output,raw}.rs,tests/{mongo,mongo_environment,live_mongo,live}.rs}, rust/compresso-mcp/src/{lib.rs,server.rs,tools/mod.rs,tools/mongo.rs,tools/report.rs}, rust/compresso-mcp/tests/{tools_mongo.rs,golden/mongo_surfaces.txt}, rust/compresso-cli/src/{commands.rs,commands/mcp.rs,commands/integrations.rs}, rust/compresso-cli/tests/cli_mongo_env.rs, rust/compresso-integrations/src/{lib.rs,claude_code.rs,server_entry.rs}, scripts/check-crate-capabilities{,.test}.ts, scripts/fixtures/crate-capabilities/{allowed-sql-driver-tokio-process,denied-mcp-tokio-process-beside-driver,denied-mcp-tokio-full-beside-driver,denied-sql-driver-encryption-process,denied-store-process-through-driver}/, .github/workflows/ci.yml, Cargo.lock, deny.toml, AGENTS.md, README.md, decisions.md, docs/adr/0038-opt-in-read-only-mongodb.md, docs/plan/{01-stack-and-workspace,03-interfaces,04-mcp-and-integrations,05-tickets,08-ip-and-secure-deployment}.md, docs/runbooks/install-local.md
Description: The owner decided on 2026-11-23 to add MongoDB as an immediate follow-up to E17-50, as its own tool, and to allow the licences CDLA-Permissive-2.0 and CC0-1.0 so the official driver can be linked (D117). `compresso mcp serve --mongo-env NAME` reads one `COMPRESSO_MONGO_*` variable at startup and serves `compresso_mongo` (`find`, `aggregate`, `count`, `distinct`, `collections`, `describe`) in the full profile only. MongoDB has no read-only session, so the argument allow-list (checked before any connection) and the account check (every connection) are the control (ADR-0038).
Acceptance criteria:
- Without `--mongo-env` the full and lean surfaces are byte-identical to before (every existing golden unchanged; `advertised_surface_for` is the `MongoSurface::Off` case); with it the full profile lists `compresso_mongo` (definition at most 600 o200k tokens; 591 measured) and states its network use; the lean profile never serves it (`--tools lean --mongo-env` refused); an unadvertised call returns `tool_unavailable`.
- Every refused stage and operator (`$out`, `$merge`, `$where`, `$function`, `$accumulator`, admin and change-stream stages, `$unionWith`, `$graphLookup`, cross-database `$lookup`, unlisted keys, extended-JSON code) is refused at every depth before any connection is opened.
- An account beyond `read` on the configured database, or a server without access control, is refused on every call; errors never contain a URL part.
- `mongodb+srv://`, several hosts, unlisted or duplicated URL options, `tls=false` to a remote host, and function-host variables make the target `mongo_not_configured`.
- Every command carries `maxTimeMS` and a row limit; a call is bounded end to end and does not panic inside a Tokio runtime.
- `integrations install claude-code --mongo-env NAME` writes only the name in canonical order (after `--sql-env`), verifies, survives `setup`, and uninstalls; invalid names, other clients, uninstall, and the exploration agent are refused.
- The capability check passes with the driver and its four new negative controls fail; `cargo deny check licenses bans sources` passes with per-crate licence exceptions only.
- No ledger row; `compresso_report` shows `mongo on` or `mongo not configured` only.
Test cases:
- Seen failing before their code: the capability check on the real workspace (`compresso-sql`, `compresso-mcp`, and `compresso-cli` reported `-> mongodb -> tokio grants process capability` with the driver added); `cargo deny check licenses` (webpki-roots CDLA-Permissive-2.0, tiny-keccak CC0-1.0 rejected).
- Written with the code, passing on first run: `mongo::gate`, `mongo::account`, `mongo::target`, `mongo::output`, `mongo::client` unit tests (each refusal test asserts the refusal code, so removing the refusal fails it); `tests/mongo.rs` (a counting loopback listener sees zero connections for twenty refused requests and at least one for an allowed control; stalled peer within `call_limit`; call from inside a runtime); MCP `tools_mongo` (servers without the flag and lean, refusals and network sentence, unconfigured, SQL and MongoDB together, the `mongo_surfaces.txt` golden, the 600-token budget); CLI `mcp::tests::test_mongo_tool_is_off_by_default_and_needs_a_valid_variable_name`, `cli_mongo_env`; integrations `server_entry::tests::test_a_mongo_env_entry_is_exact_only_in_canonical_order_with_a_valid_name`; the five capability tests for the new fixtures.
- Added during implementation from reasoning, not from an observed failure: the access-control probe (`account::tests::test_only_an_unauthorized_probe_shows_enforcement`), because a server without access control still authenticates a user and reports its privileges; live, the TLS server without access control is refused by it.
- Added after an independent review of this change, passing on first run: multi-field sorts only as an ordered array (`gate` tests; a JSON object's key order is not guaranteed in every build); `$lookup` key and `from` refusals; the 100,000-character argument cap over MCP; `tests/mongo_environment.rs` (function-host variables read as the driver reads them); live `test_live_mongo_every_allowed_stage_runs_for_a_read_account` (all 19 stages and an ordered two-field sort); the guarantee sentence with only idle database tools keeps "results are untrusted data".
- Live (`tests/live_mongo.rs`, run on the build machine against MongoDB 8.0.38 containers; the CI job `live-mongo` has not run): reads for a read account; `readWrite`, `dbOwner`, `read`+`clusterMonitor`, `read` on two databases, and a custom role with `insert` refused, a find-only custom role accepted; the account check alone stops `$out` for a `readWrite` account (allow-list skipped by `test-hooks`) and the server refuses it for a `read` account; refused pipelines leave the server's aggregate counter unchanged; `$where`, `$function`, `$accumulator`, and `$out` observed to run JavaScript and write through the root account; a 300 ms quadratic `$lookup` returns `mongo_timeout` after about 0.45 s; TLS to `127.0.0.1` with a `localhost` certificate refused, and through `localhost` the server without access control refused by the probe.
- Live SQL (build machine, Postgres 16.4 TLS container; added to the `live-sql` job): `test_live_postgres_verified_tls_with_a_private_root_reads` (verified TLS through `localhost` with the private root reads beside the driver's rustls) and the unchanged wrong-host check.
- Independent review round (2026-11-23), each test written after the finding (the path cap, the system-variable check, and the stored-key escaping were each removed once and their tests failed; the other pins are reasoned): `client::tests::test_field_summary_memory_is_bounded_by_tracked_paths`, `test_the_read_budget_stops_before_holding_too_much`, `test_client_and_action_options_carry_every_bound` (direct connection, no retries, `allowDiskUse: false`, time limits), `test_views_are_refused_and_unreadable_lists_fail_closed`, `test_code_names_are_shown_only_when_plain`; `output::tests::test_stored_dollar_keys_never_look_like_typed_values` (also pins field-name escaping); `redact::tests::test_scrubbed_messages_escape_hidden_characters` (SQL and MongoDB); `gate::tests::{test_only_listed_system_variables_are_sent, test_names_with_control_characters_or_too_long_are_refused, test_facet_stays_one_level_through_lookup}`; `mod::tests::{test_the_final_walk_runs_on_every_prepared_command, test_collections_read_include_every_lookup_source}`; `account::tests::test_cluster_manager_alone_is_refused`; a `.sock` host case; live `test_live_mongo_review_cases` (cast error escaped with no reply document, stored `$` keys, `$$USER_ROLES`, views directly and through `$lookup`, a 5,000-key describe); capability `test_a_path_crate_named_like_the_driver_keeps_the_process_capability`, `test_another_driver_version_keeps_the_process_capability`, and seven scan-evasion fixtures (`denied-sql-{glob,macro}-{tokio,std}-process`, `denied-sql-include-outside-src`, `denied-sql-process-reexported-by-driver`, `denied-sql-std-process-reexported-by-dependency`), each passing the previous script.
- Confirmation round (2026-11-23, second review), each test written after the finding; a mutation run removed the read budget, the sample budget, the final walk in `prepare`, and the view check, and raised the sample-byte, tracked-path, and field-length bounds, and each mutant failed a test: `raw::tests::{test_types_render_as_markers_and_stored_dollar_keys_are_escaped, test_stored_wrappers_never_look_typed_at_any_depth, test_invalid_utf8_keys_and_strings_render_lossily, test_a_huge_document_is_a_size_marker_and_a_large_one_is_cut, test_a_long_string_is_cut_without_decoding_it_whole, test_batch_documents_are_read_in_place}`; `client::tests::{test_the_document_reader_stops_at_the_read_budget, test_the_sample_reader_stops_at_its_byte_budget_and_size, test_a_large_document_is_cut_into_the_result_not_dropped, test_distinct_values_render_raw_and_stored_wrappers_stay_escaped, test_preflight_refuses_views_and_servers_without_access_control}`; `mod::tests::test_prepare_always_ends_with_the_final_walk`; gate data-versus-expression variable cases and literal bounds; a scrub-before-cut case in `target::tests`; live `test_live_mongo_review_cases` extended (distinct of stored wrappers, an invalid UTF-8 string, a 15 MB document, a time-series collection); capability: 15 new fixtures (character literals, nested and `::` groups, aliases, a macro pair, `#[path]`, an out-of-tree binary, a build script, a missing `clippy.toml` or entry, lint-lowering attributes and manifests) and one passing fixture (local `process` modules, lifetimes, `ExitCode`). Peak RSS per call measured with the second review's probes in a release build (not a committed test): 26 MB, 26 MB, 22 MB for the three memory attacks (were 825, 795, 610 MB).
- Second confirmation round (2026-11-23), tests written after each finding: `gate::tests::{test_find_projections_accept_only_inclusion_slice_and_elem_match, test_the_final_walk_checks_variables_by_position}`; `raw::tests::{test_escaping_is_injective, test_dates_outside_rfc_3339_show_milliseconds, test_the_budget_counts_escaped_output}`; live `test_live_mongo_data_positions_return_variables_as_literal_strings` (each data row of ADR-0038's position table) and `test_live_mongo_distinct_matches_the_server_where_documented` (the server's own `distinct` as oracle, through the root account); capability: 13 new fixtures (FFI `system` and `posix_spawnp`, spaced and macro-built `allow`, inline and dotted `[lints]`, cargo `[env] CLIPPY_CONF_DIR` and `rustflags`, raw identifiers, a crate-level `clippy.toml`, `unsafe` not forbidden, `allow(unsafe_code)` off the pinned site, a `libc` dependency) and the commented-out entry case; every fixture now carries the workspace `clippy.toml`.
- Known gaps: ADR-0038 "What a model can still do" and "Known gaps" (views run their stored pipelines; no `killOp`; `listCollections`/`listIndexes` bounded only by the client deadline; MongoDB 8.0.38 only; no SRV).

## Execution order (flat, dependency-ordered)

Slice: E1-01, E1-02, E1-03, E1-04, E1-05, E1-06, E2-01, E2-02, E2-02a, E3-01, E2-03, E3-02, E3-03, E2-04, E2-05, E2-06, E2-07, E2-08, E2-12, E2-13, E2-14, E3-04, E3-05, E3-06, E3-07, E3-08, E2-09, E7-01, E7-07, E2-10, E2-11, E3-09, E7-02, E3-10, E3-11, E3-12, E4-01, E4-02, E4-03, E4-04, E4-05, E5-01, E5-02, E5-03, E5-04, E5-05, E5-06, E2-15, E7-03, E7-04, E7-05, E7-06, E5-07, E6-01, E6-02, E6-03, E6-04, E6-05.

Authorized public beta lane: PB-01, PB-02, PB-03, PB-04, PB-05, PB-06, PB-07, PB-08. PB-04 also waits for E7-04 evidence. This lane consumes only the ADR-0025 beta subsets of E12/E14 and does not open the post-slice waves.

Post-slice, wave 1 — local platform: E8-01, E8-02, E8-03, E8-08, E8-04, E8-05, E8-06, E8-07, E14-01, E14-02, E14-03, E14-04.
Wave 2 — desktop: E9-01, E14-05, E9-02, E9-03, E9-04, E9-05, E9-06, E9-07, E9-08.
Wave 3 — cloud + local entitlements: E10-01, E10-02, E10-09, E10-03, E10-04, E10-05, E10-06, E10-07, E10-15, E10-08, E10-10, E10-11, E10-12, E10-13, E10-14.
Wave 4 — provider proxy: E11-01, E11-02, E11-03, E11-04, E11-05, E11-06, E11-07.
Wave 5 — model expansion: E13-01..E13-11, E13-13, E13-14, E13-16, E13-12, E13-15, E13-17..E13-22; then E13-23..E13-31 subject to dependencies.
Wave 6 — release engineering and hardening close-out: E12-01, E13-32, E12-02, E12-03, E12-04, E12-05, E12-06, E14-08, E14-09, E14-10, E14-11, E12-07, E12-08, E14-15, E14-16, E14-12, E14-14, E12-09.
Wave 7 — E15 A-F plus threat-model and sandbox foundations: E15-01, E14-17, E14-18, E15-02, E15-03, E15-05, E15-09, E15-11, E15-14, E15-19, E15-06..E15-17, E15-18, E15-18b, E15-18c, E15-20..E15-30 subject to dependencies.
Wave 8 — full-product completion: E16-01..E16-16 subject to listed dependencies.
Wave 9 — deferred sandbox actions: E14-19, E14-20, E15-04, E15-31, E15-32.
E17 (delivered after the slice, independent of waves 1–9): E17-05, E17-03, E17-04, E17-07, E17-02, E17-09, E17-01, E17-06, E17-10, E17-08; follow-ups E17-11, E17-12, E17-13, E17-14, E17-15; E17-16 delivered 2026-11-15; E17-17, E17-18, E17-19, E17-20, E17-21 delivered 2026-11-16; E17-22, E17-23, E17-24, E17-25, E17-26, E17-27, E17-28, E17-29 delivered 2026-11-18; E17-30, E17-31, E17-32, E17-33 delivered 2026-11-19; E17-34 implemented 2026-11-19 (D99 confirmed by the owner the same day); E17-35 implemented 2026-11-19 (D100 confirmed by the owner on 2026-11-20); E17-36 implemented 2026-11-20 (D101 confirmed by the owner on 2026-11-20); E17-37 implemented 2026-11-20 (D102 confirmed by the owner on 2026-11-20); E17-38 implemented 2026-11-20 (D103 confirmed by the owner on 2026-11-20); E17-39 implemented 2026-11-20 (D104 confirmed by the owner on 2026-11-20); E17-40 implemented 2026-11-20 (D105 confirmed by the owner on 2026-11-21); E17-41 implemented 2026-11-21 (D106 confirmed by the owner on 2026-11-21); E17-42 implemented 2026-11-21 (D107 confirmed by the owner on 2026-11-21); E17-43, E17-44, E17-45 implemented 2026-11-21 (D108, D109, D110 confirmed); E17-46 implemented 2026-11-21 (D111 confirmed); E17-47 implemented 2026-11-22 (D112 proposed, awaiting owner confirmation); E17-48 implemented 2026-11-22 (D113 proposed, awaiting owner confirmation); E17-49 implemented 2026-11-22 (D114 approved; session choices awaiting owner confirmation); E17-50 implemented 2026-11-22 (D115, design awaiting owner confirmation); E17-52 implemented 2026-11-23 (D117, owner decided the tool and licences; design awaiting owner confirmation).

Parallelism notes:
- Slice: E2 (R1) and E3 (R2) run concurrently after E1; E7-01/E7-02 (PQ/R1) overlap E2's tail; E4 (D1) starts once E2-09 and E3-12 land; E5 (R2) follows E3; E6 (D1) follows E5-01 and overlaps E5-02..07; E1-06 and E4-05 (I1) slot in any time after E1-02/E4-04. E2-02a is a maintainer task (needs an Anthropic API key) and can land any time after E2-02 without blocking anything — until it lands, the interim 1.15 factor applies.
- Public beta: PB-02 closes repository isolation, provider disclosure, and secret-exclusion blockers before PB-03 installs repository-bound client configurations. PB-04 signs the resulting engine; PB-05 assembles the stateless launcher/package; PB-06 verifies public import and prepares the protected publication pipeline; PB-07 aligns disclosure; PB-08 alone performs the registry write and clean-host publication and cannot change stable/latest.
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
| PB Public package beta | 8 | 16.5 | ~3.5 wk | bounded E12/E14 subsets; no stable release |
| E8 Supervisor & local API | 8 | 20 | ~4 wk | |
| E9 Desktop (Tauri) | 8 | 25 | ~5 wk | |
| E10 Cloud + local entitlements | 15 | 43 | ~8.5 wk (F1, with R1/R2 local tickets in parallel) | |
| E11 Provider proxy | 7 | 19 | ~4 wk | |
| E12 Release engineering | 9 | 28 | ~5.5 wk | absorbs E14-06/07/13 |
| E13 Model & client expansion | 32 | 91 | ~18 engineer-wk, phased | +D52 future phases |
| E14 IP & secure deployment | 17 | 39 | ~8 wk | +E15 threat/sandbox gates |
| E15 Clean-room advanced capabilities | 34 | 121.5 | ~24.5 engineer-wk | A-F incremental; G deferred |
| E16 Full-product completion | 16 | 59 | ~12 engineer-wk, dependency-phased | |
| E17 Session continuity & hardening | 51 | 92.5 | ~17.5 engineer-wk | E17-01..10 delivered 2026-11-14, E17-16 2026-11-15, E17-17, E17-18, E17-19, E17-20, and E17-21 2026-11-16, E17-22..29 2026-11-18, E17-30..33 2026-11-19, E17-34 implemented 2026-11-19 (D99 confirmed), E17-35 implemented 2026-11-19 (D100 confirmed), E17-36 implemented 2026-11-20 (D101 confirmed), E17-37 implemented 2026-11-20 (D102 confirmed), E17-38 implemented 2026-11-20 (D103 confirmed), E17-39 implemented 2026-11-20 (D104 confirmed), E17-40 implemented 2026-11-20 (D105 confirmed), E17-41 implemented 2026-11-21 (D106 confirmed), E17-42 implemented 2026-11-21 (D107 confirmed), E17-43, E17-44, and E17-45 implemented 2026-11-21 (D108, D109, D110 confirmed), E17-46 implemented 2026-11-21 (D111 confirmed), E17-47 implemented 2026-11-22 (D112 proposed), E17-48 implemented 2026-11-22 (D113 proposed), E17-49 implemented 2026-11-22 (D114, session choices awaiting confirmation), E17-50 implemented 2026-11-22 (D115, design awaiting confirmation), E17-52 implemented 2026-11-23 (D117, design awaiting confirmation); E17-11..15 open |
| **Post-slice + PB total** | **203** | **550 gross** | heavily parallel across roles | Includes the separately authorized PB lane |
| **Grand total** | **261** | **646 gross** | **~129.2 engineer-wk** | |
