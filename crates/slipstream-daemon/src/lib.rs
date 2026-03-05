pub mod coordinator;
pub mod fcp_bridge;
pub mod handler;
pub mod plugin_manager;
pub mod protocol;
pub mod registry;
pub mod types;

use std::sync::Arc;

use slipstream_core::manager::SessionManager;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::sync::Semaphore;

use crate::coordinator::Coordinator;
use crate::fcp_bridge::{FcpBridge, FcpRegisterParams, FcpResponse};
use crate::plugin_manager::PluginManager;
use crate::registry::FormatRegistry;

/// Maximum number of concurrent client connections.
pub const MAX_CONNECTIONS: usize = 128;

/// Run the daemon server. Call this from `main()` or the unified binary.
///
/// `socket_path` overrides the default; if `None`, uses `$XDG_RUNTIME_DIR/slipstream.sock`.
pub async fn run_daemon(socket_path: Option<std::path::PathBuf>) {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("slipstream_daemon=info".parse().unwrap()),
        )
        .init();

    let socket_path = socket_path.unwrap_or_else(slipstream_core::default_socket_path);

    // Clean up stale socket — verify it's actually a socket (not a regular file or symlink)
    #[cfg(unix)]
    if socket_path.exists() {
        use std::os::unix::fs::FileTypeExt;
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

    let listener = match tokio::net::UnixListener::bind(&socket_path) {
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
    let fcp_bridge = Arc::new(FcpBridge::new());
    let plugin_mgr = Arc::new(PluginManager::new());

    // Discover FCP plugins (sibling binaries, config, PATH)
    if let Ok(exe) = std::env::current_exe() {
        plugin_mgr.discover_all(&exe);
    }

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

    serve(listener, mgr, registry, coordinator, fcp_bridge, plugin_mgr, socket_path).await;
}

/// Run the server accept loop. Spawns a task per connection.
/// Returns when the listener is dropped or encounters a fatal error.
pub async fn serve(
    listener: UnixListener,
    mgr: Arc<SessionManager>,
    registry: Arc<FormatRegistry>,
    coordinator: Arc<Coordinator>,
    fcp_bridge: Arc<FcpBridge>,
    plugin_mgr: Arc<PluginManager>,
    socket_path: std::path::PathBuf,
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
                let fcp_bridge = Arc::clone(&fcp_bridge);
                let plugin_mgr = Arc::clone(&plugin_mgr);
                let socket_path = socket_path.clone();
                tokio::spawn(async move {
                    if let Err(e) =
                        handle_connection(stream, mgr, registry, coordinator, fcp_bridge, plugin_mgr, socket_path).await
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
/// Protocol: newline-delimited JSON. First message determines connection type:
/// - `fcp.register` → FCP handler connection (bidirectional)
/// - anything else → normal client (request/response)
async fn handle_connection(
    stream: tokio::net::UnixStream,
    mgr: Arc<SessionManager>,
    registry: Arc<FormatRegistry>,
    coordinator: Arc<Coordinator>,
    fcp_bridge: Arc<FcpBridge>,
    plugin_mgr: Arc<PluginManager>,
    socket_path: std::path::PathBuf,
) -> Result<(), Box<dyn std::error::Error>> {
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::with_capacity(64 * 1024, reader).lines();
    let mut resp_buf: Vec<u8> = Vec::with_capacity(4096);

    // Read first line to determine connection type
    let first_line = loop {
        match lines.next_line().await? {
            Some(line) if !line.trim().is_empty() => break line,
            Some(_) => continue,
            None => return Ok(()), // Connection closed before any message
        }
    };

    let first_req = serde_json::from_str::<protocol::Request>(&first_line);

    // Check if this is an FCP handler registration
    if let Ok(ref req) = first_req {
        if req.method == "fcp.register" {
            return handle_fcp_handler_connection(
                req.id,
                &first_line,
                lines,
                &mut writer,
                &mut resp_buf,
                &fcp_bridge,
            )
            .await;
        }
    }

    // Process first message + continue loop
    let mut pending_line = Some(first_line);

    loop {
        let line = if let Some(l) = pending_line.take() {
            l
        } else {
            match lines.next_line().await? {
                Some(l) if !l.trim().is_empty() => l,
                Some(_) => continue,
                None => break,
            }
        };

        let mgr_clone = Arc::clone(&mgr);
        let reg_clone = Arc::clone(&registry);
        let coord_clone = Arc::clone(&coordinator);
        let bridge_clone = Arc::clone(&fcp_bridge);
        let plugin_clone = Arc::clone(&plugin_mgr);

        let dispatch_result = match serde_json::from_str::<protocol::Request>(&line) {
            Ok(req) => {
                tracing::debug!("request: {} (id={:?})", req.method, req.id);
                tokio::task::spawn_blocking(move || {
                    handler::dispatch(req, &mgr_clone, &reg_clone, &coord_clone, &bridge_clone, &plugin_clone)
                })
                .await
                .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })?
            }
            Err(e) => handler::DispatchResult::Ready(protocol::Response::err(
                None,
                protocol::RpcError {
                    code: protocol::ERR_PARSE,
                    message: format!("parse error: {e}"),
                    data: None,
                },
            )),
        };

        // Resolve dispatch result — may need async FCP routing or plugin spawn
        let response = match dispatch_result {
            handler::DispatchResult::Ready(resp) => resp,
            handler::DispatchResult::FcpRoute {
                id,
                handler_name,
                action,
            } => resolve_fcp_route(id, &handler_name, action, &fcp_bridge).await,
            handler::DispatchResult::FcpSpawn {
                id,
                plugin_name,
                action,
            } => {
                resolve_fcp_spawn(id, &plugin_name, action, &fcp_bridge, &plugin_mgr, &socket_path).await
            }
        };

        resp_buf.clear();
        serde_json::to_writer(&mut resp_buf, &response)?;
        resp_buf.push(b'\n');
        writer.write_all(&resp_buf).await?;
    }

    Ok(())
}

/// Resolve an FCP route request by calling the live FCP handler asynchronously.
async fn resolve_fcp_route(
    id: Option<u64>,
    handler_name: &str,
    action: handler::FcpRouteAction,
    fcp_bridge: &Arc<FcpBridge>,
) -> protocol::Response {
    let result = match action {
        handler::FcpRouteAction::Session { action } => {
            fcp_bridge.route_session(handler_name, &action).await
        }
        handler::FcpRouteAction::Ops { path, ops } => {
            fcp_bridge.route_ops(handler_name, &path, ops).await
        }
    };

    match result {
        Ok(fcp_resp) => {
            if let Some(err) = fcp_resp.error {
                protocol::Response::err(
                    id,
                    protocol::RpcError {
                        code: err.code as i32,
                        message: err.message,
                        data: None,
                    },
                )
            } else {
                // Return FCP response verbatim — add fcp_passthrough marker
                let mut result_val = fcp_resp.result.unwrap_or(serde_json::Value::Null);
                if let Some(obj) = result_val.as_object_mut() {
                    obj.insert(
                        "fcp_passthrough".to_string(),
                        serde_json::json!(handler_name),
                    );
                }
                protocol::Response {
                    id,
                    result: Some(result_val),
                    error: None,
                }
            }
        }
        Err(e) => protocol::Response::err(
            id,
            protocol::RpcError {
                code: protocol::ERR_INTERNAL,
                message: e.to_string(),
                data: None,
            },
        ),
    }
}

/// Resolve an FcpSpawn — spawn the plugin, wait for registration, then route.
async fn resolve_fcp_spawn(
    id: Option<u64>,
    plugin_name: &str,
    action: handler::FcpRouteAction,
    fcp_bridge: &Arc<FcpBridge>,
    plugin_mgr: &Arc<PluginManager>,
    socket_path: &std::path::Path,
) -> protocol::Response {
    // Skip if already live (race: another connection may have spawned it)
    if !fcp_bridge.is_handler_live(plugin_name) {
        // Spawn the plugin
        if let Err(e) = plugin_mgr.spawn(plugin_name, socket_path).await {
            tracing::warn!("plugin spawn failed: {e}");
            return protocol::Response::err(
                id,
                protocol::RpcError {
                    code: protocol::ERR_INTERNAL,
                    message: format!("failed to spawn plugin {plugin_name}: {e}"),
                    data: None,
                },
            );
        }

        // Wait for it to register
        if !plugin_mgr.wait_for_registration(plugin_name, fcp_bridge).await {
            plugin_mgr.mark_failed(plugin_name);
            tracing::warn!("plugin {plugin_name} did not register within timeout");
            return protocol::Response::err(
                id,
                protocol::RpcError {
                    code: protocol::ERR_INTERNAL,
                    message: format!(
                        "plugin {plugin_name} did not register within {}s — text-mode fallback",
                        plugin_manager::SPAWN_TIMEOUT.as_secs()
                    ),
                    data: None,
                },
            );
        }

        tracing::info!("plugin {plugin_name} registered successfully");
    }

    // Now route via the live handler
    resolve_fcp_route(id, plugin_name, action, fcp_bridge).await
}

/// Handle an FCP handler connection (bidirectional message passing).
///
/// After registration, this function enters a loop: it receives requests
/// from the bridge channel, writes them to the FCP handler's socket,
/// reads responses, and sends them back via oneshot.
async fn handle_fcp_handler_connection(
    req_id: Option<u64>,
    first_line: &str,
    mut lines: tokio::io::Lines<BufReader<tokio::net::unix::OwnedReadHalf>>,
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    resp_buf: &mut Vec<u8>,
    fcp_bridge: &Arc<FcpBridge>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Parse registration params from the first message
    let first_req: protocol::Request = serde_json::from_str(first_line)?;
    let params: FcpRegisterParams = serde_json::from_value(first_req.params)?;
    let handler_name = params.handler_name.clone();

    tracing::info!(
        "fcp handler registering: {} (extensions: {:?})",
        handler_name,
        params.extensions
    );

    // Register and get the receive channel
    let mut request_rx = fcp_bridge.register(params);

    // Send success response
    let register_result = fcp_bridge::FcpRegisterResult {
        status: "registered".to_string(),
        handler_name: handler_name.clone(),
        extensions: fcp_bridge.handler_extensions(&handler_name),
    };
    let response = protocol::Response::ok(req_id, register_result);
    resp_buf.clear();
    serde_json::to_writer(&mut *resp_buf, &response)?;
    resp_buf.push(b'\n');
    writer.write_all(resp_buf).await?;

    tracing::info!("fcp handler registered: {handler_name}");

    // Bidirectional loop: receive requests from bridge, forward to handler,
    // read response, send back via oneshot.
    loop {
        tokio::select! {
            // Request from the bridge (daemon wants to send something to the FCP handler)
            Some((fcp_req, resp_tx)) = request_rx.recv() => {
                // Write request to FCP handler socket
                resp_buf.clear();
                serde_json::to_writer(&mut *resp_buf, &fcp_req)?;
                resp_buf.push(b'\n');
                writer.write_all(resp_buf).await?;

                // Read response from FCP handler
                match lines.next_line().await? {
                    Some(line) => {
                        let fcp_resp: FcpResponse = serde_json::from_str(&line)?;
                        let _ = resp_tx.send(fcp_resp);
                    }
                    None => {
                        // Handler disconnected while we were waiting for response
                        let _ = resp_tx.send(FcpResponse {
                            id: Some(fcp_req.id),
                            result: None,
                            error: Some(fcp_bridge::FcpErrorData {
                                code: -1,
                                message: format!("{handler_name} handler disconnected"),
                            }),
                        });
                        break;
                    }
                }
            }
            // FCP handler sent something unprompted (connection drop detection)
            result = lines.next_line() => {
                match result {
                    Ok(Some(_line)) => {
                        // FCP handlers shouldn't send unsolicited messages;
                        // log and ignore
                        tracing::debug!("fcp handler {handler_name}: ignoring unsolicited message");
                    }
                    _ => {
                        // Connection closed or error
                        tracing::info!("fcp handler {handler_name}: connection closed");
                        break;
                    }
                }
            }
        }
    }

    // Cleanup on disconnect
    fcp_bridge.unregister(&handler_name);
    Ok(())
}
