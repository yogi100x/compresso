# @compresso/cli

This package distributes the native Compresso CLI for supported Unix hosts. It
uses a dependency-free POSIX launcher and does not require Node.js at runtime.

Supported targets:

- macOS Apple Silicon (`darwin-arm64`)
- macOS Intel (`darwin-x64`)
- Linux x86-64 with glibc (`linux-x64`)

The release process must place an executable named `compresso` in each
`vendor/<target>/` directory and replace that directory's `sha256` manifest
entry before publishing. The checked-in manifests are intentionally
unpopulated so an incomplete scaffold fails closed instead of running an
unverified artifact.

After installation, run:

```sh
compresso setup codex .
```

Setup previews every change before writing. For Claude Code and Codex the
server entry enables session recall over that client's own local transcripts
(`--recall-source`); add `--no-recall` to leave it off. Setup also installs
one post-compaction reminder hook for those two clients, never over hooks you
already defined; add `--no-compaction-hook` to leave it out.

This package has no install, postinstall, or prepare lifecycle scripts. The
launcher selects and verifies the vendored artifact locally before replacing
itself with the native process.
