//! Read-only MCP server and tool-handler boundaries.

mod server;
mod session;
mod tools;

pub use server::{
    McpDeps, McpLedgerHooks, serve_io, serve_io_with_hooks, serve_io_with_tokenizer_startup,
    serve_stdio, server_instructions,
};
