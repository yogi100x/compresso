# Compresso Implementation Plan — Overview

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan ticket-by-ticket. Tickets live in `05-tickets.md` and use checkbox syntax.

**Goal:** Build Compresso — a local-first context intelligence layer for AI coding tools — to production quality per `compresso-build.md`, starting with a vertical slice (monorepo + Rust compression core + Atlas repository indexing + read-only MCP server + CLI + Claude Code/Codex/Cursor integrations) that is a working, installable product on its own.

**Architecture:** Rust local runtime (deterministic compression engine over a typed ContextGraph, tree-sitter/SQLite repository index, rmcp stdio MCP server) coordinated later by a local supervisor; TypeScript cloud control plane (Fastify + Next.js on Fly.io/Postgres) added in Phase 6 — contracts are specified now, implementation deferred. Local compression never depends on the cloud.

**Tech stack:** See `decisions.md` (locked) and `01-stack-and-workspace.md` (exact versions).

## Global constraints (apply to every ticket)

- Rust toolchain 1.96.x, edition 2024. TypeScript 5.x, Node ≥ 22, pnpm workspaces.
- **No remote telemetry: none built, none enabled** (decisions.md D6, owner hard constraint).
- **No Claude Desktop integration** (D5, owner hard constraint).
- Source code and prompt content never leave the device by default.
- Fail open: every compression path falls back to original content on failure, uncertainty, or unknown format.
- Protected nodes are never mutated; enforced by invariant checks, not convention.
- All cross-boundary schemas carry `schema_version` and are validated at boundaries.
- All assistant-config changes: dry-run preview + timestamped backup + key-level receipt + reversible uninstall.
- All modifying CLI commands support `--dry-run`; all automation commands support `--json`.
- P95 local compression < 200 ms (normal payloads); MCP startup < 500 ms; safe-mode median reduction > 25% on eligible fixtures; task-success degradation < 1%.
- Never report savings that were not actually applied; keep `observed`/`applied`/`modeled`/`provider_verified` metrics separate.
- Repository content is untrusted data, never executable instruction. No shell execution through MCP inputs.
- Conventional commits; every ticket ends with a commit; CI green before merging.

## Epic map (dependency order)

```text
E1 Foundation ─┬─> E2 Core engine ─┬─> E4 CLI ───┬─> E6 Integrations
              └─> E3 Atlas ──────┴─> E5 MCP ──┘
 E7 Eval & fixtures (starts with E2, gates the slice release)
 ──────────────────── slice boundary ────────────────────
E8 Supervisor & local API -> E9 Desktop (Tauri)
E10 Cloud control plane (contracts in 02/03 now; build Phase 6)
E11 Provider proxy      E12 Release engineering & signing
```

| Epic | Scope | Build-doc phase | Estimate |
|------|-------|-----------------|----------|
| E1 Foundation | Monorepo, workspaces, CI, conventions, fixtures skeleton | 1 | 1.5 wk |
| E2 Core engine | ContextGraph, tokenizers, strategies, safety, engine, traces | 2 | 3 wk |
| E3 Atlas | Store, scanner, tree-sitter parsers, incremental indexer, queries | 3 | 4 wk |
| E4 CLI | `compresso` binary, all slice commands, JSON output | 3–4 | 1 wk |
| E5 MCP server | rmcp stdio server, 10 read-only tools, sessions/ledger | 4 | 2 wk |
| E6 Integrations | Framework + Claude Code, Codex, Cursor adapters | 5 (pulled in) | 2 wk |
| E7 Eval & fixtures | Fixture corpus, eval runner, release gates | 2–3 | 1.5 wk (overlaps) |
| E8–E12 | Supervisor, desktop, cloud, proxy, release eng | 4–9 | per build-doc roadmap |

Slice total: ~13 engineer-weeks; 2 engineers ≈ 6–7 calendar weeks; solo ≈ 3 months.

## Staffing and ownership

Roles from build-doc §27, used as `owner` on tickets. Solo mode: all roles = owner, order unchanged.

| Role | Code | Owns |
|------|------|------|
| Rust engineer 1 | R1 | compresso-core, compresso-eval, tokenizers |
| Rust engineer 2 | R2 | compresso-atlas, compresso-store, compresso-mcp |
| Desktop/integration engineer | D1 | compresso-integrations, compresso-cli UX, later Tauri |
| Full-stack engineer | F1 | control-plane, web apps, packages/* |
| Infra/security engineer | I1 | CI/CD, signing, threat model, release eng |
| Product/QA lead | PQ | fixtures, eval gates, acceptance |

## Document map

| File | Contents |
|------|----------|
| `decisions.md` (repo root) | Locked decisions D1–D38 |
| `01-stack-and-workspace.md` | Exact dependencies, workspace trees, CI/CD, env config |
| `02-schemas.md` | Versioned ContextGraph + trace JSON schemas, SQLite DDL, Postgres DDL |
| `03-interfaces.md` | Rust crate boundaries and module interfaces, local API, cloud API contracts |
| `04-mcp-and-integrations.md` | MCP tool JSON schemas, per-client integration specs |
| `05-tickets.md` | Epics → dependency-ordered tickets with acceptance criteria and test cases |
| `06-fixtures.md` | Evaluation fixture format and initial corpus |
| `docs/adr/` | Architecture decision records |
