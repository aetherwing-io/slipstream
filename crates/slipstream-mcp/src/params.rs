use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct OpenParams {
    /// File paths to open in the session
    pub files: Vec<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReadParams {
    /// Session ID from a previous open call
    pub session_id: String,
    /// File path to read
    pub path: String,
    /// Start line (0-indexed, inclusive). Use with end for range reads.
    pub start: Option<usize>,
    /// End line (exclusive). Use with start for range reads.
    pub end: Option<usize>,
    /// Number of lines to read from current cursor position
    pub count: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WriteParams {
    /// Session ID from a previous open call
    pub session_id: String,
    /// File path to write
    pub path: String,
    /// Start line (0-indexed, inclusive)
    pub start: usize,
    /// End line (exclusive). Use start==end for insertion.
    pub end: usize,
    /// Replacement lines
    pub content: Vec<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct StrReplaceParams {
    /// Session ID from a previous open call
    pub session_id: String,
    /// File path to edit
    pub path: String,
    /// The exact text to find (multi-line, must match exactly including whitespace)
    pub old_str: String,
    /// The replacement text
    pub new_str: String,
    /// Replace all occurrences (default false, requires exactly one match)
    #[serde(default)]
    pub replace_all: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CursorParams {
    /// Session ID from a previous open call
    pub session_id: String,
    /// File path
    pub path: String,
    /// Target line number to move cursor to
    pub to: usize,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct FlushParams {
    /// Session ID from a previous open call
    pub session_id: String,
    /// Force flush even if conflicts are detected
    #[serde(default)]
    pub force: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CloseParams {
    /// Session ID from a previous open call
    pub session_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct BatchParams {
    /// Session ID from a previous open call
    pub session_id: String,
    /// Array of operations: [{"method": "file.read", "path": "...", ...}, ...]
    pub ops: serde_json::Value,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ExecParams {
    /// File paths to open in the session
    pub files: Vec<String>,
    /// Array of operations to apply: [{"method": "file.str_replace", "path": "...", "old_str": "...", "new_str": "..."}, ...]
    pub ops: Option<serde_json::Value>,
    /// Read all opened files before applying ops
    #[serde(default)]
    pub read_all: bool,
    /// Flush edits to disk after applying ops
    #[serde(default)]
    pub flush: bool,
    /// Force flush even if conflicts detected
    #[serde(default)]
    pub force: bool,
}
