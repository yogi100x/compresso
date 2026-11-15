# Compresso — Locked Decisions

Updated: 2026-11-14. Source: `compresso-build.md` + owner Q&A. Every item here is **decided**; ADRs in `docs/adr/` record rationale. Open items are marked OPEN.

## 1. Product scope and sequencing

| # | Decision | Value |
|---|----------|-------|
| D1 | First deliverable | Vertical slice: monorepo + Rust compression core + Atlas indexing + read-only MCP (stdio) + CLI + client integrations |
| D2 | Relationship to the locally installed predecessor binary and its data directory | Clean rebuild. No predecessor source reuse. The predecessor CLI kept on disk as behavioral reference only |
| D3 | The predecessor CLI during development | Disabled now via manual config edit: the predecessor CLI's MCP entry and CLAUDE.md managed block removed with backups. The locally installed predecessor binary and its data directory untouched |
| D4 | First integration clients | Claude Code, Codex (CLI + Desktop config), Cursor — all three in first slice. Both Codex and Cursor are installed on the dev machine; live testing possible |
| D5 | Claude Desktop | **Excluded.** No Claude Desktop bundle staging, detection, or integration. Owner constraint |
| D6 | Telemetry | **Remote telemetry: none built, none enabled.** Local-only stats. Any future telemetry is opt-in per build doc §21.3 and requires a new owner decision. Owner constraint |
| D7 | MCP tool surface | Spec tool names from build doc §13 (`compresso_repo_map`, `compresso_search`, `compresso_read`, `compresso_outline`, `compresso_unfold`, `compresso_context`, `compresso_impact`, `compresso_compress`, `compresso_stats`, `compresso_explain`), with argument names/semantics kept compatible with the predecessor CLI's equivalents where the tools overlap (predecessor `read`→`compresso_read`, `outline`→`compresso_outline`, `unfold`→`compresso_unfold`, `search`→`compresso_search`, `compress_text`→`compresso_compress`, `context_gate`→gate mode of `compresso_read`) |
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

## 5. Post-audit decisions (2026-07-21 audit remediation)

| # | Decision | Value |
|---|----------|-------|
| D39 | Lossiness taxonomy | Safe mode = deterministic + rule-based, NEVER called lossless. `None` → dedup only; `Structural` (safe default) → whole-unit removals, all enumerated in trace + `removals_enumerated` safety check; `Summarizing` → aggressive only |
| D40 | Task-success gate | CI proxy: recall-probe gate (100% protected, ≥99% overall) + compressed-vs-passthrough baseline comparison. Real task-success (<1% degradation) measured in closed beta before stable |
| D41 | Store concurrency | `Store` = `Arc<Mutex<Connection>>`, Send+Sync, blocking API, `spawn_blocking` from async, busy_timeout 5s. Escape hatch: writer thread behind same API |
| D42 | Slice installability | Checksummed tarball + `installers/install-local.sh` → `~/.local/bin/compresso` in slice (SHA256-verified, unsigned). Signing/notarization/package managers stay E12. Narrowed 2026-11-15 by ADR-0030: the checksum manifest is keyless-signed by main-branch CI and the installer verifies it; native signing, notarization, and package managers still stay E12 |
| D43 | Entitlements local impl | `entitlement_cache` SQLite table, JWS ed25519 verify vs embedded previous/current/next keys, `keyring` for device creds, all failures → `local_only()` |
| D44 | Staleness contract | Never an error: serve current content with `stale: true` + reindex hint; unfold re-locates symbol by name in changed file, `not_found` only if it disappeared |
| D45 | Model/client expansion adopted | `compresso-expand.md` architecture is in-plan as epic E13: canonical model protocol, protocol drivers + model profiles (profile-first rule), providers Kimi/GLM/MiniMax/DeepSeek after Anthropic/OpenAI, conformance suite. Spec: `docs/plan/07-model-expansion.md` |
| D46 | IP & secure deployment adopted | `ip.md` is in-plan as epic E14: crown-jewel boundary (locally required routing/policy is distributed without source but remains recoverable from the binary), release hardening, IPC auth, anti-tamper, legal program. Spec: `docs/plan/08-ip-and-secure-deployment.md` |
| D47 | Non-compression operations | Retrieval tools record `tokens_served`, `trace_json = NULL`; only compression operations carry trace v1 documents |
| D48 | Protection ingress rule | `protected` forced true for instruction/tool kinds on all external payloads; unprotect only via trusted local surfaces |
| D49 | Clean-room advanced capability expansion | Adopt the independently specified behavior in `compresso-addon-capabilities.md` as clean-room epic E15. No reference-product source, prompts, identifiers, assets, or obfuscated internals may be copied; Compresso contracts and fixtures are original. Spec: `docs/plan/09-advanced-capabilities.md` |
| D50 | High-impact tool boundary | Search remains read-only. Edit, SQL mutation, browser action, and terminal control use separate plan/apply tools, approved scopes, explicit permissions, bounded execution, audit records, and rollback where technically possible. Assist mode is default; native client tools are never disabled automatically |
| D51 | Typed removal ledger | Compression loss accounting uses typed `RemovalRecord` values with node/locator, hashes, and before/after token counts. Free-form notes cannot satisfy `removals_enumerated`; the safety check reconciles the exact delta without overlapping records |
| D52 | Future model expansion | E13 continues after initial Kimi/GLM/MiniMax/DeepSeek support with local and enterprise gateways, multimodal/realtime, embeddings/reranking, batch execution, evaluation-driven routing, a provider SDK, and self-hosted registry operations |
| D53 | Provider conversations are atomic graph structures | Protected `assistant_reasoning`, typed turn/tool identifiers, complete request/response pairing, and provider-profile validation before serialization. Unknown provider state is passthrough; an assistant turn cannot be removed independently from its tool loop |
| D54 | Non-empty input cannot compress to empty success | Generic text is segmented before packing. `minimum_output` is mandatory; inability to retain meaningful content returns byte-identical fallback |
| D55 | Differential token accounting and eager initialization | Strategies report changed node ids, counts recompute on mutation, and totals update by delta. Production strategies cannot repeatedly tokenize the full graph. Tokenizers initialize at server startup; cold and warm latency are separate gates |
| D56 | Local account client is a product boundary | E10 includes a concrete HTTPS `CloudClient`, device-code polling, refresh rotation, keychain storage, account CLI, logout/revocation, and offline degradation. Cloud endpoints alone are insufficient |
| D57 | Local mutations use base-bound plan/apply | Config, integration install, and uninstall expose plan/apply endpoints. Plans bind target `base_hash`, diff hash, expiry, and session; apply rejects drift |
| D58 | Provider-observed usage is not counterfactual savings | Store observed input/cache/output usage and actual cost separately. Savings remain modeled unless a controlled paired baseline was executed |
| D59 | Symbol and import ambiguity is explicit | Symbol lookup returns candidates and requires a selector. Imports record resolved, ambiguous, or unresolved status; impact reports unresolved counts |
| D60 | Migration numbers are centrally reserved | 0001 core, 0002 entitlements/account, 0003 model runtime, 0004 E15 capabilities, 0005 import resolution. Tickets cannot claim conditional numbers |
| D61 | Prompt-cache prefixes are immutable | Compression operates only on uncached suffixes after a cache epoch starts. Prefix changes create a visible new epoch and accounting separates observed cached tokens from modeled savings |
| D62 | Beta quality evidence uses previewed opt-in exports | D6 remains absolute. Closed-beta task success uses locally evaluated, content-free, user-previewed exports with paired randomized tasks and a preregistered confidence-bound gate |
| D63 | Full-product omissions belong to E16 | Compression routing/modes, remaining languages/integrations, HTTP MCP, missing desktop/cloud/admin/operations surfaces, fuzzing, and Linux ARM64 are E16 tickets rather than implicit backlog |
| D64 | Entitlement verification tolerates supported staleness | Clients carry previous/current/next keys; overlap covers offline grace plus the supported release window. Bounded skew applies to issuance/not-before only; expiry uses a monotonic anchor |
| D65 | Machine entitlement check cadence | Standard paid local features use a device-bound signed 21-day machine lease. Automatic renewal starts at day 14; failures retry at most daily with bounded jitter until hard expiry. Renewal requires device-key challenge proof and sends no content/usage telemetry. Entitled enterprise orgs may explicitly issue device-bound 30/60/90-day offline leases, renewing seven days before expiry; 90 days is the hard maximum and longer revocation exposure is disclosed |
| D66 | Client integrity is not authorization | Signed binaries, runtime manifests, local lease verification, stripping, and obfuscation are tamper resistance only and remain patchable by a machine owner. Client lease/integrity/feature claims never authorize cloud access; every paid cloud request is independently derived from current server subscription, org, seat, device, revocation, and capability state. Integrity failure is content-free/local-only and never blocks free operation or the assistant |
| D67 | Public package-manager beta | Authorize the dependency-ordered PB-01..PB-08 lane and `@compresso/cli@beta`: private engine source produces signed immutable macOS arm64/x64 and Linux x64 artifacts; a separate public auditable distribution repository verifies and bundles them with one stable POSIX launcher. The launcher directly execs the engine and creates no mutable or temporary state. Repository access in an MCP session is bound to one immutable canonical approved repository root selected only by explicit `--root` or approved cwd; a session with neither remains unbound and exposes only repository-independent tools. There is no sole-approved-repository fallback and tools cannot bind or switch roots. PB-02/PB-03 block release on repository binding, truthful provider disclosure, secret exclusions, repository management, `compresso setup CLIENT ROOT`, and the stable `COMPRESSO_LAUNCHER_PATH` upgrade contract. Native OS signing, npm OIDC provenance, staged public-repository approval, and independently verifiable artifact signatures define the channel controls; bundled SHA-256 detects corruption/tampering but is not an unpatchable trust anchor, and a full npm compromise can replace the launcher, engine, and checks. No install scripts, runtime Node.js requirement, licensing, cloud, desktop, or Windows support. Shipped binaries are recoverable and obfuscation is deterrence only. This beta authorization does not resolve D36 or permit the stable npm tag. ADR-0025. |
| D68 | Safe shell and generic-text Atlas packs | Extend E3 with non-executing shell indexing for `.sh`, `.bash`, and `.zsh`, plus inert bounded text indexing for `.md`, `.markdown`, `.txt`, `.rst`, and `.adoc`. Do not interpret shell, markup, links, includes, code blocks, config, or data formats. Retain every scanner/root/secret boundary; cap shell definitions and text records; stamp an explicit parser fingerprint so existing repositories re-index exactly once. ADR-0026. |
| D69 | Retrieval savings remain explicitly modeled | Persist exact retrieval pack tokens plus content-free repository attribution, estimated raw-source tokens, and modeled follow-up calls avoided. Applied compression savings remain separate. `total_effective_savings` combines applied compression with modeled retrieval only when both bases are visibly labelled. Historical rows without a recorded baseline claim zero retrieval avoidance. ADR-0027. |
| D70 | E17 local follow-ups | Add epic E17 (10 delivered tickets, plus open follow-ups E17-11..14 authorized 2026-11-14): compaction routing reminder, index-free local session recall, transcript-derived savings with a local price table, parallel Atlas scanning, Windows text/pipe hardening, MCP health probe, search diagnostics and all-terms multi-word search, doc–code drift lint, PR decision drafts (preview by default, no automatic post), resumable benchmark campaigns. Clean-room; everything stays local; no telemetry |
| D71 | Bounded MCP health probe (ADR-0028) | Accepted 2026-11-14. `compresso-cli`'s `mcp_probe` module is the only production source allowed to start a child process, and only for the running executable or a receipt-authenticated entry, with a scrubbed env, neutral cwd and process-group kill (TM-015) |
| D72 | Transcript savings and local price table (ADR-0029) | Savings can be derived from client transcripts; cost is priced once at the base input rate from a checked-in, versioned price table that is never fetched; unknown or unsafe model ids are never priced or echoed. Table rows are verified against the published pricing page before each table version |
| D73 | Public benchmark names in code | `swe-bench-verified` and `terminal-bench` may appear as campaign adapter names in code and docs: they name public benchmark suites, not third-party coding tools, so the product-name neutralization rule does not apply |
| D74 | One recall crate | E15-14..16 indexed recall extends the `compresso-recall` crate delivered by E17-02 rather than creating a second crate; the index-free path remains the default |
| D75 | Session recall on by default at install | Owner decision 2026-11-14. `compresso integrations install` and `compresso setup` write `--recall-source <own client>` into the Claude Code and Codex server entries, so `compresso_recall` is served without a manual flag. Each client gets only its own transcripts; Cursor gets none. `--no-recall` opts out. The flag lives inside the receipt-owned entry (previewed, hashed, reversible). Recall stays repository-scoped, read-only, redacted, index-free, and upload-free. The health probe launches a recall-bearing entry only with a matching receipt |
| D76 | Compaction reminder hook on by default in setup | Owner decision 2026-11-14, amending 04 §2.5. `compresso setup` installs the post-compaction routing-reminder `SessionStart`/`compact` hook for Claude Code and Codex as a second previewed plan with its own receipt; `--no-compaction-hook` opts out. The hook goes in user-level settings only (`~/.claude/settings.json`, `~/.codex/hooks.json`), never in a repository file such as `<repo>/.claude/settings.json`, so it is not committed and applies across the user's repositories; a receipted project-level hook from an earlier build is migrated away when setup or the hook install next runs for that repository, and an unreceipted one is left alone with a warning. Because the hook is user-wide, only the explicit `compresso integrations uninstall <client> --compaction-hook` removes it; uninstalling one repository's MCP entry does not. Existing user `SessionStart` hooks are never rewritten: the step is skipped with a warning and fix text and setup still exits 0. Cursor has no step. `compresso integrations install` keeps the hook behind `--compaction-hook`, because that command is one plan hash, one owned key, and one symmetric uninstall |
| D77 | Read ceilings for compression input and repository files | Owner asked 2026-11-15 to cap the remaining unbounded reads now (E17-16); the limits below were chosen in implementation and await owner confirmation. `compresso compress` reads at most 8 MiB (the engine input limit) from stdin or `--file` and rejects more with `input_too_large` before tokenizing, never echoing it (SEC-005 remediation); previously the CLI had no limit. Repository reads stop at 16 MiB (`MAX_READ_FILE_BYTES`) and fail with `file_too_large`, including line-range reads; `compresso_context` drops the body excerpt of such a file. MCP `compresso_compress` kept its echo-back of oversized input in this change; D79 replaces it |
| D79 | Oversized MCP compression input is refused, not echoed | Decided 2026-11-15 by the implementing session after the owner asked it to take the recommended action on the remaining SEC-005 items; awaits owner confirmation. `compresso_compress` refuses text over 8 MiB with the tool error `input_too_large` before building the context graph or counting tokens, and does not return the text. This narrows D38 for one case: fail-open exists so compression never loses or corrupts content, and here nothing is lost because the caller still holds the text it sent, while echoing it would double an input far beyond any model context and, as measured, the old path counted tokens twice first and could fail with `internal` when the tokenizer panicked. The limit and error code match the CLI (D77). Every other failure still fails open. Staleness checks hash files through a fixed 64 KiB buffer and treat a size that differs from the index as stale without reading |
| D78 | `doctor` health with warnings | Owner decision 2026-11-15. A warning state (`not_probed`, `config_modified`) never counts as healthy: the server report keeps `healthy: false`, and the check is shown as `[WARN]` with `severity: "warning"` and its fix hint. Warnings do not fail the command (`passed: true`, exit 0); only `severity: "error"` does. `healthy: true` means the server was launched and answered the probe |

## 6. Owner hard constraints (never override without new owner decision)

1. No telemetry enabled, ever, without explicit opt-in decision (D6).
2. No Claude Desktop bundle/integration (D5).
3. Source code and prompt content never leave the device by default.
4. Cloud/licensing failure must never break the underlying coding assistant.
5. All assistant-config changes: preview + timestamped backup + key-level receipt + reversible uninstall.
