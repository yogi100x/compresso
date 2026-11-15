# ADR-0030: Keyless-signed checksum manifest for the slice installer

## Status

Accepted (owner decision, 2026-11-15). Narrows D42 for the slice installer;
native platform signing, notarization, and package managers stay in E12 as
ADR-0013 describes.

## Context

`installers/install-local.sh` verified a tarball against a `SHA256SUMS` file
fetched from the same origin as the tarball (SEC-007). Anyone able to replace
the tarball at that origin could replace the manifest too, so the checksum
detected corruption but not a substituted release. D42 had left the slice
installer checksum-only and deferred all signing to E12, where E12-05 names an
owner-held ed25519 key.

The private-beta packaging workflow already signs its release manifest with
Sigstore keyless signing bound to the GitHub Actions OIDC identity, so the
project has that mechanism and its trust assumptions in use.

## Decision

The main-branch CI job that aggregates `SHA256SUMS` signs it with
`cosign sign-blob` (keyless) and publishes `SHA256SUMS.sigstore.json` beside
it. No long-lived signing key exists, so there is none to store, rotate, or
leak.

The installer pins the one identity it trusts in its own source:

- certificate identity
  `https://github.com/yogi100x/compresso/.github/workflows/ci.yml@refs/heads/main`
- OIDC issuer `https://token.actions.githubusercontent.com`

Nothing fetched from the release origin can change those values. CI verifies
each signature with the identity read from the installer, so the two cannot
drift apart unnoticed.

Verification rules:

- An `https://` source must serve `SHA256SUMS.sigstore.json`. A missing bundle,
  a missing `cosign` binary, or a rejected signature stops the install before
  anything is written under `~/.local`.
- A local source is verified the same way. Without a bundle the install is
  refused unless the caller passes `--allow-unsigned`, which exists for a
  build made on the same machine: it installs on the checksum alone and prints
  a warning. The flag is rejected for a URL source.
- Verification needs cosign 3.0 or newer. CI signs with the cosign 3 release
  the pinned installer action provides, and its default bundle format is not
  read by cosign 2 without extra flags.

The owner chose the signing scheme. The local opt-out and the minimum cosign
version are implementation decisions made with it.

This is consistent with the bootstrap trust chain in
`docs/plan/08-ip-and-secure-deployment.md` §3.4, which already lists
"verify signature and digest" as an installer step and had exempted only the
unsigned slice installer.

## Consequences

- New process capability: the installer executes `cosign`, which the user
  installs separately. `cosign verify-blob` contacts Sigstore's public
  infrastructure to refresh its trust root unless the user has configured it
  for offline use. The installer itself still opens no connection other than
  the artifact downloads.
- New CI capability: the `checksums` job holds `id-token: write` and sends a
  signature to Sigstore. Each signature is recorded in the public Rekor
  transparency log together with the repository name, workflow path, and
  commit, on every push to `main`, permanently. The repository is public at
  the time of this decision, so the log reveals nothing that is not already
  visible; if it is made private again, those three facts stay public for
  every signed commit. The owner accepted this exposure explicitly on
  2026-11-15. The private-beta workflow has the same property but only on a
  manual, environment-gated run.
- Installing a CI release now needs `cosign`; a machine without it cannot
  install one. Existing local workflows that install a self-built tarball must
  add `--allow-unsigned`.
- Trust is anchored in GitHub's OIDC issuer, the Sigstore root, and the
  integrity of the `ci.yml` workflow on `main`. A change to that workflow that
  reaches `main` can sign any manifest; branch protection is the control.
- The signature proves "built by `ci.yml` on `main`", so the controls on
  `main` are part of the trust chain. Applied at the owner's instruction on
  2026-11-15 (they were absent before): `main` requires a pull request and
  the eight CI checks, with no force push, no deletion, and no administrator
  exemption; a tag ruleset lets only repository administrators create,
  move, or delete `cli-v*` tags; and the `private-beta-release` environment
  requires the owner's approval and deploys only from `cli-v*` tags. These
  are repository settings, not code, and can be changed by an administrator.
- The installer does not pin a commit or tag, so any manifest signed by a
  main-branch CI run verifies. The manifest names each tarball with its
  version, so a manifest for another version does not match the requested
  file; but slice builds keep one version across many commits, so an origin
  can serve an older signed build of the same version. Rollback protection
  remains with the E12 updater. Signing only tagged releases and checking the
  tag in the certificate would close this and is the natural next step once
  releases are tagged.
- E12-05 (owner-held ed25519 signature for Linux packages) is unaffected and
  can be added alongside.
