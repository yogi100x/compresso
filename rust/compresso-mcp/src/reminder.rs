//! Bounded tool-routing reminder re-surfaced after a client compacts context.
//!
//! The MCP `initialize` instructions are delivered once per server session. A
//! coding client that later summarizes its conversation can drop them, and an
//! MCP server is never told that compaction happened. Clients that expose a
//! post-compaction hook can run `compresso mcp routing-reminder` to put this short,
//! constant text back into context.

/// Hard upper bound on the reminder size in bytes.
pub const ROUTING_REMINDER_MAX_BYTES: usize = 640;

const ROUTING_REMINDER: &str = "Compresso routing reminder: the Compresso MCP tools remain available after context compaction and are still the preferred way to explore this repository. Use one compresso_context call for a broad task and answer from its pack. For precise follow-ups use compresso_search, compresso_outline, compresso_unfold, or compresso_read instead of client-native search or whole-file reads. Use compresso_compress before passing long text onward. If a tool reports the repository is unindexed, run `compresso index` and retry.";

const _: () = assert!(ROUTING_REMINDER.len() <= ROUTING_REMINDER_MAX_BYTES);

/// Returns the constant, bounded routing reminder.
///
/// The text is static: it contains no repository content, session data, or
/// timestamps, so emitting it any number of times yields identical bytes.
pub const fn routing_reminder() -> &'static str {
    ROUTING_REMINDER
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server_instructions;

    #[test]
    fn test_routing_reminder_is_bounded_single_line_ascii() {
        let reminder = routing_reminder();
        assert!(!reminder.is_empty());
        assert!(reminder.len() <= ROUTING_REMINDER_MAX_BYTES);
        assert!(reminder.is_ascii());
        assert!(!reminder.contains('\n'));
    }

    #[test]
    fn test_routing_reminder_names_only_tools_the_server_declares() {
        let instructions = server_instructions();
        let tools = routing_reminder()
            .split(|character: char| !(character.is_ascii_alphanumeric() || character == '_'))
            .filter(|word| word.starts_with("compresso_"))
            .collect::<Vec<_>>();
        assert!(tools.contains(&"compresso_context"));
        assert!(tools.len() >= 5);
        for tool in tools {
            assert!(instructions.contains(tool), "{tool} is not a served tool");
        }
    }

    #[test]
    fn test_routing_reminder_is_stable_across_calls() {
        assert_eq!(routing_reminder(), routing_reminder());
        assert_eq!(routing_reminder().as_ptr(), routing_reminder().as_ptr());
    }
}
