//! End-to-end tests: full JSON-RPC protocol over Unix socket.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use slipstream_core::manager::SessionManager;
use slipstream_daemon::coordinator::Coordinator;
use slipstream_daemon::registry::FormatRegistry;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

/// A simple JSON-RPC test client over Unix socket.
struct TestClient {
    reader: tokio::io::Lines<BufReader<tokio::net::unix::OwnedReadHalf>>,
    writer: tokio::net::unix::OwnedWriteHalf,
    next_id: u64,
}

impl TestClient {
    async fn connect(path: &Path) -> Self {
        let stream = UnixStream::connect(path).await.unwrap();
        let (read_half, write_half) = stream.into_split();
        TestClient {
            reader: BufReader::new(read_half).lines(),
            writer: write_half,
            next_id: 1,
        }
    }

    async fn request(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> serde_json::Value {
        let id = self.next_id;
        self.next_id += 1;

        let req = serde_json::json!({
            "id": id,
            "method": method,
            "params": params,
        });

        let mut bytes = serde_json::to_vec(&req).unwrap();
        bytes.push(b'\n');
        self.writer.write_all(&bytes).await.unwrap();

        let line = self.reader.next_line().await.unwrap().unwrap();
        serde_json::from_str(&line).unwrap()
    }

    async fn open_session(&mut self, files: &[&Path]) -> String {
        let paths: Vec<&str> = files.iter().map(|p| p.to_str().unwrap()).collect();
        let resp = self.request("session.open", serde_json::json!({ "files": paths })).await;
        resp["result"]["session_id"].as_str().unwrap().to_string()
    }
}

/// Start server on a temp socket, return the socket path.
/// The server runs in a background tokio task.
fn start_server(mgr: Arc<SessionManager>) -> PathBuf {
    // Use /tmp with a short name to stay under macOS SUN_LEN (~104 bytes)
    let socket_path = PathBuf::from(format!(
        "/tmp/ss-{}.sock",
        &uuid::Uuid::new_v4().to_string()[..8]
    ));
    let _ = std::fs::remove_file(&socket_path);

    let listener = tokio::net::UnixListener::bind(&socket_path).unwrap();
    let registry = Arc::new(FormatRegistry::default_registry());
    let coordinator = Arc::new(Coordinator::new());
    tokio::spawn(slipstream_daemon::serve(listener, mgr, registry, coordinator));

    socket_path
}

#[tokio::test]
async fn e2e_batch_lifecycle() {
    // Setup: two temp files
    let dir = tempfile::tempdir().unwrap();
    let file_a = dir.path().join("a.txt");
    let file_b = dir.path().join("b.txt");
    std::fs::write(&file_a, "alpha\nbeta\ngamma\n").unwrap();
    std::fs::write(&file_b, "one\ntwo\nthree\n").unwrap();

    let mgr = Arc::new(SessionManager::new());
    let sock = start_server(Arc::clone(&mgr));
    // Give server a moment to bind
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    let mut client = TestClient::connect(&sock).await;

    // Open session with both files
    let sid = client.open_session(&[&file_a, &file_b]).await;

    // Batch: read file_a lines 0-2, write file_b lines 1-2
    let resp = client.request("batch", serde_json::json!({
        "session_id": sid,
        "ops": [
            {"method": "file.read", "path": file_a.to_str().unwrap(), "start": 0, "end": 2},
            {"method": "file.write", "path": file_b.to_str().unwrap(), "start": 1, "end": 2, "content": ["TWO_REPLACED"]},
        ]
    })).await;

    let results = resp["result"].as_array().unwrap();
    assert_eq!(results.len(), 2);

    // Verify read result
    let lines: Vec<&str> = results[0]["lines"].as_array().unwrap()
        .iter().map(|v| v.as_str().unwrap()).collect();
    assert_eq!(lines, vec!["alpha", "beta"]);

    // Verify write result
    assert_eq!(results[1]["edits_pending"].as_u64().unwrap(), 1);

    // Flush
    let flush_resp = client.request("session.flush", serde_json::json!({
        "session_id": sid,
    })).await;
    assert_eq!(flush_resp["result"]["status"].as_str().unwrap(), "ok");

    // Verify file_b on disk
    let content = std::fs::read_to_string(&file_b).unwrap();
    assert_eq!(content, "one\nTWO_REPLACED\nthree\n");

    // Close
    let close_resp = client.request("session.close", serde_json::json!({
        "session_id": sid,
    })).await;
    assert_eq!(close_resp["result"]["status"].as_str().unwrap(), "closed");

    // Cleanup
    let _ = std::fs::remove_file(&sock);
}

#[tokio::test]
async fn e2e_concurrent_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("shared.txt");
    std::fs::write(&file, "line0\nline1\nline2\nline3\n").unwrap();

    let mgr = Arc::new(SessionManager::new());
    let sock = start_server(Arc::clone(&mgr));
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    let mut client_a = TestClient::connect(&sock).await;
    let mut client_b = TestClient::connect(&sock).await;

    // Both open session on same file
    let sid_a = client_a.open_session(&[&file]).await;
    let sid_b = client_b.open_session(&[&file]).await;

    // Client A writes to lines 1-2
    client_a.request("file.write", serde_json::json!({
        "session_id": sid_a,
        "path": file.to_str().unwrap(),
        "start": 1, "end": 2,
        "content": ["MODIFIED"]
    })).await;

    // Client B reads → should see other_sessions with A's dirty ranges
    let read_resp = client_b.request("file.read", serde_json::json!({
        "session_id": sid_b,
        "path": file.to_str().unwrap(),
        "start": 0, "end": 4
    })).await;

    let result = &read_resp["result"];
    let other = result["other_sessions"].as_array().unwrap();
    assert_eq!(other.len(), 1, "should see one other session");
    assert_eq!(other[0]["session"].as_str().unwrap(), sid_a);

    let ranges = other[0]["dirty_ranges"].as_array().unwrap();
    assert!(!ranges.is_empty(), "session A should have dirty ranges");

    // Cleanup
    let _ = std::fs::remove_file(&sock);
}

#[tokio::test]
async fn e2e_batch_mixed_ops() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("mixed.txt");
    std::fs::write(&file, "a\nb\nc\nd\ne\n").unwrap();

    let mgr = Arc::new(SessionManager::new());
    let sock = start_server(Arc::clone(&mgr));
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    let mut client = TestClient::connect(&sock).await;
    let sid = client.open_session(&[&file]).await;

    // Batch: write, cursor.move, read-from-cursor
    let resp = client.request("batch", serde_json::json!({
        "session_id": sid,
        "ops": [
            {"method": "file.write", "path": file.to_str().unwrap(), "start": 0, "end": 1, "content": ["A"]},
            {"method": "cursor.move", "path": file.to_str().unwrap(), "to": 3},
            {"method": "file.read", "path": file.to_str().unwrap(), "count": 2},
        ]
    })).await;

    let results = resp["result"].as_array().unwrap();
    assert_eq!(results.len(), 3);

    // Write result
    assert_eq!(results[0]["edits_pending"].as_u64().unwrap(), 1);
    // Cursor move result
    assert_eq!(results[1]["status"].as_str().unwrap(), "ok");
    // Read from cursor=3, count=2 → lines d, e
    let lines: Vec<&str> = results[2]["lines"].as_array().unwrap()
        .iter().map(|v| v.as_str().unwrap()).collect();
    assert_eq!(lines, vec!["d", "e"]);
    assert_eq!(results[2]["cursor"].as_u64().unwrap(), 5);

    // Cleanup
    let _ = std::fs::remove_file(&sock);
}

// --- Format-aware open + coordinator E2E tests ---

/// Start server and return (socket_path, coordinator) for tests that need coordinator access.
fn start_server_with_coord(
    mgr: Arc<SessionManager>,
) -> (PathBuf, Arc<Coordinator>) {
    let socket_path = PathBuf::from(format!(
        "/tmp/ss-{}.sock",
        &uuid::Uuid::new_v4().to_string()[..8]
    ));
    let _ = std::fs::remove_file(&socket_path);

    let listener = tokio::net::UnixListener::bind(&socket_path).unwrap();
    let registry = Arc::new(FormatRegistry::default_registry());
    let coordinator = Arc::new(Coordinator::new());
    let coord_ref = Arc::clone(&coordinator);
    tokio::spawn(slipstream_daemon::serve(listener, mgr, registry, coordinator));

    (socket_path, coord_ref)
}

#[tokio::test]
async fn e2e_format_aware_open_text() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("hello.rs");
    std::fs::write(&file, "fn main() {}\n").unwrap();

    let mgr = Arc::new(SessionManager::new());
    let (sock, _coord) = start_server_with_coord(Arc::clone(&mgr));
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    let mut client = TestClient::connect(&sock).await;
    let resp = client
        .request("session.open", serde_json::json!({"files": [file.to_str().unwrap()]}))
        .await;

    let result = &resp["result"];
    assert!(result["session_id"].as_str().is_some());
    assert!(result.get("session_digest").is_some());
    assert_eq!(result["session_digest"]["native_count"], 1);

    let _ = std::fs::remove_file(&sock);
}

#[tokio::test]
async fn e2e_format_aware_open_external() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("report.xlsx");
    std::fs::write(&file, "").unwrap();

    let mgr = Arc::new(SessionManager::new());
    let (sock, _coord) = start_server_with_coord(Arc::clone(&mgr));
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    let mut client = TestClient::connect(&sock).await;
    let resp = client
        .request("session.open", serde_json::json!({"files": [file.to_str().unwrap()]}))
        .await;

    let result = &resp["result"];
    let arr = result.as_array().unwrap();
    assert_eq!(arr[0]["status"], "external_handler");
    assert_eq!(arr[0]["handler"], "sheets");
    assert!(arr[0]["tracking_id"].as_str().unwrap().starts_with("ext-"));
    assert!(arr[0]["instructions"]["open"]
        .as_str()
        .unwrap()
        .contains(&file.display().to_string()));

    let _ = std::fs::remove_file(&sock);
}

#[tokio::test]
async fn e2e_format_aware_open_advisory() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("Makefile");
    std::fs::write(&file, "all:\n\techo hi\n").unwrap();

    let mgr = Arc::new(SessionManager::new());
    let (sock, _coord) = start_server_with_coord(Arc::clone(&mgr));
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    let mut client = TestClient::connect(&sock).await;
    let resp = client
        .request("session.open", serde_json::json!({"files": [file.to_str().unwrap()]}))
        .await;

    let result = &resp["result"];
    assert_eq!(result["status"], "advisory");
    assert_eq!(result["loaded_as_text"], true);
    assert!(result["guidance"]
        .as_str()
        .unwrap()
        .to_lowercase()
        .contains("make"));

    let sid = result["session_id"].as_str().unwrap();
    let read_resp = client
        .request(
            "file.read",
            serde_json::json!({
                "session_id": sid,
                "path": file.to_str().unwrap(),
            }),
        )
        .await;
    let lines = read_resp["result"]["lines"].as_array().unwrap();
    assert!(!lines.is_empty());

    let _ = std::fs::remove_file(&sock);
}

#[tokio::test]
async fn e2e_coordinator_register_and_status() {
    let mgr = Arc::new(SessionManager::new());
    let (sock, _coord) = start_server_with_coord(Arc::clone(&mgr));
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    let mut client = TestClient::connect(&sock).await;

    let resp = client
        .request(
            "coordinator.register",
            serde_json::json!({"path": "/some/fake.xlsx", "handler": "sheets"}),
        )
        .await;
    let result = &resp["result"];
    assert!(result["tracking_id"].as_str().is_some());

    let status_resp = client
        .request("coordinator.status", serde_json::json!({}))
        .await;
    let ext = status_resp["result"]["external_registrations"]
        .as_array()
        .unwrap();
    assert_eq!(ext.len(), 1);
    assert_eq!(ext[0]["handler"], "sheets");

    let _ = std::fs::remove_file(&sock);
}

#[tokio::test]
async fn e2e_coordinator_check_dirty() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("dirty.txt");
    std::fs::write(&file, "data\n").unwrap();

    let mgr = Arc::new(SessionManager::new());
    let (sock, _coord) = start_server_with_coord(Arc::clone(&mgr));
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    let mut client = TestClient::connect(&sock).await;
    let sid = client.open_session(&[&file]).await;

    client
        .request(
            "file.write",
            serde_json::json!({
                "session_id": sid,
                "path": file.to_str().unwrap(),
                "start": 0, "end": 1,
                "content": ["modified"]
            }),
        )
        .await;

    let resp = client
        .request("coordinator.check", serde_json::json!({"action": "build"}))
        .await;
    let result = &resp["result"];
    let warnings = result["warnings"].as_array().unwrap();
    assert!(!warnings.is_empty());
    assert!(result["suggestion"]
        .as_str()
        .unwrap()
        .to_lowercase()
        .contains("flush"));

    let _ = std::fs::remove_file(&sock);
}

#[tokio::test]
async fn e2e_coordinator_check_clean_after_flush() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("clean.txt");
    std::fs::write(&file, "data\n").unwrap();

    let mgr = Arc::new(SessionManager::new());
    let (sock, _coord) = start_server_with_coord(Arc::clone(&mgr));
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    let mut client = TestClient::connect(&sock).await;
    let sid = client.open_session(&[&file]).await;

    client
        .request(
            "file.write",
            serde_json::json!({
                "session_id": sid,
                "path": file.to_str().unwrap(),
                "start": 0, "end": 1,
                "content": ["changed"]
            }),
        )
        .await;

    client
        .request("session.flush", serde_json::json!({"session_id": sid}))
        .await;

    let resp = client
        .request("coordinator.check", serde_json::json!({"action": "build"}))
        .await;
    let result = &resp["result"];
    let warnings = result["warnings"].as_array().unwrap();
    assert!(warnings.is_empty());
    assert_eq!(result["suggestion"], "Ready to build");

    let _ = std::fs::remove_file(&sock);
}

#[tokio::test]
async fn e2e_digest_on_write_response() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("wr.txt");
    std::fs::write(&file, "line\n").unwrap();

    let mgr = Arc::new(SessionManager::new());
    let (sock, _coord) = start_server_with_coord(Arc::clone(&mgr));
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    let mut client = TestClient::connect(&sock).await;
    let sid = client.open_session(&[&file]).await;

    let resp = client
        .request(
            "file.write",
            serde_json::json!({
                "session_id": sid,
                "path": file.to_str().unwrap(),
                "start": 0, "end": 1,
                "content": ["new"]
            }),
        )
        .await;
    let result = &resp["result"];
    assert!(result.get("session_digest").is_some());
    assert_eq!(result["session_digest"]["native_dirty"], 1);

    let _ = std::fs::remove_file(&sock);
}

#[tokio::test]
async fn e2e_digest_not_on_read_response() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("rd.txt");
    std::fs::write(&file, "content\n").unwrap();

    let mgr = Arc::new(SessionManager::new());
    let (sock, _coord) = start_server_with_coord(Arc::clone(&mgr));
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    let mut client = TestClient::connect(&sock).await;
    let sid = client.open_session(&[&file]).await;

    let resp = client
        .request(
            "file.read",
            serde_json::json!({
                "session_id": sid,
                "path": file.to_str().unwrap(),
                "start": 0, "end": 1
            }),
        )
        .await;
    let result = &resp["result"];
    assert!(result.get("session_digest").is_none());

    let _ = std::fs::remove_file(&sock);
}

#[tokio::test]
async fn e2e_coordinator_unregister() {
    let mgr = Arc::new(SessionManager::new());
    let (sock, _coord) = start_server_with_coord(Arc::clone(&mgr));
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    let mut client = TestClient::connect(&sock).await;

    let resp = client
        .request(
            "coordinator.register",
            serde_json::json!({"path": "/tmp/unreg.xlsx", "handler": "sheets"}),
        )
        .await;
    let tid = resp["result"]["tracking_id"].as_str().unwrap().to_string();

    client
        .request(
            "coordinator.unregister",
            serde_json::json!({"tracking_id": tid}),
        )
        .await;

    let status_resp = client
        .request("coordinator.status", serde_json::json!({}))
        .await;
    let ext = status_resp["result"]["external_registrations"]
        .as_array()
        .unwrap();
    assert!(ext.is_empty());

    let _ = std::fs::remove_file(&sock);
}

#[tokio::test]
async fn e2e_full_coordinator_workflow() {
    let dir = tempfile::tempdir().unwrap();
    let py_file = dir.path().join("app.py");
    std::fs::write(&py_file, "x = 1\n").unwrap();

    let mgr = Arc::new(SessionManager::new());
    let (sock, _coord) = start_server_with_coord(Arc::clone(&mgr));
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    let mut client = TestClient::connect(&sock).await;

    // 1. Open native text file
    let sid = client.open_session(&[&py_file]).await;

    // 2. Register external xlsx
    let reg_resp = client
        .request(
            "coordinator.register",
            serde_json::json!({"path": "/tmp/workflow.xlsx", "handler": "sheets"}),
        )
        .await;
    let tid = reg_resp["result"]["tracking_id"]
        .as_str()
        .unwrap()
        .to_string();

    // 3. Write to native file (makes it dirty)
    client
        .request(
            "file.write",
            serde_json::json!({
                "session_id": sid,
                "path": py_file.to_str().unwrap(),
                "start": 0, "end": 1,
                "content": ["x = 2"]
            }),
        )
        .await;

    // 4. Check build → expect 2 warnings (dirty native + external)
    let check_resp = client
        .request("coordinator.check", serde_json::json!({"action": "build"}))
        .await;
    let warnings = check_resp["result"]["warnings"].as_array().unwrap();
    assert_eq!(warnings.len(), 2, "expected 2 warnings: {warnings:?}");

    // 5. Flush native session
    client
        .request("session.flush", serde_json::json!({"session_id": sid}))
        .await;

    // 6. Unregister external
    client
        .request(
            "coordinator.unregister",
            serde_json::json!({"tracking_id": tid}),
        )
        .await;

    // 7. Check build again → clean
    let check_resp = client
        .request("coordinator.check", serde_json::json!({"action": "build"}))
        .await;
    let warnings = check_resp["result"]["warnings"].as_array().unwrap();
    assert!(warnings.is_empty());
    assert_eq!(check_resp["result"]["suggestion"], "Ready to build");

    let _ = std::fs::remove_file(&sock);
}
