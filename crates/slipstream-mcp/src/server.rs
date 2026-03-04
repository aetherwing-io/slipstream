use std::path::{Path, PathBuf};
use std::sync::Arc;

use rmcp::{
    ErrorData as McpError,
    ServerHandler,
    handler::server::router::tool::ToolRouter,
    handler::server::wrapper::Parameters,
    model::*,
    tool, tool_handler, tool_router,
};
use tokio::sync::Mutex;

use slipstream_cli::client::{Client, ClientError};

use crate::params::*;

/// Inner state: connection config + optional connected client.
struct Inner {
    client: Option<Client>,
    socket_path: PathBuf,
    auto_start: bool,
}

impl Inner {
    /// Return a connected client, connecting lazily on first use.
    async fn ensure_connected(&mut self) -> Result<&mut Client, ClientError> {
        if self.client.is_none() {
            let c = Client::connect(&self.socket_path, self.auto_start).await?;
            self.client = Some(c);
        }
        Ok(self.client.as_mut().expect("just connected"))
    }
}

#[derive(Clone)]
pub struct SlipstreamServer {
    inner: Arc<Mutex<Inner>>,
    tool_router: ToolRouter<Self>,
}

/// Convert a client result into MCP tool result text.
/// Errors are returned as success with error text so the LLM can see and react to them.
fn to_tool_result(result: Result<serde_json::Value, ClientError>) -> Result<CallToolResult, McpError> {
    match result {
        Ok(value) => {
            let text = serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string());
            Ok(CallToolResult::success(vec![Content::text(text)]))
        }
        Err(ClientError::Rpc { code, message, data }) => {
            let mut err_text = format!("Error {code}: {message}");
            if let Some(d) = data {
                err_text.push_str(&format!("\n{}", serde_json::to_string_pretty(&d).unwrap_or_default()));
            }
            Ok(CallToolResult::success(vec![Content::text(err_text)]))
        }
        Err(e) => {
            Ok(CallToolResult::success(vec![Content::text(format!("Error: {e}"))]))
        }
    }
}

#[tool_router]
impl SlipstreamServer {
    /// Create a server with lazy daemon connection.
    /// Does NOT connect to the daemon — connection happens on first tool call.
    pub fn new(socket_path: &Path, auto_start: bool) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                client: None,
                socket_path: socket_path.to_path_buf(),
                auto_start,
            })),
            tool_router: Self::tool_router(),
        }
    }

    pub fn from_client(client: Client, socket_path: &Path) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                client: Some(client),
                socket_path: socket_path.to_path_buf(),
                auto_start: false,
            })),
            tool_router: Self::tool_router(),
        }
    }

    #[tool(description = "Open a slipstream session with one or more files. Returns session_id and file metadata (line counts, versions). The session_id is required for all subsequent operations.")]
    async fn slipstream_open(
        &self,
        Parameters(p): Parameters<OpenParams>,
    ) -> Result<CallToolResult, McpError> {
        let mut inner = self.inner.lock().await;
        let result = match inner.ensure_connected().await {
            Ok(client) => client
                .request("session.open", serde_json::json!({ "files": p.files }))
                .await,
            Err(e) => Err(e),
        };
        to_tool_result(result)
    }

    #[tool(description = "Read lines from a file in a slipstream session. Specify start+end for range read, count for cursor read, or neither for full file.")]
    async fn slipstream_read(
        &self,
        Parameters(p): Parameters<ReadParams>,
    ) -> Result<CallToolResult, McpError> {
        let mut params = serde_json::json!({
            "session_id": p.session_id,
            "path": p.path,
        });
        if let Some(start) = p.start {
            params["start"] = serde_json::json!(start);
        }
        if let Some(end) = p.end {
            params["end"] = serde_json::json!(end);
        }
        if let Some(count) = p.count {
            params["count"] = serde_json::json!(count);
        }
        let mut inner = self.inner.lock().await;
        let result = match inner.ensure_connected().await {
            Ok(client) => client.request("file.read", params).await,
            Err(e) => Err(e),
        };
        to_tool_result(result)
    }

    #[tool(description = "Low-level line-number write. Replaces lines [start, end) with content. Use start==end for insertion. WARNING: Line-number writes are error-prone — prefer slipstream_str_replace for editing. Only use this for insertions at a known line or when str_replace cannot match the target text. Not applied until flush.")]
    async fn slipstream_write(
        &self,
        Parameters(p): Parameters<WriteParams>,
    ) -> Result<CallToolResult, McpError> {
        let mut inner = self.inner.lock().await;
        let result = match inner.ensure_connected().await {
            Ok(client) => client
                .request("file.write", serde_json::json!({
                    "session_id": p.session_id,
                    "path": p.path,
                    "start": p.start,
                    "end": p.end,
                    "content": p.content,
                }))
                .await,
            Err(e) => Err(e),
        };
        to_tool_result(result)
    }

    #[tool(description = "RECOMMENDED way to edit files. Replace exact text match — pass old_str (the existing code) and new_str (the replacement). No line numbers needed. Requires exactly one match unless replace_all is true. Use this for ALL edits instead of slipstream_write.")]
    async fn slipstream_str_replace(
        &self,
        Parameters(p): Parameters<StrReplaceParams>,
    ) -> Result<CallToolResult, McpError> {
        let mut inner = self.inner.lock().await;
        let result = match inner.ensure_connected().await {
            Ok(client) => client
                .request("file.str_replace", serde_json::json!({
                    "session_id": p.session_id,
                    "path": p.path,
                    "old_str": p.old_str,
                    "new_str": p.new_str,
                    "replace_all": p.replace_all,
                }))
                .await,
            Err(e) => Err(e),
        };
        to_tool_result(result)
    }

    #[tool(description = "Move the read cursor to a specific line number in a slipstream session.")]
    async fn slipstream_cursor(
        &self,
        Parameters(p): Parameters<CursorParams>,
    ) -> Result<CallToolResult, McpError> {
        let mut inner = self.inner.lock().await;
        let result = match inner.ensure_connected().await {
            Ok(client) => client
                .request("cursor.move", serde_json::json!({
                    "session_id": p.session_id,
                    "path": p.path,
                    "to": p.to,
                }))
                .await,
            Err(e) => Err(e),
        };
        to_tool_result(result)
    }

    #[tool(description = "Flush all pending edits to disk in a slipstream session. Detects conflicts with other sessions unless force=true.")]
    async fn slipstream_flush(
        &self,
        Parameters(p): Parameters<FlushParams>,
    ) -> Result<CallToolResult, McpError> {
        let mut inner = self.inner.lock().await;
        let result = match inner.ensure_connected().await {
            Ok(client) => client
                .request("session.flush", serde_json::json!({
                    "session_id": p.session_id,
                    "force": p.force,
                }))
                .await,
            Err(e) => Err(e),
        };
        to_tool_result(result)
    }

    #[tool(description = "Close a slipstream session and release all resources. Unflushed edits are discarded.")]
    async fn slipstream_close(
        &self,
        Parameters(p): Parameters<CloseParams>,
    ) -> Result<CallToolResult, McpError> {
        let mut inner = self.inner.lock().await;
        let result = match inner.ensure_connected().await {
            Ok(client) => client
                .request("session.close", serde_json::json!({
                    "session_id": p.session_id,
                }))
                .await,
            Err(e) => Err(e),
        };
        to_tool_result(result)
    }

    #[tool(description = "Execute a batch of read/write/cursor operations in a single call. Each op: {\"method\": \"file.read\"|\"file.write\"|\"file.str_replace\"|\"cursor.move\", ...params}. IMPORTANT: For edits, prefer file.str_replace ops ({\"method\": \"file.str_replace\", \"path\": \"...\", \"old_str\": \"...\", \"new_str\": \"...\"}) over file.write. Line-number writes are error-prone.")]
    async fn slipstream_batch(
        &self,
        Parameters(p): Parameters<BatchParams>,
    ) -> Result<CallToolResult, McpError> {
        let mut inner = self.inner.lock().await;
        let result = match inner.ensure_connected().await {
            Ok(client) => client
                .request("batch", serde_json::json!({
                    "session_id": p.session_id,
                    "ops": p.ops,
                }))
                .await,
            Err(e) => Err(e),
        };
        to_tool_result(result)
    }

    #[tool(description = "Get full coordinator status: all tracked files, pending edits, external registrations, and warnings. Use after context compaction to recover file state.")]
    async fn slipstream_status(
        &self,
        Parameters(_p): Parameters<StatusParams>,
    ) -> Result<CallToolResult, McpError> {
        let mut inner = self.inner.lock().await;
        let result = match inner.ensure_connected().await {
            Ok(client) => client
                .request("coordinator.status", serde_json::json!({}))
                .await,
            Err(e) => Err(e),
        };
        to_tool_result(result)
    }

    #[tool(description = "Register an externally-managed file with the coordinator. Call this after opening a file in an FCP server (sheets, drawio, midi, terraform) so slipstream can track its state.")]
    async fn slipstream_register(
        &self,
        Parameters(p): Parameters<RegisterParams>,
    ) -> Result<CallToolResult, McpError> {
        let mut inner = self.inner.lock().await;
        let result = match inner.ensure_connected().await {
            Ok(client) => client
                .request("coordinator.register", serde_json::json!({
                    "path": p.path,
                    "handler": p.handler,
                }))
                .await,
            Err(e) => Err(e),
        };
        to_tool_result(result)
    }

    #[tool(description = "Remove an externally-managed file from coordinator tracking. Call this after the external tool has saved and closed the file.")]
    async fn slipstream_unregister(
        &self,
        Parameters(p): Parameters<UnregisterParams>,
    ) -> Result<CallToolResult, McpError> {
        let mut inner = self.inner.lock().await;
        let result = match inner.ensure_connected().await {
            Ok(client) => client
                .request("coordinator.unregister", serde_json::json!({
                    "tracking_id": p.tracking_id,
                }))
                .await,
            Err(e) => Err(e),
        };
        to_tool_result(result)
    }

    #[tool(description = "Pre-flight check before an action. Use action=\"build\" to check if all files are saved. Returns warnings (unflushed native edits, unsaved external files) and a suggestion.")]
    async fn slipstream_check(
        &self,
        Parameters(p): Parameters<CheckParams>,
    ) -> Result<CallToolResult, McpError> {
        let mut inner = self.inner.lock().await;
        let result = match inner.ensure_connected().await {
            Ok(client) => client
                .request("coordinator.check", serde_json::json!({
                    "action": p.action,
                }))
                .await,
            Err(e) => Err(e),
        };
        to_tool_result(result)
    }

    #[tool(description = "All-in-one: open files, optionally read them, apply batch operations, optionally flush to disk, and close. Combines open+batch+flush+close into a single tool call. Use read_all=true to get file contents. Pass ops as an array of file.str_replace/file.read/file.write operations. Use flush=true to write changes to disk.")]
    async fn slipstream_exec(
        &self,
        Parameters(p): Parameters<ExecParams>,
    ) -> Result<CallToolResult, McpError> {
        let mut inner = self.inner.lock().await;
        let client = match inner.ensure_connected().await {
            Ok(c) => c,
            Err(e) => return to_tool_result(Err(e)),
        };

        let mut output = serde_json::Map::new();

        // 1. Open session
        let open_result = match client
            .request("session.open", serde_json::json!({ "files": p.files }))
            .await
        {
            Ok(v) => v,
            Err(e) => return to_tool_result(Err(e)),
        };

        let session_id = match open_result["session_id"].as_str() {
            Some(s) => s.to_string(),
            None => return to_tool_result(Err(ClientError::Rpc {
                code: -1,
                message: "session.open did not return session_id".to_string(),
                data: None,
            })),
        };

        output.insert("open".to_string(), open_result);

        // 2. Read all files if requested
        if p.read_all {
            let read_ops: Vec<serde_json::Value> = p.files.iter()
                .map(|f| serde_json::json!({ "method": "file.read", "path": f }))
                .collect();
            match client.request("batch", serde_json::json!({
                "session_id": session_id,
                "ops": read_ops,
            })).await {
                Ok(v) => { output.insert("read".to_string(), v); }
                Err(e) => {
                    // Close session before returning error
                    let _ = client.request("session.close", serde_json::json!({
                        "session_id": session_id,
                    })).await;
                    return to_tool_result(Err(e));
                }
            }
        }

        // 3. Apply ops if provided
        if let Some(ops) = p.ops {
            match client.request("batch", serde_json::json!({
                "session_id": session_id,
                "ops": ops,
            })).await {
                Ok(v) => { output.insert("batch".to_string(), v); }
                Err(e) => {
                    let _ = client.request("session.close", serde_json::json!({
                        "session_id": session_id,
                    })).await;
                    return to_tool_result(Err(e));
                }
            }
        }

        // 4. Flush if requested
        if p.flush {
            match client.request("session.flush", serde_json::json!({
                "session_id": session_id,
                "force": p.force,
            })).await {
                Ok(v) => { output.insert("flush".to_string(), v); }
                Err(e) => {
                    let _ = client.request("session.close", serde_json::json!({
                        "session_id": session_id,
                    })).await;
                    return to_tool_result(Err(e));
                }
            }
        }

        // 5. Always close
        match client.request("session.close", serde_json::json!({
            "session_id": session_id,
        })).await {
            Ok(v) => { output.insert("close".to_string(), v); }
            Err(e) => return to_tool_result(Err(e)),
        }

        to_tool_result(Ok(serde_json::Value::Object(output)))
    }
}

#[tool_handler]
impl ServerHandler for SlipstreamServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            instructions: Some(
                "Slipstream is a file session coordinator. It tracks all files across native text editing and external format handlers (FCP servers for .xlsx, .drawio, .mid, .tf). \
                 Open files with slipstream_open — for native text files (py, rs, ts, etc.), you get a session_id for editing. \
                 For external formats (xlsx, drawio, mid, tf), you get guidance on which FCP tool to use. \
                 IMPORTANT: Use slipstream_str_replace (or file.str_replace in batch) for all text edits — it matches exact text without line numbers. \
                 After opening external files in their FCP tool, call slipstream_register to track them. \
                 Before running a build, call slipstream_check(action=\"build\") to verify all files are saved. \
                 Call slipstream_status at any time to see the full state of all tracked files.".into()
            ),
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            ..Default::default()
        }
    }
}
