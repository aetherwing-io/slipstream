use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;
use parking_lot::RwLock;

use crate::buffer::{BufferPool, BufferError, FileBuffer};
use crate::edit::Edit;
use crate::str_match::{self, StrReplaceError};

/// Unique session identifier (newtype for type safety).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SessionId(String);

impl SessionId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for SessionId {
    fn from(s: String) -> Self {
        SessionId(s)
    }
}

impl From<&str> for SessionId {
    fn from(s: &str) -> Self {
        SessionId(s.to_owned())
    }
}

impl PartialEq<&str> for SessionId {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

/// Session status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionStatus {
    Open,
    Flushing,
    Closed,
}

/// A cursor tracking the current read position in a file.
#[derive(Debug, Clone)]
pub struct Cursor {
    pub line: usize,
}

impl Cursor {
    pub fn new() -> Self {
        Cursor { line: 0 }
    }

    pub fn at(line: usize) -> Self {
        Cursor { line }
    }
}

impl Default for Cursor {
    fn default() -> Self {
        Self::new()
    }
}

/// Per-session handle to a file buffer. Tracks snapshot version, cursor, and pending edits.
#[derive(Debug)]
pub struct FileHandle {
    pub buffer: Arc<RwLock<FileBuffer>>,
    /// Buffer version when this session opened the file.
    pub snapshot_version: u64,
    pub cursor: Cursor,
    pub edits: Vec<Edit>,
    /// Canonical path for this file handle.
    pub path: PathBuf,
}

impl FileHandle {
    /// Read lines by range [start, end). Returns the lines from the buffer.
    pub fn read_range(&self, start: usize, end: usize) -> Result<Vec<String>, SessionError> {
        let buf = self.buffer.read();
        let start = start.min(buf.lines.len());
        let end = end.min(buf.lines.len());
        Ok(buf.lines[start..end].to_vec())
    }

    /// Read `count` lines from the current cursor position, advancing the cursor.
    pub fn read_from_cursor(&mut self, count: usize) -> Result<Vec<String>, SessionError> {
        let buf = self.buffer.read();
        let start = self.cursor.line.min(buf.lines.len());
        let end = (start + count).min(buf.lines.len());
        let lines = buf.lines[start..end].to_vec();
        self.cursor.line = end;
        Ok(lines)
    }

    /// Move the cursor to a specific line.
    pub fn move_cursor(&mut self, to: usize) {
        self.cursor.line = to;
    }

    /// Queue an edit (not applied until flush).
    pub fn queue_edit(&mut self, start: usize, end: usize, content: Vec<String>) {
        self.edits.push(Edit::new(start, end, content));
    }

    /// Number of pending edits.
    pub fn pending_edit_count(&self) -> usize {
        self.edits.len()
    }

    /// Get the dirty ranges (line ranges with pending edits).
    pub fn dirty_ranges(&self) -> Vec<(usize, usize)> {
        self.edits.iter().map(|e| e.range()).collect()
    }

    /// Total line count in the underlying buffer.
    pub fn line_count(&self) -> Result<usize, SessionError> {
        let buf = self.buffer.read();
        Ok(buf.line_count())
    }
}

/// A session represents one agent's working context.
#[derive(Debug)]
pub struct Session {
    pub id: SessionId,
    pub files: HashMap<PathBuf, FileHandle>,
    /// Cache of raw path → canonical path for this session's files.
    path_cache: HashMap<PathBuf, PathBuf>,
    pub status: SessionStatus,
    pub created_at: Instant,
    pub last_activity: Instant,
}

/// Errors from session operations.
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("buffer error: {0}")]
    Buffer(#[from] BufferError),

    #[error("session not open: {0}")]
    NotOpen(SessionId),

    #[error("session already closed: {0}")]
    AlreadyClosed(SessionId),

    #[error("file not in session: {}", .0.display())]
    FileNotInSession(PathBuf),

    #[error("str_replace error: {0}")]
    StrReplace(#[from] StrReplaceError),
}

impl Session {
    /// Create a new session and open the given files.
    pub fn open(
        id: SessionId,
        paths: &[&Path],
        pool: &BufferPool,
    ) -> Result<Self, SessionError> {
        let now = Instant::now();
        let mut files = HashMap::new();
        let mut path_cache = HashMap::new();

        for path in paths {
            let canonical = pool.canonicalize(path)?;
            let buf = pool.open(path)?;
            let version = buf.read().version;

            path_cache.insert(path.to_path_buf(), canonical.clone());

            files.insert(
                canonical.clone(),
                FileHandle {
                    buffer: buf,
                    snapshot_version: version,
                    cursor: Cursor::new(),
                    edits: Vec::new(),
                    path: canonical,
                },
            );
        }

        Ok(Session {
            id,
            files,
            path_cache,
            status: SessionStatus::Open,
            created_at: now,
            last_activity: now,
        })
    }

    /// Touch the session to reset inactivity timer.
    pub fn touch(&mut self) {
        self.last_activity = Instant::now();
    }

    /// Resolve a path using the session's cache, falling back to fs::canonicalize.
    fn resolve_path(&self, path: &Path) -> Result<PathBuf, SessionError> {
        if let Some(canonical) = self.path_cache.get(path) {
            return Ok(canonical.clone());
        }
        // Path might already be canonical (e.g. from tests using canonicalized paths)
        if self.files.contains_key(path) {
            return Ok(path.to_path_buf());
        }
        // Fallback: syscall (rare — only if caller passes an uncached path)
        std::fs::canonicalize(path).map_err(|e| SessionError::Buffer(BufferError::Io(e)))
    }

    /// Get a file handle (immutable).
    pub fn file(&self, path: &Path) -> Result<&FileHandle, SessionError> {
        let canonical = self.resolve_path(path)?;
        self.files
            .get(&canonical)
            .ok_or_else(|| SessionError::FileNotInSession(path.to_path_buf()))
    }

    /// Get a file handle (mutable).
    pub fn file_mut(&mut self, path: &Path) -> Result<&mut FileHandle, SessionError> {
        let canonical = self.resolve_path(path)?;
        self.files
            .get_mut(&canonical)
            .ok_or_else(|| SessionError::FileNotInSession(path.to_path_buf()))
    }

    /// Read lines from a file by range.
    pub fn read(&mut self, path: &Path, start: usize, end: usize) -> Result<Vec<String>, SessionError> {
        self.touch();
        let handle = self.file(path)?;
        handle.read_range(start, end)
    }

    /// Read lines from cursor position.
    pub fn read_next(&mut self, path: &Path, count: usize) -> Result<(Vec<String>, usize), SessionError> {
        self.touch();
        let handle = self.file_mut(path)?;
        let lines = handle.read_from_cursor(count)?;
        let cursor = handle.cursor.line;
        Ok((lines, cursor))
    }

    /// Move cursor in a file.
    pub fn move_cursor(&mut self, path: &Path, to: usize) -> Result<(), SessionError> {
        self.touch();
        let handle = self.file_mut(path)?;
        handle.move_cursor(to);
        Ok(())
    }

    /// Queue a write edit on a file.
    pub fn write(
        &mut self,
        path: &Path,
        start: usize,
        end: usize,
        content: Vec<String>,
    ) -> Result<usize, SessionError> {
        self.touch();
        let handle = self.file_mut(path)?;
        handle.queue_edit(start, end, content);
        Ok(handle.pending_edit_count())
    }

    /// String-match replace: find `old_str` in buffer, queue edit to replace with `new_str`.
    /// Requires exactly one match unless `replace_all` is true.
    /// Returns (match_start_line, match_count, edits_pending).
    pub fn str_replace(
        &mut self,
        path: &Path,
        old_str: &str,
        new_str: &str,
        replace_all: bool,
    ) -> Result<(usize, usize, usize), SessionError> {
        self.touch();

        if old_str.is_empty() {
            return Err(StrReplaceError::EmptySearch.into());
        }

        let handle = self.file_mut(path)?;
        let buf = handle.buffer.read();
        let result = str_match::find_str_in_lines(&buf.lines, old_str);
        let match_count = result.positions.len();

        if match_count == 0 {
            return Err(StrReplaceError::NoMatch.into());
        }
        if match_count > 1 && !replace_all {
            return Err(StrReplaceError::AmbiguousMatch { count: match_count }.into());
        }

        let old_line_count = str_match::needle_line_count(old_str);
        let new_lines = str_match::split_new_text(new_str);

        // Queue from bottom-up so positions don't shift during apply_edits
        let mut positions = result.positions;
        let first_match = positions[0];
        positions.sort_unstable_by(|a, b| b.cmp(a));

        drop(buf); // release read lock before mutating edits

        for start in &positions {
            handle.queue_edit(*start, *start + old_line_count, new_lines.clone());
        }

        Ok((first_match, match_count, handle.pending_edit_count()))
    }

    /// Close the session, releasing all buffer references.
    pub fn close(mut self, pool: &BufferPool) -> Result<(), SessionError> {
        self.status = SessionStatus::Closed;
        for (_, handle) in self.files.drain() {
            let _ = pool.release(&handle.path);
        }
        Ok(())
    }

    /// Check if session has exceeded the inactivity timeout.
    pub fn is_expired(&self, timeout: std::time::Duration) -> bool {
        self.last_activity.elapsed() > timeout
    }

    /// Get info about other sessions' dirty ranges for a given file.
    /// Used by the daemon to populate `other_sessions` in responses.
    pub fn dirty_ranges_for_file(&self, path: &Path) -> Result<Vec<(usize, usize)>, SessionError> {
        let handle = self.file(path)?;
        Ok(handle.dirty_ranges())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as IoWrite;
    use tempfile::NamedTempFile;

    fn temp_file(content: &str) -> NamedTempFile {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f.flush().unwrap();
        f
    }

    #[test]
    fn open_session_with_files() {
        let f1 = temp_file("line one\nline two\nline three\n");
        let f2 = temp_file("alpha\nbeta\n");
        let pool = BufferPool::new();

        let session = Session::open(
            "s1".into(),
            &[f1.path(), f2.path()],
            &pool,
        ).unwrap();

        assert_eq!(session.status, SessionStatus::Open);
        assert_eq!(session.files.len(), 2);
    }

    #[test]
    fn read_by_range() {
        let f = temp_file("zero\none\ntwo\nthree\nfour\n");
        let pool = BufferPool::new();
        let mut session = Session::open("s1".into(), &[f.path()], &pool).unwrap();

        let lines = session.read(f.path(), 1, 3).unwrap();
        assert_eq!(lines, vec!["one", "two"]);
    }

    #[test]
    fn read_from_cursor() {
        let f = temp_file("a\nb\nc\nd\ne\n");
        let pool = BufferPool::new();
        let mut session = Session::open("s1".into(), &[f.path()], &pool).unwrap();

        let (lines, cursor) = session.read_next(f.path(), 2).unwrap();
        assert_eq!(lines, vec!["a", "b"]);
        assert_eq!(cursor, 2);

        let (lines, cursor) = session.read_next(f.path(), 2).unwrap();
        assert_eq!(lines, vec!["c", "d"]);
        assert_eq!(cursor, 4);
    }

    #[test]
    fn move_cursor() {
        let f = temp_file("a\nb\nc\nd\n");
        let pool = BufferPool::new();
        let mut session = Session::open("s1".into(), &[f.path()], &pool).unwrap();

        session.move_cursor(f.path(), 2).unwrap();
        let (lines, cursor) = session.read_next(f.path(), 1).unwrap();
        assert_eq!(lines, vec!["c"]);
        assert_eq!(cursor, 3);
    }

    #[test]
    fn queue_edits() {
        let f = temp_file("a\nb\nc\n");
        let pool = BufferPool::new();
        let mut session = Session::open("s1".into(), &[f.path()], &pool).unwrap();

        let count = session.write(f.path(), 1, 2, vec!["B".into()]).unwrap();
        assert_eq!(count, 1);

        let count = session.write(f.path(), 0, 0, vec!["inserted".into()]).unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn dirty_ranges() {
        let f = temp_file("a\nb\nc\nd\ne\n");
        let pool = BufferPool::new();
        let mut session = Session::open("s1".into(), &[f.path()], &pool).unwrap();

        session.write(f.path(), 1, 3, vec!["x".into()]).unwrap();
        session.write(f.path(), 4, 4, vec!["y".into()]).unwrap();

        let ranges = session.dirty_ranges_for_file(f.path()).unwrap();
        assert_eq!(ranges, vec![(1, 3), (4, 4)]);
    }

    #[test]
    fn close_releases_buffers() {
        let f = temp_file("content\n");
        let pool = BufferPool::new();
        let session = Session::open("s1".into(), &[f.path()], &pool).unwrap();

        assert_eq!(pool.loaded_count(), 1);
        session.close(&pool).unwrap();
        assert_eq!(pool.loaded_count(), 0);
    }

    #[test]
    fn snapshot_version_recorded() {
        let f = temp_file("content\n");
        let pool = BufferPool::new();
        let session = Session::open("s1".into(), &[f.path()], &pool).unwrap();

        let handle = session.file(f.path()).unwrap();
        assert_eq!(handle.snapshot_version, 1);
    }

    #[test]
    fn file_not_in_session() {
        let f1 = temp_file("content\n");
        let f2 = temp_file("other\n");
        let pool = BufferPool::new();
        let mut session = Session::open("s1".into(), &[f1.path()], &pool).unwrap();

        assert!(matches!(
            session.read(f2.path(), 0, 1),
            Err(SessionError::FileNotInSession(_))
        ));
    }

    #[test]
    fn read_past_end_clamps() {
        let f = temp_file("a\nb\n");
        let pool = BufferPool::new();
        let mut session = Session::open("s1".into(), &[f.path()], &pool).unwrap();

        let lines = session.read(f.path(), 0, 100).unwrap();
        assert_eq!(lines, vec!["a", "b"]);
    }

    #[test]
    fn str_replace_basic() {
        let f = temp_file("alpha\nbeta\ngamma\ndelta\n");
        let pool = BufferPool::new();
        let mut session = Session::open("s1".into(), &[f.path()], &pool).unwrap();

        let (match_line, match_count, edits_pending) =
            session.str_replace(f.path(), "beta", "BETA", false).unwrap();
        assert_eq!(match_line, 1);
        assert_eq!(match_count, 1);
        assert_eq!(edits_pending, 1);
    }

    #[test]
    fn str_replace_multi_line() {
        let f = temp_file("a\nb\nc\nd\ne\n");
        let pool = BufferPool::new();
        let mut session = Session::open("s1".into(), &[f.path()], &pool).unwrap();

        let (match_line, _, edits_pending) =
            session.str_replace(f.path(), "b\nc\nd", "X\nY", false).unwrap();
        assert_eq!(match_line, 1);
        assert_eq!(edits_pending, 1);

        // Verify the edit range
        let ranges = session.dirty_ranges_for_file(f.path()).unwrap();
        assert_eq!(ranges, vec![(1, 4)]); // lines 1-3 replaced
    }

    #[test]
    fn str_replace_no_match_error() {
        let f = temp_file("a\nb\nc\n");
        let pool = BufferPool::new();
        let mut session = Session::open("s1".into(), &[f.path()], &pool).unwrap();

        let err = session.str_replace(f.path(), "xyz", "new", false).unwrap_err();
        assert!(matches!(err, SessionError::StrReplace(StrReplaceError::NoMatch)));
    }

    #[test]
    fn str_replace_ambiguous_error() {
        let f = temp_file("dup\nother\ndup\n");
        let pool = BufferPool::new();
        let mut session = Session::open("s1".into(), &[f.path()], &pool).unwrap();

        let err = session.str_replace(f.path(), "dup", "new", false).unwrap_err();
        assert!(matches!(err, SessionError::StrReplace(StrReplaceError::AmbiguousMatch { count: 2 })));
    }

    #[test]
    fn str_replace_all() {
        let f = temp_file("dup\nother\ndup\n");
        let pool = BufferPool::new();
        let mut session = Session::open("s1".into(), &[f.path()], &pool).unwrap();

        let (match_line, match_count, edits_pending) =
            session.str_replace(f.path(), "dup", "REPLACED", true).unwrap();
        assert_eq!(match_line, 0);
        assert_eq!(match_count, 2);
        assert_eq!(edits_pending, 2);
    }

    #[test]
    fn str_replace_empty_old_str_error() {
        let f = temp_file("a\nb\n");
        let pool = BufferPool::new();
        let mut session = Session::open("s1".into(), &[f.path()], &pool).unwrap();

        let err = session.str_replace(f.path(), "", "new", false).unwrap_err();
        assert!(matches!(err, SessionError::StrReplace(StrReplaceError::EmptySearch)));
    }

    #[test]
    fn cursor_read_past_end_returns_remaining() {
        let f = temp_file("a\nb\nc\n");
        let pool = BufferPool::new();
        let mut session = Session::open("s1".into(), &[f.path()], &pool).unwrap();

        session.move_cursor(f.path(), 2).unwrap();
        let (lines, cursor) = session.read_next(f.path(), 100).unwrap();
        assert_eq!(lines, vec!["c"]);
        assert_eq!(cursor, 3);
    }
}
