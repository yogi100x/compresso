# ADR-0033: Call sites stored as written, resolved at query time

## Status

Proposed 2026-11-18 by the implementing session (D89, D90; E17-24, E17-25);
awaits owner confirmation.

## Context

Multi-file trace tasks ("how does X reach Y") cost about twenty model turns
with or without Compresso, because the index held symbols and file-level
import edges but no call edges: the model walked the chain by hand with
search and reads. A call graph needs a place in the store and a rule for
deciding which symbol a call names.

Two storage shapes were considered.

1. **Resolve while indexing and store the callee symbol id.** Symbol rows are
   deleted and reinserted whenever their file is re-indexed, and incremental
   indexing reparses only changed files. A resolved edge from `a.rs` into
   `b.rs` would point at a deleted row as soon as `b.rs` changed, so every
   index pass would need a repository-wide re-resolution pass, and an index
   pass that failed halfway would leave dangling edges.
2. **Store what the caller wrote; resolve when asked.** Each row is owned by
   the caller's file, written and deleted in that file's transaction, and the
   callee is looked up against the current symbols and import edges at query
   time.

## Decision

Option 2.

- **Table.** Migration `0008_call_sites.sql` adds `call_sites`
  (`repo_id`, `from_file_id`, `caller_symbol_id`, `callee_name`,
  `callee_qualifier`, `call_form`, `line`). All three references cascade on
  delete. `caller_symbol_id` is safe to store because the caller's symbols and
  calls are replaced in the same transaction. Indexes: caller, `(repo_id,
  callee_name)`, file.
- **Extraction.** The parsers walk the syntax tree the symbol pass already
  built (no second parse). A call is recorded when it lies inside an indexed
  function or method; the innermost one is the caller. One row per caller,
  callee name, qualifier, and form keeps the first call site. At most 4,096
  rows per file and 65,536 call expressions examined per file; reaching either
  bound is reported (`calls_truncated`, `call_site_files_truncated`). The tree
  walk and the dotted-qualifier check are iterative, and a qualifier longer
  than 256 bytes is rejected before it is walked, so nesting depth cannot
  exhaust the stack; the enclosing caller of each line is computed once per
  file by a sweep over the symbol ranges. Names must be identifier
  characters and at most 256 bytes; anything else from error recovery is
  dropped, so a malformed file cannot abort an index (as in E17-21).
  Covered: Rust, Python, JavaScript, TypeScript, TSX. Rust macro arguments
  are a flat token tree, so calls inside them (`assert!(run())`,
  `format!("{}", self.name())`) are read from tokens: a name followed by a
  parenthesized group, optionally behind `Type::`, `self.`, or `expr.`; a
  multi-segment path inside a macro is skipped. Not covered: shell, text,
  macro invocations themselves, calls at module level or in class bodies,
  and calls whose callee is not a name (closures, call results,
  subscripts).
- **Call forms.** `plain` (`run()`, `new Widget()`), `path` (`Store::open()`,
  `os.path.join()`, `api.v1.fetch()`), `self` (`self.x()`, `Self::x()`,
  `this.x()`, `cls.x()`), `receiver` (any other `expr.method()`), and
  `reference` (a bare name passed as a call argument, `map_err(to_failure)`,
  `sorted(xs, key=rank)`: a function used as a value).
- **Resolution (query time).** Candidates are indexed symbols with the
  callee's name, read through `Store::indexed_symbols_named(repo, name)`
  (every exact-name definition with its path; distinct from the context
  planner's bounded `Store::symbols_named(repo, name, limit)`, D88), narrowed
  by form: `plain` to functions, classes, and
  structs; `self` to methods of the caller's own type; `path` to methods of
  the type the qualifier names, or functions in the module file it names
  (falling back to a receiver call for Python and script variables, and to
  unresolved `external_path` for Rust paths such as `std::fs::read`);
  `receiver` to methods; `reference` to functions, and only in the caller's
  file or a file it imports (most bare arguments are variables, so anything
  else, including an ambiguous match, is silently dropped). Then, in order: exactly one candidate in the caller's
  file; else exactly one in a file the caller's file imports (resolved import
  edges); else exactly one in the repository. More than one at the first tier
  that has any is `ambiguous` and lists the candidates; none is
  `unresolved`. A `receiver` call resolved only by a repository-unique name is
  labelled `name_only`, because the receiver's type is unknown and a standard
  library method can share the name. Only resolved calls are followed. A
  library call (no repository symbol of that name, or a Rust path naming no
  repository type or module) cannot lead to a repository symbol and is not
  reported; an ambiguous call, or a `self` call outside any type, is listed
  and makes the answer incomplete.
- **Parser version.** The suffix gains `+calls-v1`, so an index built before
  this change reparses every file once and fills the table.

Review-round amendments (E17-29, D94): the enclosing caller is attributed by
byte position, so two functions sharing one line each keep their own calls
(files indexed earlier are corrected when next re-indexed; no version bump). A
qualifier that fails the name rule or the 256-byte cap drops only that call. If
the store still rejects a file's call sites, the file is indexed without them
and counted in `call_site_files_rejected`; one file never aborts the index.
Each trace reads at most 100,000 call-site rows and reports `truncated` when it
reaches that cap; resolutions are cached per trace. Trace availability is
gated on the `+calls-v1` parser-version marker, not on the default version.

## Consequences

- An incremental index never leaves a call pointing at a deleted symbol, and
  indexing needs no repository-wide pass. Measured on a pinned snapshot of
  this repository (386 files, 7,103 symbols, 28,578 call sites: 14,594
  receiver calls, 4,290 references), release build, macOS arm64, five
  interleaved full-index runs each: median 1,170 ms before and 1,347 ms after
  (about 15%; an earlier, busier round gave 1,325 and 1,525 ms); no-op index
  0.25 s before and 0.26-0.28 s after; store 7.9-8.8 MB before and 9.7-11 MB
  after.
- Resolution cost moves to the trace query, which bounds it by visited nodes.
- Resolution is by name, not by type. It can miss a call through a trait
  object, a stored callback, a re-export, an inherited method, or a call at
  module level, and a `name_only` hop can be wrong; every hop carries the
  rule that resolved it. A function passed by name as an argument is found;
  one stored in a field or returned is not.
- The ledger check makes migrations a prefix list, so a binary released before
  migration 0008 refuses a store this build has opened
  (`InvalidMigrationHistory`). Downgrading needs the newer binary or a fresh
  store.
