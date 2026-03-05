use std::path::PathBuf;

use clap::{Parser, Subcommand};

use slipstream_cli::client::{self, Client, ClientError};

#[derive(Parser)]
#[command(name = "slipstream", about = "CLI client for the Slipstream editing daemon")]
struct Cli {
    /// Path to the daemon's Unix socket
    #[arg(long, env = "SLIPSTREAM_SOCKET")]
    socket: Option<PathBuf>,

    /// Don't auto-start the daemon if it isn't running
    #[arg(long)]
    no_auto_start: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Open a session with one or more files
    Open {
        /// File paths to open
        files: Vec<PathBuf>,
    },

    /// Read lines from a file
    Read {
        /// Session ID (omit to auto-open and close)
        #[arg(short, long)]
        session: Option<String>,

        path: PathBuf,

        /// Line range, e.g. 6650:6720
        #[arg(short, long)]
        lines: Option<String>,

        /// Lines from cursor
        #[arg(short = 'n', long)]
        count: Option<usize>,
    },

    /// Write lines to a file in an active session
    Write {
        /// Session ID
        #[arg(short, long)]
        session: String,

        /// File path
        path: PathBuf,

        /// Start line (0-indexed, inclusive)
        #[arg(long)]
        start: usize,

        /// End line (exclusive)
        #[arg(long)]
        end: usize,

        /// Content lines (repeatable)
        #[arg(short, long)]
        content: Vec<String>,

        /// Read content lines from stdin instead of -c flags
        #[arg(long)]
        stdin: bool,
    },

    /// Move the cursor for a file in an active session
    Cursor {
        /// Session ID
        #[arg(short, long)]
        session: String,

        /// File path
        path: PathBuf,

        /// Target line number
        #[arg(long)]
        to: usize,
    },

    /// Flush pending edits to disk
    Flush {
        /// Session ID
        #[arg(short, long)]
        session: String,

        /// Force flush even if conflicts detected
        #[arg(long)]
        force: bool,
    },

    /// Close a session and release resources
    Close {
        /// Session ID
        #[arg(short, long)]
        session: String,
    },

    /// List active sessions with file counts
    List,

    /// Execute a batch of operations in a single round trip
    Batch {
        /// Session ID
        #[arg(short, long)]
        session: String,

        /// Operations as JSON array (inline or @file)
        #[arg(long)]
        ops: String,
    },

    /// LLM/agent quick reference. If you are an AI agent, start here.
    Agent,

    /// Open files, apply operations, optionally flush, and close — all in one call.
    /// Combines open + batch + flush + close into a single CLI invocation.
    Exec {
        /// File paths to open
        #[arg(long, required = true, num_args = 1..)]
        files: Vec<PathBuf>,

        /// Operations as JSON array (inline, @file, or @- for stdin)
        #[arg(long)]
        ops: Option<String>,

        /// Read all opened files before applying ops
        #[arg(long)]
        read_all: bool,

        /// Flush edits to disk after applying ops
        #[arg(long)]
        flush: bool,

        /// Force flush even if conflicts detected
        #[arg(long)]
        force: bool,

        /// Send individual RPC calls per op instead of a single batch call.
        /// Tests the standalone handler path (both paths call the same dispatch_op).
        #[arg(long)]
        no_batch: bool,
    },

    /// Start the daemon (listens on Unix socket)
    Daemon {
        /// Socket path (overrides default)
        socket: Option<PathBuf>,
    },

    /// Start the MCP stdio server
    Mcp,
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let cli = Cli::parse();

    let socket_path = cli.socket.unwrap_or_else(client::default_socket_path);
    let auto_start = !cli.no_auto_start;

    if let Err(e) = run(cli.command, &socket_path, auto_start).await {
        let error_json = match &e {
            ClientError::Rpc { code, message, data } => {
                serde_json::json!({
                    "error": {
                        "code": code,
                        "message": message,
                        "data": data,
                    }
                })
            }
            other => {
                serde_json::json!({
                    "error": {
                        "code": -1,
                        "message": other.to_string(),
                    }
                })
            }
        };
        eprintln!("{}", serde_json::to_string_pretty(&error_json).unwrap());
        std::process::exit(1);
    }
}

async fn run(
    command: Command,
    socket_path: &std::path::Path,
    auto_start: bool,
) -> Result<(), ClientError> {
    // Subcommands that don't need a client connection
    match command {
        Command::Daemon { socket } => {
            slipstream_daemon::run_daemon(socket).await;
            return Ok(());
        }
        Command::Mcp => {
            slipstream_mcp::run_mcp().await
                .map_err(|e| ClientError::AutoStart(e.to_string()))?;
            return Ok(());
        }
        _ => {}
    }

    let mut client = Client::connect(socket_path, auto_start).await?;

    let result = match command {
        // Daemon and Mcp handled above — unreachable here
        Command::Daemon { .. } | Command::Mcp => unreachable!(),

        Command::Open { files } => {
            let paths: Vec<&str> = files.iter()
                .filter_map(|p| p.to_str())
                .collect();
            client.request("session.open", serde_json::json!({ "files": paths })).await?
        }

        Command::Read { session, path, lines, count } => {
            let (session_id, auto_opened) = match session {
                Some(s) => (s, false),
                None => {
                    let path_str = path.to_str().unwrap_or_default();
                    let open_result = client.request("session.open", serde_json::json!({ "files": [path_str] })).await?;
                    let sid = open_result["session_id"]
                        .as_str()
                        .ok_or_else(|| ClientError::Rpc {
                            code: -1,
                            message: "session.open did not return session_id".to_string(),
                            data: None,
                        })?
                        .to_string();
                    (sid, true)
                }
            };

            let mut params = serde_json::json!({
                "session_id": session_id,
                "path": path,
            });
            if let Some(ref range) = lines {
                let (start, end) = parse_line_range(range)?;
                params["start"] = serde_json::json!(start);
                params["end"] = serde_json::json!(end);
            } else if let Some(n) = count {
                params["count"] = serde_json::json!(n);
            }
            let result = client.request("file.read", params).await?;

            if auto_opened {
                let _ = client.request("session.close", serde_json::json!({
                    "session_id": session_id,
                })).await;
            }

            result
        }

        Command::Write { session, path, start, end, content, stdin } => {
            let lines = if stdin {
                read_stdin_lines()
            } else {
                content
            };
            client.request("file.write", serde_json::json!({
                "session_id": session,
                "path": path,
                "start": start,
                "end": end,
                "content": lines,
            })).await?
        }

        Command::Cursor { session, path, to } => {
            client.request("cursor.move", serde_json::json!({
                "session_id": session,
                "path": path,
                "to": to,
            })).await?
        }

        Command::Flush { session, force } => {
            client.request("session.flush", serde_json::json!({
                "session_id": session,
                "force": force,
            })).await?
        }

        Command::Close { session } => {
            client.request("session.close", serde_json::json!({
                "session_id": session,
            })).await?
        }

        Command::List => {
            client.request("session.list", serde_json::json!({})).await?
        }

        Command::Batch { session, ops } => {
            let ops_value = parse_ops(&ops)?;
            client.request("batch", serde_json::json!({
                "session_id": session,
                "ops": ops_value,
            })).await?
        }

        Command::Agent => {
            print!("{AGENT_REFERENCE}");
            return Ok(());
        }

        Command::Exec { files, ops, read_all, flush, force, no_batch } => {
            return run_exec(&mut client, files, ops, read_all, flush, force, no_batch).await;
        }
    };

    println!("{}", serde_json::to_string_pretty(&result).unwrap());
    Ok(())
}

/// Execute the combined open + batch + flush + close workflow.
async fn run_exec(
    client: &mut Client,
    files: Vec<PathBuf>,
    ops: Option<String>,
    read_all: bool,
    flush: bool,
    force: bool,
    no_batch: bool,
) -> Result<(), ClientError> {
    let mut output = serde_json::Map::new();

    // 1. Open session with files
    let paths: Vec<&str> = files.iter()
        .filter_map(|p| p.to_str())
        .collect();
    let open_result = client.request("session.open", serde_json::json!({ "files": paths })).await?;

    let session_id = open_result["session_id"]
        .as_str()
        .ok_or_else(|| ClientError::Rpc {
            code: -1,
            message: "session.open did not return session_id".to_string(),
            data: None,
        })?
        .to_string();

    output.insert("open".to_string(), open_result);

    // 2. Read all files if requested
    if read_all {
        if no_batch {
            let mut read_results = Vec::new();
            for p in &paths {
                let r = client.request("file.read", serde_json::json!({
                    "session_id": session_id,
                    "path": p,
                })).await?;
                read_results.push(r);
            }
            output.insert("read".to_string(), serde_json::Value::Array(read_results));
        } else {
            let read_ops: Vec<serde_json::Value> = paths.iter()
                .map(|p| serde_json::json!({ "method": "file.read", "path": p }))
                .collect();
            let read_result = client.request("batch", serde_json::json!({
                "session_id": session_id,
                "ops": read_ops,
            })).await?;
            output.insert("read".to_string(), read_result);
        }
    }

    // 3. Apply ops if provided
    let mut saved_ops: Option<serde_json::Value> = None;
    if let Some(ops_str) = ops {
        let ops_value = parse_ops(&ops_str)?;
        saved_ops = Some(ops_value.clone());
        if no_batch {
            let ops_array = ops_value.as_array().ok_or_else(|| ClientError::Rpc {
                code: -1,
                message: "ops must be a JSON array".to_string(),
                data: None,
            })?;
            let mut op_results = Vec::new();
            for op in ops_array {
                let method = op["method"].as_str().ok_or_else(|| ClientError::Rpc {
                    code: -1,
                    message: "each op must have a 'method' field".to_string(),
                    data: None,
                })?;
                let mut params = op.clone();
                params["session_id"] = serde_json::Value::String(session_id.clone());
                let r = client.request(method, params).await?;
                op_results.push(r);
            }
            output.insert("ops".to_string(), serde_json::Value::Array(op_results));
        } else {
            let batch_result = client.request("batch", serde_json::json!({
                "session_id": session_id,
                "ops": ops_value,
            })).await?;
            output.insert("batch".to_string(), batch_result);
        }
    }

    // 4. Flush if requested
    if flush {
        let flush_result = client.request("session.flush", serde_json::json!({
            "session_id": session_id,
            "force": force,
        })).await?;
        output.insert("flush".to_string(), flush_result);
    }

    // 5. Always close the session
    let close_result = client.request("session.close", serde_json::json!({
        "session_id": session_id,
    })).await?;
    output.insert("close".to_string(), close_result);

    // Compact domain output
    use slipstream_cli::format;
    let text = format::format_one_shot(
        &serde_json::Value::Object(output),
        saved_ops.as_ref(),
        read_all,
    );
    println!("{text}");
    Ok(())
}

const AGENT_REFERENCE: &str = r#"# Slipstream — Agent Quick Reference

IF YOU ARE AN LLM/AI AGENT, USE `exec` FOR EVERYTHING.
One command = open files + apply edits + flush + close.

## Edit a file (str_replace)

    slipstream exec --files src/main.rs --ops '[
      {"method":"file.str_replace","path":"src/main.rs","old_str":"foo","new_str":"bar"}
    ]' --flush

## Edit multiple files

    slipstream exec --files src/a.rs src/b.rs --ops '[
      {"method":"file.str_replace","path":"src/a.rs","old_str":"x","new_str":"y"},
      {"method":"file.str_replace","path":"src/b.rs","old_str":"x","new_str":"y"}
    ]' --flush

## Read a file

    slipstream exec --files src/main.rs --read-all

## Read then edit

    slipstream exec --files src/main.rs --read-all --ops '[
      {"method":"file.str_replace","path":"src/main.rs","old_str":"old","new_str":"new"}
    ]' --flush

## Insert lines (start==end inserts before that line)

    slipstream exec --files f.rs --ops '[
      {"method":"file.write","path":"f.rs","start":0,"end":0,"content":["// new header"]}
    ]' --flush

## Replace lines (start<end replaces that range)

    slipstream exec --files f.rs --ops '[
      {"method":"file.write","path":"f.rs","start":5,"end":8,"content":["new line 5","new line 6"]}
    ]' --flush

## Replace all occurrences

    Add "replace_all":true to a str_replace op.

## Key flags
    --files     Files to open (required, space-separated)
    --ops       JSON array of operations (inline, @file, or @- for stdin)
    --read-all  Print file contents before applying ops
    --flush     Write changes to disk (without this, edits are discarded)
    --force     Override conflict detection on flush

## Output
    JSON object with open/read/batch/flush/close results.
    Exit 0 on success, 1 on error (error JSON on stderr).
"#;

fn read_stdin_lines() -> Vec<String> {
    use std::io::BufRead;
    std::io::stdin().lock().lines()
        .map(|l| l.unwrap_or_default())
        .collect()
}

fn parse_ops(input: &str) -> Result<serde_json::Value, ClientError> {
    let raw = if input == "@-" {
        // Read from stdin
        let contents = std::io::read_to_string(std::io::stdin())
            .map_err(ClientError::Io)?;
        serde_json::from_str(&contents)?
    } else if let Some(path) = input.strip_prefix('@') {
        let contents = std::fs::read_to_string(path)
            .map_err(|e| ClientError::Io(std::io::Error::new(e.kind(), format!("{path}: {e}"))))?;
        serde_json::from_str(&contents)?
    } else {
        serde_json::from_str(input)?
    };
    // Normalize mixed DSL/JSON ops into daemon wire format
    slipstream_cli::parse::normalize_ops(&raw).map_err(|e| ClientError::Rpc {
        code: -32602,
        message: e,
        data: None,
    })
}

/// Parse "start:end" line range, e.g. "6650:6720"
fn parse_line_range(s: &str) -> Result<(usize, usize), ClientError> {
    let parts: Vec<&str> = s.splitn(2, ':').collect();
    if parts.len() != 2 {
        return Err(ClientError::Rpc {
            code: -1,
            message: format!("invalid line range '{s}', expected start:end"),
            data: None,
        });
    }
    let start: usize = parts[0].parse().map_err(|_| ClientError::Rpc {
        code: -1,
        message: format!("invalid start line '{}'", parts[0]),
        data: None,
    })?;
    let end: usize = parts[1].parse().map_err(|_| ClientError::Rpc {
        code: -1,
        message: format!("invalid end line '{}'", parts[1]),
        data: None,
    })?;
    Ok((start, end))
}
