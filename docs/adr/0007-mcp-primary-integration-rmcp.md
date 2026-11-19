# 0007 — MCP as the primary integration protocol via the rmcp SDK

## Status

Accepted

## Context

Claude Code, Codex, Cursor, and most VS Code-compatible assistants share one integration surface: the Model Context Protocol (build-doc §13, §15). Native hooks and the provider proxy are client- or provider-specific and riskier. Decision D17 selects `rmcp`, the official Rust MCP SDK, with stdio transport first; D7 locks the tool surface to the build-doc §13 names with argument semantics compatible with the predecessor CLI's behavioral reference where tools overlap.

## Decision

MCP is the primary integration protocol. `compresso-mcp` serves the read-only tool catalog over stdio using `rmcp`, with authenticated localhost streamable HTTP in a later phase; hooks and the proxy are secondary integration paths.

## Consequences

- One implementation covers all first-slice clients (D4), and read-only tools cannot write files or execute shells (build-doc §25.6, §13.3 gates write tools separately). Amended 2026-11-19 (D97, ADR-0035): the catalog is read-only except `compresso_edit`, a bounded, verified literal-edit tool served only when the operator starts the server with `--edits`; it executes no shell either.
- Using the official SDK tracks protocol revisions without bespoke wire-format code, at the cost of a `tokio` dependency (D21); core compression stays synchronous.
- Tool schemas are versioned and validated, and MCP failure must not prevent assistant startup (Phase 4 exit criteria).
- Server instructions stay brief and factual and never instruct clients to prefer Compresso over their own policy (build-doc §13.4). Amended 2026-11-19 (D92): this binds the default profile's instructions; opt-in surfaces (the lean tool profile and the exploration agent) may direct exploration to Compresso instead of shell search, and a tool description may state what the tool replaces (04 §1.2).
