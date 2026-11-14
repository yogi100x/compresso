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

Clients that run a hook after compaction can install that command:

```sh
compresso integrations install claude-code --compaction-hook   # preview, confirm
compresso integrations status  claude-code --compaction-hook
compresso integrations verify  claude-code --compaction-hook
compresso integrations uninstall claude-code --compaction-hook
```

`--project DIR`, `--dry-run`, `--yes`, and `--json` behave as they do for the
MCP entry. The hook is opt-in; `compresso setup` does not install it.

## Client support

| Client | Mechanism | File | Status |
|---|---|---|---|
| Claude Code | `SessionStart` hook, matcher `compact` | `~/.claude/settings.json`, `<project>/.claude/settings.json` | Installed by `--compaction-hook` |
| Codex | `SessionStart` hook, matcher `compact` | `~/.codex/hooks.json`, `<project>/.codex/hooks.json` | Installed by `--compaction-hook`; Codex applies its own hook trust review, and project hooks load only in trusted projects |
| Cursor | none | n/a | Refused as `unsupported`: the only compaction hook (`preCompact`) is observational and cannot add model context |

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
  refused without writing and prints the entry to add by hand. The plan
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

`docs/plan/04-mcp-and-integrations.md` §2.5 states that no hooks are installed
for any client in the slice. This opt-in hook is an exception that needs an
owner decision and a matching amendment to that section.
