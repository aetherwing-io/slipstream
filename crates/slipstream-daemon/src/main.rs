use std::path::PathBuf;
use std::sync::Arc;
#[cfg(unix)]
use std::os::unix::fs::FileTypeExt;

use slipstream_core::manager::SessionManager;
use slipstream_daemon::coordinator::Coordinator;
use slipstream_daemon::registry::FormatRegistry;
use tokio::net::UnixListener;

/// Default socket path.
fn default_socket_path() -> PathBuf {
    let runtime_dir = std::env::var("XDG_RUNTIME_DIR")
        .or_else(|_| std::env::var("TMPDIR"))
        .unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(runtime_dir).join("slipstream.sock")
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("slipstream_daemon=info".parse().unwrap()),
        )
        .init();

    let socket_path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(default_socket_path);

    // Clean up stale socket — verify it's actually a socket (not a regular file or symlink)
    if socket_path.exists() {
        match std::fs::symlink_metadata(&socket_path) {
            Ok(meta) if meta.file_type().is_socket() => {
                if let Err(e) = std::fs::remove_file(&socket_path) {
                    tracing::error!(
                        "failed to remove stale socket {}: {e}",
                        socket_path.display()
                    );
                    std::process::exit(1);
                }
            }
            Ok(meta) => {
                tracing::error!(
                    "path {} exists but is not a socket (type: {:?}), refusing to remove",
                    socket_path.display(),
                    meta.file_type()
                );
                std::process::exit(1);
            }
            Err(e) => {
                tracing::error!(
                    "failed to stat {}: {e}",
                    socket_path.display()
                );
                std::process::exit(1);
            }
        }
    }

    let listener = match UnixListener::bind(&socket_path) {
        Ok(l) => l,
        Err(e) => {
            tracing::error!("failed to bind {}: {e}", socket_path.display());
            std::process::exit(1);
        }
    };

    // Restrict socket to owner only (mode 0600)
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&socket_path, std::fs::Permissions::from_mode(0o600))
            .expect("failed to set socket permissions");
    }

    tracing::info!("listening on {}", socket_path.display());

    let mgr = Arc::new(SessionManager::new());
    let registry = Arc::new(FormatRegistry::default_registry());
    let coordinator = Arc::new(Coordinator::new());

    // Spawn session sweeper (periodic cleanup of expired sessions)
    let sweep_mgr = Arc::clone(&mgr);
    let sweep_coord = Arc::clone(&coordinator);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
        loop {
            interval.tick().await;
            if let Ok(expired) = sweep_mgr.sweep_expired() {
                if !expired.is_empty() {
                    sweep_coord.on_sessions_swept(&expired);
                    for id in &expired {
                        tracing::info!("expired session: {id}");
                    }
                }
            }
        }
    });

    slipstream_daemon::serve(listener, mgr, registry, coordinator).await;
}
