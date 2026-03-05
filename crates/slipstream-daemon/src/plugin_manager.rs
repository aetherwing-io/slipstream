//! Plugin Manager — discovers and spawns FCP server binaries on demand.
//!
//! Discovery order (merged, deduplicated by extension):
//! 1. Sibling binaries: `current_exe().parent() / "fcp-*"`
//! 2. Config file: `~/.config/slipstream/plugins.toml`
//! 3. PATH scan: `fcp-*` binaries in PATH
//!
//! Spawned processes get `SLIPSTREAM_SOCKET` env var and are expected to
//! connect back with `fcp.register`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use serde::Deserialize;
use tokio::process::Child;
use tokio::sync::Mutex;

/// How long to wait for a spawned plugin to register with the FCP bridge.
pub const SPAWN_TIMEOUT: Duration = Duration::from_secs(5);

/// How long to wait before retrying a failed plugin spawn.
pub const FAILURE_COOLDOWN: Duration = Duration::from_secs(300);

/// Poll interval when waiting for a plugin to register.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// A discovered plugin configuration.
#[derive(Debug, Clone)]
pub struct PluginEntry {
    pub name: String,
    pub command: PathBuf,
    pub args: Vec<String>,
    pub extensions: Vec<String>,
}

/// State of a spawned plugin process.
struct PluginProcess {
    _child: Child,
    #[allow(dead_code)]
    started_at: Instant,
}

/// Tracks plugin spawn failures with cooldown.
struct FailureRecord {
    failed_at: Instant,
}

/// The Plugin Manager — discovers FCP server binaries and spawns them on demand.
pub struct PluginManager {
    /// extension (lowercase) → plugin name
    ext_map: DashMap<String, String>,
    /// plugin name → entry config
    entries: DashMap<String, PluginEntry>,
    /// plugin name → running process (behind Mutex for spawn serialization)
    children: Arc<Mutex<HashMap<String, PluginProcess>>>,
    /// plugin name → failure record
    failures: DashMap<String, FailureRecord>,
}

impl PluginManager {
    pub fn new() -> Self {
        Self {
            ext_map: DashMap::new(),
            entries: DashMap::new(),
            children: Arc::new(Mutex::new(HashMap::new())),
            failures: DashMap::new(),
        }
    }

    /// Register a plugin entry. First registration for an extension wins.
    fn register_entry(&self, entry: PluginEntry) {
        for ext in &entry.extensions {
            let ext_lower = ext.to_lowercase();
            // First registration wins — don't overwrite
            self.ext_map.entry(ext_lower).or_insert(entry.name.clone());
        }
        self.entries.entry(entry.name.clone()).or_insert(entry);
    }

    /// Look up which plugin handles an extension.
    pub fn lookup(&self, ext: &str) -> Option<String> {
        self.ext_map.get(&ext.to_lowercase()).map(|v| v.clone())
    }

    /// Get the plugin entry by name.
    pub fn get_entry(&self, name: &str) -> Option<PluginEntry> {
        self.entries.get(name).map(|e| e.clone())
    }

    /// List all discovered plugins.
    pub fn list_plugins(&self) -> Vec<PluginEntry> {
        self.entries.iter().map(|e| e.value().clone()).collect()
    }

    /// Check if a plugin is in failure cooldown.
    pub fn is_in_cooldown(&self, name: &str) -> bool {
        if let Some(record) = self.failures.get(name) {
            record.failed_at.elapsed() < FAILURE_COOLDOWN
        } else {
            false
        }
    }

    /// Mark a plugin as failed (starts cooldown timer).
    pub fn mark_failed(&self, name: &str) {
        self.failures.insert(
            name.to_string(),
            FailureRecord {
                failed_at: Instant::now(),
            },
        );
    }

    /// Clear failure record (e.g., after successful registration).
    pub fn clear_failure(&self, name: &str) {
        self.failures.remove(name);
    }

    // --- Discovery ---

    /// Discover sibling binaries next to the current executable.
    /// Convention: `fcp-py` handles `.py`, `fcp-rust` handles `.rs`, etc.
    pub fn discover_siblings(&self, current_exe: &Path) {
        let dir = match current_exe.parent() {
            Some(d) => d,
            None => return,
        };

        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return,
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }

            let name = match path.file_name().and_then(|n| n.to_str()) {
                Some(n) => n.to_string(),
                None => continue,
            };

            if !name.starts_with("fcp-") {
                continue;
            }

            // Check executable bit on Unix
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Ok(meta) = path.metadata() {
                    if meta.permissions().mode() & 0o111 == 0 {
                        continue;
                    }
                }
            }

            // Derive extension from name: fcp-py → py, fcp-rust → rs
            let suffix = &name["fcp-".len()..];
            let extensions = extension_from_plugin_name(suffix);

            if !extensions.is_empty() {
                self.register_entry(PluginEntry {
                    name: name.clone(),
                    command: path,
                    args: Vec::new(),
                    extensions,
                });
                tracing::info!("discovered sibling plugin: {name}");
            }
        }
    }

    /// Load plugins from a TOML config file.
    pub fn load_config(&self, path: &Path) {
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => return, // Config file is optional
        };

        let config: PluginsConfig = match toml::from_str(&content) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("failed to parse {}: {e}", path.display());
                return;
            }
        };

        for (name, plugin) in config.plugins {
            let full_name = format!("fcp-{name}");
            self.register_entry(PluginEntry {
                name: full_name.clone(),
                command: PathBuf::from(&plugin.command),
                args: plugin.args.unwrap_or_default(),
                extensions: plugin.extensions,
            });
            tracing::info!("loaded plugin from config: {full_name}");
        }
    }

    /// Discover `fcp-*` binaries in PATH.
    pub fn discover_path(&self) {
        let path_var = match std::env::var("PATH") {
            Ok(p) => p,
            Err(_) => return,
        };

        for dir in std::env::split_paths(&path_var) {
            let entries = match std::fs::read_dir(&dir) {
                Ok(e) => e,
                Err(_) => continue,
            };

            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_file() {
                    continue;
                }

                let name = match path.file_name().and_then(|n| n.to_str()) {
                    Some(n) => n.to_string(),
                    None => continue,
                };

                if !name.starts_with("fcp-") {
                    continue;
                }

                // Skip if we already have this plugin
                if self.entries.contains_key(&name) {
                    continue;
                }

                let suffix = &name["fcp-".len()..];
                let extensions = extension_from_plugin_name(suffix);

                if !extensions.is_empty() {
                    self.register_entry(PluginEntry {
                        name: name.clone(),
                        command: path,
                        args: Vec::new(),
                        extensions,
                    });
                    tracing::debug!("discovered PATH plugin: {name}");
                }
            }
        }
    }

    /// Run all discovery phases in order.
    pub fn discover_all(&self, current_exe: &Path) {
        self.discover_siblings(current_exe);

        // Config file: ~/.config/slipstream/plugins.toml
        if let Some(config_dir) = dirs_config_dir() {
            let config_path = config_dir.join("slipstream").join("plugins.toml");
            self.load_config(&config_path);
        }

        self.discover_path();
    }

    // --- Spawn ---

    /// Spawn a plugin process. Returns Ok(()) if spawned, Err if failed.
    /// The plugin is expected to connect back to the daemon via SLIPSTREAM_SOCKET.
    pub async fn spawn(
        &self,
        name: &str,
        socket_path: &Path,
    ) -> Result<(), String> {
        // Check cooldown
        if self.is_in_cooldown(name) {
            return Err(format!("{name} in failure cooldown"));
        }

        let entry = self.get_entry(name).ok_or_else(|| format!("plugin {name} not found"))?;

        // Check if already running
        {
            let children = self.children.lock().await;
            if children.contains_key(name) {
                return Ok(());
            }
        }

        tracing::info!("spawning plugin: {name} ({})", entry.command.display());

        let child = tokio::process::Command::new(&entry.command)
            .args(&entry.args)
            .env("SLIPSTREAM_SOCKET", socket_path.as_os_str())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| {
                self.mark_failed(name);
                format!("failed to spawn {name}: {e}")
            })?;

        let mut children = self.children.lock().await;
        children.insert(
            name.to_string(),
            PluginProcess {
                _child: child,
                started_at: Instant::now(),
            },
        );

        self.clear_failure(name);
        Ok(())
    }

    /// Wait for a plugin to register with the FCP bridge.
    /// Polls `fcp_bridge.is_handler_live()` every 100ms up to SPAWN_TIMEOUT.
    pub async fn wait_for_registration(
        &self,
        name: &str,
        fcp_bridge: &crate::fcp_bridge::FcpBridge,
    ) -> bool {
        let deadline = Instant::now() + SPAWN_TIMEOUT;
        while Instant::now() < deadline {
            if fcp_bridge.is_handler_live(name) {
                return true;
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
        false
    }

    /// Kill all child processes (for daemon shutdown).
    pub async fn shutdown(&self) {
        let mut children = self.children.lock().await;
        for (name, mut proc) in children.drain() {
            tracing::info!("stopping plugin: {name}");
            let _ = proc._child.kill().await;
        }
    }
}

// --- Config file format ---

#[derive(Debug, Deserialize)]
struct PluginsConfig {
    #[serde(default)]
    plugins: HashMap<String, PluginConfig>,
}

#[derive(Debug, Deserialize)]
struct PluginConfig {
    command: String,
    args: Option<Vec<String>>,
    extensions: Vec<String>,
}

// --- Helpers ---

/// Map plugin name suffix to file extensions.
/// Known mappings for common FCP servers.
fn extension_from_plugin_name(suffix: &str) -> Vec<String> {
    match suffix {
        "py" | "python" => vec!["py".into()],
        "rust" | "rs" => vec!["rs".into()],
        "sheets" => vec!["xlsx".into(), "xls".into(), "ods".into(), "csv".into()],
        "midi" => vec!["mid".into(), "midi".into()],
        "slides" => vec!["pptx".into(), "ppt".into(), "odp".into()],
        "terraform" | "tf" => vec!["tf".into(), "tfvars".into()],
        "drawio" => vec!["drawio".into()],
        "ts" | "typescript" => vec!["ts".into(), "tsx".into()],
        "js" | "javascript" => vec!["js".into(), "jsx".into()],
        "go" => vec!["go".into()],
        "java" => vec!["java".into()],
        "c" | "cpp" => vec!["c".into(), "cpp".into(), "h".into(), "hpp".into()],
        _ => {
            // Unknown suffix — use suffix as extension directly
            vec![suffix.into()]
        }
    }
}

/// Cross-platform config directory (~/.config on Unix, AppData on Windows).
fn dirs_config_dir() -> Option<PathBuf> {
    #[cfg(unix)]
    {
        std::env::var("XDG_CONFIG_HOME")
            .ok()
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var("HOME")
                    .ok()
                    .map(|h| PathBuf::from(h).join(".config"))
            })
    }
    #[cfg(not(unix))]
    {
        std::env::var("APPDATA").ok().map(PathBuf::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    #[test]
    fn test_extension_from_plugin_name() {
        assert_eq!(extension_from_plugin_name("py"), vec!["py"]);
        assert_eq!(extension_from_plugin_name("rust"), vec!["rs"]);
        assert_eq!(
            extension_from_plugin_name("sheets"),
            vec!["xlsx", "xls", "ods", "csv"]
        );
        // Unknown suffix maps to itself
        assert_eq!(extension_from_plugin_name("wasm"), vec!["wasm"]);
    }

    #[test]
    fn test_register_and_lookup() {
        let mgr = PluginManager::new();
        mgr.register_entry(PluginEntry {
            name: "fcp-py".into(),
            command: PathBuf::from("/usr/bin/fcp-py"),
            args: vec![],
            extensions: vec!["py".into()],
        });
        assert_eq!(mgr.lookup("py"), Some("fcp-py".into()));
        assert_eq!(mgr.lookup("PY"), Some("fcp-py".into()));
        assert!(mgr.lookup("rs").is_none());
    }

    #[test]
    fn test_first_registration_wins() {
        let mgr = PluginManager::new();
        mgr.register_entry(PluginEntry {
            name: "fcp-py-sibling".into(),
            command: PathBuf::from("/opt/fcp-py"),
            args: vec![],
            extensions: vec!["py".into()],
        });
        mgr.register_entry(PluginEntry {
            name: "fcp-py-path".into(),
            command: PathBuf::from("/usr/bin/fcp-py"),
            args: vec![],
            extensions: vec!["py".into()],
        });
        // First registration wins
        assert_eq!(mgr.lookup("py"), Some("fcp-py-sibling".into()));
    }

    #[test]
    fn test_failure_cooldown() {
        let mgr = PluginManager::new();
        assert!(!mgr.is_in_cooldown("fcp-py"));
        mgr.mark_failed("fcp-py");
        assert!(mgr.is_in_cooldown("fcp-py"));
        mgr.clear_failure("fcp-py");
        assert!(!mgr.is_in_cooldown("fcp-py"));
    }

    #[test]
    fn test_list_plugins() {
        let mgr = PluginManager::new();
        mgr.register_entry(PluginEntry {
            name: "fcp-py".into(),
            command: PathBuf::from("/usr/bin/fcp-py"),
            args: vec![],
            extensions: vec!["py".into()],
        });
        mgr.register_entry(PluginEntry {
            name: "fcp-rust".into(),
            command: PathBuf::from("/usr/bin/fcp-rust"),
            args: vec![],
            extensions: vec!["rs".into()],
        });
        let plugins = mgr.list_plugins();
        assert_eq!(plugins.len(), 2);
    }

    #[test]
    fn test_discover_siblings() {
        let tmp = tempfile::tempdir().unwrap();

        // Create a fake fcp-py binary
        let fcp_py = tmp.path().join("fcp-py");
        {
            let mut f = std::fs::File::create(&fcp_py).unwrap();
            f.write_all(b"#!/bin/sh\n").unwrap();
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&fcp_py, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        // Create a fake slipstream binary
        let exe = tmp.path().join("slipstream");
        std::fs::File::create(&exe).unwrap();

        let mgr = PluginManager::new();
        mgr.discover_siblings(&exe);
        assert_eq!(mgr.lookup("py"), Some("fcp-py".into()));
    }

    #[test]
    fn test_load_config() {
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("plugins.toml");
        std::fs::write(
            &config_path,
            r#"
[plugins.sheets]
command = "uvx"
args = ["fcp-sheets"]
extensions = ["xlsx", "xls", "ods", "csv"]

[plugins.custom]
command = "/opt/fcp-custom"
extensions = ["xyz"]
"#,
        )
        .unwrap();

        let mgr = PluginManager::new();
        mgr.load_config(&config_path);

        assert_eq!(mgr.lookup("xlsx"), Some("fcp-sheets".into()));
        assert_eq!(mgr.lookup("xls"), Some("fcp-sheets".into()));
        assert_eq!(mgr.lookup("xyz"), Some("fcp-custom".into()));

        let entry = mgr.get_entry("fcp-sheets").unwrap();
        assert_eq!(entry.command, PathBuf::from("uvx"));
        assert_eq!(entry.args, vec!["fcp-sheets"]);
    }

    #[test]
    fn test_load_config_missing_file() {
        let mgr = PluginManager::new();
        mgr.load_config(Path::new("/nonexistent/plugins.toml"));
        assert!(mgr.list_plugins().is_empty());
    }

    #[test]
    fn test_load_config_bad_toml() {
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("plugins.toml");
        std::fs::write(&config_path, "this is not valid toml {{{").unwrap();

        let mgr = PluginManager::new();
        mgr.load_config(&config_path);
        assert!(mgr.list_plugins().is_empty());
    }
}
