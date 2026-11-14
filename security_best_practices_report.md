# Compresso Defensive Security Audit

Assessment date: 2026-07-22

Assessment target: current working tree at `/Users/yogi/compresso`

Assessment posture: defensive, read-only review of the complete implemented E1-E7 application

Distribution status: private repository; no releases or uploaded Actions artifacts; known binaries and installer artifacts are local only

Remediation update: **2026-07-23. SEC-003 is fixed in the current working tree, pending commit/release.** (Superseded: committed and on `main`; see the 2026-11-14 table below.) Public plan serialization and CLI output now expose only a secret-safe, key-level preview; exact target bytes remain private to integrity hashing and authenticated crash recovery. This prevents future preview disclosure but cannot invalidate any credential exposed before the fix, which must still be revoked or rotated separately. The finding counts below describe the original 2026-07-22 assessment snapshot.

Remediation update: **2026-11-14.** Status of the findings below after the E17 review fixes. The finding sections keep their original 2026-07-22 text.

| Finding | Status | Note |
|---|---|---|
| SEC-001 | Fixed for excluded content | `compresso_read` refuses a file the scanner excludes (repository `.gitignore`, built-in excluded directories, secret patterns) unless it is indexed, judged on the resolved path so links and casing cannot bypass it. A file the scanner would accept stays readable before it is indexed. The built-in secret pattern list is unchanged. |
| SEC-002 | Fixed | Each MCP server is bound to one repository root at startup; `test_bound_root_retarget_cannot_leak_another_approved_repository` covers it. |
| SEC-003 | Fixed and committed | The 2026-07-23 remediation described below as "in the working tree, pending commit" is on `main`: plans and CLI output carry only the key-level preview, and the bearer-canary tests in `rust/compresso-integrations/src/codex.rs` and `plan.rs` pin it. Credentials exposed before the fix still need rotation. |
| SEC-004 | Fixed | Turbo upgraded to 2.9.14; CI installs the locked TypeScript dependencies, runs the workspace checks, and audits them on every pull request. |
| SEC-005 | Partly fixed | Transport framing is bounded: each incoming MCP message is capped at 64 MiB (`MAX_INCOMING_MESSAGE_BYTES`, `rust/compresso-mcp/src/framing.rs`). A longer line is discarded as it streams, answered with JSON-RPC `-32600`, and the session continues. The other evidence items in the finding (compression graph built before the 8 MiB check, whole-file reads before truncation, unlimited internal read budgets) are unchanged, and `compresso compress` still reads all of stdin or `--file` into memory. |
| SEC-007 | Fixed in code, not yet exercised on a real release | The installer accepts only `https://` sources and does not leave HTTPS on a redirect. The main-branch CI job signs `SHA256SUMS` with keyless Sigstore, and the installer requires a valid `SHA256SUMS.sigstore.json` from the pinned `ci.yml@refs/heads/main` identity for every install (ADR-0030). Only a local source may skip it, and only with the explicit `--allow-unsigned` flag meant for self-built artifacts. The signing step has not run on `main` yet and verification has only been tested against a `cosign` stand-in. Rollback to an older signed release is not prevented. |
| SEC-008 | Fixed at the default location | On Unix the store creates its directories 0700 and its database 0600. Opening the default store also removes group and other access from an existing `compresso` data directory, database, WAL, and shared-memory file (bits are only removed, symbolic links are skipped, the parent of the data directory is untouched). This runs on the commands that open the default store (`index`, `repos`, `stats`, `decisions`, `setup`, `mcp serve`); `test_ordinary_command_tightens_a_store_left_readable_by_an_earlier_release` covers it end to end. `doctor` and the integration receipt projection open the database by explicit path and do not tighten it themselves, and the store default ignores `XDG_DATA_HOME` where the runtime paths honour it, so a store under a custom data home is not retrofitted. A database opened at an explicit path keeps its existing permissions; ownership and symlink policy are still not enforced. |
| SEC-009 | Partly fixed | Every third-party action is pinned to a commit SHA and the CI matrix checker enforces it. The actionlint container image is still referenced by tag. |
| SEC-010 | Fixed | `.gitleaks.toml` allowlists only `fixture-NNNN` values of the `generic-api-key` rule in `fixtures/benchmarks/json-2000-elements/input.json`. CI proves the canary is detected, proves the allowlist does not cover another value in that file, then requires a clean repository scan. Gitleaks 8.30.1 over the committed tree on 2026-11-15 reports no leaks, where the audit saw 1,008. |

Findings not listed in this table were not re-assessed in this update.

## Executive summary

Compresso has a strong local-first foundation, and the audit found no evidence that the implemented E1-E7 runtime sends telemetry or performs network I/O. Sentry and PostHog should **not** be added to this delivery slice: doing so would conflict with the locked decision that remote telemetry is neither built nor enabled. Local structured logs, content-free operation traces, and local statistics are the appropriate observability controls for the current scope.

The original assessment found four High-severity issues affecting repository confidentiality, project isolation, assistant configuration secrets, and development-machine integrity:

1. `compresso_read` can read ignored, secret-pattern, oversized, and unindexed files that the scanner deliberately excludes.
2. one MCP process can select any repository ever approved in the global database instead of being bound to its current project;
3. integration previews serialized and printed complete assistant configuration files, potentially including unrelated API tokens (**SEC-003 fixed in the current working tree**); and
4. Turbo 2.5.6 is vulnerable to arbitrary code execution when project scripts are run in a malicious checkout.

No confirmed Compresso-originated compromise or credential exfiltration was found. The most important release gate is to make authorization consistent end-to-end: bind an MCP process to one canonical root, and apply one shared repository-read policy to every path that can return source content.

### Risk summary

| Severity | Count | Disposition |
|---|---:|---|
| High | 4 | Fix before broader testing with untrusted repositories or any distribution |
| Medium | 6 | Fix before public release; several should be pulled into the same authorization hardening work |
| Low | 3 | Defense-in-depth and supply-chain hardening |

## Scope and methodology

The review covered:

- all substantive Rust crates: Core, Atlas, Store, MCP, CLI, Integrations, and Eval;
- workspace manifests and lockfiles;
- schema and architecture constraints for E1-E7;
- filesystem and repository trust boundaries;
- MCP framing, schemas, tool authorization, output handling, and denial-of-service behavior;
- assistant configuration preview, apply, backup, receipt, recovery, and uninstall paths;
- SQLite storage, retention, and local filesystem permissions;
- CI workflows, dependency auditing, secret scanning, packaging, and the local installer;
- tracked agent instructions and relevant locally installed Claude plugins; and
- representative builds, linters, tests, security scanners, and installer tests.

E8-E16 and licensing infrastructure were reviewed only where their documents influence a current boundary. They were not treated as implemented functionality. No source-code fix was made during this audit. Existing user changes were preserved.

Severity combines realistic likelihood and impact for the confirmed deployment model. In particular:

- cloud-hosted assistants are allowed to receive repository content explicitly returned by an MCP tool;
- secret and ignored files are not implicitly authorized merely because they exist under a project root;
- each MCP process is intended to serve one current project only; and
- the installer is currently local/pre-release, so its network path has lower present likelihood but remains a distribution blocker.

## Findings

### 1. [SEC-001] Direct MCP reads bypass the scanner's secret and indexing policy

Severity: **High**

Confidence: **High**

Relevant weaknesses: CWE-200 (Exposure of Sensitive Information), CWE-863 (Incorrect Authorization)

The scanner excludes ignored directories, secret-pattern files, symlinks, unsupported files, and oversized files before indexing. The direct MCP read path does not consult those decisions. It verifies only lexical/canonical containment and that the target is a regular file, then reads the file directly from disk.

Evidence:

- The scanner applies secret, symlink, and size exclusions in [scanner.rs](/Users/yogi/compresso/rust/compresso-atlas/src/scanner.rs:178), and built-in exclusions/secret patterns in [scanner.rs](/Users/yogi/compresso/rust/compresso-atlas/src/scanner.rs:394).
- `compresso_read` resolves a repository and calls `validate_repo_path` followed by `read_bounded`, without checking indexed membership or scanner disposition, in [atlas_read.rs](/Users/yogi/compresso/rust/compresso-mcp/src/tools/atlas_read.rs:108).
- `validate_repo_path` checks traversal, canonical containment, and regular-file status, but not ignore/secret/index state, in [common.rs](/Users/yogi/compresso/rust/compresso-mcp/src/tools/common.rs:240).
- Atlas ultimately uses `fs::read_to_string` on the resolved file in [queries.rs](/Users/yogi/compresso/rust/compresso-atlas/src/queries.rs:927).
- The built-in secret list is limited to `.env*`, `*.pem`, and `id_rsa*`; it omits common files such as `id_ed25519`, `.npmrc`, `.netrc`, `*.key`, cloud credential files, and tool-specific token stores.

Abuse scenario: malicious repository text convinces a cloud assistant to call `compresso_read` for `.env`, `.npmrc`, `.git/config`, a private key, or another ignored file. Compresso itself makes no network request, but the returned result enters the assistant context and can be sent to the provider.

Recommended remediation:

1. Introduce one typed `RepositoryReadPolicy` shared by scanner, index queries, and every MCP retrieval tool.
2. By default, require a requested path to be an indexed file with an allowed scanner disposition.
3. Reject ignored, secret-pattern, oversized, binary, symlinked, special, and unindexed files with a stable non-content-bearing error.
4. Expand secret patterns conservatively and support repository-local ignore policy without trusting repository content to weaken built-in protections.
5. If raw unindexed reads are a product requirement, expose them only through an explicit user-confirmed override outside autonomous MCP tool use.

Verification criteria:

- Canary secrets placed in `.env`, `.npmrc`, `.netrc`, `id_ed25519`, `*.key`, ignored paths, `.git`, and oversized files cannot be returned by any MCP tool.
- A file skipped during scan cannot later be read through direct read, gate mode, outline, unfold, context, or impact paths.
- Ordinary indexed source files remain readable within the token budget.

### 2. [SEC-002] MCP repository approval is ambient and not bound to one process/project

Severity: **High**

Confidence: **High**

Relevant weakness: CWE-862 (Missing Authorization)

The intended boundary is one canonical project root per MCP process. The implementation stores approvals globally and accepts an explicit absolute `root` for any previously approved/indexed repository. Omitting `root` prefers the current working directory, but this is a convenience default rather than an authorization boundary.

Evidence:

- Explicit roots are accepted if found in the global approved-repository store in [common.rs](/Users/yogi/compresso/rust/compresso-mcp/src/tools/common.rs:172).
- The documented resolution contract explicitly permits any approved root in [04-mcp-and-integrations.md](/Users/yogi/compresso/docs/plan/04-mcp-and-integrations.md:11).
- Indexing automatically approves the supplied directory in [indexer.rs](/Users/yogi/compresso/rust/compresso-atlas/src/indexer.rs:107) through the repository store in [repos.rs](/Users/yogi/compresso/rust/compresso-store/src/repos.rs:29).
- The CLI accepts an arbitrary directory to index in [index.rs](/Users/yogi/compresso/rust/compresso-cli/src/commands/index.rs:12), with no broad-root warning for a home directory or filesystem root.
- The command surface has no obvious list/revoke/forget workflow for repository approvals in [commands.rs](/Users/yogi/compresso/rust/compresso-cli/src/commands.rs:25).

Abuse scenario: while operating in project A, a compromised or prompt-influenced assistant supplies the canonical path of previously indexed project B and retrieves its source or secrets. Accidentally indexing a broad directory such as the user's home directory magnifies the exposure.

Recommended remediation:

1. Resolve and canonicalize exactly one project root at MCP startup; store it in immutable session state.
2. Reject tool-level `root` values that do not exactly match that bound root. Prefer removing the parameter from the runtime-facing schema if clients do not need it.
3. Treat the global repository database as discovery metadata, not live session authorization.
4. Add `repo list`, `repo forget`, and approval-expiry/revocation controls.
5. Refuse filesystem root, home directory, and other broad/system roots by default; require an explicit interactive confirmation for exceptional local use.

Verification criteria:

- Start the MCP server in project A after indexing A and B; every attempt to request B returns `outside_root` or a dedicated `wrong_project` error.
- Changing the process working directory after startup does not change authority.
- Repository revocation takes effect predictably, and broad-root indexing is blocked non-interactively.

### 3. [SEC-003] Integration preview and JSON output disclose complete assistant configurations — fixed in working tree

Remediation status: **Fixed on 2026-07-23; pending commit/release.** Superseded: the fix is on `main`; see the status table at the top.

Severity: **High**

Confidence: **High**

Relevant weaknesses: CWE-200 (Exposure of Sensitive Information), CWE-532 (Insertion of Sensitive Information into Log File)

At assessment time, the mutation engine correctly preserved complete target bytes internally, but the same complete document was serialized into `MutationPlan`. Its unified diff rendered the full before/after file. Human dry runs printed it, and JSON output serialized the complete plan. Claude, Cursor, and Codex configuration files commonly contain unrelated MCP environment variables or API tokens; those values could therefore be copied into terminal logs or a cloud assistant transcript.

Original evidence, now superseded by the remediation:

- `MutationPlan` publicly serializes `target_contents` and `unified_diff` in [plan.rs](/Users/yogi/compresso/rust/compresso-integrations/src/plan.rs:189).
- The diff writer emits every line of both the original and resulting file in [plan.rs](/Users/yogi/compresso/rust/compresso-integrations/src/plan.rs:1626).
- Interactive and dry-run paths print the full diff in [integrations.rs](/Users/yogi/compresso/rust/compresso-cli/src/commands/integrations.rs:207).
- JSON rendering embeds the complete serialized plan in [integrations.rs](/Users/yogi/compresso/rust/compresso-cli/src/commands/integrations.rs:473).

Recommended remediation:

1. Keep exact target bytes private to the in-memory/apply object; do not expose them through `Serialize`, `Debug`, CLI JSON, or receipts.
2. Render only Compresso-owned key paths and their intended values, with structural redaction for all unowned sensitive fields.
3. Preserve base/result/plan hashes so apply remains bound to exactly reviewed bytes without revealing those bytes.
4. Ensure errors and debug logging never include full documents.
5. Add canary-token tests across human dry-run, confirmation output, JSON output, error paths, receipts, and debug representations.

Verification criteria: insert a unique canary token into an unrelated assistant configuration field and prove it is absent from stdout, stderr, JSON, receipts, logs, and error strings while preview/apply/uninstall integrity tests continue to pass.

Implemented remediation:

- General-purpose `MutationPlan` serialization now delegates to the public safe projection and omits exact target bytes, prior receipts, and owned-value hashes in [plan.rs](/Users/yogi/compresso/rust/compresso-integrations/src/plan.rs:315) and [plan.rs](/Users/yogi/compresso/rust/compresso-integrations/src/plan.rs:372). A separate private serializer retains exact bytes only for plan hashing and authenticated local recovery journals.
- Preview rendering visits only Compresso-owned paths, emits only meaningful changed fields, and structurally redacts unexpected owned values in [plan.rs](/Users/yogi/compresso/rust/compresso-integrations/src/plan.rs:1995).
- Human and JSON integration/setup output use the safe projection in [integrations.rs](/Users/yogi/compresso/rust/compresso-cli/src/commands/integrations.rs:688) and [setup.rs](/Users/yogi/compresso/rust/compresso-cli/src/commands/setup.rs:394).
- TOML parser failures no longer embed source excerpts, preventing a malformed authorization line from entering an error string.
- Synthetic bearer canaries cover public serialization, debug output, malformed-config errors, human/JSON previews, setup dry runs, and byte-preservation of unrelated Codex TOML in [plan.rs](/Users/yogi/compresso/rust/compresso-integrations/src/plan.rs:3096), [codex.rs](/Users/yogi/compresso/rust/compresso-integrations/src/codex.rs:1040), [codex.rs](/Users/yogi/compresso/rust/compresso-integrations/src/codex.rs:1150), [cli_integrations.rs](/Users/yogi/compresso/rust/compresso-cli/tests/cli_integrations.rs:166), and [cli_setup.rs](/Users/yogi/compresso/rust/compresso-cli/tests/cli_setup.rs:75).

Verification result: the integration and CLI suites, full workspace tests, formatting, Clippy with warnings denied, schema formatting, and diff checks pass in the current working tree. No real assistant configuration was mutated during verification.

### 4. [SEC-004] Turbo 2.5.6 permits code execution from an untrusted checkout

Severity: **High** in Compresso's threat model

Confidence: **High**

Relevant weakness: CWE-829 (Inclusion of Functionality from Untrusted Control Sphere)

The workspace pins Turbo 2.5.6 in [package.json](/Users/yogi/compresso/package.json:19) and the lockfile. That version is affected by [GHSA-3qcw-2rhx-2726](https://github.com/advisories/GHSA-3qcw-2rhx-2726): Turbo versions before 2.9.14 can execute a repository-controlled Yarn binary through `.yarnrc.yml`. The upstream advisory rates the generic case Low, but Compresso explicitly treats repository content as untrusted and its normal `build`, `test`, and `typecheck` scripts invoke Turbo. In this context, opening a malicious branch and running a routine project command can execute attacker code as the developer.

Turbo 2.5.6 also falls in [GHSA-hcf7-66rw-9f5r](https://github.com/advisories/GHSA-hcf7-66rw-9f5r), a login callback CSRF/session-fixation issue. That path appears less relevant because no self-hosted Turbo login flow was identified.

The JavaScript security gate is incomplete: CI sets up Node but does not install pnpm with a frozen lockfile, run `pnpm audit`, or run `pnpm check`. Locally, `pnpm check` currently fails because Biome scans intentionally malformed fixtures and several unformatted files; [biome.json](/Users/yogi/compresso/biome.json:6) does not narrowly exclude the malformed fixture.

Recommended remediation:

1. Upgrade Turbo to at least 2.9.14 and regenerate the lockfile with the pinned pnpm version.
2. Until upgraded, do not invoke Turbo in untrusted checkouts; explicitly inspect for `.yarnrc.yml` first.
3. Add `pnpm install --frozen-lockfile`, `pnpm audit`, and `pnpm check` to CI.
4. Exclude only the exact intentionally malformed fixture from Biome, and format/fix the remaining valid files.
5. Add dependency-update automation with human review and a policy for security-advisory response.

Verification criteria: `pnpm audit` has no known vulnerability at or above the chosen threshold, Turbo resolves to 2.9.14 or newer, the malicious `yarnPath` advisory reproduction cannot execute, and all JavaScript checks run in CI from a frozen lockfile.

### 5. [SEC-005] MCP input and file processing are not bounded before allocation

Severity: **Medium**

Confidence: **High**

Relevant weakness: CWE-400 (Uncontrolled Resource Consumption)

The stdio transport reads until a newline into a growable vector without a frame limit. Compression constructs and tokenizes a context graph before applying its 8 MiB input check. Atlas reads and tokenizes an entire file before token-budget truncation, and some read/gate paths explicitly use `usize::MAX` internally. A buggy or compromised local client can therefore consume substantial memory, CPU, and response bandwidth.

Evidence:

- Unbounded `read_until` framing is in [server.rs](/Users/yogi/compresso/rust/compresso-mcp/src/server.rs:473).
- Compression builds `ContextGraph` before checking `MAX_COMPRESSION_INPUT_BYTES` in [engine_tools.rs](/Users/yogi/compresso/rust/compresso-mcp/src/tools/engine_tools.rs:45).
- `read_bounded` loads and counts the complete file before truncation in [queries.rs](/Users/yogi/compresso/rust/compresso-atlas/src/queries.rs:468).
- Start-only and gate reads request an effectively unlimited internal budget in [atlas_read.rs](/Users/yogi/compresso/rust/compresso-mcp/src/tools/atlas_read.rs:144).

Recommended remediation:

- enforce a hard JSON-RPC frame size while reading, before serde allocation;
- check raw string byte length before graph construction/tokenization;
- do not fail open by echoing an oversized untrusted boundary input in full;
- use bounded/streamed file reads and enforce the scanner's maximum file size at retrieval time;
- add per-request CPU/time limits and bounded operation retention; and
- test no-newline, deeply nested JSON, oversized strings, huge single-line files, and decompression-style pathological inputs.

Remediation update (2026-11-14, transport framing only): the stdio transport now reads through a bounded line reader. A message may be at most `MAX_INCOMING_MESSAGE_BYTES` (64 MiB) before its newline. Once a line passes the cap the buffered prefix is released and the rest of the line is discarded as it arrives, so the framing buffer never holds more than the cap whatever the line length; parsing an accepted message still allocates in addition to that buffer. The client receives a JSON-RPC `-32600` error carrying `max_message_bytes` and no message content. The error names the request `id` only when a top-level integer or short string id is complete within the first 4 KiB of the line; otherwise `id` is `null`. The session then continues with the next line. If input ends inside an oversized line, the error is still sent, the session is closed in the ledger, and the server returns the typed `McpInputError::UnterminatedOversizedMessage` (CLI code `message_too_large`). Each rejection writes one content-free diagnostic line to stderr; it is not a ledger row, because the operations table accepts only the fixed tool vocabulary. Tests: `rust/compresso-mcp/tests/message_cap.rs`, plus the `framing` and `server` unit tests, including a streamed 200 MiB line. The remaining evidence items above are still open.

### 6. [SEC-006] Prompt-injection containment and privacy language stop at the Compresso process boundary

Severity: **Medium**

Confidence: **High**

Relevant weakness: CWE-200 (Exposure of Sensitive Information)

Compresso initiates no runtime network request, which is a strong property. However, the server and plan documentation say that “no content leaves this machine.” That is not true of the complete deployment: MCP results become part of Claude/Codex/Cursor context and may be transmitted to the configured cloud provider. Returned repository text is also untrusted data that can influence an agent to call other tools.

Evidence:

- The shipped MCP instructions make the absolute “no content leaves this machine” claim in [server.rs](/Users/yogi/compresso/rust/compresso-mcp/src/server.rs:28).
- The design document repeats it in [04-mcp-and-integrations.md](/Users/yogi/compresso/docs/plan/04-mcp-and-integrations.md:3).
- Tool responses are JSON text blocks containing source content, but tool descriptions do not explicitly label that content as untrusted or warn the client not to follow embedded instructions.
- Existing adversarial fixtures exercise compressor safety, not end-to-end agent behavior after repository text is returned.

Recommended remediation:

1. Replace the claim with: “Compresso initiates no network traffic. Content returned to your configured assistant may be sent to that assistant's provider under its settings and policies.”
2. Label source-bearing output fields as untrusted repository data in tool descriptions and integration guidance.
3. Keep authorization independent of model instructions: root binding and secret-file policy must not be overridable by repository text.
4. Add end-to-end tests in which malicious source asks the agent to read secrets, switch repositories, mutate configuration, or invoke host tools; those attempts must be blocked at the relevant capability boundary.
5. Document provider retention/privacy assumptions and least-privilege client tool settings.

### 7. [SEC-007] Network installer trusts HTTP and an unsigned same-origin checksum

Severity: **Medium today; High if externally distributed**

Confidence: **High**

Relevant weakness: CWE-494 (Download of Code Without Integrity Check)

The installer accepts plain HTTP, follows redirects without protocol/host restriction, and downloads both the executable archive and `SHA256SUMS` from the same origin. A network attacker or compromised origin can replace both, so the checksum protects against accidental corruption but not publisher impersonation. The current local-only distribution status lowers immediate likelihood; this must still block external release.

Evidence:

- HTTP and HTTPS are advertised as accepted sources in [install-local.sh](/Users/yogi/compresso/installers/install-local.sh:10).
- `curl --location`/`wget` follow redirects without a restricted protocol or pinned host in [install-local.sh](/Users/yogi/compresso/installers/install-local.sh:45).
- Archive and checksum manifest are fetched from the same base URL in [install-local.sh](/Users/yogi/compresso/installers/install-local.sh:77).
- The subsequent checksum verification is structurally sound but authenticates only that unsigned manifest in [install-local.sh](/Users/yogi/compresso/installers/install-local.sh:120).
- [ADR 0013](/Users/yogi/compresso/docs/adr/0013-signed-releases-and-rollback.md:7) explicitly defers signing to a later phase.

Recommended remediation before distribution:

- reject HTTP;
- restrict redirect protocols to HTTPS and define whether cross-host redirects are permitted;
- verify a signed manifest against a trust root embedded independently of the download origin;
- publish provenance and an SBOM, and use platform signing/notarization where applicable; and
- retain the existing single-regular-file tar validation, atomic install, and rollback behavior.

### 8. [SEC-008] Private data-directory and database permissions are not enforced

Severity: **Medium**

Confidence: **High**

Relevant weakness: CWE-732 (Incorrect Permission Assignment for Critical Resource)

Store startup creates parent directories and opens SQLite without enforcing owner-only permissions, ownership, or no-symlink constraints. On the audited machine the restrictive `~/.local/share` parent currently mitigates traversal, but the Compresso subdirectory is mode 0755 and the database/WAL/SHM are mode 0644. Those defaults are not safe to rely on across Unix configurations or custom data locations.

Evidence:

- Store creation calls `create_dir_all` and `Connection::open` without permission hardening in [lib.rs](/Users/yogi/compresso/rust/compresso-store/src/lib.rs:232).
- The non-Windows default is manually fixed to `~/.local/share` in [lib.rs](/Users/yogi/compresso/rust/compresso-store/src/lib.rs:259), while Core has separate runtime/XDG path resolution in [config.rs](/Users/yogi/compresso/rust/compresso-core/src/config.rs:441).
- Relative XDG values are not visibly rejected at this boundary, creating inconsistent location/privacy expectations.

The store contains repository roots and paths, symbols/signatures, operation/session metadata, integration receipts, and other potentially sensitive development metadata.

Recommended remediation:

- create/verify the private data directory as 0700 and database/WAL/SHM as 0600 on Unix;
- reject symlinked or wrong-owner storage paths before opening;
- apply an explicit user-only ACL on Windows;
- use one shared, specification-compliant runtime path resolver and reject relative XDG base directories; and
- test under permissive umasks and pre-created attacker-controlled paths.

### 9. [SEC-009] CI executes third-party actions and images through mutable tags

Severity: **Medium**

Confidence: **High**

Relevant weakness: CWE-829 (Inclusion of Functionality from Untrusted Control Sphere)

The workflow has good top-level `contents: read` permissions and does not use `pull_request_target`. However, actions and container images are referenced by mutable tags such as `actions/checkout@v4`, `Swatinem/rust-cache@v2`, `taiki-e/install-action@v2`, and `ghcr.io/gitleaks/gitleaks:v8.30.1` in [ci.yml](/Users/yogi/compresso/.github/workflows/ci.yml:13). A compromised upstream tag can execute in CI, read private source, or tamper with generated artifacts.

Recommended remediation:

- pin every action to a reviewed full commit SHA and every container to an immutable digest;
- annotate pins with the human-readable release version;
- automate update pull requests while retaining review;
- isolate packaging/signing permissions from pull-request jobs; and
- add artifact attestations when external releases begin.

### 10. [SEC-010] The repository secret-scanning gate currently fails on 1,008 synthetic findings

Severity: **Medium**

Confidence: **High**

The configured Gitleaks canary correctly proves the scanner can detect a planted secret. The actual repository scan then reports 1,008 `generic-api-key` findings, all traced during this audit to the generated `key: "fixture-####"` benchmark content produced in [generate.mjs](/Users/yogi/compresso/fixtures/benchmarks/generate.mjs:640). The CI scan in [ci.yml](/Users/yogi/compresso/.github/workflows/ci.yml:82) therefore cannot currently serve as a clean blocking gate. Persistent false-positive floods create alert fatigue and encourage broad exclusions.

No real committed secret was confirmed by the scan.

Recommended remediation:

1. Prefer changing the synthetic fixture field name if fixture contracts permit.
2. Otherwise add the narrowest possible path-and-pattern/fingerprint allowlist for this generated fixture only.
3. Keep the generic detector enabled globally and retain the independent canary test.
4. Require a zero-finding repository scan in CI and periodically test multiple credential formats.

### 11. [SEC-011] The crate capability checker is a finite denylist, not a security sandbox

Severity: **Low**

Confidence: **High**

Relevant weakness: CWE-693 (Protection Mechanism Failure)

The capability checker is valuable architecture lint and currently passes. It detects a curated set of network/process dependencies and source paths in [check-crate-capabilities.ts](/Users/yogi/compresso/scripts/check-crate-capabilities.ts:99), then checks direct/resolved dependencies and source use in [check-crate-capabilities.ts](/Users/yogi/compresso/scripts/check-crate-capabilities.ts:632). It cannot prove absence of network/process behavior through an unlisted crate, FFI/libc, generated code, build scripts, procedural macros, or indirect capabilities not in its tables.

Recommended remediation: document the checker as policy lint, move pure crates toward explicit dependency allowlists, review build/proc-macro dependencies, diff capability-bearing dependency paths in CI, and supplement with code review and platform sandboxing where a hard boundary is required.

### 12. [SEC-012] Canonicalize-then-open paths have a local symlink race

Severity: **Low**

Confidence: **Medium**

Relevant weakness: CWE-367 (Time-of-check Time-of-use Race Condition)

Atlas canonicalizes a requested path, verifies containment/file type, returns the path, and opens it later in [queries.rs](/Users/yogi/compresso/rust/compresso-atlas/src/queries.rs:872). A concurrent local writer can replace a checked path between validation and open. The scanner has a similar check/open separation. Exploitation requires another process able to modify the same repository under the user's account, so this is defense-in-depth rather than the primary authorization issue.

Recommended remediation: use descriptor-relative, no-follow/beneath opens where supported (`openat2`/`openat` patterns), verify metadata identity after opening, and provide platform-equivalent safe-open abstractions with race tests.

### 13. [SEC-013] Tracked agent instructions mandate an unpinned third-party plugin boundary

Severity: **Low**

Confidence: **Medium**

Relevant weakness: CWE-829 (Inclusion of Functionality from Untrusted Control Sphere)

The tracked [CLAUDE.md](/Users/yogi/compresso/CLAUDE.md:1) mandates routing repository reads, shell execution, and web access through the locally installed `context-mode` plugin. Static inspection of the installed plugin found no Sentry, PostHog, or obvious telemetry path, and it is separate from the third-party agent plugin discussed below. Nevertheless, a tracked instruction that requires an unpinned local plugin creates a development supply-chain boundary with repository-read, sandbox execution, and network-fetch capabilities.

Recommended remediation: make the integration optional for contributors or document a reviewed version/hash/source; treat plugin upgrades as dependency changes; and ensure the plugin cannot expand Compresso runtime capabilities or bypass the one-project policy.

## Positive controls observed

- Remote telemetry is forced off in [config.rs](/Users/yogi/compresso/rust/compresso-core/src/config.rs:217), consistent with locked decision D6 in [decisions.md](/Users/yogi/compresso/decisions.md:14). No implemented runtime dependency or code path for Sentry, PostHog, sockets, or HTTP was found in E1-E7.
- The MCP implementation uses stdio, marks tools read-only/non-destructive/idempotent, and contains no shell execution path.
- Scanner traversal canonicalizes roots, rejects containment escapes, does not follow symlinks, skips binaries/oversized files, and honors ignore rules during indexing.
- SQL operations inspected use parameterized statements; FTS literal handling is escaped.
- Compression fails open, protects designated content, and records content-free traces.
- Integration apply/uninstall internals use canonical-path checks, exact base/result/diff/plan hashes, TTL/session binding, owned-key validation, restrictive temporary/backup files, atomic replacement, authenticated receipts/journals, crash recovery, and reversible uninstall. Public preview/output is now separated from private exact apply bytes by the SEC-003 remediation.
- CI has read-only default GitHub token permissions, no `pull_request_target`, Rust advisory/license gates, capability checks, a secret-scanner canary, a multi-platform matrix, and deterministic packaging tests.
- The installer constrains the tar payload to exactly one regular `compresso` file and uses atomic replacement with rollback. SEC-007 concerns publisher authenticity.

## Verification performed

| Check | Result |
|---|---|
| `cargo fmt --all -- --check` | Pass |
| `cargo clippy --workspace --all-targets -- -D warnings` | Pass |
| `cargo build --release -p compresso-cli --locked` | Pass |
| `cargo audit` | Pass; no Rust advisory findings |
| `cargo deny check advisories licenses bans sources` | Pass; duplicate-version warnings only |
| Capability checker tests | Pass; 19/19 |
| Capability enforcement | Pass across 10 declared crates |
| ShellCheck | Pass for shell scripts |
| actionlint | Pass for both workflows |
| CI matrix assertion | Pass |
| Packaging self-test | Pass |
| Local installer test suite | Pass |
| Integrations, MCP, Store, Proxy, and Updater targeted tests | Pass |
| `cargo test --workspace` | One load-sensitive Eval test failed once; its exact isolated rerun passed. Treat as a reliability flake, not a demonstrated security failure. |
| `pnpm audit --json` | Fail; Turbo advisories described in SEC-004 |
| `pnpm check` | Fail; intentionally malformed fixture plus formatting issues are not narrowly handled |
| Gitleaks repository scan | Fail; 1,008 synthetic benchmark findings described in SEC-010 (superseded: clean as of 2026-11-15, see the status table at the top) |

The worktree already contained modifications to `rust/compresso-eval/src/lib.rs`, `scripts/ci/check_ci_matrix.rb`, `impl.md`, and `rust/compresso-eval/src/calibrate.rs`. The audit did not alter or stage those files.

## Prioritized remediation plan

### P0 — before distribution or testing against genuinely untrusted repositories

1. Fix SEC-001 and SEC-002 together: immutable one-project MCP binding plus one shared indexed/secret-aware read policy.
2. SEC-003 fixed in the current working tree with output canary tests; commit/release the remediation before invoking integration previews through cloud assistants.
3. Upgrade Turbo and make the frozen JavaScript audit/check pipeline green.
4. Disable the network installer path until SEC-007 has an authenticated release design.

### P1 — before public release

1. Bound MCP frames, raw inputs, file reads, CPU time, and retained operations.
2. Correct provider-boundary language and add end-to-end prompt-injection tests.
3. Enforce private storage permissions and unify XDG/runtime path handling.
4. Pin CI actions/images and restore a zero-noise secret gate.

### P2 — defense in depth

1. Harden path opens against local races.
2. Strengthen and accurately describe capability-boundary automation.
3. Pin or make optional the tracked development-agent plugin dependency.

## Related local-environment observations (not Compresso findings)

### Third-party agent plugin for Claude

Version 0.3.71 of the third-party agent plugin is installed in the local Claude plugin cache but is currently disabled in Claude settings. No running process or active TCP connection for the plugin was observed at inspection time. The local update metadata and the plugin's public repository indicate a newer 0.3.87 release.

Static inspection of the installed version, without executing it, found that when enabled it configures PostHog telemetry on by default and installs session telemetry hooks. The inspected event paths include session completion/failure/usage metrics, session and user identifiers, service tier, token/cache counts, turn/tool counts, compaction statistics, and timing/savings metrics. No outbound prompt or source-body field was observed in the session telemetry payload paths inspected, though runtime error metadata/stacks can contain contextual details and the obfuscated bundle prevents an absolute claim about every path. Set its documented telemetry-disable environment variable (see the vendor's docs) before enabling it if no plugin telemetry is desired, or leave the plugin disabled.

The plugin's local usage metadata is stored with relatively permissive file modes, while its authentication file is owner-only. Tightening its metadata directory to owner-only access is prudent on a multi-user system.

### Separate exposed process argument

During host-process inspection, a live Supabase access token belonging to a separate Claude MCP process was visible in that process's command line. The token is not repeated here and was not associated with Compresso or the third-party plugin. It should be revoked/rotated and supplied through a mechanism that does not place secrets in process arguments; command-line secrets are discoverable through process listings and diagnostic tools.
