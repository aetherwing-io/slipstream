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
