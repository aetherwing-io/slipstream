//! End-to-end tests for the MCP server adapter.
//!
//! Tests validate that SlipstreamServer can be constructed from a Client
//! connected to an in-process daemon, and that the underlying tool logic
//! works correctly through the daemon. Since the #[tool] macro makes tool
//! methods private, we test via the Client (same path as MCP tool calls).

use std::path::PathBuf;
use std::sync::Arc;

use slipstream_core::manager::SessionManager;
use slipstream_daemon::coordinator::Coordinator;
use slipstream_daemon::registry::FormatRegistry;
use slipstream_mcp::server::SlipstreamServer;
use tokio::net::UnixListener;

/// Start an in-process daemon on a temp socket, return the socket path.
fn start_server(mgr: Arc<SessionManager>) -> PathBuf {
    let socket_path = PathBuf::from(format!(
        "/tmp/ss-mcp-{}.sock",
        &uuid::Uuid::new_v4().to_string()[..8]
    ));
    let _ = std::fs::remove_file(&socket_path);

    let listener = UnixListener::bind(&socket_path).unwrap();
    let registry = Arc::new(FormatRegistry::default_registry());
    let coordinator = Arc::new(Coordinator::new());
    tokio::spawn(slipstream_daemon::serve(listener, mgr, registry, coordinator));

    socket_path
}

/// Connect and build a SlipstreamServer + a separate Client for assertions.
async fn setup() -> (
    SlipstreamServer,
    slipstream_cli::client::Client,
    PathBuf,
    tempfile::TempDir,
) {
    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(SessionManager::new());
    let sock = start_server(Arc::clone(&mgr));
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    // One client for the MCP server, one for test assertions
    let client_for_server = slipstream_cli::client::Client::connect(&sock, false)
        .await
        .expect("should connect");
    let client_for_test = slipstream_cli::client::Client::connect(&sock, false)
        .await
        .expect("should connect");

    let server = SlipstreamServer::from_client(client_for_server, &sock);

    (server, client_for_test, sock, dir)
}

/// Verify that SlipstreamServer can be constructed and the daemon is reachable.
#[tokio::test]
async fn server_construction() {
    let (_server, _client, sock, _dir) = setup().await;
    // If we get here, construction succeeded and daemon is reachable.
    let _ = std::fs::remove_file(&sock);
}

/// Full lifecycle via the daemon client (same path MCP tools take).
#[tokio::test]
async fn full_lifecycle_via_client() {
    let (_server, mut client, sock, dir) = setup().await;
    let file = dir.path().join("test.txt");
    std::fs::write(&file, "line0\nline1\nline2\nline3\n").unwrap();

    // Open
    let result = client
        .request(
            "session.open",
            serde_json::json!({ "files": [file.to_str().unwrap()] }),
        )
        .await
        .unwrap();
    let sid = result["session_id"].as_str().unwrap().to_string();
    assert!(!sid.is_empty());

    // Read range
    let result = client
        .request(
            "file.read",
            serde_json::json!({
                "session_id": sid,
                "path": file.to_str().unwrap(),
                "start": 0,
                "end": 2,
            }),
        )
        .await
        .unwrap();
    let lines: Vec<&str> = result["lines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(lines, vec!["line0", "line1"]);
    assert_eq!(result["cursor"].as_u64().unwrap(), 2);

    // Write
    let result = client
        .request(
            "file.write",
            serde_json::json!({
                "session_id": sid,
                "path": file.to_str().unwrap(),
                "start": 1,
                "end": 2,
                "content": ["REPLACED"],
            }),
        )
        .await
        .unwrap();
    assert_eq!(result["edits_pending"].as_u64().unwrap(), 1);

    // Flush
    let result = client
        .request(
            "session.flush",
            serde_json::json!({ "session_id": sid }),
        )
        .await
        .unwrap();
    assert_eq!(result["status"].as_str().unwrap(), "ok");

    // Verify on disk
    let content = std::fs::read_to_string(&file).unwrap();
    assert_eq!(content, "line0\nREPLACED\nline2\nline3\n");

    // Close
    let result = client
        .request(
            "session.close",
            serde_json::json!({ "session_id": sid }),
        )
        .await
        .unwrap();
    assert_eq!(result["status"].as_str().unwrap(), "closed");

    let _ = std::fs::remove_file(&sock);
}

/// Batch operations.
#[tokio::test]
async fn batch_operations() {
    let (_server, mut client, sock, dir) = setup().await;
    let file_a = dir.path().join("a.txt");
    let file_b = dir.path().join("b.txt");
    std::fs::write(&file_a, "alpha\nbeta\n").unwrap();
    std::fs::write(&file_b, "one\ntwo\nthree\n").unwrap();

    let result = client
        .request(
            "session.open",
            serde_json::json!({
                "files": [file_a.to_str().unwrap(), file_b.to_str().unwrap()]
            }),
        )
        .await
        .unwrap();
    let sid = result["session_id"].as_str().unwrap().to_string();

    let result = client
        .request(
            "batch",
            serde_json::json!({
                "session_id": sid,
                "ops": [
                    {"method": "file.read", "path": file_a.to_str().unwrap(), "start": 0, "end": 2},
                    {"method": "file.write", "path": file_b.to_str().unwrap(), "start": 0, "end": 1, "content": ["ONE"]},
                ]
            }),
        )
        .await
        .unwrap();
    let arr = result.as_array().unwrap();
    assert_eq!(arr.len(), 2);

    let lines: Vec<&str> = arr[0]["lines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(lines, vec!["alpha", "beta"]);
    assert_eq!(arr[1]["edits_pending"].as_u64().unwrap(), 1);

    let _ = std::fs::remove_file(&sock);
}

/// Error handling: bad session ID returns an error.
#[tokio::test]
async fn error_bad_session() {
    let (_server, mut client, sock, dir) = setup().await;
    let file = dir.path().join("f.txt");
    std::fs::write(&file, "x\n").unwrap();

    let err = client
        .request(
            "file.read",
            serde_json::json!({
                "session_id": "nonexistent-session",
                "path": file.to_str().unwrap(),
                "start": 0,
                "end": 1,
            }),
        )
        .await
        .unwrap_err();

    match err {
        slipstream_cli::client::ClientError::Rpc { code, .. } => {
            assert_eq!(code, 404);
        }
        other => panic!("expected Rpc error, got: {other}"),
    }

    let _ = std::fs::remove_file(&sock);
}

/// Read variants: range, cursor, full-file.
#[tokio::test]
async fn read_variants() {
    let (_server, mut client, sock, dir) = setup().await;
    let file = dir.path().join("lines.txt");
    std::fs::write(&file, "a\nb\nc\nd\ne\n").unwrap();

    let result = client
        .request(
            "session.open",
            serde_json::json!({ "files": [file.to_str().unwrap()] }),
        )
        .await
        .unwrap();
    let sid = result["session_id"].as_str().unwrap().to_string();

    // Range read
    let result = client
        .request(
            "file.read",
            serde_json::json!({
                "session_id": sid,
                "path": file.to_str().unwrap(),
                "start": 1,
                "end": 3,
            }),
        )
        .await
        .unwrap();
    let lines: Vec<&str> = result["lines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(lines, vec!["b", "c"]);

    // Cursor move + cursor read
    client
        .request(
            "cursor.move",
            serde_json::json!({
                "session_id": sid,
                "path": file.to_str().unwrap(),
                "to": 3,
            }),
        )
        .await
        .unwrap();

    let result = client
        .request(
            "file.read",
            serde_json::json!({
                "session_id": sid,
                "path": file.to_str().unwrap(),
                "count": 2,
            }),
        )
        .await
        .unwrap();
    let lines: Vec<&str> = result["lines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(lines, vec!["d", "e"]);
    assert_eq!(result["cursor"].as_u64().unwrap(), 5);

    // Full-file read
    let result = client
        .request(
            "file.read",
            serde_json::json!({
                "session_id": sid,
                "path": file.to_str().unwrap(),
            }),
        )
        .await
        .unwrap();
    let lines: Vec<&str> = result["lines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(lines, vec!["a", "b", "c", "d", "e"]);

    let _ = std::fs::remove_file(&sock);
}

// --- Coordinator MCP tool tests ---

#[tokio::test]
async fn mcp_new_tools_registered() {
    let (_server, mut client, sock, _dir) = setup().await;

    // coordinator.status
    let result = client
        .request("coordinator.status", serde_json::json!({}))
        .await;
    assert!(result.is_ok(), "status should succeed");

    // coordinator.register
    let result = client
        .request(
            "coordinator.register",
            serde_json::json!({"path": "/some/file.xlsx", "handler": "sheets"}),
        )
        .await
        .unwrap();
    let tid = result["tracking_id"].as_str().unwrap().to_string();

    // coordinator.unregister
    let result = client
        .request(
            "coordinator.unregister",
            serde_json::json!({"tracking_id": tid}),
        )
        .await;
    assert!(result.is_ok(), "unregister should succeed");

    // coordinator.check
    let result = client
        .request(
            "coordinator.check",
            serde_json::json!({"action": "build"}),
        )
        .await;
    assert!(result.is_ok(), "check should succeed");

    let _ = std::fs::remove_file(&sock);
}

#[tokio::test]
async fn mcp_register_and_check() {
    let (_server, mut client, sock, _dir) = setup().await;

    // Register external file
    let result = client
        .request(
            "coordinator.register",
            serde_json::json!({"path": "/tmp/mcp_test.xlsx", "handler": "sheets"}),
        )
        .await
        .unwrap();
    let tid = result["tracking_id"].as_str().unwrap().to_string();

    // Check → should warn about external file
    let result = client
        .request("coordinator.check", serde_json::json!({"action": "build"}))
        .await
        .unwrap();
    let warnings = result["warnings"].as_array().unwrap();
    assert!(!warnings.is_empty());

    // Unregister
    client
        .request(
            "coordinator.unregister",
            serde_json::json!({"tracking_id": tid}),
        )
        .await
        .unwrap();

    // Check again → clean
    let result = client
        .request("coordinator.check", serde_json::json!({"action": "build"}))
        .await
        .unwrap();
    let warnings = result["warnings"].as_array().unwrap();
    assert!(warnings.is_empty());

    let _ = std::fs::remove_file(&sock);
}

#[tokio::test]
async fn mcp_open_returns_digest() {
    let (_server, mut client, sock, dir) = setup().await;
    let file = dir.path().join("digest_test.txt");
    std::fs::write(&file, "hello\n").unwrap();

    let result = client
        .request(
            "session.open",
            serde_json::json!({"files": [file.to_str().unwrap()]}),
        )
        .await
        .unwrap();
    assert!(result.get("session_digest").is_some());
    assert!(result["session_digest"]["total_tracked"].as_u64().unwrap() >= 1);

    let _ = std::fs::remove_file(&sock);
}

#[tokio::test]
async fn mcp_status_returns_all_fields() {
    let (_server, mut client, sock, _dir) = setup().await;

    let result = client
        .request("coordinator.status", serde_json::json!({}))
        .await
        .unwrap();
    assert!(result["tracked_files"].as_array().is_some());
    assert!(result["native_sessions"].as_array().is_some());
    assert!(result["external_registrations"].as_array().is_some());
    assert!(result["warnings"].as_array().is_some());

    let _ = std::fs::remove_file(&sock);
}
