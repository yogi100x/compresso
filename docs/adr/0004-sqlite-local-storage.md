# 0004 — SQLite local storage

## Status

Accepted

## Context

The local runtime needs transactional storage for repositories, files, symbols, edges, sessions, operations, and receipts (build-doc §16) on three platforms without asking users to run a database server. Search over symbols must be fast enough for interactive MCP calls. Decision D16 locks the implementation.

## Decision

Use SQLite via `rusqlite` with the bundled feature, WAL mode, FTS5 (trigram) for symbol search, and versioned migrations owned by `compresso-store`. Only `compresso-store` contains SQL; all other crates go through its typed methods (03-interfaces §3).

## Consequences

- Zero-dependency, single-file storage that installs and uninstalls cleanly and works identically across platforms.
- WAL mode gives concurrent readers with a single writer, which fits the supervisor-coordinated access pattern; the store must serialize writes.
- FTS5 provides bm25-ranked symbol search without an external search engine.
- Versioned migrations plus the raw-content prohibition (no prompts or full source stored, 02-schemas §4) are enforceable in one crate and testable in CI.
