# 01 — Stack, Workspace, CI/CD, Environments

## 1. Rust dependencies (workspace `rust/Cargo.toml` `[workspace.dependencies]`)

Versions are floors, pinned by `Cargo.lock` at first build.

| Crate | Version | Used by | Purpose |
|-------|---------|---------|---------|
| serde (derive) | 1.0 | all | Serialization |
| serde_json | 1.0 | all | JSON |
| thiserror | 2.0 | libs | Typed errors per crate |
| anyhow | 1.0 | binaries only | Error context at edges |
| clap (derive) | 4.5 | cli | CLI parsing |
| tokio (rt-multi-thread, macros, io-std) | 1.45 | mcp, agent | Async runtime (rmcp requirement) |
| rmcp (server, transport-io) | 0.3 | mcp | Official MCP SDK, stdio transport |
| rusqlite (bundled) | 0.32 | store | SQLite + FTS5 |
| tiktoken-rs | 0.6 | core | o200k_base / cl100k_base exact counts |
| tree-sitter | 0.25 | atlas | Parsing runtime |
| tree-sitter-rust | 0.24 | atlas | Rust grammar |
| tree-sitter-typescript | 0.23 | atlas | TS + TSX grammars |
| tree-sitter-javascript | 0.23 | atlas | JS grammar |
| tree-sitter-python | 0.23 | atlas | Python grammar |
| blake3 | 1.5 | atlas, updater | Content hashing |
| ignore | 0.4 | atlas | gitignore-aware walking |
| toml | 0.8 | core, integrations | Config + Codex config edit |
| dirs | 6.0 | store, cli | Platform paths |
| tracing / tracing-subscriber | 0.1 / 0.3 | all | Structured logs, redaction layer |
| jsonschema | 0.26 | core, mcp | Boundary schema validation |
| insta | 1.41 | dev | Snapshot tests |
| proptest | 1.5 | dev | Property tests |
| criterion | 0.5 | dev | Latency benchmarks |
| tempfile | 3.14 | dev | Test dirs |
| keyring | 3.6 | agent (E10-local) | OS keychain for device credentials |
| jsonwebtoken | 9.3 | agent (E10-local) | JWS EdDSA verification of entitlement bundles |

Forbidden in slice: any crate performing network I/O from core/atlas/mcp paths (telemetry hard-off; local-only). `reqwest` allowed only in `compresso-updater` (post-slice) and dev tools.

## 2. TypeScript dependencies (Phase 6; recorded now)

pnpm ≥ 10 + turborepo 2. TypeScript 5.x strict. Biome for lint/format. Vitest for tests. Fastify 5 + zod 4 (API), Next.js 15 (site/account/admin), Better Auth 1.x, Stripe SDK 17, drizzle-orm + drizzle-kit (Postgres), pino (logs).

## 3. Workspace layout (authoritative)

```text
compresso/
  Cargo.toml                  # [workspace] members = ["rust/*"] resolver 3
  rust-toolchain.toml         # channel 1.96
  package.json                # pnpm root, private
  pnpm-workspace.yaml         # apps/*, packages/*
  turbo.json
  biome.json
  .github/workflows/ci.yml
  .github/workflows/release.yml   # stub until Phase 8
  decisions.md
  compresso-build.md
  docs/{plan,adr,architecture,security,privacy,integrations,runbooks}/
  rust/
    compresso-core/           # graph, tokens, strategies, safety, engine, trace, config
    compresso-atlas/          # scanner, parsers, indexer, queries
    compresso-store/          # sqlite pool, migrations, repositories/files/symbols/ops DAOs
    compresso-mcp/            # rmcp server, tool handlers, sessions
    compresso-integrations/   # Integration trait, claude_code/codex/cursor adapters, receipts
    compresso-cli/            # bin `compresso`
    compresso-eval/           # fixture loader, eval runner, gate checks
    compresso-agent/          # supervisor (E8, stub crate now)
    compresso-proxy/          # provider proxy (E11, stub crate now)
    compresso-updater/        # updater (E12, stub crate now)
  packages/{api-client,shared-types,ui,config}/    # stubs until Phase 6
  apps/{web,control-plane,admin,desktop}/          # stubs until Phase 5-6
  schemas/{context-graph,compression-trace,local-api,cloud-api}/   # versioned JSON Schemas
  fixtures/{code,debugging,logs,conversations,provider-payloads,adversarial}/
  installers/install-local.sh                      # slice: checksum-verified local install to ~/.local/bin
  installers/{install.sh,install.ps1,homebrew}/    # Phase 8 (signed, hosted)
  infra/{terraform,docker,monitoring}/             # Phase 6+
```

Crate dependency rule (enforced by review + `cargo-deny` later): `cli → {core, atlas, store, mcp, integrations, eval}`; `mcp → {core, atlas, store}`; `atlas → {store, core(tokens only)}`; `integrations → (nothing internal except serde types)`; `core → (no internal deps)`. No cycles; `store` never imports `atlas`.

## 4. CI/CD (`.github/workflows/ci.yml`)

Triggers: push to main, PRs. Jobs:

1. `rust` matrix: `macos-14` (ARM64), `macos-13` (x86_64), `ubuntu-24.04`, `windows-2022` — `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `cargo build --release -p compresso-cli`.
2. `ts` (activates Phase 6): `pnpm install --frozen-lockfile`, `pnpm turbo lint typecheck test`.
3. `eval` (ubuntu + macos-14): `cargo run -p compresso-eval -- run fixtures/ --gates docs/plan/gates.toml` — fails CI when release gates regress.
4. `security`: `cargo audit` (advisories), `cargo deny check licenses bans` (added E1; secret scanning via gitleaks action).

Caching: `Swatinem/rust-cache@v2`. Artifacts: per-platform release tarball (`compresso-<version>-<target>.tar.gz`) + `SHA256SUMS` uploaded on main; `installers/install-local.sh` downloads/copies and verifies the checksum before installing to `~/.local/bin/compresso` (slice-level install path, D42; signing is Phase 8).

## 5. Environment configuration

- Local dev: no env vars required; `compresso` works offline out of the box.
- Runtime env overrides (build-doc §17): `COMPRESSO_MODE`, `COMPRESSO_TARGET_TOKENS`, `COMPRESSO_TELEMETRY` (accepted, forced `false` in slice — no upload code exists), `COMPRESSO_BYPASS`, `COMPRESSO_LOG`.
- Config file: `~/.config/compresso/config.toml`, exact format in build-doc §17; `compresso config explain` prints effective value + source (default | file | env | flag).
- Data paths (macOS): config `~/Library/Application Support/compresso/config.toml` is NOT used — we standardize on XDG-style `~/.config/compresso` on all platforms except Windows (`%APPDATA%\compresso`), db at `~/.local/share/compresso/compresso.db` (`%LOCALAPPDATA%` on Windows), cache `~/.cache/compresso/`, logs `~/.local/state/compresso/logs/`. Rationale: parity with existing CLI-tool conventions; ADR-0016.
- Cloud (Phase 6): Fly.io apps `compresso-api` (Fastify container), Vercel-free — Next.js apps also containerized on Fly; Postgres via Fly Postgres or Neon (D27 OPEN); secrets via `fly secrets`; staging + production environments; migrations via drizzle-kit in CI release step.
