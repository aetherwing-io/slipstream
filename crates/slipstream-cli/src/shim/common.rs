use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};

use slipstream_core::client::{Client, ClientError};

/// Connect to the slipstream daemon (with auto-start).
pub async fn connect() -> Result<Client, ClientError> {
    let socket_path = socket_path();
    let auto_start = std::env::var("SLIPSTREAM_NO_AUTO_START").is_err();
    Client::connect(&socket_path, auto_start).await
}

fn socket_path() -> PathBuf {
    match std::env::var("SLIPSTREAM_SOCKET") {
        Ok(p) => PathBuf::from(p),
        Err(_) => slipstream_core::default_socket_path(),
    }
}

/// Open a session with the given files, returning the session_id.
pub async fn session_open(
    client: &mut Client,
    files: &[PathBuf],
) -> Result<String, ClientError> {
    let result = client
        .request(
            "session.open",
            serde_json::json!({ "files": files }),
        )
        .await?;
    Ok(result["session_id"]
        .as_str()
        .unwrap_or("")
        .to_string())
}

/// Read lines from a file in the session.
/// Returns (lines, trailing_newline).
pub async fn file_read(
    client: &mut Client,
    session_id: &str,
    path: &Path,
    start: Option<usize>,
    end: Option<usize>,
) -> Result<(Vec<String>, bool), ClientError> {
    let mut params = serde_json::json!({
        "session_id": session_id,
        "path": path,
    });
    if let Some(s) = start {
        params["start"] = serde_json::json!(s);
    }
    if let Some(e) = end {
        params["end"] = serde_json::json!(e);
    }
    let result = client.request("file.read", params).await?;
    let lines: Vec<String> = result["lines"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .map(|v| v.as_str().unwrap_or("").to_string())
                .collect()
        })
        .unwrap_or_default();
    let trailing_newline = result["trailing_newline"].as_bool().unwrap_or(true);
    Ok((lines, trailing_newline))
}

/// Read all lines from a file, returning (lines, total_line_count, trailing_newline).
pub async fn file_read_all(
    client: &mut Client,
    session_id: &str,
    path: &Path,
) -> Result<(Vec<String>, usize, bool), ClientError> {
    let result = client
        .request(
            "file.read",
            serde_json::json!({
                "session_id": session_id,
                "path": path,
                "start": 0,
            }),
        )
        .await?;
    let lines: Vec<String> = result["lines"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .map(|v| v.as_str().unwrap_or("").to_string())
                .collect()
        })
        .unwrap_or_default();
    let total = lines.len();
    let trailing_newline = result["trailing_newline"].as_bool().unwrap_or(true);
    Ok((lines, total, trailing_newline))
}

/// Replace a string in a file.
pub async fn file_str_replace(
    client: &mut Client,
    session_id: &str,
    path: &Path,
    old_str: &str,
    new_str: &str,
    replace_all: bool,
) -> Result<serde_json::Value, ClientError> {
    client
        .request(
            "file.str_replace",
            serde_json::json!({
                "session_id": session_id,
                "path": path,
                "old_str": old_str,
                "new_str": new_str,
                "replace_all": replace_all,
            }),
        )
        .await
}

/// Close a session (auto-flushes by default).
pub async fn session_close(
    client: &mut Client,
    session_id: &str,
) -> Result<(), ClientError> {
    client
        .request(
            "session.close",
            serde_json::json!({
                "session_id": session_id,
            }),
        )
        .await?;
    Ok(())
}

/// Emit an LLM-visible hint to stdout after successful daemon routing.
pub fn emit_hint(binary_name: &str) {
    if std::env::var("SLIPSTREAM_SHIM_QUIET").is_ok() {
        return;
    }

    // First invocation only — one-shot interstitial
    let counter_path = std::path::PathBuf::from("/tmp/.slipstream_shim_hint_count");
    let count = std::fs::read_to_string(&counter_path)
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok())
        .unwrap_or(0);
    if count >= 1 {
        return;
    }
    let _ = std::fs::write(&counter_path, (count + 1).to_string());

    println!(
        "\u{26a0} {} compatibility mode. Run `slipstream --agents` for capabilities.",
        binary_name
    );
}

/// Run a closure with daemon connection, falling back to real binary on failure.
pub fn run_with_fallback<F>(binary_name: &str, args: &[String], f: F) -> i32
where
    F: FnOnce() -> Result<(), ShimError>,
{
    match f() {
        Ok(()) => {
            emit_hint(binary_name);
            0
        }
        Err(ShimError::Fallback) => fallback_exec(binary_name, args),
        Err(ShimError::Client(e)) => {
            if no_fallback() {
                eprintln!("slipstream ({binary_name}): {e}");
                1
            } else {
                fallback_exec(binary_name, args)
            }
        }
        Err(ShimError::Io(e)) => {
            eprintln!("{binary_name}: {e}");
            1
        }
        Err(ShimError::Usage(msg)) => {
            eprintln!("{binary_name}: {msg}");
            2
        }
    }
}

/// Exec the real binary, replacing this process. Never returns on success.
pub(super) fn fallback_exec(binary_name: &str, args: &[String]) -> ! {
    if let Some(real_binary) = find_real_binary(binary_name) {
        let err = std::process::Command::new(&real_binary).args(args).exec();
        eprintln!(
            "slipstream: failed to exec {}: {err}",
            real_binary.display()
        );
    } else {
        eprintln!("slipstream: cannot find real {binary_name} binary");
    }
    std::process::exit(127)
}

/// Find the real binary by checking SLIPSTREAM_SHIM_FALLBACK_DIR, then common locations.
/// Skips any candidate that resolves (via symlink) back to the current executable.
fn find_real_binary(binary_name: &str) -> Option<PathBuf> {
    let my_exe = std::env::current_exe()
        .ok()
        .and_then(|p| std::fs::canonicalize(p).ok());

    if let Ok(dir) = std::env::var("SLIPSTREAM_SHIM_FALLBACK_DIR") {
        let p = PathBuf::from(&dir).join(binary_name);
        if is_real_binary(&p, &my_exe) {
            return Some(p);
        }
    }

    let search_dirs = ["/usr/bin", "/bin", "/usr/local/bin"];
    for dir in &search_dirs {
        let p = PathBuf::from(dir).join(binary_name);
        if is_real_binary(&p, &my_exe) {
            return Some(p);
        }
    }

    None
}

/// Returns true if `path` exists and does not resolve to the same binary as `my_exe`.
fn is_real_binary(path: &Path, my_exe: &Option<PathBuf>) -> bool {
    if !path.exists() {
        return false;
    }
    if let Some(ref me) = my_exe {
        if let Ok(resolved) = std::fs::canonicalize(path) {
            if &resolved == me {
                return false; // symlink to self
            }
        }
    }
    true
}

fn no_fallback() -> bool {
    std::env::var("SLIPSTREAM_SHIM_NO_FALLBACK").is_ok()
}

/// Build a single-threaded tokio runtime for minimal startup overhead.
pub fn build_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("failed to build tokio runtime")
}

#[derive(Debug)]
pub enum ShimError {
    /// Signal that we should fallback to the real binary.
    Fallback,
    /// Daemon client error (connection failed, RPC error, etc.)
    Client(ClientError),
    /// General I/O error.
    Io(std::io::Error),
    /// Usage / argument parsing error.
    #[allow(dead_code)]
    Usage(String),
}

impl From<ClientError> for ShimError {
    fn from(e: ClientError) -> Self {
        ShimError::Client(e)
    }
}

impl From<std::io::Error> for ShimError {
    fn from(e: std::io::Error) -> Self {
        ShimError::Io(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn is_real_binary_returns_false_for_nonexistent() {
        let p = PathBuf::from("/tmp/slipstream_test_nonexistent_binary_xyz");
        assert!(!is_real_binary(&p, &None));
    }

    #[test]
    fn is_real_binary_returns_true_for_real_binary() {
        // /usr/bin/true exists on all unix systems
        let p = PathBuf::from("/usr/bin/true");
        if p.exists() {
            let fake_exe = Some(PathBuf::from("/usr/bin/false"));
            assert!(is_real_binary(&p, &fake_exe));
        }
    }

    #[test]
    fn emit_hint_suppressed_by_env() {
        // Set the quiet env var and verify emit_hint returns without panicking.
        // (We can't easily capture stderr in-process, but we verify the env check logic.)
        std::env::set_var("SLIPSTREAM_SHIM_QUIET", "1");
        emit_hint("cat"); // should return immediately, no output
        std::env::remove_var("SLIPSTREAM_SHIM_QUIET");
    }

    #[test]
    fn is_real_binary_returns_false_for_symlink_to_self() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("real_bin");
        std::fs::write(&target, "#!/bin/sh\n").unwrap();

        let link = dir.path().join("link_bin");
        symlink(&target, &link).unwrap();

        let canonical_target = std::fs::canonicalize(&target).unwrap();
        assert!(!is_real_binary(&link, &Some(canonical_target)));
    }

    #[test]
    fn is_real_binary_returns_true_for_symlink_to_different_binary() {
        let dir = tempfile::tempdir().unwrap();
        let target_a = dir.path().join("bin_a");
        let target_b = dir.path().join("bin_b");
        std::fs::write(&target_a, "a").unwrap();
        std::fs::write(&target_b, "b").unwrap();

        let link = dir.path().join("link_to_a");
        symlink(&target_a, &link).unwrap();

        let canonical_b = std::fs::canonicalize(&target_b).unwrap();
        assert!(is_real_binary(&link, &Some(canonical_b)));
    }
}
