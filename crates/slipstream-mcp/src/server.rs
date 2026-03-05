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

use slipstream_cli::format;
use crate::params::*;
use slipstream_cli::parse::{self, SessionAction};

/// Inner state: connection config + optional connected client.
/// No session mapping — daemon is the sole source of truth for sessions.
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

/// Convert a parse/resolve error into an MCP tool result.
fn err_result(msg: String) -> Result<CallToolResult, McpError> {
    Ok(CallToolResult::success(vec![Content::text(format!("! {msg}"))]))
}

/// Convert an RPC error into compact format.
fn format_error(e: ClientError) -> Result<CallToolResult, McpError> {
    match e {
        ClientError::Rpc { code, message, data } => {
            let text = format::format_rpc_error(code, &message, data.as_ref());
            Ok(CallToolResult::success(vec![Content::text(text)]))
        }
        other => {
            Ok(CallToolResult::success(vec![Content::text(format!("! {other}"))]))
        }
    }
}

const HELP_TEXT: &str = r#"# ss Reference Card

## Editing

```
ss(path="f.rs", old_str="foo", new_str="bar")
ss(path="f.rs", old_str="foo", new_str="bar", replace_all=true)
```

Batch — multiple files in one call:
```
ss(ops=[
  {"method": "file.str_replace", "path": "a.rs", "old_str": "x", "new_str": "y"},
  {"method": "file.str_replace", "path": "b.rs", "old_str": "x", "new_str": "y"}
])
```

## Reading

```
ss_session("read src/main.rs")
ss_session("read src/main.rs start:10 end:30")
```

Read + edit in one call:
```
ss(ops=[...edits...], read_all=true)
```

## Op Formats

### file.str_replace
```json
{"method": "file.str_replace", "path": "f.rs", "old_str": "foo", "new_str": "bar"}
```
Add `"replace_all": true` to replace every occurrence.

### file.write
```json
{"method": "file.write", "path": "f.rs", "start": 0, "end": 0, "content": ["inserted line"]}
```
start==end inserts, start<end replaces.

### file.read
```json
{"method": "file.read", "path": "f.rs", "start": 10, "end": 30}
```

### DSL shorthand (alternative in ops array)
```
"str_replace f.rs old:\"foo\" new:\"bar\""
"read f.rs start:0 end:20"
```

## Output Format

```
~ f.rs:42 str_replace (1 match)     edit applied
> f.rs (1 edit)                      flushed to disk
+ f.rs (150L v1)                     file opened
@ f.rs:10-30 (20L cursor:30)        read content
! f.rs str_replace: not found        error
- closed                             session closed
[1f 1e flush:ok closed]              status bar
```

## Advanced — Session Control

Sessions are managed automatically. Use these when you need
to hold files open across multiple calls (read → think → edit).

| Action | Example |
|--------|---------|
| open | `open src/main.rs src/lib.rs` |
| open named | `open data.csv as:worker-1` |
| flush | `flush` or `flush --force` |
| close | `close` (auto-flushes) |
| close discard | `close --no-flush` |
| close force | `close --force` |
| status | `status` |
| list | `list` |
| check build | `check build` |

Named sessions persist across calls. `close` auto-flushes.
Use `--force` to override conflict detection.
"#;

/// Parse mixed DSL/JSON op items into JSON values for the daemon batch protocol.
fn parse_ops(items: &[OpItem]) -> Result<serde_json::Value, String> {
    let json_array: Vec<serde_json::Value> = items
        .iter()
        .map(|item| match item {
            OpItem::Dsl(dsl) => serde_json::Value::String(dsl.clone()),
            OpItem::Json(obj) => obj.clone(),
        })
        .collect();
    parse::normalize_ops(&serde_json::Value::Array(json_array))
}

/// Extract unique file paths from a JSON ops array for auto-open.
fn extract_paths_from_ops(ops: &serde_json::Value) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut paths = Vec::new();
    if let Some(arr) = ops.as_array() {
        for op in arr {
            if let Some(path) = op.get("path").and_then(|p| p.as_str()) {
                if seen.insert(path) {
                    paths.push(path.to_string());
                }
            }
        }
    }
    paths
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

    #[tool(description = "Edit files. ss(path, old_str, new_str) for a single edit. ss(ops=[...]) for multiple edits across files. Both are self-contained — no setup needed.")]
    async fn ss(
        &self,
        Parameters(p): Parameters<SsParams>,
    ) -> Result<CallToolResult, McpError> {
        // Quick mode: path + old_str + new_str → single str_replace
        if let Some(ref path) = p.path {
            let old_str = match p.old_str {
                Some(ref s) => s.clone(),
                None => return err_result(format!(
                    "quick mode requires old_str (received: path={:?}, old_str=None, new_str={:?})",
                    path, p.new_str
                )),
            };
            let new_str = match p.new_str {
                Some(ref s) => s.clone(),
                None => return err_result(format!(
                    "quick mode requires new_str (received: path={:?}, old_str={:?}, new_str=None)",
                    path, p.old_str
                )),
            };
            let replace_all = p.replace_all.unwrap_or(false);

            let mut op = serde_json::json!({
                "method": "file.str_replace",
                "path": path,
                "old_str": old_str,
                "new_str": new_str,
            });
            if replace_all {
                op["replace_all"] = serde_json::json!(true);
            }

            let ops = serde_json::Value::Array(vec![op]);
            let files = vec![path.clone()];

            // If an explicit session is given, use session mode
            if let Some(ref session_name) = p.session {
                return self.exec_session_ops(session_name, ops, p.flush, p.force).await;
            }

            return self.exec_one_shot(files, Some(ops), p.read_all, p.flush, p.force).await;
        }

        // Batch mode: ops array
        if let Some(ref items) = p.ops {
            let json_ops = match parse_ops(items) {
                Ok(ops) => ops,
                Err(msg) => return err_result(msg),
            };

            // If an explicit session is given, use session mode
            if let Some(ref session_name) = p.session {
                return self.exec_session_ops(session_name, json_ops, p.flush, p.force).await;
            }

            // Auto-open: extract paths from ops and use one-shot mode
            let files = extract_paths_from_ops(&json_ops);
            if files.is_empty() {
                return err_result("no file paths found in ops".to_string());
            }
            return self.exec_one_shot(files, Some(json_ops), p.read_all, p.flush, p.force).await;
        }

        // Neither path nor ops provided
        err_result("provide either path (quick mode) or ops (batch mode)".to_string())
    }

    #[tool(description = "Read files and manage sessions. ss_session('read <path>') to read a file. ss_session('read <path> start:N end:N') for a range. Run ss_help for advanced session control.")]
    async fn ss_session(
        &self,
        Parameters(p): Parameters<SsSessionParams>,
    ) -> Result<CallToolResult, McpError> {
        let action = match parse::parse_session_action(&p.action) {
            Ok(a) => a,
            Err(msg) => return err_result(msg),
        };

        let mut inner = self.inner.lock().await;

        match action {
            SessionAction::Open { files, name } => {
                let session_name = name.unwrap_or_else(|| "default".to_string());

                let client = match inner.ensure_connected().await {
                    Ok(c) => c,
                    Err(e) => return format_error(e),
                };

                let result = client
                    .request("session.open", serde_json::json!({
                        "files": files,
                        "name": session_name,
                    }))
                    .await;

                match result {
                    Ok(v) => {
                        let text = format::format_open(&v, &session_name);
                        Ok(CallToolResult::success(vec![Content::text(text)]))
                    }
                    Err(e) => format_error(e),
                }
            }
            SessionAction::Flush { name, force } => {
                let session_name = name.as_deref().unwrap_or("default");
                let client = match inner.ensure_connected().await {
                    Ok(c) => c,
                    Err(e) => return format_error(e),
                };
                let result = client
                    .request("session.flush", serde_json::json!({
                        "session_id": session_name,
                        "force": force,
                    }))
                    .await;
                match result {
                    Ok(v) => {
                        let text = format::format_flush(&v, session_name);
                        Ok(CallToolResult::success(vec![Content::text(text)]))
                    }
                    Err(e) => format_error(e),
                }
            }
            SessionAction::Close { name, flush, force } => {
                let session_name = name.as_deref().unwrap_or("default");
                let client = match inner.ensure_connected().await {
                    Ok(c) => c,
                    Err(e) => return format_error(e),
                };
                let result = client
                    .request("session.close", serde_json::json!({
                        "session_id": session_name,
                        "flush": flush,
                        "force": force,
                    }))
                    .await;
                match result {
                    Ok(v) => {
                        let text = if flush {
                            format::format_close(&v)
                        } else {
                            format::format_close_no_flush(&v)
                        };
                        Ok(CallToolResult::success(vec![Content::text(text)]))
                    }
                    Err(e) => format_error(e),
                }
            }
            SessionAction::Register { path, handler } => {
                let client = match inner.ensure_connected().await {
                    Ok(c) => c,
                    Err(e) => return format_error(e),
                };
                let result = client
                    .request("coordinator.register", serde_json::json!({
                        "path": path,
                        "handler": handler,
                    }))
                    .await;
                match result {
                    Ok(v) => {
                        let tid = v.get("tracking_id").and_then(|t| t.as_str()).unwrap_or("?");
                        Ok(CallToolResult::success(vec![Content::text(
                            format!("+ registered {path} → {handler} (id:{tid})")
                        )]))
                    }
                    Err(e) => format_error(e),
                }
            }
            SessionAction::Unregister { tracking_id } => {
                let client = match inner.ensure_connected().await {
                    Ok(c) => c,
                    Err(e) => return format_error(e),
                };
                let result = client
                    .request("coordinator.unregister", serde_json::json!({
                        "tracking_id": tracking_id,
                    }))
                    .await;
                match result {
                    Ok(_) => Ok(CallToolResult::success(vec![Content::text(
                        format!("- unregistered {tracking_id}")
                    )])),
                    Err(e) => format_error(e),
                }
            }
            SessionAction::Read { path, session, start, end, count } => {
                let session_name = session.as_deref().unwrap_or("default");

                let client = match inner.ensure_connected().await {
                    Ok(c) => c,
                    Err(e) => return format_error(e),
                };

                // Auto-open: tell daemon to open with this session name.
                // If session already exists, daemon adds the file. If not, creates it.
                let _open_result = client
                    .request("session.open", serde_json::json!({
                        "files": [&path],
                        "name": session_name,
                    }))
                    .await;
                // Ignore open errors — file.read will fail with a clear message if needed.

                let mut params = serde_json::json!({
                    "session_id": session_name,
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
                match client.request("file.read", params).await {
                    Ok(v) => {
                        let text = format::format_read(&v, &path, start, end);
                        Ok(CallToolResult::success(vec![Content::text(text)]))
                    }
                    Err(e) => format_error(e),
                }
            }
            SessionAction::Status => {
                let client = match inner.ensure_connected().await {
                    Ok(c) => c,
                    Err(e) => return format_error(e),
                };
                let result = client
                    .request("coordinator.status", serde_json::json!({}))
                    .await;
                // Status/list/check — keep JSON for now, these are rare diagnostic calls
                match result {
                    Ok(v) => {
                        let text = serde_json::to_string_pretty(&v).unwrap_or_else(|_| v.to_string());
                        Ok(CallToolResult::success(vec![Content::text(text)]))
                    }
                    Err(e) => format_error(e),
                }
            }
            SessionAction::List => {
                let client = match inner.ensure_connected().await {
                    Ok(c) => c,
                    Err(e) => return format_error(e),
                };
                let result = client
                    .request("session.list", serde_json::json!({}))
                    .await;
                match result {
                    Ok(v) => {
                        let text = serde_json::to_string_pretty(&v).unwrap_or_else(|_| v.to_string());
                        Ok(CallToolResult::success(vec![Content::text(text)]))
                    }
                    Err(e) => format_error(e),
                }
            }
            SessionAction::Check { action } => {
                let client = match inner.ensure_connected().await {
                    Ok(c) => c,
                    Err(e) => return format_error(e),
                };
                let result = client
                    .request("coordinator.check", serde_json::json!({ "action": action }))
                    .await;
                match result {
                    Ok(v) => {
                        let text = serde_json::to_string_pretty(&v).unwrap_or_else(|_| v.to_string());
                        Ok(CallToolResult::success(vec![Content::text(text)]))
                    }
                    Err(e) => format_error(e),
                }
            }
        }
    }

    #[tool(description = "Full reference card — ops format, session control, advanced options.")]
    async fn ss_help(&self) -> Result<CallToolResult, McpError> {
        Ok(CallToolResult::success(vec![Content::text(HELP_TEXT)]))
    }
}

impl SlipstreamServer {
    /// One-shot mode: open → read? → ops? → close (auto-flushes).
    /// Uses a deterministic name so the daemon can reuse/create as needed.
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
            Err(e) => return format_error(e),
        };

        let mut output = serde_json::Map::new();

        // 1. Open session with a one-shot name
        let open_result = match client
            .request("session.open", serde_json::json!({
                "files": files,
                "name": "__oneshot__",
            }))
            .await
        {
            Ok(v) => v,
            Err(e) => return format_error(e),
        };

        let session_id = "__oneshot__";
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
                        "flush": false,
                    })).await;
                    return format_error(e);
                }
            }
        }

        // 3. Apply ops if provided — save a copy for formatting
        let saved_ops = ops.clone();
        if let Some(ops) = ops {
            match client.request("batch", serde_json::json!({
                "session_id": session_id,
                "ops": ops,
            })).await {
                Ok(v) => { output.insert("batch".to_string(), v); }
                Err(e) => {
                    let _ = client.request("session.close", serde_json::json!({
                        "session_id": session_id,
                        "flush": false,
                    })).await;
                    return format_error(e);
                }
            }
        }

        // 4. Close (with auto-flush handled by daemon)
        match client.request("session.close", serde_json::json!({
            "session_id": session_id,
            "flush": flush,
            "force": force,
        })).await {
            Ok(v) => { output.insert("close".to_string(), v); }
            Err(e) => return format_error(e),
        }

        let text = format::format_one_shot(&serde_json::Value::Object(output), saved_ops.as_ref(), read_all);
        Ok(CallToolResult::success(vec![Content::text(text)]))
    }

    /// Session mode: apply ops to a named session (daemon manages it).
    async fn exec_session_ops(
        &self,
        session_name: &str,
        ops: serde_json::Value,
        flush: bool,
        force: bool,
    ) -> Result<CallToolResult, McpError> {
        let mut inner = self.inner.lock().await;

        let client = match inner.ensure_connected().await {
            Ok(c) => c,
            Err(e) => return format_error(e),
        };

        let mut output = serde_json::Map::new();

        // Apply batch ops — save for formatting
        let saved_ops = ops.clone();
        match client.request("batch", serde_json::json!({
            "session_id": session_name,
            "ops": ops,
        })).await {
            Ok(v) => { output.insert("batch".to_string(), v); }
            Err(e) => return format_error(e),
        }

        // Flush if requested
        if flush {
            match client.request("session.flush", serde_json::json!({
                "session_id": session_name,
                "force": force,
            })).await {
                Ok(v) => { output.insert("flush".to_string(), v); }
                Err(e) => return format_error(e),
            }
        }

        let text = format::format_session_ops(&serde_json::Value::Object(output), &saved_ops, session_name);
        Ok(CallToolResult::success(vec![Content::text(text)]))
    }
}

#[tool_handler]
impl ServerHandler for SlipstreamServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            instructions: Some(
                "Slipstream edits files. \
                 ss(path, old_str, new_str) for single edits. \
                 ss(ops=[...]) for batch edits across files. \
                 ss_session('read <path>') to read files. \
                 ss_help for advanced options.".into()
            ),
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            ..Default::default()
        }
    }
}
