use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use dashmap::DashMap;

use crate::buffer::BufferPool;
use crate::flush::{FlushError, FlushResult};
use crate::session::{Session, SessionError, SessionId};

/// Default session inactivity timeout: 5 minutes.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(300);

/// Errors from session manager operations.
#[derive(Debug, thiserror::Error)]
pub enum ManagerError {
    #[error("session not found: {0}")]
    SessionNotFound(String),

    #[error("session error: {0}")]
    Session(#[from] SessionError),

    #[error("flush error: {0}")]
    Flush(#[from] FlushError),
}

/// Centralized session lifecycle manager.
///
/// Owns the `BufferPool` and all active sessions. Provides the single
/// entry point the daemon handler calls for all operations.
///
/// All methods take `&self` (interior mutability via `DashMap`) so the
/// manager can be shared across async tasks behind `Arc`.
pub struct SessionManager {
    pool: BufferPool,
    sessions: DashMap<SessionId, Session>,
    timeout: Duration,
}

impl SessionManager {
    /// Create a new session manager with default settings.
    pub fn new() -> Self {
        SessionManager {
            pool: BufferPool::new(),
            sessions: DashMap::new(),
            timeout: DEFAULT_TIMEOUT,
        }
    }

    /// Create a new session manager with a custom timeout.
    pub fn with_timeout(timeout: Duration) -> Self {
        SessionManager {
            pool: BufferPool::new(),
            sessions: DashMap::new(),
            timeout,
        }
    }

    /// Resolve a path to its canonical form.
    ///
    /// Delegates to the buffer pool's canonicalization cache.
    pub fn canonical_path(&self, path: &Path) -> Result<std::path::PathBuf, ManagerError> {
        self.pool.canonicalize(path).map_err(|e| ManagerError::Session(e.into()))
    }

    /// Create a new session, opening the given files.
    pub fn create_session(
        &self,
        id: SessionId,
        paths: &[&Path],
    ) -> Result<(), ManagerError> {
        let session = Session::open(id.clone(), paths, &self.pool)
            .map_err(ManagerError::Session)?;
        self.sessions.insert(id, session);
        Ok(())
    }

    /// Execute a closure with read access to a session.
    pub fn with_session<F, R>(&self, id: &SessionId, f: F) -> Result<R, ManagerError>
    where
        F: FnOnce(&Session) -> Result<R, ManagerError>,
    {
        let entry = self.sessions.get(id)
            .ok_or_else(|| ManagerError::SessionNotFound(id.as_str().to_owned()))?;
        f(entry.value())
    }

    /// Execute a closure with mutable access to a session.
    pub fn with_session_mut<F, R>(&self, id: &SessionId, f: F) -> Result<R, ManagerError>
    where
        F: FnOnce(&mut Session) -> Result<R, ManagerError>,
    {
        let mut entry = self.sessions.get_mut(id)
            .ok_or_else(|| ManagerError::SessionNotFound(id.as_str().to_owned()))?;
        f(entry.value_mut())
    }

    /// Flush a session's pending edits to disk.
    ///
    /// Gathers other sessions' edits for conflict detection, then delegates
    /// to the flush engine.
    pub fn flush_session(
        &self,
        id: &SessionId,
        force: bool,
    ) -> Result<FlushResult, ManagerError> {
        // Step 1: Get target's file paths (releases shard lock on drop).
        // Touch the session first to prevent the sweeper from expiring it
        // during the multi-step flush operation (TOCTOU fix).
        let target_file_paths: Vec<std::path::PathBuf> = {
            let mut target = self.sessions.get_mut(id)
                .ok_or_else(|| ManagerError::SessionNotFound(id.as_str().to_owned()))?;
            target.touch();
            target.files.keys().cloned().collect()
        };

        // Step 2: Gather other sessions' edits (read-only iteration)
        let mut other_edits_map = HashMap::new();
        for path in &target_file_paths {
            let path_edits: Vec<_> = self.sessions.iter()
                .filter(|entry| entry.key() != id)
                .filter_map(|entry| {
                    entry.value().files.get(path).map(|handle| {
                        crate::flush::OtherSessionEdits {
                            session_id: entry.key().clone(),
                            edits: handle.edits.clone(),
                        }
                    })
                })
                .collect();
            if !path_edits.is_empty() {
                other_edits_map.insert(path.clone(), path_edits);
            }
        }

        // Step 3: Flush (mutable access to target only)
        let mut target = self.sessions.get_mut(id)
            .ok_or_else(|| ManagerError::SessionNotFound(id.as_str().to_owned()))?;
        crate::flush::flush_session(target.value_mut(), &other_edits_map, force)
            .map_err(ManagerError::Flush)
    }

    /// Close a session, releasing all buffer references.
    pub fn close_session(&self, id: &SessionId) -> Result<(), ManagerError> {
        let (_, session) = self.sessions.remove(id)
            .ok_or_else(|| ManagerError::SessionNotFound(id.as_str().to_owned()))?;
        session.close(&self.pool).map_err(ManagerError::Session)
    }

    /// Sweep expired sessions (those exceeding the inactivity timeout).
    /// Returns the IDs of sessions that were cleaned up.
    pub fn sweep_expired(&self) -> Result<Vec<SessionId>, ManagerError> {
        let expired: Vec<SessionId> = self.sessions.iter()
            .filter(|entry| entry.value().is_expired(self.timeout))
            .map(|entry| entry.key().clone())
            .collect();
        let mut swept = Vec::new();
        for id in &expired {
            if let Some((id, session)) = self.sessions.remove(id) {
                if session.is_expired(self.timeout) {
                    // Still expired — close it
                    let _ = session.close(&self.pool);
                    swept.push(id);
                } else {
                    // Was touched since we checked — put it back
                    self.sessions.insert(id, session);
                }
            }
        }
        Ok(swept)
    }

    /// Number of active sessions.
    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }

    /// List active session IDs.
    pub fn session_ids(&self) -> Result<Vec<SessionId>, ManagerError> {
        Ok(self.sessions.iter().map(|entry| entry.key().clone()).collect())
    }

    /// Get dirty ranges for a file from all OTHER sessions (for `other_sessions` in responses).
    pub fn other_sessions_info(
        &self,
        exclude_session: &SessionId,
        path: &Path,
    ) -> Result<Vec<(SessionId, Vec<(usize, usize)>)>, ManagerError> {
        let mut info = Vec::new();
        for entry in self.sessions.iter() {
            if entry.key() != exclude_session {
                if let Ok(ranges) = entry.value().dirty_ranges_for_file(path) {
                    if !ranges.is_empty() {
                        info.push((entry.key().clone(), ranges));
                    }
                }
            }
        }
        Ok(info)
    }
}

impl Default for SessionManager {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for SessionManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionManager")
            .field("session_count", &self.session_count())
            .field("timeout", &self.timeout)
            .finish()
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
    fn create_and_close_session() {
        let mgr = SessionManager::new();
        let f = temp_file("hello\nworld\n");
        let id: SessionId = "s1".into();

        mgr.create_session(id.clone(), &[f.path()]).unwrap();
        assert_eq!(mgr.session_count(), 1);

        mgr.close_session(&id).unwrap();
        assert_eq!(mgr.session_count(), 0);
    }

    #[test]
    fn read_through_manager() {
        let mgr = SessionManager::new();
        let f = temp_file("a\nb\nc\n");
        let id: SessionId = "s1".into();

        mgr.create_session(id.clone(), &[f.path()]).unwrap();

        let lines = mgr.with_session_mut(&id, |session| {
            Ok(session.read(f.path(), 0, 2)?)
        }).unwrap();
        assert_eq!(lines, vec!["a", "b"]);
    }

    #[test]
    fn write_and_flush_through_manager() {
        let mgr = SessionManager::new();
        let f = temp_file("a\nb\nc\n");
        let id: SessionId = "s1".into();

        mgr.create_session(id.clone(), &[f.path()]).unwrap();

        mgr.with_session_mut(&id, |session| {
            session.write(f.path(), 1, 2, vec!["B".into()])?;
            Ok(())
        }).unwrap();

        let result = mgr.flush_session(&id, false).unwrap();
        assert!(matches!(result, FlushResult::Ok { .. }));

        let content = std::fs::read_to_string(f.path()).unwrap();
        assert_eq!(content, "a\nB\nc\n");
    }

    #[test]
    fn session_not_found() {
        let mgr = SessionManager::new();
        let id: SessionId = "nonexistent".into();

        let err = mgr.close_session(&id).unwrap_err();
        assert!(matches!(err, ManagerError::SessionNotFound(_)));
    }

    #[test]
    fn sweep_expired_sessions() {
        let mgr = SessionManager::with_timeout(Duration::from_millis(1));
        let f = temp_file("content\n");
        let id: SessionId = "s1".into();

        mgr.create_session(id.clone(), &[f.path()]).unwrap();
        assert_eq!(mgr.session_count(), 1);

        // Wait for expiry
        std::thread::sleep(Duration::from_millis(10));

        let expired = mgr.sweep_expired().unwrap();
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0], "s1");
        assert_eq!(mgr.session_count(), 0);
    }

    #[test]
    fn other_sessions_info_excludes_self() {
        let mgr = SessionManager::new();
        let f = temp_file("a\nb\nc\n");
        let id1: SessionId = "s1".into();
        let id2: SessionId = "s2".into();

        mgr.create_session(id1.clone(), &[f.path()]).unwrap();
        mgr.create_session(id2.clone(), &[f.path()]).unwrap();

        // s2 writes an edit
        mgr.with_session_mut(&id2, |session| {
            session.write(f.path(), 1, 2, vec!["X".into()])?;
            Ok(())
        }).unwrap();

        // s1 should see s2's dirty ranges
        let canonical = std::fs::canonicalize(f.path()).unwrap();
        let info = mgr.other_sessions_info(&id1, &canonical).unwrap();
        assert_eq!(info.len(), 1);
        assert_eq!(info[0].0, "s2");
        assert_eq!(info[0].1, vec![(1, 2)]);

        // s2 should NOT see its own dirty ranges
        let info = mgr.other_sessions_info(&id2, &canonical).unwrap();
        assert!(info.is_empty());
    }

    #[test]
    fn flush_detects_cross_session_conflict() {
        // Conflict requires: version mismatch + overlapping PENDING edits from another session.
        // Setup: s1 flushes (bumps version), then s2 and s3 have overlapping pending edits.
        let mgr = SessionManager::new();
        let f = temp_file("a\nb\nc\nd\ne\n");
        let id1: SessionId = "s1".into();
        let id2: SessionId = "s2".into();
        let id3: SessionId = "s3".into();

        mgr.create_session(id1.clone(), &[f.path()]).unwrap();
        mgr.create_session(id2.clone(), &[f.path()]).unwrap();
        mgr.create_session(id3.clone(), &[f.path()]).unwrap();

        // s1 edits a non-overlapping region and flushes → bumps version to 2
        mgr.with_session_mut(&id1, |session| {
            session.write(f.path(), 4, 5, vec!["E".into()])?;
            Ok(())
        }).unwrap();
        let result = mgr.flush_session(&id1, false).unwrap();
        assert!(matches!(result, FlushResult::Ok { .. }));

        // s2 and s3 queue overlapping edits (both still at snapshot version 1)
        mgr.with_session_mut(&id2, |session| {
            session.write(f.path(), 1, 3, vec!["X".into()])?;
            Ok(())
        }).unwrap();
        mgr.with_session_mut(&id3, |session| {
            session.write(f.path(), 2, 4, vec!["Y".into()])?;
            Ok(())
        }).unwrap();

        // s2 flushes → version mismatch (snapshot=1, buf=2) + s3 has overlapping pending edits → conflict
        let result = mgr.flush_session(&id2, false).unwrap();
        assert!(matches!(result, FlushResult::Conflict { .. }));
    }

    #[test]
    fn session_ids() {
        let mgr = SessionManager::new();
        let f = temp_file("content\n");

        mgr.create_session("a".into(), &[f.path()]).unwrap();
        mgr.create_session("b".into(), &[f.path()]).unwrap();

        let mut ids: Vec<String> = mgr.session_ids().unwrap()
            .into_iter()
            .map(|id| id.as_str().to_owned())
            .collect();
        ids.sort();
        assert_eq!(ids, vec!["a", "b"]);
    }
}
