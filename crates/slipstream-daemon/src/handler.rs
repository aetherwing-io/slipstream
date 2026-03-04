use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use slipstream_core::flush::FlushResult;
use slipstream_core::manager::{ManagerError, SessionManager};
use slipstream_core::session::SessionId;

use crate::coordinator::{Coordinator, CoordinatorError};
use crate::protocol::{self, Request, Response, RpcError};
use crate::registry::{FormatRegistry, HandlerEntry};
use crate::types::*;

// --- Resource limits ---
pub const MAX_SESSIONS: usize = 64;
pub const MAX_FILES_PER_SESSION: usize = 32;
pub const MAX_BATCH_OPS: usize = 256;
pub const MAX_CONTENT_LINES_PER_WRITE: usize = 50_000;

// --- Verb dispatch ---

/// Result of executing a single operation verb against a session.
pub enum OpResult {
    Read {
        lines: Vec<String>,
        cursor: usize,
    },
    Write {
        edits_pending: usize,
        dirty: Option<(PathBuf, usize)>,
    },
    StrReplace {
        edits_pending: usize,
        match_line: usize,
        match_count: usize,
        dirty: Option<(PathBuf, usize)>,
    },
    CursorMove,
}

impl OpResult {
    /// Extract dirty file info (canonical path, edits_pending) for coordinator updates.
    pub fn dirty_info(&self) -> Option<(&Path, usize)> {
        match self {
            OpResult::Write { dirty: Some((p, n)), .. } => Some((p, *n)),
            OpResult::StrReplace { dirty: Some((p, n)), .. } => Some((p, *n)),
            _ => None,
        }
    }

    /// Serialize to JSON for wire response.
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            OpResult::Read { lines, cursor } => {
                serde_json::json!({"lines": lines, "cursor": cursor})
            }
            OpResult::Write { edits_pending, .. } => {
                serde_json::json!({"edits_pending": edits_pending})
            }
            OpResult::StrReplace { edits_pending, match_line, match_count, .. } => {
                serde_json::json!({"edits_pending": edits_pending, "match_line": match_line, "match_count": match_count})
            }
            OpResult::CursorMove => {
                serde_json::json!({"status": "ok"})
            }
        }
    }
}

/// Execute a single verb against an already-locked session.
/// Caller is responsible for: lock acquisition, content-size validation, coordinator side-effects.
pub fn dispatch_op(
    session: &mut slipstream_core::session::Session,
    op: Op,
    mgr: &SessionManager,
) -> Result<OpResult, slipstream_core::manager::ManagerError> {
    match op {
        Op::Read { path, start, end, count } => {
            let (lines, cursor) = if let (Some(start), Some(end)) = (start, end) {
                let lines = session.read(&path, start, end)?;
                (lines, end)
            } else if let Some(count) = count {
                session.read_next(&path, count)?
            } else {
                let handle = session.file(&path)?;
                let lc = handle.line_count()?;
                let lines = handle.read_range(0, lc)?;
                (lines, lc)
            };
            Ok(OpResult::Read { lines, cursor })
        }
        Op::Write { path, start, end, content } => {
            let count = session.write(&path, start, end, content)?;
            let dirty = mgr.canonical_path(&path)
                .ok()
                .map(|canonical| (canonical, count));
            Ok(OpResult::Write { edits_pending: count, dirty })
        }
        Op::StrReplace { path, old_str, new_str, replace_all } => {
            let (match_line, match_count, edits_pending) =
                session.str_replace(&path, &old_str, &new_str, replace_all)?;
            let dirty = mgr.canonical_path(&path)
                .ok()
                .map(|canonical| (canonical, edits_pending));
            Ok(OpResult::StrReplace { edits_pending, match_line, match_count, dirty })
        }
        Op::CursorMove { path, to } => {
            session.move_cursor(&path, to)?;
            Ok(OpResult::CursorMove)
        }
    }
}

/// Update coordinator dirty state from an OpResult.
fn apply_dirty(result: &OpResult, coordinator: &Coordinator) {
    if let Some((path, edits_pending)) = result.dirty_info() {
        coordinator.mark_dirty(path, edits_pending);
    }
}

/// Dispatch a JSON-RPC request to the appropriate handler.
pub fn dispatch(
    req: Request,
    mgr: &Arc<SessionManager>,
    registry: &Arc<FormatRegistry>,
    coordinator: &Arc<Coordinator>,
) -> Response {
    match req.method.as_str() {
        "session.open" => handle_session_open(req, mgr, registry, coordinator),
        "session.flush" => handle_session_flush(req, mgr, coordinator),
        "session.close" => handle_session_close(req, mgr, coordinator),
        "file.read" => handle_file_read(req, mgr),
        "file.write" => handle_file_write(req, mgr, coordinator),
        "file.str_replace" => handle_file_str_replace(req, mgr, coordinator),
        "cursor.move" => handle_cursor_move(req, mgr),
        "batch" => handle_batch(req, mgr, coordinator),
        "session.list" => handle_session_list(req, coordinator),
        "coordinator.status" => handle_coordinator_status(req, coordinator),
        "coordinator.register" => handle_coordinator_register(req, coordinator),
        "coordinator.unregister" => handle_coordinator_unregister(req, coordinator),
        "coordinator.check" => handle_coordinator_check(req, coordinator),
        _ => Response::err(
            req.id,
            RpcError {
                code: protocol::ERR_METHOD_NOT_FOUND,
                message: format!("unknown method: {}", req.method),
                data: None,
            },
        ),
    }
}

fn parse_params<T: serde::de::DeserializeOwned>(req: &mut Request) -> Result<T, Response> {
    serde_json::from_value(std::mem::take(&mut req.params)).map_err(|e| {
        Response::err(
            req.id,
            RpcError {
                code: protocol::ERR_INVALID_PARAMS,
                message: format!("invalid params: {e}"),
                data: None,
            },
        )
    })
}

fn internal_error(id: Option<u64>, msg: String) -> Response {
    Response::err(
        id,
        RpcError {
            code: protocol::ERR_INTERNAL,
            message: msg,
            data: None,
        },
    )
}

fn resource_limit_error(id: Option<u64>, msg: String) -> Response {
    Response::err(
        id,
        RpcError {
            code: protocol::ERR_RESOURCE_LIMIT,
            message: msg,
            data: None,
        },
    )
}

fn session_not_found(id: Option<u64>, session_id: &str) -> Response {
    Response::err(
        id,
        RpcError {
            code: protocol::ERR_SESSION_NOT_FOUND,
            message: format!("session not found: {session_id}"),
            data: None,
        },
    )
}

fn inject_digest(
    mut response: Response,
    coordinator: &Coordinator,
    cwd: &Path,
    session_filter: Option<&SessionId>,
) -> Response {
    if let Some(ref mut value) = response.result {
        let digest = coordinator.build_digest(cwd, session_filter);
        if let Ok(digest_val) = serde_json::to_value(&digest) {
            if let Some(obj) = value.as_object_mut() {
                obj.insert("session_digest".to_string(), digest_val);
            }
        }
    }
    response
}

fn cwd() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

fn handle_session_open(
    mut req: Request,
    mgr: &Arc<SessionManager>,
    registry: &Arc<FormatRegistry>,
    coordinator: &Arc<Coordinator>,
) -> Response {
    let params: SessionOpenParams = match parse_params(&mut req) {
        Ok(p) => p,
        Err(resp) => return resp,
    };

    // Resource limits
    if mgr.session_count() >= MAX_SESSIONS {
        return resource_limit_error(
            req.id,
            format!("session limit reached ({MAX_SESSIONS} max)"),
        );
    }
    if params.files.len() > MAX_FILES_PER_SESSION {
        return resource_limit_error(
            req.id,
            format!(
                "too many files in session: {} (max {MAX_FILES_PER_SESSION})",
                params.files.len()
            ),
        );
    }

    // Classify files by handler type
    let mut native_files: Vec<PathBuf> = Vec::new();
    let mut advisory_files: Vec<(PathBuf, String, String)> = Vec::new(); // (path, handler_name, guidance)
    let mut external_results: Vec<ExternalHandlerResult> = Vec::new();

    for path in &params.files {
        let filename = path
            .file_name()
            .map(|f| f.to_string_lossy().to_string())
            .unwrap_or_default();

        let entry = registry
            .lookup_filename(&filename)
            .or_else(|| {
                path.extension()
                    .and_then(|ext| registry.lookup_ext(&ext.to_string_lossy()))
            });

        match entry {
            Some(HandlerEntry::Full(cfg)) => {
                let handler_name = cfg.tool_prefix.clone();
                let tracking_id = match coordinator.register_external(path, &handler_name) {
                    Ok(id) => id,
                    Err(e) => return internal_error(req.id, format!("{e}")),
                };
                external_results.push(ExternalHandlerResult {
                    status: "external_handler".to_string(),
                    path: path.display().to_string(),
                    handler: handler_name,
                    description: cfg.description.clone(),
                    instructions: HandlerInstructions {
                        open: cfg.interpolate_path(path),
                        save: cfg.session_save.clone(),
                        help: cfg.help_tool.clone(),
                        examples: cfg.examples.clone(),
                    },
                    tracking_id,
                });
            }
            Some(HandlerEntry::Advisory(cfg)) => {
                advisory_files.push((
                    path.clone(),
                    cfg.description.clone(),
                    cfg.guidance.clone(),
                ));
            }
            None => {
                native_files.push(path.clone());
            }
        }
    }

    // Mixed list rejection
    if !external_results.is_empty() && (!native_files.is_empty() || !advisory_files.is_empty()) {
        let ext_path = &external_results[0].path;
        let handler = &external_results[0].handler;
        return Response::err(
            req.id,
            RpcError {
                code: protocol::ERR_INVALID_PARAMS,
                message: format!(
                    "mixed session.open: path {ext_path} is managed by {handler} — open external files separately"
                ),
                data: None,
            },
        );
    }

    // All external
    if !external_results.is_empty() {
        return Response::ok(req.id, external_results);
    }

    // Native + advisory open
    let all_paths: Vec<PathBuf> = native_files
        .iter()
        .chain(advisory_files.iter().map(|(p, _, _)| p))
        .cloned()
        .collect();

    let session_id: SessionId = uuid::Uuid::new_v4().to_string().into();
    let path_refs: Vec<&Path> = all_paths.iter().map(|p| p.as_path()).collect();

    if let Err(e) = mgr.create_session(session_id.clone(), &path_refs) {
        return internal_error(req.id, format!("{e}"));
    }

    // Register all files in coordinator
    for path in &all_paths {
        let canonical = mgr
            .canonical_path(path)
            .unwrap_or_else(|_| path.to_path_buf());
        coordinator.register_native(path, &canonical, session_id.clone());
    }

    // Build file info from session
    let files = match mgr.with_session(&session_id, |session| {
        let mut info = std::collections::HashMap::new();
        for (path, handle) in &session.files {
            let line_count = handle
                .line_count()
                .map_err(slipstream_core::manager::ManagerError::Session)?;
            let version = handle.snapshot_version;
            info.insert(
                path.clone(),
                FileInfo {
                    lines: line_count,
                    version,
                },
            );
        }
        Ok(info)
    }) {
        Ok(info) => info,
        Err(e) => return internal_error(req.id, format!("{e}")),
    };

    // Build response — if advisory files present, merge advisory fields
    if !advisory_files.is_empty() && native_files.is_empty() {
        // All advisory: wrap in advisory response
        let base = SessionOpenResult {
            session_id: session_id.clone(),
            files,
        };
        let mut val = match serde_json::to_value(base) {
            Ok(v) => v,
            Err(e) => return internal_error(req.id, format!("serialization error: {e}")),
        };
        if let Some(obj) = val.as_object_mut() {
            let (_, handler_name, guidance) = &advisory_files[0];
            obj.insert("status".to_string(), serde_json::json!("advisory"));
            obj.insert("handler".to_string(), serde_json::json!(handler_name));
            obj.insert("guidance".to_string(), serde_json::json!(guidance));
            obj.insert("loaded_as_text".to_string(), serde_json::json!(true));
        }
        let mut resp = Response {
            id: req.id,
            result: Some(val),
            error: None,
        };
        resp = inject_digest(resp, coordinator, &cwd(), Some(&session_id));
        return resp;
    }

    let resp = Response::ok(
        req.id,
        SessionOpenResult {
            session_id: session_id.clone(),
            files,
        },
    );
    inject_digest(resp, coordinator, &cwd(), Some(&session_id))
}

fn handle_session_flush(
    mut req: Request,
    mgr: &Arc<SessionManager>,
    coordinator: &Arc<Coordinator>,
) -> Response {
    let params: SessionFlushParams = match parse_params(&mut req) {
        Ok(p) => p,
        Err(resp) => return resp,
    };

    match mgr.flush_session(&params.session_id, params.force) {
        Ok(FlushResult::Ok { files_written, warnings }) => {
            // Mark flushed files in coordinator
            for f in &files_written {
                let canonical = mgr
                    .canonical_path(&f.path)
                    .unwrap_or_else(|_| f.path.clone());
                coordinator.mark_flushed(&canonical);
            }
            let files: Vec<FileWrittenInfo> = files_written
                .into_iter()
                .map(|f| FileWrittenInfo {
                    path: f.path,
                    edits_applied: f.edits_applied,
                })
                .collect();
            let warning_strs: Vec<FlushWarningInfo> = warnings
                .into_iter()
                .map(|w| FlushWarningInfo {
                    path: w.path,
                    other_session: w.other_session.as_str().to_owned(),
                    pending_edit_count: w.pending_edit_count,
                })
                .collect();
            let resp = Response::ok(
                req.id,
                SessionFlushResult {
                    status: "ok".into(),
                    files_written: files,
                    warnings: warning_strs,
                },
            );
            inject_digest(resp, coordinator, &cwd(), Some(&params.session_id))
        }
        Ok(FlushResult::Conflict { conflicts }) => {
            let data: Vec<ConflictData> = conflicts
                .into_iter()
                .map(|c| ConflictData {
                    path: c.path,
                    your_edits: c.your_edits,
                    conflicting_edits: c.conflicting_edits,
                    by_session: c.by_session.as_str().to_owned(),
                    hint: "Re-read conflicting ranges and retry, or use force:true to overwrite"
                        .into(),
                })
                .collect();
            Response::err(
                req.id,
                RpcError {
                    code: protocol::ERR_CONFLICT,
                    message: "conflicting edits detected".into(),
                    data: serde_json::to_value(data).ok(),
                },
            )
        }
        Err(e) => match_manager_error(req.id, params.session_id.as_str(), e),
    }
}

fn handle_session_close(
    mut req: Request,
    mgr: &Arc<SessionManager>,
    coordinator: &Arc<Coordinator>,
) -> Response {
    let params: SessionCloseParams = match parse_params(&mut req) {
        Ok(p) => p,
        Err(resp) => return resp,
    };

    match mgr.close_session(&params.session_id) {
        Ok(()) => {
            coordinator.remove_closed_by_session(&params.session_id);
            let resp = Response::ok(req.id, serde_json::json!({"status": "closed"}));
            inject_digest(resp, coordinator, &cwd(), Some(&params.session_id))
        }
        Err(e) => match_manager_error(req.id, params.session_id.as_str(), e),
    }
}

fn handle_file_read(mut req: Request, mgr: &Arc<SessionManager>) -> Response {
    let params: FileReadParams = match parse_params(&mut req) {
        Ok(p) => p,
        Err(resp) => return resp,
    };

    let op = Op::Read {
        path: params.path.clone(),
        start: params.start,
        end: params.end,
        count: params.count,
    };

    let result = mgr.with_session_mut(&params.session_id, |session| {
        dispatch_op(session, op, mgr)
    });

    match result {
        Ok(op_result) => {
            if let OpResult::Read { lines, cursor } = op_result {
                // Standalone reads populate other_sessions (batch intentionally skips this)
                let canonical = mgr.canonical_path(&params.path);
                let other_sessions = if let Ok(canonical) = canonical {
                    mgr.other_sessions_info(&params.session_id, &canonical)
                        .unwrap_or_default()
                        .into_iter()
                        .map(|(id, ranges)| OtherSessionInfo {
                            session: id.as_str().to_owned(),
                            dirty_ranges: ranges,
                        })
                        .collect()
                } else {
                    Vec::new()
                };

                // No digest injection on reads
                Response::ok(
                    req.id,
                    FileReadResult {
                        lines,
                        cursor,
                        other_sessions,
                    },
                )
            } else {
                unreachable!("dispatch_op(Op::Read) always returns OpResult::Read")
            }
        }
        Err(e) => match_manager_error(req.id, params.session_id.as_str(), e),
    }
}

fn handle_file_write(
    mut req: Request,
    mgr: &Arc<SessionManager>,
    coordinator: &Arc<Coordinator>,
) -> Response {
    let params: FileWriteParams = match parse_params(&mut req) {
        Ok(p) => p,
        Err(resp) => return resp,
    };

    if params.content.len() > MAX_CONTENT_LINES_PER_WRITE {
        return resource_limit_error(
            req.id,
            format!(
                "content too large: {} lines (max {MAX_CONTENT_LINES_PER_WRITE})",
                params.content.len()
            ),
        );
    }

    let op = Op::Write {
        path: params.path,
        start: params.start,
        end: params.end,
        content: params.content,
    };

    match mgr.with_session_mut(&params.session_id, |session| {
        dispatch_op(session, op, mgr)
    }) {
        Ok(op_result) => {
            apply_dirty(&op_result, coordinator);
            let resp = Response::ok(req.id, op_result.to_json());
            inject_digest(resp, coordinator, &cwd(), Some(&params.session_id))
        }
        Err(e) => match_manager_error(req.id, params.session_id.as_str(), e),
    }
}

fn handle_file_str_replace(
    mut req: Request,
    mgr: &Arc<SessionManager>,
    coordinator: &Arc<Coordinator>,
) -> Response {
    let params: FileStrReplaceParams = match parse_params(&mut req) {
        Ok(p) => p,
        Err(resp) => return resp,
    };

    let op = Op::StrReplace {
        path: params.path,
        old_str: params.old_str,
        new_str: params.new_str,
        replace_all: params.replace_all,
    };

    match mgr.with_session_mut(&params.session_id, |session| {
        dispatch_op(session, op, mgr)
    }) {
        Ok(op_result) => {
            apply_dirty(&op_result, coordinator);
            let resp = Response::ok(req.id, op_result.to_json());
            inject_digest(resp, coordinator, &cwd(), Some(&params.session_id))
        }
        Err(e) => match_manager_error(req.id, params.session_id.as_str(), e),
    }
}

fn handle_cursor_move(mut req: Request, mgr: &Arc<SessionManager>) -> Response {
    let params: CursorMoveParams = match parse_params(&mut req) {
        Ok(p) => p,
        Err(resp) => return resp,
    };

    let op = Op::CursorMove {
        path: params.path,
        to: params.to,
    };

    match mgr.with_session_mut(&params.session_id, |session| {
        dispatch_op(session, op, mgr)
    }) {
        Ok(op_result) => Response::ok(req.id, op_result.to_json()),
        Err(e) => match_manager_error(req.id, params.session_id.as_str(), e),
    }
}

fn handle_batch(
    mut req: Request,
    mgr: &Arc<SessionManager>,
    coordinator: &Arc<Coordinator>,
) -> Response {
    let params: BatchParams = match parse_params(&mut req) {
        Ok(p) => p,
        Err(resp) => return resp,
    };

    let session_id = params.session_id;
    let ops = params.ops;

    if ops.len() > MAX_BATCH_OPS {
        return resource_limit_error(
            req.id,
            format!(
                "too many batch operations: {} (max {MAX_BATCH_OPS})",
                ops.len()
            ),
        );
    }

    // Validate content size limits before entering the session lock
    for op in &ops {
        if let Op::Write { content, .. } = op {
            if content.len() > MAX_CONTENT_LINES_PER_WRITE {
                return resource_limit_error(
                    req.id,
                    format!(
                        "content too large: {} lines (max {MAX_CONTENT_LINES_PER_WRITE})",
                        content.len()
                    ),
                );
            }
        }
    }

    let had_mutations = ops.iter().any(|op| op.is_mutation());

    let op_count = ops.len();
    let results: Result<Vec<OpResult>, _> =
        mgr.with_session_mut(&session_id, |session| {
            // Partition ops: phase1 (non-replace_all) vs phase2 (replace_all)
            let mut phase1_ops: Vec<(usize, Op)> = Vec::new();
            let mut phase2_ops: Vec<(usize, Op)> = Vec::new();
            for (idx, op) in ops.into_iter().enumerate() {
                if op.is_replace_all() {
                    phase2_ops.push((idx, op));
                } else {
                    phase1_ops.push((idx, op));
                }
            }

            let mut results: Vec<Option<OpResult>> = (0..op_count).map(|_| None).collect();

            // Phase 1: Execute non-replace_all ops (queue edits on original buffer)
            for (idx, op) in phase1_ops {
                match dispatch_op(session, op, mgr) {
                    Ok(result) => results[idx] = Some(result),
                    Err(e) => {
                        return Err(ManagerError::BatchOpFailed {
                            index: idx,
                            total: op_count,
                            source: Box::new(e),
                        });
                    }
                }
            }

            // Phase 2: Execute replace_all ops against materialized buffer
            if !phase2_ops.is_empty() {
                let mut phase2_by_file: HashMap<PathBuf, Vec<(usize, String, String)>> =
                    HashMap::new();
                for (idx, op) in phase2_ops {
                    if let Op::StrReplace {
                        path,
                        old_str,
                        new_str,
                        ..
                    } = op
                    {
                        phase2_by_file
                            .entry(path)
                            .or_default()
                            .push((idx, old_str, new_str));
                    }
                }

                for (path, ops_for_file) in phase2_by_file {
                    let handle = session.file_mut(&path)?;
                    let original_len = handle.original_line_count();
                    let mut materialized = handle.materialized_lines();
                    handle.clear_edits();

                    for (idx, old_str, new_str) in ops_for_file {
                        let (match_line, match_count) =
                            slipstream_core::session::Session::str_replace_on_materialized(
                                &mut materialized,
                                &old_str,
                                &new_str,
                            )
                            .map_err(|e| ManagerError::BatchOpFailed {
                                index: idx,
                                total: op_count,
                                source: Box::new(ManagerError::Session(e)),
                            })?;

                        let dirty = mgr
                            .canonical_path(&path)
                            .ok()
                            .map(|c| (c, 1));
                        results[idx] = Some(OpResult::StrReplace {
                            edits_pending: 1,
                            match_line,
                            match_count,
                            dirty,
                        });
                    }

                    // Queue single edit replacing entire file with final content
                    handle.queue_edit(0, original_len, materialized);
                }
            }

            let results: Vec<OpResult> =
                results.into_iter().map(|r| r.unwrap()).collect();
            Ok(results)
        });

    match results {
        Ok(op_results) => {
            // Update coordinator state for all mutated files
            for r in &op_results {
                apply_dirty(r, coordinator);
            }
            let values: Vec<serde_json::Value> = op_results.iter().map(|r| r.to_json()).collect();
            let resp = Response::ok(req.id, values);
            if had_mutations {
                inject_digest(resp, coordinator, &cwd(), Some(&session_id))
            } else {
                resp
            }
        }
        Err(e) => match_manager_error(req.id, session_id.as_str(), e),
    }
}

// --- Coordinator handlers ---

fn handle_session_list(req: Request, coordinator: &Arc<Coordinator>) -> Response {
    let result = coordinator.list_sessions();
    let val = match serde_json::to_value(result) {
        Ok(v) => v,
        Err(e) => return internal_error(req.id, format!("serialization error: {e}")),
    };
    Response::ok(req.id, val)
}

fn handle_coordinator_status(req: Request, coordinator: &Arc<Coordinator>) -> Response {
    let status = coordinator.status();
    let val = match serde_json::to_value(status) {
        Ok(v) => v,
        Err(e) => return internal_error(req.id, format!("serialization error: {e}")),
    };
    let resp = Response::ok(req.id, val);
    inject_digest(resp, coordinator, &cwd(), None)
}

fn handle_coordinator_register(
    mut req: Request,
    coordinator: &Arc<Coordinator>,
) -> Response {
    let params: CoordinatorRegisterParams = match parse_params(&mut req) {
        Ok(p) => p,
        Err(resp) => return resp,
    };
    let path = Path::new(&params.path);
    let tracking_id = match coordinator.register_external(path, &params.handler) {
        Ok(id) => id,
        Err(CoordinatorError::InvalidHandlerName(msg)) => {
            return Response::err(
                req.id,
                RpcError {
                    code: protocol::ERR_INVALID_PARAMS,
                    message: format!("invalid handler name: {msg}"),
                    data: None,
                },
            );
        }
        Err(e) => return internal_error(req.id, e.to_string()),
    };
    let result = CoordinatorRegisterResult {
        tracking_id,
        status: "registered".to_string(),
    };
    let val = match serde_json::to_value(result) {
        Ok(v) => v,
        Err(e) => return internal_error(req.id, format!("serialization error: {e}")),
    };
    let resp = Response::ok(req.id, val);
    inject_digest(resp, coordinator, &cwd(), None)
}

fn handle_coordinator_unregister(mut req: Request, coordinator: &Arc<Coordinator>) -> Response {
    let params: CoordinatorUnregisterParams = match parse_params(&mut req) {
        Ok(p) => p,
        Err(resp) => return resp,
    };
    match coordinator.unregister(&params.tracking_id) {
        Ok(()) => {
            let resp = Response::ok(req.id, serde_json::json!({}));
            inject_digest(resp, coordinator, &cwd(), None)
        }
        Err(CoordinatorError::TrackingIdNotFound(_)) => Response::err(
            req.id,
            RpcError {
                code: 404,
                message: "tracking_id not found".to_string(),
                data: None,
            },
        ),
        Err(e) => internal_error(req.id, e.to_string()),
    }
}

fn handle_coordinator_check(mut req: Request, coordinator: &Arc<Coordinator>) -> Response {
    let params: CoordinatorCheckParams = match parse_params(&mut req) {
        Ok(p) => p,
        Err(resp) => return resp,
    };
    let result = coordinator.check_action(params.action);
    let val = match serde_json::to_value(result) {
        Ok(v) => v,
        Err(e) => return internal_error(req.id, format!("serialization error: {e}")),
    };
    let resp = Response::ok(req.id, val);
    inject_digest(resp, coordinator, &cwd(), None)
}

fn match_manager_error(
    id: Option<u64>,
    session_id: &str,
    err: slipstream_core::manager::ManagerError,
) -> Response {
    use slipstream_core::manager::ManagerError;
    match err {
        ManagerError::SessionNotFound(_) => session_not_found(id, session_id),
        other => internal_error(id, format!("{other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    use tempfile::NamedTempFile;

    fn temp_file(content: &str) -> NamedTempFile {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f.flush().unwrap();
        f
    }

    fn make_request(method: &str, params: serde_json::Value) -> Request {
        Request {
            id: Some(1),
            method: method.to_string(),
            params,
        }
    }

    fn default_registry() -> Arc<FormatRegistry> {
        Arc::new(FormatRegistry::default_registry())
    }

    fn default_coordinator() -> Arc<Coordinator> {
        Arc::new(Coordinator::new())
    }

    /// Open a session with the given files and return (session_id, mgr, coordinator).
    fn open_session(
        files: &[&NamedTempFile],
    ) -> (String, Arc<SessionManager>, Arc<FormatRegistry>, Arc<Coordinator>) {
        let mgr = Arc::new(SessionManager::new());
        let reg = default_registry();
        let coord = default_coordinator();
        let paths: Vec<serde_json::Value> = files
            .iter()
            .map(|f| serde_json::Value::String(f.path().to_str().unwrap().to_string()))
            .collect();

        let req = make_request("session.open", serde_json::json!({ "files": paths }));
        let resp = dispatch(req, &mgr, &reg, &coord);
        let result = resp.result.expect("session.open should succeed");
        let session_id = result["session_id"].as_str().unwrap().to_string();
        (session_id, mgr, reg, coord)
    }

    fn result_ok(resp: &Response) -> &serde_json::Value {
        assert!(
            resp.error.is_none(),
            "expected ok, got error: {:?}",
            resp.error
        );
        resp.result.as_ref().unwrap()
    }

    #[test]
    fn batch_multiple_reads() {
        let f1 = temp_file("alpha\nbeta\ngamma\n");
        let f2 = temp_file("one\ntwo\nthree\n");
        let (sid, mgr, reg, coord) = open_session(&[&f1, &f2]);

        let req = make_request(
            "batch",
            serde_json::json!({
                "session_id": sid,
                "ops": [
                    {"method": "file.read", "path": f1.path(), "start": 0, "end": 2},
                    {"method": "file.read", "path": f2.path(), "start": 1, "end": 3},
                ]
            }),
        );
        let resp = dispatch(req, &mgr, &reg, &coord);
        let results = result_ok(&resp);
        let arr = results.as_array().unwrap();
        assert_eq!(arr.len(), 2);

        let lines1: Vec<&str> = arr[0]["lines"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(lines1, vec!["alpha", "beta"]);

        let lines2: Vec<&str> = arr[1]["lines"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(lines2, vec!["two", "three"]);
    }

    #[test]
    fn batch_read_and_write() {
        let f1 = temp_file("line0\nline1\nline2\n");
        let (sid, mgr, reg, coord) = open_session(&[&f1]);

        let req = make_request(
            "batch",
            serde_json::json!({
                "session_id": sid,
                "ops": [
                    {"method": "file.read", "path": f1.path(), "start": 0, "end": 1},
                    {"method": "file.write", "path": f1.path(), "start": 1, "end": 2, "content": ["REPLACED"]},
                ]
            }),
        );
        let resp = dispatch(req, &mgr, &reg, &coord);
        let arr = result_ok(&resp).as_array().unwrap();
        assert_eq!(arr.len(), 2);

        let lines: Vec<&str> = arr[0]["lines"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(lines, vec!["line0"]);
        assert_eq!(arr[1]["edits_pending"].as_u64().unwrap(), 1);
    }

    #[test]
    fn batch_write_then_read_returns_original() {
        let f1 = temp_file("aaa\nbbb\nccc\n");
        let (sid, mgr, reg, coord) = open_session(&[&f1]);

        let req = make_request(
            "batch",
            serde_json::json!({
                "session_id": sid,
                "ops": [
                    {"method": "file.write", "path": f1.path(), "start": 0, "end": 1, "content": ["XXX"]},
                    {"method": "file.read", "path": f1.path(), "start": 0, "end": 1},
                ]
            }),
        );
        let resp = dispatch(req, &mgr, &reg, &coord);
        let arr = result_ok(&resp).as_array().unwrap();

        assert_eq!(arr[0]["edits_pending"].as_u64().unwrap(), 1);
        let lines: Vec<&str> = arr[1]["lines"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(lines, vec!["aaa"]);
    }

    #[test]
    fn batch_cursor_move() {
        let f1 = temp_file("a\nb\nc\nd\ne\n");
        let (sid, mgr, reg, coord) = open_session(&[&f1]);

        let req = make_request(
            "batch",
            serde_json::json!({
                "session_id": sid,
                "ops": [
                    {"method": "cursor.move", "path": f1.path(), "to": 2},
                    {"method": "file.read", "path": f1.path(), "count": 2},
                ]
            }),
        );
        let resp = dispatch(req, &mgr, &reg, &coord);
        let arr = result_ok(&resp).as_array().unwrap();
        assert_eq!(arr.len(), 2);

        assert_eq!(arr[0]["status"].as_str().unwrap(), "ok");
        let lines: Vec<&str> = arr[1]["lines"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(lines, vec!["c", "d"]);
        assert_eq!(arr[1]["cursor"].as_u64().unwrap(), 4);
    }

    #[test]
    fn batch_error_propagates() {
        let f1 = temp_file("hello\n");
        let (sid, mgr, reg, coord) = open_session(&[&f1]);

        let req = make_request(
            "batch",
            serde_json::json!({
                "session_id": sid,
                "ops": [
                    {"method": "file.read", "path": f1.path(), "start": 0, "end": 1},
                    {"method": "file.read", "path": "/nonexistent/file.txt", "start": 0, "end": 1},
                ]
            }),
        );
        let resp = dispatch(req, &mgr, &reg, &coord);
        assert!(resp.error.is_some(), "batch should fail when an op errors");
    }

    #[test]
    fn batch_empty_ops() {
        let f1 = temp_file("x\n");
        let (sid, mgr, reg, coord) = open_session(&[&f1]);

        let req = make_request(
            "batch",
            serde_json::json!({
                "session_id": sid,
                "ops": []
            }),
        );
        let resp = dispatch(req, &mgr, &reg, &coord);
        let arr = result_ok(&resp).as_array().unwrap();
        assert!(arr.is_empty());
    }

    #[test]
    fn read_includes_other_sessions() {
        let f1 = temp_file("line0\nline1\nline2\nline3\n");
        let mgr = Arc::new(SessionManager::new());
        let reg = default_registry();
        let coord = default_coordinator();

        let open_a = make_request("session.open", serde_json::json!({"files": [f1.path()]}));
        let resp_a = dispatch(open_a, &mgr, &reg, &coord);
        let sid_a = result_ok(&resp_a)["session_id"].as_str().unwrap().to_string();

        let open_b = make_request("session.open", serde_json::json!({"files": [f1.path()]}));
        let resp_b = dispatch(open_b, &mgr, &reg, &coord);
        let sid_b = result_ok(&resp_b)["session_id"].as_str().unwrap().to_string();

        let write_req = make_request(
            "file.write",
            serde_json::json!({
                "session_id": sid_a,
                "path": f1.path(),
                "start": 1, "end": 2,
                "content": ["CHANGED"]
            }),
        );
        dispatch(write_req, &mgr, &reg, &coord);

        let read_req = make_request(
            "file.read",
            serde_json::json!({
                "session_id": sid_b,
                "path": f1.path(),
                "start": 0, "end": 4
            }),
        );
        let resp = dispatch(read_req, &mgr, &reg, &coord);
        let result = result_ok(&resp);

        let other = result["other_sessions"].as_array().unwrap();
        assert_eq!(other.len(), 1);
        assert_eq!(other[0]["session"].as_str().unwrap(), sid_a);

        let ranges = other[0]["dirty_ranges"].as_array().unwrap();
        assert!(!ranges.is_empty(), "session A should have dirty ranges");
    }

    // --- Format-aware open tests ---

    #[test]
    fn handler_open_native_text_file() {
        let f = temp_file("hello\nworld\n");
        let mgr = Arc::new(SessionManager::new());
        let reg = default_registry();
        let coord = default_coordinator();

        let req = make_request("session.open", serde_json::json!({"files": [f.path()]}));
        let resp = dispatch(req, &mgr, &reg, &coord);
        let result = result_ok(&resp);

        assert!(result["session_id"].as_str().is_some());
        let digest = &result["session_digest"];
        assert_eq!(digest["total_tracked"], 1);
        assert_eq!(digest["native_count"], 1);
    }

    #[test]
    fn handler_open_external_xlsx() {
        let f = tempfile::Builder::new()
            .suffix(".xlsx")
            .tempfile()
            .unwrap();
        let mgr = Arc::new(SessionManager::new());
        let reg = default_registry();
        let coord = default_coordinator();

        let req = make_request("session.open", serde_json::json!({"files": [f.path()]}));
        let resp = dispatch(req, &mgr, &reg, &coord);
        let result = result_ok(&resp);
        // External returns an array
        let arr = result.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["status"], "external_handler");
        assert_eq!(arr[0]["handler"], "sheets");
        let tid = arr[0]["tracking_id"].as_str().unwrap();
        assert!(tid.starts_with("ext-"));
        let instructions = &arr[0]["instructions"];
        assert!(instructions["open"].as_str().unwrap().contains(&f.path().display().to_string()));

        let digest = coord.build_digest(&cwd(), None);
        assert_eq!(digest.external_count, 1);
    }

    #[test]
    fn handler_open_advisory_makefile() {
        let dir = tempfile::tempdir().unwrap();
        let makefile_path = dir.path().join("Makefile");
        std::fs::write(&makefile_path, "all:\n\techo hello\n").unwrap();

        let mgr = Arc::new(SessionManager::new());
        let reg = default_registry();
        let coord = default_coordinator();

        let req = make_request("session.open", serde_json::json!({"files": [makefile_path]}));
        let resp = dispatch(req, &mgr, &reg, &coord);
        let result = result_ok(&resp);

        assert_eq!(result["status"], "advisory");
        assert_eq!(result["loaded_as_text"], true);
        assert!(result["guidance"].as_str().unwrap().to_lowercase().contains("make"));
        assert!(result["session_id"].as_str().is_some());

        let digest = coord.build_digest(&cwd(), None);
        assert_eq!(digest.native_count, 1);
    }

    #[test]
    fn handler_open_mixed_rejects() {
        let py = temp_file("x = 1\n");
        let xlsx = tempfile::Builder::new()
            .suffix(".xlsx")
            .tempfile()
            .unwrap();
        let mgr = Arc::new(SessionManager::new());
        let reg = default_registry();
        let coord = default_coordinator();

        let req = make_request(
            "session.open",
            serde_json::json!({"files": [py.path(), xlsx.path()]}),
        );
        let resp = dispatch(req, &mgr, &reg, &coord);
        assert!(resp.result.is_none(), "mixed open should error");
        assert!(resp.error.is_some());
        let msg = resp.error.as_ref().unwrap().message.clone();
        assert!(msg.contains("mixed session.open"), "error should mention mixed: {msg}");
    }

    // --- Mutation + coordinator integration tests ---

    #[test]
    fn handler_write_updates_coordinator() {
        let f = temp_file("line0\nline1\n");
        let (sid, mgr, reg, coord) = open_session(&[&f]);

        let req = make_request(
            "file.write",
            serde_json::json!({
                "session_id": sid,
                "path": f.path(),
                "start": 0, "end": 1,
                "content": ["REPLACED"]
            }),
        );
        let resp = dispatch(req, &mgr, &reg, &coord);
        let result = result_ok(&resp);
        assert_eq!(result["session_digest"]["native_dirty"], 1);
    }

    #[test]
    fn handler_flush_marks_flushed() {
        let f = temp_file("original\n");
        let (sid, mgr, reg, coord) = open_session(&[&f]);

        // Write
        let req = make_request(
            "file.write",
            serde_json::json!({
                "session_id": sid,
                "path": f.path(),
                "start": 0, "end": 1,
                "content": ["changed"]
            }),
        );
        dispatch(req, &mgr, &reg, &coord);

        // Flush
        let req = make_request(
            "session.flush",
            serde_json::json!({"session_id": sid}),
        );
        let resp = dispatch(req, &mgr, &reg, &coord);
        let result = result_ok(&resp);
        assert!(result.get("session_digest").is_some());
        assert_eq!(result["session_digest"]["native_dirty"], 0);
    }

    #[test]
    fn handler_close_marks_closed() {
        let f = temp_file("data\n");
        let (sid, mgr, reg, coord) = open_session(&[&f]);

        let req = make_request("session.close", serde_json::json!({"session_id": sid}));
        let resp = dispatch(req, &mgr, &reg, &coord);
        let result = result_ok(&resp);
        assert!(result.get("session_digest").is_some());

        // All coordinator entries for this session should be closed
        let status = coord.status();
        for tf in &status.tracked_files {
            assert!(
                matches!(tf.state, crate::coordinator::FileState::Closed),
                "file should be closed after session.close"
            );
        }
    }

    #[test]
    fn handler_str_replace_updates_coordinator() {
        let f = temp_file("hello world\n");
        let (sid, mgr, reg, coord) = open_session(&[&f]);

        let req = make_request(
            "file.str_replace",
            serde_json::json!({
                "session_id": sid,
                "path": f.path(),
                "old_str": "hello world",
                "new_str": "goodbye world"
            }),
        );
        let resp = dispatch(req, &mgr, &reg, &coord);
        let result = result_ok(&resp);
        assert_eq!(result["session_digest"]["native_dirty"], 1);
        assert!(result.get("session_digest").is_some());
    }

    // --- Coordinator RPC handler tests ---

    #[test]
    fn handler_coordinator_register() {
        let mgr = Arc::new(SessionManager::new());
        let reg = default_registry();
        let coord = default_coordinator();

        let req = make_request(
            "coordinator.register",
            serde_json::json!({"path": "/some/file.xlsx", "handler": "sheets"}),
        );
        let resp = dispatch(req, &mgr, &reg, &coord);
        let result = result_ok(&resp);
        assert!(result["tracking_id"].as_str().unwrap().starts_with("ext-"));
        assert_eq!(result["status"], "registered");
        assert!(result.get("session_digest").is_some());
    }

    #[test]
    fn handler_coordinator_unregister() {
        let mgr = Arc::new(SessionManager::new());
        let reg = default_registry();
        let coord = default_coordinator();

        // Register first
        let req = make_request(
            "coordinator.register",
            serde_json::json!({"path": "/some/file.xlsx", "handler": "sheets"}),
        );
        let resp = dispatch(req, &mgr, &reg, &coord);
        let tid = result_ok(&resp)["tracking_id"].as_str().unwrap().to_string();

        // Unregister
        let req = make_request(
            "coordinator.unregister",
            serde_json::json!({"tracking_id": tid}),
        );
        let resp = dispatch(req, &mgr, &reg, &coord);
        result_ok(&resp); // should succeed
        assert_eq!(coord.build_digest(&cwd(), None).total_tracked, 0);
    }

    #[test]
    fn handler_coordinator_unregister_unknown_id() {
        let mgr = Arc::new(SessionManager::new());
        let reg = default_registry();
        let coord = default_coordinator();

        let req = make_request(
            "coordinator.unregister",
            serde_json::json!({"tracking_id": "ext-999"}),
        );
        let resp = dispatch(req, &mgr, &reg, &coord);
        assert!(resp.error.is_some());
        assert_eq!(resp.error.as_ref().unwrap().code, 404);
    }

    #[test]
    fn handler_coordinator_check_dirty() {
        let f = temp_file("content\n");
        let (sid, mgr, reg, coord) = open_session(&[&f]);

        // Write without flushing
        let req = make_request(
            "file.write",
            serde_json::json!({
                "session_id": sid,
                "path": f.path(),
                "start": 0, "end": 1,
                "content": ["modified"]
            }),
        );
        dispatch(req, &mgr, &reg, &coord);

        let req = make_request(
            "coordinator.check",
            serde_json::json!({"action": "build"}),
        );
        let resp = dispatch(req, &mgr, &reg, &coord);
        let result = result_ok(&resp);
        let warnings = result["warnings"].as_array().unwrap();
        assert!(!warnings.is_empty());
        assert!(result["suggestion"].as_str().unwrap().to_lowercase().contains("flush"));
    }

    #[test]
    fn handler_coordinator_check_clean() {
        let f = temp_file("clean\n");
        let (sid, mgr, reg, coord) = open_session(&[&f]);

        // Write then flush
        let req = make_request(
            "file.write",
            serde_json::json!({
                "session_id": sid,
                "path": f.path(),
                "start": 0, "end": 1,
                "content": ["changed"]
            }),
        );
        dispatch(req, &mgr, &reg, &coord);

        let req = make_request("session.flush", serde_json::json!({"session_id": sid}));
        dispatch(req, &mgr, &reg, &coord);

        let req = make_request(
            "coordinator.check",
            serde_json::json!({"action": "build"}),
        );
        let resp = dispatch(req, &mgr, &reg, &coord);
        let result = result_ok(&resp);
        let warnings = result["warnings"].as_array().unwrap();
        assert!(warnings.is_empty());
        assert_eq!(result["suggestion"], "Ready to build");
    }

    #[test]
    fn handler_coordinator_check_unknown_action() {
        let mgr = Arc::new(SessionManager::new());
        let reg = default_registry();
        let coord = default_coordinator();

        let req = make_request(
            "coordinator.check",
            serde_json::json!({"action": "deploy"}),
        );
        let resp = dispatch(req, &mgr, &reg, &coord);
        assert!(resp.error.is_some());
    }

    #[test]
    fn handler_coordinator_status() {
        let f = temp_file("data\n");
        let (_sid, mgr, reg, coord) = open_session(&[&f]);

        // Also register an external
        let req = make_request(
            "coordinator.register",
            serde_json::json!({"path": "/tmp/test.xlsx", "handler": "sheets"}),
        );
        dispatch(req, &mgr, &reg, &coord);

        let req = make_request("coordinator.status", serde_json::json!({}));
        let resp = dispatch(req, &mgr, &reg, &coord);
        let result = result_ok(&resp);
        assert!(result["tracked_files"].as_array().is_some());
        assert!(result["native_sessions"].as_array().is_some());
        assert!(result["external_registrations"].as_array().is_some());
        assert!(result["warnings"].as_array().is_some());
    }

    // --- Read-only ops should NOT include digest ---

    #[test]
    fn handler_file_read_no_digest() {
        let f = temp_file("line\n");
        let (sid, mgr, reg, coord) = open_session(&[&f]);

        let req = make_request(
            "file.read",
            serde_json::json!({
                "session_id": sid,
                "path": f.path(),
                "start": 0, "end": 1
            }),
        );
        let resp = dispatch(req, &mgr, &reg, &coord);
        let result = result_ok(&resp);
        assert!(result.get("session_digest").is_none());
    }

    #[test]
    fn handler_cursor_move_no_digest() {
        let f = temp_file("a\nb\nc\n");
        let (sid, mgr, reg, coord) = open_session(&[&f]);

        let req = make_request(
            "cursor.move",
            serde_json::json!({
                "session_id": sid,
                "path": f.path(),
                "to": 1
            }),
        );
        let resp = dispatch(req, &mgr, &reg, &coord);
        let result = result_ok(&resp);
        assert!(result.get("session_digest").is_none());
    }

    #[test]
    fn read_no_other_sessions_when_alone() {
        let f1 = temp_file("solo\n");
        let (sid, mgr, reg, coord) = open_session(&[&f1]);

        let req = make_request(
            "file.read",
            serde_json::json!({
                "session_id": sid,
                "path": f1.path(),
                "start": 0, "end": 1
            }),
        );
        let resp = dispatch(req, &mgr, &reg, &coord);
        let result = result_ok(&resp);

        let other = result.get("other_sessions");
        assert!(
            other.is_none() || other.unwrap().as_array().unwrap().is_empty(),
            "should have no other_sessions when alone"
        );
    }

    #[test]
    fn batch_replace_all_sees_prior_str_replace() {
        // The benchmark scenario: multi-line str_replace inserts a copyright header
        // at lines 0-2, and replace_all renames a function throughout the file.
        // Without two-phase, the large edit overwrites the rename.
        let f = temp_file("import { get_connection } from 'db';\nconst pool = get_connection();\nfunction main() {\n  const db = get_connection();\n  return db;\n}\n");
        let (sid, mgr, reg, coord) = open_session(&[&f]);

        let req = make_request(
            "batch",
            serde_json::json!({
                "session_id": sid,
                "ops": [
                    {
                        "method": "file.str_replace",
                        "path": f.path(),
                        "old_str": "import { get_connection } from 'db';\nconst pool = get_connection();",
                        "new_str": "// Copyright 2026\nimport { acquire_connection } from 'db';\nconst pool = acquire_connection();",
                        "replace_all": false
                    },
                    {
                        "method": "file.str_replace",
                        "path": f.path(),
                        "old_str": "get_connection",
                        "new_str": "acquire_connection",
                        "replace_all": true
                    }
                ]
            }),
        );
        let resp = dispatch(req, &mgr, &reg, &coord);
        let results = result_ok(&resp);
        let arr = results.as_array().unwrap();
        assert_eq!(arr.len(), 2);

        // replace_all should have found the remaining occurrence in the function body
        let replace_all_result = &arr[1];
        assert!(replace_all_result["match_count"].as_u64().unwrap() >= 1);

        // Flush and verify the file content
        let flush_req = make_request(
            "session.flush",
            serde_json::json!({"session_id": sid}),
        );
        let flush_resp = dispatch(flush_req, &mgr, &reg, &coord);
        assert!(flush_resp.error.is_none(), "flush should succeed");

        let content = std::fs::read_to_string(f.path()).unwrap();
        assert!(
            !content.contains("get_connection"),
            "all occurrences should be renamed, got: {content}"
        );
        assert!(content.contains("acquire_connection"));
        assert!(content.contains("// Copyright 2026"));
    }

    #[test]
    fn batch_replace_all_only_unchanged() {
        // replace_all with no other ops works identically to current behavior
        let f = temp_file("hello world\nfoo bar\nhello again\n");
        let (sid, mgr, reg, coord) = open_session(&[&f]);

        let req = make_request(
            "batch",
            serde_json::json!({
                "session_id": sid,
                "ops": [
                    {
                        "method": "file.str_replace",
                        "path": f.path(),
                        "old_str": "hello",
                        "new_str": "goodbye",
                        "replace_all": true
                    }
                ]
            }),
        );
        let resp = dispatch(req, &mgr, &reg, &coord);
        let results = result_ok(&resp);
        let arr = results.as_array().unwrap();
        assert_eq!(arr[0]["match_count"].as_u64().unwrap(), 2);

        let flush_req = make_request(
            "session.flush",
            serde_json::json!({"session_id": sid}),
        );
        dispatch(flush_req, &mgr, &reg, &coord);

        let content = std::fs::read_to_string(f.path()).unwrap();
        assert!(!content.contains("hello"));
        assert!(content.contains("goodbye world"));
        assert!(content.contains("goodbye again"));
    }

    #[test]
    fn batch_multiple_replace_all_same_file() {
        // Two replace_all ops on same file, both applied correctly
        let f = temp_file("aaa bbb\nccc aaa\nbbb ddd\n");
        let (sid, mgr, reg, coord) = open_session(&[&f]);

        let req = make_request(
            "batch",
            serde_json::json!({
                "session_id": sid,
                "ops": [
                    {
                        "method": "file.str_replace",
                        "path": f.path(),
                        "old_str": "aaa",
                        "new_str": "AAA",
                        "replace_all": true
                    },
                    {
                        "method": "file.str_replace",
                        "path": f.path(),
                        "old_str": "bbb",
                        "new_str": "BBB",
                        "replace_all": true
                    }
                ]
            }),
        );
        let resp = dispatch(req, &mgr, &reg, &coord);
        let results = result_ok(&resp);
        let arr = results.as_array().unwrap();
        assert_eq!(arr[0]["match_count"].as_u64().unwrap(), 2);
        assert_eq!(arr[1]["match_count"].as_u64().unwrap(), 2);

        let flush_req = make_request(
            "session.flush",
            serde_json::json!({"session_id": sid}),
        );
        dispatch(flush_req, &mgr, &reg, &coord);

        let content = std::fs::read_to_string(f.path()).unwrap();
        assert_eq!(content, "AAA BBB\nccc AAA\nBBB ddd\n");
    }

    #[test]
    fn batch_replace_all_with_write_op() {
        // Write op + replace_all, verify replace_all sees written content
        let f = temp_file("line0\nline1\nline2\n");
        let (sid, mgr, reg, coord) = open_session(&[&f]);

        let req = make_request(
            "batch",
            serde_json::json!({
                "session_id": sid,
                "ops": [
                    {
                        "method": "file.write",
                        "path": f.path(),
                        "start": 1,
                        "end": 2,
                        "content": ["new_line1 with target_word"]
                    },
                    {
                        "method": "file.str_replace",
                        "path": f.path(),
                        "old_str": "target_word",
                        "new_str": "REPLACED",
                        "replace_all": true
                    }
                ]
            }),
        );
        let resp = dispatch(req, &mgr, &reg, &coord);
        let results = result_ok(&resp);
        let arr = results.as_array().unwrap();
        // replace_all should find the target_word that was introduced by the write
        assert_eq!(arr[1]["match_count"].as_u64().unwrap(), 1);

        let flush_req = make_request(
            "session.flush",
            serde_json::json!({"session_id": sid}),
        );
        dispatch(flush_req, &mgr, &reg, &coord);

        let content = std::fs::read_to_string(f.path()).unwrap();
        assert!(content.contains("REPLACED"));
        assert!(!content.contains("target_word"));
    }
}
