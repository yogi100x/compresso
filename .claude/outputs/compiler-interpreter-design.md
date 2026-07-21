# Atlas parser design through E3-08

## Pattern and rationale

Use one error-tolerant Tree-sitter pipeline that accepts a pinned grammar, a
committed query asset, and a language-specific capture normalizer. Rust,
TypeScript, TSX, JavaScript, and Python share capture ordering and byte-range
deduplication while retaining separate grammar and query inputs. Script-language
normalization filters definitions to module scope, qualifies class methods, and
skips declarations that have no stable name. TypeScript maps only concrete
implementations rather than overload or interface member signatures.

TypeScript and TSX remain distinct registry dialects because JSX changes the
grammar. Their query files are also separate live assets so future TSX-specific
captures can evolve without conditional query construction.

JavaScript captures CommonJS dependencies syntactically but accepts them only
when the callee is the identifier `require` and its sole argument is a string
literal. It records exact module-level `module.exports` assignments separately
from ordinary or nested assignments, without attempting semantic execution.

Python captures definition nodes rather than their decorated wrappers. This
keeps source ranges anchored at `def`/`async def` while the normalizer prepends
decorator text to the signature. Only module functions and classes plus direct
methods of module classes are emitted; nested declarations are skipped. A
constant must be a direct module assignment to one identifier whose spelling is
strict ASCII `UPPER_CASE`. Import aliases normalize to the original module or
member, and multi-name imports expand in source order without resolving them.

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
- Dynamic/template CommonJS loads and member calls named `require` are ignored;
  static string-literal calls remain explicit unresolved imports.
- Python destructuring, attribute assignments, lowercase names, nested
  declarations, and wildcard imports are ignored because they do not produce a
  stable symbol or dependency identity under the E3-08 contract.
- Imports and export-from edges are normalized without quotes but remain
  explicitly `Unresolved("not_indexed")` until indexing resolves them.

## References

- [Tree-sitter: Using Parsers](https://tree-sitter.github.io/tree-sitter/using-parsers/)
- [Tree-sitter: Query Syntax and error recovery](https://tree-sitter.github.io/tree-sitter/using-parsers/queries/1-syntax.html)
- [Tree-sitter Python grammar](https://github.com/tree-sitter/tree-sitter-python/blob/master/grammar.js)
