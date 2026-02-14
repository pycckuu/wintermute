//! MCP JSON-RPC 2.0 client over stdin/stdout (feature-dynamic-integrations).
//!
//! Implements the Model Context Protocol handshake (`initialize`),
//! tool discovery (`tools/list`), and tool invocation (`tools/call`)
//! over a child process's stdin/stdout pipes.

use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, ChildStdout};

// ── Error types ──

/// Errors from MCP client operations (feature-dynamic-integrations).
#[derive(Debug, Error)]
pub enum McpError {
    /// I/O error communicating with the MCP server process.
    #[error("MCP I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// JSON serialization/deserialization error.
    #[error("MCP JSON error: {0}")]
    Json(#[from] serde_json::Error),

    /// The MCP server returned a JSON-RPC error response.
    #[error("MCP server error {code}: {message}")]
    ServerError {
        /// JSON-RPC error code.
        code: i64,
        /// Error message from the server.
        message: String,
    },

    /// Protocol-level error (unexpected response format, missing fields).
    #[error("MCP protocol error: {0}")]
    ProtocolError(String),

    /// MCP initialize handshake failed.
    #[error("MCP initialize failed: {0}")]
    InitFailed(String),
}

// ── MCP protocol types ──

/// JSON-RPC 2.0 request (spec: JSON-RPC 2.0, MCP transport).
#[derive(Debug, Serialize)]
struct JsonRpcRequest<'a> {
    jsonrpc: &'static str,
    id: u64,
    method: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<serde_json::Value>,
}

/// JSON-RPC 2.0 response (spec: JSON-RPC 2.0).
#[derive(Debug, Deserialize)]
struct JsonRpcResponse {
    #[allow(dead_code)]
    jsonrpc: String,
    #[allow(dead_code)]
    id: Option<serde_json::Value>,
    result: Option<serde_json::Value>,
    error: Option<JsonRpcError>,
}

/// JSON-RPC 2.0 error object.
#[derive(Debug, Deserialize)]
struct JsonRpcError {
    code: i64,
    message: String,
}

/// A tool discovered via MCP `tools/list` (feature-dynamic-integrations, spec 6.11).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolDef {
    /// Tool name as reported by the MCP server.
    pub name: String,
    /// Optional description of what the tool does.
    pub description: Option<String>,
    /// JSON Schema for the tool's input parameters.
    #[serde(default = "default_empty_object")]
    pub input_schema: serde_json::Value,
    /// Optional MCP annotations (readOnlyHint, destructiveHint, etc.).
    #[serde(default)]
    pub annotations: McpToolAnnotations,
}

/// MCP tool annotations for semantics inference (feature-dynamic-integrations, spec 4.4).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpToolAnnotations {
    /// If true, the tool only reads data and has no side effects.
    pub read_only_hint: Option<bool>,
    /// If true, the tool performs destructive/irreversible operations.
    pub destructive_hint: Option<bool>,
}

fn default_empty_object() -> serde_json::Value {
    serde_json::json!({})
}

/// Result content from an MCP `tools/call` response.
#[derive(Debug, Clone, Deserialize)]
pub struct McpCallResult {
    /// Content blocks returned by the tool.
    #[serde(default)]
    pub content: Vec<McpContent>,
    /// Whether the tool invocation was an error.
    #[serde(default, rename = "isError")]
    pub is_error: bool,
}

/// A single content block in an MCP tool result.
#[derive(Debug, Clone, Deserialize)]
pub struct McpContent {
    /// Content type ("text", "image", "resource").
    #[serde(rename = "type")]
    pub content_type: String,
    /// Text content (for type="text").
    pub text: Option<String>,
}

// ── MCP Client ──

/// JSON-RPC 2.0 client communicating with an MCP server over stdin/stdout
/// (feature-dynamic-integrations).
///
/// Each MCP server is a child process. The client writes JSON-RPC requests
/// to the child's stdin and reads responses from stdout, one JSON object
/// per line (newline-delimited JSON).
pub struct McpClient {
    stdin: ChildStdin,
    reader: BufReader<ChildStdout>,
    next_id: AtomicU64,
}

impl McpClient {
    /// Create a new MCP client from child process pipes.
    pub fn new(stdin: ChildStdin, stdout: ChildStdout) -> Self {
        Self {
            stdin,
            reader: BufReader::new(stdout),
            next_id: AtomicU64::new(1),
        }
    }

    /// Perform the MCP `initialize` handshake (feature-dynamic-integrations).
    ///
    /// Sends `initialize` with client capabilities, then sends
    /// `notifications/initialized` to complete the handshake.
    /// Returns the server's capabilities on success.
    pub async fn initialize(&mut self) -> Result<serde_json::Value, McpError> {
        let params = serde_json::json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {
                "name": "pfar",
                "version": env!("CARGO_PKG_VERSION")
            }
        });

        let result = self.call("initialize", Some(params)).await?;

        // Send initialized notification (no response expected).
        self.send_notification("notifications/initialized", None)
            .await?;

        Ok(result)
    }

    /// Discover available tools via MCP `tools/list` (feature-dynamic-integrations).
    pub async fn list_tools(&mut self) -> Result<Vec<McpToolDef>, McpError> {
        let result = self.call("tools/list", None).await?;

        let tools_value = result
            .get("tools")
            .ok_or_else(|| McpError::ProtocolError("tools/list: missing 'tools' field".into()))?;

        let tools: Vec<McpToolDef> = serde_json::from_value(tools_value.clone())?;
        Ok(tools)
    }

    /// Invoke a tool via MCP `tools/call` (feature-dynamic-integrations).
    pub async fn call_tool(
        &mut self,
        name: &str,
        args: serde_json::Value,
    ) -> Result<McpCallResult, McpError> {
        let params = serde_json::json!({
            "name": name,
            "arguments": args
        });

        let result = self.call("tools/call", Some(params)).await?;
        let call_result: McpCallResult = serde_json::from_value(result)?;
        Ok(call_result)
    }

    /// Send a JSON-RPC request and wait for the response.
    async fn call(
        &mut self,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> Result<serde_json::Value, McpError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);

        let request = JsonRpcRequest {
            jsonrpc: "2.0",
            id,
            method,
            params,
        };

        // Write request as a single line + newline.
        let mut request_bytes = serde_json::to_vec(&request)?;
        request_bytes.push(b'\n');
        self.stdin.write_all(&request_bytes).await?;
        self.stdin.flush().await?;

        // Read response lines, skipping notifications (no "id" field).
        loop {
            let mut line = String::new();
            let bytes_read = self.reader.read_line(&mut line).await?;
            if bytes_read == 0 {
                return Err(McpError::ProtocolError(
                    "MCP server closed stdout unexpectedly".into(),
                ));
            }

            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            let response: JsonRpcResponse = serde_json::from_str(trimmed)?;

            // Skip notifications (messages without an id or with null id).
            if response.id.is_none() || response.id.as_ref().is_some_and(|v| v.is_null()) {
                continue;
            }

            if let Some(err) = response.error {
                return Err(McpError::ServerError {
                    code: err.code,
                    message: err.message,
                });
            }

            return response.result.ok_or_else(|| {
                McpError::ProtocolError("response has neither result nor error".into())
            });
        }
    }

    /// Send a JSON-RPC notification (no id, no response expected).
    async fn send_notification(
        &mut self,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> Result<(), McpError> {
        #[derive(Serialize)]
        struct JsonRpcNotification<'a> {
            jsonrpc: &'static str,
            method: &'a str,
            #[serde(skip_serializing_if = "Option::is_none")]
            params: Option<serde_json::Value>,
        }

        let notification = JsonRpcNotification {
            jsonrpc: "2.0",
            method,
            params,
        };

        let mut bytes = serde_json::to_vec(&notification)?;
        bytes.push(b'\n');
        self.stdin.write_all(&bytes).await?;
        self.stdin.flush().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── McpToolDef deserialization ──

    #[test]
    fn test_mcp_tool_def_full() {
        let json = serde_json::json!({
            "name": "search",
            "description": "Search Notion pages",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string" }
                }
            },
            "annotations": {
                "readOnlyHint": true
            }
        });

        let tool: McpToolDef =
            serde_json::from_value(json).expect("should deserialize full tool def");
        assert_eq!(tool.name, "search");
        assert_eq!(tool.description.as_deref(), Some("Search Notion pages"));
        assert_eq!(tool.annotations.read_only_hint, Some(true));
        assert!(tool.annotations.destructive_hint.is_none());
    }

    #[test]
    fn test_mcp_tool_def_minimal() {
        let json = serde_json::json!({
            "name": "do_something"
        });

        let tool: McpToolDef =
            serde_json::from_value(json).expect("should deserialize minimal tool def");
        assert_eq!(tool.name, "do_something");
        assert!(tool.description.is_none());
        assert!(tool.annotations.read_only_hint.is_none());
        assert!(tool.annotations.destructive_hint.is_none());
        assert!(tool.input_schema.is_object());
    }

    #[test]
    fn test_mcp_tool_annotations_destructive() {
        let json = serde_json::json!({
            "name": "delete_page",
            "description": "Delete a Notion page",
            "annotations": {
                "readOnlyHint": false,
                "destructiveHint": true
            }
        });

        let tool: McpToolDef =
            serde_json::from_value(json).expect("should deserialize destructive tool");
        assert_eq!(tool.annotations.read_only_hint, Some(false));
        assert_eq!(tool.annotations.destructive_hint, Some(true));
    }

    // ── McpCallResult deserialization ──

    #[test]
    fn test_mcp_call_result_success() {
        let json = serde_json::json!({
            "content": [
                { "type": "text", "text": "Found 3 pages matching query" }
            ]
        });

        let result: McpCallResult =
            serde_json::from_value(json).expect("should deserialize success result");
        assert!(!result.is_error);
        assert_eq!(result.content.len(), 1);
        assert_eq!(result.content[0].content_type, "text");
        assert_eq!(
            result.content[0].text.as_deref(),
            Some("Found 3 pages matching query")
        );
    }

    #[test]
    fn test_mcp_call_result_error() {
        let json = serde_json::json!({
            "content": [
                { "type": "text", "text": "Authentication failed" }
            ],
            "isError": true
        });

        let result: McpCallResult =
            serde_json::from_value(json).expect("should deserialize error result");
        assert!(result.is_error);
    }

    #[test]
    fn test_mcp_call_result_empty_content() {
        let json = serde_json::json!({});

        let result: McpCallResult =
            serde_json::from_value(json).expect("should deserialize empty result");
        assert!(!result.is_error);
        assert!(result.content.is_empty());
    }

    // ── JSON-RPC request serialization ──

    #[test]
    fn test_jsonrpc_request_serialization() {
        let request = JsonRpcRequest {
            jsonrpc: "2.0",
            id: 1,
            method: "tools/list",
            params: None,
        };

        let json = serde_json::to_value(&request).expect("should serialize");
        assert_eq!(json["jsonrpc"], "2.0");
        assert_eq!(json["id"], 1);
        assert_eq!(json["method"], "tools/list");
        assert!(json.get("params").is_none());
    }

    #[test]
    fn test_jsonrpc_request_with_params() {
        let request = JsonRpcRequest {
            jsonrpc: "2.0",
            id: 42,
            method: "tools/call",
            params: Some(serde_json::json!({
                "name": "search",
                "arguments": { "query": "test" }
            })),
        };

        let json = serde_json::to_value(&request).expect("should serialize");
        assert_eq!(json["id"], 42);
        assert_eq!(json["params"]["name"], "search");
    }

    // ── JSON-RPC response deserialization ──

    #[test]
    fn test_jsonrpc_response_success() {
        let json = r#"{"jsonrpc":"2.0","id":1,"result":{"tools":[]}}"#;
        let response: JsonRpcResponse =
            serde_json::from_str(json).expect("should deserialize success response");
        assert!(response.result.is_some());
        assert!(response.error.is_none());
    }

    #[test]
    fn test_jsonrpc_response_error() {
        let json =
            r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"Method not found"}}"#;
        let response: JsonRpcResponse =
            serde_json::from_str(json).expect("should deserialize error response");
        assert!(response.result.is_none());
        let err = response.error.expect("should have error");
        assert_eq!(err.code, -32601);
        assert_eq!(err.message, "Method not found");
    }

    #[test]
    fn test_jsonrpc_notification_no_id() {
        let json = r#"{"jsonrpc":"2.0","method":"notifications/tools/list_changed"}"#;
        let response: JsonRpcResponse =
            serde_json::from_str(json).expect("should deserialize notification");
        assert!(response.id.is_none());
    }

    // ── Integration test with mock process (uses tokio pipe) ──

    #[tokio::test]
    async fn test_mcp_client_initialize_and_list_tools() {
        // Spawn a mock MCP server that responds to initialize and tools/list.
        let mut child = tokio::process::Command::new("bash")
            .arg("-c")
            .arg(concat!(
                r#"read line; "#,
                r#"echo '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2024-11-05","capabilities":{},"serverInfo":{"name":"mock","version":"1.0"}}}'; "#,
                r#"read line; "#, // notifications/initialized (no response)
                r#"read line; "#, // tools/list
                r#"echo '{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"search","description":"Search pages","inputSchema":{"type":"object","properties":{"query":{"type":"string"}}},"annotations":{"readOnlyHint":true}},{"name":"delete","description":"Delete a page","annotations":{"destructiveHint":true}}]}}'; "#,
            ))
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("should spawn mock MCP server");

        let stdin = child.stdin.take().expect("should have stdin");
        let stdout = child.stdout.take().expect("should have stdout");

        let mut client = McpClient::new(stdin, stdout);

        // Initialize.
        let caps = client
            .initialize()
            .await
            .expect("initialize should succeed");
        assert_eq!(caps["protocolVersion"], "2024-11-05");

        // List tools.
        let tools = client
            .list_tools()
            .await
            .expect("tools/list should succeed");
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].name, "search");
        assert_eq!(tools[0].annotations.read_only_hint, Some(true));
        assert_eq!(tools[1].name, "delete");
        assert_eq!(tools[1].annotations.destructive_hint, Some(true));

        let _ = child.kill().await;
    }

    #[tokio::test]
    async fn test_mcp_client_call_tool() {
        let mut child = tokio::process::Command::new("bash")
            .arg("-c")
            .arg(concat!(
                r#"read line; "#, // request
                r#"echo '{"jsonrpc":"2.0","id":1,"result":{"content":[{"type":"text","text":"result data"}]}}'; "#,
            ))
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("should spawn mock");

        let stdin = child.stdin.take().expect("stdin");
        let stdout = child.stdout.take().expect("stdout");

        let mut client = McpClient::new(stdin, stdout);
        let result = client
            .call_tool("search", serde_json::json!({"query": "test"}))
            .await
            .expect("call_tool should succeed");

        assert!(!result.is_error);
        assert_eq!(result.content.len(), 1);
        assert_eq!(result.content[0].text.as_deref(), Some("result data"));

        let _ = child.kill().await;
    }

    #[tokio::test]
    async fn test_mcp_client_server_error() {
        let mut child = tokio::process::Command::new("bash")
            .arg("-c")
            .arg(concat!(
                r#"read line; "#,
                r#"echo '{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"Method not found"}}'; "#,
            ))
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("should spawn mock");

        let stdin = child.stdin.take().expect("stdin");
        let stdout = child.stdout.take().expect("stdout");

        let mut client = McpClient::new(stdin, stdout);
        let err = client
            .call("test/method", None)
            .await
            .expect_err("should return error");

        match err {
            McpError::ServerError { code, message } => {
                assert_eq!(code, -32601);
                assert_eq!(message, "Method not found");
            }
            other => panic!("expected ServerError, got: {other}"),
        }

        let _ = child.kill().await;
    }

    #[tokio::test]
    async fn test_mcp_client_skips_notifications() {
        // Server sends a notification before the actual response.
        let mut child = tokio::process::Command::new("bash")
            .arg("-c")
            .arg(concat!(
                r#"read line; "#,
                r#"echo '{"jsonrpc":"2.0","method":"notifications/progress","params":{"progress":50}}'; "#,
                r#"echo '{"jsonrpc":"2.0","id":1,"result":{"ok":true}}'; "#,
            ))
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("should spawn mock");

        let stdin = child.stdin.take().expect("stdin");
        let stdout = child.stdout.take().expect("stdout");

        let mut client = McpClient::new(stdin, stdout);
        let result = client
            .call("test/method", None)
            .await
            .expect("should skip notification and return result");
        assert_eq!(result["ok"], true);

        let _ = child.kill().await;
    }
}
