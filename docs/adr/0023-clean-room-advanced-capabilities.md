# 0023 - Clean-room advanced capability expansion

## Status

Accepted

## Context

The reference product's publicly observable behavior identifies useful product categories including combined search, transactional editing, SQL intelligence, local recall, knowledge, multi-persona review, backtesting, and client integrations. Compresso must implement useful behavior without copying proprietary source, prompts, branding, identifiers, assets, or obfuscated implementation details.

## Decision

Adopt `compresso-addon-capabilities.md` only as behavioral research input. E15 defines original Compresso schemas, interfaces, tools, algorithms, fixtures, and product copy in `docs/plan/09-advanced-capabilities.md`. Every E15 change records the public behavioral requirement it addresses and demonstrates that implementation artifacts were authored from Compresso specifications, reimplemented from public observation without copying source, prompts, identifiers, or assets. The reference product is never a build, runtime, test, or evaluation dependency.

## Consequences

- Clean-room provenance is reviewable and backed by original fixtures.
- Feature parity is measured by Compresso requirements, not byte or prompt similarity.
- The reference product's unknown internals are out of scope rather than reverse engineered.
- License, trademark, and trade-secret review in E14 covers all E15 artifacts.
