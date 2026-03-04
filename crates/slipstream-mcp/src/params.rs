use schemars::JsonSchema;
use serde::Deserialize;

/// A single operation item — either a DSL string or a JSON object.
///
/// DSL strings are compact for simple ops:
///   "str_replace f.rs old:\"foo\" new:\"bar\""
///
/// JSON objects avoid double-escaping for multi-line content:
///   {"method": "file.str_replace", "path": "f.rs", "old_str": "multi\nline", "new_str": "replacement"}
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum OpItem {
    /// DSL string, e.g. "str_replace f.rs old:\"foo\" new:\"bar\""
    Dsl(String),
    /// JSON object passed directly to daemon, e.g. {"method": "file.str_replace", "path": "f.rs", ...}
    Json(serde_json::Value),
}

/// Parameters for the main `slipstream` tool.
///
/// Two modes:
/// - **One-shot** (`files` provided): open → read? → ops? → flush? → close.
/// - **Session** (`files` omitted): apply ops to an active named session.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SlipstreamParams {
    /// File paths — if provided, creates a one-shot session (auto open+close).
    /// Omit to operate on the active session from slipstream_session('open ...').
    pub files: Option<Vec<String>>,
    /// Named session to operate on (default: "default").
    /// Only meaningful when files is omitted (session mode).
    pub session: Option<String>,
    /// Batch operations — each item is either a DSL string or a JSON object.
    /// DSL strings: "str_replace f.rs old:\"foo\" new:\"bar\"", "read f.rs start:10 end:20"
    /// JSON objects: {"method": "file.str_replace", "path": "f.rs", "old_str": "multi\nline", "new_str": "new"}
    /// Both formats can be mixed in the same array. Use JSON for multi-line content to avoid double-escaping.
    pub ops: Option<Vec<OpItem>>,
    /// Read all files before applying ops
    #[serde(default)]
    pub read_all: bool,
    /// Flush edits to disk after ops
    #[serde(default)]
    pub flush: bool,
    /// Force flush even if conflicts detected
    #[serde(default)]
    pub force: bool,
}

/// Parameters for `slipstream_session` — lifecycle actions.
///
/// Actions: open, flush, close, register, unregister.
/// Examples: "open src/main.rs", "open data.csv as:worker-1", "flush", "close session:worker-1"
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SessionActionParams {
    /// Lifecycle action string.
    /// Examples: "open src/main.rs src/lib.rs", "open f.rs as:worker-1",
    /// "flush", "flush --force", "flush session:worker-1",
    /// "close", "close session:worker-1",
    /// "register /path/file.xlsx sheets", "unregister ext-001"
    pub action: String,
}

/// Parameters for `slipstream_query` — read-only queries.
///
/// Queries: read, status, list, check.
/// Examples: "read src/main.rs", "read src/main.rs start:10 end:20", "status", "list", "check build"
#[derive(Debug, Deserialize, JsonSchema)]
pub struct QueryParams {
    /// Query string.
    /// Examples: "read src/main.rs", "read src/main.rs start:10 end:20",
    /// "read src/main.rs count:50", "read src/main.rs session:worker-1",
    /// "status", "list", "check build"
    pub q: String,
}
