# 0011 — No account requirement for local features

## Status

Accepted

## Context

Build-doc §5 requires the user to choose local-only or account-backed operation at first launch, and §20.1 defines a Local plan — deterministic compression, repository maps, outlines and symbol retrieval, MCP integration, local statistics — with "no account required." Owner hard constraint 4 forbids licensing failures from breaking the coding assistant, which is only structurally guaranteed if the core never needs an entitlement.

## Decision

Every Local-plan feature works permanently without an account, sign-in, or network access. Accounts gate only Pro, Team, and Enterprise features (build-doc §20), and entitlement checks never sit in the local compression, indexing, or MCP paths.

## Consequences

- Onboarding completes without a signup wall, and the installer never signs users in (build-doc §22).
- Offline behavior for free features is trivially correct: there is nothing to cache or refresh (§18.1 grants "permanent access to local/free features").
- Entitlements can only enable extras; they can never disable or interfere with the underlying assistant (§20).
- Monetization must come from genuinely additive Pro/Team value — proxy, advanced routing, multi-device, org controls — not from paywalling the core.
