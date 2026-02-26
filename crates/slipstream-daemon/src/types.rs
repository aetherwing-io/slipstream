use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

// --- session.open ---

#[derive(Debug, Deserialize)]
pub struct SessionOpenParams {
    pub files: Vec<PathBuf>,
}

#[derive(Debug, Serialize)]
pub struct SessionOpenResult {
    pub session_id: String,
    pub files: HashMap<PathBuf, FileInfo>,
}

#[derive(Debug, Serialize)]
pub struct FileInfo {
    pub lines: usize,
    pub version: u64,
}

// --- session.flush ---

#[derive(Debug, Deserialize)]
pub struct SessionFlushParams {
    pub session_id: String,
    #[serde(default)]
    pub force: bool,
}

#[derive(Debug, Serialize)]
pub struct SessionFlushResult {
    pub status: String,
    pub files_written: Vec<FileWrittenInfo>,
}

#[derive(Debug, Serialize)]
pub struct FileWrittenInfo {
    pub path: PathBuf,
    pub edits_applied: usize,
}

// --- session.close ---

#[derive(Debug, Deserialize)]
pub struct SessionCloseParams {
    pub session_id: String,
}

// --- file.read ---

#[derive(Debug, Deserialize)]
pub struct FileReadParams {
    pub session_id: String,
    pub path: PathBuf,
    /// If start/end provided, read by range
    pub start: Option<usize>,
    pub end: Option<usize>,
    /// If count provided (and no start/end), read from cursor
    pub count: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct FileReadResult {
    pub lines: Vec<String>,
    pub cursor: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub other_sessions: Vec<OtherSessionInfo>,
}

#[derive(Debug, Serialize)]
pub struct OtherSessionInfo {
    pub session: String,
    pub dirty_ranges: Vec<(usize, usize)>,
}

// --- file.write ---

#[derive(Debug, Deserialize)]
pub struct FileWriteParams {
    pub session_id: String,
    pub path: PathBuf,
    pub start: usize,
    pub end: usize,
    pub content: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct FileWriteResult {
    pub edits_pending: usize,
}

// --- file.str_replace ---

#[derive(Debug, Deserialize)]
pub struct FileStrReplaceParams {
    pub session_id: String,
    pub path: PathBuf,
    pub old_str: String,
    pub new_str: String,
    #[serde(default)]
    pub replace_all: bool,
}

#[derive(Debug, Serialize)]
pub struct FileStrReplaceResult {
    pub edits_pending: usize,
    pub match_line: usize,
    pub match_count: usize,
}

// --- cursor.move ---

#[derive(Debug, Deserialize)]
pub struct CursorMoveParams {
    pub session_id: String,
    pub path: PathBuf,
    pub to: usize,
}

// --- batch ---

#[derive(Debug, Deserialize)]
pub struct BatchParams {
    pub session_id: String,
    pub ops: Vec<BatchOp>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "method")]
pub enum BatchOp {
    #[serde(rename = "file.read")]
    Read {
        path: PathBuf,
        #[serde(default)]
        start: Option<usize>,
        #[serde(default)]
        end: Option<usize>,
        #[serde(default)]
        count: Option<usize>,
    },
    #[serde(rename = "file.write")]
    Write {
        path: PathBuf,
        start: usize,
        end: usize,
        content: Vec<String>,
    },
    #[serde(rename = "file.str_replace")]
    StrReplace {
        path: PathBuf,
        old_str: String,
        new_str: String,
        #[serde(default)]
        replace_all: bool,
    },
    #[serde(rename = "cursor.move")]
    CursorMove {
        path: PathBuf,
        to: usize,
    },
}

// --- Conflict error data ---

#[derive(Debug, Serialize)]
pub struct ConflictData {
    pub path: PathBuf,
    pub your_edits: Vec<(usize, usize)>,
    pub conflicting_edits: Vec<(usize, usize)>,
    pub by_session: String,
    pub hint: String,
}
