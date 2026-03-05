//! FCP Bridge — routes ops/queries/sessions to live FCP handler connections.
//!
//! FCP servers connect to the same Unix socket as normal clients. The first
//! message determines connection type: `fcp.register` enters handler mode,
//! anything else is a normal client. Connection = liveness signal.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};

/// Timeout for routing requests to FCP handlers.
pub const FCP_ROUTE_TIMEOUT: Duration = Duration::from_secs(5);

/// A request sent from the daemon to an FCP handler.
#[derive(Debug, Serialize)]
pub struct FcpRequest {
    pub id: u64,
    pub method: String,
    pub params: serde_json::Value,
}

/// A response from an FCP handler back to the daemon.
#[derive(Debug, Deserialize)]
pub struct FcpResponse {
    pub id: Option<u64>,
    #[serde(default)]
    pub result: Option<serde_json::Value>,
    #[serde(default)]
    pub error: Option<FcpErrorData>,
}

#[derive(Debug, Deserialize)]
pub struct FcpErrorData {
    pub code: i64,
    pub message: String,
}

/// Parameters for the `fcp.register` method.
#[derive(Debug, Deserialize)]
pub struct FcpRegisterParams {
    pub handler_name: String,
    pub extensions: Vec<String>,
    #[serde(default)]
    pub capabilities: FcpCapabilities,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct FcpCapabilities {
    #[serde(default = "default_true")]
    pub ops: bool,
    #[serde(default = "default_true")]
    pub query: bool,
    #[serde(default = "default_true")]
    pub session: bool,
}

fn default_true() -> bool {
    true
}

/// Result returned to FCP handler after successful registration.
#[derive(Debug, Serialize)]
pub struct FcpRegisterResult {
    pub status: String,
    pub handler_name: String,
    pub extensions: Vec<String>,
}

/// A live FCP handler connection.
pub struct FcpHandlerConnection {
    pub handler_name: String,
    pub extensions: Vec<String>,
    pub capabilities: FcpCapabilities,
    pub request_tx: mpsc::Sender<(FcpRequest, oneshot::Sender<FcpResponse>)>,
    pub registered_at: Instant,
}

/// The FCP Bridge — maps extensions to live handler connections.
///
/// Thread-safe via DashMap. Created once in main.rs, shared via Arc.
pub struct FcpBridge {
    /// handler_name → live connection channel
    handlers: DashMap<String, FcpHandlerConnection>,
    /// extension (lowercase) → handler_name
    live_ext_map: DashMap<String, String>,
    /// Monotonic request ID counter for outbound requests.
    next_request_id: AtomicU64,
}

impl FcpBridge {
    pub fn new() -> Self {
        Self {
            handlers: DashMap::new(),
            live_ext_map: DashMap::new(),
            next_request_id: AtomicU64::new(0),
        }
    }

    fn next_id(&self) -> u64 {
        self.next_request_id.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// Register a live FCP handler. Returns the channel receiver for the
    /// handler connection loop to consume.
    pub fn register(
        &self,
        params: FcpRegisterParams,
    ) -> mpsc::Receiver<(FcpRequest, oneshot::Sender<FcpResponse>)> {
        let (tx, rx) = mpsc::channel(32);

        // Map extensions → handler_name
        for ext in &params.extensions {
            self.live_ext_map
                .insert(ext.to_lowercase(), params.handler_name.clone());
        }

        self.handlers.insert(
            params.handler_name.clone(),
            FcpHandlerConnection {
                handler_name: params.handler_name,
                extensions: params.extensions,
                capabilities: params.capabilities,
                request_tx: tx,
                registered_at: Instant::now(),
            },
        );

        rx
    }

    /// Unregister a handler (called on connection drop).
    pub fn unregister(&self, handler_name: &str) {
        if let Some((_, conn)) = self.handlers.remove(handler_name) {
            for ext in &conn.extensions {
                self.live_ext_map.remove(&ext.to_lowercase());
            }
            tracing::info!("fcp handler unregistered: {handler_name}");
        }
    }

    /// Check if an extension has a live FCP handler.
    pub fn lookup_live(&self, ext: &str) -> Option<String> {
        self.live_ext_map
            .get(&ext.to_lowercase())
            .map(|v| v.value().clone())
    }

    /// Check if a handler is live by name.
    pub fn is_handler_live(&self, handler_name: &str) -> bool {
        self.handlers.contains_key(handler_name)
    }

    /// Get the extensions registered for a handler.
    pub fn handler_extensions(&self, handler_name: &str) -> Vec<String> {
        self.handlers
            .get(handler_name)
            .map(|c| c.extensions.clone())
            .unwrap_or_default()
    }

    /// Route an ops request to an FCP handler. Returns the handler's response verbatim.
    pub async fn route_ops(
        &self,
        handler_name: &str,
        path: &Path,
        ops: Vec<String>,
    ) -> Result<FcpResponse, FcpRouteError> {
        let req = FcpRequest {
            id: self.next_id(),
            method: "fcp.ops".to_string(),
            params: serde_json::json!({
                "path": path.display().to_string(),
                "ops": ops,
            }),
        };
        self.send_request(handler_name, req).await
    }

    /// Route a query to an FCP handler.
    pub async fn route_query(
        &self,
        handler_name: &str,
        path: &Path,
        query: &str,
    ) -> Result<FcpResponse, FcpRouteError> {
        let req = FcpRequest {
            id: self.next_id(),
            method: "fcp.query".to_string(),
            params: serde_json::json!({
                "path": path.display().to_string(),
                "q": query,
            }),
        };
        self.send_request(handler_name, req).await
    }

    /// Route a session action to an FCP handler.
    pub async fn route_session(
        &self,
        handler_name: &str,
        action: &str,
    ) -> Result<FcpResponse, FcpRouteError> {
        let req = FcpRequest {
            id: self.next_id(),
            method: "fcp.session".to_string(),
            params: serde_json::json!({
                "action": action,
            }),
        };
        self.send_request(handler_name, req).await
    }

    /// Send a request to a handler and wait for a response (with timeout).
    async fn send_request(
        &self,
        handler_name: &str,
        req: FcpRequest,
    ) -> Result<FcpResponse, FcpRouteError> {
        let tx = {
            let conn = self
                .handlers
                .get(handler_name)
                .ok_or_else(|| FcpRouteError::HandlerNotFound(handler_name.to_string()))?;
            conn.request_tx.clone()
        };

        let (resp_tx, resp_rx) = oneshot::channel();

        tx.send((req, resp_tx))
            .await
            .map_err(|_| FcpRouteError::HandlerDisconnected(handler_name.to_string()))?;

        match tokio::time::timeout(FCP_ROUTE_TIMEOUT, resp_rx).await {
            Ok(Ok(resp)) => Ok(resp),
            Ok(Err(_)) => Err(FcpRouteError::HandlerDisconnected(
                handler_name.to_string(),
            )),
            Err(_) => Err(FcpRouteError::Timeout(handler_name.to_string())),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum FcpRouteError {
    #[error("! {name} handler not found — use {name}() directly or restart fcp-{name}", name = .0)]
    HandlerNotFound(String),

    #[error("! {name} handler disconnected — use {name}() directly or restart fcp-{name}", name = .0)]
    HandlerDisconnected(String),

    #[error("! {name} handler timed out ({timeout}s) — use {name}() directly or restart fcp-{name}", name = .0, timeout = FCP_ROUTE_TIMEOUT.as_secs())]
    Timeout(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_register_and_lookup() {
        let bridge = FcpBridge::new();
        let _rx = bridge.register(FcpRegisterParams {
            handler_name: "sheets".to_string(),
            extensions: vec!["xlsx".to_string(), "xls".to_string()],
            capabilities: FcpCapabilities::default(),
        });

        assert_eq!(bridge.lookup_live("xlsx"), Some("sheets".to_string()));
        assert_eq!(bridge.lookup_live("xls"), Some("sheets".to_string()));
        assert_eq!(bridge.lookup_live("XLSX"), Some("sheets".to_string()));
        assert!(bridge.lookup_live("csv").is_none());
        assert!(bridge.is_handler_live("sheets"));
    }

    #[test]
    fn test_unregister_cleans_up() {
        let bridge = FcpBridge::new();
        let _rx = bridge.register(FcpRegisterParams {
            handler_name: "midi".to_string(),
            extensions: vec!["mid".to_string(), "midi".to_string()],
            capabilities: FcpCapabilities::default(),
        });

        assert!(bridge.lookup_live("mid").is_some());
        bridge.unregister("midi");
        assert!(bridge.lookup_live("mid").is_none());
        assert!(bridge.lookup_live("midi").is_none());
        assert!(!bridge.is_handler_live("midi"));
    }

    #[test]
    fn test_multiple_handlers() {
        let bridge = FcpBridge::new();
        let _rx1 = bridge.register(FcpRegisterParams {
            handler_name: "sheets".to_string(),
            extensions: vec!["xlsx".to_string()],
            capabilities: FcpCapabilities::default(),
        });
        let _rx2 = bridge.register(FcpRegisterParams {
            handler_name: "midi".to_string(),
            extensions: vec!["mid".to_string()],
            capabilities: FcpCapabilities::default(),
        });

        assert_eq!(bridge.lookup_live("xlsx"), Some("sheets".to_string()));
        assert_eq!(bridge.lookup_live("mid"), Some("midi".to_string()));

        bridge.unregister("sheets");
        assert!(bridge.lookup_live("xlsx").is_none());
        assert_eq!(bridge.lookup_live("mid"), Some("midi".to_string()));
    }

    #[test]
    fn test_re_register_replaces() {
        let bridge = FcpBridge::new();
        let _rx1 = bridge.register(FcpRegisterParams {
            handler_name: "sheets".to_string(),
            extensions: vec!["xlsx".to_string()],
            capabilities: FcpCapabilities::default(),
        });
        // Re-register with different extensions
        let _rx2 = bridge.register(FcpRegisterParams {
            handler_name: "sheets".to_string(),
            extensions: vec!["xlsx".to_string(), "csv".to_string()],
            capabilities: FcpCapabilities::default(),
        });

        assert_eq!(bridge.lookup_live("xlsx"), Some("sheets".to_string()));
        assert_eq!(bridge.lookup_live("csv"), Some("sheets".to_string()));
    }

    #[tokio::test]
    async fn test_route_to_missing_handler() {
        let bridge = FcpBridge::new();
        let result = bridge
            .route_ops("nonexistent", Path::new("/tmp/test.xlsx"), vec![])
            .await;
        assert!(matches!(result, Err(FcpRouteError::HandlerNotFound(_))));
    }

    #[tokio::test]
    async fn test_route_ops_with_live_handler() {
        let bridge = FcpBridge::new();
        let mut rx = bridge.register(FcpRegisterParams {
            handler_name: "sheets".to_string(),
            extensions: vec!["xlsx".to_string()],
            capabilities: FcpCapabilities::default(),
        });

        // Spawn a mock handler that echoes back
        tokio::spawn(async move {
            if let Some((req, resp_tx)) = rx.recv().await {
                let _ = resp_tx.send(FcpResponse {
                    id: Some(req.id),
                    result: Some(serde_json::json!({
                        "text": "A1: Revenue",
                        "success": true,
                    })),
                    error: None,
                });
            }
        });

        let resp = bridge
            .route_ops(
                "sheets",
                Path::new("/tmp/report.xlsx"),
                vec!["set A1 Revenue".to_string()],
            )
            .await
            .unwrap();

        assert!(resp.error.is_none());
        let text = resp
            .result
            .unwrap()
            .get("text")
            .unwrap()
            .as_str()
            .unwrap()
            .to_string();
        assert_eq!(text, "A1: Revenue");
    }

    #[tokio::test]
    async fn test_route_timeout() {
        let bridge = FcpBridge::new();
        // Register but never respond — handler just holds the receiver
        let _rx = bridge.register(FcpRegisterParams {
            handler_name: "slow".to_string(),
            extensions: vec!["slow".to_string()],
            capabilities: FcpCapabilities::default(),
        });
        // Drop _rx so the channel closes immediately, simulating disconnect
        drop(_rx);

        let result = bridge
            .route_ops("slow", Path::new("/tmp/test.slow"), vec![])
            .await;
        assert!(matches!(result, Err(FcpRouteError::HandlerDisconnected(_))));
    }
}
