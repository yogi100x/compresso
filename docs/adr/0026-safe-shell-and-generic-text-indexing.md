# 0026 — Safe shell and generic-text indexing

## Status

Accepted

## Context

The first Atlas slice indexes AST symbols from Rust, TypeScript/TSX,
JavaScript, and Python. Repository-wide discovery still misses operational
behavior commonly held in shell scripts and supporting documentation. The
authentication benchmark exposed this directly: a token-refresh shell script
was relevant ground truth but could not enter Atlas search, forcing broader
native search or manual follow-up reads.

Using a shell interpreter would violate the untrusted-repository boundary.
Treating every non-binary file as generic text would expand the secret and
noise surface, index config/data formats without a contract, and create
unbounded FTS records. Adding a full shell grammar is also unnecessary for the
required recall target and would increase the dependency and malformed-input
surface.

## Decision

Add two concrete parser-registry packs:

- `Shell` accepts only `.sh`, `.bash`, and `.zsh`. A linear lexical scanner
  emits one module record whose bounded signature contains descriptive
  comments and identifier names, plus at most 256 conservative function
  records. Assignment values are excluded from the module signature. Function
  ranges use quote/comment-aware brace counting and stop after 400 lines when
  malformed input never closes.
- `Text` accepts only `.md`, `.markdown`, `.txt`, `.rst`, and `.adoc`. It emits
  Markdown/AsciiDoc heading sections when present and otherwise plain-text
  paragraphs. A file has at most 256 records; each record spans at most 40
  lines and its whitespace-normalized UTF-8 signature is at most 1,024 bytes.

Both scanners treat input as inert bytes decoded as UTF-8. They never execute a
process, expand a variable, resolve a command, interpret markup, follow a link
or include, or read a path named by content. The existing approved-root,
canonicalization, ignore, secret-pattern, binary, and one-million-byte file
limits remain authoritative. Config and data formats such as TOML, YAML, JSON,
and XML do not enter generic-text FTS.

Context planning is source-first. If any implementation record matches the
task, generic-text matches do not join the pack unless the task explicitly
asks for documentation, a README, guide, design, specification, ADR, or
Markdown. When no implementation matches, text remains the automatic fallback.
This prevents repeated prose mentions from displacing executable evidence
without requiring the user to select a tool or language.

The persisted language vocabulary gains `shell` and `text`; schema version 1
already permits arbitrary text values and therefore needs no migration. The
default parser version becomes the package version plus
`atlas-shell-text-v1`, causing a one-time full reparse for existing indexes
without changing the public package version.

(Since the Rust import resolver change the suffix is `atlas-shell-text-v1+rust-imports-v2`,
which again causes one full reparse so stored import edges are rebuilt.)

(Since call-site extraction, D89 and ADR-0033, the suffix is
`atlas-shell-text-v1+rust-imports-v2+calls-v1`; the one full reparse fills
the new `call_sites` table for indexes built before it.)

## Consequences

- Shell-backed operational workflows become discoverable in one context call
  without granting Atlas process execution.
- Documentation can contribute evidence, but bounded records prevent one large
  file from dominating storage or context assembly.
- Implementation questions stay concise even when the same terms occur many
  times in documentation; documentation-focused requests remain one-call.
- This is lexical retrieval, not semantic shell analysis. Here-documents,
  dynamically constructed commands, unusual function syntax, and content
  beyond a capped malformed function may be represented conservatively.
- Path-based secret filtering cannot identify a secret pasted into an
  otherwise ordinary source or documentation file. Such content remains
  subject to the same local approved-repository trust boundary and bounded
  read behavior as existing indexed source.
- Go, Java, C/C++, C#, Ruby, and PHP remain in E16; this decision does not
  authorize those language packs.
