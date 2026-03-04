use std::collections::HashMap;
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
use crate::parse::{self, Query, SessionAction};

/// Inner state: connection config + optional connected client + named sessions.
struct Inner {
    client: Option<Client>,
    socket_path: PathBuf,
    auto_start: bool,
    /// Named session map: name → session_id (UUID from daemon).
    /// "default" is the implicit session name.
    sessions: HashMap<String, String>,
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

    /// Look up a named session → UUID. Defaults to "default" if name is None.
    fn resolve_session(&self, name: Option<&str>) -> Result<String, String> {
        let key = name.unwrap_or("default");
        self.sessions.get(key).cloned().ok_or_else(|| {
            if key == "default" {
                "no active session. Use slipstream_session('open <files>') first.".to_string()
            } else {
                format!(
                    "no active session '{key}'. Use slipstream_session('open <files> as:{key}') first."
                )
            }
        })
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

/// Convert a parse/resolve error into an MCP tool result.
fn err_result(msg: String) -> Result<CallToolResult, McpError> {
    Ok(CallToolResult::success(vec![Content::text(format!("Error: {msg}"))]))
}

const HELP_TEXT: &str = r#"# Slipstream Reference Card

## slipstream — main operations

Two modes:
- **One-shot** (`files` provided): open → read? → ops? → flush? → close
- **Session** (`files` omitted): apply ops to active named session

### Ops format (DSL strings and/or JSON objects — mix freely)

**DSL strings** — compact, great for single-line ops:
```
ops: [
  "str_replace f.rs old:\"foo\" new:\"bar\"",
  "str_replace f.rs old:\"old\" new:\"new\" replace_all",
  "read f.rs",
  "read f.rs start:0 end:20",
  "read f.rs count:50",
  "write f.rs start:0 end:0 content:\"// header\"",
  "cursor f.rs to:50"
]
```

**JSON objects** — use for multi-line content (avoids double-escaping):
```
ops: [
  {"method": "file.str_replace", "path": "f.rs", "old_str": "fn foo(\n    x: i32\n)", "new_str": "fn bar(\n    x: i64\n)"},
  {"method": "file.read", "path": "f.rs", "start": 0, "end": 20}
]
```

**Mixed** — combine both in one array:
```
ops: [
  "read f.rs",
  {"method": "file.str_replace", "path": "f.rs", "old_str": "multi\nline\ncontent", "new_str": "replacement"},
  "str_replace f.rs old:\"simple\" new:\"change\""
]
```

**Note**: str_replace uses substring matching — `old:"dispatch_op"` matches within `pub fn dispatch_op(`. No need to include the full line.

### DSL escape sequences
- `\n` → newline (for multi-line old/new strings)
- `\\` → literal backslash
- `\"` → literal quote

### When to use which format
- **DSL**: simple single-line replacements (80% of edits)
- **JSON**: multi-line content, special characters, or when content already has backslashes/quotes

## slipstream_session — lifecycle

| Action | Example |
|--------|---------|
| open | `open src/main.rs src/lib.rs` |
| open named | `open data.csv as:worker-1` |
| flush | `flush` or `flush session:worker-1` |
| flush force | `flush --force` |
| close | `close` or `close session:worker-1` |
| register | `register /path/file.xlsx sheets` |
| unregister | `unregister ext-001` |

## slipstream_query — read-only

| Query | Example |
|-------|---------|
| read full | `read src/main.rs` |
| read range | `read src/main.rs start:10 end:20` |
| read cursor | `read src/main.rs count:50` |
| read named | `read src/main.rs session:worker-1` |
| status | `status` |
| list | `list` |
| check | `check build` |

Note: `read` auto-opens files not in the session — no need to `open` first for reads.

## Common Workflows

**One-shot edit** (most common):
```
slipstream(files=["f.rs"], read_all=true, ops=["str_replace f.rs old:\"foo\" new:\"bar\""], flush=true)
```

**Multi-turn session**:
```
slipstream_session("open src/main.rs src/lib.rs")
slipstream_query("read src/main.rs")
slipstream(ops=["str_replace src/main.rs old:\"foo\" new:\"bar\""])
slipstream_session("flush")
slipstream_session("close")
```

**Concurrent subagents**:
```
Agent A: slipstream_session("open f1.rs as:a")
Agent B: slipstream_session("open f2.rs as:b")
Agent A: slipstream(session="a", ops=["str_replace f1.rs old:\"x\" new:\"y\""])
Agent B: slipstream(session="b", ops=["str_replace f2.rs old:\"x\" new:\"y\""])
Both:    slipstream_session("flush session:a"), slipstream_session("flush session:b")
Both:    slipstream_session("close session:a"), slipstream_session("close session:b")
```

**External files** (FCP coordination):
```
slipstream_session("register /path/file.xlsx sheets")
# ... use sheets tool to edit ...
slipstream_query("status")
slipstream_session("unregister ext-001")
```
"#;

/// Parse mixed DSL/JSON op items into JSON values for the daemon batch protocol.
fn parse_ops(items: &[crate::params::OpItem]) -> Result<serde_json::Value, String> {
    use crate::params::OpItem;
    let mut json_ops = Vec::with_capacity(items.len());
    for (i, item) in items.iter().enumerate() {
        match item {
            OpItem::Dsl(dsl) => match parse::parse_op(dsl) {
                Ok(op) => json_ops.push(op.to_json()),
                Err(e) => return Err(format!("op {i}: {e}")),
            },
            OpItem::Json(obj) => {
                // Validate that the JSON object has a "method" field
                if !obj.get("method").and_then(|v| v.as_str()).is_some() {
                    return Err(format!("op {i}: JSON object must have a \"method\" string field"));
                }
                json_ops.push(obj.clone());
            }
        }
    }
    Ok(serde_json::Value::Array(json_ops))
}

#[tool_router]
impl SlipstreamServer {
    /// Create a server with lazy daemon connection.
    pub fn new(socket_path: &Path, auto_start: bool) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                client: None,
                socket_path: socket_path.to_path_buf(),
                auto_start,
                sessions: HashMap::new(),
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
                sessions: HashMap::new(),
            })),
            tool_router: Self::tool_router(),
        }
    }

    #[tool(description = "File editing operations. Two modes: (1) One-shot with files=[...] — auto open/close, self-contained. (2) Session mode (files omitted) — ops run on active session. Ops accept DSL strings or JSON objects (mix freely): DSL: \"str_replace f.rs old:\\\"foo\\\" new:\\\"bar\\\"\". JSON: {\"method\": \"file.str_replace\", \"path\": \"f.rs\", \"old_str\": \"multi\\nline\", \"new_str\": \"new\"}. Use JSON for multi-line content to avoid double-escaping. Verbs: str_replace, read, write, cursor. Use read_all=true to get file contents, flush=true to write to disk.")]
    async fn slipstream(
        &self,
        Parameters(p): Parameters<SlipstreamParams>,
    ) -> Result<CallToolResult, McpError> {
        // Parse ops (DSL strings and/or JSON objects) → JSON array
        let json_ops = match p.ops {
            Some(ref items) => match parse_ops(items) {
                Ok(ops) => Some(ops),
                Err(msg) => return err_result(msg),
            },
            None => None,
        };

        if let Some(files) = p.files {
            // --- One-shot mode: open → read? → ops? → flush? → close ---
            self.exec_one_shot(files, json_ops, p.read_all, p.flush, p.force).await
        } else {
            // --- Session mode: look up named session → batch ---
            let mut inner = self.inner.lock().await;

            // Resolve session before borrowing client
            let session_id = match inner.resolve_session(p.session.as_deref()) {
                Ok(id) => id,
                Err(msg) => return err_result(msg),
            };
            let session_label = p.session.as_deref().unwrap_or("default").to_string();

            // If no ops, just return session info
            let ops = match json_ops {
                Some(ops) => ops,
                None => {
                    return Ok(CallToolResult::success(vec![Content::text(
                        format!("Session '{session_label}' active (id: {session_id}). Pass ops to apply operations.")
                    )]));
                }
            };

            let client = match inner.ensure_connected().await {
                Ok(c) => c,
                Err(e) => return to_tool_result(Err(e)),
            };

            let mut output = serde_json::Map::new();

            // Apply batch ops
            match client.request("batch", serde_json::json!({
                "session_id": session_id,
                "ops": ops,
            })).await {
                Ok(v) => { output.insert("batch".to_string(), v); }
                Err(e) => return to_tool_result(Err(e)),
            }

            // Flush if requested
            if p.flush {
                match client.request("session.flush", serde_json::json!({
                    "session_id": session_id,
                    "force": p.force,
                })).await {
                    Ok(v) => { output.insert("flush".to_string(), v); }
                    Err(e) => return to_tool_result(Err(e)),
                }
            }

            to_tool_result(Ok(serde_json::Value::Object(output)))
        }
    }

    #[tool(description = "Session lifecycle. Actions: open <files> [as:name], flush [--force] [session:name], close [session:name], register <path> <handler>, unregister <id>. Default session is implicit — most usage never needs naming.")]
    async fn slipstream_session(
        &self,
        Parameters(p): Parameters<SessionActionParams>,
    ) -> Result<CallToolResult, McpError> {
        let action = match parse::parse_session_action(&p.action) {
            Ok(a) => a,
            Err(msg) => return err_result(msg),
        };

        let mut inner = self.inner.lock().await;

        match action {
            SessionAction::Open { files, name } => {
                let session_name = name.unwrap_or_else(|| "default".to_string());

                // Check if name already in use
                if inner.sessions.contains_key(&session_name) {
                    return err_result(format!(
                        "session '{session_name}' already active. Close it first or use a different name."
                    ));
                }

                let client = match inner.ensure_connected().await {
                    Ok(c) => c,
                    Err(e) => return to_tool_result(Err(e)),
                };

                let result = client
                    .request("session.open", serde_json::json!({ "files": files }))
                    .await;

                match &result {
                    Ok(v) => {
                        if let Some(sid) = v["session_id"].as_str() {
                            inner.sessions.insert(session_name, sid.to_string());
                        }
                    }
                    Err(_) => {}
                }

                to_tool_result(result)
            }
            SessionAction::Flush { name, force } => {
                // Resolve before borrowing client
                let session_id = match inner.resolve_session(name.as_deref()) {
                    Ok(id) => id,
                    Err(msg) => return err_result(msg),
                };
                let client = match inner.ensure_connected().await {
                    Ok(c) => c,
                    Err(e) => return to_tool_result(Err(e)),
                };
                let result = client
                    .request("session.flush", serde_json::json!({
                        "session_id": session_id,
                        "force": force,
                    }))
                    .await;
                to_tool_result(result)
            }
            SessionAction::Close { name } => {
                let session_name = name.as_deref().unwrap_or("default").to_string();
                let session_id = match inner.resolve_session(Some(&session_name)) {
                    Ok(id) => id,
                    Err(msg) => return err_result(msg),
                };
                let client = match inner.ensure_connected().await {
                    Ok(c) => c,
                    Err(e) => return to_tool_result(Err(e)),
                };
                let result = client
                    .request("session.close", serde_json::json!({
                        "session_id": session_id,
                    }))
                    .await;

                // Remove from map on success
                if result.is_ok() {
                    inner.sessions.remove(&session_name);
                }

                to_tool_result(result)
            }
            SessionAction::Register { path, handler } => {
                let client = match inner.ensure_connected().await {
                    Ok(c) => c,
                    Err(e) => return to_tool_result(Err(e)),
                };
                let result = client
                    .request("coordinator.register", serde_json::json!({
                        "path": path,
                        "handler": handler,
                    }))
                    .await;
                to_tool_result(result)
            }
            SessionAction::Unregister { tracking_id } => {
                let client = match inner.ensure_connected().await {
                    Ok(c) => c,
                    Err(e) => return to_tool_result(Err(e)),
                };
                let result = client
                    .request("coordinator.unregister", serde_json::json!({
                        "tracking_id": tracking_id,
                    }))
                    .await;
                to_tool_result(result)
            }
        }
    }

    #[tool(description = "Read-only queries. Actions: read <path> [start:N end:N | count:N] [session:name], status, list, check <action>.")]
    async fn slipstream_query(
        &self,
        Parameters(p): Parameters<QueryParams>,
    ) -> Result<CallToolResult, McpError> {
        let query = match parse::parse_query(&p.q) {
            Ok(q) => q,
            Err(msg) => return err_result(msg),
        };

        let mut inner = self.inner.lock().await;

        match query {
            Query::Read { path, session, start, end, count } => {
                // Resolve session — if none exists, auto-create default and open the file
                let session_id = match inner.resolve_session(session.as_deref()) {
                    Ok(id) => id,
                    Err(_) => {
                        // No session exists — create default by opening this file
                        let client = match inner.ensure_connected().await {
                            Ok(c) => c,
                            Err(e) => return to_tool_result(Err(e)),
                        };
                        let open_result = client
                            .request("session.open", serde_json::json!({ "files": [&path] }))
                            .await;
                        match open_result {
                            Ok(v) => {
                                if let Some(sid) = v["session_id"].as_str() {
                                    let session_name = session.as_deref().unwrap_or("default").to_string();
                                    inner.sessions.insert(session_name, sid.to_string());
                                    sid.to_string()
                                } else {
                                    return err_result("auto-open failed: no session_id returned".to_string());
                                }
                            }
                            Err(e) => return to_tool_result(Err(e)),
                        }
                    }
                };
                let client = match inner.ensure_connected().await {
                    Ok(c) => c,
                    Err(e) => return to_tool_result(Err(e)),
                };
                let mut params = serde_json::json!({
                    "session_id": session_id,
                    "path": path,
                });
                if let Some(s) = start {
                    params["start"] = serde_json::json!(s);
                }
                if let Some(e) = end {
                    params["end"] = serde_json::json!(e);
                }
                if let Some(c) = count {
                    params["count"] = serde_json::json!(c);
                }
                let result = client.request("file.read", params.clone()).await;

                // If file not in session, auto-open it and retry
                match &result {
                    Err(ClientError::Rpc { message, .. }) if message.contains("not in session") => {
                        // Add the file to the existing session
                        let open_result = client
                            .request("session.open_file", serde_json::json!({
                                "session_id": session_id,
                                "path": &path,
                            }))
                            .await;
                        // session.open_file may not exist — fall back to reporting the original error
                        if open_result.is_ok() {
                            let retry = client.request("file.read", params).await;
                            to_tool_result(retry)
                        } else {
                            // The daemon doesn't support adding files to existing sessions,
                            // so just report the original error clearly
                            to_tool_result(result)
                        }
                    }
                    _ => to_tool_result(result),
                }
            }
            Query::Status => {
                let client = match inner.ensure_connected().await {
                    Ok(c) => c,
                    Err(e) => return to_tool_result(Err(e)),
                };
                let result = client
                    .request("coordinator.status", serde_json::json!({}))
                    .await;
                to_tool_result(result)
            }
            Query::List => {
                let client = match inner.ensure_connected().await {
                    Ok(c) => c,
                    Err(e) => return to_tool_result(Err(e)),
                };
                let result = client
                    .request("session.list", serde_json::json!({}))
                    .await;
                to_tool_result(result)
            }
            Query::Check { action } => {
                let client = match inner.ensure_connected().await {
                    Ok(c) => c,
                    Err(e) => return to_tool_result(Err(e)),
                };
                let result = client
                    .request("coordinator.check", serde_json::json!({ "action": action }))
                    .await;
                to_tool_result(result)
            }
        }
    }

    #[tool(description = "Show reference card with ops format, session actions, query syntax, and common workflows.")]
    async fn slipstream_help(&self) -> Result<CallToolResult, McpError> {
        Ok(CallToolResult::success(vec![Content::text(HELP_TEXT)]))
    }
}

impl SlipstreamServer {
    /// One-shot mode: open → read? → ops? → flush? → close.
    async fn exec_one_shot(
        &self,
        files: Vec<String>,
        ops: Option<serde_json::Value>,
        read_all: bool,
        flush: bool,
        force: bool,
    ) -> Result<CallToolResult, McpError> {
        let mut inner = self.inner.lock().await;
        let client = match inner.ensure_connected().await {
            Ok(c) => c,
            Err(e) => return to_tool_result(Err(e)),
        };

        let mut output = serde_json::Map::new();

        // 1. Open session
        let open_result = match client
            .request("session.open", serde_json::json!({ "files": files }))
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
        if read_all {
            let read_ops: Vec<serde_json::Value> = files.iter()
                .map(|f| serde_json::json!({ "method": "file.read", "path": f }))
                .collect();
            match client.request("batch", serde_json::json!({
                "session_id": session_id,
                "ops": read_ops,
            })).await {
                Ok(v) => { output.insert("read".to_string(), v); }
                Err(e) => {
                    let _ = client.request("session.close", serde_json::json!({
                        "session_id": session_id,
                    })).await;
                    return to_tool_result(Err(e));
                }
            }
        }

        // 3. Apply ops if provided
        if let Some(ops) = ops {
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
        if flush {
            match client.request("session.flush", serde_json::json!({
                "session_id": session_id,
                "force": force,
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
                 Open files with slipstream_session('open <files>') — for native text files (py, rs, ts, etc.), you get a session for editing. \
                 For external formats (xlsx, drawio, mid, tf), you get guidance on which FCP tool to use. \
                 IMPORTANT: Use slipstream_str_replace (or file.str_replace in batch) for all text edits — it matches exact text without line numbers. \
                 After opening external files in their FCP tool, call slipstream_session('register <path> <handler>') to track them. \
                 Before running a build, call slipstream_query('check build') to verify all files are saved. \
                 Call slipstream_query('status') at any time to see the full state of all tracked files. \
                 Use slipstream_query('list') for a lightweight view of just active sessions with file counts.".into()
            ),
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            ..Default::default()
        }
    }
}
