# 0023 - Clean-room WOZ-inspired capability expansion

## Status

Accepted

## Context

Public WOZCODE behavior identifies useful product categories including combined search, transactional editing, SQL intelligence, local recall, knowledge, multi-persona review, backtesting, and client integrations. Compresso must implement useful behavior without copying proprietary source, prompts, branding, identifiers, assets, or obfuscated implementation details.

## Decision

Adopt `compresso-addon-woz.md` only as behavioral research input. E15 defines original Compresso schemas, interfaces, tools, algorithms, fixtures, and product copy in `docs/plan/09-woz-inspired-capabilities.md`. Every E15 change records the public behavioral requirement it addresses and demonstrates that implementation artifacts were authored from Compresso specifications. WOZCODE is never a build, runtime, test, or evaluation dependency.

## Consequences

- Clean-room provenance is reviewable and backed by original fixtures.
- Feature parity is measured by Compresso requirements, not byte or prompt similarity.
- Unknown WOZ internals are out of scope rather than reverse engineered.
- License, trademark, and trade-secret review in E14 covers all E15 artifacts.
