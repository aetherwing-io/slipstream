use std::path::PathBuf;

use rmcp::{transport::stdio, ServiceExt};
use slipstream_cli::client::default_socket_path;

mod params;
mod server;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let socket_path = std::env::var("SLIPSTREAM_SOCKET")
        .map(PathBuf::from)
        .unwrap_or_else(|_| default_socket_path());

    let auto_start = std::env::var("SLIPSTREAM_NO_AUTO_START").is_err();

    let server = server::SlipstreamServer::new(&socket_path, auto_start);

    let service = server.serve(stdio()).await.inspect_err(|e| {
        eprintln!("MCP server error: {e}");
    })?;

    service.waiting().await?;

    Ok(())
}
