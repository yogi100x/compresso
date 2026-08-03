# Compresso Intellectual Property and Secure Deployment Plan

## 1. Purpose

This document defines how Compresso protects proprietary algorithms, source code, strategy logic, models, product knowledge, credentials, releases, and brand assets while preserving user trust and local-first privacy.

The objective is not to make reverse engineering impossible. Software that runs on a user's machine can eventually be inspected, traced, patched, or extracted by a sufficiently motivated researcher. The objective is to:

- Keep the most valuable implementation off-device where appropriate.
- Raise the cost of reverse engineering shipped components.
- Prevent unauthorized modification and malicious redistribution.
- Protect signing keys, credentials, and cloud infrastructure.
- Preserve an auditable security and privacy boundary.
- Establish legal and operational trade-secret protections.
- Avoid depending on obscurity for security.

This document is an engineering and operational plan, not jurisdiction-specific legal advice. Trademark, patent, employment, privacy, and licensing decisions require qualified counsel in the relevant jurisdictions.

## 2. Protection principles

- Architecture provides stronger protection than obfuscation.
- Truly secret algorithms must not be shipped to untrusted devices.
- No secret key may be embedded in a client binary.
- Signing provides authenticity and integrity, not secrecy.
- Encryption of local code does not hide it after execution begins.
- Security-critical behavior must remain testable and auditable.
- Privacy controls, telemetry schemas, installers, and uninstallers should be understandable.
- Local source code must not be uploaded merely to protect Compresso's algorithms.
- Paid-feature enforcement must never break the underlying coding assistant.
- Client compromise is assumed possible; cloud authorization must not trust client claims.

## 3. IP inventory

Maintain a controlled inventory with an owner, classification, storage location, protection method, and disclosure status for every asset.

### 3.1 Crown-jewel assets

- Context ranking algorithms.
- Compression strategy selection.
- Relevance and risk scoring weights.
- Reviewer synthesis and tuning logic.
- Historical review evaluation methods.
- Model-routing policies.
- Organization knowledge ranking.
- Proprietary training and evaluation datasets.
- Strategy performance data.
- Abuse detection and entitlement policies.

Preferred protection:

- Trade-secret controls.
- Restricted repositories.
- Server-side execution where privacy permits.
- Copyright ownership.
- Patent review for genuinely novel methods.

### 3.2 Shipped proprietary assets

- Rust context engine.
- Atlas indexing implementation.
- Edit transaction engine.
- SQL policy engine.
- Recall implementation.
- Desktop application.
- Updater.

Preferred protection:

- Closed-source distribution where desired.
- Native compilation.
- Symbol stripping and link-time optimization.
- Signed artifacts.
- Selective obfuscation.
- Proprietary license terms.

### 3.3 Auditable public boundary

Consider keeping these components public or independently auditable:

- Shell and PowerShell bootstrap installers.
- Release signature verification.
- MCP schemas.
- Assistant configuration manifests.
- Telemetry schemas.
- Privacy controls.
- Uninstallation logic.
- Security and data-flow documentation.

These components contain little differentiating logic and benefit from user scrutiny.

### 3.4 Brand assets

- Compresso name.
- Domains and package names.
- Logo and visual identity.
- Product copy.
- Documentation.

Actions:

- Complete trademark and naming searches before launch.
- Register primary domains and common defensive variants.
- Reserve relevant package and social identifiers.
- Record creation and ownership of visual assets.
- Use consistent copyright and trademark notices.

## 4. Threat model

### 4.1 Adversaries

- Casual users copying scripts or prompts.
- Competitors inspecting product behavior.
- Skilled reverse engineers analyzing native binaries.
- Attackers patching local entitlement checks.
- Malicious package mirrors or update infrastructure.
- Compromised CI or developer accounts.
- Insiders exfiltrating source or strategy data.
- Dependency maintainers or packages becoming compromised.
- Attackers impersonating Compresso releases.

### 4.2 Protected outcomes

- Users can verify authentic Compresso software.
- Attackers cannot create accepted malicious updates.
- Cloud services reject forged entitlements and device claims.
- Local secrets do not appear in binaries or logs.
- Reverse engineering requires substantial specialist effort.
- A leaked client binary does not expose cloud signing or service credentials.
- A leaked low-privilege repository does not expose all crown-jewel logic.
- Security remains effective even when implementation details are known.

### 4.3 Explicit limitations

Compresso cannot guarantee that a local user cannot:

- Disassemble a native executable.
- Capture decrypted code or model data from memory.
- Trace local file and network activity.
- Patch local-only licensing checks.
- Reimplement public protocol behavior.

The plan accepts these limits and concentrates stronger controls on release authenticity, cloud authorization, trade-secret handling, and architectural separation.

## 5. Deployment architecture

```text
Open or auditable bootstrap and adapters
                    |
                    v
       Signed local IPC/MCP boundary
                    |
                    v
       Closed native Compresso agent
       - local search and indexing
       - compression
       - transactional editing
       - SQL and recall
                    |
          optional signed HTTPS
                    |
                    v
        Compresso cloud control plane
       - auth and device registration
       - entitlements and billing
       - proprietary policy services
       - organization knowledge
       - release and feature metadata
```

### 5.1 Local-only responsibilities

Keep these local by default:

- Repository contents.
- Atlas indexes.
- File search and reads.
- File edits and validation.
- Database credentials and queries.
- Session transcripts and recall index.
- Deterministic compression.
- Local operation traces.
- Secret detection.

### 5.2 Cloud responsibilities

Keep these server-side when they must remain secret or centrally enforced:

- Authentication.
- Device registration.
- Billing and subscription state.
- Signed entitlements.
- Release metadata.
- Feature flags.
- Organization policies.
- Proprietary learned routing services.
- Reviewer tuning services.
- Organization knowledge services.
- Abuse controls.

Cloud features that require code or context upload must be optional, explicit, narrowly scoped, and documented field by field.

## 6. Package boundaries

### 6.1 Bootstrap installer

The installer must be small and auditable. It may:

1. Detect platform and architecture.
2. Fetch signed release metadata.
3. Download an immutable artifact.
4. Verify signature and digest.
5. Install atomically.
6. Print explicit next steps.

It must not:

- Contain proprietary algorithms.
- Collect telemetry.
- Sign in automatically.
- Modify assistant configuration automatically.
- Download mutable unsigned second-stage code.

### 6.2 Integration adapters

Claude and Codex adapters should be thin. They own configuration translation, MCP registration, hook translation, health checks, and uninstall receipts. They do not own compression, search, edit, SQL, recall, or review algorithms.

### 6.3 Native engine

The closed Rust engine owns the local proprietary implementation. It exposes a narrow versioned protocol over protected local IPC.

### 6.4 Desktop frontend

The Tauri frontend contains presentation logic only. Sensitive ranking, entitlement, and strategy logic must not be placed in JavaScript or browser-accessible state.

## 7. Rust release hardening

Recommended Cargo profile:

```toml
[profile.release]
opt-level = 3
lto = "fat"
codegen-units = 1
panic = "abort"
strip = "symbols"
debug = 0
incremental = false
```

Additional controls:

- Keep debug symbols in private symbol storage.
- Remove local source paths from production diagnostics.
- Minimize exported native symbols.
- Use internal visibility for private libraries.
- Eliminate unused code and resources.
- Do not bundle internal fixtures, design documents, prompts, or source maps.
- Separate sensitive algorithms into restricted crates.
- Avoid descriptive internal assertions in public release builds.
- Generate build identifiers without exposing repository credentials or developer names.
- Confirm crash reports contain symbols only after server-side symbolication.

Stripping and optimization increase analysis cost but do not make algorithms secret.

## 8. Selective obfuscation policy

Obfuscation is permitted only after profiling, compatibility testing, and security review.

Potential targets:

- Strategy router.
- Ranking implementation.
- Reviewer policy interpreter.
- Model-selection rules.
- Non-security entitlement presentation logic.

Possible techniques:

- Symbol renaming.
- Selective control-flow transformation.
- String-literal protection.
- Constant splitting.
- Commercial LLVM-based transformations for selected crates.

Do not obfuscate:

- Signature verification.
- Cryptographic primitives.
- Installer and updater trust logic.
- Privacy and telemetry controls.
- Secret redaction.
- Uninstaller.
- Crash recovery.
- Data deletion behavior.

Obfuscation must not prevent vulnerability response, deterministic builds where required, or internal audits.

## 9. Tauri and frontend hardening

- Disable development tools in production builds.
- Enforce a restrictive Content Security Policy.
- Permit only explicit Tauri commands.
- Validate every frontend-to-Rust request.
- Do not expose filesystem or shell primitives directly.
- Use authenticated IPC rather than unauthenticated localhost endpoints where possible.
- Do not store access tokens in localStorage.
- Do not include private source maps in public releases.
- Keep privileged behavior in Rust commands with capability checks.
- Sign desktop updates and verify them before installation.

Frontend obfuscation may reduce casual copying but is not protection for crown-jewel logic.

## 10. Local IPC security

Preferred transports:

- Unix domain sockets on macOS and Linux.
- Named pipes on Windows.
- Stdio for assistant-launched MCP bridges.

Controls:

- Per-install random authentication secret.
- OS-user-only socket and file permissions.
- Protocol version negotiation.
- Session-specific capability tokens.
- Request-size and concurrency limits.
- Method-level permissions.
- Approved-root path authorization.
- Peer process and user validation where available.
- Explicit timeout and cancellation semantics.
- Structured redacted audit records.

If localhost HTTP is required:

- Bind only to `127.0.0.1` and `::1` intentionally.
- Use a random or configured private port.
- Require a short-lived bearer token.
- Validate Origin where a browser is involved.
- Do not use permissive CORS.
- Reject Host-header confusion.

## 11. Strategy and policy distribution

Compresso may distribute signed strategy packs:

```text
strategy-pack-v17.bin
strategy-pack-v17.manifest
strategy-pack-v17.sig
```

Manifest fields:

- Pack version.
- Minimum and maximum compatible client versions.
- Release channel.
- Issue and expiration timestamps.
- Artifact digest.
- Signing key identifier.
- Rollback policy.

The native client verifies signatures before loading any strategy pack.

Important limitation: downloadable strategy packs can be inspected after loading. They are suitable for tamper resistance and operational updates, not for algorithms that must remain secret.

## 12. Cloud deployment security

### 12.1 Network architecture

- Terminate TLS at a managed load balancer or ingress.
- Run Fastify API containers on private application networks.
- Keep PostgreSQL and Redis private.
- Restrict database ingress to application and migration identities.
- Use separate development, staging, and production accounts or projects.
- Deny public database access.
- Apply egress controls to sensitive workers where practical.

### 12.2 Application controls

- Validate all input and output against versioned schemas.
- Enforce authorization in service/domain layers, not only routes.
- Rate-limit by account, device, organization, and IP where appropriate.
- Use idempotency keys for billing and device operations.
- Verify all webhook signatures.
- Record immutable administrative audit events.
- Separate customer support permissions from release administration.
- Use background jobs for billing, email, and telemetry processing.
- Apply bounded retries and dead-letter handling.

### 12.3 Data controls

- Encrypt cloud storage and backups.
- Use least-privilege database roles.
- Separate operational and analytics data.
- Minimize retained device and event metadata.
- Test backup restoration.
- Implement data export and deletion workflows.
- Keep source code, prompts, paths, symbols, SQL, and patches out of the control-plane database.

## 13. Authentication and device identity

- Use browser-based OAuth/OIDC for interactive users.
- Use a device authorization flow for CLI and desktop login.
- Generate a device key pair locally.
- Store private device keys in the OS credential service.
- Register only the device public key with the cloud.
- Use short-lived access tokens and rotated refresh tokens.
- Revoke individual devices without invalidating every device.
- Require stronger authentication for organization and release administrators.
- Support enterprise SSO without moving local source data into the identity system.

## 14. Entitlements and licensing

Use signed entitlement documents:

```json
{
  "subject": "user_123",
  "device": "device_public_key_hash",
  "organization": "org_456",
  "plan": "pro",
  "features": ["advanced-routing", "review", "proxy"],
  "issued_at": "...",
  "expires_at": "...",
  "offline_until": "...",
  "key_id": "entitlement-2026-01"
}
```

The server signs entitlements with an asymmetric private key. Clients contain only public verification keys.

Rules:

- Local/free features remain available without an account.
- Paid entitlements are cached.
- Provide a generous offline grace period.
- Refresh in the background.
- Never interrupt an active coding operation because the entitlement service is unavailable.
- Enforce cloud-only features on the server.
- Assume determined users can patch local paid-feature checks.
- Do not add invasive DRM, debugger detection, or destructive anti-tamper behavior.

## 15. Secret management

Never embed these in source or binaries:

- Code-signing keys.
- Entitlement signing keys.
- Database credentials.
- Cloud provider credentials.
- OAuth client secrets not intended for public clients.
- Billing secrets.
- Telemetry ingestion secrets.
- Administrative API keys.

Controls:

- Store production secrets in a managed secret service.
- Use workload identity and OIDC for CI and cloud services.
- Prefer short-lived credentials.
- Rotate secrets and signing keys under documented procedures.
- Separate release signing from application deployment permissions.
- Scan source, commits, artifacts, logs, and container layers for secrets.
- Store local user credentials in platform credential stores.
- Redact secrets before logs, diagnostics, or telemetry are created.

## 16. Build and release pipeline

```text
Protected source repository
  -> ephemeral build worker
  -> tests and evaluation gates
  -> dependency and license scan
  -> platform builds
  -> SBOM and provenance
  -> KMS/HSM signing
  -> notarization
  -> immutable artifact storage
  -> CDN
  -> signed update metadata
```

Required controls:

- Protected branches and mandatory review.
- Restricted workflow modification permissions.
- Ephemeral runners for release builds.
- CI authentication through OIDC.
- Signing keys in KMS or HSM.
- Two-person release approval.
- Immutable artifacts and version tags.
- Dependency lockfiles.
- SBOM generation.
- Build provenance.
- Malware and vulnerability scanning.
- Separate stable, beta, nightly, and enterprise channels.
- macOS signing and notarization.
- Windows Authenticode signing.
- Signed Linux archives and packages.
- Private debug-symbol upload.

## 17. Secure update system

Use a TUF-style metadata hierarchy:

```text
root
  -> timestamp
  -> snapshot
  -> targets
```

The updater verifies:

- Trusted root keys.
- Signature threshold.
- Metadata expiration.
- Version monotonicity.
- Artifact digest.
- Platform and architecture.
- Compatibility constraints.
- Rollback protection.

Update flow:

1. Download metadata.
2. Verify the trust chain.
3. Download into a private staging directory.
4. Verify artifact signature and digest.
5. Check disk and compatibility requirements.
6. Stop affected local components safely.
7. Atomically install the new version.
8. Run migrations.
9. Perform health checks.
10. Roll back automatically when checks fail.

Retain at least one known-good local version.

## 18. Telemetry and diagnostics

- Telemetry is off by default.
- Diagnostic upload requires explicit approval.
- Users can inspect exact event schemas and sample payloads.
- No source, prompts, paths, symbols, patches, SQL, database schemas, or tool arguments.
- Use coarse timestamps where exact time is unnecessary.
- Avoid persistent identifiers unless operationally required.
- Separate product analytics from crash diagnostics.
- Redact before persistence, not only before upload.
- Provide export and deletion controls.
- Treat project labels and branch names as potentially sensitive.

Do not hide telemetry behavior as part of IP protection. Transparency at this boundary increases trust without revealing proprietary algorithms.

## 19. Local data protection

- Store account and device credentials in the OS credential service.
- Restrict local databases and configuration to the current user.
- Encrypt optional synchronized knowledge before upload where architecture permits.
- Keep raw session recall disabled until enabled.
- Implement retention and deletion policies.
- Use transactional migrations.
- Avoid storing raw prompts or source in operation ledgers.
- Protect backup and rollback files with the same permissions as originals.
- Verify canonical paths before every read or write.

## 20. Repository and contributor controls

- Separate public adapters from private engine repositories or access groups.
- Restrict crown-jewel crates to engineers who require access.
- Require hardware-backed multi-factor authentication.
- Review organization owners and repository administrators regularly.
- Disable dormant accounts promptly.
- Log access to sensitive repositories and release systems.
- Require signed contributor agreements or employment/contractor assignments.
- Document prior inventions for contributors.
- Require confidentiality obligations that survive departure where enforceable.
- Perform secure offboarding and credential rotation.
- Do not place production datasets in developer laptops by default.

## 21. Open-source compliance

Maintain a machine-generated and reviewed dependency inventory.

Policy:

- MIT, BSD, and Apache-2.0 dependencies require notice compliance.
- Apache-2.0 patent terms must be preserved.
- LGPL dependencies require linking and modification review.
- GPL dependencies require distribution-model review.
- AGPL dependencies require legal review before any use in a proprietary product or service.
- Do not copy code, prompts, or assets from the predecessor CLI or the reference product.
- Reimplement public behavior through original specifications and fixtures.
- Preserve all required third-party notices.

CI must fail when an unapproved license enters a release dependency graph.

## 22. Legal protection program

With qualified counsel:

- Confirm ownership of all source and design assets.
- Put employee and contractor IP assignment agreements in place.
- Maintain a trade-secret inventory and handling policy.
- Register important copyrights where useful for enforcement.
- Search and file trademarks for Compresso and primary visual marks.
- Review novel compression, routing, and evaluation methods for patentability.
- Publish proprietary end-user and enterprise license terms.
- Prohibit unauthorized redistribution and circumvention where lawful.
- Establish an infringement reporting and response process.

Trade-secret treatment requires operational evidence: restricted access, confidentiality agreements, internal classification, and consistent handling.

## 23. Anti-tamper boundaries

Reasonable controls:

- OS code-signature verification.
- Signed internal resource manifests.
- Signed strategy packs.
- Protocol version and integrity checks.
- Automatic repair or safe refusal for corrupted installations.
- Cloud-side validation of signed device and entitlement claims.

Avoid:

- Destructive responses to detected modification.
- Rootkit-like behavior.
- Aggressive debugger detection.
- Certificate pinning that prevents safe certificate rotation.
- Silent collection of reverse-engineering indicators.
- Treating local anti-tamper checks as authorization controls.

## 24. Incident response

Create runbooks for:

- Signing-key compromise.
- Malicious release publication.
- Source repository breach.
- Insider exfiltration.
- Dependency compromise.
- Leaked cloud credentials.
- Entitlement signing compromise.
- Counterfeit distribution.
- User-data exposure.

Signing compromise response must support:

1. Immediate release freeze.
2. Key revocation or trust-root rotation.
3. CDN and package withdrawal.
4. Customer notification.
5. Known-good recovery release.
6. Forensic preservation.
7. Root-cause and control review.

## 25. Deployment checklist

### Architecture

- [ ] Crown-jewel logic classified as local, cloud, or restricted source.
- [ ] Claude and Codex adapters contain no core algorithms.
- [ ] Tauri frontend contains no sensitive policy logic.
- [ ] Cloud remains outside the mandatory local compression path.

### Client hardening

- [ ] Release symbols stripped.
- [ ] Private debug symbols archived securely.
- [ ] Source maps excluded from public artifacts.
- [ ] Local IPC authenticated and permission-restricted.
- [ ] Workspace boundaries enforced.
- [ ] Secrets absent from binaries.

### Supply chain

- [ ] Lockfiles committed.
- [ ] Dependencies and licenses scanned.
- [ ] SBOM generated.
- [ ] Provenance generated.
- [ ] Artifacts signed.
- [ ] macOS notarized.
- [ ] Windows signed.
- [ ] Update metadata signed with threshold-capable trust roots.

### Cloud

- [ ] Production database is private.
- [ ] Workload identity replaces static cloud credentials.
- [ ] Authorization tested at domain boundaries.
- [ ] Webhook signatures verified.
- [ ] Admin actions audited.
- [ ] Backup restoration tested.

### Legal and organizational

- [ ] Contributor IP assignments complete.
- [ ] Trade-secret inventory maintained.
- [ ] Open-source notices complete.
- [ ] Trademark search completed.
- [ ] Proprietary licenses reviewed.
- [ ] Sensitive repository access reviewed.

## 26. Release acceptance gates

A production release is blocked unless:

- Every artifact verifies against signed metadata.
- No production secret appears in source, binary, container, or logs.
- SBOM and provenance are generated.
- Critical and high-severity dependency findings are resolved or formally accepted.
- Local IPC authentication tests pass.
- Update rollback tests pass on macOS, Windows, and Linux.
- Claude and Codex uninstall restoration tests pass.
- Telemetry remains disabled by default.
- Privacy schemas contain no prohibited fields.
- Cloud outage tests show local features remain operational.
- Required third-party notices are included.

## 27. Architecture decisions to record

Create ADRs for:

- Public installer and adapter boundary.
- Closed native engine.
- Cloud placement of proprietary learned strategies.
- Rust hardening profile.
- Selective obfuscation policy.
- Local IPC transport and authentication.
- Device key and entitlement design.
- TUF-style update metadata.
- Debug-symbol retention.
- Telemetry-off default.
- Source and dependency license policy.
- Trade-secret repository separation.

## 28. Recommended implementation order

1. Inventory crown-jewel assets and owners.
2. Separate client adapters from the native engine.
3. Define the signed local IPC protocol.
4. Establish production secret management and workload identity.
5. Build signed CI artifacts with private symbols.
6. Add SBOM and provenance generation.
7. Implement platform code signing and notarization.
8. Implement signed update metadata and rollback.
9. Implement device keys and signed entitlements.
10. Add repository, contributor, and license controls.
11. Apply selective release hardening.
12. Evaluate obfuscation only after performance and support baselines exist.
13. Complete external security and legal review before public launch.

## 29. Definition of done

Compresso's IP and deployment protection is production-ready when:

- Crown-jewel assets are inventoried and access-controlled.
- Claude and Codex use thin adapters over the same protected engine.
- Sensitive local code ships as signed, stripped native binaries.
- Truly secret strategies remain server-side where privacy permits.
- No client binary contains cloud or signing secrets.
- Installers and update metadata are independently verifiable.
- Releases include signatures, SBOM, and provenance.
- Local IPC and cloud APIs enforce authenticated capabilities.
- Telemetry is transparent and opt-in.
- Contributor ownership and open-source compliance are documented.
- Update rollback and signing-compromise runbooks have been exercised.
- Reverse engineering is made costly without weakening user security or trust.
