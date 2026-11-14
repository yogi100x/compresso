# Documentation drift lint

`scripts/doc-drift-lint.ts` fails when documentation and the shipped surface
disagree. It runs in CI and locally:

```sh
cargo run -p compresso-cli --example surface_inventory > surface.json
node --experimental-strip-types scripts/doc-drift-lint.ts --inventory surface.json --json
```

Exit status is `0` when there is no drift, `1` when there is, and `2` when the
inventory or configuration cannot be used.

## Source of truth

The linter never parses Rust source. `rust/compresso-cli/src/surface.rs` builds
the inventory from the live objects:

- the clap command trees of `compresso`, `compresso-eval`, and
  `compresso-eval-campaign`, including every flag, alias, and hidden marker;
- the MCP tool registry the server advertises;
- the configuration key table behind `compresso config explain`.

## What is checked

| Finding | Meaning |
|---------|---------|
| `unknown_cli_command` | A documented invocation names a subcommand that does not exist. |
| `unknown_cli_flag` | A documented invocation passes a flag the command does not accept. |
| `unknown_mcp_tool` | A tool-shaped name is not in the MCP registry. |
| `unknown_config_key` | A key in a known configuration section does not exist. |
| `missing_path` | A repository path in a code span, or a relative link target, does not exist. |
| `missing_ticket_file` | A ticket marked done lists a repository path that does not exist. |
| `undocumented_cli_command` | A visible `compresso` command is mentioned in no document. |
| `undocumented_mcp_tool` | A registered MCP tool is mentioned in no document. |
| `stale_allowlist_entry` | An allowlist entry matches nothing and must be removed. |

Invocations are read from inline code spans and from shell code blocks,
including `cargo run -p <package> -- <arguments>` forms and backslash
continuations. Flags are only checked inside an invocation of one of the
inventoried binaries, so flags of other programs are never reported. Paths are
checked only when they start with a directory listed in `path_roots`; globs
must match at least one file and placeholders are skipped.

## Scope

`scripts/doc-drift-lint.config.json` selects the documents:

- `docs.include` minus `docs.exclude` are current-state documents and are
  checked in full.
- `mention_only_docs` are contract documents. A command or tool they name
  counts as documented, but they also describe future surface, so their own
  references are not findings.
- `completed_tickets` are ticket backlogs. Only the `Files:` line of a ticket
  that is checked off is treated as a current-state claim. Bare file names and
  crate-relative shorthand are ambiguous and are skipped.

Plan documents are otherwise out of scope. Validating ticket identifiers,
dependencies, rollups, templates, and the files of tickets that are still open
is the job of the plan linter tracked as E16-16, not of this tool.

## Allowlist

A reference to something planned, or a known gap, is recorded in `allow`:

```json
{
  "kind": "missing_path",
  "reference": "schemas/cloud-api/v1.json",
  "doc": "docs/adr/0002-typescript-modular-cloud-monolith.md",
  "reason": "Planned cloud API schema."
}
```

`kind` and `reference` must equal the finding's fields; `doc` optionally limits
the entry to one document; `reason` is mandatory. An entry that no longer
matches a finding is itself reported, so the allowlist shrinks when the planned
thing ships and cannot hide later drift.
