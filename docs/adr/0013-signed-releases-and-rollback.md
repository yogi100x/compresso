# 0013 — Signed releases and rollback (Phase 8)

## Status

Accepted

## Context

Compromised release infrastructure and malicious updates are named threats (build-doc §21.1), and a bad update to a tool wired into coding assistants can break users' working environments at scale. Build-doc §22–23 define the signed installer and update pipeline; D35 defers production signing to Phase 8 while keeping dev builds versioned.

## Decision

All production releases ship with signed manifests and artifacts, native platform signing (macOS Developer ID plus notarization, Windows code signing, Linux signatures and checksums), SBOM, and build provenance. The updater verifies signatures before atomic binary switch, preserves the previous version, and rolls back automatically on failed health checks; a production gate requires that signed update verification cannot be bypassed silently.

## Consequences

- Update-channel compromise is mitigated: unsigned or tampered artifacts fail verification and are never installed.
- Failed updates self-heal via automatic rollback to the preserved previous version, tested on all platforms before launch (Phase 8 exit criteria).
- Development builds remain unsigned but versioned until Phase 8, so early adopters of dev builds accept a weaker guarantee.
- The team carries real release-engineering cost: signing infrastructure, key custody, canary rollout, and compromise drills (build-doc §24.4).
