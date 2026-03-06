pub mod buffer;
pub mod client;
pub mod edit;
pub mod flush;
pub mod format;
pub mod manager;
pub mod parse;
pub mod session;
pub mod str_match;

use std::path::PathBuf;

/// Default daemon socket path, shared by daemon, CLI, and MCP.
pub fn default_socket_path() -> PathBuf {
    let runtime_dir = std::env::var("XDG_RUNTIME_DIR")
        .or_else(|_| std::env::var("TMPDIR"))
        .unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(runtime_dir).join("slipstream.sock")
}

/// Resolve a potentially relative path to absolute using `std::env::current_dir()`.
/// Absolute paths pass through unchanged. Uses `join` (not `canonicalize`) so
/// non-existent files still resolve. Falls back to the original path if CWD
/// cannot be determined.
pub fn resolve_path(path: &str) -> String {
    let p = std::path::Path::new(path);
    if p.is_absolute() {
        return path.to_string();
    }
    match std::env::current_dir() {
        Ok(cwd) => cwd.join(p).to_string_lossy().to_string(),
        Err(_) => path.to_string(),
    }
}

/// Walk a JSON ops array and resolve each `"path"` field to absolute using [`resolve_path`].
pub fn resolve_ops_paths(ops: &mut serde_json::Value) {
    if let Some(arr) = ops.as_array_mut() {
        for op in arr {
            if let Some(path_val) = op.get("path").and_then(|p| p.as_str()).map(|s| s.to_string())
            {
                op["path"] = serde_json::Value::String(resolve_path(&path_val));
            }
        }
    }
}

#[cfg(test)]
mod path_tests {
    use super::*;

    #[test]
    fn resolve_path_absolute_unchanged() {
        let result = resolve_path("/usr/bin/cat");
        assert_eq!(result, "/usr/bin/cat");
    }

    #[test]
    fn resolve_path_relative_becomes_absolute() {
        let result = resolve_path("src/main.rs");
        assert!(result.starts_with('/'), "should be absolute: {result}");
        assert!(
            result.ends_with("src/main.rs"),
            "should end with original: {result}"
        );
    }

    #[test]
    fn resolve_path_dot_relative() {
        let result = resolve_path("./Cargo.toml");
        assert!(result.starts_with('/'), "should be absolute: {result}");
        assert!(
            result.ends_with("Cargo.toml"),
            "should end with filename: {result}"
        );
    }

    #[test]
    fn resolve_ops_paths_makes_relative_absolute() {
        let mut ops = serde_json::json!([
            {"method": "file.str_replace", "path": "src/main.rs", "old_str": "a", "new_str": "b"},
            {"method": "file.read", "path": "/absolute/path.rs"},
        ]);
        resolve_ops_paths(&mut ops);
        let arr = ops.as_array().unwrap();
        let p0 = arr[0]["path"].as_str().unwrap();
        let p1 = arr[1]["path"].as_str().unwrap();
        assert!(
            p0.starts_with('/'),
            "relative should become absolute: {p0}"
        );
        assert!(p0.ends_with("src/main.rs"));
        assert_eq!(p1, "/absolute/path.rs", "absolute unchanged");
    }

    #[test]
    fn resolve_ops_paths_no_path_field_ok() {
        let mut ops = serde_json::json!([{"method": "session.list"}]);
        resolve_ops_paths(&mut ops);
    }
}
