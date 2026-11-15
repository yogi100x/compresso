# ADR-0027: Local retrieval savings accounting

## Status

Accepted.

## Context

The original ledger counted retrieval payloads as observed and applied tokens,
so `net_tokens_saved` correctly represented applied compression only. It did
not show the value of bounded repository retrieval, and treating a
counterfactual raw read as applied savings would violate D58.

## Decision

Retrieval operations persist content-free local metadata: repository id,
exact serialized pack tokens, estimated raw-source tokens, and modeled
follow-up calls avoided. `compresso_context` estimates its raw baseline as the
sum of indexed token estimates for matched symbol bodies before bounded
excerpt packing. A task-ready pack models one avoided follow-up read per
included evidence file; incomplete, stale, and no-match packs claim none.

Statistics keep two bases separate:

- compression savings are applied and retain the existing vocabulary;
- retrieval raw tokens, avoided tokens, and follow-up calls are explicitly
  modeled counterfactuals.

`total_effective_savings` is applied net compression savings plus modeled
retrieval tokens avoided and is labelled accordingly in human and structured
output. Historical retrieval rows retain exact pack counts but claim zero
modeled retrieval savings because no baseline was recorded.

All data remains in local SQLite. No prompt, query, source excerpt, or raw
result is persisted.

## Consequences

Users can inspect aggregate, per-repository, and bounded newest-first
per-operation results without conflating measured compression with modeled
retrieval. Results remain honest but are not a substitute for paired
evaluation against a controlled baseline.

Scope of the ledger: retrieval operations are recorded by the MCP server,
where a session exists to attribute them to. The one-shot CLI retrieval
commands (`map`, `search`, `read`, `outline`, `unfold`, `context`, `impact`)
write no ledger row, so `compresso stats` reports retrieval savings for MCP
sessions only; CLI `compress` is recorded. Among MCP tools only a task-ready
`compresso_context` pack claims avoided tokens; the other retrieval tools
record the tokens they served with a raw baseline equal to that, so they
add to pack totals and avoid nothing.
