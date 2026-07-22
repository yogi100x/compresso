# Local slice installer

`installers/install-local.sh` installs the unsigned, checksum-verified D42 slice release at
`~/.local/bin/compresso`. It does not sign in, collect telemetry, start services, or modify
Compresso, Claude, Codex, Cursor, or other assistant configuration.

## Supported platforms

The POSIX installer maps the host to one of the E1-06 artifacts:

| Host | Release target |
|---|---|
| Apple Silicon macOS | `aarch64-apple-darwin` |
| Intel macOS | `x86_64-apple-darwin` |
| x86-64 Linux | `x86_64-unknown-linux-gnu` |

Windows uses the packaged `.exe` directly until the signed PowerShell installer arrives in E12.

## Install from local release artifacts

Place the platform tarball and the aggregated `SHA256SUMS` in one directory, then run:

```sh
installers/install-local.sh /path/to/release-artifacts
```

A direct tarball path is also accepted:

```sh
installers/install-local.sh /path/to/compresso-0.1.0-aarch64-apple-darwin.tar.gz
```

The sibling `SHA256SUMS` remains required. A directory must contain exactly one tarball matching
the current platform.

## Install from a URL

Pass an immutable, versioned HTTP(S) tarball URL. The installer fetches `SHA256SUMS` from the same
directory:

```sh
installers/install-local.sh \
  https://releases.example/compresso/0.1.0/compresso-0.1.0-aarch64-apple-darwin.tar.gz
```

URL directory listing and mutable “latest” resolution are intentionally unsupported. D42 release
artifacts are checksum-verified but unsigned; signature verification, notarization, and package
managers arrive in E12.

## Verification and replacement behavior

The installer downloads or copies both files to a temporary directory, finds the exact tarball
entry in `SHA256SUMS`, and compares the expected and actual SHA-256 digests. A mismatch reports
both values and stops before creating or changing anything under `~/.local/bin`.

Only a verified archive containing one regular file named `compresso` is accepted. The binary is
copied to a temporary file inside `~/.local/bin`, changed to mode `0755`, and atomically renamed to
`compresso`. When an installation already exists, its bytes are first preserved atomically as
`compresso.previous`. Re-running the installer is safe and makes the immediately prior binary the
backup.

To restore that backup manually:

```sh
cp "$HOME/.local/bin/compresso.previous" "$HOME/.local/bin/compresso"
chmod 0755 "$HOME/.local/bin/compresso"
```

If `~/.local/bin` is absent from `PATH`, the installer prints the shell export to add it. It never
edits shell startup files.

## Post-install checks

```sh
compresso --version
compresso doctor
```

If verification fails, confirm that the tarball and `SHA256SUMS` came from the same release. If the
platform is unsupported, use a matching E1-06 artifact or build locally; do not rename an artifact
for a different target.

## Test the installer

The test suite builds deterministic stub artifacts through the E1-06 packager and exercises local
and URL resolution, checksum failure, atomic replacement, mode `0755`, PATH guidance, and config
immutability:

```sh
installers/tests/install_local_test.sh
```
