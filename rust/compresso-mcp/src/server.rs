use std::borrow::Cow;
use std::io;
use std::sync::Arc;

use compresso_core::{Config, Engine};
use compresso_store::Store;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, Content, Implementation, ListToolsResult,
    PaginatedRequestParams, ServerCapabilities, ServerInfo, Tool, ToolAnnotations,
};
use rmcp::service::{
    RequestContext, RoleServer, RxJsonRpcMessage, ServerInitializeError, TxJsonRpcMessage,
};
use rmcp::transport::Transport;
use rmcp::{ErrorData as McpError, ServerHandler, ServiceExt};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;

const JSON_SCHEMA_VERSION: &str = "https://json-schema.org/draft/2020-12/schema";

const INSTRUCTIONS: &str = "Compresso provides token-efficient, read-only access to indexed repositories, plus local text compression. Tools: compresso_repo_map (ranked repository overview), compresso_search (indexed symbol search), compresso_outline (symbol skeleton of one file), compresso_unfold (exact body of one symbol), compresso_read (token-budgeted file read; gate:true returns a worth-reading verdict and cost estimate without content), compresso_context (task-scoped context pack), compresso_impact (reverse-dependency report for a file), compresso_compress (compress arbitrary text, returns before/after token counts), compresso_stats (local savings summary), compresso_explain (trace for a past operation). Every tool bounds its output by a max_tokens budget, executes no shell commands, and works entirely locally; no content leaves this machine. For navigation, outline plus unfold is usually cheaper than a full file read. If a tool reports a repository is not indexed, run `compresso index` in that repository and retry.";

const REPO_MAP_DESCRIPTION: &str = "Returns a token-budgeted map of an indexed repository: directory tree plus the most structurally important symbols per file, ranked by edge in-degree. Use this first when orienting in an unfamiliar repository; it is the cheapest way to decide where to look next. Read-only; requires the repository to have been indexed with `compresso index`.";
const SEARCH_DESCRIPTION: &str = "Searches the symbol index of a repository (names, qualified names, signatures; FTS5, bm25-ranked) and returns compact hits with file, line range, and signature. Cheaper and more precise than grep for \"where is X defined/used\" questions. Read-only; requires an indexed repository.";
const READ_DESCRIPTION: &str = "Reads a file (or line range) from an approved repository under a hard token budget, truncating with explicit elision markers when the budget is exceeded. With `gate: true` it returns only a worth-reading verdict and token cost estimate — no content — so you can decide whether a large file is worth the budget before spending it. Read-only; paths must be inside an approved repository root.";
const OUTLINE_DESCRIPTION: &str = "Returns the symbol skeleton of one file: every indexed symbol with kind, signature, line range, and per-symbol token estimate, but no bodies. Combine with compresso_unfold to fetch only the bodies you need — typically ~70% cheaper than a full file read for navigation. Read-only; requires an indexed repository.";
const UNFOLD_DESCRIPTION: &str = "Returns the exact indexed source body for one symbol. Optional identity fields disambiguate overloads; ambiguous selectors return bounded candidates instead of choosing arbitrarily. Read-only; requires an indexed repository and an approved path.";
const CONTEXT_DESCRIPTION: &str = "Builds a minimal context pack for a stated task within a token budget: a focused repository-map excerpt, outlines of the most relevant files, and the highest-value symbol bodies, selected by index search and edge ranking. Use it to start a task with one call instead of several exploratory reads. Read-only; requires an indexed repository.";
const IMPACT_DESCRIPTION: &str = "Returns a bounded reverse-dependency report for one indexed file. The result identifies dependency depth and reports unresolved or ambiguous edges so completeness is explicit. Read-only; requires an indexed repository and an approved path.";
const COMPRESS_DESCRIPTION: &str = "Compresses arbitrary text (logs, transcripts, search dumps, tool output) using Compresso's safe, deterministic strategy pipeline, and returns the compressed string with before/after token counts per provider. Safe mode is deterministic and rule-based (dedup, log collapsing, structured compaction, budget packing): removals are whole units, every removal is enumerated in the operation trace, protected content is never altered, and any failure returns the original text unchanged. It is not lossless; request passthrough mode for byte-exact needs. Use before passing long text to a sub-agent or follow-up prompt. Does not require an indexed repository.";
const STATS_DESCRIPTION: &str = "Returns Compresso's local savings summary: operations recorded, observed vs applied tokens, net tokens saved, and modeled cost saved, using the fixed savings vocabulary. All figures come from the local SQLite store; nothing is transmitted anywhere. Optional filters narrow by time window or session.";
const EXPLAIN_DESCRIPTION: &str = "Returns the full compression trace for one past operation: mode, strategies attempted and applied, per-strategy token deltas, safety-check outcomes, fallback status, and a human-readable explanation. Traces contain no raw content — only identifiers, kinds, counts, and explanations. Use it to understand exactly what Compresso did to a given result.";

/// Dependencies shared by the MCP server and its tool handlers.
pub struct McpDeps {
    pub store: Store,
    pub engine: Engine,
    pub config: Config,
}

struct CompressoServer {
    deps: McpDeps,
    tools: Vec<Tool>,
}

impl CompressoServer {
    fn new(deps: McpDeps) -> Self {
        Self {
            deps,
            tools: tool_declarations(),
        }
    }
}

impl ServerHandler for CompressoServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("compresso", env!("CARGO_PKG_VERSION")))
            .with_instructions(INSTRUCTIONS)
    }

    fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListToolsResult, McpError>> + Send + '_ {
        std::future::ready(Ok(ListToolsResult {
            tools: self.tools.clone(),
            ..ListToolsResult::default()
        }))
    }

    fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<CallToolResult, McpError>> + Send + '_ {
        let registered = self.tools.iter().any(|tool| tool.name == request.name);
        let atlas_tool = crate::tools::handles(&request.name);
        let store = self.deps.store.clone();
        async move {
            if atlas_tool {
                let name = request.name.into_owned();
                let arguments = request.arguments;
                return match tokio::task::spawn_blocking(move || {
                    crate::tools::dispatch(&store, &name, arguments)
                })
                .await
                {
                    Ok(result) => Ok(result),
                    Err(_) => Ok(CallToolResult::error(vec![Content::text(
                        "[internal] Compresso could not complete the request because of an internal error.",
                    )])),
                };
            }
            if registered {
                Ok(CallToolResult::error(vec![Content::text(format!(
                    "[internal] Tool handler '{}' is not implemented in E5-01.",
                    request.name
                ))]))
            } else {
                Err(McpError::invalid_params(
                    format!("unknown Compresso tool '{}'", request.name),
                    None,
                ))
            }
        }
    }
}

/// Starts the MCP server over process stdin/stdout and waits for disconnect.
pub async fn serve_stdio(deps: McpDeps) -> anyhow::Result<()> {
    let (reader, writer) = rmcp::transport::stdio();
    serve_io(deps, reader, writer).await
}

/// Starts the MCP server on injected async streams.
///
/// This is public so integration tests and future local transports can exercise
/// the same framing, handshake, and handler boundary as stdio.
pub async fn serve_io<R, W>(deps: McpDeps, reader: R, writer: W) -> anyhow::Result<()>
where
    R: AsyncRead + Send + Unpin + 'static,
    W: AsyncWrite + Send + Unpin + 'static,
{
    let transport = ResilientJsonLineTransport::new(reader, writer);
    let service = match CompressoServer::new(deps).serve(transport).await {
        Ok(service) => service,
        Err(ServerInitializeError::ConnectionClosed(_)) => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    service.waiting().await?;
    Ok(())
}

/// Returns the exact neutral instructions sent during MCP initialization.
pub const fn server_instructions() -> &'static str {
    INSTRUCTIONS
}

fn tool_declarations() -> Vec<Tool> {
    vec![
        tool(
            "compresso_repo_map",
            REPO_MAP_DESCRIPTION,
            json!({
                "root": { "type": "string", "description": "Absolute path to the repository root. Optional; defaults to the server working directory's approved repository." },
                "focus": { "type": "string", "description": "Repo-relative path prefix to bias ranking toward (e.g. 'src/auth')." },
                "max_tokens": token_budget(2000)
            }),
            &[],
        ),
        tool(
            "compresso_search",
            SEARCH_DESCRIPTION,
            json!({
                "root": { "type": "string", "description": "Absolute repository root. Optional." },
                "query": { "type": "string", "minLength": 1, "description": "Search text, e.g. 'SessionManager' or 'start session'." },
                "limit": { "type": "integer", "minimum": 1, "maximum": 100, "default": 20 },
                "max_tokens": token_budget(2000)
            }),
            &["query"],
        ),
        tool(
            "compresso_read",
            READ_DESCRIPTION,
            json!({
                "root": { "type": "string", "description": "Absolute repository root. Optional." },
                "path": { "type": "string", "minLength": 1, "description": "Repo-relative file path." },
                "start_line": { "type": "integer", "minimum": 1, "description": "1-based inclusive. Optional." },
                "end_line": { "type": "integer", "minimum": 1, "description": "1-based inclusive. Optional; requires start_line." },
                "max_tokens": token_budget(2000),
                "gate": { "type": "boolean", "default": false, "description": "If true, return verdict + cost estimate only, no content." }
            }),
            &["path"],
        ),
        tool(
            "compresso_outline",
            OUTLINE_DESCRIPTION,
            json!({
                "root": { "type": "string", "description": "Absolute repository root. Optional." },
                "path": { "type": "string", "minLength": 1, "description": "Repo-relative file path." },
                "max_tokens": token_budget(1500)
            }),
            &["path"],
        ),
        tool(
            "compresso_unfold",
            UNFOLD_DESCRIPTION,
            json!({
                "root": { "type": "string", "description": "Absolute approved repository root. Optional when cwd is approved." },
                "path": { "type": "string", "minLength": 1, "description": "Repository-relative file path." },
                "qualified_name": { "type": "string", "minLength": 1 },
                "kind": { "type": "string" },
                "signature_hash": { "type": "string", "pattern": "^[0-9a-f]{64}$" },
                "start_line_hint": { "type": "integer", "minimum": 1 },
                "max_tokens": token_budget(4000)
            }),
            &["path", "qualified_name"],
        ),
        tool(
            "compresso_context",
            CONTEXT_DESCRIPTION,
            json!({
                "root": { "type": "string", "description": "Absolute repository root. Optional." },
                "task": { "type": "string", "minLength": 3, "description": "Natural-language task, e.g. 'fix session timeout'." },
                "max_tokens": token_budget(4000)
            }),
            &["task"],
        ),
        tool(
            "compresso_impact",
            IMPACT_DESCRIPTION,
            json!({
                "root": { "type": "string", "description": "Absolute approved repository root. Optional when cwd is approved." },
                "path": { "type": "string", "minLength": 1, "description": "Repository-relative file path." },
                "max_depth": { "type": "integer", "minimum": 1, "maximum": 2, "default": 2 },
                "max_results": { "type": "integer", "minimum": 1, "maximum": 500, "default": 100 }
            }),
            &["path"],
        ),
        tool(
            "compresso_compress",
            COMPRESS_DESCRIPTION,
            json!({
                "text": { "type": "string", "minLength": 1, "description": "Raw text to compress." },
                "mode": { "type": "string", "enum": ["safe", "passthrough"], "default": "safe", "description": "Slice supports safe and passthrough (D10)." },
                "max_tokens": { "type": "integer", "minimum": 100, "maximum": 100000, "description": "Optional target budget; when set, pack_budget trims lowest-priority content to fit." },
                "max_latency_ms": { "type": "integer", "minimum": 1, "maximum": 10000, "default": 200 }
            }),
            &["text"],
        ),
        tool(
            "compresso_stats",
            STATS_DESCRIPTION,
            json!({
                "since": { "type": "string", "description": "ISO 8601 timestamp or relative duration ('24h', '7d', '30d'). Default: all retained history (90 days)." },
                "session_id": { "type": "string", "description": "Restrict to one session (ulid)." }
            }),
            &[],
        ),
        tool(
            "compresso_explain",
            EXPLAIN_DESCRIPTION,
            json!({
                "operation_id": { "type": "string", "minLength": 1, "description": "ulid returned by any Compresso tool result." }
            }),
            &["operation_id"],
        ),
    ]
}

fn token_budget(default: u64) -> Value {
    json!({
        "type": "integer",
        "minimum": 100,
        "maximum": 100000,
        "default": default
    })
}

fn tool(
    name: &'static str,
    description: &'static str,
    properties: Value,
    required: &[&str],
) -> Tool {
    let schema = json!({
        "$schema": JSON_SCHEMA_VERSION,
        "type": "object",
        "additionalProperties": false,
        "properties": properties,
        "required": required
    });
    let Value::Object(schema) = schema else {
        unreachable!("tool schema is constructed as an object")
    };
    Tool::new(Cow::Borrowed(name), Cow::Borrowed(description), schema).with_annotations(
        ToolAnnotations::new()
            .read_only(true)
            .destructive(false)
            .idempotent(true)
            .open_world(false),
    )
}

struct ResilientJsonLineTransport<R, W> {
    reader: BufReader<R>,
    writer: Arc<Mutex<W>>,
    buffer: Vec<u8>,
}

impl<R, W> ResilientJsonLineTransport<R, W>
where
    R: AsyncRead + Send + Unpin + 'static,
    W: AsyncWrite + Send + Unpin + 'static,
{
    fn new(reader: R, writer: W) -> Self {
        Self {
            reader: BufReader::new(reader),
            writer: Arc::new(Mutex::new(writer)),
            buffer: Vec::new(),
        }
    }
}

impl<R, W> Transport<RoleServer> for ResilientJsonLineTransport<R, W>
where
    R: AsyncRead + Send + Unpin + 'static,
    W: AsyncWrite + Send + Unpin + 'static,
{
    type Error = io::Error;

    fn send(
        &mut self,
        item: TxJsonRpcMessage<RoleServer>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'static {
        let writer = Arc::clone(&self.writer);
        async move {
            let mut frame = serde_json::to_vec(&item).map_err(io::Error::other)?;
            frame.push(b'\n');
            let mut writer = writer.lock().await;
            writer.write_all(&frame).await?;
            writer.flush().await
        }
    }

    async fn receive(&mut self) -> Option<RxJsonRpcMessage<RoleServer>> {
        loop {
            self.buffer.clear();
            match self.reader.read_until(b'\n', &mut self.buffer).await {
                Ok(0) => return None,
                Ok(_) => {}
                Err(_) => return None,
            }

            while matches!(self.buffer.last(), Some(b'\n' | b'\r')) {
                self.buffer.pop();
            }
            match serde_json::from_slice(&self.buffer) {
                Ok(message) => return Some(message),
                Err(_) => {
                    const PARSE_ERROR: &[u8] = b"{\"jsonrpc\":\"2.0\",\"id\":null,\"error\":{\"code\":-32700,\"message\":\"Parse error\"}}\n";
                    let mut writer = self.writer.lock().await;
                    if writer.write_all(PARSE_ERROR).await.is_err() || writer.flush().await.is_err()
                    {
                        return None;
                    }
                }
            }
        }
    }

    async fn close(&mut self) -> Result<(), Self::Error> {
        self.writer.lock().await.shutdown().await
    }
}
