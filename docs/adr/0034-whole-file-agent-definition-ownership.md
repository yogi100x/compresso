# ADR-0034: Whole-file ownership for the opt-in exploration agent definition

## Status

Accepted 2026-11-19 by the owner (D93, E17-28); proposed 2026-11-18. It extends the
integration mutation framework with a third ownership format and adds no
network or process capability.

## Context

Benchmarks showed coding models call Compresso in addition to their client's
native shell search rather than in place of it, so each Compresso call is
extra work. Claude Code lets a project define a subagent as one Markdown file
with YAML frontmatter under `.claude/agents/`; the frontmatter `tools` field
limits which tools that agent can call. An agent whose tools are the Compresso
exploration tools plus file tools cannot fall back to shell search, which is
the substitution the reference product achieves by running as its own agent
profile.

Every assistant configuration mutation must have a dry-run preview, backup,
receipt, and reversible uninstall. The existing framework owns key paths
inside JSON or TOML documents and only ever replaces file contents; it never
deletes a file. A whole agent file does not fit either format, and a reversible
uninstall of a file Compresso created must remove it.

## Decision

- **Third format.** `ConfigFormat::Markdown` (`"markdown"` in
  `schemas/integrations/v1.json`) means the adapter owns the whole file
  through exactly one owned path (`agent_definition`). The owned value hash is
  the BLAKE3 hash of the file bytes. Text-fidelity shaping (byte-order mark
  and line endings) does not apply because Compresso renders every byte.
- **Deletion is a planned result.** For this format only, an empty target
  means the file is absent. The plan's `result_hash` is the hash of a missing
  file; commit removes the file instead of replacing it; journal validation
  and recovery accept an already-absent file as the planned result, so a crash
  between deletion and receipt persistence replays to completion without
  recreating the file.
- **Backup.** Install over a missing file records the existing
  `(none: file created)` sentinel. An uninstall may delete only while the file
  still hashes to the receipt's installed value, so the deleted bytes are
  exactly Compresso's own render. That plan records the new sentinel
  `(none: unmodified Compresso file removed)` and writes no backup, so no
  backup file is left in a directory a team may commit. Any other write keeps
  the normal timestamped backup, which since E17-53 (D118, ADR-0037's
  amendment) is kept in Compresso's data directory, never beside the file.
- **Ownership.** Install proceeds only when the file is missing or already
  carries the Compresso marker line and agent name. Any other file of that
  name is refused with `user_file_present` and never changed. A user-edited
  Compresso file is retained on uninstall with a `key_modified` warning.
- **Paths.** `<project>/.claude/agents/compresso-explorer.md`, chosen by
  `--project` or else the working directory. `.claude`, `.claude/agents`, and
  the file must not be symbolic links. The adapter also resolves the user path
  under the injected home directory for receipts, but the CLI exposes only the
  project scope.
- **Opt-in only.** Only `compresso integrations install claude-code
  --explore-agent` writes it. `setup` and a plain integration install never do.
  `uninstall`, `status`, and `verify` take the same flag.

## Consequences

- A project gains one Compresso-written Markdown file. Removing it leaves the
  `.claude/agents/` directory Compresso may have created; the directory is
  empty when nothing else lives there.
- The agent's tool list is derived from the lean MCP exploration allowlist
  (D92), so adding an exploration tool there also changes the agent file a
  reinstall writes; `verify` reports an older file as not current. With
  E17-22 and E17-25 merged, that list is eight Compresso tools (context,
  repo_map, search, grep, trace, outline, unfold, read; recall is never
  included) plus `Read, Edit, Write`; the agent body's routing bullets do not
  yet mention grep or trace.
- The agent keeps the client's `Read` tool because Claude Code's `Edit` and
  `Write` refuse to change an existing file the session has not read. Its
  instructions limit `Read` to the read immediately before an edit.
- Claude Code reads the agent file; Compresso makes no claim about how a
  given Claude Code version enforces the `tools` field beyond its documented
  behavior.
