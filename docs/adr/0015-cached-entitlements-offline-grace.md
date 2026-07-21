# 0015 — Device-bound entitlements with 14-day renewal and 21-day expiry

## Status

Accepted

## Context

Paid features must tolerate ordinary travel and control-plane outages without making each local operation depend on the network. The original build-doc §18.1 requested at least 30 days of offline grace; owner decision D65 supersedes that duration with a more frequent machine check while retaining no interruption of active coding sessions. Owner hard constraint 4 still forbids licensing failures from breaking the assistant. D29 provides device-bound auth to make cached grants verifiable without hardware fingerprinting.

## Decision

The control plane issues device-bound machine leases as signed entitlement bundles (JWS EdDSA/ed25519). A normal lease is valid for 21 days and sets `refresh_after` at day 14. It binds account, device ID, device public-key hash, plan/features, lease ID, issued/not-before/refresh/expiry timestamps, and schema version. The local runtime caches it in the `entitlement_cache` SQLite table (02-schemas §4) and verifies it offline via `EntitlementVerifier` (03-interfaces §9) against embedded previous/current/next public keys selected by `kid`.

Enterprise offline is a separate signed lease class, not a configurable extension of a standard lease. An entitled organization administrator may request exactly 30, 60, or 90 days for one registered organization/device; renewal begins seven days before expiry and 90 days is the protocol maximum. The control plane derives eligibility and duration from current server records and the UI discloses the longer revocation-exposure window before issuance. A standard account cannot request, edit, or import itself into this class.

Renewal requires proof of possession: the server issues a short-lived single-use nonce and the client signs it with the device private key stored in the OS keychain. No automatic entitlement request occurs before day 14. If the day-14 attempt fails, the client retries at most once per 24 hours with bounded jitter until day 21. Explicit login, account status, or manual refresh may contact the service sooner. A successful renewal atomically replaces the cache with a new 21-day lease.

The renewal request is schema-allowlisted to device and lease identifiers, client/entitlement versions, nonce, and device-key proof. It contains no prompts, source, repository metadata, paths, operation history, or usage statistics and is never telemetry. Cloud-only features remain server-authorized on every request. `iat`/`nbf` permit five minutes of skew; refresh and expiry use monotonic elapsed time anchored to verified `fetched_at`. Unknown `kid` may trigger one due or explicit refresh, then degrades local-only. Every terminal failure — missing, invalid, device mismatch, or expired lease — yields `Entitlements::local_only()`, never an error that blocks local operation.

Signed dates and features are the only local authority: configuration, environment variables, CLI flags, cache columns, wall-clock changes, client integrity claims, and requested feature names cannot override verified claims. This remains tamper resistance, not a security boundary, because a determined owner can patch the verifier itself. Every paid cloud request is independently authorized from current server-side subscription, organization, seat, device, revocation, and capability state (D66); client-provided lease or integrity state is ignored for authorization.

## Consequences

- Cloud outages between day 14 and day 21 do not interrupt paid features; local/free features are unaffected forever (ADR-0011).
- Revocation has bounded local latency: a machine that remains offline can retain paid features for at most 21 days from lease issuance; cloud access stops immediately when the server knows it is revoked.
- An enterprise offline machine can retain paid features for its explicitly selected 30/60/90-day signed term; this longer revocation exposure is an accepted and disclosed enterprise trade-off.
- Device credentials live in the OS keychain and bundles are signature-verified locally, so cached grants cannot be forged by editing files.
- Entitlement status shows the next renewal date, hard expiry, last attempt, and retry state but remains non-blocking in CLI and desktop UI.
- Expiry never interrupts an active operation; paid local features downgrade on the next operation while free local functionality continues.
