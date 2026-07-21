# Rust parser extraction design

## Chosen pattern and rationale

Compresso uses tree-sitter-rust with a committed query for broad, error-tolerant item capture. Query captures are converted into Store-owned symbol records in source-byte order. Small Rust-specific normalization steps qualify implementation methods and expand grouped `use` declarations while preserving declaration order.

This keeps the parser boundary concrete and deterministic, reuses tree-sitter's incremental-capable syntax model, and allows complete items in malformed files to remain indexable. Other recognized languages return a typed unsupported-language error until their dedicated tickets land.

## Complexity

Parsing is linear in source size for ordinary inputs. Captures are sorted by byte range in `O(c log c)`, where `c` is the number of query captures, and range deduplication is `O(c)` expected time. Symbol conversion is linear in captured text. Grouped-use expansion is linear in the declaration text plus the emitted import paths.

## Failure modes and handling

- Parser or language setup failures return typed `ParseError` values.
- Invalid extraction queries fail at the registry boundary rather than producing partial, misleading records.
- A missing syntax tree returns `ParseError::ParseFailed`.
- Malformed Rust is parsed error-tolerantly; complete captured items are returned and the API does not panic.
- Unsupported languages return `ParseError::UnsupportedLanguage` without fabricating symbols.
- Imports remain explicitly unresolved with `not_indexed` until incremental indexing resolves them.
- Duplicate captures are removed by byte range, and all output is ordered deterministically by source position.

## References

- [Tree-sitter: Using Parsers](https://tree-sitter.github.io/tree-sitter/using-parsers/)
- [Tree-sitter: Query Syntax and error recovery](https://tree-sitter.github.io/tree-sitter/using-parsers/queries/1-syntax.html)
