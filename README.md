# Compresso

Compresso is a local-first context intelligence layer for AI coding tools. The
current delivery target is the offline vertical slice covering the compression
core, repository indexing, CLI, read-only MCP server, and supported client
integrations.

## Connecting a coding client

`compresso setup <claude-code|codex|cursor> <ROOT>` indexes and approves one
repository and writes the Compresso MCP server entry into that client's
configuration. `compresso integrations install <client>` writes only the
server entry. Both show the exact plan first (`--dry-run` previews without
writing), keep a backup and a receipt, and are undone with
`compresso integrations uninstall <client>`.

Session recall is on by default for Claude Code and Codex: the server entry
ends in `--recall-source <that client>`, which serves the `compresso_recall`
tool over that client's own local session transcripts. Recall is scoped to the
session's repository, read-only, secret-redacted, builds no index, and uploads
nothing; a client never gets another client's transcripts, and Cursor gets
none because its transcript format is unsupported. Pass `--no-recall` to
either command to leave it off, or re-run the command with `--no-recall` to
turn it off later. Compresso itself makes no network request, but anything an
MCP tool returns, recall snippets included, goes to the coding client, which
may send it to its model provider.

## Development

Development standards are documented in
[Repository conventions](docs/architecture/conventions.md).
