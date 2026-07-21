# 0018 — No Claude Desktop integration

## Status

Accepted

## Context

Build-doc §15 lists Claude Desktop among potential initial clients, but decision D5 — an owner hard constraint — explicitly excludes it: no Claude Desktop bundle staging, detection, or integration. The first-slice integration targets are Claude Code, Codex (CLI and Desktop config), and Cursor (D4).

## Decision

Compresso ships no Claude Desktop adapter: no detection logic, no `Integration` implementation, no MCP bundle staging, and no configuration writes for Claude Desktop. Revisiting this requires a new explicit owner decision; nothing in the codebase or documentation may imply forthcoming support.

## Consequences

- The integration matrix and its test burden stay bounded to Claude Code, Codex, and Cursor (03-interfaces §6 names exactly these `client_id`s).
- Assistant-detection code must not enumerate or touch Claude Desktop configuration files, keeping the owner constraint auditable.
- Users who want Compresso with Claude Desktop have no supported path, which support documentation states plainly.
- The generic `Integration` trait remains capable of expressing such an adapter later, so a future owner decision would be additive, not architectural.
