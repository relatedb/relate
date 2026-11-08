//! The host-session seam: the tool calls the verbs make, so tests can
//! drive them with a recording fake. A sibling of the dungeon child's
//! seam, kept here because that one is crate-private to its package.

use relate_wire::mcp::{McpChild, StdioChild};
use serde_json::Value;

/// One `tools/call` against the host; returns the tool's result JSON.
pub trait HostSession {
    fn call(&mut self, tool: &str, arguments: Value) -> Result<Value, String>;
}

impl HostSession for StdioChild {
    fn call(&mut self, tool: &str, arguments: Value) -> Result<Value, String> {
        // The inherent `McpChild::call`; the kit's `From<McpChildError>
        // for String` carries the failure out.
        Ok(McpChild::call(self, tool, arguments)?)
    }
}
