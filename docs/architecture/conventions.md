# Repository conventions

These conventions apply across the Compresso workspace. Architecture decisions
and ticket-specific acceptance criteria remain the source of truth when they are
more specific.

## Errors

- Library crates define focused, typed error enums with `thiserror`.
- `anyhow` is allowed only at executable boundaries, where additional context
  helps an operator act on a failure. Library APIs must not expose `anyhow`.
- Errors should identify the failed operation and preserve the original source
  error when one exists. Do not encode machine-readable state in error strings.
- Compression failures fail open to byte-identical original content. Protected
  content is never mutated.

## Modules and dependencies

- Keep `lib.rs` focused on the public API and module declarations. Put each
  cohesive capability in a focused module rather than a growing catch-all file.
- Keep binaries thin: argument parsing and edge context belong in the binary;
  reusable behavior belongs in library crates.
- Follow the dependency and capability matrix in
  `docs/plan/01-stack-and-workspace.md`. In particular, `compresso-core` has no
  internal crate dependency and local-processing crates do no network I/O or
  process execution.
- Prefer explicit, typed domain values at crate boundaries. Validate untrusted
  repository and client input before it reaches implementation modules.

## Naming and tests

- Rust modules, functions, variables, and test functions use `snake_case`;
  types and traits use `UpperCamelCase`; constants use `SCREAMING_SNAKE_CASE`.
- Test names use the `test_snake_case` form and describe observable behavior.
- Unit tests live beside the module they exercise. Cross-module, schema,
  workspace-boundary, and fixture tests live under the owning crate's `tests/`
  directory.
- Fixture directory names are kebab-case and must match the manifest `name`.

## Commits

- Every ticket ends in its own Conventional Commit, such as
  `feat(core): add context graph validation` or
  `test(eval): validate fixture manifests`.
- Keep commits scoped to the active ticket. Never stage unrelated or existing
  user changes.
- Update a ticket checkbox only after its acceptance criteria and relevant
  format, lint, test, and build checks pass.

## Versioned schemas

- Every cross-process, persistence, and public API schema carries an explicit
  `schema_version` field.
- Validate schema-backed values at every process or persistence boundary. Do
  not infer or silently upgrade an unknown version.
- Schema files live in versioned paths such as `schemas/context-graph/v1.json`.
  Breaking changes require a new version; compatible additions must preserve
  validation of existing documents.
- Runtime types, fixtures, examples, and tests must advance together when a
  schema version changes.
