# 0008 — Fail-open passthrough on any failure

## Status

Accepted

## Context

Compresso sits between a user and their coding assistant; a compression bug that drops or mangles context is strictly worse than sending the original tokens. Build-doc §4 makes "fail open to the original content" and "unknown request formats must pass through unchanged" product principles, and D38 locks this for every compression path.

## Decision

Every compression path falls back to the original, unmodified content on any failure, safety-check rejection, uncertainty, latency overrun, or unknown format. `Engine::compress` never returns an error: failures produce `fallback_used = true` with the input returned unchanged and a `fallback_reason` recorded in the trace (03-interfaces §2.4).

## Consequences

- The underlying assistant can never be broken by Compresso logic errors, cloud outages, or malformed inputs — the worst case is zero savings.
- Safety checks (§10.6) reject rather than repair; rejected results are discarded wholesale, which keeps the safety code simple and auditable.
- Passthrough rate becomes a first-class metric: silent over-fallback erodes the product's value and is watched in evaluation and local stats.
- Savings accounting stays honest — fallback operations report zero applied savings, never modeled ones (build-doc §4, §21.3).
