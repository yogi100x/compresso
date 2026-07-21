# Atlas parser design through E3-06

## Pattern and rationale

Use one error-tolerant Tree-sitter pipeline that accepts a pinned grammar, a
committed query asset, and a language-specific capture normalizer. Rust,
TypeScript, and TSX share capture ordering and byte-range deduplication while
retaining separate grammar and query inputs. TypeScript normalization filters
definitions to module scope, qualifies class methods, skips declarations that
have no stable name, and maps only concrete implementations rather than
overload or interface member signatures.

TypeScript and TSX remain distinct registry dialects because JSX changes the
grammar. Their query files are also separate live assets so future TSX-specific
captures can evolve without conditional query construction.

## Complexity

- Tree parsing and query execution are linear in source size for ordinary input.
- Sorting `c` captures is `O(c log c)`; deduplication and normalization are
  expected `O(c)`.
- Memory is the syntax tree plus `O(c + s + i)` for captures, symbols, and
  imports.

## Failure modes and handling

- Grammar setup, incompatible query, or missing syntax tree returns a typed
  `ParseError`; no parser path panics.
- Malformed files retain every complete capture before or around error nodes.
- Missing names, unexpected parents, destructuring declarations, and anonymous
  defaults are skipped without discarding other symbols.
- Unsupported routed languages return `UnsupportedLanguage` until their tickets
  add a grammar and live query.
- Imports and export-from edges are normalized without quotes but remain
  explicitly `Unresolved("not_indexed")` until indexing resolves them.

## References

- [Tree-sitter: Using Parsers](https://tree-sitter.github.io/tree-sitter/using-parsers/)
- [Tree-sitter: Query Syntax and error recovery](https://tree-sitter.github.io/tree-sitter/using-parsers/queries/1-syntax.html)
