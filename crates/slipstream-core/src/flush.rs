use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

use crate::edit::{self, Edit};
use crate::session::{Session, SessionError, SessionId, SessionStatus};

/// Result of flushing a single file.
#[derive(Debug)]
pub struct FileFlushResult {
    pub path: PathBuf,
    pub edits_applied: usize,
}

/// Conflict details for a single file.
#[derive(Debug)]
pub struct FlushConflict {
    pub path: PathBuf,
    pub your_edits: Vec<(usize, usize)>,
    pub conflicting_edits: Vec<(usize, usize)>,
    pub by_session: SessionId,
}

/// Warning about another session's pending edits on a flushed file.
#[derive(Debug)]
pub struct FlushWarning {
    pub path: PathBuf,
    pub other_session: SessionId,
    pub pending_edit_count: usize,
}

/// Result of a flush operation.
#[must_use]
#[derive(Debug)]
pub enum FlushResult {
    /// All files flushed successfully.
    Ok {
        files_written: Vec<FileFlushResult>,
        warnings: Vec<FlushWarning>,
    },
    /// Some files had conflicts (no files were written).
    Conflict {
        conflicts: Vec<FlushConflict>,
    },
}

/// Errors from flush operations.
#[derive(Debug, thiserror::Error)]
pub enum FlushError {
    #[error("session error: {0}")]
    Session(#[from] SessionError),

    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
}

/// Information about other sessions' edits on a file, used for conflict checking.
pub struct OtherSessionEdits {
    pub session_id: SessionId,
    pub edits: Vec<Edit>,
}

/// Flush all pending edits from a session to disk.
///
/// Algorithm:
/// 1. For each file with pending edits:
///    a. Lock the buffer
///    b. Check snapshot_version vs current version
///    c. If version mismatch and not force: check for range overlaps with `other_edits`
///    d. Sort edits bottom-up, apply to buffer
///    e. Write to disk atomically (temp + rename)
///    f. Increment version, update disk_hash
/// 2. If any conflicts found and not force: return conflicts, write nothing
pub fn flush_session(
    session: &mut Session,
    other_edits: &HashMap<PathBuf, Vec<OtherSessionEdits>>,
    force: bool,
) -> Result<FlushResult, FlushError> {
    session.status = SessionStatus::Flushing;

    // Phase 1: Check for conflicts across all files before writing anything
    let mut conflicts = Vec::new();
    let mut files_to_write: Vec<(PathBuf, Vec<Edit>)> = Vec::new();

    for (path, handle) in &mut session.files {
        if handle.edits.is_empty() {
            continue;
        }

        let buf = handle.buffer.read();

        // Version mismatch means another session flushed since we opened
        if buf.version != handle.snapshot_version && !force {
            // Check if our edits overlap with theirs
            if let Some(other_sessions) = other_edits.get(path) {
                for other in other_sessions {
                    let conflict_pairs = edit::find_conflicts(&handle.edits, &other.edits);
                    if !conflict_pairs.is_empty() {
                        let your_ranges: Vec<(usize, usize)> = conflict_pairs
                            .iter()
                            .map(|&(i, _)| handle.edits[i].range())
                            .collect();
                        let their_ranges: Vec<(usize, usize)> = conflict_pairs
                            .iter()
                            .map(|&(_, j)| other.edits[j].range())
                            .collect();

                        conflicts.push(FlushConflict {
                            path: path.clone(),
                            your_edits: your_ranges,
                            conflicting_edits: their_ranges,
                            by_session: other.session_id.clone(),
                        });
                    }
                }
            }
        }

        // Collect edits to apply
        let edits = std::mem::take(&mut handle.edits);
        files_to_write.push((path.clone(), edits));
    }

    // If conflicts and not forcing, abort
    if !conflicts.is_empty() && !force {
        // Put edits back
        for (path, edits) in files_to_write {
            if let Some(handle) = session.files.get_mut(&path) {
                handle.edits = edits;
            }
        }
        session.status = SessionStatus::Open;
        return Ok(FlushResult::Conflict { conflicts });
    }

    // Phase 2: Apply edits and write to disk
    let mut results = Vec::new();

    for (path, mut edits) in files_to_write {
        let handle = session.files.get_mut(&path)
            .expect("files_to_write was built from session.files");

        let edit_count = edits.len();

        // Phase 2a: Apply edits under write lock, snapshot the result, then release
        let (snapshot_lines, snapshot_trailing, file_path) = {
            let mut buf = handle.buffer.write();

            // Sort bottom-up to avoid offset cascading
            edit::sort_bottom_up(&mut edits);

            // Apply edits to buffer (takes ownership to avoid cloning)
            edit::apply_edits(&mut buf.lines, edits);

            (buf.lines.clone(), buf.trailing_newline, buf.path.clone())
        }; // write lock released here

        // Phase 2b: Write snapshot to disk without holding any lock (slow I/O)
        let new_hash = atomic_write_and_hash(&file_path, &snapshot_lines, snapshot_trailing)?;

        // Phase 2c: Reacquire write lock to update metadata (fast)
        {
            let mut buf = handle.buffer.write();
            buf.disk_hash = new_hash;
            buf.version += 1;
        }

        // Update handle snapshot (read current version)
        handle.snapshot_version = handle.buffer.read().version;

        results.push(FileFlushResult {
            path: path.clone(),
            edits_applied: edit_count,
        });
    }

    // Build warnings: other sessions with pending edits on files we just flushed
    let mut warnings = Vec::new();
    let flushed_paths: Vec<&PathBuf> = results.iter().map(|r| &r.path).collect();
    for path in &flushed_paths {
        if let Some(other_sessions) = other_edits.get(*path) {
            for other in other_sessions {
                if !other.edits.is_empty() {
                    warnings.push(FlushWarning {
                        path: (*path).clone(),
                        other_session: other.session_id.clone(),
                        pending_edit_count: other.edits.len(),
                    });
                }
            }
        }
    }

    session.status = SessionStatus::Open;
    Ok(FlushResult::Ok { files_written: results, warnings })
}

/// Write lines to a file atomically while computing the FNV-1a hash simultaneously.
/// This avoids the intermediate String allocation of `reconstruct()` + separate hash.
///
/// Uses `tempfile::NamedTempFile` for safe temp file handling:
/// - Auto-cleans on drop (no leaked temp files on write failure)
/// - Uses `O_CREAT|O_EXCL` (unpredictable name, no TOCTOU race)
/// - `persist()` does atomic rename
fn atomic_write_and_hash(path: &Path, lines: &[String], trailing_newline: bool) -> Result<u64, FlushError> {
    use std::io::{BufWriter, Write};
    use tempfile::NamedTempFile;

    let parent = path.parent().unwrap_or(Path::new("."));
    let temp = NamedTempFile::new_in(parent).map_err(FlushError::Io)?;

    // Preserve permissions of the target file if it exists
    if let Ok(meta) = std::fs::metadata(path) {
        let _ = temp.as_file().set_permissions(meta.permissions());
    }

    let mut writer = BufWriter::new(&temp);
    let mut hash: u64 = 0xcbf29ce484222325;

    for (i, line) in lines.iter().enumerate() {
        if i > 0 {
            writer.write_all(b"\n")?;
            hash ^= b'\n' as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
        for byte in line.as_bytes() {
            hash ^= *byte as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
        writer.write_all(line.as_bytes())?;
    }
    if trailing_newline {
        writer.write_all(b"\n")?;
        hash ^= b'\n' as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }

    writer.flush().map_err(FlushError::Io)?;
    drop(writer);

    // persist() does an atomic rename; if it fails, NamedTempFile drops and cleans up
    temp.persist(path).map_err(|e| FlushError::Io(e.error))?;

    Ok(hash)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::BufferPool;
    use crate::session::Session;
    use std::io::Write as IoWrite;
    use tempfile::NamedTempFile;

    fn temp_file(content: &str) -> NamedTempFile {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f.flush().unwrap();
        f
    }

    #[test]
    fn flush_applies_edits_to_disk() {
        let f = temp_file("line zero\nline one\nline two\nline three\n");
        let pool = BufferPool::new();
        let mut session = Session::open("s1".into(), &[f.path()], &pool).unwrap();

        // Replace "line one" with "REPLACED"
        session.write(f.path(), 1, 2, vec!["REPLACED".into()]).unwrap();

        let result = flush_session(&mut session, &HashMap::new(), false).unwrap();
        match result {
            FlushResult::Ok { files_written, .. } => {
                assert_eq!(files_written.len(), 1);
                assert_eq!(files_written[0].edits_applied, 1);
            }
            FlushResult::Conflict { .. } => panic!("unexpected conflict"),
        }

        // Verify file on disk
        let content = std::fs::read_to_string(f.path()).unwrap();
        assert_eq!(content, "line zero\nREPLACED\nline two\nline three\n");
    }

    #[test]
    fn flush_multiple_edits_bottom_up() {
        let f = temp_file("a\nb\nc\nd\ne\n");
        let pool = BufferPool::new();
        let mut session = Session::open("s1".into(), &[f.path()], &pool).unwrap();

        session.write(f.path(), 1, 2, vec!["B".into()]).unwrap();
        session.write(f.path(), 3, 4, vec!["D".into()]).unwrap();

        let result = flush_session(&mut session, &HashMap::new(), false).unwrap();
        assert!(matches!(result, FlushResult::Ok { .. }));

        let content = std::fs::read_to_string(f.path()).unwrap();
        assert_eq!(content, "a\nB\nc\nD\ne\n");
    }

    #[test]
    fn flush_insertion() {
        let f = temp_file("a\nb\n");
        let pool = BufferPool::new();
        let mut session = Session::open("s1".into(), &[f.path()], &pool).unwrap();

        session.write(f.path(), 1, 1, vec!["inserted".into()]).unwrap();

        let result = flush_session(&mut session, &HashMap::new(), false).unwrap();
        assert!(matches!(result, FlushResult::Ok { .. }));

        let content = std::fs::read_to_string(f.path()).unwrap();
        assert_eq!(content, "a\ninserted\nb\n");
    }

    #[test]
    fn flush_deletion() {
        let f = temp_file("a\nb\nc\nd\n");
        let pool = BufferPool::new();
        let mut session = Session::open("s1".into(), &[f.path()], &pool).unwrap();

        session.write(f.path(), 1, 3, vec![]).unwrap();

        let result = flush_session(&mut session, &HashMap::new(), false).unwrap();
        assert!(matches!(result, FlushResult::Ok { .. }));

        let content = std::fs::read_to_string(f.path()).unwrap();
        assert_eq!(content, "a\nd\n");
    }

    #[test]
    fn flush_detects_conflict() {
        let f = temp_file("a\nb\nc\nd\ne\n");
        let pool = BufferPool::new();
        let mut session = Session::open("s1".into(), &[f.path()], &pool).unwrap();

        // Simulate another session having flushed (bump version)
        {
            let handle = session.file(f.path()).unwrap();
            handle.buffer.write().version = 2;
        }

        // Our edit overlaps with "other session's" edit
        session.write(f.path(), 1, 3, vec!["X".into()]).unwrap();

        let canonical = std::fs::canonicalize(f.path()).unwrap();
        let mut other_edits = HashMap::new();
        other_edits.insert(
            canonical,
            vec![OtherSessionEdits {
                session_id: "s2".into(),
                edits: vec![Edit::new(2, 4, vec!["Y".into()])],
            }],
        );

        let result = flush_session(&mut session, &other_edits, false).unwrap();
        match result {
            FlushResult::Conflict { conflicts } => {
                assert_eq!(conflicts.len(), 1);
                assert_eq!(conflicts[0].by_session, "s2");
            }
            FlushResult::Ok { .. } => panic!("expected conflict"),
        }

        // Verify file was NOT modified
        let content = std::fs::read_to_string(f.path()).unwrap();
        assert_eq!(content, "a\nb\nc\nd\ne\n");
    }

    #[test]
    fn flush_force_overrides_conflict() {
        let f = temp_file("a\nb\nc\nd\ne\n");
        let pool = BufferPool::new();
        let mut session = Session::open("s1".into(), &[f.path()], &pool).unwrap();

        // Simulate version bump
        {
            let handle = session.file(f.path()).unwrap();
            handle.buffer.write().version = 2;
        }

        session.write(f.path(), 1, 3, vec!["FORCED".into()]).unwrap();

        let canonical = std::fs::canonicalize(f.path()).unwrap();
        let mut other_edits = HashMap::new();
        other_edits.insert(
            canonical,
            vec![OtherSessionEdits {
                session_id: "s2".into(),
                edits: vec![Edit::new(2, 4, vec!["Y".into()])],
            }],
        );

        let result = flush_session(&mut session, &other_edits, true).unwrap();
        match result {
            FlushResult::Ok { files_written, .. } => {
                assert_eq!(files_written.len(), 1);
            }
            FlushResult::Conflict { .. } => panic!("expected force to succeed"),
        }

        let content = std::fs::read_to_string(f.path()).unwrap();
        assert_eq!(content, "a\nFORCED\nd\ne\n");
    }

    #[test]
    fn flush_no_edits_is_noop() {
        let f = temp_file("content\n");
        let pool = BufferPool::new();
        let mut session = Session::open("s1".into(), &[f.path()], &pool).unwrap();

        let result = flush_session(&mut session, &HashMap::new(), false).unwrap();
        match result {
            FlushResult::Ok { files_written, .. } => {
                assert!(files_written.is_empty());
            }
            FlushResult::Conflict { .. } => panic!("unexpected conflict"),
        }
    }

    #[test]
    fn flush_preserves_trailing_newline() {
        let f = temp_file("a\nb\n");
        let pool = BufferPool::new();
        let mut session = Session::open("s1".into(), &[f.path()], &pool).unwrap();

        session.write(f.path(), 0, 1, vec!["A".into()]).unwrap();
        let result = flush_session(&mut session, &HashMap::new(), false).unwrap();
        assert!(matches!(result, FlushResult::Ok { .. }));

        let content = std::fs::read_to_string(f.path()).unwrap();
        assert_eq!(content, "A\nb\n");
    }

    #[test]
    fn flush_preserves_no_trailing_newline() {
        let f = temp_file("a\nb");
        let pool = BufferPool::new();
        let mut session = Session::open("s1".into(), &[f.path()], &pool).unwrap();

        session.write(f.path(), 0, 1, vec!["A".into()]).unwrap();
        let result = flush_session(&mut session, &HashMap::new(), false).unwrap();
        assert!(matches!(result, FlushResult::Ok { .. }));

        let content = std::fs::read_to_string(f.path()).unwrap();
        assert_eq!(content, "A\nb");
    }

    #[test]
    fn flush_version_increments() {
        let f = temp_file("a\nb\n");
        let pool = BufferPool::new();
        let mut session = Session::open("s1".into(), &[f.path()], &pool).unwrap();

        session.write(f.path(), 0, 1, vec!["A".into()]).unwrap();
        let result = flush_session(&mut session, &HashMap::new(), false).unwrap();
        assert!(matches!(result, FlushResult::Ok { .. }));

        let handle = session.file(f.path()).unwrap();
        let buf = handle.buffer.read();
        assert_eq!(buf.version, 2);
        assert_eq!(handle.snapshot_version, 2);
    }

    #[test]
    fn flush_clears_pending_edits() {
        let f = temp_file("a\nb\n");
        let pool = BufferPool::new();
        let mut session = Session::open("s1".into(), &[f.path()], &pool).unwrap();

        session.write(f.path(), 0, 1, vec!["A".into()]).unwrap();
        assert_eq!(session.file(f.path()).unwrap().pending_edit_count(), 1);

        let result = flush_session(&mut session, &HashMap::new(), false).unwrap();
        assert!(matches!(result, FlushResult::Ok { .. }));
        assert_eq!(session.file(f.path()).unwrap().pending_edit_count(), 0);
    }

    #[test]
    fn flush_conflict_preserves_edits() {
        let f = temp_file("a\nb\nc\n");
        let pool = BufferPool::new();
        let mut session = Session::open("s1".into(), &[f.path()], &pool).unwrap();

        {
            let handle = session.file(f.path()).unwrap();
            handle.buffer.write().version = 2;
        }

        session.write(f.path(), 1, 2, vec!["X".into()]).unwrap();

        let canonical = std::fs::canonicalize(f.path()).unwrap();
        let mut other_edits = HashMap::new();
        other_edits.insert(
            canonical,
            vec![OtherSessionEdits {
                session_id: "s2".into(),
                edits: vec![Edit::new(1, 2, vec!["Y".into()])],
            }],
        );

        let result = flush_session(&mut session, &other_edits, false).unwrap();
        assert!(matches!(result, FlushResult::Conflict { .. }));

        // Edits should still be pending
        assert_eq!(session.file(f.path()).unwrap().pending_edit_count(), 1);
    }

    #[test]
    fn flush_warns_about_other_sessions_pending_edits() {
        let f = temp_file("a\nb\nc\nd\ne\n");
        let pool = BufferPool::new();
        let mut session = Session::open("s1".into(), &[f.path()], &pool).unwrap();

        // Our edit on lines 0..1
        session.write(f.path(), 0, 1, vec!["A".into()]).unwrap();

        // Other session has pending edits on lines 3..4 (no overlap, no conflict)
        let canonical = std::fs::canonicalize(f.path()).unwrap();
        let mut other_edits = HashMap::new();
        other_edits.insert(
            canonical,
            vec![OtherSessionEdits {
                session_id: "s2".into(),
                edits: vec![Edit::new(3, 4, vec!["D".into()])],
            }],
        );

        let result = flush_session(&mut session, &other_edits, false).unwrap();
        match result {
            FlushResult::Ok { files_written, warnings } => {
                assert_eq!(files_written.len(), 1);
                assert_eq!(warnings.len(), 1);
                assert_eq!(warnings[0].other_session, "s2");
                assert_eq!(warnings[0].pending_edit_count, 1);
            }
            FlushResult::Conflict { .. } => panic!("expected ok with warnings"),
        }
    }

    #[test]
    fn flush_no_warnings_when_no_other_sessions() {
        let f = temp_file("a\nb\nc\n");
        let pool = BufferPool::new();
        let mut session = Session::open("s1".into(), &[f.path()], &pool).unwrap();

        session.write(f.path(), 0, 1, vec!["A".into()]).unwrap();

        let result = flush_session(&mut session, &HashMap::new(), false).unwrap();
        match result {
            FlushResult::Ok { warnings, .. } => {
                assert!(warnings.is_empty());
            }
            FlushResult::Conflict { .. } => panic!("expected ok"),
        }
    }
}
