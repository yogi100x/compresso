# 0020 — Profile-first provider integration

## Status

Accepted

## Context

With the canonical protocol in place (ADR-0019), each new provider still needs an integration path. Most target providers (Kimi, GLM, MiniMax, DeepSeek) expose OpenAI-compatible or Anthropic-compatible endpoints, but "compatible" APIs differ in tool-choice modes, JSON Schema subsets, reasoning replay, and streaming quirks. Writing native code per provider would make every integration a compiled, release-coupled engineering project and would grow an unbounded provider-specific surface; pretending the compatible APIs are identical would ship silent breakage instead.

## Decision

A compatible provider should normally be added with a model profile using an existing protocol driver; provider-specific native code is reserved for behavior that cannot be represented safely by the existing drivers and profile schema (`compresso-expand.md` §1, restated in `07-model-expansion.md` §1.2). Profiles are signed, versioned TOML validated against `schemas/model-profile/v1.json`, declaring endpoints, host allowlists, model IDs, limits, capability evidence, tool-schema restrictions, and compatibility flags — never secrets or executable code. The §5.3 decision test gates escalation: a native adapter is written only when a provider needs a different stream parser, custom authentication or signing, or a provider-specific recovery state machine, and every native adapter requires its own ADR explaining why a profile is insufficient. Native adapters still speak the canonical protocol. All declared behavior is proven by the conformance suite before a capability is marked verified.

## Consequences

- A compatible provider can be added or updated by shipping signed configuration, without rebuilding Compresso — including capability corrections when a provider changes behavior.
- The native-adapter surface stays minimal by construction; growth there is a tracked design smell prompting driver or schema generalization.
- Provider differences become auditable data (profile diffs) instead of scattered code, and the same conformance suite exercises every integration path.
- Profiles cannot express genuinely novel protocols, so a provider needing one waits for a justified native adapter — a deliberate speed-for-safety trade.
- The profile schema itself becomes critical security surface: it is signed, pinned by `schema_version`, rejects unknown fields, and can never inject arbitrary headers or code.
