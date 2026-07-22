//! Read-only MCP server and tool-handler boundaries.

mod server;

pub use server::{McpDeps, serve_io, serve_stdio, server_instructions};
