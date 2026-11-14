# Compresso Implementation Plan — Overview

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan ticket-by-ticket. Tickets live in `05-tickets.md` and use checkbox syntax.

**Goal:** Build Compresso — a local-first context intelligence layer for AI coding tools — to production quality per `compresso-build.md`, starting with a vertical slice (monorepo + Rust compression core + Atlas repository indexing + read-only MCP server + CLI + Claude Code/Codex/Cursor integrations) that is a working, installable product on its own. "Installable" in the slice means a checksummed release tarball + `installers/install-local.sh` placing the binary at `~/.local/bin/compresso` (SHA256-verified, unsigned); native code signing, notarization, and package managers remain E12.

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
- Task-success proxy in CI: recall-probe gate (every fixture recall probe answerable from compressed output; 100% on protected probes, ≥99% overall) plus a compressed-vs-passthrough baseline comparison. True task-success (<1% degradation) is measured on real sessions in closed beta before the stable release — the CI gate is a proxy, not the proof.
- Never report savings that were not actually applied; keep `observed`/`applied`/`modeled`/`provider_observed` metrics separate.
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
E13 Model & client expansion (07-model-expansion.md; source: compresso-expand.md)
E14 IP protection & secure deployment (08-ip-and-secure-deployment.md; source: ip.md)
E15 Clean-room advanced capabilities (09-advanced-capabilities.md; source: compresso-addon-capabilities.md)
E16 Full-product completion (routing, modes, languages, integrations, operations)
```

| Epic | Scope | Build-doc phase | Estimate |
|------|-------|-----------------|----------|
| E1 Foundation | Monorepo, workspaces, CI, conventions, fixtures skeleton | 1 | ~2 wk |
| E2 Core engine | ContextGraph, tokenizers, strategies, safety, engine, traces | 2 | ~5 wk |
| E3 Atlas | Store, scanner, tree-sitter parsers, incremental indexer, queries | 3 | 4 wk |
| E4 CLI | `compresso` binary, all slice commands, JSON output | 3–4 | ~1.5 wk |
| E5 MCP server | rmcp stdio server, 10 read-only tools, sessions/ledger | 4 | ~2 wk |
| E6 Integrations | Framework + Claude Code, Codex, Cursor adapters | 5 (pulled in) | ~2 wk |
| E7 Eval & fixtures | Fixture corpus, eval runner, release gates | 2–3 | ~2.5 wk (overlaps) |
| E8–E12 | Supervisor, desktop, cloud, proxy, release eng | 4–9 | full tickets in 05 |
| E13 Model & client expansion | Canonical protocol, drivers, profiles, router, initial and future providers | post-slice | 32 tickets in 05 |
| E14 IP & secure deployment | Hardening, anti-tamper, legal/compliance, high-impact sandbox gates | 8–9 | 17 tickets in 05 |
| E15 Clean-room advanced capabilities | Search, transactional edit, SQL, recall, knowledge, review, deferred browser/terminal | future | 34 tickets in 05 |
| E16 Full-product completion | Remaining routing, modes, languages, clients, desktop/cloud operations | future | 16 tickets / 59d |

Slice total (E1–E7, 58 tickets): ~19 solo engineer-weeks; 2 engineers approximately 9–10 calendar weeks; solo approximately 3–3.5 months. Full backlog: 214 tickets (plus the 8-ticket PB lane); rollup at the end of `05-tickets.md`.

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
| `decisions.md` (repo root) | Locked decisions D1–D66 |
| `01-stack-and-workspace.md` | Exact dependencies, workspace trees, CI/CD, env config |
| `02-schemas.md` | Versioned ContextGraph + trace JSON schemas, SQLite DDL, Postgres DDL |
| `03-interfaces.md` | Rust crate boundaries and module interfaces, local API, cloud API contracts |
| `04-mcp-and-integrations.md` | MCP tool JSON schemas, per-client integration specs |
| `05-tickets.md` | Epics → dependency-ordered tickets with acceptance criteria and test cases |
| `06-fixtures.md` | Evaluation fixture format and initial corpus |
| `07-model-expansion.md` | Canonical model protocol, drivers, profiles, router, conformance (source: `compresso-expand.md`) |
| `08-ip-and-secure-deployment.md` | IP boundaries, release hardening, anti-tamper, legal program (source: `ip.md`) |
| `09-advanced-capabilities.md` | Clean-room search, edit, SQL, recall, knowledge, review, parity, sandbox phases |
| `docs/adr/` | Architecture decision records (0001–0024) |



## Audit-r2 planning baseline

The authoritative post-audit backlog is 214 tickets: 58 slice and 156 post-slice (including E17), totaling 565 gross engineer-days before contingency. E16 closes every full-product deliverable previously present only in `compresso-build.md`. Role aliases are `PO` (product owner) and `LEGAL` (qualified external counsel); they are advisory/approval owners, not engineering implementers.
