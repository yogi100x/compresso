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
| rmcp (server, transport-io; default features off) | =1.4.0 | mcp | Official MCP SDK, stdio transport; minimum patched line for RUSTSEC-2026-0189 |
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
| reqwest | 0.12 | proxy, providers, updater, account client | HTTPS behind crate-specific outbound-host policies |

Network and process capabilities are deny-by-default and assigned by the matrix in §3.1. A dependency being present elsewhere in the workspace never grants another crate permission to use it.

## 2. TypeScript dependencies (Phase 6; recorded now)

pnpm ≥ 10 + turborepo 2. TypeScript 5.x strict. Biome for lint/format. Vitest for unit tests; React Testing Library with `@testing-library/user-event` and `@testing-library/jest-dom` for component/accessibility tests; `@playwright/test` for browser E2E. Fastify 5 + zod 4 (API), Next.js 15 (site/account/admin), Better Auth 1.x, Stripe SDK 17, drizzle-orm + drizzle-kit (Postgres), pino (logs).

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
    compresso-model-protocol/ # E13 canonical model contracts
    compresso-providers/      # E13 sanctioned provider network boundary
    compresso-router/         # E13 deterministic model routing
    compresso-search/         # E15 bounded search
    compresso-edit/           # E15 plan/apply edits
    compresso-sql/            # E15 database inspection/execution
    compresso-recall/         # E15 encrypted recall
    compresso-knowledge/      # E15 scoped knowledge
    compresso-review/         # E15 review orchestration
    compresso-sandbox/        # E14 shared OS isolation primitives
    compresso-browser/        # E15 deferred, sandbox-gated
    compresso-terminal/       # E15 deferred, sandbox-gated
  packages/{api-client,shared-types,ui,config}/    # stubs until Phase 6
  apps/{web,control-plane,admin,desktop}/          # stubs until Phase 5-6
  schemas/{context-graph,compression-trace,local-api,cloud-api}/   # versioned JSON Schemas
  fixtures/{code,debugging,logs,conversations,provider-payloads,adversarial}/
  installers/install-local.sh                      # slice: checksum-verified local install to ~/.local/bin
  installers/{install.sh,install.ps1,homebrew}/    # Phase 8 (signed, hosted)
  infra/{terraform,docker,monitoring}/             # Phase 6+
```

Crate dependency rule: `core` has no internal dependencies; `store → core types`; `atlas → store + core::tokens`; `mcp → core + atlas + store`; `integrations` imports no algorithmic crate; `model-protocol` imports core types only; `providers → model-protocol`; `router → model-protocol`; `agent` may compose public interfaces but owns no provider-specific wire logic; E15 local crates use only the explicit core/store/atlas surfaces listed in 03; `browser/terminal → sandbox` and cannot bypass it. CI rejects cycles and undeclared internal edges.

### 3.1 Crate capability rules

| Crate group | Outbound network | Process execution | Additional boundary |
|---|---|---|---|
| core, atlas, store, mcp, search, edit, recall, knowledge, review, router, model-protocol | none | none | Pure local processing |
| integrations, cli, eval | none; maintainer calibration binary may call only the configured counting endpoint | no arbitrary child process; detection uses bounded executable lookup; the CLI's `commands/mcp_probe.rs` alone may start the exact adapter-written Compresso launcher for one bounded MCP handshake (ADR-0028) | Config paths and approved repositories only |
| proxy | Explicit configured provider hosts | none | TLS, signed/profile host allowlist, redirect/private-IP/body/time limits |
| providers | Explicit configured provider hosts | none | Keychain credentials and the same host controls |
| agent | Compresso account/entitlement control-plane origin only | supervisor-owned child lifecycle only | No arbitrary destination or command |
| updater | Signed release metadata/artifact origins only | installer/update helper only | Signature, digest, version and rollback checks |
| sql | User-created database profile endpoints only | none | No arbitrary HTTP; TLS/host/read-only policy |
| sandbox | Policy-approved destinations only | Policy-approved executable/argv only | Owns OS isolation, resource and cancellation controls |
| browser, terminal | No direct socket API | No direct spawn API | All capabilities delegated through sandbox |
| desktop webview | none | none | Typed Rust local-API bridge only |

CI parses every `Cargo.toml` and source dependency tree against this table. New capability requires an ADR, matrix update, threat-model update, and a deliberate failing-then-passing boundary test.

A `bounded-local` crate's exact Cargo integration-test target under `tests/` may use `std::process` to execute test artifacts; network APIs, process-capable dependencies, and process use in library, binary, build, example, or benchmark targets remain denied.

## 4. CI/CD (`.github/workflows/ci.yml`)

Triggers: push to main, PRs. Jobs:

1. `rust` matrix: `macos-14` (ARM64), `macos-15-intel` (x86_64), `ubuntu-24.04` (x86_64), `windows-2022` — `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `cargo build --release -p compresso-cli`.
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
