//! Read-only MCP server and tool-handler boundaries.

mod framing;
mod profile;
mod reminder;
mod server;
mod session;
mod tools;

pub use framing::MAX_INCOMING_MESSAGE_BYTES;
pub use profile::{McpToolProfile, lean_tool_allowlist};
pub use reminder::{ROUTING_REMINDER_MAX_BYTES, routing_reminder};
pub use server::{
    McpDeps, McpInputError, McpLedgerHooks, McpRootError, McpServeOptions, advertised_surface,
    serve_io, serve_io_for_root, serve_io_with_hooks, serve_io_with_hooks_for_root,
    serve_io_with_options, serve_io_with_recall, serve_io_with_tokenizer_startup, serve_stdio,
    serve_stdio_for_root, serve_stdio_with_options, serve_stdio_with_recall, server_instructions,
    tool_names,
};
pub use tools::recall::McpRecallOptions;
