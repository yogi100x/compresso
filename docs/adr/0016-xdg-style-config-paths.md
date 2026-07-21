# 0016 — XDG-style config paths on all platforms except Windows

## Status

Accepted

## Context

Build-doc §16 recommends `~/.config/compresso/config.toml`, `~/.local/share/compresso/compresso.db`, `~/.cache/compresso/indexes/`, and `~/.local/state/compresso/logs/`, with native equivalents per platform, and D23 locks the binary at `~/.local/bin/compresso`. Compresso is a developer tool driven from terminals, where XDG-style dotfile paths are the convention users script against — including on macOS, where `~/Library` paths are hostile to shell workflows.

## Decision

Use XDG-style paths (config, data, cache, state as above, honoring `XDG_*` overrides) on macOS and Linux alike. Windows uses its native known folders: `%APPDATA%\compresso` for config, `%LOCALAPPDATA%\compresso` for data and cache, with logs under the local app data state directory.

## Consequences

- macOS and Linux documentation, dotfile management, and support instructions are identical, and paths are predictable for scripts and backups.
- Deliberately deviating from the macOS `~/Library/Application Support` convention is acceptable for a CLI-first tool but must be documented for macOS users.
- Windows keeps platform-correct behavior for roaming profiles and MDM; the path mapping is centralized in one module and covered by the CI matrix (D33).
- Uninstall and diagnostics know every location the product writes to, supporting the reversibility guarantee (build-doc §4, §5.13).
