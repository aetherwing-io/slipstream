//! End-to-end tests for the CLI client against an in-process daemon.

use std::path::PathBuf;
use std::sync::Arc;

use slipstream_core::manager::SessionManager;
use slipstream_daemon;
use tokio::net::UnixListener;

/// Start an in-process daemon on a temp socket, return the socket path.
fn start_server(mgr: Arc<SessionManager>) -> PathBuf {
    let socket_path = PathBuf::from(format!(
        "/tmp/ss-cli-{}.sock",
        &uuid::Uuid::new_v4().to_string()[..8]
    ));
    let _ = std::fs::remove_file(&socket_path);

    let listener = UnixListener::bind(&socket_path).unwrap();
    tokio::spawn(slipstream_daemon::serve(listener, mgr));

    socket_path
}

/// Connect the CLI client (bypassing auto-start since we run in-process).
async fn connect_client(
    socket_path: &std::path::Path,
) -> slipstream_cli::client::Client {
    slipstream_cli::client::Client::connect(socket_path, false)
        .await
        .expect("should connect to in-process daemon")
}

#[tokio::test]
async fn full_lifecycle() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("test.txt");
    std::fs::write(&file, "line0\nline1\nline2\nline3\n").unwrap();

    let mgr = Arc::new(SessionManager::new());
    let sock = start_server(Arc::clone(&mgr));
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    let mut client = connect_client(&sock).await;

    // Open
    let result = client
        .request("session.open", serde_json::json!({ "files": [file.to_str().unwrap()] }))
        .await
        .unwrap();
    let sid = result["session_id"].as_str().unwrap().to_string();
    assert!(!sid.is_empty());

    // Read range
    let result = client
        .request("file.read", serde_json::json!({
            "session_id": sid,
            "path": file.to_str().unwrap(),
            "start": 0,
            "end": 2,
        }))
        .await
        .unwrap();
    let lines: Vec<&str> = result["lines"]
        .as_array().unwrap()
        .iter().map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(lines, vec!["line0", "line1"]);
    assert_eq!(result["cursor"].as_u64().unwrap(), 2);

    // Write
    let result = client
        .request("file.write", serde_json::json!({
            "session_id": sid,
            "path": file.to_str().unwrap(),
            "start": 1,
            "end": 2,
            "content": ["REPLACED"],
        }))
        .await
        .unwrap();
    assert_eq!(result["edits_pending"].as_u64().unwrap(), 1);

    // Flush
    let result = client
        .request("session.flush", serde_json::json!({
            "session_id": sid,
        }))
        .await
        .unwrap();
    assert_eq!(result["status"].as_str().unwrap(), "ok");

    // Verify on disk
    let content = std::fs::read_to_string(&file).unwrap();
    assert_eq!(content, "line0\nREPLACED\nline2\nline3\n");

    // Close
    let result = client
        .request("session.close", serde_json::json!({
            "session_id": sid,
        }))
        .await
        .unwrap();
    assert_eq!(result["status"].as_str().unwrap(), "closed");

    let _ = std::fs::remove_file(&sock);
}

#[tokio::test]
async fn batch_operations() {
    let dir = tempfile::tempdir().unwrap();
    let file_a = dir.path().join("a.txt");
    let file_b = dir.path().join("b.txt");
    std::fs::write(&file_a, "alpha\nbeta\n").unwrap();
    std::fs::write(&file_b, "one\ntwo\nthree\n").unwrap();

    let mgr = Arc::new(SessionManager::new());
    let sock = start_server(Arc::clone(&mgr));
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    let mut client = connect_client(&sock).await;

    // Open both files
    let result = client
        .request("session.open", serde_json::json!({
            "files": [file_a.to_str().unwrap(), file_b.to_str().unwrap()]
        }))
        .await
        .unwrap();
    let sid = result["session_id"].as_str().unwrap().to_string();

    // Batch: read a, write b
    let result = client
        .request("batch", serde_json::json!({
            "session_id": sid,
            "ops": [
                {"method": "file.read", "path": file_a.to_str().unwrap(), "start": 0, "end": 2},
                {"method": "file.write", "path": file_b.to_str().unwrap(), "start": 0, "end": 1, "content": ["ONE"]},
            ]
        }))
        .await
        .unwrap();

    let arr = result.as_array().unwrap();
    assert_eq!(arr.len(), 2);

    // Read result
    let lines: Vec<&str> = arr[0]["lines"]
        .as_array().unwrap()
        .iter().map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(lines, vec!["alpha", "beta"]);

    // Write result
    assert_eq!(arr[1]["edits_pending"].as_u64().unwrap(), 1);

    let _ = std::fs::remove_file(&sock);
}

#[tokio::test]
async fn error_bad_session() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("f.txt");
    std::fs::write(&file, "x\n").unwrap();

    let mgr = Arc::new(SessionManager::new());
    let sock = start_server(Arc::clone(&mgr));
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    let mut client = connect_client(&sock).await;

    // Request with a nonexistent session ID
    let err = client
        .request("file.read", serde_json::json!({
            "session_id": "nonexistent-session",
            "path": file.to_str().unwrap(),
            "start": 0,
            "end": 1,
        }))
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

#[tokio::test]
async fn read_variants() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("lines.txt");
    std::fs::write(&file, "a\nb\nc\nd\ne\n").unwrap();

    let mgr = Arc::new(SessionManager::new());
    let sock = start_server(Arc::clone(&mgr));
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    let mut client = connect_client(&sock).await;

    let result = client
        .request("session.open", serde_json::json!({ "files": [file.to_str().unwrap()] }))
        .await
        .unwrap();
    let sid = result["session_id"].as_str().unwrap().to_string();

    // Range read
    let result = client
        .request("file.read", serde_json::json!({
            "session_id": sid,
            "path": file.to_str().unwrap(),
            "start": 1,
            "end": 3,
        }))
        .await
        .unwrap();
    let lines: Vec<&str> = result["lines"]
        .as_array().unwrap()
        .iter().map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(lines, vec!["b", "c"]);

    // Cursor move then cursor read
    client
        .request("cursor.move", serde_json::json!({
            "session_id": sid,
            "path": file.to_str().unwrap(),
            "to": 3,
        }))
        .await
        .unwrap();

    let result = client
        .request("file.read", serde_json::json!({
            "session_id": sid,
            "path": file.to_str().unwrap(),
            "count": 2,
        }))
        .await
        .unwrap();
    let lines: Vec<&str> = result["lines"]
        .as_array().unwrap()
        .iter().map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(lines, vec!["d", "e"]);
    assert_eq!(result["cursor"].as_u64().unwrap(), 5);

    // Full-file read (no start/end/count)
    let result = client
        .request("file.read", serde_json::json!({
            "session_id": sid,
            "path": file.to_str().unwrap(),
        }))
        .await
        .unwrap();
    let lines: Vec<&str> = result["lines"]
        .as_array().unwrap()
        .iter().map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(lines, vec!["a", "b", "c", "d", "e"]);

    let _ = std::fs::remove_file(&sock);
}

/// Test the exec workflow: open + read_all + close (no edits, no flush).
#[tokio::test]
async fn exec_read_only() {
    let dir = tempfile::tempdir().unwrap();
    let file_a = dir.path().join("a.txt");
    let file_b = dir.path().join("b.txt");
    std::fs::write(&file_a, "alpha\nbeta\n").unwrap();
    std::fs::write(&file_b, "one\ntwo\n").unwrap();

    let mgr = Arc::new(SessionManager::new());
    let sock = start_server(Arc::clone(&mgr));
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    let mut client = connect_client(&sock).await;

    // 1. Open
    let open_result = client
        .request("session.open", serde_json::json!({
            "files": [file_a.to_str().unwrap(), file_b.to_str().unwrap()]
        }))
        .await
        .unwrap();
    let sid = open_result["session_id"].as_str().unwrap().to_string();

    // 2. Batch read all
    let read_result = client
        .request("batch", serde_json::json!({
            "session_id": sid,
            "ops": [
                {"method": "file.read", "path": file_a.to_str().unwrap()},
                {"method": "file.read", "path": file_b.to_str().unwrap()},
            ]
        }))
        .await
        .unwrap();

    let arr = read_result.as_array().unwrap();
    assert_eq!(arr.len(), 2);
    let lines_a: Vec<&str> = arr[0]["lines"].as_array().unwrap()
        .iter().map(|v| v.as_str().unwrap()).collect();
    let lines_b: Vec<&str> = arr[1]["lines"].as_array().unwrap()
        .iter().map(|v| v.as_str().unwrap()).collect();
    assert_eq!(lines_a, vec!["alpha", "beta"]);
    assert_eq!(lines_b, vec!["one", "two"]);

    // 3. Close (no flush since no edits)
    let close_result = client
        .request("session.close", serde_json::json!({ "session_id": sid }))
        .await
        .unwrap();
    assert_eq!(close_result["status"].as_str().unwrap(), "closed");

    let _ = std::fs::remove_file(&sock);
}

/// Test the exec workflow: open + batch str_replace + flush + close.
#[tokio::test]
async fn exec_str_replace_flush() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("code.py");
    std::fs::write(&file, "def hello():\n    print('world')\n    return True\n").unwrap();

    let mgr = Arc::new(SessionManager::new());
    let sock = start_server(Arc::clone(&mgr));
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    let mut client = connect_client(&sock).await;

    // 1. Open
    let open_result = client
        .request("session.open", serde_json::json!({
            "files": [file.to_str().unwrap()]
        }))
        .await
        .unwrap();
    let sid = open_result["session_id"].as_str().unwrap().to_string();

    // 2. Batch: two str_replace ops
    let batch_result = client
        .request("batch", serde_json::json!({
            "session_id": sid,
            "ops": [
                {
                    "method": "file.str_replace",
                    "path": file.to_str().unwrap(),
                    "old_str": "    print('world')",
                    "new_str": "    print('hello world')",
                },
                {
                    "method": "file.str_replace",
                    "path": file.to_str().unwrap(),
                    "old_str": "    return True",
                    "new_str": "    return False",
                },
            ]
        }))
        .await
        .unwrap();

    let arr = batch_result.as_array().unwrap();
    assert_eq!(arr.len(), 2);
    assert!(arr[0]["match_count"].as_u64().unwrap() >= 1);
    assert!(arr[1]["match_count"].as_u64().unwrap() >= 1);

    // 3. Flush
    let flush_result = client
        .request("session.flush", serde_json::json!({
            "session_id": sid,
        }))
        .await
        .unwrap();
    assert_eq!(flush_result["status"].as_str().unwrap(), "ok");

    // 4. Close
    client
        .request("session.close", serde_json::json!({ "session_id": sid }))
        .await
        .unwrap();

    // 5. Verify on disk
    let content = std::fs::read_to_string(&file).unwrap();
    assert_eq!(content, "def hello():\n    print('hello world')\n    return False\n");

    let _ = std::fs::remove_file(&sock);
}

/// Test the full exec workflow: open + read_all + batch edits + flush + close.
#[tokio::test]
async fn exec_full_workflow() {
    let dir = tempfile::tempdir().unwrap();
    let file_a = dir.path().join("config.py");
    let file_b = dir.path().join("routes.py");
    std::fs::write(&file_a, "DEBUG = True\nLOG_LEVEL = 'INFO'\n").unwrap();
    std::fs::write(&file_b, "def index():\n    return 'home'\n\ndef about():\n    return 'about'\n").unwrap();

    let mgr = Arc::new(SessionManager::new());
    let sock = start_server(Arc::clone(&mgr));
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    let mut client = connect_client(&sock).await;

    // 1. Open both files
    let open_result = client
        .request("session.open", serde_json::json!({
            "files": [file_a.to_str().unwrap(), file_b.to_str().unwrap()]
        }))
        .await
        .unwrap();
    let sid = open_result["session_id"].as_str().unwrap().to_string();
    assert_eq!(open_result["files"].as_object().unwrap().len(), 2);

    // 2. Read all files
    let read_result = client
        .request("batch", serde_json::json!({
            "session_id": sid,
            "ops": [
                {"method": "file.read", "path": file_a.to_str().unwrap()},
                {"method": "file.read", "path": file_b.to_str().unwrap()},
            ]
        }))
        .await
        .unwrap();
    let reads = read_result.as_array().unwrap();
    assert_eq!(reads.len(), 2);

    // 3. Apply edits via str_replace across both files
    let batch_result = client
        .request("batch", serde_json::json!({
            "session_id": sid,
            "ops": [
                {
                    "method": "file.str_replace",
                    "path": file_a.to_str().unwrap(),
                    "old_str": "DEBUG = True",
                    "new_str": "DEBUG = False",
                },
                {
                    "method": "file.str_replace",
                    "path": file_b.to_str().unwrap(),
                    "old_str": "    return 'home'",
                    "new_str": "    return render('index.html')",
                },
                {
                    "method": "file.str_replace",
                    "path": file_b.to_str().unwrap(),
                    "old_str": "    return 'about'",
                    "new_str": "    return render('about.html')",
                },
            ]
        }))
        .await
        .unwrap();
    let edits = batch_result.as_array().unwrap();
    assert_eq!(edits.len(), 3);

    // 4. Flush
    let flush_result = client
        .request("session.flush", serde_json::json!({
            "session_id": sid,
            "force": false,
        }))
        .await
        .unwrap();
    assert_eq!(flush_result["files_written"].as_array().unwrap().len(), 2);

    // 5. Close
    client
        .request("session.close", serde_json::json!({ "session_id": sid }))
        .await
        .unwrap();

    // 6. Verify both files on disk
    let content_a = std::fs::read_to_string(&file_a).unwrap();
    assert_eq!(content_a, "DEBUG = False\nLOG_LEVEL = 'INFO'\n");

    let content_b = std::fs::read_to_string(&file_b).unwrap();
    assert_eq!(content_b, "def index():\n    return render('index.html')\n\ndef about():\n    return render('about.html')\n");

    let _ = std::fs::remove_file(&sock);
}
