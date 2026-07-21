# 0006 — Deterministic compression before model-based summarization

## Status

Accepted

## Context

Model-based summarization is lossy, non-deterministic, slow, and expensive, while most context waste in coding sessions is mechanical: duplicates, repeated logs, verbose JSON, and stale history (build-doc §10.2). The release gates demand determinism tests, protected-content invariants, and a 200 ms P95 budget (§25.1, §25.6) that model calls cannot reliably meet. D10 scopes the slice to `safe` and `passthrough` modes.

## Decision

Ship deterministic strategies first — `dedup_exact`, `collapse_logs`, `compact_json`, `pack_budget` — as the default `safe` mode. Model-based summarization (local or bring-your-own-model) remains optional, explicitly enabled, and confined to later `balanced`/`aggressive` phases.

## Consequences

- Output is reproducible and snapshot-testable; the fixture corpus (docs/plan/06-fixtures.md) can assert exact preservation guarantees.
- No model cost, no network dependency, and no added latency variance in the hot path.
- The achievable reduction ceiling is lower than with summarization; the >25% median gate is calibrated to what deterministic strategies deliver.
- Every future lossy strategy must pass the same safety-check and fail-open machinery the deterministic strategies establish.
