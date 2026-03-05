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
use slipstream_cli::parse::{self, SessionAction};

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
                "no active session. Use ss_session('open <files>') first.".to_string()
            } else {
                format!(
                    "no active session '{key}'. Use ss_session('open <files> as:{key}') first."
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

const HELP_TEXT: &str = r#"# ss Reference Card

## Quick Mode — Single Edit (most common)

Edit a file in one call — auto opens, edits, flushes, closes:
```
ss(path="f.rs", old_str="foo", new_str="bar")
ss(path="f.rs", old_str="foo", new_str="bar", replace_all=true)
```
Same ergonomics as native Edit. ~25 output tokens. Fire-and-forget.

## Batch Mode — Multiple Edits

Edit multiple files in one call:
```
ss(ops=[
  {"method": "file.str_replace", "path": "a.rs", "old_str": "x", "new_str": "y"},
  {"method": "file.str_replace", "path": "b.rs", "old_str": "x", "new_str": "y"}
])
```
Auto-opens all referenced files, applies edits, flushes, closes.

Read files first, then edit:
```
ss(ops=[
  {"method": "file.str_replace", "path": "f.rs", "old_str": "before", "new_str": "after"}
], read_all=true)
```

## JSON Op Reference

### file.str_replace — find and replace text
```json
{"method": "file.str_replace", "path": "f.rs", "old_str": "foo", "new_str": "bar"}
{"method": "file.str_replace", "path": "f.rs", "old_str": "foo", "new_str": "bar", "replace_all": true}
```

### file.write — insert or replace lines by position
```json
{"method": "file.write", "path": "f.rs", "start": 0, "end": 0, "content": ["inserted line"]}
```

### file.read — read lines
```json
{"method": "file.read", "path": "f.rs"}
{"method": "file.read", "path": "f.rs", "start": 10, "end": 30}
```

## DSL Shorthand (alternative to JSON in ops array)
```
"str_replace f.rs old:\"foo\" new:\"bar\""
"str_replace f.rs old:\"x\" new:\"y\" replace_all"
"read f.rs start:0 end:20"
```

## ss_session — Lifecycle + Queries

| Action | Example |
|--------|---------|
| open | `open src/main.rs src/lib.rs` |
| open named | `open data.csv as:worker-1` |
| flush | `flush` or `flush --force` |
| close | `close` (auto-flushes) |
| close discard | `close --no-flush` |
| close force | `close --force` |
| read | `read src/main.rs start:10 end:20` |
| status | `status` |
| list | `list` |
| check | `check build` |

Note: `read` auto-opens files not in the session.

## Common Workflows

**Quick edit** (most common):
```
ss(path="f.rs", old_str="old_code", new_str="new_code")
```

**Multi-file batch** (self-contained — no open/close needed):
```
ss(ops=[...edits across multiple files...])
```

**Read-then-edit** (only when you need to inspect files first):
```
ss_session("open src/main.rs")
ss_session("read src/main.rs start:0 end:50")
ss(ops=[...], session="default")
ss_session("close")
```

Note: ss() quick mode and batch mode are fully self-contained.
Only use ss_session("open") when you need a persistent session for reading.
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

    #[tool(description = "File editing operations. Two modes — both are self-contained (auto open/flush/close). Do NOT call ss_session('open') before using ss(). (1) Quick mode: ss(path, old_str, new_str) — single str_replace. (2) Batch mode: ss(ops=[...]) — multiple edits across files in one call. JSON op examples: {\"method\": \"file.str_replace\", \"path\": \"f.rs\", \"old_str\": \"foo\", \"new_str\": \"bar\"} — add \"replace_all\": true to replace all occurrences. {\"method\": \"file.write\", \"path\": \"f.rs\", \"start\": 0, \"end\": 0, \"content\": [\"inserted line\"]} — start==end inserts, start<end replaces. {\"method\": \"file.read\", \"path\": \"f.rs\", \"start\": 0, \"end\": 20}. Use read_all=true to get file contents.")]
    async fn ss(
        &self,
        Parameters(p): Parameters<SsParams>,
    ) -> Result<CallToolResult, McpError> {
        // Quick mode: path + old_str + new_str → single str_replace
        if let Some(ref path) = p.path {
            let old_str = match p.old_str {
                Some(ref s) => s.clone(),
                None => return err_result("quick mode requires old_str".to_string()),
            };
            let new_str = match p.new_str {
                Some(ref s) => s.clone(),
                None => return err_result("quick mode requires new_str".to_string()),
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

            // If there's an active session (explicit or default), use it instead of one-shot
            {
                let session_name = p.session.as_deref().unwrap_or("default");
                let inner = self.inner.lock().await;
                if inner.sessions.contains_key(session_name) {
                    drop(inner);
                    return self.exec_session_ops(p.session.as_deref(), ops, p.flush, p.force).await;
                }
            }

            return self.exec_one_shot(files, Some(ops), p.read_all, p.flush, p.force).await;
        }

        // Batch mode: ops array
        if let Some(ref items) = p.ops {
            let json_ops = match parse_ops(items) {
                Ok(ops) => ops,
                Err(msg) => return err_result(msg),
            };

            // If a named session is active, use session mode
            if let Some(ref session_name) = p.session {
                let inner = self.inner.lock().await;
                if inner.sessions.contains_key(session_name) {
                    drop(inner);
                    return self.exec_session_ops(p.session.as_deref(), json_ops, p.flush, p.force).await;
                }
            }

            // Check default session too
            if p.session.is_none() {
                let inner = self.inner.lock().await;
                if inner.sessions.contains_key("default") {
                    drop(inner);
                    return self.exec_session_ops(None, json_ops, p.flush, p.force).await;
                }
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

    #[tool(description = "Session lifecycle and queries. Actions: open <files> [as:name], flush [--force] [session:name], close [--no-flush] [--force] [session:name], read <path> [start:N end:N | count:N] [session:name], status, list, check <action>, register <path> <handler>, unregister <id>. Close auto-flushes by default.")]
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
            SessionAction::Close { name, flush, force } => {
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
                        "flush": flush,
                        "force": force,
                    }))
                    .await;

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
            SessionAction::Read { path, session, start, end, count } => {
                // Resolve session — if none exists, auto-create default and open the file
                let session_id = match inner.resolve_session(session.as_deref()) {
                    Ok(id) => id,
                    Err(_) => {
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
                let result = client.request("file.read", params).await;
                to_tool_result(result)
            }
            SessionAction::Status => {
                let client = match inner.ensure_connected().await {
                    Ok(c) => c,
                    Err(e) => return to_tool_result(Err(e)),
                };
                let result = client
                    .request("coordinator.status", serde_json::json!({}))
                    .await;
                to_tool_result(result)
            }
            SessionAction::List => {
                let client = match inner.ensure_connected().await {
                    Ok(c) => c,
                    Err(e) => return to_tool_result(Err(e)),
                };
                let result = client
                    .request("session.list", serde_json::json!({}))
                    .await;
                to_tool_result(result)
            }
            SessionAction::Check { action } => {
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

    #[tool(description = "Show reference card with quick mode, batch mode, ops format, session actions, and common workflows.")]
    async fn ss_help(&self) -> Result<CallToolResult, McpError> {
        Ok(CallToolResult::success(vec![Content::text(HELP_TEXT)]))
    }
}

impl SlipstreamServer {
    /// One-shot mode: open → read? → ops? → flush? → close.
    /// Close now auto-flushes via daemon, so we skip the separate flush step.
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
                        "flush": false,
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
                        "flush": false,
                    })).await;
                    return to_tool_result(Err(e));
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
            Err(e) => return to_tool_result(Err(e)),
        }

        to_tool_result(Ok(serde_json::Value::Object(output)))
    }

    /// Session mode: apply ops to an existing named session.
    async fn exec_session_ops(
        &self,
        session_name: Option<&str>,
        ops: serde_json::Value,
        flush: bool,
        force: bool,
    ) -> Result<CallToolResult, McpError> {
        let mut inner = self.inner.lock().await;

        let session_id = match inner.resolve_session(session_name) {
            Ok(id) => id,
            Err(msg) => return err_result(msg),
        };
        let session_label = session_name.unwrap_or("default").to_string();

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
        if flush {
            match client.request("session.flush", serde_json::json!({
                "session_id": session_id,
                "force": force,
            })).await {
                Ok(v) => { output.insert("flush".to_string(), v); }
                Err(e) => return to_tool_result(Err(e)),
            }
        }

        let _ = session_label; // used for future diagnostics
        to_tool_result(Ok(serde_json::Value::Object(output)))
    }
}

#[tool_handler]
impl ServerHandler for SlipstreamServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            instructions: Some(
                "Slipstream is a file session coordinator. \
                 Use ss(path=\"f.rs\", old_str=\"old\", new_str=\"new\") for quick single edits — auto opens, edits, flushes, closes. \
                 Use ss(ops=[...]) for batch edits across multiple files — also self-contained, no setup needed. \
                 Use ss_session('open <files>') ONLY for multi-turn sessions where you need to read before editing. \
                 Use ss_session('read <path>') to read files. \
                 Use ss_session('close') to close (auto-flushes). \
                 Use ss_session('status') or ss_session('list') to check state. \
                 Use ss_session('check build') before running builds.".into()
            ),
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            ..Default::default()
        }
    }
}
