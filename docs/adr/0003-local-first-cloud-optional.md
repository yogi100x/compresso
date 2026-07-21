# 0003 — Local-first operation, cloud-optional

## Status

Accepted

## Context

Compresso's core promise operates on source code and prompts, which must not leave the device by default (build-doc §2, owner hard constraint 3). The compression path must keep working during cloud outages (build-doc §2, §18), and §24.3 sets an explicit objective of zero cloud dependency in the active compression path. Owner hard constraint 4 forbids cloud or licensing failures from ever breaking the underlying coding assistant.

## Decision

All compression, indexing, retrieval, MCP serving, and statistics run locally against local storage. The cloud provides only accounts, entitlements, billing, releases, team policies, and support; no live cloud request ever sits in the compression path.

## Consequences

- The product works fully offline for local-plan features, and cloud outages degrade only account-related conveniences.
- Source code, prompts, file paths, and symbols stay on-device by default; cloud tables are schema-forbidden from storing them (02-schemas §5).
- Local SQLite is the source of truth for sessions, operations, and savings; the dashboard reads it directly.
- Server-side features (cross-device history, cloud analytics) are structurally harder to add and must justify themselves against the privacy posture.
