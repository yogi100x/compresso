# 0022 — Selective obfuscation policy

## Status

Accepted

## Context

ADR-0021 accepts that crown-jewel logic shipping in native binaries can eventually be reverse engineered; obfuscation can raise that cost further but hurts performance, debuggability, crash symbolication, and reproducibility, and obfuscating the wrong components would destroy the auditable boundary users rely on to verify privacy claims (ip.md §8). Compresso also carries hard latency gates (D37: P95 < 200 ms) and a fail-open guarantee (ADR-0008) that any transformation must preserve.

## Decision

Obfuscation is selective, optional, and evidence-gated. Only the crown-jewel modules named in 08-ip-and-secure-deployment §1.1 are eligible: strategy registry ordering and estimate weights, `pack_budget` scoring, `compresso-atlas` ranking queries, proxy model-selection rules, and non-security entitlement presentation strings. Permitted techniques escalate from symbol renaming and string/constant protection to selective control-flow transformation and commercial LLVM-based passes on specific crates. Adoption requires that performance baselines and support processes already exist (ip.md §28 step 12), and every release gate (G1–G14) must be re-run and pass on the obfuscated `dist` build (gate G15). The auditable public boundary is never obfuscated: schemas, MCP contracts, installers, adapters, signature and entitlement verification, cryptographic code, updater trust logic, privacy controls, secret redaction, uninstaller/restore, crash recovery, and data deletion stay readable.

## Consequences

- Casual and mid-tier extraction of tuning weights and selection heuristics becomes materially harder; expert extraction remains possible and is not claimed otherwise.
- The privacy-relevant and safety-relevant surface stays inspectable, so obfuscation never undermines the trust story or vulnerability response.
- Each obfuscated release costs extra CI time (full gate re-run) and requires the private symbol archive to keep crashes debuggable; if gates regress, the technique is dropped rather than the gate relaxed.
- Obfuscation is explicitly the last E14 work item (E14-15 spike with a go/no-go outcome); shipping without it is the default and is acceptable.
