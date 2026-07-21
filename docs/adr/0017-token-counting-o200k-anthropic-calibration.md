# 0017 — Token counting via tiktoken o200k with an Anthropic calibration factor

## Status

Accepted

## Context

Budgets, packing decisions, and every savings claim depend on token counts, and "never report savings that were not actually applied" (build-doc §4) extends to not reporting counts more precise than they are. OpenAI tokenizers are public (`o200k_base`, legacy `cl100k_base` via `tiktoken-rs`), but Anthropic's tokenizer is not; only the `count_tokens` API reports exact counts. Decision D14 locks the approach and 03-interfaces §2.2 defines the `TokenCounter` trait.

## Decision

Count OpenAI-bound text exactly with `tiktoken-rs` (`o200k_base` primary, `cl100k_base` legacy). Estimate Anthropic-bound text as the o200k count multiplied by a calibration factor derived offline from the committed `fixtures/provider-payloads/anthropic-counts` corpus against the Anthropic `count_tokens` API (procedure in docs/plan/06-fixtures.md §7.6). Every count carries a provider tag; unknown providers fall back to a tagged heuristic.

## Consequences

- OpenAI counts are exact; Anthropic counts are honest estimates, tagged `anthropic_calibrated`, and never feed `provider_verified_cost_saved` (02-schemas §3).
- Counting is fully offline and deterministic — no API calls in the hot path, in tests, or in CI.
- The calibration factor has documented provenance (fixture, model, date) and is recomputed when the corpus or tokenizers change, with the diff reviewed in the PR.
- A per-provider factor is a global approximation; content-type-specific error is accepted and bounded by the diversity of the calibration samples.
