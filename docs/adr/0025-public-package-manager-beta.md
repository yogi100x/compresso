# 0025 — Public package-manager beta distribution

## Status

Accepted

## Context

The E1-E7 vertical slice is a useful offline product, but its local installer
is a development artifact: users must already have a tarball and the checksum
is not an independent authenticity proof. A public beta needs a familiar
package-manager entry point without publishing the private engine source,
adding install-time execution, or pulling licensing, cloud, desktop, and the
rest of E8-E16 into scope.

The user controls the machine. A native engine that runs there can be copied,
traced, disassembled, and patched. Signing establishes artifact origin and
integrity; it does not make the engine confidential or authorize a cloud
request. This decision therefore extends ADR-0021, ADR-0022, and D66 rather
than weakening them.

## Decision

Authorize the PB-01..PB-08 public package-manager beta lane with this supply
chain:

```text
private engine source
  -> private release workflow
  -> signed immutable native artifacts and provenance
  -> signature-verifying import into a separate public distribution repository
  -> one @compresso/cli package published under the npm beta tag
```

The public repository contains auditable packaging inputs only: the POSIX
launcher, package metadata, public verification material, signed manifests,
provenance references, notices, and user documentation. It never receives
engine source, private debug symbols, fixtures, prompts, evaluation corpora,
credentials, or private signing material. Imported artifacts are immutable and
are accepted only after their signature, digest, platform, version, and
provenance identity verify against pinned public trust material.

`@compresso/cli` is one beta package bundling exactly three native engines:
macOS arm64, macOS x86_64, and Linux x86_64. Its `bin` entry is a stable POSIX
launcher that resolves its package root independently of the caller's working
directory, selects only the exact supported platform/architecture pair,
checks the selected artifact against its bundled SHA-256 value, exports its own
normalized absolute lexical invocation path as `COMPRESSO_LAUNCHER_PATH`
without resolving the final package-manager symlink, and directly replaces
itself with the engine while preserving arguments, standard streams, signals,
and exit status. The symlink target must resolve to an executable regular file,
but the stable shim path is what integrations record. The launcher creates no
mutable file, temporary directory, or session state. Windows is deferred and
must fail as unsupported rather than selecting a near match.

The package has no `install`, `preinstall`, `postinstall`, `prepare`, or other
installation lifecycle script. Installation performs no download and executes
no Compresso code. Node.js/npm is the distribution front door only: after the
package is laid out, invoking `compresso` requires no Node.js runtime. npm
trusted publishing with OIDC provenance, native OS signing where available,
and staged approval in the protected public distribution repository are the
default release-channel trust controls.

An MCP session root is the immutable canonical repository root to which
repository access is bound at startup. It is selected only by an explicit `mcp serve
--root ROOT` naming an approved repository or by resolving the canonical
current working directory to its longest approved ancestor. There is no
fallback to the sole approved repository. A tool-supplied root may only confirm
the bound root; it cannot select or switch to another repository, even if that
repository is approved. With neither an explicit root nor an approved cwd, the
server remains unbound: repository-independent compression, stats, and explain
tools remain available, while every repository tool fails with an actionable,
content-free error. A tool call cannot bind an unbound session. MCP provider
disclosure must say that Compresso performs no independent network I/O while
results delivered to the configured coding client may be sent by that client
to its configured model provider.

Before public beta publication, repository-bound MCP behavior, provider
disclosure, secret-file exclusions, repository list/removal commands, the
single-command `compresso setup CLIENT ROOT` flow, and the stable launcher-path
upgrade contract are release blockers. They are owned by PB-02 and PB-03 rather
than treated as launcher temporary-state concerns.

Beta documentation and command output disclose truthfully that the package is
an auditable wrapper around a proprietary recoverable native binary, which
platform artifacts it contains, and which provider behavior is actually
present. Token-accounting provider labels are not advertised as live provider
integrations; the E1-E7 beta has no provider proxy, account, licensing, cloud,
desktop, remote telemetry, or unrequested assistant-configuration mutation.
The explicitly requested setup command may mutate supported client
configuration only after its dry-run preview and confirmation contract.

The beta pulls only the narrowly required parts of E12/E14 into PB ownership:
the `dist` build and private symbols, artifact allowlist and secret scan,
immutable signed manifest/artifacts, SBOM/provenance, public trust material,
and package-chain security review. Independent artifact signatures authenticate
the private-to-public import and enable external, manual, and OS verification;
the launcher's bundled SHA-256 check is corruption and post-install tamper
detection, not an unpatchable trust anchor. It does not complete or authorize
the full E12/E14 epics, native desktop distribution, updater, Homebrew,
licensing, cloud control plane, or obfuscation. Obfuscation remains optional,
last, and merely deterrent; the beta may ship without it.

D36 remains OPEN. Reserving and testing `@compresso/cli` under a beta tag is an
operational beta decision, not trademark clearance. Publication to the stable
npm tag remains prohibited until D36 and the full stable release gates are
resolved.

## Consequences

- Users get one conventional install surface and one stable `compresso`
  command across the supported POSIX targets, with no install-time execution
  or runtime Node.js dependency.
- The public repository can audit exactly which signed private artifact entered
  each npm tarball without disclosing engine source.
- A full npm registry or package compromise can replace the launcher, engine,
  and bundled checks, especially on Linux where native OS verification may not
  be automatic. Independent signatures remain valuable for the protected
  private-to-public import and for external or manual verification, but the
  package must not claim they make an npm-delivered binary unpatchable.
- The single package is larger than per-platform optional packages; beta
  simplicity and one auditable manifest outweigh download-size optimization.
- Windows users receive an explicit unsupported result until a separately
  reviewed launcher/package contract is authorized.
- No claim is made that compilation, stripping, signing, or obfuscation keeps a
  public binary secret or makes paid local behavior unpatchable.
