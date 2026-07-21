# 0014 — Localhost-only proxy

## Status

Accepted

## Context

The optional provider proxy sees complete model requests, including system prompts and provider API keys in flight — the most sensitive data in the product. Build-doc §14 requires the proxy to bind to localhost only, never persist provider keys, preserve unknown headers and fields, and pass unsupported payloads through unchanged; §21.1 lists proxy credential leakage as a threat.

## Decision

The proxy binds exclusively to `127.0.0.1` (default `127.0.0.1:7410`), never writes provider API keys to disk or logs, redacts secrets from diagnostics, and passes any payload it does not fully recognize through byte-unchanged. A request-level bypass and the process-wide `COMPRESSO_BYPASS=1` switch disable interference entirely.

## Consequences

- No remote network attack surface: the proxy is unreachable from other machines by construction.
- Credential leakage is bounded to process memory; audit traces record request metadata without raw prompts (build-doc §14).
- Unknown providers, fields, and formats keep working — compatibility failures degrade to passthrough, never breakage (D38).
- Streaming must be forwarded without buffering, which constrains how compression can ever apply to streamed responses.
