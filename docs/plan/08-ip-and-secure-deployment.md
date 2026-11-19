# 08 — IP Protection and Secure Deployment (E14)

Source: `ip.md`, adopted as epic E14 by D46. This document translates that plan into the workspace, schemas, and interfaces defined in `01-stack-and-workspace.md`, `02-schemas.md`, and `03-interfaces.md`. It builds on ADR-0013 (signed releases and rollback) and ADR-0015 (cached entitlements with offline grace) and introduces ADR-0021 (crown-jewel local boundary) and ADR-0022 (selective obfuscation policy).

Objective, stated honestly: software that runs on the user's machine can eventually be disassembled, traced, and patched. E14 does not pretend otherwise. It (a) keeps the assets whose secrecy actually matters off-device, (b) raises the cost of extracting what must ship, (c) makes release authenticity and cloud authorization independent of client honesty, and (d) keeps the privacy-relevant boundary readable so users can verify our claims. Nothing in this epic may violate the owner hard constraints: no remote telemetry (D6), no Claude Desktop integration (D5), source never leaves the device by default, and no protection mechanism may ever break the user's coding assistant (fail-open, ADR-0008).

## 1. IP inventory mapped to the workspace

Maintain the inventory as `docs/security/ip-inventory.md` (E14-01): one row per asset with owner, classification (`crown-jewel` | `shipped-proprietary` | `auditable-public` | `brand`), storage location, protection method, and disclosure status. The authoritative first pass:

### 1.1 Crown-jewel assets (restricted source; compiled-only or server-only)

| Asset (ip.md §3.1) | Workspace location | Ships to device? |
|---|---|---|
| Compression strategy selection: registry ordering, `estimate()` confidence weights, mode→strategy mapping | `rust/compresso-core/src/strategy/` (`StrategyRegistry`, per-strategy files) | Yes, compiled only |
| Relevance/priority scoring: `pack_budget` priority-per-token ranking, protected-kind weighting | `rust/compresso-core/src/strategy/pack_budget.rs`, `engine` pipeline | Yes, compiled only |
| Context ranking: `repo_map` in-degree ranking, `plan_context` search-driven packing heuristics | `rust/compresso-atlas/src/queries.rs` | Yes, compiled only |
| Model-routing policies and model profiles (E13 profile-first rule) | `rust/compresso-proxy/` policy modules + profile data (`07-model-expansion.md`) | Static profiles compiled; learned routing server-side only |
| Reviewer synthesis/tuning logic and historical evaluation methods | `rust/compresso-eval/` runner internals; future cloud reviewer services | Runner compiled; tuning services server-side only |
| Strategy performance data, training/evaluation result corpora | `fixtures/` outputs, eval reports, private storage | Never shipped |
| Organization knowledge ranking | Cloud services (`apps/control-plane`, post-slice) | Never shipped |
| Abuse-detection and entitlement issuance policies | `apps/control-plane` domain layer | Never shipped; clients hold only public verification keys (03-interfaces §9) |

Preferred protection: trade-secret handling (§17, §19 of this doc), restricted repository access, server-side execution where local-first privacy permits, and — for the parts that must run locally — the hardening in §4 and the obfuscation policy in §5. See ADR-0021 for the boundary rationale.

### 1.2 Shipped proprietary assets (closed distribution, signed, stripped)

`compresso-core`, `compresso-atlas`, `compresso-store`, `compresso-mcp`, `compresso-agent`, `compresso-proxy`, `compresso-updater` (as compiled parts of the `compresso` binary and the desktop app), plus `apps/desktop` (Tauri). Protection: native compilation, the `dist` profile (§4), signed artifacts (ADR-0013), proprietary license terms (§19), selective obfuscation only per §5.

### 1.3 Auditable public boundary (keep readable; never obfuscate)

These carry little differentiating logic and benefit from scrutiny — they are how users verify our privacy and safety claims:

- `schemas/` — every versioned JSON Schema: context-graph, compression-trace, local-api, cloud-api allowlists (02-schemas).
- MCP tool contracts and server instructions (`04-mcp-and-integrations.md` §1; D7 tool surface).
- `installers/` — `install-local.sh` now; `install.sh`, `install.ps1`, Homebrew formula in E12-07.
- `rust/compresso-integrations/` — adapters, key-level diffs, receipts, uninstall/restore logic (owner constraint 5 depends on this being inspectable).
- Release signature and update-metadata verification logic in `compresso-updater` (verification, not the update orchestration internals).
- Entitlement verification contract (`EntitlementVerifier`, 03-interfaces §9) and the entitlement bundle format.
- Privacy enforcement documentation, trace format ("no raw content" rules), and uninstallation docs.

Candidate for source publication after launch review: `compresso-integrations` and `installers/` as a public repository. Until then they remain source-available on request in the private repo but are treated as public-quality (no secrets, no crown-jewel imports — enforced by the crate dependency rule in 01-stack §3).

### 1.4 Brand assets

Compresso name (trademark search is D36, OPEN), domains, package identifiers (crates.io, npm, Homebrew tap, PyPI-defensive), logo and visual identity, product copy. Actions: complete searches before public launch, register primary domains plus defensive variants, reserve package/social identifiers, record asset ownership, use consistent © and ™ notices. Tracked in E14-14.

## 2. Threat model summary

Adversaries (ip.md §4.1): casual copiers; competitors inspecting behavior; skilled reverse engineers on native binaries; users patching local entitlement checks; malicious mirrors or update infrastructure; compromised CI or developer accounts; insiders; compromised dependencies; release impersonators.

Protected outcomes (§4.2): users can verify authentic releases; malicious updates are never accepted; the cloud rejects forged entitlement and device claims; no local secret appears in binaries or logs; release hardening measurably deters casual extraction without promising confidentiality against a device owner; a leaked client binary exposes no signing or service credential; a leaked low-privilege repo does not expose all crown-jewel logic; security holds even when implementation details are known.

Explicit limitations — accepted, not fought: a local user can always disassemble the executable, capture decrypted data from memory, trace file and network activity, patch local-only licensing checks, and reimplement public protocol behavior. Compresso therefore concentrates its strong controls on release authenticity (ADR-0013), cloud-side authorization (§10–§11), trade-secret handling (§17), and architectural separation (ADR-0021) — never on client-side obscurity. Local entitlement checks are a convenience, not a security boundary (ADR-0015).

## 3. Deployment and package boundaries

### 3.1 What never ships client-side

Entitlement signing keys and all issuance logic; abuse-detection thresholds and models; learned routing/reviewer-tuning services; organization knowledge ranking; strategy performance corpora; release-administration credentials. These live only in `apps/control-plane` and its private infrastructure (§9). Cloud features requiring code or context upload must be optional, explicit, narrowly scoped, and documented field by field — and are outside the mandatory local compression path (ADR-0003).

### 3.2 What runs locally by necessity

Everything in the local-first promise: repository contents, Atlas indexes, search/read, edits, SQL, session transcripts and recall, deterministic compression, operation traces, secret detection. Because source never leaves the device (owner constraint 3), the strategy-selection and ranking logic that operates on that source must also run locally — this is the unavoidable exposure ADR-0021 accepts and mitigates.

### 3.3 Package layering

Per ip.md §6, mapped to the workspace: (1) auditable bootstrap installers (`installers/`); (2) thin adapters (`compresso-integrations`) owning config translation, MCP registration, health checks, and uninstall receipts — never compression, search, edit, SQL, recall, or review algorithms; (3) the closed native engine behind the versioned MCP/local-API protocol; (4) the Tauri frontend, presentation-only (§6). Enforced by the crate dependency rule (01-stack §3) and by review: any PR moving algorithmic logic into `compresso-integrations`, `installers/`, or `apps/desktop` webview code is rejected.

### 3.4 Bootstrap installer trust chain

The installers must be small enough to read in one sitting and must only: detect platform/arch → fetch signed release metadata → download an immutable, versioned artifact → verify signature and digest (the slice installer verifies the keyless-signed checksum manifest, ADR-0030; its binary carries no native signature, D42) → install atomically → print explicit next steps. They must not contain proprietary algorithms, collect anything, sign in automatically, modify assistant configuration automatically, or fetch mutable unsigned second-stage code. `install-local.sh` (slice) already conforms; native platform signatures arrive with E12-01/E12-02.

## 4. Rust release hardening

Add a `dist` profile to the workspace `Cargo.toml`, used only by `release.yml` (CI and dev builds keep the default `release` profile so gate benchmarks stay comparable):

```toml
[profile.dist]
inherits = "release"
opt-level = 3
lto = "fat"
codegen-units = 1
debug = 2                    # full debuginfo generated...
split-debuginfo = "packed"   # ...split out (dSYM / .dwp / .pdb)
strip = "symbols"            # ...and stripped from the shipped binary
incremental = false
panic = "unwind"             # deliberate deviation from ip.md §7 — see below
```

`panic = "abort"` (ip.md's recommendation) is rejected: fail-open (ADR-0008, D38) requires that a panicking strategy or parser degrades to passthrough inside `Engine::compress` and that the MCP server survives a bad request. The engine wraps strategy execution in `catch_unwind`; `abort` would turn a single malformed payload into a dead MCP server mid-session. Tamper resistance is not worth breaking the user's assistant.

Release builds additionally set `RUSTFLAGS="--remap-path-prefix=$CARGO_WORKSPACE_DIR=/compresso --remap-path-prefix=$HOME=/~"` so no developer path or username appears in panic locations or debug info.

Controls checklist (E14-02, E14-03):

- Split debug artifacts uploaded to private symbol storage in `release.yml`; never attached to public releases. Crash output is symbolicated by developers against the private archive; there is no remote crash reporting to symbolicate for (D6).
- Production panic hook in each binary: logs `internal error <version> <build-id> at <remapped location>` to the redacted local log; never formats node content, paths under user repos, or SQL into the message.
- Descriptive internal assertions use `debug_assert!`; `assert!` messages in `dist` builds must not describe algorithm internals or include user data (clippy lint + review checklist).
- `#[no_mangle]`/`pub extern` surfaces minimized; no exported symbols beyond `main` and Tauri command glue.
- Build identifiers derived from the version + content hash — no repository URLs, commit author names, or CI credentials embedded.
- No fixtures, prompts, design documents, or source maps in any artifact (`release.yml` packaging allowlist, verified by an artifact-contents test).
- Crown-jewel modules stay in the crates/files listed in §1.1 so §5 obfuscation and repo restrictions (§17) can target them precisely.

Stripping and LTO raise analysis cost; they do not make algorithms secret. That honesty is load-bearing for every later decision.

## 5. Selective obfuscation policy

Full policy in ADR-0022. Summary: obfuscation is a last, optional, evidence-gated layer — permitted only after the performance baselines (D37 gates) and support processes exist (ip.md §28 step 12), and only after profiling, compatibility testing, and security review of the obfuscated build.

Eligible (crown-jewel modules from §1.1 only): strategy registry ordering and estimate weights, `pack_budget` scoring, `queries.rs` ranking, proxy model-selection rules, and non-security entitlement *presentation* strings. Techniques, in escalating order: symbol renaming beyond what stripping gives, string-literal protection for tuning constants, constant splitting, selective control-flow transformation, commercial LLVM-based transformation of the specific crates — each adopted only if the eval gates (P95 < 200 ms, reduction > 25%) still pass on the obfuscated `dist` build.

Never obfuscated: everything in §1.3, plus signature verification, cryptographic code, installer/updater trust logic, privacy controls and redaction, secret detection, uninstaller/restore, crash recovery, and data deletion. Obfuscation must never prevent vulnerability response, reproducible-build verification where required, or internal audit.

## 6. Tauri and frontend hardening

The desktop app (D24, ADR-0010, E9) contains presentation logic only; ranking, entitlement evaluation, and strategy logic never appear in JavaScript or webview-reachable state. Requirements (E14-05):

- Devtools disabled in `dist` builds; restrictive CSP (no remote origins — the app talks only to the local API).
- Tauri capability allowlist: only the explicit commands the UI needs; no generic fs/shell/http primitives exposed to the webview.
- Every frontend→Rust command validates its input; privileged behavior stays in Rust commands with capability checks.
- The local-API bearer token is read by the Rust side from `~/.local/state/compresso/api-token` and attached to requests in Rust; it is never handed to the webview, never in `localStorage`.
- No private source maps in public artifacts; desktop updates are signed and verified before install (shared updater trust logic, §14).

Frontend minification may deter casual copying; it is explicitly not a protection for crown-jewel logic, which is not present in the frontend at all.

## 7. Local IPC security

The local API spec (03-interfaces §7, E8-02) is the baseline: UDS + localhost HTTP, bearer token at `~/.local/state/compresso/api-token` (0600, generated at first start), stdio for assistant-launched MCP. E14-04 adds the remaining ip.md §10 controls as tested requirements:

- Socket and token files owned by the current OS user with user-only permissions; UDS path under `~/.local/state/compresso/` (0700 directory).
- Localhost listener binds `127.0.0.1`/`::1` only, random or configured private port; requests without a valid bearer token get 401 with no detail.
- Origin validated (reject browser origins) and Host-header confusion rejected; no CORS allowances at all — the API is for local trusted clients, not web pages.
- Protocol/schema version negotiation (`schema_version` on every payload, 02-schemas); unknown versions fail open, never crash.
- Request-size, concurrency, and per-method timeout limits; explicit cancellation semantics.
- Method-level authorization: config writes require E8-06; integration install/uninstall require E8-07; read endpoints never return raw content (traces only).
- Approved-root path authorization reuses the E3-11 canonicalization boundary for every path that arrives via API or MCP.
- MCP tools are read-only except one opt-in write tool (E17-32, D97, ADR-0035). `compresso_edit` is served only when the operator starts the server with `--edits` (installers write it only on `--edits`); without the flag the tool is neither listed nor dispatched. Threat model: repository content is untrusted and may steer the model to request edits, so the tool bounds what an edit can reach and do rather than judging intent: only indexed, non-secret, non-gitignored UTF-8 files of at most 1 MB under the approved root; never `.git`, a known control file (ignore files, agent instructions such as `CLAUDE.md` and `AGENTS.md`, agent, editor, CI, development-container, and git-hook configuration; the full case-insensitive list is in ADR-0035; a denylist, not a guarantee: build scripts stay editable and the client's permission prompt governs them), a read-only file, the store's database files (or the store directory when it lies inside the repository), a path with a symbolic-link component, or a new file; literal replace, whole-identifier rename, and anchored insert only, with no regex, shell, or network. Every change is reported with its lines and a rename with its remaining matches. Edited bytes and original copies are staged and synced before any target changes; each target is re-verified (device, inode on Unix, content hash) immediately before its rename; permissions, line endings, and BOM are kept; a failed rename restores the files already renamed from their staged originals. Residual risks: a directory component swapped in the instant between the last check and the rename (no handle-relative rename in the standard library), non-Unix builds without the device and inode check, and ownership and extended attributes not carried to the replacement file.
- Peer credential checks (SO_PEERCRED / getpeereid / named-pipe SIDs) where the platform provides them.
- Structured, redacted audit records in the operations ledger — never raw prompts or source (02-schemas §4 rules).

## 8. Strategy and policy distribution

Post-slice, Compresso may ship strategy packs (tuned orderings, weights, log patterns, model profiles) out of band from binary releases:

```text
strategy-pack-v<N>.bin  +  .manifest  +  .sig
```

Manifest fields: pack version, min/max compatible client versions, release channel, issued/expires timestamps, blake3 artifact digest, signing key id, rollback policy. The native client verifies the signature (same embedded-public-key + `kid` rotation pattern as entitlements, ADR-0015) before loading; an invalid, expired, or version-incompatible pack is ignored and the built-in compiled defaults are used — fail open, never fail closed. Org policy sets are already covered: signed `policy_sets` JSON from `GET /v1/orgs/:id/policies` (03-interfaces §8, 02-schemas §5).

Hard limitation, stated plainly: a downloaded pack can be dumped after loading. Packs are a tamper-resistance and operational-update mechanism, not a secrecy mechanism. Anything that must stay secret (learned routing, abuse thresholds) stays server-side (§3.1); packs carry only data we accept becoming readable. This bounds what E13's model profiles may contain when distributed as packs.

## 9. Cloud deployment security

Baseline already specified: Fastify on Fly.io private networks, managed Postgres, schema and privacy enforcement in 02-schemas §5, API contract in 03-interfaces §8, built as E10. ip.md §12 adds these requirements to E10/E14:

- TLS terminates at the managed ingress; API containers and Postgres/Redis on private networks; database ingress restricted to application and migration identities; separate dev/staging/production Fly organizations; no public database access; egress controls on sensitive workers where practical.
- All input/output validated against the versioned `schemas/cloud-api/` allowlists (already mandatory per 02-schemas §5); authorization enforced in the domain layer, not only in routes; rate limits by account, device, org, and IP; idempotency keys on billing and device operations; Stripe webhook signatures verified (E10-03); immutable admin audit events (append-only `audit_logs`, E10-06); support permissions separated from release administration; background jobs with bounded retries and dead-letter handling.
- Encrypted storage and backups; least-privilege database roles; minimal retained device/event metadata; tested backup restoration; data export and deletion workflows. The control-plane database never stores source code, prompts, paths, repo names, symbols, SQL, or patches (D30) — this is also an IP control: our cloud cannot leak what it never holds.

## 10. Authentication and device identity

As specified in 03-interfaces §8 and E10-02, per ip.md §13: browser OAuth/OIDC (Better Auth, D29) for interactive users; device-code flow for CLI/desktop; device keypair generated locally with the private key in the OS keychain (`keyring`, ADR-0015) and only the public key registered; 15-minute access tokens with rotating refresh tokens bound to the device keypair; per-device revocation; stronger authentication (hardware-backed MFA) required for organization and release administrators; enterprise SSO without moving any local source data into the identity system. E14 adds no new mechanism here — only the review gate that no client credential ever appears outside the keychain (E14-04 tests, §12 scanning).

## 11. Entitlements and licensing anti-abuse

ADR-0015 and 03-interfaces §9 / 02-schemas §4 define the mechanism: device-bound signed JWS machine leases, embedded previous/current/next public keys, renewal beginning at day 14, hard expiry at day 21, challenge proof from the keychain-held device key, `entitlement_cache`, and every terminal failure degrading to `local_only()`. E14 confirms the anti-abuse stance on top of it:

- The server signs; clients only verify. Entitlement issuance policy, plan mapping, and abuse detection (unusual device counts, refresh patterns, token replay) run server-side against `devices`/`device_sessions` and are never shipped.
- Cloud-only features are enforced by shared server middleware on every request using current subscription, organization membership, seat, device, revocation, and capability records. The server ignores client lease contents, client integrity claims, requested feature names, and UI state as authorization inputs. Local paid-feature checks are UX, and we assume determined users can patch them (ADR-0021/D66).
- Automatic licensing traffic occurs only when the lease is due: no request before day 14, then at most one retry per 24 hours with bounded jitter until day 21. The request allowlist contains device/lease identifiers, client and entitlement versions, nonce, and device-key proof only — never source, prompts, repository metadata, paths, operation history, or usage statistics. This traffic is not telemetry and cannot be repurposed as telemetry under D6.
- Enterprise offline is a distinct server-issued lease class, never a local setting: entitled organizations may choose exactly 30, 60, or 90 days for one registered org/device, renewal begins seven days before expiry, and 90 days is a hard protocol maximum. The UI must disclose the selected revocation-exposure window before issuance. Standard users remain on D65's 14/21 cadence.
- No invasive DRM, debugger detection, hardware fingerprinting beyond the registered device key, or destructive anti-tamper — ever (§20, owner constraint 4).
- Local/free features work forever without an account (ADR-0011); paid entitlements never interrupt an active operation (ADR-0015).

## 12. Secret management

Never in source, binaries, containers, or logs: code-signing keys, entitlement signing keys, database credentials, cloud provider credentials, confidential OAuth secrets, billing secrets, admin API keys. Concrete requirements (E14-06, mostly landing inside E12):

- Production secrets in the platform secret manager (`fly secrets` for runtime; GitHub OIDC → cloud KMS for CI — no long-lived CI cloud keys).
- Release-signing and entitlement-signing keys in KMS/HSM, non-exportable; signing permission separated from deployment permission; documented rotation procedures for both (entitlement rotation already designed: current+next `kid`, ADR-0015).
- Short-lived credentials preferred everywhere; gitleaks already scans the repo (E1-03) — extend scanning to release artifacts, container layers, and CI logs in `release.yml` (E12-02 gate).
- Local user credentials only in the OS credential store (`keyring`); redaction happens before any log or diagnostic file is written (tracing redaction layer, 01-stack §1), not just before export.

## 13. Build and release pipeline security

E12 owns the pipeline; E14 supplies the requirement list `release.yml` must satisfy (mapped to tickets):

- Protected branches, mandatory review, restricted workflow-modification permissions; ephemeral runners for release builds; CI cloud auth via OIDC only (E12-01, E14-07).
- Two-person approval for stable-channel publication (GitHub environment protection) (E12-01).
- Immutable artifacts and tags; committed lockfiles (`Cargo.lock`, `pnpm-lock.yaml`); `cargo audit` + `cargo deny` already gate CI (E1-03).
- SBOM (CycloneDX via `cargo cyclonedx` + pnpm SBOM) and SLSA-style provenance attestation for every artifact (E12-01).
- Malware/vulnerability scan of artifacts; separate stable/beta/nightly/enterprise channels (02-schemas §5 `release_channels`).
- macOS Developer ID signing + notarization + stapling; Windows Authenticode; signed Linux archives and checksums (E12-03, E12-04, E12-05; ADR-0013).
- Private debug-symbol upload (§4) as a release step; artifact-contents allowlist test (no fixtures/prompts/maps) (E14-10).
- Eval release gates (`gates.toml`, E7-03) are a hard precondition of publishing, run against the `dist` build.

## 14. Secure update system

E12-06 implements the updater; E14-08 upgrades its metadata to a TUF-style hierarchy (root → timestamp → snapshot → targets) so that a single leaked CDN or metadata key cannot forge accepted updates. The updater verifies: trusted root keys, signature threshold, metadata expiration, version monotonicity (rollback protection), artifact digest, platform/arch match, and compatibility constraints — then downloads to a private staging directory, verifies again, checks disk/compatibility, stops affected components safely, installs atomically, runs migrations, health-checks, and rolls back automatically on failure (ADR-0013, `update_state` table). At least one known-good previous version is always retained. A tampered manifest being refused is an existing release-gate test (E12-06); E14-08 adds threshold-signing and metadata-expiry tests. Trust-root rotation is exercised as part of the signing-compromise runbook (§21).

## 15. Telemetry and diagnostics stance

There is no remote telemetry. None is built, none is enabled, and no code path in slice or post-slice clients calls `POST /v1/events` (D6, ADR-0012 — owner hard constraint; any change requires a new owner decision). ip.md §18's "off by default" posture is therefore strengthened to "does not exist":

- All stats are local: the `operations` ledger, `compresso stats`, and the local API/desktop dashboards (02-schemas §3 vocabulary).
- Diagnostics are local and user-exported only: `compresso doctor` output and a diagnostics bundle are written to a local file at the user's explicit request; the user inspects and transmits it themselves. Bundles contain redacted logs and traces only — no source, prompts, paths beyond the config's own, symbols, patches, SQL, or tool arguments (trace rules, 02-schemas §2).
- Redaction happens before persistence, not before export; the `event_batches` cloud table and telemetry allowlist schema remain specified-but-unbuilt so a future opt-in decision has a reviewed contract to target.
- Transparency here is deliberate IP strategy: we do not hide the telemetry boundary to protect algorithms. An inspectable no-upload posture builds the trust that closed local binaries spend.

## 16. Local data protection

Per ip.md §19, largely already specified: account/device credentials in the OS keychain (ADR-0015); local DB, config, token, and log files restricted to the current user (0600/0700); raw session recall disabled until explicitly enabled; 90-day operation retention with pruning (02-schemas §4); transactional migrations (E3-01); no raw prompts or source in the operations ledger; backups and config backups (`integration_receipts.backup_path`) carry the same permissions as originals; every read/write path canonicalized and root-checked (E3-11). E14 adds: optional synchronized knowledge (post-slice, if ever built) is encrypted client-side before upload, and deletion commands (`compresso reset`, repo removal) are verified to delete index and ledger rows, not merely unlink files.

## 17. Repository and contributor controls

The repo is currently local-only with a single owner (D32); these controls activate when a remote and contributors exist (E14-12):

- Crown-jewel crates/paths (§1.1) restricted to engineers who need them — via a private engine repository or GitHub CODEOWNERS + branch protections and, if needed, a split repo; adapters/installers/schemas may live at broader visibility (§1.3).
- Hardware-backed MFA required for the org; owners/admins reviewed quarterly; dormant accounts disabled promptly; access to sensitive repos and release systems logged.
- Signed contributor agreements or employment/contractor IP assignment before first commit; prior-invention disclosure; confidentiality obligations surviving departure where enforceable; secure offboarding with credential rotation.
- Production datasets and strategy performance corpora never on developer laptops by default.

Trade-secret status requires operational evidence: restricted access, classification labels in the IP inventory, NDAs, and consistent handling. E14-12 delivers the written handling policy.

## 18. Open-source compliance

`cargo deny check licenses` already gates CI (E1-03). E14-11 extends it into a full program: machine-generated dependency inventory (Rust + TS) reviewed each release; MIT/BSD/Apache-2.0 allowed with notice compliance and Apache-2.0 patent terms preserved; LGPL requires linking/modification review; GPL requires distribution-model review; AGPL requires legal review before any use — CI fails when an unapproved license enters a release dependency graph (both `cargo deny` and a pnpm license check). A generated `THIRD-PARTY-NOTICES` file ships in every artifact. Clean-room rule (D2): no code, prompts, or assets copied from the predecessor CLI or the reference product; public behavior is reimplemented from our own specs and fixtures, and reviews enforce it.

## 19. Legal protection program

With qualified counsel (E14-14; this plan is not legal advice): confirm ownership of all source and design assets; employee/contractor IP assignments; trade-secret inventory and handling policy (§17); copyright registration where useful; trademark search and filing for Compresso and the primary marks (resolves D36); patentability review of genuinely novel compression/routing/evaluation methods before public disclosure; proprietary EULA and enterprise terms prohibiting unauthorized redistribution and circumvention where lawful; an infringement reporting and response process. Launch is blocked on D36 resolution and license publication (§22).

## 20. Anti-tamper boundaries

Reasonable controls, all fail-open: OS code-signature verification of our own binaries; a release-signed runtime-integrity manifest covering executables and paid resources (E14-10); signed strategy packs (§8); protocol version and integrity checks; automatic repair or safe refusal for corrupted installations. Integrity failure finishes the active operation, disables paid local features, records only a content-free local status, and leaves free compression/MCP plus the user's assistant working. These checks deter casual modification and detect accidental corruption; because their verifier also runs locally, they are explicitly not an authorization boundary (D66).

Prohibited, permanently: destructive responses to detected modification; rootkit-like persistence; aggressive debugger detection; certificate pinning that blocks safe rotation; silent collection of reverse-engineering indicators (impossible anyway under D6 — there is no channel to collect on); and treating any local anti-tamper check as an authorization control. Authorization lives server-side (§11).

## 21. Incident response

Runbooks under `docs/runbooks/` (E14-13, delivered with E12-09): signing-key compromise, malicious release publication, source repository breach, insider exfiltration, dependency compromise, leaked cloud credentials, entitlement-signing compromise, counterfeit distribution, user-data exposure. The signing-compromise runbook must support: immediate release freeze; key revocation or trust-root rotation (§14); CDN/package withdrawal (release `yanked_at`, 02-schemas §5); customer notification; known-good recovery release; forensic preservation; root-cause and control review. Entitlement-signing compromise is bounded by design: promote the pre-published `next` key to `current`, retain the prior key only for the defined overlap window where it is not compromised, reissue affected machine leases, retire the compromised key, and rely on the 21-day hard lease expiry to bound offline exposure (ADR-0015). Signing-compromise and rollback runbooks are exercised (tabletop plus a real staging drill) before stable launch.

## 22. Release acceptance gates (merged)

A production (stable-channel) release is blocked unless all of the following pass. Sources: existing gates (D37, `gates.toml` E7-03, ADR-0013, E12) merged with ip.md §26. Measurable engine gates stay in `gates.toml`; pipeline gates are enforced by `release.yml`; the rest are release-checklist items with named owners.

| # | Gate | Source |
|---|------|--------|
| G1 | `median_reduction_eligible > 0.25`, `p95_latency_ms < 200`, `protected_mutations == 0`, `malformed_json == 0`, recall-probe 100%/≥99% — on the `dist` build | D37 / E7-03 |
| G2 | MCP startup < 500 ms | D37 |
| G3 | Every artifact verifies against signed metadata; tampered manifest refused; update rollback passes on macOS, Windows, Linux | ADR-0013 / E12-06 |
| G4 | No production secret in source, binaries, containers, or logs (scanners green, §12) | ip.md §26 |
| G5 | SBOM and provenance generated for every artifact | E12-01 |
| G6 | Critical/high dependency findings resolved or formally accepted | E1-03 / ip.md §26 |
| G7 | Local IPC authentication and permission tests pass (§7) | E14-04 |
| G8 | Claude Code, Codex, and Cursor uninstall/restore tests pass | E6-05 |
| G9 | No remote telemetry code path exists in the shipped build; diagnostics local/user-exported only | D6 / §15 |
| G10 | Privacy schema tests pass: no prohibited fields writable in cloud JSONB, traces contain no raw content | 02-schemas §5 |
| G11 | Cloud-outage test: all local features operational with the control plane down | ADR-0011 / E10-08 |
| G12 | Third-party notices complete; no unapproved license in the dependency graph | §18 |
| G13 | Artifact-contents allowlist passes: symbols stripped, private symbols archived, no source maps/fixtures/prompts | §4 |
| G14 | Security testing matrix executed; external review has no unresolved critical/high findings | E12-08 |
| G15 | If obfuscation is enabled: all above gates re-run on the obfuscated build | §5 / ADR-0022 |

## 23. E14 ticket seeds

One-liners for expansion in `05-tickets.md`. "In E12" means the work lands inside the named E12 ticket; standalone tickets get E14 numbers. Ordering follows ip.md §28.

| Seed | Title | Owner | Depends | Home |
|------|-------|-------|---------|------|
| E14-01 | Crown-jewel IP inventory and classification doc (`docs/security/ip-inventory.md`) | I1 + PQ | E1-01 | E14 |
| E14-02 | `dist` Cargo profile, path remapping, private symbol archive step | I1 | E1-02 | E14 (consumed by E12-01) |
| E14-03 | Panic-hook and release assertion/message policy + clippy lint | R1 | E14-02 | E14 |
| E14-04 | Local IPC security test suite (token/socket perms, 401, Origin/Host, peer creds, limits) | I1 + R2 | E8-02 | E14 |
| E14-05 | Tauri hardening: CSP, capability allowlist, devtools-off, token isolation from webview | D1 | E9-01 | E14 |
| E14-06 | Secret-management setup: KMS/HSM signing keys, OIDC workload identity, artifact/log secret scanning | I1 | E1-03 | in E12-02 |
| E14-07 | SBOM + provenance generation and verification in `release.yml` | I1 | E12-01 | in E12-02 |
| E14-08 | TUF-style update metadata (root/timestamp/snapshot/targets), threshold signing, rotation test | I1 | E12-06 | E14 (extends E12-06) |
| E14-09 | Signed strategy-pack format, manifest schema, and fail-open client verifier | R1 + I1 | E12-01, E2-09 | E14 |
| E14-10 | Artifact-contents allowlist test (no fixtures/prompts/source maps/symbols) as release gate | I1 | E14-02, E12-01 | E14 |
| E14-11 | License-compliance program: pnpm license gate, THIRD-PARTY-NOTICES in artifacts, AGPL policy | I1 | E1-03 | E14 |
| E14-12 | Repository/contributor controls and trade-secret handling policy (activates with remote repo) | I1 + PO | E14-01 | E14 |
| E14-13 | IP/security incident runbooks incl. signing- and entitlement-key compromise drill | I1 + PQ | E12-06 | in E12-09 |
| E14-14 | Legal program checklist: trademark search (closes D36), IP assignments, proprietary EULA | PO + LEGAL | E14-01 | E14 |
| E14-15 | Obfuscation evaluation spike: candidate techniques on §1.1 modules, gate re-run, go/no-go ADR update | I1 + R1 | E12-08, E7-04 | E14 (last, per ip.md §28.12) |
| E14-16 | Merge §22 gate table into `gates.toml` + `release.yml` enforcement and release checklist | I1 | E7-03, E12-01 | E14 |

## 24. E15 high-impact capability hardening

E14-17 owns the threat model for edit, SQL, recall, review, browser, and terminal boundaries. E14-18 owns reusable sandbox profiles and adversarial escape tests. Browser and terminal cannot ship enabled until these gates are enforced:

| Gate | Requirement |
|---|---|
| G16 | Write and SQL plan/apply TOCTOU, approval-binding, rollback, and injection tests pass |
| G17 | Recall/review encryption, deletion, provenance, and support-bundle exclusion tests pass |
| G18 | Browser/terminal sandbox escape, credential isolation, destination, and resource-limit tests pass |

The crown-jewel inventory includes review prompts, routing/evaluation weights, fuzzy-match policy, SQL classification policy, and knowledge ranking. Public schemas, permission labels, installer behavior, and rollback verification remain auditable and unobfuscated.


## 25. Authoritative ticket-reference map

This map supersedes older inline numbers left by pre-audit drafting: release pipeline E12-01; SBOM/provenance E12-02; macOS signing E12-03; Windows signing E12-04; Linux signing E12-05; updater/rollback E12-06; installers/Homebrew E12-07; security review E12-08; beta/runbooks/launch E12-09; TUF metadata E14-08; strategy packs E14-09; artifact gate E14-10; shared sandbox policy E14-18; browser sandbox E14-19; terminal sandbox E14-20. E14-06, E14-07, and E14-13 remain requirement aliases absorbed by E12-01/E12-02/E12-09 and are not standalone backlog tickets.
