# Claude Code MCP manual verification

## Verification record

- Verification date: 2026-07-22 (Asia/Kolkata)
- Client: Claude Code 2.1.216
- Server: Compresso 0.1.0, release build from commit `21ef9a3`
- Platform: macOS Apple ARM64
- Repository under test: `/Users/yogi/compresso`
- Result: PASS

The release binary was packaged as
`compresso-0.1.0-aarch64-apple-darwin.tar.gz`, verified against its generated
`SHA256SUMS` entry, and installed through `installers/install-local.sh` at
`~/.local/bin/compresso`. The installed binary passed all six `compresso doctor`
checks. The repository was then indexed before the client session.

Register the installed stdio server in Claude Code with this exact command:

```sh
claude mcp add compresso -- ~/.local/bin/compresso mcp serve
```

The verification session was noninteractive and used `--strict-mcp-config`
with only the installed Compresso server. Built-in tools were disabled and the
allowlist contained exactly the ten `mcp__compresso__*` tools listed below.
Claude reported the server as `connected` and exposed exactly those ten tools.
The `compresso_grep` row was added later (E17-22) from a direct stdio probe
of a debug build, not from that client session; its latency is measured by the probe client.

Client-side latency is the elapsed time between Claude Code's streamed
`tool_use` and matching `tool_result` events. It includes client/MCP transport
overhead and is distinct from any duration reported inside a tool result.

## Live checklist

| Check | Tool | Arguments used | Observed response | Client latency | Schema mismatch |
| --- | --- | --- | --- | ---: | --- |
| [x] | `compresso_compress` | `mode=safe`, `max_tokens=100`, `max_latency_ms=200`, 312-byte repeated build log | Reduced OpenAI count 60 to 9 with `collapse_logs`; no fallback; operation `01KY44T01E0RY5QD973YAT9128` | 159 ms | None |
| [x] | `compresso_context` | `root=/Users/yogi/compresso`, task `trace MCP stdio startup and tool declaration flow`, `max_tokens=800` | Returned a versioned task context pack using 192 tokens; bounded result reported `truncated=true` | 207 ms | None |
| [x] | `compresso_explain` | `operation_id=01KY44T01E0RY5QD973YAT9128` | Returned the matching v1 safe-mode trace with four strategy results and eleven safety checks | 145 ms | None |
| [x] | `compresso_grep` | Added by E17-22 and verified separately on 2026-11-18 over raw stdio (debug build, throwaway store, this repository indexed): `patterns=["is_secret_path","MAX_READ_FILE_BYTES"]`, `path=rust/**/*.rs`, `max_tokens=1500` | 179 files in scope, all scanned; 52 lines in 7 files and 12 lines in 4 files counted, 36 listed with enclosing symbols in 1,455 tokens; `complete=false` with a note giving the 28 unlisted lines | 653 ms | None |
| [x] | `compresso_impact` | `root=/Users/yogi/compresso`, `path=rust/compresso-core/src/graph.rs`, `max_depth=2`, `max_results=20` | Returned 12 dependents; explicitly reported 1,410 unresolved edges, zero ambiguous edges, and `complete=false` | 368 ms | None |
| [x] | `compresso_outline` | `root=/Users/yogi/compresso`, `path=rust/compresso-mcp/src/server.rs`, `max_tokens=1000` | Returned nine symbols using 995 tokens with explicit budget truncation | 272 ms | None |
| [x] | `compresso_read` | Gate: `path=rust/compresso-mcp/src/server.rs`, `gate=true`, `max_tokens=200`; traversal: `path=../../etc/passwd`, `max_tokens=100`; both with the repository root | Gate returned only a verdict for a 5,775-token file and no content field; traversal returned the expected `outside_root` tool error | 352 ms gate; 198 ms traversal | None |
| [x] | `compresso_repo_map` | `root=/Users/yogi/compresso`, `focus=rust/compresso-mcp`, `max_tokens=600` | Returned 32 of 359 files at exactly 600 tokens with explicit truncation | 296 ms | None |
| [x] | `compresso_search` | `root=/Users/yogi/compresso`, `query=server_instructions`, `limit=5`, `max_tokens=400` | Returned one hit using 72 tokens without truncation | 189 ms | None |
| [x] | `compresso_stats` | `since=24h` | Returned nine recorded operations and 51 net tokens saved at the time of the call | 159 ms | None |
| [x] | `compresso_unfold` | `root=/Users/yogi/compresso`, `path=rust/compresso-mcp/src/server.rs`, `qualified_name=server_instructions`, `max_tokens=500` | Returned `status=found`, 72 characters, and 17 tokens without truncation | 336 ms | None |
| [x] | `compresso_trace` | Raw stdio JSON-RPC session (not a client session; see note below) against a release build bound to a pinned snapshot of this repository in a throwaway store: `from=Indexer::index`, `to=replace_symbols_in_transaction`; `to=repository_stale`; `from=plan_context`; `to=new`; `to=rust/compresso-mcp/src/tools/common.rs:map_atlas_error` | One four-hop path with call sites and `resolution` per hop; six callers of `repository_stale` (`total=6`, `complete=true`, matching a text search); 22 callees with one ambiguous call listed; `status=ambiguous` with 10 of 92 candidates; ten users of `map_atlas_error`, most passing it to `map_err` (`reference: true`) | 80-158 ms round trip | None |

`compresso_trace` (E17-25) was added after that session. Its row was checked on 2026-11-18 over raw stdio JSON-RPC (`initialize`, `tools/list` showing eleven tools, four `tools/call` requests) with `HOME` pointed at a scratch directory, not through a coding client; a client session should re-check it.

The session made 11 calls: every tool once, plus the second `compresso_read`
call required for traversal rejection. The session completed successfully. A
targeted inspection of warning/error-level client log records found zero
handshake, schema, or protocol errors and zero MCP connection-failure records;
client stderr was empty. No tool schema deviation was observed.

## Gate-mode evidence

Sanitized client-side result summary:

```json
{"path":"rust/compresso-mcp/src/server.rs","gate":{"worth_reading":true,"file_tokens_total":5775,"recommended_budget":220},"tokens_used":63,"token_provider":"openai_o200k"}
```

The live result had a `gate` object and no `content` field.

## Traversal rejection evidence

Sanitized client-side transcript excerpt:

```text
client -> compresso_read {"root":"/Users/yogi/compresso","path":"../../etc/passwd","max_tokens":100}
server -> [outside_root] The requested path resolves outside the approved repository root.
```

The traversal rejection was an expected MCP tool error, not a protocol error.
Raw client logs and repository content are intentionally not committed; this
runbook retains only arguments, bounded metadata, latency observations, and the
required rejection excerpt.
