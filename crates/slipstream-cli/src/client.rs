use std::path::{Path, PathBuf};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

/// Compute the default socket path, matching the daemon's logic.
pub fn default_socket_path() -> PathBuf {
    let runtime_dir = std::env::var("XDG_RUNTIME_DIR")
        .or_else(|_| std::env::var("TMPDIR"))
        .unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(runtime_dir).join("slipstream.sock")
}

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("connection failed: {0}")]
    Connection(std::io::Error),

    #[error("auto-start failed: {0}")]
    AutoStart(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("RPC error {code}: {message}")]
    Rpc {
        code: i64,
        message: String,
        data: Option<serde_json::Value>,
    },
}

pub struct Client {
    reader: tokio::io::Lines<BufReader<tokio::net::unix::OwnedReadHalf>>,
    writer: tokio::net::unix::OwnedWriteHalf,
    next_id: u64,
    write_buf: Vec<u8>,
}

impl Client {
    /// Connect to the daemon at the given socket path.
    /// If `auto_start` is true and the socket doesn't exist, spawn the daemon and retry.
    pub async fn connect(socket_path: &Path, auto_start: bool) -> Result<Self, ClientError> {
        match UnixStream::connect(socket_path).await {
            Ok(stream) => Ok(Self::from_stream(stream)),
            Err(e) if auto_start => {
                auto_start_daemon(socket_path)?;
                // Retry with backoff
                let delays = [50, 100, 200, 400, 800];
                for delay in delays {
                    tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                    if let Ok(stream) = UnixStream::connect(socket_path).await {
                        return Ok(Self::from_stream(stream));
                    }
                }
                Err(ClientError::Connection(e))
            }
            Err(e) => Err(ClientError::Connection(e)),
        }
    }

    fn from_stream(stream: UnixStream) -> Self {
        let (read_half, write_half) = stream.into_split();
        Client {
            reader: BufReader::new(read_half).lines(),
            writer: write_half,
            next_id: 1,
            write_buf: Vec::with_capacity(4096),
        }
    }

    /// Send a JSON-RPC request and return the result value.
    pub async fn request(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, ClientError> {
        let id = self.next_id;
        self.next_id += 1;

        let req = serde_json::json!({
            "id": id,
            "method": method,
            "params": params,
        });

        self.write_buf.clear();
        serde_json::to_writer(&mut self.write_buf, &req)?;
        self.write_buf.push(b'\n');
        self.writer.write_all(&self.write_buf).await?;

        let line = self.reader.next_line().await?
            .ok_or_else(|| ClientError::Io(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "daemon closed connection",
            )))?;

        let resp: serde_json::Value = serde_json::from_str(&line)?;

        if let Some(err) = resp.get("error") {
            return Err(ClientError::Rpc {
                code: err["code"].as_i64().unwrap_or(-1),
                message: err["message"].as_str().unwrap_or("unknown").to_string(),
                data: err.get("data").cloned(),
            });
        }

        Ok(resp.get("result").cloned().unwrap_or(serde_json::Value::Null))
    }
}

/// Spawn the daemon process detached, pointing at the given socket path.
fn auto_start_daemon(socket_path: &Path) -> Result<(), ClientError> {
    // Look for the daemon binary next to our own executable first, then PATH
    let daemon_name = "slipstream-daemon";
    let daemon_path = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join(daemon_name)))
        .filter(|p| p.exists())
        .unwrap_or_else(|| PathBuf::from(daemon_name));

    use std::process::{Command, Stdio};
    Command::new(&daemon_path)
        .arg(socket_path.as_os_str())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| ClientError::AutoStart(format!(
            "failed to start {}: {e}", daemon_path.display()
        )))?;

    Ok(())
}
