# Compresso — Locked Decisions

Date: 2026-07-21. Source: `compresso-build.md` + owner Q&A. Every item here is **decided**; ADRs in `docs/adr/` record rationale. Open items are marked OPEN.

## 1. Product scope and sequencing

| # | Decision | Value |
|---|----------|-------|
| D1 | First deliverable | Vertical slice: monorepo + Rust compression core + Atlas indexing + read-only MCP (stdio) + CLI + client integrations |
| D2 | Relationship to installed Tetris (`~/.local/bin/tetris`) | Clean rebuild. No Tetris source reuse. Tetris kept on disk as behavioral reference only |
| D3 | Tetris during development | Disabled now via manual config edit: Tetris MCP entry and CLAUDE.md managed block removed with backups. Binary and `~/.tetris/` data untouched |
| D4 | First integration clients | Claude Code, Codex (CLI + Desktop config), Cursor — all three in first slice. Both Codex and Cursor are installed on the dev machine; live testing possible |
| D5 | Claude Desktop | **Excluded.** No Claude Desktop bundle staging, detection, or integration. Owner constraint |
| D6 | Telemetry | **Remote telemetry: none built, none enabled.** Local-only stats. Any future telemetry is opt-in per build doc §21.3 and requires a new owner decision. Owner constraint |
| D7 | MCP tool surface | Spec tool names from build doc §13 (`compresso_repo_map`, `compresso_search`, `compresso_read`, `compresso_outline`, `compresso_unfold`, `compresso_context`, `compresso_impact`, `compresso_compress`, `compresso_stats`, `compresso_explain`), with argument names/semantics kept compatible with Tetris equivalents where the tools overlap (`tetris_read`→`compresso_read`, `tetris_outline`→`compresso_outline`, `tetris_unfold`→`compresso_unfold`, `tetris_search`→`compresso_search`, `tetris_compress_text`→`compresso_compress`, `tetris_context_gate`→gate mode of `compresso_read`) |
| D8 | Atlas languages, first pass | TypeScript/TSX, JavaScript, Python, Rust. Remaining build-doc languages (Go, Java, C/C++, C#, Ruby, PHP) later behind the same parser registry |
| D9 | Test corpora | Compresso monorepo itself (dogfood) + committed synthetic fixtures under `fixtures/` |
| D10 | Compression modes in slice | `safe` (default) and `passthrough`. `balanced`/`aggressive` later phases |
| D11 | Default compression mode | `safe` |

## 2. Local runtime technology

| # | Decision | Value |
|---|----------|-------|
| D12 | Language | Rust (toolchain 1.96.x, edition 2024) |
| D13 | Workspace | Cargo workspace at `rust/` with crates `compresso-cli`, `compresso-agent`, `compresso-core`, `compresso-atlas`, `compresso-mcp`, `compresso-proxy`, `compresso-integrations`, `compresso-store`, `compresso-updater`, `compresso-eval` (build doc §7) |
| D14 | Token counting | Real tokenizers, provider-tagged: `tiktoken-rs` (`o200k_base` primary, `cl100k_base` legacy) for OpenAI-exact counts + Claude-calibrated estimator (o200k count × calibration factor, calibrated against Anthropic `count_tokens` API offline fixtures). `TokenCounter` trait; every count carries `provider` tag |
| D15 | Parsing | tree-sitter with `tree-sitter-rust`, `tree-sitter-typescript` (ts+tsx), `tree-sitter-javascript`, `tree-sitter-python` |
| D16 | Local storage | SQLite via `rusqlite` (bundled feature), WAL mode, versioned migrations in `compresso-store`. FTS5 for search |
| D17 | MCP implementation | `rmcp` (official Rust MCP SDK), stdio transport first; authenticated localhost streamable HTTP later phase |
| D18 | Hashing | `blake3` for file content hashes |
| D19 | CLI framework | `clap` (derive), JSON output via `--json` on all automation commands |
| D20 | Error handling | `thiserror` per-crate error enums; `anyhow` only in binaries |
| D21 | Async runtime | `tokio` (required by rmcp); core compression stays synchronous/deterministic |
| D22 | Serialization | `serde` + `serde_json`; all cross-boundary schemas versioned (`schema_version` field) and validated |
| D23 | Install locations | Binary `~/.local/bin/compresso`; config `~/.config/compresso/config.toml`; data/db/cache/logs per build doc §16 with native platform equivalents |
| D24 | Desktop app | Tauri (later phase, per build doc) |

## 3. Cloud technology (contracts specified now; built Phase 6)

| # | Decision | Value |
|---|----------|-------|
| D25 | API framework | Fastify (TypeScript) control-plane API |
| D26 | Web frontends | Next.js: public site, account app, admin console |
| D27 | Hosting | Fly.io containers for API; managed Postgres (Fly Postgres or Neon — pick at provisioning, contracts unaffected). OPEN: exact Postgres provider |
| D28 | Billing | Stripe (subscriptions, seats, webhooks) |
| D29 | Auth | Better Auth (self-hosted TS), organization + SSO plugins; device authorization flow for CLI/desktop |
| D30 | Cloud DB | PostgreSQL; schema in `docs/plan/02-schemas.md`. Never stores source code, prompts, file paths, repo names, symbols, patches, raw tool args (build doc §18.2) |
| D31 | TS workspace | pnpm workspaces + turborepo; packages per build doc §7 (`apps/web`, `apps/control-plane`, `apps/admin`, `packages/*`) |

## 4. Delivery and operations

| # | Decision | Value |
|---|----------|-------|
| D32 | VCS | Git, local repo now; GitHub remote when owner creates it. CI authored as GitHub Actions from day one |
| D33 | CI matrix | macOS ARM64 (primary dev), macOS x86_64, Linux x86_64, Windows x86_64. Rust fmt+clippy+test, TS lint+typecheck+test |
| D34 | Platform priority | macOS ARM64 first-class (dev machine); others must stay green in CI |
| D35 | Release signing | Deferred to Phase 8 per build doc §23; dev builds unsigned but versioned |
| D36 | Naming/trademark | OPEN — owner validates `compresso` name, domains, package ids before public launch (build doc §2) |
| D37 | Latency gates | Per build doc §24.3/§25.6: P95 local compression < 200 ms, MCP startup < 500 ms, safe-mode median reduction > 25% on eligible fixtures, task-success degradation < 1% |
| D38 | Fail-open | Every compression path falls back to original content on failure, uncertainty, or unknown format (build doc §4) |

## 5. Owner hard constraints (never override without new owner decision)

1. No telemetry enabled, ever, without explicit opt-in decision (D6).
2. No Claude Desktop bundle/integration (D5).
3. Source code and prompt content never leave the device by default.
4. Cloud/licensing failure must never break the underlying coding assistant.
5. All assistant-config changes: preview + timestamped backup + key-level receipt + reversible uninstall.
