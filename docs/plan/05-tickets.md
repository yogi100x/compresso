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
- test_trace_requires_fallback_used: trace document missing `fallback_used` fails against the compression-trace v1 root schema (which `$ref`s `$defs/trace`).

### [ ] E1-05: Fixtures skeleton and conventions doc
Owner: PQ · Estimate: 1.5d · Depends: E1-01
Files: fixtures/{code,debugging,logs,conversations,provider-payloads,adversarial}/README.md, fixtures/logs/repeated-timestamps/, fixtures/code/rust-small/, docs/plan/06-fixtures.md (skeleton referencing final format), docs/architecture/conventions.md
Description: Create the six-category fixture directory tree from `06-fixtures.md` §3 (code, debugging, logs, conversations, provider-payloads, adversarial) with at least one seed fixture per category (a 500-line repeated log, a small Rust crate, a failing-test debugging log, a tool-call conversation JSON, an Anthropic count-calibration payload stub, a prompt-injection adversarial repo), each carrying a `fixture.toml` in the authoritative `06-fixtures.md` §2 format: exactly one input form (`graph` | `[[input.files]]`/`[[input.nodes]]` | `repo`), an `[expectations]` table, optional `[[recall_probes]]`, and the top-level `eligible` flag. Write `conventions.md` covering error conventions (thiserror per crate, anyhow in binaries only per D20), commit format, module layout, and test naming (`test_snake_case`).
Acceptance criteria:
- All six fixture categories exist with at least one committed fixture and a `fixture.toml` declaring an input form, an `[expectations]` table, and the top-level `eligible` flag per `06-fixtures.md` §2.
- `fixtures/logs/repeated-timestamps/input.txt` contains exactly 500 log lines identical after timestamp normalization (used verbatim by E2-05 and E7).
- conventions.md documents error, naming, commit, and schema-versioning conventions and is linked from the repo README.
Test cases:
- test_fixture_tomls_parse: walk fixtures/, parse every fixture.toml with the `toml` crate, assert `name`, `category`, `description`, `eligible`, `mode`, `[budget]`, one input form, and `[expectations]` present.
- test_repeated_log_fixture_shape: read repeated-timestamps/input.txt, assert 500 lines and that stripping the leading ISO-8601 timestamp yields a single distinct line.

### [ ] E1-06: Slice release tarball CI job (D42)
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

### [ ] E2-01: ContextGraph types, validation, serde round-trip
Owner: R1 · Estimate: 2d · Depends: E1-01, E1-04
Files: rust/compresso-core/src/graph.rs, tests in same file + rust/compresso-core/tests/graph_schema_roundtrip.rs
Description: Implement `ContextGraph`, `ContextNode`, `NodeKind`, `Source`, `ContextEdge`, `EdgeKind`, `GraphError` exactly per `03-interfaces.md` §2.1, with serde renames matching `schemas/context-graph/v1.json` (snake_case kinds, tagged `source`). Implement `ContextGraph::validate()` enforcing the code-level invariants from `02-schemas.md` §1 (unique ids, edge endpoints exist, `protected: true` required for `SystemInstruction`/`UserInstruction`/`ToolDefinition`/`ToolRequest`; arbitrary metadata can never downgrade protection), plus `force_ingress_protection(&mut self) -> Vec<String>` implementing the D48 ingress rule: on any externally supplied graph (MCP, proxy, fixtures loaded as untrusted input) `protected` is forced to `true` for those four kinds and a `protection_forced:<node_id>` note is returned for inclusion in the trace — unprotecting is possible only from trusted local surfaces (CLI `--allow-unprotect`, local config), never from an inbound payload. Also `total_tokens()` and `from_text(content, kind, counter)`.
Acceptance criteria:
- A graph containing one node of every `NodeKind` and one edge of every `EdgeKind` serializes to JSON that validates against `schemas/context-graph/v1.json` and deserializes back to an equal value.
- `validate()` returns typed `GraphError` variants for duplicate ids, dangling edges, and protection violations; never panics on adversarial input.
- `token_provider` defaults to `openai_o200k` on deserialization when absent.
- `force_ingress_protection` flips `protected` to true for the four instruction/tool kinds, returns one `protection_forced:<id>` note per flipped node, is a no-op (empty notes) on already-compliant graphs, and cannot be suppressed by any metadata key (D48).
Test cases:
- test_roundtrip_all_kinds_validates_against_schema: build the every-kind graph, serialize, validate with jsonschema against v1.json, deserialize, assert equality.
- test_validate_rejects_duplicate_ids: two nodes with id "n1" → `Err(GraphError::DuplicateId("n1"))`.
- test_validate_rejects_dangling_edge: edge from "n1" to "missing" → `Err(GraphError::UnknownEdgeEndpoint { .. })`.
- test_system_instruction_must_be_protected: node kind `SystemInstruction` with `protected: false` and no trusted-local unprotect → protection-invariant error.
- test_ingress_forces_protection: external graph with node `sys-1` (`system_instruction`, `protected: false`) → `force_ingress_protection` sets `protected: true`, returns `["protection_forced:sys-1"]`, and `validate()` then passes; adding metadata `"allow_unprotect": true` to the node does not suppress the forcing (D48).
- test_from_text_counts_tokens: `ContextGraph::from_text("fn main() {}", NodeKind::SourceFile, counter)` yields one node with `token_count == counter.count("fn main() {}")` and `total_tokens()` equal to it.

### [ ] E2-02: TokenCounter providers
Owner: R1 · Estimate: 1.5d · Depends: E1-01, E1-05
Files: rust/compresso-core/src/tokens.rs, tests in same file
Description: Implement `TokenProvider`, the `TokenCounter` trait, and `counter_for(provider) -> &'static dyn TokenCounter` per `03-interfaces.md` §2.2: tiktoken-rs `o200k_base` and `cl100k_base` counters, `AnthropicCalibrated` (o200k count × `CALIBRATION`; until E2-02a commits measured counts into `fixtures/provider-payloads/anthropic-counts.json`, `CALIBRATION` is the documented interim default `1.15` per ADR-0017 and `03-interfaces.md` §2.2), and a `Heuristic` fallback (bytes/4) for use when tokenizer init fails. Singletons are lazily initialized once.
Acceptance criteria:
- `counter_for(TokenProvider::OpenAiO200k).count(text)` matches tiktoken-rs `o200k_base` exactly; same for cl100k.
- `AnthropicCalibrated` returns `ceil(o200k_count × CALIBRATION)` and its `provider()` tag is `AnthropicCalibrated`; CALIBRATION is a named const with a doc comment citing the fixture file and ADR-0017 — interim value `1.15` while the fixture's `anthropic_tokens`/`calibration_factor` are null, replaced by the measured median once E2-02a lands.
- `counter_for` returns the same static reference on repeated calls (no re-init); counting a 1 MB string completes in < 50 ms in release mode.
Test cases:
- test_o200k_hello_world: `count("hello world") == 2` for OpenAiO200k.
- test_empty_string_zero: all four providers return 0 for "".
- test_calibrated_is_scaled_o200k: for the text of `fixtures/provider-payloads/anthropic-counts.json` samples, calibrated count equals `ceil(o200k × CALIBRATION)`.
- test_calibration_accuracy_gated_on_fixture: while the fixture's `anthropic_tokens` values are null, the ±10% accuracy assertion is skipped with a `calibration pending (E2-02a)` warning; once they are non-null, calibrated counts must be within 10% of the recorded Anthropic count for every sample.
- test_counter_for_singleton: `std::ptr::eq(counter_for(OpenAiO200k), counter_for(OpenAiO200k))` is true.
- test_provider_tag_roundtrip: each counter's `provider()` serializes to the schema enum string (`"openai_o200k"` etc.).

### [ ] E2-02a: Anthropic calibration measurement
Owner: PQ+R1 · Estimate: 1d · Depends: E2-02, E1-05
Files: fixtures/provider-payloads/anthropic-counts/anthropic-counts.json, rust/compresso-eval/src/calibrate.rs, docs/adr/0017-anthropic-calibration.md
Description: Implement `compresso eval calibrate-anthropic --model <model>` running the documented procedure from `06-fixtures.md` §7.6: for each sample, call Anthropic `POST /v1/messages/count_tokens` with the sample text as a single user message, subtract the once-measured empty-message envelope overhead, fill `anthropic_tokens`/`anthropic_model`/`counted_at`, compute `ratio_i = anthropic_tokens_i / o200k_tokens_i`, and set `calibration_factor` to the median ratio rounded to three decimals. Run offline by a maintainer with an API key — never in CI. Commit the measured counts, update the `CALIBRATION` const from the fixture (provenance comment naming fixture, model, date), and thereby activate E2-02's ±10% accuracy assertion. Counts produced with the factor stay tagged `anthropic_calibrated` and never feed `provider_verified_cost_saved` (02-schemas §3).
Acceptance criteria:
- Committed `anthropic-counts.json` has non-null `anthropic_tokens` for all ten samples plus `anthropic_model`, `counted_at`, and a three-decimal `calibration_factor` equal to the median ratio.
- `CALIBRATION` equals the fixture's `calibration_factor` (test reads the fixture and compares), replacing the 1.15 interim; ADR-0017 updated with model and date.
- E2-02's `test_calibration_accuracy_gated_on_fixture` now executes its ±10% assertion (no longer skipped) and passes.
Test cases:
- test_calibration_factor_is_median_ratio: recompute the median of `anthropic_tokens_i / o200k_tokens_i` from the committed fixture → equals `calibration_factor` to three decimals.
- test_calibration_const_matches_fixture: `CALIBRATION == fixture.calibration_factor`.
- test_calibrate_requires_api_key: `compresso eval calibrate-anthropic` without `ANTHROPIC_API_KEY` exits 1 with `error.code == "missing_api_key"` and writes nothing.
- test_calibrate_never_in_ci: repo lint asserts no CI workflow invokes `calibrate-anthropic`.

### [ ] E2-03: CompressionStrategy trait and StrategyRegistry
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

### [ ] E2-04: Exact-duplicate removal strategy
Owner: R1 · Estimate: 1d · Depends: E2-03
Files: rust/compresso-core/src/strategies/dedup_exact.rs, tests in same file
Description: Implement `dedup_exact` per `03-interfaces.md` §2.3: identical unprotected node contents keep the first occurrence and replace later occurrences with a short reference stub (`[duplicate of node <id>]`), preserving node ids, kinds, and edges. `supports()` is true only when at least two unprotected nodes share byte-identical content; `estimate()` returns saved tokens for all-but-first duplicates. Per D39 this is the only strategy permitted at `Lossiness::None`; every replaced duplicate appends a machine-readable `removed node:<id>` note (naming the surviving node) to `StrategyOutput.notes` so the `removals_enumerated` safety check can account for the token loss.
Acceptance criteria:
- First occurrence of duplicated content is byte-unchanged; replacements reference the kept node's id; edges untouched.
- Protected nodes are never candidates, even when byte-identical to unprotected ones.
- Output graph passes `validate()`; `token_count` on stub nodes is recomputed with the graph's counter.
Test cases:
- test_dedup_three_identical_tool_responses: three unprotected `ToolResponse` nodes with content `"SELECT * FROM users LIMIT 50;\n...200 identical rows..."`; after execute, node 1 byte-equal, nodes 2–3 contain `"[duplicate of node"` and node 1's id, `total_tokens()` strictly decreases; notes contain `removed node:` entries for nodes 2–3 naming node 1 as survivor (D39).
- test_protected_duplicate_untouched: a protected `SystemInstruction` and an unprotected `ToolResponse` with identical content → both byte-unchanged, `supports()` false when they are the only pair.
- test_no_duplicates_not_supported: graph of three distinct contents → `supports() == false`; `execute` (if forced) returns the input unchanged with an explanatory note.
- test_estimate_matches_execute: `estimate().tokens_saved` equals `before_tokens - after_tokens` measured after execute on the three-duplicate graph, within the stub-token overhead.

### [ ] E2-05: Log-collapse strategy
Owner: R1 · Estimate: 1.5d · Depends: E2-03, E1-05
Files: rust/compresso-core/src/strategies/collapse_logs.rs, tests in same file
Description: Implement `collapse_logs` per `03-interfaces.md` §2.3: within unprotected nodes of kinds `ToolResponse`, `CompilerOutput`, `TestOutput`, runs of ≥3 consecutive lines that are identical after normalizing timestamps, hex strings (≥8 chars), and UUIDs are collapsed to `<first line> [×N repeats]`. Non-matching runs and protected nodes pass through byte-identical. Each collapsed run appends a machine-readable `collapsed <n> lines` note to `StrategyOutput.notes` (D39 `Structural` lossiness: whole-line removals, each enumerated) feeding the `removals_enumerated` safety check.
Acceptance criteria:
- Runs shorter than 3 lines are never collapsed; collapsed output keeps the first raw line verbatim plus the `[×N repeats]` suffix.
- Normalization treats ISO-8601 timestamps, `0x`-prefixed and bare hex ≥8 chars, and RFC-4122 UUIDs as equal; everything else must match exactly.
- Protected nodes and `SourceFile`/`CodeSymbol` nodes are untouched.
Test cases:
- test_collapse_500_timestamped_lines: load `fixtures/logs/repeated-timestamps/input.txt` (500 identical lines with varying timestamps) into one `ToolResponse` node; output content has ≤5 lines and contains `"[×500"`; notes contain a `collapsed` entry accounting for the removed lines.
- test_two_repeats_untouched: node with 2 identical lines → byte-unchanged, strategy note records the skip.
- test_uuid_and_hex_normalization: 10 lines `"request 3f2a9c1d4b5e6f70 done (id=550e8400-e29b-41d4-a716-446655440000)"` with differing hex/uuid values → collapsed to 1 line containing `"[×10"`.
- test_protected_log_untouched: same 500-line content in a node with `protected: true` → byte-identical output.
- test_interleaved_runs: 5×"A", 1×"B", 4×"A" → output `A [×5]`, `B`, `A [×4]` (three lines; runs never merge across interruptions).

### [ ] E2-06: JSON-compaction strategy
Owner: R1 · Estimate: 1.5d · Depends: E2-03
Files: rust/compresso-core/src/strategies/compact_json.rs, tests in same file
Description: Implement `compact_json` per `03-interfaces.md` §2.3: unprotected `ToolResponse` nodes whose content parses as JSON are minified, long string values are truncated with an elision marker (`"…[+N chars]"`), and long arrays are elided in the middle (`"…[+N items]"` sentinel element), preserving object/array structure and all keys. Content that fails `serde_json` parsing is untouched. Every string truncation and array elision appends a machine-readable `truncated <json-pointer>` note to `StrategyOutput.notes` (D39 `Structural`: whole-unit truncations, each enumerated) feeding the `removals_enumerated` safety check.
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

### [ ] E2-07: Budget-packing strategy
Owner: R1 · Estimate: 1.5d · Depends: E2-03
Files: rust/compresso-core/src/strategies/pack_budget.rs, tests in same file
Description: Implement `pack_budget` per `03-interfaces.md` §2.3: while `total_tokens() > budget.max_tokens`, drop whole unprotected nodes in ascending `priority / token_count` order. Code nodes (`SourceFile`, `CodeSymbol`) are dropped whole or kept whole — never partially truncated. Dropped node ids are recorded as machine-readable `removed node:<id>` entries in `StrategyOutput.notes` (D39 note format, consumed by `removals_enumerated`); edges referencing dropped nodes are removed so the output still validates.
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

### [ ] E2-08: Safety module — run_all with eight checks
Owner: R1 · Estimate: 1.5d · Depends: E2-01, E2-02
Files: rust/compresso-core/src/safety.rs, tests in same file
Description: Implement `SafetyCheck` and `run_all(original, candidate, budget, elapsed_ms) -> Vec<SafetyCheck>` per `03-interfaces.md` §2.5 with exactly the eight named checks from `03-interfaces.md` §2.5: `protected_unchanged`, `no_inflation`, `json_still_valid`, `fences_balanced`, `refs_valid`, `latency_ok`, `schema_valid`, `removals_enumerated`. Per the D39 lossiness taxonomy, `removals_enumerated` fails when the token loss between original and candidate is not accounted for by the machine-readable strategy notes (`removed node:<id>`, `collapsed <n> lines`, `truncated <pointer>`) that the engine accumulates and passes alongside the graphs. Every check always appears in the output vector with `passed` and a human-readable `detail` containing no raw node content.
Acceptance criteria:
- `run_all` returns exactly eight checks with those names, in that order, on every call.
- `protected_unchanged` compares byte equality of content for every node protected in the original; a missing protected node also fails it.
- `detail` strings reference node ids and counts only — a test asserts no node content substring leaks into any detail.
Test cases:
- test_all_pass_on_identity: `run_all(g, g, budget, 10)` → all eight `passed == true`.
- test_protected_mutation_fails: candidate changes one byte of a protected node → `protected_unchanged.passed == false`, detail names the node id.
- test_inflation_fails_without_note: candidate `total_tokens()` > original → `no_inflation.passed == false`.
- test_broken_json_fails: original node content `{"a":1}` becomes `{"a":` → `json_still_valid` fails; a node that was never valid JSON does not trigger it.
- test_unbalanced_fence_fails: original with two ``` fences, candidate with one → `fences_balanced` fails.
- test_dangling_file_ref_fails: candidate contains `src/ghost.rs:42` absent from the original → `refs_valid` fails; refs present in the original pass.
- test_latency_exceeded_fails: `elapsed_ms = 500` with `budget.max_latency_ms = 200` → `latency_ok` fails.
- test_invalid_candidate_schema_fails: candidate with duplicate node ids → `schema_valid` fails.
- test_removals_not_enumerated_fails: candidate is 300 tokens smaller than the original but the accumulated strategy notes account for none of it → `removals_enumerated.passed == false`, detail reports the unaccounted token count; with matching `removed node:`/`collapsed`/`truncated` notes covering the delta, the check passes.

### [ ] E2-09: Engine::compress pipeline, trace generation, fallback
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
Description: Implement `unfold(store, repo, path, symbol)` (exact body via stored line range, read from disk, blake3 hash-checked against the stored file hash; per D44 staleness is never an error: on hash mismatch the function re-locates the symbol **by name** in the current on-disk content and serves the current body with `stale: true` plus a reindex hint — `not_found` is returned only when the symbol cannot be re-located in the changed file), `search(store, repo, query, limit)` over FTS5 (delegating to `Store::search_symbols`), and `read_bounded(root, path, range, budget_tokens, counter)` per `03-interfaces.md` §4. Every externally-supplied path is canonicalized and must satisfy `starts_with(approved_root)` **after** symlink resolution; violations return `AtlasError::OutsideRoot`. Path traversal and symlink-escape tests are REQUIRED for this ticket to close.
Acceptance criteria:
- `unfold` returns the exact source lines `start_line..=end_line` when the hash matches; after an on-disk edit without reindex, it re-locates the symbol by name and returns the symbol's current on-disk body (not blindly the stored range) with `stale: true` and a reindex hint; no `stale_index`-style error variant exists (D44).
- `read_bounded` truncates at `budget_tokens` with an explicit truncation flag and honors 1-based inclusive `range`.
- `../` traversal, absolute paths outside root, and symlinks pointing outside root all return `AtlasError::OutsideRoot`; symlinks resolving inside root are allowed.
Test cases:
- test_unfold_exact_body: unfold `SessionManager::start` on the E3-05 fixture → returned text byte-equal to lines 10–14 of sample.rs.
- test_unfold_stale_after_disk_edit: index, insert two lines above the symbol and append a trailing comment, unfold → `stale == true`, returned body is the symbol's current (shifted) on-disk body, hint mentions `compresso index`.
- test_unfold_symbol_deleted_not_found: index, delete the symbol from the file, unfold → `Err(AtlasError::SymbolNotFound)` with a `compresso index` instruction — never a stale error variant (D44).
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

### [ ] E5-02: Tool handlers — compresso_repo_map, compresso_outline, compresso_unfold
Owner: R2 · Estimate: 1.5d · Depends: E5-01, E3-10, E3-11
Files: rust/compresso-mcp/src/tools/atlas_nav.rs, tests in rust/compresso-mcp/tests/tools_nav.rs
Description: Implement the three navigation handlers per `04-mcp-and-integrations.md` §1, calling `repo_map`, `outline`, and `unfold` with `max_tokens` budgets from arguments (name per 04 §1.1, Tetris-compatible per D7; defaults from `Config`). Argument names/semantics stay compatible with the Tetris equivalents per D7 (`tetris_outline`→`compresso_outline`, `tetris_unfold`→`compresso_unfold`). Handlers are pure request/response, validate arguments against the tool schema, and map `AtlasError` variants to `isError` tool results with the stable bracketed codes from 04 §1.1 (`not_indexed`, `outside_root`, `not_found`, `invalid_arguments`, `internal`). Per D44, `compresso_unfold` never returns a staleness error: on hash mismatch it serves the current on-disk body (re-located by name) with `stale: true` and a reindex hint; `not_found` only when the symbol is gone.
Acceptance criteria:
- Each tool called against a temp indexed fixture repo returns content matching the atlas-layer constants (same body as E3-10/E3-11 tests) inside MCP content blocks.
- Invalid arguments (missing `path`, `max_tokens` outside 100..100000) return `invalid_arguments` errors without invoking atlas.
- `compresso_unfold` on a stale file re-locates the symbol by name and returns the current body with `stale: true` and a reindex hint; the error surface has no `stale_index` code — `not_found` only when the symbol no longer exists, with up to 5 closest qualified-name matches listed (04 §1.7).
Test cases:
- test_repo_map_tool_budget: call with `{"max_tokens": 300}` → returned text ≤ 300 tokens by the o200k counter.
- test_outline_tool_matches_atlas: outline of `src/sample.rs` via MCP equals the E3-10 outline rendering.
- test_unfold_tool_exact_body: unfold `SessionManager::start` → text equals fixture lines 10–14.
- test_unfold_missing_symbol_error: unknown symbol → `isError` result starting with `[not_found]`, listing up to 5 closest qualified-name matches, no panic.
- test_unfold_stale_serves_current_body: edit the fixture file after indexing (shift the symbol two lines) → result `stale == true`, content equals the symbol's current on-disk body, hint mentions `compresso index` — not an error (D44).
- test_unfold_symbol_gone_not_found: delete the symbol from the file → `[not_found]` with a `compresso index` instruction; no `stale_index` code exists anywhere in the error surface.

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
Description: Implement the `Integration` trait (`client_id`, `detect`, `plan_install`, `install`, `verify`, `uninstall`), `InstallCtx` with `Scope::User | Scope::Project(PathBuf)`, `Detection`, `InstallPlan` (exact key-level diff, human-readable), `InstallReceipt`, `VerificationReport`, `RestoreReport` per `03-interfaces.md` §6, plus shared helpers: timestamped backup (`<config>.compresso-backup-<ISO timestamp>`, UTC with colons replaced by hyphens per 04 §2.1; sentinel `"(none: file created)"` when the config did not exist), atomic write (tmp file + rename on the same filesystem), and receipt persistence via `Store::save_receipt` (wired at the CLI layer so this crate keeps zero internal deps per `01-stack` §3 — receipts are returned as values, stored by the caller).
Acceptance criteria:
- `install` always writes a backup before modifying anything and returns a receipt listing the exact keys written, backup path, and compresso version.
- Atomic write proven: killing the process between tmp-write and rename (test seam) leaves the original config byte-intact.
- Crate has no `compresso-*` dependencies (serde types only), matching the dependency rule.
Test cases:
- test_backup_created_before_write: fake adapter installs into a temp config → backup file exists with original bytes and a `compresso-backup-<UTC ISO>` suffix (04 §2.1 format).
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
Description: Wire `compresso integrations list|install|uninstall|verify` (per-client or `--all`) with mandatory dry-run preview (`--dry-run` prints the `InstallPlan` diff; interactive install shows the plan and requires confirmation unless `--yes`), receipt persistence via `Store::save_receipt`, and `compresso uninstall` performing all-client restore then self-cleanup listing. Binary-path resolution per D42/04 §2.1: `InstallCtx.compresso_bin` is the absolute resolved path of the installed binary, preferring `~/.local/bin/compresso` (as installed by E4-05) and falling back to `std::env::current_exe()` with a printed warning when the installed path is absent — adapters always write an absolute `command` that resolves to an existing executable. Cross-adapter test suite runs every adapter under a fabricated `$HOME` (tempfile + env override; `USERPROFILE` on Windows) proving idempotency and restoration per build-doc §25.6 gates.
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
- test_installed_binary_path_preferred: with a binary present at `<temp HOME>/.local/bin/compresso`, the written `command` equals that absolute path; without it, `command` equals `current_exe()` and stderr carries a warning (D42).

---

## E7 — Eval & fixtures

### [ ] E7-01: Fixture loader
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
- test_eligibility_parsed: `fixtures/adversarial/prompt-injection-repo/fixture.toml` → `eligible == false`; `fixtures/logs/repeated-timestamps` → `eligible == true`.
- test_recall_probes_parsed: fixture with two `[[recall_probes]]` (one `protected = true`) → probes exposed with correct `answer_substring` and protected flags.
- test_eligibility_rule_violation_rejected: temp fixture with `eligible = true` and `mode = "passthrough"` → load error citing the §4 eligibility rules.
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

### [ ] E7-05: Recall-probe evaluation and passthrough baseline
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

### [ ] E8-04: Read endpoints — health, sessions, operations, stats
Owner: R2 · Estimate: 2.5d · Depends: E8-03
Files: rust/compresso-agent/src/api/read.rs, tests in rust/compresso-agent/tests/api_read.rs
Description: Implement `GET /v1/health` (`{status, version, components: [{name, status}]}` fed by the E8-08 health subsystem), `GET /v1/sessions` (paginated), `GET /v1/operations?session=&since=&limit=` (trace documents only, never raw content; retrieval rows expose `tokens_served` with `trace_json: null` per D47), and `GET /v1/stats?since=` returning `StatsSummary` in the 02-schemas §3 savings vocabulary — all against `Store` via `spawn_blocking` (D41).
Acceptance criteria:
- /v1/operations returns compression rows with their trace v1 documents and retrieval rows with `trace_json: null` + `tokens_served`; no response contains raw file or prompt content.
- /v1/stats field names match 02-schemas §3 exactly (`observed_tokens`, `applied_tokens`, `removed_tokens`, `compression_overhead`, `net_tokens_saved`, `modeled_cost_saved` labelled modeled).
- Pagination is stable (ordered by `created_at`), `limit` capped, and documented in schemas/local-api/v1.json.
Test cases:
- test_health_shape_and_components: health matches the 03 §7 shape.
- test_sessions_paginated: 30 sessions, limit 10 → three stable pages.
- test_operations_filter_by_session_and_since: filters compose.
- test_operations_retrieval_rows_null_trace: retrieval rows carry `tokens_served`, null trace (D47).
- test_no_raw_content_in_responses: compress a known canary string, then fetch all endpoints → canary absent everywhere.

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

### [ ] E8-06: Config endpoints
Owner: R2 · Estimate: 2d · Depends: E8-03
Files: rust/compresso-agent/src/api/config.rs, tests in rust/compresso-agent/tests/api_config.rs
Description: `GET /v1/config` returns the effective config with per-key sources from `Config::explain()` (03-interfaces §2.6). `PUT /v1/config` performs a validated partial update: merge into `~/.config/compresso/config.toml` with `toml_edit` (comments preserved), validate the merged result via `Config::load()` semantics, then atomic write (tmp + rename). Unknown keys are rejected; `privacy.telemetry = true` is rejected with code `telemetry_forbidden` (D6). Concurrent PUTs serialize behind a lock; failed validation leaves the file byte-untouched.
Acceptance criteria:
- GET lists every key with source `default|file|env|flag`.
- PUT with an unknown key → 400 `invalid_arguments`, file unchanged; PUT setting telemetry true → 400 `telemetry_forbidden` citing D6.
- Comments and unrelated keys in config.toml survive a PUT byte-for-byte.
Test cases:
- test_get_config_sources: env override shows source `env`.
- test_put_partial_update_atomic: valid PUT → only the targeted key changes, write is atomic (crash seam leaves original intact).
- test_put_unknown_key_rejected: file hash unchanged after rejection.
- test_put_telemetry_rejected_d6: `{"privacy":{"telemetry":true}}` → 400 `telemetry_forbidden`.
- test_concurrent_puts_serialized: two racing PUTs → both applied in some order, final file valid, no interleaved corruption.

### [ ] E8-07: Integration endpoints — two-step plan/install flow
Owner: D1 · Estimate: 3d · Depends: E8-03, E6-05
Files: rust/compresso-agent/src/api/integrations.rs, tests in rust/compresso-agent/tests/api_integrations.rs
Description: `GET /v1/integrations` returns detection + receipt status per client (`Integration::detect`, `Store::receipts`). `POST /v1/integrations/:client/plan` runs `plan_install` and returns `{plan, unified_diff, plan_hash, backup_path}` where `plan_hash` is the blake3 of the exact unified diff (03-interfaces §7 two-step flow). `POST /v1/integrations/:client/install` requires body `{approve: true, plan_hash}` matching a plan issued this supervisor session; a stale hash — the target config changed since planning, or the hash came from a previous session — is rejected with `plan_stale` and a fresh plan must be requested. `POST /v1/integrations/:client/uninstall` mirrors the flow. All writes go through the E6 adapters unchanged (backup, receipt, atomic write). `claude-desktop` as `:client` is refused citing D5.
Acceptance criteria:
- Install without a prior plan, with a wrong hash, or after the target config changed → rejected with `plan_stale`/`invalid_arguments`, zero writes.
- A successful API install produces a backup and receipt identical to a CLI-driven install (same adapter code path, asserted by comparing receipts).
- `POST /v1/integrations/claude-desktop/plan` → error citing D5; no Claude Desktop detection code path exists.
Test cases:
- test_plan_then_install_happy_path: plan → diff + hash → install with hash → receipt row, config updated.
- test_install_without_plan_rejected: install first → rejected, config untouched.
- test_stale_plan_hash_rejected: modify the client config between plan and install → `plan_stale`.
- test_uninstall_two_step: plan + approve → config restored, `uninstalled_at` stamped.
- test_claude_desktop_refused_d5: refusal message names the decision.

### [ ] E8-08: Health subsystem and embedded fallback mode
Owner: R2 · Estimate: 2d · Depends: E8-04
Files: rust/compresso-agent/src/health.rs, tests in rust/compresso-agent/tests/health_fallback.rs
Description: Component health registry (store, indexer, mcp, api, config) with status `ok|degraded|failed` and content-free detail strings, feeding `GET /v1/health`. Embedded fallback mode per the Phase 4 exit criteria: `compresso mcp serve` continues to work with no supervisor running (opens `Store` directly), and supervisor failure never blocks MCP, the CLI, or the underlying assistant (owner constraint 4).
Acceptance criteria:
- Health reports every component; a degraded component (e.g. indexer mid-failure) is visible without failing the whole endpoint.
- `compresso mcp serve` works with the supervisor stopped; killing the supervisor mid-session leaves a running stdio MCP server fully functional.
- CLI commands work with the supervisor down (direct Store access path).
Test cases:
- test_health_reports_all_components.
- test_degraded_component_visible: seam-fail the indexer → `indexer: degraded`, endpoint 200.
- test_mcp_serves_without_supervisor: no agent process → all ten tools answer.
- test_supervisor_kill_does_not_break_mcp: kill agent mid-session → in-flight MCP session keeps answering.
- test_cli_works_with_supervisor_down: `compresso stats --json` succeeds.

---

## E9 — Desktop (Tauri)

Per amended ADR-0010: the desktop app is a **local-API client only** — it never embeds the runtime crates and never touches the SQLite store directly; all data flows over the authenticated E8 local API. Presentation logic only (08 §6); ranking, entitlement evaluation, and strategy logic never appear in webview-reachable state. Epic estimate: ~5 wk.

### [ ] E9-01: Tauri shell as local-API client
Owner: D1 · Estimate: 4d · Depends: E8-04
Files: apps/desktop/src-tauri/tauri.conf.json, apps/desktop/src-tauri/src/main.rs, apps/desktop/src-tauri/src/api_client.rs, apps/desktop/src/main.tsx, apps/desktop/src/lib/api.ts, tests in apps/desktop/src-tauri/tests/api_client.rs
Description: Tauri app boots, reads the bearer token from `~/.local/state/compresso/api-token` **in Rust** (never handed to the webview, never in localStorage — 08 §6), attaches it to local-API requests made from Rust commands, and renders `/v1/health`. If the supervisor is not running, the shell offers to start it (`compresso agent start`) and shows a "runtime offline" screen — it never spawns engine code in-process (ADR-0010 amendment: no direct runtime embedding). Webview→Rust commands are a typed allowlist; no generic fs/shell/http primitives are exposed.
Acceptance criteria:
- `apps/desktop/src-tauri/Cargo.toml` declares no dependency on compresso-core, compresso-atlas, compresso-store, or compresso-mcp (dependency assertion); all data reaches the UI via local-API round-trips.
- Token isolation: a webview-context dump contains no token bytes; requests originate from Rust with the bearer attached.
- Supervisor-down state renders the offline screen with a start action; health renders per-component status.
Test cases:
- test_no_runtime_crate_deps: parse Cargo.toml → forbid compresso-{core,atlas,store,mcp}.
- test_token_never_in_webview_state: canary token → absent from serialized webview state and localStorage.
- test_health_rendered_from_api: mocked local API → component list rendered.
- test_offline_screen_when_agent_down: connection refused → offline screen with start action.

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
- Compression rows render all strategy results and the eight safety checks; retrieval rows render tokens_served and tool name.
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
Description: Dashboard over `GET /v1/stats` using the 02-schemas §3 vocabulary verbatim: `observed_tokens`, `applied_tokens`, `removed_tokens`, `compression_overhead`, `net_tokens_saved`, and `modeled_cost_saved` visibly labelled "modeled". `provider_verified_cost_saved` appears only when present (post-slice proxy data, E11-06) and is never fabricated or merged with modeled figures. Time-window and per-tool breakdowns.
Acceptance criteria:
- Field labels map 1:1 to the §3 vocabulary; modeled figures always carry the "modeled" label in the DOM.
- With no provider-verified data, that panel is absent (not zero-filled).
- Window and per-tool filters match `/v1/stats` aggregates.
Test cases:
- test_vocabulary_labels_exact.
- test_modeled_label_always_visible.
- test_provider_verified_hidden_when_absent.
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

### [ ] E10-01: Fastify scaffold and drizzle migrations
Owner: F1 · Estimate: 3d · Depends: E1-01
Files: apps/control-plane/src/app.ts, apps/control-plane/src/routes/index.ts, apps/control-plane/drizzle/schema.ts, apps/control-plane/drizzle/0001_init.sql, .github/workflows/ci.yml (ts + postgres job), tests in apps/control-plane/test/app.test.ts
Description: Fastify app with the `/v1` prefix and the universal error shape `{error: {code, message, request_id}}`; drizzle schema mirroring every table in 02-schemas §5 (users, organizations, organization_members, plans, subscriptions, entitlements, devices, device_sessions, release_channels, releases, feature_flags, policy_sets, event_batches, audit_logs, support_cases) including the `subscriptions` `num_nonnulls(owner_user_id, owner_org_id) = 1` check; migrations generated and applied in CI against a disposable Postgres. `audit_logs` is append-only: the migration issues `REVOKE UPDATE, DELETE ON audit_logs FROM app_role`. No route writes `event_batches` (D6).
Acceptance criteria:
- Migration SQL is semantically equivalent to 02-schemas §5 (columns, FKs, checks, uniques verified by an introspection test).
- Every route (including 404s) returns the error shape with a per-request `request_id`.
- CI boots Postgres, applies migrations from scratch, and runs the suite green.
Test cases:
- test_migrations_apply_clean: fresh database → all fifteen tables present.
- test_error_shape_on_404: unknown route → `{error: {code, message, request_id}}`.
- test_audit_logs_append_only: UPDATE as app role → permission denied.
- test_subscriptions_owner_exclusivity: both owner ids set → constraint violation.
- test_no_event_batches_route: route table contains no handler writing event_batches.

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

### [ ] E10-05: Entitlement issuance (JWS ed25519, kid rotation)
Owner: F1 · Estimate: 3d · Depends: E10-03
Files: apps/control-plane/src/routes/entitlements.ts, apps/control-plane/src/entitlements/signer.ts, tests in apps/control-plane/test/entitlements.test.ts
Description: `GET /v1/entitlements` returns a signed entitlement bundle: JWS with EdDSA/ed25519, header `kid` naming the signing key, payload carrying plan features from the `entitlements` table plus issued/expires timestamps (30-day offline validity per 03-interfaces §8). Signing keys live in KMS (E12-01 secret management); `current` and `next` kids are both live so clients embedding both public keys (03 §9) verify across rotation. Failure and past-due paths return **degraded-but-valid** bundles — never an error shape that could block local operation (build-doc §4). The server signs; clients only verify; issuance policy and abuse detection stay server-side (08 §11).
Acceptance criteria:
- Bundles verify against the published current key; a bundle signed with `next` also verifies on a client holding both keys.
- A past-due subscription yields a degraded bundle (reduced features, valid signature), not a 4xx.
- Bundle payload contains only plan/feature identifiers, timestamps, and ids — no email, no device metadata beyond the device id.
Test cases:
- test_bundle_verifies_current_kid.
- test_bundle_next_kid_verifies_for_rotation.
- test_past_due_returns_degraded_valid_bundle.
- test_bundle_payload_id_only.
- test_expiry_30_days.

### [ ] E10-06: Local — migration 0002 entitlement_cache
Owner: R2 · Estimate: 1d · Depends: E3-01
Files: rust/compresso-store/migrations/0002_entitlements.sql, rust/compresso-store/src/entitlements.rs, tests in rust/compresso-store/tests/migrations.rs
Description: Commit `0002_entitlements.sql` byte-for-byte per 02-schemas §4: `entitlement_cache` singleton table (`id CHECK (id = 1)`, `bundle_jws`, `key_id`, `fetched_at`, `expires_at` — hard offline-grace end, fetched_at + 30d unless the bundle says less). Add `Store::entitlement_cache_get() -> Result<Option<EntitlementCacheRow>>` and `Store::entitlement_cache_put(&EntitlementCacheRow)` (singleton upsert). Applies transactionally on top of 0001.
Acceptance criteria:
- Fresh open applies migrations 1 and 2; `schema_migrations` has both rows; reopen applies nothing.
- Two puts leave exactly one row (latest wins); get round-trips every field.
- A second row insert attempt violates the `id = 1` check.
Test cases:
- test_migration_0002_applies_on_fresh_and_existing_db.
- test_cache_singleton_upsert_latest_wins.
- test_cache_roundtrip_fields.
- test_second_row_rejected_by_check.

### [ ] E10-07: Local — EntitlementVerifier and keyring device credentials
Owner: R1 · Estimate: 2.5d · Depends: E10-06
Files: rust/compresso-agent/src/entitlements/verifier.rs, rust/compresso-agent/src/entitlements/credentials.rs, tests in rust/compresso-agent/tests/entitlements_verify.rs
Description: Implement `EntitlementVerifier::verify(bundle_jws, now)` per 03-interfaces §9: JWS EdDSA/ed25519, header `kid` selecting among `EMBEDDED_KEYS` (≥2 for rotation: current + next; releases ship both so bundles signed with `next` verify on versions N and N+1). Expired-but-within-grace bundles verify with `Entitlements::degraded = true`; bad signature, unknown kid, malformed JWS, or expired-past-grace → typed `EntitlementError`. Device credentials (refresh token, device private key) are stored exclusively via the `keyring` crate in the OS keychain (D43) — never on disk.
Acceptance criteria:
- Verification accepts current- and next-kid bundles and rejects tampered payloads, unknown kids, and alg-substitution attempts (only EdDSA accepted).
- Grace semantics: expired < grace end → `degraded = true`; past `expires_at` → error (cache layer then degrades to local_only).
- A regression test exercises registration + refresh and scans every file under the data/config/state directories for credential bytes → none found.
Test cases:
- test_verify_current_and_next_kid.
- test_unknown_kid_rejected.
- test_tampered_payload_rejected.
- test_alg_substitution_rejected.
- test_grace_window_sets_degraded.
- test_credentials_never_on_disk.

### [ ] E10-08: Local — EntitlementCache, background refresh, degradation matrix
Owner: R1 · Estimate: 2.5d · Depends: E10-07
Files: rust/compresso-agent/src/entitlements/cache.rs, tests in rust/compresso-agent/tests/entitlements_degradation.rs
Description: `EntitlementCache::current(now)` per 03-interfaces §9 — **never errors**: missing row, invalid JWS, unknown kid, expired-past-grace, or store failure all return `Entitlements::local_only()`. `refresh(client: &dyn CloudClient)` runs as a supervisor background task with exponential backoff and jitter, persisting via `entitlement_cache_put`. Degradation tests enumerate every failure mode — network down, HTTP 500, invalid bundle, revoked device (401), clock skew beyond grace, corrupt cache row — and assert each yields `local_only()` while compression, MCP, and the CLI keep working (owner constraint 4; ADR-0015; gate G11: all local features operational with the control plane down).
Acceptance criteria:
- `current()` is total: the full failure matrix returns `local_only()`, never `Err`, never a panic, never a block.
- Refresh backoff is exponential with jitter and caps; a successful refresh upgrades entitlements without restart.
- An end-to-end test runs MCP tool calls during a simulated cloud outage → all succeed.
Test cases:
- test_current_never_errors_failure_matrix: table-driven over the six failure modes → `local_only()`.
- test_refresh_backoff_with_jitter_caps.
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

### [ ] E11-06: Proxy operation records and verified savings
Owner: R1 · Estimate: 2d · Depends: E11-04, E11-05
Files: rust/compresso-proxy/src/records.rs, tests in rust/compresso-proxy/tests/records.rs
Description: Each proxied request records an operation with D47 semantics: compression operations carry a trace v1 document (root-schema validated); bypass/passthrough requests record `tokens_served` with `trace_json = NULL`. Raw prompts are never stored. Provider-reported usage fields on responses populate `provider_verified_cost_saved` — which is populated **only** from provider-reported usage deltas (02-schemas §3) and never mixed with modeled figures; both feed `compresso stats`.
Acceptance criteria:
- Compression rows validate against the trace root schema; bypass rows have null traces with tokens_served.
- A canary prompt string never appears in the store.
- `provider_verified_cost_saved` is absent when the provider reports no usage; modeled and verified figures are reported in separate fields.
Test cases:
- test_proxied_compression_records_root_valid_trace.
- test_bypass_records_null_trace_tokens_served.
- test_no_prompt_bytes_in_store.
- test_provider_verified_only_from_usage_deltas.
- test_modeled_and_verified_never_merged.

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

### [ ] E12-09: Beta, incident runbooks, and launch
Owner: PQ+I1 · Estimate: 3d · Depends: E12-08, E10-08
Files: docs/runbooks/incident-*.md, docs/runbooks/release-checklist.md, docs/beta/report.md
Description: Closed beta on real coding sessions measures true task-success degradation (<1%, D40 — the CI recall gate is a proxy, this is the proof) plus reduction and latency in the field. (Absorbs E14-13.) Incident runbooks per 08 §21 committed under `docs/runbooks/`: signing-key compromise, malicious release publication, source-repo breach, insider exfiltration, dependency compromise, leaked cloud credentials, entitlement-signing compromise (rotate to `next` kid, reissue, 30-day cache bound), counterfeit distribution, user-data exposure. Signing-compromise and rollback runbooks are exercised (tabletop plus a real staging drill) before stable. G1–G15 checklist signed off; stable channel opened.
Acceptance criteria:
- Beta report shows task-success degradation < 1% on real sessions; regressions triaged before launch.
- All nine runbooks committed; signing-compromise and rollback drills executed with logs attached.
- The G1–G15 release checklist is fully signed with named owners per gate.
Test cases:
- beta_task_success_degradation_below_1pct (measured report artifact).
- runbooks_committed_and_drilled (drill logs referenced).
- gates_g1_g15_checklist_signed_off.

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

### [ ] E13-05: compresso-providers crate and OpenAiChatDriver
Owner: R2 · Estimate: 4d · Depends: E13-03
Files: rust/compresso-providers/Cargo.toml, rust/compresso-providers/src/lib.rs, rust/compresso-providers/src/drivers/openai_chat.rs, deny.toml (network exception), tests + fixtures under fixtures/providers/openai/
Description: New crate — the second sanctioned network exception (`reqwest` + rustls) encoded in `deny.toml` per 07 §1.1: the only crate opening outbound connections to model providers, and only to endpoints the user explicitly configured against a signed host allowlist; a fresh install performs zero provider network I/O. `OpenAiChatDriver` implements `ProviderAdapter` parameterized by a profile: canonical→wire request serialization (developer-role mapping, strict-mode structured outputs per 07 §7), SSE parsing into `ModelEvent`s, tool-call extraction accumulating argument fragments in bounded buffers (default 1 MiB per call, profile-tunable downward), usage and finish-reason normalization. Shared payload types are exported for `compresso-proxy` consumption (07 §1.1).
Acceptance criteria:
- Request serialization matches recorded fixtures byte-for-byte for representative canonical requests.
- SSE fixture streams normalize into the exact expected `ModelEvent` sequences, including fragmented tool arguments.
- deny.toml permits reqwest only for compresso-providers (and compresso-updater); zero sockets opened when no provider is configured.
Test cases:
- test_request_serialization_matches_fixtures.
- test_sse_normalized_event_sequence.
- test_tool_argument_fragments_bounded_buffer.
- test_finish_and_usage_normalization.
- test_deny_toml_network_exception_scoped.
- test_fresh_install_zero_provider_network.

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
Files: profiles/anthropic.toml, profiles/openai.toml, fixtures/providers/{anthropic,openai}/, tests in rust/compresso-providers/tests/first_party_profiles.rs
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
Files: profiles/kimi.toml, profiles/glm.toml, fixtures/providers/{kimi,glm}/, tests in rust/compresso-providers/tests/expansion_profiles.rs
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

### [ ] E13-21: Agent-loop integration
Owner: R1+D1 · Estimate: 4d · Depends: E13-03, E13-14, E13-20
Files: rust/compresso-agent/src/loop.rs, rust/compresso-agent/src/session_coordinator.rs, tests in rust/compresso-agent/tests/agent_loop.rs
Description: The 13-step provider-neutral loop of 07 §4.2 in `compresso-agent`: receive task → resolve project/user/org policy → query Atlas → build canonical messages and tools → determine required capabilities → router selection → execute the adapter and consume canonical events → assemble and validate tool calls against canonical schemas (E13-14) → policy-gated approval → execute permitted tools → append canonical results and preserved provider state → continue until completion/limit/cancel/failure → return canonical output and usage. The loop owns maximum-turn, maximum-cost, maximum-time, and repeated-tool-call protections; steps 3–4 run the existing compression engine identically for every provider (compression sits above the canonical protocol); a model can never change its own permission level; provider-state custody stays in the session coordinator. Phase-1 exit criterion: the entire loop runs against `FakeProvider` with zero network, including a complete streaming tool loop; real drivers then run behind recorded fixtures. Accounting is local-only: normalized usage and content-free metadata via `record_operation` (D47 semantics extended with a usage column — store migration 0003 if required).
Acceptance criteria:
- Full loop over FakeProvider with zero sockets opened (asserted); streaming tool loop completes with validated arguments and recorded usage.
- Compression runs on assembled context before provider dispatch for every provider identically (trace evidence).
- Turn/cost/time/repeat protections each stop the loop with typed outcomes; a tool call requesting permission escalation is refused.
Test cases:
- test_full_loop_fake_provider_zero_network.
- test_compression_precedes_provider_dispatch.
- test_permission_escalation_refused.
- test_budget_protections_stop_loop_each.
- test_provider_state_custody_session_scoped.
- test_usage_recorded_locally_only.

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

### [ ] E14-10: Artifact-contents allowlist gate
Owner: I1 · Estimate: 1.5d · Depends: E14-02, E12-01
Files: scripts/release/artifact_allowlist.sh, .github/workflows/release.yml (gate step)
Description: Release-gate test (G13) asserting every published artifact contains only allowlisted contents: symbols stripped, no fixtures, no prompts or design documents, no source maps, no debug info, no `.git` metadata, no developer paths; private symbols archived separately (E14-02). Runs in `release.yml` against every artifact of every platform before publication.
Acceptance criteria:
- The gate passes on a clean release build for all platforms.
- Seeding a fixture file, a source map, or an unstripped binary into an artifact fails the gate (scratch-branch verification, documented).
- The allowlist itself is committed and reviewed — additions require a PR.
Test cases:
- test_allowlist_passes_clean_release.
- test_seeded_fixture_fails_gate.
- test_source_map_detected_and_failed.
- test_unstripped_symbols_detected.

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
Owner: owner+counsel · Estimate: 2d · Depends: E14-01
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
Files: docs/adr/0022-selective-obfuscation.md (update), scripts/release/obfuscate/ (spike artifacts)
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

---

## Execution order (flat, dependency-ordered)

Slice: E1-01, E1-02, E1-03, E1-04, E1-05, E1-06, E2-01, E2-02, E2-02a, E3-01, E2-03, E3-02, E3-03, E2-04, E2-05, E2-06, E2-07, E2-08, E3-04, E3-05, E3-06, E3-07, E3-08, E2-09, E7-01, E2-10, E2-11, E3-09, E7-02, E3-10, E3-11, E3-12, E4-01, E4-02, E4-03, E4-04, E4-05, E7-03, E7-04, E7-05, E7-06, E5-01, E5-02, E5-03, E5-04, E5-05, E5-06, E5-07, E6-01, E6-02, E6-03, E6-04, E6-05.

Post-slice, wave 1 — local platform: E8-01, E8-02, E8-03, E8-04, E8-05, E8-06, E8-07, E8-08, E14-01, E14-02, E14-03, E14-04.
Wave 2 — desktop: E9-01, E14-05, E9-02, E9-03, E9-04, E9-05, E9-06, E9-07, E9-08.
Wave 3 — cloud + local entitlements: E10-01, E10-02, E10-09, E10-03, E10-04, E10-05, E10-06, E10-07, E10-08, E10-10, E10-11, E10-12, E10-13, E10-14.
Wave 4 — provider proxy: E11-01, E11-02, E11-03, E11-04, E11-05, E11-06.
Wave 5 — model expansion (may start any time after E1; phases 1–3 have no dependency on E8–E12): E13-01, E13-02, E13-03, E13-04, E13-05, E13-06, E13-07, E13-08, E13-09, E13-10, E13-11, E13-16, E13-12, E13-13, E13-14, E13-15, E13-17, E13-18, E13-19, E13-20, E13-21, E13-22.
Wave 6 — release engineering and hardening close-out: E12-01, E12-02, E12-03, E12-04, E12-05, E12-06, E14-08, E14-09, E14-10, E14-11, E12-07, E12-08, E14-15, E14-16, E14-12, E14-14, E12-09.

Parallelism notes:
- Slice: E2 (R1) and E3 (R2) run concurrently after E1; E7-01/E7-02 (PQ/R1) overlap E2's tail; E4 (D1) starts once E2-09 and E3-12 land; E5 (R2) follows E3; E6 (D1) follows E5-01 and overlaps E5-02..07; E1-06 and E4-05 (I1) slot in any time after E1-02/E4-04. E2-02a is a maintainer task (needs an Anthropic API key) and can land any time after E2-02 without blocking anything — until it lands, the interim 1.15 factor applies.
- Post-slice: wave 1 (R2) and wave 3 (F1 owns cloud; R1/R2 own the E10-06..08 local entitlement tickets) run concurrently; wave 2 (D1) follows E8-04. Wave 5 (R1/R2/PQ) is independent of E8–E12 through its first three phases and should start alongside wave 1 to smooth R1/R2 utilization. E11 (R1) needs only E2/E3 and can overlap wave 3. E14-01..04 land early in wave 1 so release hardening inputs (dist profile, IPC suite) are ready when E12-01 starts; E14-05 rides with E9-01.
- Hard cross-epic edges: E12-01 needs E14-02; E9-08 needs E14-05; E13-12 needs E13-16; E12-09 needs E10-08 (cloud-outage gate G11); E14-16 needs E7-06 and E12-01; E14-15 runs last, after E12-08.
- Solo mode: all roles collapse to the owner; the flat lists above remain the execution order.

## Estimate rollup

| Epic | Tickets | Days | Weeks | Notes |
|------|---------|------|-------|-------|
| E1 Foundation | 6 | 9 | ~2 wk | +E1-06 tarball job (D42) |
| E2 Core engine | 12 | 17 | ~3.5 wk | +E2-02a calibration; eight safety checks |
| E3 Atlas | 12 | 20.5 | ~4 wk | D41 concurrency + D44 staleness folded in |
| E4 CLI | 5 | 6.5 | ~1.5 wk | +E4-05 installer (D42) |
| E5 MCP server | 7 | 10 | 2 wk | D47 recording, D44 unfold |
| E6 Integrations | 5 | 10 | 2 wk | installed-binary path resolution (D42) |
| E7 Eval & fixtures | 6 | 10.5 | ~2 wk (overlaps E2/E3) | +recall gate tickets (D40) |
| **Slice total** | **53** | **83.5 gross** | **~14 engineer-wk; 2 engineers ≈ 7–8 calendar wk** | |
| E8 Supervisor & local API | 8 | 19 | ~4 wk | |
| E9 Desktop (Tauri) | 8 | 25 | ~5 wk | |
| E10 Cloud + local entitlements | 14 | 38 | ~8 wk (F1, with R1/R2 local tickets in parallel) | |
| E11 Provider proxy | 6 | 16 | ~3 wk | |
| E12 Release engineering | 9 | 26 | ~5 wk | absorbs E14-06/07/13 |
| E13 Model & client expansion | 22 | 61 | ~12 wk (R1/R2/PQ, overlaps E8–E12) | |
| E14 IP & secure deployment | 13 | 26.5 | ~5.5 wk (I1-heavy, overlaps E12) | |
| **Post-slice total** | **80** | **211.5 gross** | heavily parallel across R1/R2/D1/F1/I1/PQ | |
| **Grand total** | **133** | **295 gross** | | |

The slice estimate grows from the pre-audit ~13 engineer-weeks to ~14: the audit remediation added real scope (calibration measurement, recall gates, installability, concurrency and staleness contracts) rather than padding. Post-slice figures are gross engineer-days per epic; with the parallelism above, E8–E14 land in roughly 4–5 calendar months for a 4–5 person team, dominated by E10 (F1) and E13 (R1/R2) running side by side.
