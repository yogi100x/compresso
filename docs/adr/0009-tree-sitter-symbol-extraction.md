# 0009 — Tree-sitter symbol extraction

## Status

Accepted

## Context

Atlas needs symbols, signatures, line boundaries, and import relationships across many languages (build-doc §12), from files that are frequently mid-edit and syntactically broken. Full compilers or language servers per language are too heavy for a background indexer with incremental refresh. Decisions D15 and D8 lock the parser and the first-pass languages.

## Decision

Use tree-sitter with a parser registry: `tree-sitter-rust`, `tree-sitter-typescript` (TS + TSX), `tree-sitter-javascript`, and `tree-sitter-python` in the first slice, with per-language extraction queries in `queries/<lang>.scm` and the remaining build-doc §12.4 languages added later behind the same registry.

## Consequences

- Error-tolerant parsing produces usable outlines from incomplete code, which is the normal state of a working repository.
- One extraction pipeline and one normalized symbol vocabulary (02-schemas §4) serve every language; adding a language is a grammar, queries, and fixtures, not new architecture.
- Grammar crates must be version-pinned, and the parser version is stamped into the index (`repositories.parser_version`) so stale extractions are detectable and re-indexable.
- Tree-sitter yields syntax, not semantics: reference resolution stays heuristic, and the `code/mini-monorepo` fixture bounds what edge resolution must achieve.
