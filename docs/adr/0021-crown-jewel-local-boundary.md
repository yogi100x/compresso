# 0021 — Crown-jewel local boundary

## Status

Accepted

## Context

Compresso's most valuable logic is its strategy selection, priority/ranking scoring, and routing policies (ip.md §3.1; D46). Local-first is non-negotiable: source code and prompts never leave the device by default (owner constraint 3, ADR-0003), so the logic that ranks and compresses that content must execute on the user's machine — and anything executing locally can eventually be disassembled, traced, or patched (ip.md §4.3). Meanwhile some crown jewels (learned routing, abuse detection, strategy performance corpora, organization knowledge ranking) do not need local data to operate.

## Decision

Split the crown jewels along data gravity. Logic that must touch local source (strategy registry ordering and estimate weights in `compresso-core`, `pack_budget` scoring, `compresso-atlas` ranking queries, static model profiles in `compresso-proxy`) runs locally but ships only as compiled, signed, stripped native code built with the `dist` profile, from access-restricted source, optionally obfuscated per ADR-0022 — local yet never shipped readable. Logic that does not need local data (entitlement issuance and abuse policies, learned routing and reviewer tuning, org knowledge ranking, performance corpora) never leaves the control plane, and clients hold only public verification keys. Thin adapters, installers, and schemas stay at the auditable public boundary and never import crown-jewel crates (enforced by the 01-stack §3 dependency rule).

## Consequences

- A leaked or reverse-engineered client binary reveals only the locally necessary subset, at real analysis cost; server-side jewels and all signing/service credentials stay unexposed (ip.md §4.2).
- We accept honestly that local algorithms are recoverable by determined experts; protection there is trade secret plus friction, never a security control — authorization and release authenticity remain server-side and signature-based (ADR-0013, ADR-0015).
- Local paid-feature checks are UX only; patching them is expected and answered with server-side enforcement and license terms, never destructive anti-tamper (owner constraint 4, ADR-0008).
- Moving algorithmic logic into adapters, installers, or the Tauri webview is a review-blocking violation; the boundary stays auditable so privacy claims stay verifiable.
