use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use dashmap::DashMap;
use serde::Serialize;
use slipstream_core::session::SessionId;

#[derive(Debug, Clone, Serialize)]
pub enum HandlerType {
    /// Native text buffer. Holds the session ID.
    Native { session_id: SessionId },
    /// Externally managed (FCP server, etc.). No slipstream session.
    External { handler_name: String },
    /// Advisory: loaded as text but with usage guidance. Has a slipstream session.
    Advisory {
        handler_name: String,
        session_id: SessionId,
    },
}

#[derive(Debug, Clone, Serialize)]
pub enum FileState {
    Clean,
    Dirty { edit_count: usize },
    Flushed,
    ExternallyManaged,
    Closed,
}

#[derive(Debug, Clone, Serialize)]
pub struct TrackedFile {
    pub path: PathBuf,
    pub canonical_path: PathBuf,
    pub handler: HandlerType,
    pub state: FileState,
    pub tracking_id: String,
    #[serde(skip)]
    pub registered_at: Instant,
    #[serde(skip)]
    pub last_activity: Instant,
}

#[derive(Debug, Serialize)]
pub struct SessionDigest {
    pub total_tracked: usize,
    pub native_count: usize,
    pub native_dirty: usize,
    pub external_count: usize,
    pub files: Vec<DigestEntry>,
}

#[derive(Debug, Serialize)]
pub struct DigestEntry {
    /// Path relative to CWD if possible, otherwise absolute.
    pub path: String,
    /// "native", "sheets", "drawio", etc.
    pub handler: String,
    /// Human-readable: "3 edits pending", "clean", "externally-managed", "flushed", "closed".
    pub state: String,
    /// Optional non-blocking advisory for this file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub advisory: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CoordinatorStatus {
    pub tracked_files: Vec<TrackedFile>,
    pub native_sessions: Vec<NativeSessionInfo>,
    pub external_registrations: Vec<ExternalRegistrationInfo>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct NativeSessionInfo {
    pub session_id: String,
    pub files: Vec<String>,
    pub dirty_count: usize,
}

#[derive(Debug, Serialize)]
pub struct ExternalRegistrationInfo {
    pub tracking_id: String,
    pub path: String,
    pub handler: String,
}

#[derive(Debug, Serialize)]
pub struct CheckResult {
    pub warnings: Vec<String>,
    pub suggestion: String,
}

#[derive(Debug, thiserror::Error)]
pub enum CoordinatorError {
    #[error("tracking_id not found: {0}")]
    TrackingIdNotFound(String),
    #[error("unknown action: {0}")]
    UnknownAction(String),
}

pub struct Coordinator {
    /// Canonical path → tracked file entry.
    files: DashMap<PathBuf, TrackedFile>,
    /// Monotonic counter for tracking IDs.
    next_id: AtomicU64,
}

fn relative_or_absolute(path: &Path, base: &Path) -> String {
    pathdiff::diff_paths(path, base)
        .unwrap_or_else(|| path.to_path_buf())
        .display()
        .to_string()
}

impl Coordinator {
    pub fn new() -> Self {
        Self {
            files: DashMap::new(),
            next_id: AtomicU64::new(0),
        }
    }

    fn next_tracking_id(&self) -> String {
        let n = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        if n <= 999 {
            format!("ext-{n:03}")
        } else {
            format!("ext-{n}")
        }
    }

    // Registration

    pub fn register_native(
        &self,
        path: &Path,
        canonical: &Path,
        session_id: SessionId,
    ) -> String {
        let tracking_id = self.next_tracking_id();
        let now = Instant::now();
        self.files.insert(
            canonical.to_path_buf(),
            TrackedFile {
                path: path.to_path_buf(),
                canonical_path: canonical.to_path_buf(),
                handler: HandlerType::Native { session_id },
                state: FileState::Clean,
                tracking_id: tracking_id.clone(),
                registered_at: now,
                last_activity: now,
            },
        );
        tracking_id
    }

    pub fn register_external(
        &self,
        path: &Path,
        handler_name: &str,
    ) -> Result<String, CoordinatorError> {
        let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let tracking_id = self.next_tracking_id();
        let now = Instant::now();
        self.files.insert(
            canonical.clone(),
            TrackedFile {
                path: path.to_path_buf(),
                canonical_path: canonical,
                handler: HandlerType::External {
                    handler_name: handler_name.to_string(),
                },
                state: FileState::ExternallyManaged,
                tracking_id: tracking_id.clone(),
                registered_at: now,
                last_activity: now,
            },
        );
        Ok(tracking_id)
    }

    pub fn unregister(&self, tracking_id: &str) -> Result<(), CoordinatorError> {
        let key = self
            .files
            .iter()
            .find(|e| e.value().tracking_id == tracking_id)
            .map(|e| e.key().clone());
        match key {
            Some(k) => {
                self.files.remove(&k);
                Ok(())
            }
            None => Err(CoordinatorError::TrackingIdNotFound(
                tracking_id.to_string(),
            )),
        }
    }

    // State transitions (called by handler after each mutation)

    pub fn mark_dirty(&self, canonical: &Path, edit_count: usize) {
        if let Some(mut entry) = self.files.get_mut(canonical) {
            entry.state = FileState::Dirty { edit_count };
            entry.last_activity = Instant::now();
        }
    }

    pub fn mark_flushed(&self, canonical: &Path) {
        if let Some(mut entry) = self.files.get_mut(canonical) {
            entry.state = FileState::Flushed;
            entry.last_activity = Instant::now();
        }
    }

    pub fn mark_closed_by_session(&self, session_id: &SessionId) {
        for mut entry in self.files.iter_mut() {
            let matches = match &entry.handler {
                HandlerType::Native { session_id: sid } => sid == session_id,
                HandlerType::Advisory { session_id: sid, .. } => sid == session_id,
                HandlerType::External { .. } => false,
            };
            if matches {
                entry.state = FileState::Closed;
            }
        }
    }

    // Sweeper integration

    pub fn on_sessions_swept(&self, expired: &[SessionId]) {
        for sid in expired {
            self.mark_closed_by_session(sid);
        }
    }

    // Digest

    pub fn build_digest(&self, cwd: &Path) -> SessionDigest {
        let mut total_tracked = 0;
        let mut native_count = 0;
        let mut native_dirty = 0;
        let mut external_count = 0;
        let mut files = Vec::new();

        for entry in self.files.iter() {
            let tf = entry.value();
            total_tracked += 1;

            let (handler_name, is_external) = match &tf.handler {
                HandlerType::Native { .. } => ("native".to_string(), false),
                HandlerType::External { handler_name } => (handler_name.clone(), true),
                HandlerType::Advisory { handler_name, .. } => (handler_name.clone(), false),
            };

            if is_external {
                external_count += 1;
            } else {
                native_count += 1;
            }

            let state_str = match &tf.state {
                FileState::Clean => "clean".to_string(),
                FileState::Dirty { edit_count } => {
                    native_dirty += 1;
                    format!("{edit_count} edits pending")
                }
                FileState::Flushed => "flushed".to_string(),
                FileState::ExternallyManaged => "externally-managed".to_string(),
                FileState::Closed => "closed".to_string(),
            };

            let path_str = relative_or_absolute(&tf.canonical_path, cwd);

            files.push(DigestEntry {
                path: path_str,
                handler: handler_name,
                state: state_str,
                advisory: None,
            });
        }

        SessionDigest {
            total_tracked,
            native_count,
            native_dirty,
            external_count,
            files,
        }
    }

    // Status

    pub fn status(&self) -> CoordinatorStatus {
        let tracked_files: Vec<TrackedFile> = self.files.iter().map(|e| e.value().clone()).collect();

        // De-duplicate native sessions
        let mut session_map: HashMap<String, (Vec<String>, usize)> = HashMap::new();
        for tf in &tracked_files {
            let sid = match &tf.handler {
                HandlerType::Native { session_id } => Some(session_id.as_str().to_string()),
                HandlerType::Advisory { session_id, .. } => Some(session_id.as_str().to_string()),
                HandlerType::External { .. } => None,
            };
            if let Some(sid) = sid {
                let entry = session_map.entry(sid).or_insert_with(|| (Vec::new(), 0));
                entry.0.push(tf.canonical_path.display().to_string());
                if matches!(tf.state, FileState::Dirty { .. }) {
                    entry.1 += 1;
                }
            }
        }
        let native_sessions: Vec<NativeSessionInfo> = session_map
            .into_iter()
            .map(|(session_id, (files, dirty_count))| NativeSessionInfo {
                session_id,
                files,
                dirty_count,
            })
            .collect();

        let external_registrations: Vec<ExternalRegistrationInfo> = tracked_files
            .iter()
            .filter_map(|tf| match &tf.handler {
                HandlerType::External { handler_name } => Some(ExternalRegistrationInfo {
                    tracking_id: tf.tracking_id.clone(),
                    path: tf.canonical_path.display().to_string(),
                    handler: handler_name.clone(),
                }),
                _ => None,
            })
            .collect();

        let warnings: Vec<String> = tracked_files
            .iter()
            .filter_map(|tf| match &tf.state {
                FileState::Dirty { edit_count } => Some(format!(
                    "{} has {edit_count} unflushed edits",
                    tf.canonical_path.display()
                )),
                _ => None,
            })
            .collect();

        CoordinatorStatus {
            tracked_files,
            native_sessions,
            external_registrations,
            warnings,
        }
    }

    // Check action

    pub fn check_action(&self, action: &str) -> Result<CheckResult, CoordinatorError> {
        if action != "build" {
            return Err(CoordinatorError::UnknownAction(action.to_string()));
        }

        let mut warnings = Vec::new();
        let mut flush_cmds = Vec::new();
        let mut save_cmds = Vec::new();

        for entry in self.files.iter() {
            let tf = entry.value();
            match (&tf.handler, &tf.state) {
                (HandlerType::Native { session_id }, FileState::Dirty { edit_count }) => {
                    let path = tf.canonical_path.display().to_string();
                    warnings.push(format!(
                        "{path} has {edit_count} unflushed edits — flush session {session_id} first"
                    ));
                    flush_cmds.push(format!(
                        "session.flush {{ session_id: \"{session_id}\" }}"
                    ));
                }
                (HandlerType::External { handler_name }, _) => {
                    let path = tf.canonical_path.display().to_string();
                    warnings.push(format!(
                        "{path} is externally managed by {handler_name} — ensure it's saved"
                    ));
                    save_cmds.push(format!("{handler_name}_session(\"save\")"));
                }
                _ => {}
            }
        }

        let suggestion = if warnings.is_empty() {
            "Ready to build".to_string()
        } else {
            let mut parts = Vec::new();
            if !flush_cmds.is_empty() {
                parts.push(format!("Flush: {}", flush_cmds.join(", ")));
            }
            if !save_cmds.is_empty() {
                parts.push(format!("Save: {}", save_cmds.join(", ")));
            }
            parts.join(". ")
        };

        Ok(CheckResult {
            warnings,
            suggestion,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::path::{Path, PathBuf};

    fn sid(s: &str) -> SessionId {
        SessionId::from(s)
    }

    fn tmp_path(name: &str) -> PathBuf {
        PathBuf::from(format!("/tmp/coordinator_test_{name}"))
    }

    #[test]
    fn test_register_native_returns_tracking_id() {
        let c = Coordinator::new();
        let tid = c.register_native(&tmp_path("foo.py"), &tmp_path("foo.py"), sid("s1"));
        assert!(tid.starts_with("ext-"));
        assert_eq!(c.build_digest(Path::new("/tmp")).total_tracked, 1);
    }

    #[test]
    fn test_register_external_returns_tracking_id() {
        let c = Coordinator::new();
        let tid = c
            .register_external(&tmp_path("sheet.xlsx"), "sheets")
            .unwrap();
        assert!(tid.starts_with("ext-"));
        assert_eq!(c.build_digest(Path::new("/tmp")).external_count, 1);
    }

    #[test]
    fn test_tracking_ids_are_unique() {
        let c = Coordinator::new();
        let ids: HashSet<String> = (0..5)
            .map(|i| {
                c.register_external(&tmp_path(&format!("file{i}.xlsx")), "sheets")
                    .unwrap()
            })
            .collect();
        assert_eq!(ids.len(), 5);
    }

    #[test]
    fn test_tracking_id_format() {
        let c = Coordinator::new();
        let t1 = c.register_external(&tmp_path("a.xlsx"), "sheets").unwrap();
        let t2 = c.register_external(&tmp_path("b.xlsx"), "sheets").unwrap();
        assert_eq!(t1, "ext-001");
        assert_eq!(t2, "ext-002");
    }

    #[test]
    fn test_unregister_removes_entry() {
        let c = Coordinator::new();
        let tid = c
            .register_external(&tmp_path("rm.xlsx"), "sheets")
            .unwrap();
        c.unregister(&tid).unwrap();
        assert_eq!(c.build_digest(Path::new("/tmp")).total_tracked, 0);
    }

    #[test]
    fn test_unregister_unknown_tracking_id_errors() {
        let c = Coordinator::new();
        let result = c.unregister("ext-999");
        assert!(matches!(
            result,
            Err(CoordinatorError::TrackingIdNotFound(_))
        ));
    }

    #[test]
    fn test_mark_dirty_updates_state() {
        let c = Coordinator::new();
        let p = tmp_path("dirty.py");
        c.register_native(&p, &p, sid("s1"));
        assert_eq!(c.build_digest(Path::new("/tmp")).native_dirty, 0);
        c.mark_dirty(&p, 3);
        let digest = c.build_digest(Path::new("/tmp"));
        assert_eq!(digest.native_dirty, 1);
        let entry = digest.files.iter().find(|f| f.path.contains("dirty")).unwrap();
        assert_eq!(entry.state, "3 edits pending");
    }

    #[test]
    fn test_mark_flushed_updates_state() {
        let c = Coordinator::new();
        let p = tmp_path("flush.py");
        c.register_native(&p, &p, sid("s1"));
        c.mark_dirty(&p, 2);
        c.mark_flushed(&p);
        let digest = c.build_digest(Path::new("/tmp"));
        assert_eq!(digest.native_dirty, 0);
        let entry = digest.files.iter().find(|f| f.path.contains("flush")).unwrap();
        assert_eq!(entry.state, "flushed");
    }

    #[test]
    fn test_mark_closed_by_session() {
        let c = Coordinator::new();
        let p1 = tmp_path("close_a1.py");
        let p2 = tmp_path("close_a2.py");
        let p3 = tmp_path("close_b.py");
        c.register_native(&p1, &p1, sid("abc"));
        c.register_native(&p2, &p2, sid("abc"));
        c.register_native(&p3, &p3, sid("xyz"));
        c.mark_closed_by_session(&sid("abc"));

        let status = c.status();
        for tf in &status.tracked_files {
            let path_str = tf.canonical_path.display().to_string();
            if path_str.contains("close_a") {
                assert!(matches!(tf.state, FileState::Closed), "abc files should be closed");
            } else if path_str.contains("close_b") {
                assert!(matches!(tf.state, FileState::Clean), "xyz file should be clean");
            }
        }
    }

    #[test]
    fn test_on_sessions_swept() {
        let c = Coordinator::new();
        let p = tmp_path("swept.py");
        c.register_native(&p, &p, sid("abc"));
        c.on_sessions_swept(&[sid("abc")]);
        let digest = c.build_digest(Path::new("/tmp"));
        let entry = digest.files.iter().find(|f| f.path.contains("swept")).unwrap();
        assert_eq!(entry.state, "closed");
    }

    #[test]
    fn test_build_digest_counts() {
        let c = Coordinator::new();
        let n1 = tmp_path("count_n1.py");
        let n2 = tmp_path("count_n2.py");
        c.register_native(&n1, &n1, sid("s1"));
        c.register_native(&n2, &n2, sid("s1"));
        c.mark_dirty(&n1, 1);
        for i in 0..3 {
            c.register_external(&tmp_path(&format!("count_e{i}.xlsx")), "sheets")
                .unwrap();
        }
        let digest = c.build_digest(Path::new("/tmp"));
        assert_eq!(digest.total_tracked, 5);
        assert_eq!(digest.native_count, 2);
        assert_eq!(digest.native_dirty, 1);
        assert_eq!(digest.external_count, 3);
    }

    #[test]
    fn test_build_digest_relative_paths() {
        let c = Coordinator::new();
        let p = PathBuf::from("/tmp/test_dir/foo.py");
        c.register_external(&p, "sheets").unwrap();
        // register_external canonicalizes, but for non-existent paths falls back to given path
        let digest = c.build_digest(Path::new("/tmp/test_dir"));
        let entry = &digest.files[0];
        assert_eq!(entry.path, "foo.py");
    }

    #[test]
    fn test_check_action_build_dirty_sessions() {
        let c = Coordinator::new();
        let p1 = tmp_path("chk_d1.py");
        let p2 = tmp_path("chk_d2.py");
        c.register_native(&p1, &p1, sid("s1"));
        c.register_native(&p2, &p2, sid("s1"));
        c.mark_dirty(&p1, 5);
        let result = c.check_action("build").unwrap();
        assert_eq!(result.warnings.len(), 1);
        assert!(result.warnings[0].contains("unflushed edits"));
        assert!(result.suggestion.to_lowercase().contains("flush"));
    }

    #[test]
    fn test_check_action_build_external_files() {
        let c = Coordinator::new();
        c.register_external(&tmp_path("chk_ext.xlsx"), "sheets")
            .unwrap();
        let result = c.check_action("build").unwrap();
        assert_eq!(result.warnings.len(), 1);
        assert!(result.warnings[0].contains("externally managed"));
        assert!(result.suggestion.to_lowercase().contains("save"));
    }

    #[test]
    fn test_check_action_build_clean() {
        let c = Coordinator::new();
        let p1 = tmp_path("chk_c1.py");
        let p2 = tmp_path("chk_c2.py");
        c.register_native(&p1, &p1, sid("s1"));
        c.register_native(&p2, &p2, sid("s1"));
        c.mark_flushed(&p1);
        c.mark_flushed(&p2);
        let result = c.check_action("build").unwrap();
        assert!(result.warnings.is_empty());
        assert_eq!(result.suggestion, "Ready to build");
    }

    #[test]
    fn test_check_action_unknown() {
        let c = Coordinator::new();
        let result = c.check_action("deploy");
        assert!(matches!(result, Err(CoordinatorError::UnknownAction(_))));
    }

    #[test]
    fn test_status_returns_all_sections() {
        let c = Coordinator::new();
        let p1 = tmp_path("stat_n1.py");
        let p2 = tmp_path("stat_n2.py");
        c.register_native(&p1, &p1, sid("s1"));
        c.register_native(&p2, &p2, sid("s1"));
        c.mark_dirty(&p1, 2);
        c.register_external(&tmp_path("stat_ext.xlsx"), "sheets")
            .unwrap();
        let status = c.status();
        assert_eq!(status.tracked_files.len(), 3);
        assert_eq!(status.external_registrations.len(), 1);
        assert_eq!(status.warnings.len(), 1);
    }
}
