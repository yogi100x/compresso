# 0001 — Rust modular local runtime

## Status

Accepted

## Context

The local engine sits in the hot path of every assistant request and must meet hard latency gates: P95 compression under 200 ms and MCP startup under 500 ms (build-doc §24.3, D37). It ships as a single self-contained binary to macOS, Windows, and Linux, and its correctness guarantees (protected-node invariants, deterministic output) benefit from a strong type system. Decisions D12 and D13 lock the toolchain and workspace shape.

## Decision

Implement the entire local runtime in Rust (toolchain 1.96.x, edition 2024) as a Cargo workspace at `rust/` with the crates listed in build-doc §7: `compresso-cli`, `compresso-agent`, `compresso-core`, `compresso-atlas`, `compresso-mcp`, `compresso-proxy`, `compresso-integrations`, `compresso-store`, `compresso-updater`, and `compresso-eval`. Crate ownership boundaries follow `docs/plan/03-interfaces.md`.

## Consequences

- Predictable latency with no garbage-collection pauses; the 200 ms P95 gate is achievable on commodity hardware.
- The type system encodes graph and protection invariants at compile time; `thiserror` per-crate error enums (D20) keep failure modes explicit.
- Crate boundaries let compression, Atlas, MCP, and store work proceed as independent tickets with `compresso-core` free of internal dependencies.
- Iteration is slower than in a scripting language and the team must staff two Rust engineers (build-doc §27); cross-boundary contracts with the TypeScript cloud must be schema-first.
