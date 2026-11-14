# Post-compaction routing reminder (E17-01)

## Problem

Compresso tells an agent which tools to prefer through the MCP `initialize`
instructions. Those are delivered once per server session. When a coding
client later compacts or summarizes the conversation, that guidance can be
dropped, and the agent drifts back to client-native search and whole-file
reads. An MCP server is not told that compaction happened, so it cannot react
on its own.

## What Compresso does

`compresso mcp routing-reminder` prints one constant, bounded reminder
(`compresso_mcp::routing_reminder()`, at most 640 bytes). It names only tools
the server declares, contains no repository content, session data, or
timestamps, opens no store, and performs no network I/O. Repeated output is
byte-identical.

`compresso mcp routing-reminder --session-start-hook` wraps the same text in the
`SessionStart` hook envelope:

```json
{"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":"..."}}
```

## Installed by default in `compresso setup` (D76)

`compresso setup claude-code ROOT` and `compresso setup codex ROOT` install the
hook as a second step, after the MCP server entry. Pass
`--no-compaction-hook` to leave it out. The hook is written at the same scope
as the MCP entry setup writes: `<ROOT>/.claude/settings.json` for Claude Code
and `~/.codex/hooks.json` for Codex.

- **Preview.** The combined plan shows the hook step before anything is
  written, in `--dry-run` and before the confirmation prompt: the hook file,
  the owned key, the plan hash, and the exact `hooks.SessionStart` entry
  (the key-level diff itself redacts array contents). JSON output carries it
  as `data.plan.compaction_hook` with `status` `planned`, `skipped`,
  `disabled`, or `unsupported`.
- **Your own hooks win.** If the hook file already defines `SessionStart`
  hooks that Compresso did not write, the step is `skipped`: nothing in that
  file is changed, setup prints a `warning:` line with the reason and a `fix:`
  line with the entry to add by hand, and setup still completes with exit
  status 0. The same applies to any other reason the hook cannot be planned,
  such as a malformed hook file; the fix text then names the command to re-run.
- **Never fails setup.** The hook is applied only after the MCP entry landed.
  If the hook file changes between approval and apply, the step is reported
  as `failed` with the same warning and fix text, the edit is left untouched,
  and setup still exits 0 with the repository indexed and the MCP entry
  installed. The outcome is `data.result.compaction_hook` in JSON output.
- **Cursor** has no usable hook, so setup has no hook step for it and prints
  nothing about one. The same holds on Windows, where the hook is unsupported.
- Re-running setup keeps an installed hook and reports it as already installed.

## Managing the hook directly

`compresso integrations install <client>` does **not** install the hook by
default. That command changes exactly one receipt-owned key under one previewed
plan hash, and `compresso integrations uninstall <client>` removes exactly
that key; folding a second file into it would either apply a change the
approved plan hash does not cover or leave a hook behind on uninstall. Use the
explicit flag instead:

```sh
compresso integrations install claude-code --compaction-hook   # preview, confirm
compresso integrations status  claude-code --compaction-hook
compresso integrations verify  claude-code --compaction-hook
compresso integrations uninstall claude-code --compaction-hook
```

`--project DIR`, `--dry-run`, `--yes`, and `--json` behave as they do for the
MCP entry.

Because the hook has its own receipt, undoing a setup takes two commands:
`compresso integrations uninstall <client>` for the MCP entry and
`compresso integrations uninstall <client> --compaction-hook` for the hook
(add `--project ROOT` for Claude Code). Uninstalling the MCP entry while the
hook is still installed prints a `compaction_hook_still_installed` warning
with the exact second command.

## Client support

| Client | Mechanism | File | Status |
|---|---|---|---|
| Claude Code | `SessionStart` hook, matcher `compact` | `~/.claude/settings.json`, `<project>/.claude/settings.json` | Installed by `compresso setup` (project file) unless `--no-compaction-hook`, or by `--compaction-hook` |
| Codex | `SessionStart` hook, matcher `compact` | `~/.codex/hooks.json`, `<project>/.codex/hooks.json` | Installed by `compresso setup` (user file) unless `--no-compaction-hook`, or by `--compaction-hook`; Codex applies its own hook trust review, and project hooks load only in trusted projects |
| Cursor | none | n/a | No step in `compresso setup`; `--compaction-hook` is refused as `unsupported`: the only compaction hook (`preCompact`) is observational and cannot add model context |

Support was taken from each client's published hook reference on 2026-11-14.
The installed configuration and the command's output are covered by automated
tests; that a live client actually injects the text after a real compaction
has not been verified in this repository and needs a manual check per client.

For Cursor, and for any client without the hook installed, routing guidance
comes only from the MCP server instructions and tool descriptions, which are
not re-sent after compaction.

## Ownership and reversibility

The adapter uses the existing plan, backup, receipt, and uninstall mechanism.
It owns exactly one key, `hooks.SessionStart`, in the hook file:

```json
{
  "hooks": {
    "SessionStart": [
      {
        "matcher": "compact",
        "hooks": [
          { "type": "command", "command": "'/abs/compresso' mcp routing-reminder --session-start-hook", "timeout": 10 }
        ]
      }
    ]
  }
}
```

- The hook file is separate from the MCP configuration, so the hook has its
  own receipt and can be installed or removed independently of the MCP entry.
- Install proceeds only when `hooks.SessionStart` is absent or already holds a
  Compresso-written value (re-install is idempotent and follows a moved
  launcher). If the user already defines `SessionStart` hooks, install is
  refused without writing and prints the entry to add by hand (in
  `compresso setup` that refusal is a skipped step, not a failure). The plan
  mechanism owns object keys, not array elements, so Compresso does not merge
  into a user's array.
- Uninstall removes the key only while it still matches the receipt hash. If
  the user added hooks afterwards, the key is retained with a `key_modified`
  warning.
- A symbolic link at the client configuration directory or hook file is
  rejected before any read or write. Launcher paths containing a single quote
  or control character are rejected because the client passes the command to a
  shell.
- Not supported on Windows.

## Contract note

`docs/plan/04-mcp-and-integrations.md` §2.5 used to state that no hooks are
installed for any client in the slice. Owner decision D76 makes this one hook
the single exception and turns it on by default in `compresso setup`; §2.5 is
amended to match. No other hook, wrapper, or PATH shim is installed.
