//! Read-only MCP server and tool-handler boundaries.

mod server;
mod tools;

pub use server::{McpDeps, serve_io, serve_stdio, server_instructions};
