pub mod coordinator;
pub mod handler;
pub mod protocol;
pub mod registry;
pub mod types;

use std::sync::Arc;

use slipstream_core::manager::SessionManager;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::sync::Semaphore;

use crate::coordinator::Coordinator;
use crate::registry::FormatRegistry;

/// Maximum number of concurrent client connections.
pub const MAX_CONNECTIONS: usize = 128;

/// Run the server accept loop. Spawns a task per connection.
/// Returns when the listener is dropped or encounters a fatal error.
pub async fn serve(
    listener: UnixListener,
    mgr: Arc<SessionManager>,
    registry: Arc<FormatRegistry>,
    coordinator: Arc<Coordinator>,
) {
    let conn_semaphore = Arc::new(Semaphore::new(MAX_CONNECTIONS));

    loop {
        match listener.accept().await {
            Ok((stream, _addr)) => {
                // Verify peer UID matches our own (reject connections from other users)
                #[cfg(unix)]
                {
                    let my_uid = unsafe { libc::getuid() };
                    match stream.peer_cred() {
                        Ok(cred) => {
                            if cred.uid() != my_uid {
                                tracing::warn!(
                                    "rejected connection from uid {} (expected {})",
                                    cred.uid(),
                                    my_uid
                                );
                                continue;
                            }
                        }
                        Err(e) => {
                            tracing::warn!("failed to get peer credentials, rejecting: {e}");
                            continue;
                        }
                    }
                }

                let permit = match conn_semaphore.clone().try_acquire_owned() {
                    Ok(permit) => permit,
                    Err(_) => {
                        tracing::warn!(
                            "connection limit reached ({MAX_CONNECTIONS}), rejecting"
                        );
                        // Drop the stream — client will get a connection reset
                        drop(stream);
                        continue;
                    }
                };

                let mgr = Arc::clone(&mgr);
                let registry = Arc::clone(&registry);
                let coordinator = Arc::clone(&coordinator);
                tokio::spawn(async move {
                    if let Err(e) =
                        handle_connection(stream, mgr, registry, coordinator).await
                    {
                        tracing::warn!("connection error: {e}");
                    }
                    // Permit is dropped here, releasing the semaphore slot
                    drop(permit);
                });
            }
            Err(e) => {
                tracing::error!("accept error: {e}");
                break;
            }
        }
    }
}

/// Handle a single client connection.
///
/// Protocol: newline-delimited JSON. Each line is a JSON-RPC request,
/// each response is a JSON line back.
async fn handle_connection(
    stream: tokio::net::UnixStream,
    mgr: Arc<SessionManager>,
    registry: Arc<FormatRegistry>,
    coordinator: Arc<Coordinator>,
) -> Result<(), Box<dyn std::error::Error>> {
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::with_capacity(64 * 1024, reader).lines();
    let mut resp_buf: Vec<u8> = Vec::with_capacity(4096);

    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }

        let mgr_clone = Arc::clone(&mgr);
        let reg_clone = Arc::clone(&registry);
        let coord_clone = Arc::clone(&coordinator);
        let response = match serde_json::from_str::<protocol::Request>(&line) {
            Ok(req) => {
                tracing::debug!("request: {} (id={:?})", req.method, req.id);
                tokio::task::spawn_blocking(move || {
                    handler::dispatch(req, &mgr_clone, &reg_clone, &coord_clone)
                })
                .await
                .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })?
            }
            Err(e) => protocol::Response::err(
                None,
                protocol::RpcError {
                    code: protocol::ERR_PARSE,
                    message: format!("parse error: {e}"),
                    data: None,
                },
            ),
        };

        resp_buf.clear();
        serde_json::to_writer(&mut resp_buf, &response)?;
        resp_buf.push(b'\n');
        writer.write_all(&resp_buf).await?;
    }

    Ok(())
}
