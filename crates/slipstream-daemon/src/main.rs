use std::path::PathBuf;

#[tokio::main]
async fn main() {
    let socket_path = std::env::args().nth(1).map(PathBuf::from);
    slipstream_daemon::run_daemon(socket_path).await;
}
