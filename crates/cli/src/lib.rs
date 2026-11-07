//! The host side of clanka: config and key setup, the companion's model over
//! OpenAI-compatible HTTP, the turn protocol shared by hosts, and the MCP and
//! terminal front ends. The `clanka` binary and the Hermes extension use it.

pub mod chat;
pub mod llm;
pub mod mcp;
pub mod setup;
pub mod turn;
