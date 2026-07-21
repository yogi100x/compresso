# 0019 — Canonical model protocol

## Status

Accepted

## Context

Decision D45 adopts `compresso-expand.md` as epic E13: Compresso will execute agent loops against Anthropic, OpenAI, Kimi, GLM, MiniMax, DeepSeek, and future providers. Provider APIs disagree on message roles, streaming envelopes, tool-call lifecycles, JSON Schema subsets, finish reasons, usage reporting, and reasoning-state handling. If those differences leak into the runtime, every core system — Atlas, memory, compression, permissions, the agent loop — accumulates provider conditionals, each new provider multiplies test surface, and the permission model becomes impossible to reason about because behavior varies by vendor.

## Decision

All provider access goes through a canonical internal protocol defined in `compresso-model-protocol`: canonical requests, messages, streaming events, finish reasons, errors, usage, and capabilities, consumed via the object-safe `ProviderAdapter` trait. Differences are absorbed by exactly three mechanisms below that trait — protocol drivers (reusable wire formats), model profiles (signed declarative configuration), and rare native adapters. Core crates (`compresso-agent`, `compresso-router`, `compresso-core`, `compresso-atlas`, `compresso-store`, `compresso-mcp`) never branch on a provider name; a CI check fails if a provider identifier appears in their source. Routing and permissions operate on verified capabilities and policy, never on vendor identity. Spec: `docs/plan/07-model-expansion.md`.

## Consequences

- One agent loop, one permission model, one compression path serve every provider; a provider regression cannot change what a model is allowed to do.
- Adding a provider does not touch core crates, so core test surface stays constant as the provider count grows.
- Normalization has a cost: canonical events are the lowest common *safe* denominator, and provider-exclusive features are unavailable until expressed as a capability plus profile flag (or, rarely, a native adapter with its own ADR).
- Unknown provider values (finish reasons, error shapes) must be retained for diagnostics while mapping to safe canonical behavior — silent coercion is a conformance failure.
- The canonical types become a load-bearing public surface: changing them requires updating `docs/plan/07-model-expansion.md` and every driver, so they are versioned and snapshot-tested from the first commit.
