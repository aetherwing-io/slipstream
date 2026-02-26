//! End-to-end tests: full JSON-RPC protocol over Unix socket.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use slipstream_core::manager::SessionManager;
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
    tokio::spawn(slipstream_daemon::serve(listener, mgr));

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
