// mcp — MCP Streamable HTTP クライアント（SPEC §7, §14-6）。
//
// サブモジュール構成:
//   manager.rs  — McpManager / McpServer（サーバ接続・ツールカタログ・tools/call）
//   protocol.rs — JSON-RPC 2.0 型と MCP メッセージ型
//   transport.rs — POST ベースの Streamable HTTP トランスポート

pub mod manager;
pub mod protocol;
pub mod transport;

pub use manager::McpManager;
