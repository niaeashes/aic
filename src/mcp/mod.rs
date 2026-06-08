// mcp — MCP Streamable HTTP client (SPEC §7, §14-6).
//
// Sub-modules:
//   manager.rs   — McpManager / McpServer (connections, tool catalog, tools/call)
//   protocol.rs  — JSON-RPC 2.0 types and MCP message types
//   transport.rs — POST-based Streamable HTTP transport

pub mod manager;
pub mod protocol;
pub mod transport;

pub use manager::McpManager;
