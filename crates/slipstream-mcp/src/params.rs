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

fn default_true() -> bool {
    true
}

/// Parameters for the main `ss` tool.
///
/// Two mutually exclusive modes:
/// - **Quick mode** (`path` provided): single str_replace edit — auto open/edit/flush/close.
/// - **Batch mode** (`ops` provided): multiple ops — auto open/apply/flush/close.
///
/// If neither `path` nor `ops` is provided, returns an error.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SsParams {
    // --- Quick mode params ---
    /// Quick mode: target file path
    pub path: Option<String>,
    /// Quick mode: text to find (substring match)
    pub old_str: Option<String>,
    /// Quick mode: replacement text
    pub new_str: Option<String>,
    /// Quick mode: replace every occurrence (default: false, errors if >1 match)
    pub replace_all: Option<bool>,

    // --- Batch mode params ---
    /// Batch mode: operations array — DSL strings or JSON objects (mix freely).
    /// DSL: "str_replace f.rs old:\"foo\" new:\"bar\""
    /// JSON: {"method": "file.str_replace", "path": "f.rs", "old_str": "multi\nline", "new_str": "new"}
    pub ops: Option<Vec<OpItem>>,

    // --- Common params ---
    /// Read all files before applying ops (batch mode only)
    #[serde(default)]
    pub read_all: bool,
    #[serde(default = "default_true")]
    #[schemars(skip)]
    pub flush: bool,
    #[serde(default)]
    #[schemars(skip)]
    pub force: bool,
    /// Named session (rarely needed — most usage is one-shot)
    pub session: Option<String>,
}

/// Parameters for `ss_session` — lifecycle + queries.
///
/// Actions: open, flush, close, read, status, list, check, register, unregister.
/// Examples: "open src/main.rs", "open data.csv as:worker-1",
/// "flush", "flush --force", "close", "close --no-flush",
/// "read src/main.rs start:10 end:20", "status", "list", "check build"
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SsSessionParams {
    /// Action string.
    /// Examples: "open src/main.rs src/lib.rs", "open f.rs as:worker-1",
    /// "flush", "flush --force", "flush session:worker-1",
    /// "close", "close --no-flush", "close session:worker-1",
    /// "read src/main.rs", "read src/main.rs start:10 end:20",
    /// "status", "list", "check build",
    /// "register /path/file.xlsx sheets", "unregister ext-001"
    pub action: String,
}
