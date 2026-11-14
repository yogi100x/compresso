//! Read-only MCP server and tool-handler boundaries.

mod reminder;
mod server;
mod session;
mod tools;

pub use reminder::{ROUTING_REMINDER_MAX_BYTES, routing_reminder};
pub use server::{
    McpDeps, McpLedgerHooks, McpRootError, serve_io, serve_io_for_root, serve_io_with_hooks,
    serve_io_with_hooks_for_root, serve_io_with_tokenizer_startup, serve_stdio,
    serve_stdio_for_root, server_instructions,
};
