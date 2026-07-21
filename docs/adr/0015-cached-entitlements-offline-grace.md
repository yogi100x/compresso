# 0015 — Cached entitlements with 30-day offline grace

## Status

Accepted

## Context

Paid features must keep working when the machine is offline or the control plane is down: build-doc §18.1 requires cached paid entitlements, at least 30 days of offline grace, and no interruption of active coding sessions, and owner hard constraint 4 forbids licensing failures from breaking the assistant. D29 provides device-bound auth to make cached grants verifiable.

## Decision

The control plane issues signed entitlement bundles (JWS EdDSA/ed25519, 30-day offline validity per 03-interfaces §8) that the local runtime caches in the `entitlement_cache` SQLite table (02-schemas §4) and verifies offline via `EntitlementVerifier` (03-interfaces §9) against embedded public keys (current + next, `kid`-selected, rotated across releases). Device credentials live in the OS keychain (`keyring` crate). Refresh happens in the background with exponential backoff; every failure path — missing, invalid, expired-past-grace — degrades to `Entitlements::local_only()`, never an error that blocks local operation.

## Consequences

- Cloud outages and travel do not interrupt paid features for at least 30 days; local/free features are unaffected forever (ADR-0011).
- Revocation has bounded latency: a revoked device retains paid features until its cached bundle expires, an accepted trade-off.
- Device credentials live in the OS keychain and bundles are signature-verified locally, so cached grants cannot be forged by editing files.
- Entitlement status must be visible but non-blocking in CLI and desktop UI — informational, never a modal gate on work.
