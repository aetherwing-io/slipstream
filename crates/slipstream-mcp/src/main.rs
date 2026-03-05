#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    slipstream_mcp::run_mcp().await
}
