use std::path::Path;
use std::sync::Arc;

use slipstream_core::flush::FlushResult;
use slipstream_core::manager::SessionManager;
use slipstream_core::session::SessionId;

use crate::protocol::{self, Request, Response, RpcError};
use crate::types::*;

/// Dispatch a JSON-RPC request to the appropriate handler.
pub fn dispatch(req: Request, mgr: &Arc<SessionManager>) -> Response {
    match req.method.as_str() {
        "session.open" => handle_session_open(req, mgr),
        "session.flush" => handle_session_flush(req, mgr),
        "session.close" => handle_session_close(req, mgr),
        "file.read" => handle_file_read(req, mgr),
        "file.write" => handle_file_write(req, mgr),
        "file.str_replace" => handle_file_str_replace(req, mgr),
        "cursor.move" => handle_cursor_move(req, mgr),
        "batch" => handle_batch(req, mgr),
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

fn handle_session_open(mut req: Request, mgr: &Arc<SessionManager>) -> Response {
    let params: SessionOpenParams = match parse_params(&mut req) {
        Ok(p) => p,
        Err(resp) => return resp,
    };

    let session_id: SessionId = uuid::Uuid::new_v4().to_string().into();
    let path_refs: Vec<&Path> = params.files.iter().map(|p| p.as_path()).collect();

    if let Err(e) = mgr.create_session(session_id.clone(), &path_refs) {
        return internal_error(req.id, format!("{e}"));
    }

    // Build file info from session
    let files = match mgr.with_session(&session_id, |session| {
        let mut info = std::collections::HashMap::new();
        for (path, handle) in &session.files {
            let line_count = handle.line_count().map_err(|e| {
                slipstream_core::manager::ManagerError::Session(e)
            })?;
            let version = handle.snapshot_version;
            info.insert(path.clone(), FileInfo {
                lines: line_count,
                version,
            });
        }
        Ok(info)
    }) {
        Ok(info) => info,
        Err(e) => return internal_error(req.id, format!("{e}")),
    };

    Response::ok(
        req.id,
        SessionOpenResult {
            session_id: session_id.as_str().to_owned(),
            files,
        },
    )
}

fn handle_session_flush(mut req: Request, mgr: &Arc<SessionManager>) -> Response {
    let params: SessionFlushParams = match parse_params(&mut req) {
        Ok(p) => p,
        Err(resp) => return resp,
    };

    let session_id: SessionId = params.session_id.into();

    match mgr.flush_session(&session_id, params.force) {
        Ok(FlushResult::Ok { files_written }) => {
            let files: Vec<FileWrittenInfo> = files_written
                .into_iter()
                .map(|f| FileWrittenInfo {
                    path: f.path,
                    edits_applied: f.edits_applied,
                })
                .collect();
            Response::ok(
                req.id,
                SessionFlushResult {
                    status: "ok".into(),
                    files_written: files,
                },
            )
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
                    data: Some(serde_json::to_value(data).unwrap_or_default()),
                },
            )
        }
        Err(e) => match_manager_error(req.id, session_id.as_str(), e),
    }
}

fn handle_session_close(mut req: Request, mgr: &Arc<SessionManager>) -> Response {
    let params: SessionCloseParams = match parse_params(&mut req) {
        Ok(p) => p,
        Err(resp) => return resp,
    };

    let session_id: SessionId = params.session_id.into();

    match mgr.close_session(&session_id) {
        Ok(()) => Response::ok(req.id, serde_json::json!({"status": "closed"})),
        Err(e) => match_manager_error(req.id, session_id.as_str(), e),
    }
}

fn handle_file_read(mut req: Request, mgr: &Arc<SessionManager>) -> Response {
    let params: FileReadParams = match parse_params(&mut req) {
        Ok(p) => p,
        Err(resp) => return resp,
    };

    let session_id: SessionId = params.session_id.into();

    let result = mgr.with_session_mut(&session_id, |session| {
        let (lines, cursor) = if let (Some(start), Some(end)) = (params.start, params.end) {
            let lines = session.read(&params.path, start, end)?;
            // Cursor after range read = end of range
            (lines, end)
        } else if let Some(count) = params.count {
            session.read_next(&params.path, count)?
        } else {
            // Default: read entire file
            let handle = session.file(&params.path)?;
            let count = handle.line_count()?;
            let lines = handle.read_range(0, count)?;
            (lines, count)
        };
        Ok((lines, cursor))
    });

    match result {
        Ok((lines, cursor)) => {
            // Get other sessions' dirty ranges
            let canonical = mgr.pool().canonicalize(&params.path);
            let other_sessions = if let Ok(canonical) = canonical {
                mgr.other_sessions_info(&session_id, &canonical)
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

            Response::ok(
                req.id,
                FileReadResult {
                    lines,
                    cursor,
                    other_sessions,
                },
            )
        }
        Err(e) => match_manager_error(req.id, session_id.as_str(), e),
    }
}

fn handle_file_write(mut req: Request, mgr: &Arc<SessionManager>) -> Response {
    let params: FileWriteParams = match parse_params(&mut req) {
        Ok(p) => p,
        Err(resp) => return resp,
    };

    let session_id: SessionId = params.session_id.into();

    match mgr.with_session_mut(&session_id, |session| {
        let count = session.write(&params.path, params.start, params.end, params.content)?;
        Ok(count)
    }) {
        Ok(edits_pending) => Response::ok(req.id, FileWriteResult { edits_pending }),
        Err(e) => match_manager_error(req.id, session_id.as_str(), e),
    }
}

fn handle_file_str_replace(mut req: Request, mgr: &Arc<SessionManager>) -> Response {
    let params: FileStrReplaceParams = match parse_params(&mut req) {
        Ok(p) => p,
        Err(resp) => return resp,
    };

    let session_id: SessionId = params.session_id.into();

    match mgr.with_session_mut(&session_id, |session| {
        let (match_line, match_count, edits_pending) =
            session.str_replace(&params.path, &params.old_str, &params.new_str, params.replace_all)?;
        Ok((match_line, match_count, edits_pending))
    }) {
        Ok((match_line, match_count, edits_pending)) => Response::ok(
            req.id,
            FileStrReplaceResult { edits_pending, match_line, match_count },
        ),
        Err(e) => match_manager_error(req.id, session_id.as_str(), e),
    }
}

fn handle_cursor_move(mut req: Request, mgr: &Arc<SessionManager>) -> Response {
    let params: CursorMoveParams = match parse_params(&mut req) {
        Ok(p) => p,
        Err(resp) => return resp,
    };

    let session_id: SessionId = params.session_id.into();

    match mgr.with_session_mut(&session_id, |session| {
        session.move_cursor(&params.path, params.to)?;
        Ok(())
    }) {
        Ok(()) => Response::ok(req.id, serde_json::json!({"status": "ok"})),
        Err(e) => match_manager_error(req.id, session_id.as_str(), e),
    }
}

fn handle_batch(mut req: Request, mgr: &Arc<SessionManager>) -> Response {
    let params: BatchParams = match parse_params(&mut req) {
        Ok(p) => p,
        Err(resp) => return resp,
    };

    let session_id: SessionId = params.session_id.into();

    let results: Result<Vec<serde_json::Value>, _> =
        mgr.with_session_mut(&session_id, |session| {
            let mut results = Vec::with_capacity(params.ops.len());

            for op in params.ops {
                let value = match op {
                    BatchOp::Read {
                        path,
                        start,
                        end,
                        count,
                    } => {
                        let (lines, cursor) =
                            if let (Some(start), Some(end)) = (start, end) {
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
                        serde_json::json!({"lines": lines, "cursor": cursor})
                    }
                    BatchOp::Write {
                        path,
                        start,
                        end,
                        content,
                    } => {
                        let count = session.write(&path, start, end, content)?;
                        serde_json::json!({"edits_pending": count})
                    }
                    BatchOp::StrReplace {
                        path,
                        old_str,
                        new_str,
                        replace_all,
                    } => {
                        let (match_line, match_count, edits_pending) =
                            session.str_replace(&path, &old_str, &new_str, replace_all)?;
                        serde_json::json!({"edits_pending": edits_pending, "match_line": match_line, "match_count": match_count})
                    }
                    BatchOp::CursorMove { path, to } => {
                        session.move_cursor(&path, to)?;
                        serde_json::json!({"status": "ok"})
                    }
                };
                results.push(value);
            }

            Ok(results)
        });

    match results {
        Ok(values) => Response::ok(req.id, values),
        Err(e) => match_manager_error(req.id, session_id.as_str(), e),
    }
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

    /// Open a session with the given files and return (session_id, mgr).
    fn open_session(files: &[&NamedTempFile]) -> (String, Arc<SessionManager>) {
        let mgr = Arc::new(SessionManager::new());
        let paths: Vec<serde_json::Value> = files
            .iter()
            .map(|f| serde_json::Value::String(f.path().to_str().unwrap().to_string()))
            .collect();

        let req = make_request("session.open", serde_json::json!({ "files": paths }));
        let resp = dispatch(req, &mgr);
        let result = resp.result.expect("session.open should succeed");
        let session_id = result["session_id"].as_str().unwrap().to_string();
        (session_id, mgr)
    }

    fn result_ok(resp: &Response) -> &serde_json::Value {
        assert!(resp.error.is_none(), "expected ok, got error: {:?}", resp.error);
        resp.result.as_ref().unwrap()
    }

    #[test]
    fn batch_multiple_reads() {
        let f1 = temp_file("alpha\nbeta\ngamma\n");
        let f2 = temp_file("one\ntwo\nthree\n");
        let (sid, mgr) = open_session(&[&f1, &f2]);

        let req = make_request("batch", serde_json::json!({
            "session_id": sid,
            "ops": [
                {"method": "file.read", "path": f1.path(), "start": 0, "end": 2},
                {"method": "file.read", "path": f2.path(), "start": 1, "end": 3},
            ]
        }));
        let resp = dispatch(req, &mgr);
        let results = result_ok(&resp);
        let arr = results.as_array().unwrap();
        assert_eq!(arr.len(), 2);

        let lines1: Vec<&str> = arr[0]["lines"].as_array().unwrap()
            .iter().map(|v| v.as_str().unwrap()).collect();
        assert_eq!(lines1, vec!["alpha", "beta"]);

        let lines2: Vec<&str> = arr[1]["lines"].as_array().unwrap()
            .iter().map(|v| v.as_str().unwrap()).collect();
        assert_eq!(lines2, vec!["two", "three"]);
    }

    #[test]
    fn batch_read_and_write() {
        let f1 = temp_file("line0\nline1\nline2\n");
        let (sid, mgr) = open_session(&[&f1]);

        let req = make_request("batch", serde_json::json!({
            "session_id": sid,
            "ops": [
                {"method": "file.read", "path": f1.path(), "start": 0, "end": 1},
                {"method": "file.write", "path": f1.path(), "start": 1, "end": 2, "content": ["REPLACED"]},
            ]
        }));
        let resp = dispatch(req, &mgr);
        let arr = result_ok(&resp).as_array().unwrap();
        assert_eq!(arr.len(), 2);

        // First op: read returns line0
        let lines: Vec<&str> = arr[0]["lines"].as_array().unwrap()
            .iter().map(|v| v.as_str().unwrap()).collect();
        assert_eq!(lines, vec!["line0"]);

        // Second op: write queued
        assert_eq!(arr[1]["edits_pending"].as_u64().unwrap(), 1);
    }

    #[test]
    fn batch_write_then_read_returns_original() {
        // Writes are queued, not applied until flush. A subsequent read in the
        // same batch should still see the original content.
        let f1 = temp_file("aaa\nbbb\nccc\n");
        let (sid, mgr) = open_session(&[&f1]);

        let req = make_request("batch", serde_json::json!({
            "session_id": sid,
            "ops": [
                {"method": "file.write", "path": f1.path(), "start": 0, "end": 1, "content": ["XXX"]},
                {"method": "file.read", "path": f1.path(), "start": 0, "end": 1},
            ]
        }));
        let resp = dispatch(req, &mgr);
        let arr = result_ok(&resp).as_array().unwrap();

        // Write result
        assert_eq!(arr[0]["edits_pending"].as_u64().unwrap(), 1);
        // Read still returns original content (edits not applied yet)
        let lines: Vec<&str> = arr[1]["lines"].as_array().unwrap()
            .iter().map(|v| v.as_str().unwrap()).collect();
        assert_eq!(lines, vec!["aaa"]);
    }

    #[test]
    fn batch_cursor_move() {
        let f1 = temp_file("a\nb\nc\nd\ne\n");
        let (sid, mgr) = open_session(&[&f1]);

        let req = make_request("batch", serde_json::json!({
            "session_id": sid,
            "ops": [
                {"method": "cursor.move", "path": f1.path(), "to": 2},
                {"method": "file.read", "path": f1.path(), "count": 2},
            ]
        }));
        let resp = dispatch(req, &mgr);
        let arr = result_ok(&resp).as_array().unwrap();
        assert_eq!(arr.len(), 2);

        // cursor.move result
        assert_eq!(arr[0]["status"].as_str().unwrap(), "ok");
        // read from cursor=2, count=2 -> lines c, d
        let lines: Vec<&str> = arr[1]["lines"].as_array().unwrap()
            .iter().map(|v| v.as_str().unwrap()).collect();
        assert_eq!(lines, vec!["c", "d"]);
        assert_eq!(arr[1]["cursor"].as_u64().unwrap(), 4);
    }

    #[test]
    fn batch_error_propagates() {
        let f1 = temp_file("hello\n");
        let (sid, mgr) = open_session(&[&f1]);

        // Second op references a file not in the session -> batch should error
        let req = make_request("batch", serde_json::json!({
            "session_id": sid,
            "ops": [
                {"method": "file.read", "path": f1.path(), "start": 0, "end": 1},
                {"method": "file.read", "path": "/nonexistent/file.txt", "start": 0, "end": 1},
            ]
        }));
        let resp = dispatch(req, &mgr);
        assert!(resp.error.is_some(), "batch should fail when an op errors");
    }

    #[test]
    fn batch_empty_ops() {
        let f1 = temp_file("x\n");
        let (sid, mgr) = open_session(&[&f1]);

        let req = make_request("batch", serde_json::json!({
            "session_id": sid,
            "ops": []
        }));
        let resp = dispatch(req, &mgr);
        let arr = result_ok(&resp).as_array().unwrap();
        assert!(arr.is_empty());
    }

    #[test]
    fn read_includes_other_sessions() {
        let f1 = temp_file("line0\nline1\nline2\nline3\n");
        let mgr = Arc::new(SessionManager::new());

        // Open session A
        let open_a = make_request("session.open", serde_json::json!({
            "files": [f1.path()]
        }));
        let resp_a = dispatch(open_a, &mgr);
        let sid_a = result_ok(&resp_a)["session_id"].as_str().unwrap().to_string();

        // Open session B
        let open_b = make_request("session.open", serde_json::json!({
            "files": [f1.path()]
        }));
        let resp_b = dispatch(open_b, &mgr);
        let sid_b = result_ok(&resp_b)["session_id"].as_str().unwrap().to_string();

        // Session A writes to lines 1-2
        let write_req = make_request("file.write", serde_json::json!({
            "session_id": sid_a,
            "path": f1.path(),
            "start": 1, "end": 2,
            "content": ["CHANGED"]
        }));
        dispatch(write_req, &mgr);

        // Session B reads -> should see session A in other_sessions
        let read_req = make_request("file.read", serde_json::json!({
            "session_id": sid_b,
            "path": f1.path(),
            "start": 0, "end": 4
        }));
        let resp = dispatch(read_req, &mgr);
        let result = result_ok(&resp);

        let other = result["other_sessions"].as_array().unwrap();
        assert_eq!(other.len(), 1);
        assert_eq!(other[0]["session"].as_str().unwrap(), sid_a);

        let ranges = other[0]["dirty_ranges"].as_array().unwrap();
        assert!(!ranges.is_empty(), "session A should have dirty ranges");
    }

    #[test]
    fn read_no_other_sessions_when_alone() {
        let f1 = temp_file("solo\n");
        let (sid, mgr) = open_session(&[&f1]);

        let req = make_request("file.read", serde_json::json!({
            "session_id": sid,
            "path": f1.path(),
            "start": 0, "end": 1
        }));
        let resp = dispatch(req, &mgr);
        let result = result_ok(&resp);

        // other_sessions should be absent (skip_serializing_if = empty) or empty
        let other = result.get("other_sessions");
        assert!(
            other.is_none() || other.unwrap().as_array().unwrap().is_empty(),
            "should have no other_sessions when alone"
        );
    }
}
