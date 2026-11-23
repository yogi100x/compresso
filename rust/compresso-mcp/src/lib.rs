//! MCP server and tool-handler boundaries. No tool changes the repository
//! except the operator-enabled `compresso_edit` (E17-32, D97, ADR-0035), and
//! no tool opens a network connection except the operator-enabled,
//! read-only `compresso_sql` and `compresso_mongo`, through `compresso-sql`
//! (E17-50, D115, ADR-0036; E17-52, D117, ADR-0038);
//! `compresso_report` saves bug and feedback reports as local files beside
//! the Compresso database (E17-48, D113).

// D117: no unsafe code (no FFI process spawning); the capability check
// requires this line.
#![forbid(unsafe_code)]

mod framing;
mod profile;
mod reminder;
mod server;
mod session;
mod tools;

pub use framing::MAX_INCOMING_MESSAGE_BYTES;
pub use profile::{McpResultFormat, McpToolProfile, lean_tool_allowlist};
pub use reminder::{ROUTING_REMINDER_MAX_BYTES, routing_reminder};
pub use server::{
    McpDeps, McpInputError, McpLedgerHooks, McpRootError, McpServeOptions,
    advertised_lean_session_surface, advertised_surface, advertised_surface_for,
    advertised_surface_with, advertised_surface_with_databases, serve_io, serve_io_for_root,
    serve_io_with_hooks, serve_io_with_hooks_for_root, serve_io_with_options, serve_io_with_recall,
    serve_io_with_tokenizer_startup, serve_stdio, serve_stdio_for_root, serve_stdio_with_options,
    serve_stdio_with_recall, server_instructions, tool_names,
};
pub use tools::mongo::MongoSurface;
pub use tools::recall::McpRecallOptions;
pub use tools::sql::SqlSurface;

/// The MongoDB tool's operator settings (E17-52, D117).
pub use compresso_sql::mongo::{MongoSettings, is_valid_mongo_env_name};

/// The SQL tool's operator settings and engine kinds (E17-50, D115).
pub use compresso_sql::{SqlEngine, SqlSettings, is_valid_env_name as is_valid_sql_env_name};

/// The edit report forms and gates shared with `compresso edit`.
pub mod edit {
    pub use crate::tools::edit::{
        edit_failure_message, fit, parse_operations_value, protected_paths, report_payload,
    };

    /// Name of the opt-in write tool.
    pub const TOOL_NAME: &str = crate::tools::edit::TOOL_NAME;

    /// The compact text form of an edit report payload.
    pub fn render_text(payload: &serde_json::Value) -> String {
        crate::tools::text::render(TOOL_NAME, payload).unwrap_or_default()
    }
}
