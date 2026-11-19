# ADR-0035: One opt-in, verified edit tool ends the read-only server guarantee

## Status

Accepted 2026-11-19 by the owner (D97, E17-32): the owner approved building
the edit tool in reply to a proposal that stated it "ends the read-only server
guarantee stated in the docs and security model". The design below (opt-in
flag, gates, bounds, atomicity, report shape) was chosen by the implementing
session and awaits owner confirmation. It adds no network or process
capability.

## Context

Every MCP tool Compresso shipped was read-only, and the server instructions,
`03-interfaces.md`, `04-mcp-and-integrations.md`, ADR-0007, and the threat
model in `08-ip-and-secure-deployment.md` said so. In a long benchmark session
a model exploring with Compresso had to rename an identifier at 8 sites. With
no edit tool it used shell `sed` with `\b`, which BSD `sed` does not support;
the command changed nothing and the model reported the rename as done. A
competing agent setup made the same edits through its own verified edit tool
and got 8 of 8. The failure was not the edit itself but the missing
verification: nothing told the model that zero sites changed.

ADR-0024 and E15 (`09-advanced-capabilities.md`) plan a broader edit surface:
a `compresso-edit` crate with separate plan, apply, and rollback edit
tools, content-hashed plans, permission classes, and rollback. That remains future scope. This ADR
adds a narrower tool now, ahead of E15.

## Decision

1. **Opt-in only.** `compresso mcp serve` without `--edits` is unchanged: same
   tool list, same instructions string byte for byte, same results. With
   `--edits` (either profile) the server also advertises `compresso_edit`, the
   only tool whose annotations say `readOnlyHint: false`,
   `destructiveHint: true`, `idempotentHint: false`. A server without the flag
   answers a call to it with `[tool_unavailable]` naming the flag, in every
   profile, and dispatches nothing. `compresso integrations install claude-code
   --edits` writes the server entry with `--edits` (before any recall pair);
   `--explore-agent --edits` grants the exploration agent the tool. Nothing
   installs either by default. `compresso edit` is the operator-run form; running
   it is the explicit choice, so it takes no enabling flag.
2. **Operations.** `replace` (exact literal, `expected_count`, default 1),
   `rename` (whole identifier across the indexed files of a scope given as a
   path prefix or glob), and `insert` (before or after an anchor that occurs
   exactly once). Up to 16 operations per call, applied in order to in-memory
   copies, all or nothing. No regular expressions. File creation and deletion
   are out of scope: every target must already be an indexed file.
3. **Identifier boundaries** are defined explicitly, not by a regex `\b`: an
   identifier character is `_` or any Unicode alphanumeric character
   (`char::is_alphanumeric`); `$` is not one. A rename matches only where the
   neighbouring characters are not identifier characters. A match is never
   rewritten, and is reported as *adjacent* ("next to a string prefix, escape,
   or sigil; check by hand"), when: an odd run of backslashes precedes it and
   the name can be an escape (one character, or `x`/`u`/`U` followed by
   hexadecimal digits), so `\n` is kept but `\App\User` and `\\n` are
   renamed; a quote or `#` follows a name that is a literal prefix (`f`, `r`,
   `b`, `u`, `rb`, `br`, `fr`, `rf`, `c`, `cr`, `L`, `U`, `u8`, `R`, any case),
   so `f"` is kept but `foo"` is renamed; `%` precedes a one-character name
   (`%d`); or, in Rust, `'` precedes it (`'a`). The file's language comes from
   its extension: in JavaScript, TypeScript, Java, Kotlin, Scala, and Dart
   `$name` is a different identifier and counts as a substring; in shell, PHP,
   Perl, and anything else `$name` and `${name}` are the variable `name` and
   are renamed. Otherwise matching is textual, so comments and strings are
   renamed too. A rename into a name that already
   occurs as an identifier in scope (by the same matcher) is refused
   (`name_exists`) unless the call passes `allow_existing`.
4. **Verification in the report.** Per file: sites and their lines, whether the
   file was stale against the index, line ending, BOM, byte sizes, hashes
   before and after, and whether the file was re-read after the write and found
   to hold exactly the edited bytes. Per rename: files searched and skipped
   (with reasons), and the remaining whole-identifier, substring, and adjacent
   matches of the old name in the scope after the edit, with locations, and
   `complete`, which is false when a file holding the old name could not be
   searched or edited (it is listed in `files_skipped`) or the index could not
   be refreshed first. A rename is done only at 0 remaining whole matches with
   `complete: true`, after checking adjacent matches by hand. The call-level
   totals `incomplete_operations`, `remaining_whole_total`,
   `remaining_substring_total`, `remaining_adjacent_total`,
   `files_skipped_total`, and `unverified_files` are never dropped when the
   report is cut, and the text form's first line carries every non-zero one
   (`INCOMPLETE`, remaining, adjacent, not verified). `dry_run` returns the same report plus a
   compact unified diff and writes nothing.
5. **Gates** (repository content and arguments are untrusted): the path must be
   repository-relative without `.`, `..`, backslash, or NUL; not a secret path;
   not under `.git`; not a control file, meaning a path any of whose components, compared case-insensitively after trimming trailing dots and spaces, is: a name starting with `.git` (`.gitignore`, `.gitattributes`, `.gitmodules`, `.gitlab-ci.yml`, ...) or `.aider`, or ending in `.code-workspace`; an ignore file `.ignore`, `.rgignore`, `.cursorignore`, `.aiexclude`, `.codeiumignore`, `.geminiignore`; an agent instruction or configuration name `CLAUDE.md`, `CLAUDE.local.md`, `AGENTS.md`, `GEMINI.md`, `.cursorrules`, `.windsurfrules`, `.clinerules`, `.claude.json`, `.mcp.json`; a hook, CI, environment, or package-manager name `.pre-commit-config.yaml`, `.travis.yml`, `azure-pipelines.yml`, `Jenkinsfile`, `.envrc`, `lefthook.yml`, `.lefthook.yml` (and `.yaml`/`.yml` forms), `.devcontainer.json`, `CODEOWNERS`, `.npmrc`, `.yarnrc.yml`; `.cargo/config` or `.cargo/config.toml`; or `.claude`, `.codex`, `.cursor`, `.windsurf`, `.gemini`, `.continue`, `.zed`, `.vscode`, `.idea`, `.github`, `.gitlab`, `.husky`, `.circleci`, `.devcontainer`, `.buildkite`, `.woodpecker`. Every list is matched at every component, so a directory named like a control file is refused too. This is a denylist of known control files, not a guarantee: build scripts and other code a tool may later run (`build.rs`, `package.json` scripts, `Makefile`) stay editable because editing code is the tool's purpose, and the client's permission prompt is the control for them (an edit to an ignore file would widen what the gates allow, and an
   edit to agent, CI, or hook configuration could turn text into command
   execution); writable by this user (probed by opening it for writing
   without truncation, since mode bits alone do not say who may write); owned
   by this user on Unix (a replacement would otherwise change the owner,
   `foreign_owner`); not excluded by the scanner (for example gitignored);
   listed in the index; resolved by the shared read gate (`resolve_under_root`:
   inside the root, no symbolic-link component, device and inode bound to the
   open on Unix); not the Compresso store's database files, nor anything in the
   store directory (which also holds install receipts) when it lies inside the
   repository; valid UTF-8; and at
   most 1 MB (the scanner's limit) before and after the edit, so an edited file
   never falls out of the index. A rename skips files it may not edit and lists
   them; a control, protected, read-only, foreign-owned, or non-UTF-8 file that
   does not hold the old name (searched as text, or as bytes when it is not
   text) is not listed, so it does not make the rename incomplete.
   A secret path and a path through a symbolic link are both reported as
   `not_found`, exactly as the read tools report them. `new` and inserted text
   may not contain NUL.
6. **Atomic, verified writes.** All changed files are re-verified (same gate,
   same device and inode, same content hash). Then, for every file, both the
   edited bytes and a copy of the original bytes are written to temporary files
   in its own directory, given the original permissions, and synced, so no
   later step needs new disk space; a failure here (for example a full disk)
   changes no repository file. Each target is then re-verified once more and
   the edited file renamed over it; directories are synced on Unix. If a rename
   fails, every file already renamed is restored by renaming its original copy
   back, after checking it still holds exactly the edited bytes (a file changed
   again by someone else is left alone and reported as not restored, and its
   original copy is kept beside it), and the error names the files restored,
   any that were not, and the kept copies. Line endings (LF or
   CRLF: `\n` in arguments becomes `\r\n` in a CRLF file) and a UTF-8 BOM are
   preserved.
7. **Stale files** are edited against their current content (the model has
   usually just read them) and marked stale in the report. An optional
   `expected_hash` (BLAKE3 of the current bytes) per replace or insert refuses
   the edit on mismatch.
8. **Bounds.** 16 operations; 64 KiB per `old`, `new`, anchor, or inserted text;
   identifiers up to 256 bytes; 200 changed files; 2,000 sites; 16 MiB written;
   256 MiB read per call to search rename scopes (one budget shared by every
   rename in the call; a rename stops as soon as its matched files pass 200); at most 20 listed locations per list
   with exact totals; a 64 KiB diff; and `max_tokens` (300 to 100,000, default
   2,000) over the report, cut by halving the diff and then the longest list,
   with `truncated: true`. A call runs exactly once; only its report is fitted,
   never the edit re-run. No shell command, no network request.
9. **Index freshness.** A call that renames first refreshes the index
   incrementally, because a rename's scope is the indexed file list and a file
   created or changed since the last index would otherwise be missed; this
   writes only Compresso's index (also on a dry run), never a repository file,
   and if it fails every rename in the call reports `complete: false`. After a
   write the repository is re-indexed through the
   incremental indexer with the counter `compresso index` uses, so only changed
   files are re-parsed and grep, search, trace, and outline see the edit at once.
   If re-indexing fails the edit stands and the report says `index: stale`.
10. **Recording.** Each successful call is one operations-ledger row with tool
    `compresso_edit`, classified as its own kind: no trace, no retrieval
    metrics, no savings, and no file content. A dry run is a successful call
    and is recorded too. The `operations` table has no
    constraint on the tool name, so no migration is needed.
11. **Instructions.** The default profile's instructions stay policy neutral
    (D92 rule (a)); with `--edits` the full profile's first sentence changes
    from "read-only access" to say every tool is read-only except the enabled
    `compresso_edit`, and nothing else changes. The lean instructions and the
    exploration agent, both opt-in surfaces (D92 rule (b)), add: make
    multi-site edits and renames with `compresso_edit`, not shell text
    substitution, and a rename is done only at 0 remaining whole matches.

## Threat model

- **Prompt injection through repository content** can ask the model to edit
  anything. The tool limits what an edit can reach (indexed, non-secret,
  non-ignored files under the root, never `.git` or the store) and what it can
  do (literal text only, no creation, deletion, permission change, or command),
  and every change is reported with its lines; it cannot judge intent. The
  operator accepts this risk by passing `--edits`; the client's own approval
  prompts still apply to the tool call.
- **Path tricks** (`..`, absolute paths, symbolic links in any component,
  swapping a file or directory for a link after validation) are refused by the
  shared read gate and by re-verification before each rename. Residual window:
  the standard library has no directory-handle-relative rename, so a directory
  component swapped in the instant between the last verification and the
  rename is not detected. Non-Unix builds have no device and inode check and
  rely on the content hash and path walk.
- **Secrets** are never read or written: secret paths are refused before any
  read, and a rename skips them.
- **Denial of service** is bounded by the limits above; nothing runs unbounded.
- Ownership and extended attributes are not copied to the replacement file;
  permissions are. Hard links to an edited file are split by the rename.
- A process killed between staging and the last rename leaves
  `.<name>.compresso-edit-*.tmp` files beside the edited files; nothing
  removes them automatically, but the scanner never indexes them.
- The language for `$` and lifetimes is chosen by file extension only; a file
  without a known extension is treated like shell (`$name` renamed).
- The owner check could not be exercised by a test (it needs a file owned by
  another user); the writability probe is tested.

## Out of scope

Creating, deleting, moving, or changing permissions of files; regular
expressions; edits outside the bound repository or to unindexed files;
multi-call transactions and rollback after a successful call (use version
control); the E15 plan/apply/rollback tools of ADR-0024, which remain the
planned general edit surface.

## Consequences

- The statement "every Compresso MCP tool is read-only" is no longer true in
  general. It stays true for every server started without `--edits`, which is
  every server an installer writes unless the operator asks.
- Older Compresso binaries do not know the `compresso_edit` ledger kind; their
  `stats` refuses a store holding an edit row. Running a newer build against a
  shared store already implies forward store migrations, so this adds no new
  class of incompatibility, but it is recorded here.
- The schema cost of the edit tool is paid only by servers that serve it:
  measured (o200k / Anthropic-calibrated) full 4,030 / 4,636 tokens, full with
  edits 4,627 / 5,322, lean 2,006 / 2,308, lean with edits 2,489 / 2,863. The
  lean half-cost test compares the profiles without the opt-in tool.
