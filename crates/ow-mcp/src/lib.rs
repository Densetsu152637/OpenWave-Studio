//! The Model Context Protocol server of OpenWave Studio.
//!
//! An MCP client (Claude Code, Claude Desktop) starts `openwave-studio mcp`, which speaks MCP
//! over stdio and relays every line to the running app over a per-user local socket. The app
//! hosts the real server: it parses the JSON-RPC messages, runs the tools against its control
//! layer and answers. This crate holds everything that does not need the app:
//!
//! * [`protocol`]: the JSON-RPC 2.0 / MCP (revision 2025-06-18) message handling, generic over
//!   a [`ToolBackend`] the app implements.
//! * [`tools`]: the tool catalog (names, descriptions, JSON Schemas generated from the typed
//!   argument structs and from the `ow-model` edit commands) and the typed [`ToolCall`].
//! * [`endpoint`]: where the socket and the session token live, and how they are protected.
//! * [`listener`]: the socket server (named pipe on Windows, Unix domain socket elsewhere); every
//!   connection must present the session token first.
//! * [`client`]: the stdio relay `openwave-studio mcp` runs, including launching the app.
//!
//! There is no network listener anywhere: only a local pipe or socket that is accessible to the
//! current user alone. No Tauri types appear here, so all of it is testable without a window.

pub mod client;
pub mod endpoint;
pub mod listener;
pub mod protocol;
pub mod tools;

mod secure;

pub use protocol::{PROTOCOL_VERSION, Server, ToolBackend, ToolError, ToolOutput};
pub use tools::{ToolCall, ToolSpec, catalog};
