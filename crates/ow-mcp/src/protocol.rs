//! JSON-RPC 2.0 and the subset of MCP (revision 2025-06-18) a tools-only server needs:
//! `initialize`, `notifications/initialized`, `ping`, `tools/list` and `tools/call`.
//!
//! Messages are single lines of JSON (the stdio framing MCP defines); [`Server::handle_line`]
//! takes one and answers with at most one. The server is synchronous: the listener runs it on a
//! blocking thread, because tools lock the project and render audio.

use std::sync::Arc;

use serde_json::{Value, json};

use crate::tools::{ParseError, ToolCall, ToolSpec, catalog};

/// The MCP revision this server implements.
pub const PROTOCOL_VERSION: &str = "2025-06-18";

/// Revisions a client may ask for; the server then answers with the same one.
const SUPPORTED_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;

/// What a tool answers: structured content plus a short text summary (both go to the client).
#[derive(Debug, Clone, PartialEq)]
pub struct ToolOutput {
    /// The result as JSON.
    pub structured: Value,
    /// One or two sentences for the model.
    pub summary: String,
}

impl ToolOutput {
    /// A result.
    pub fn new(structured: Value, summary: impl Into<String>) -> Self {
        Self {
            structured,
            summary: summary.into(),
        }
    }
}

/// A tool that ran and failed (shown to the model as an error result, not a protocol error).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolError(pub String);

impl<T: Into<String>> From<T> for ToolError {
    fn from(message: T) -> Self {
        Self(message.into())
    }
}

/// What the app implements: runs one typed tool call. Called from blocking threads, possibly
/// from several connections at once.
pub trait ToolBackend: Send + Sync {
    /// Runs `call`.
    fn call(&self, call: ToolCall) -> Result<ToolOutput, ToolError>;
}

/// The MCP server over a backend.
pub struct Server {
    backend: Arc<dyn ToolBackend>,
    tools: Vec<ToolSpec>,
    version: String,
}

impl Server {
    /// A server for `backend`; `version` is the app version reported in `serverInfo`.
    pub fn new(backend: Arc<dyn ToolBackend>, version: impl Into<String>) -> Self {
        Self {
            backend,
            tools: catalog(),
            version: version.into(),
        }
    }

    /// Handles one message line. Returns the response line (without the newline), or `None` for
    /// notifications and responses.
    pub fn handle_line(&self, line: &str) -> Option<String> {
        let message: Value = match serde_json::from_str(line) {
            Ok(message) => message,
            Err(error) => {
                return Some(error_line(Value::Null, PARSE_ERROR, &error.to_string()));
            }
        };
        if !message.is_object() {
            // Batches were removed from MCP in 2025-06-18.
            return Some(error_line(
                Value::Null,
                INVALID_REQUEST,
                "a request must be a JSON object (batches are not supported)",
            ));
        }
        let id = message.get("id").cloned();
        let Some(method) = message.get("method").and_then(Value::as_str) else {
            // A response to a request of ours (there are none), or garbage.
            return match id {
                Some(id) if message.get("result").is_none() && message.get("error").is_none() => {
                    Some(error_line(id, INVALID_REQUEST, "missing method"))
                }
                _ => None,
            };
        };
        let params = message.get("params").cloned().unwrap_or(Value::Null);
        let Some(id) = id else {
            // A notification: nothing to answer (`notifications/initialized`, `cancelled`).
            return None;
        };
        let line = match self.dispatch(method, params) {
            Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string(),
            Err((code, message)) => error_line(id, code, &message),
        };
        Some(line)
    }

    fn dispatch(&self, method: &str, params: Value) -> Result<Value, (i64, String)> {
        match method {
            "initialize" => Ok(self.initialize(&params)),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(self.list_tools()),
            "tools/call" => self.call_tool(params),
            other => Err((METHOD_NOT_FOUND, format!("method not found: {other}"))),
        }
    }

    fn initialize(&self, params: &Value) -> Value {
        let asked = params.get("protocolVersion").and_then(Value::as_str);
        let version = asked
            .filter(|asked| SUPPORTED_VERSIONS.contains(asked))
            .unwrap_or(PROTOCOL_VERSION);
        json!({
            "protocolVersion": version,
            "capabilities": {"tools": {"listChanged": false}},
            "serverInfo": {"name": "openwave-studio", "title": "OpenWave Studio", "version": self.version},
            "instructions": "Drives the running OpenWave Studio app: read the project with get_project, change it with edit (reserve ids with alloc_ids first), mix and master with edit and analyze, and render with export_wav. The agent has the user's file access for imports, saves and exports. There are no generative music tools: write notes and arrange clips yourself.",
        })
    }

    fn list_tools(&self) -> Value {
        let tools: Vec<Value> = self
            .tools
            .iter()
            .map(|tool| {
                json!({
                    "name": tool.name,
                    "description": tool.description,
                    "inputSchema": tool.input_schema,
                })
            })
            .collect();
        json!({ "tools": tools })
    }

    fn call_tool(&self, params: Value) -> Result<Value, (i64, String)> {
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .ok_or((INVALID_PARAMS, "tools/call needs a tool name".to_owned()))?;
        let arguments = params.get("arguments").cloned().unwrap_or(Value::Null);
        let call = match ToolCall::parse(name, arguments) {
            Ok(call) => call,
            Err(ParseError::Unknown(name)) => {
                return Err((INVALID_PARAMS, format!("unknown tool: {name}")));
            }
            // The model can fix its arguments when it sees why they were refused.
            Err(ParseError::Arguments(why)) => {
                return Ok(error_result(&format!(
                    "invalid arguments for {name}: {why}"
                )));
            }
        };
        Ok(match self.backend.call(call) {
            Ok(output) => json!({
                "content": [{"type": "text", "text": output.summary}],
                "structuredContent": output.structured,
                "isError": false,
            }),
            Err(ToolError(message)) => error_result(&message),
        })
    }
}

fn error_result(message: &str) -> Value {
    json!({
        "content": [{"type": "text", "text": message}],
        "isError": true,
    })
}

fn error_line(id: Value, code: i64, message: &str) -> String {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Echo;

    impl ToolBackend for Echo {
        fn call(&self, call: ToolCall) -> Result<ToolOutput, ToolError> {
            match call {
                ToolCall::GetProject(_) => Ok(ToolOutput::new(json!({"ok": true}), "the project")),
                other => Err(ToolError(format!("{} failed", other.name()))),
            }
        }
    }

    fn ask(server: &Server, request: Value) -> Value {
        serde_json::from_str(&server.handle_line(&request.to_string()).unwrap()).unwrap()
    }

    fn server() -> Server {
        Server::new(Arc::new(Echo), "1.2.3")
    }

    #[test]
    fn initialize_answers_with_capabilities_and_the_requested_version() {
        let answer = ask(
            &server(),
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26"}}),
        );
        assert_eq!(answer["id"], 1);
        assert_eq!(answer["result"]["protocolVersion"], "2025-03-26");
        assert!(answer["result"]["capabilities"]["tools"].is_object());
        assert_eq!(answer["result"]["serverInfo"]["version"], "1.2.3");
        let newer = ask(
            &server(),
            json!({"jsonrpc":"2.0","id":2,"method":"initialize","params":{"protocolVersion":"2099-01-01"}}),
        );
        assert_eq!(newer["result"]["protocolVersion"], PROTOCOL_VERSION);
    }

    #[test]
    fn notifications_get_no_answer_and_ping_works() {
        let server = server();
        assert!(
            server
                .handle_line(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
                .is_none()
        );
        assert_eq!(
            ask(&server, json!({"jsonrpc":"2.0","id":"a","method":"ping"}))["result"],
            json!({})
        );
    }

    #[test]
    fn tools_list_and_call() {
        let server = server();
        let listed = ask(
            &server,
            json!({"jsonrpc":"2.0","id":3,"method":"tools/list"}),
        );
        let tools = listed["result"]["tools"].as_array().unwrap();
        assert!(tools.iter().any(|tool| tool["name"] == "edit"));
        assert!(
            tools
                .iter()
                .all(|tool| tool["inputSchema"]["type"] == "object")
        );

        let called = ask(
            &server,
            json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"get_project","arguments":{}}}),
        );
        assert_eq!(called["result"]["isError"], false);
        assert_eq!(called["result"]["structuredContent"]["ok"], true);
        assert_eq!(called["result"]["content"][0]["text"], "the project");

        let failed = ask(
            &server,
            json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"undo"}}),
        );
        assert_eq!(failed["result"]["isError"], true);
        assert_eq!(failed["result"]["content"][0]["text"], "undo failed");
    }

    #[test]
    fn protocol_errors_have_json_rpc_codes() {
        let server = server();
        let unknown_tool = ask(
            &server,
            json!({"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"nope"}}),
        );
        assert_eq!(unknown_tool["error"]["code"], INVALID_PARAMS);
        let unknown_method = ask(&server, json!({"jsonrpc":"2.0","id":7,"method":"nope"}));
        assert_eq!(unknown_method["error"]["code"], METHOD_NOT_FOUND);
        let garbage: Value = serde_json::from_str(&server.handle_line("{nope").unwrap()).unwrap();
        assert_eq!(garbage["error"]["code"], PARSE_ERROR);
        let batch: Value = serde_json::from_str(&server.handle_line("[]").unwrap()).unwrap();
        assert_eq!(batch["error"]["code"], INVALID_REQUEST);
        let bad_arguments = ask(
            &server,
            json!({"jsonrpc":"2.0","id":8,"method":"tools/call","params":{"name":"alloc_ids","arguments":{"count":"x"}}}),
        );
        assert_eq!(bad_arguments["result"]["isError"], true);
    }
}
